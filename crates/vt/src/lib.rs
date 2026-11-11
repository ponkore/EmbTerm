//! et-vt: libghostty-vt（VT の解釈）の FFI と安全なラッパ。
//!
//! - [`Terminal`]: 端末の状態。PTY から読んだバイト列を [`Terminal::vt_write`] で渡す。
//!   端末からの知らせ（PTY への応答・ベル・タイトル・カレントディレクトリ）は [`Effects`] で受ける
//! - [`RenderState`]: 描画用の写し。端末のロックを持っている間に [`RenderState::update`] し、
//!   ロックを外してから行とセルを読んで描く（docs/TODO.md T-006）
//! - [`KeyEncoder`]: 特殊キー（矢印・ファンクションキーなど）を端末のモードに合わせた VT の列にする
//!
//! unsafe はこのクレートの中に閉じ込める。libghostty-vt の端末はスレッド安全ではないので、
//! 複数のスレッドから使うときは呼ぶ側が `Mutex` で守る（[`Terminal`] は `Send` だが `Sync` ではない）。

pub mod ffi;
#[cfg(test)]
mod tests;

use std::ffi::{CStr, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

/// RGB の色
pub type Rgb = ffi::GhosttyColorRgb;

/// libghostty-vt の呼び出しの失敗（`GhosttyResult` の負の値）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.0 {
            ffi::GHOSTTY_OUT_OF_MEMORY => "OUT_OF_MEMORY",
            ffi::GHOSTTY_INVALID_VALUE => "INVALID_VALUE",
            ffi::GHOSTTY_OUT_OF_SPACE => "OUT_OF_SPACE",
            ffi::GHOSTTY_NO_VALUE => "NO_VALUE",
            _ => "?",
        };
        write!(f, "libghostty-vt のエラー {} ({name})", self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn check(r: ffi::GhosttyResult) -> Result<()> {
    if r == ffi::GHOSTTY_SUCCESS { Ok(()) } else { Err(Error(r)) }
}

/// libghostty-vt の版などを JSON で返す（構造体の配置の照合に使う）
pub fn type_json() -> &'static str {
    unsafe { CStr::from_ptr(ffi::ghostty_type_json()) }.to_str().unwrap_or("")
}

/// 1 つのコードポイントが何桁か（端末が使う表と同じもの。eaw は 1）
pub fn codepoint_width(c: char) -> u8 {
    unsafe { ffi::ghostty_unicode_codepoint_width(c as u32) }
}

/// 端末からの知らせを受けるもの。メソッドは [`Terminal::vt_write`] の中から**同期的に**呼ばれる
/// （呼んだスレッドで、端末のロックを持ったまま）。重いことや、ロックを取り直すことはしない。
/// `write_pty` で受けたバイト列は、PTY に直接書かずにチャネルなどへ積む（T-006）
pub trait Effects: Send {
    /// 端末が PTY に書き戻したいバイト列（DA・CPR などへの応答）
    fn write_pty(&mut self, _data: &[u8]) {}
    /// BEL（0x07）
    fn bell(&mut self) {}
    /// タイトル（OSC 0 / 2）が変わった
    fn title_changed(&mut self, _title: &str) {}
    /// カレントディレクトリ（OSC 7 など）が変わった。OSC 7 なら URI のまま
    fn pwd_changed(&mut self, _pwd: &str) {}
}

/// 何もしない [`Effects`]
pub struct NoEffects;
impl Effects for NoEffects {}

/// コールバックに渡す userdata。`Box::into_raw` したものを端末に渡し、[`Terminal`] の `Drop` で戻す
struct Inner {
    effects: Box<dyn Effects>,
    /// `CSI 16 t` などで答えるセルの画素の大きさ。0 だと Kitty graphics を使わないアプリがあるので 1 以上にする
    cell_px: (u32, u32),
}

/// ビューポートのスクロールのしかた
#[derive(Clone, Copy, Debug)]
pub enum Scroll {
    /// スクロールバックの先頭へ
    Top,
    /// いちばん下（アプリが書いている画面）へ
    Bottom,
    /// 行数だけ動かす。負は上（古いほう）
    Delta(isize),
}

/// スクロールバーの状態（行数）
#[derive(Clone, Copy, Debug, Default)]
pub struct Scrollbar {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

/// 端末のモード（modes.h の `GhosttyMode`）。`ansi` は ANSI のモード（`CSI h`）か DEC のプライベートモード（`CSI ? h`）か
#[derive(Clone, Copy, Debug)]
pub struct Mode {
    pub value: u16,
    pub ansi: bool,
}

impl Mode {
    pub const FOCUS_EVENT: Mode = Mode { value: 1004, ansi: false };
    pub const BRACKETED_PASTE: Mode = Mode { value: 2004, ansi: false };
    pub const SYNCHRONIZED_OUTPUT: Mode = Mode { value: 2026, ansi: false };
    pub const GRAPHEME_CLUSTER: Mode = Mode { value: 2027, ansi: false };

    fn raw(self) -> u16 {
        (self.value & 0x7fff) | ((self.ansi as u16) << 15)
    }
}

/// 端末 1 つ
pub struct Terminal {
    raw: ffi::GhosttyTerminal,
    inner: *mut Inner,
}

// libghostty-vt の端末は別のスレッドへ動かしてよい（同時に触らなければよい）。同時に触らないことは
// `&mut self` と、呼ぶ側の `Mutex` で守る
unsafe impl Send for Terminal {}

impl Terminal {
    /// 端末を作る。`effects` が端末からの知らせを受ける
    pub fn new(cols: u16, rows: u16, effects: Box<dyn Effects>) -> Result<Self> {
        let mut raw = ptr::null_mut();
        check(unsafe { ffi::ghostty_terminal_new(ptr::null(), &mut raw, cols.max(1), rows.max(1)) })?;
        let inner = Box::into_raw(Box::new(Inner { effects, cell_px: (1, 1) }));
        let t = Self { raw, inner };
        unsafe {
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_USERDATA, inner as *const c_void))?;
            let write_pty: ffi::GhosttyTerminalWritePtyFn = cb_write_pty;
            let bell: ffi::GhosttyTerminalBellFn = cb_bell;
            let title: ffi::GhosttyTerminalTitleChangedFn = cb_title_changed;
            let pwd: ffi::GhosttyTerminalPwdChangedFn = cb_pwd_changed;
            let size: ffi::GhosttyTerminalSizeFn = cb_size;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_WRITE_PTY, write_pty as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_BELL, bell as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_TITLE_CHANGED, title as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_PWD_CHANGED, pwd as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_SIZE, size as *const c_void))?;
        }
        Ok(t)
    }

    /// PTY から読んだバイト列を解釈する。途中で切れた列をそのまま渡してよい
    pub fn vt_write(&mut self, data: &[u8]) {
        if !data.is_empty() {
            unsafe { ffi::ghostty_terminal_vt_write(self.raw, data.as_ptr(), data.len()) }
        }
    }

    /// 大きさを変える（主画面は reflow する）。セルの画素の大きさは `CSI 14/16/18 t` の答えと画像に使う
    pub fn resize(&mut self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) -> Result<()> {
        let (cw, ch) = (cell_w.max(1), cell_h.max(1));
        unsafe { (*self.inner).cell_px = (cw, ch) };
        check(unsafe { ffi::ghostty_terminal_resize(self.raw, cols.max(1), rows.max(1), cw, ch) })
    }

    /// 桁数と行数
    pub fn size(&self) -> (u16, u16) {
        (self.get_u16(ffi::GHOSTTY_TERMINAL_DATA_COLS), self.get_u16(ffi::GHOSTTY_TERMINAL_DATA_ROWS))
    }

    /// カーソルの位置（アプリが書いている画面での桁と行）
    pub fn cursor(&self) -> (u16, u16) {
        (self.get_u16(ffi::GHOSTTY_TERMINAL_DATA_CURSOR_X), self.get_u16(ffi::GHOSTTY_TERMINAL_DATA_CURSOR_Y))
    }

    fn get_u16(&self, data: c_int) -> u16 {
        let mut v: u16 = 0;
        unsafe { ffi::ghostty_terminal_get(self.raw, data, &mut v as *mut u16 as *mut c_void) };
        v
    }

    fn get_string(&self, data: c_int) -> String {
        let mut s = ffi::GhosttyString { ptr: ptr::null(), len: 0 };
        let r = unsafe { ffi::ghostty_terminal_get(self.raw, data, &mut s as *mut _ as *mut c_void) };
        if r != ffi::GHOSTTY_SUCCESS || s.ptr.is_null() {
            return String::new();
        }
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(s.ptr, s.len) }).into_owned()
    }

    /// タイトル（OSC 0 / 2）。無ければ空
    pub fn title(&self) -> String {
        self.get_string(ffi::GHOSTTY_TERMINAL_DATA_TITLE)
    }

    /// カレントディレクトリ（OSC 7 なら URI のまま）。無ければ空
    pub fn pwd(&self) -> String {
        self.get_string(ffi::GHOSTTY_TERMINAL_DATA_PWD)
    }

    /// 代替画面にいるか
    pub fn is_alternate_screen(&self) -> bool {
        let mut v: c_int = 0;
        unsafe {
            ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &mut v as *mut _ as *mut c_void)
        };
        v == 1
    }

    /// スクロールバーの状態
    pub fn scrollbar(&self) -> Scrollbar {
        let mut s = ffi::GhosttyTerminalScrollbar::default();
        unsafe { ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_SCROLLBAR, &mut s as *mut _ as *mut c_void) };
        Scrollbar { total: s.total, offset: s.offset, len: s.len }
    }

    /// ビューポートがいちばん下（アプリが書いている画面）にあるか
    pub fn viewport_at_bottom(&self) -> bool {
        let mut v = true;
        unsafe {
            ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_VIEWPORT_ACTIVE, &mut v as *mut _ as *mut c_void)
        };
        v
    }

    /// ビューポートを動かす（スクロールバックを遡る）
    pub fn scroll_viewport(&mut self, scroll: Scroll) {
        let (tag, delta) = match scroll {
            Scroll::Top => (ffi::GHOSTTY_TERMINAL_SCROLL_VIEWPORT_TOP, 0),
            Scroll::Bottom => (ffi::GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM, 0),
            Scroll::Delta(d) => (ffi::GHOSTTY_TERMINAL_SCROLL_VIEWPORT_DELTA, d),
        };
        let behavior = ffi::GhosttyTerminalScrollViewport {
            tag,
            value: ffi::GhosttyTerminalScrollViewportValue { _padding: [delta as u64, 0] },
        };
        unsafe { ffi::ghostty_terminal_scroll_viewport(self.raw, behavior) };
    }

    /// モードの今の値
    pub fn mode(&self, mode: Mode) -> bool {
        let mut c = ffi::GhosttyTerminalModeConfig { mode: mode.raw(), value: false };
        unsafe { ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_MODE, &mut c as *mut _ as *mut c_void) };
        c.value
    }

    /// モードを変える。`default` なら RIS で戻る値も変える
    pub fn set_mode(&mut self, mode: Mode, value: bool, default: bool) -> Result<()> {
        let c = ffi::GhosttyTerminalModeConfig { mode: mode.raw(), value };
        let opt = if default { ffi::GHOSTTY_TERMINAL_OPT_MODE_DEFAULT } else { ffi::GHOSTTY_TERMINAL_OPT_MODE };
        check(unsafe { ffi::ghostty_terminal_set(self.raw, opt, &c as *const _ as *const c_void) })
    }

    /// リサイズで行をスクロールバックから戻すか。ConPTY は自分の画面（スクロールバック無し）を持つので false にする
    /// （ghostel の `resize_pull_scrollback = false` と同じ。docs/research/ghostel.md §4.2）
    pub fn set_resize_pull_scrollback(&mut self, value: bool) -> Result<()> {
        check(unsafe {
            ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_RESIZE_PULL_SCROLLBACK, &value as *const _ as *const c_void)
        })
    }

    /// スクロールバックの行数の上限
    pub fn set_scrollback_max_lines(&mut self, lines: usize) -> Result<()> {
        check(unsafe {
            ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, &lines as *const _ as *const c_void)
        })
    }

    /// 既定の前景・背景・カーソルの色（OSC 10/11/12 で上書きされうる）
    pub fn set_default_colors(&mut self, fg: Rgb, bg: Rgb, cursor: Option<Rgb>) -> Result<()> {
        unsafe {
            check(ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, &fg as *const _ as *const c_void))?;
            check(ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, &bg as *const _ as *const c_void))?;
            let c = cursor.as_ref().map_or(ptr::null(), |c| c as *const _ as *const c_void);
            check(ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, c))
        }
    }

    /// XTGETTCAP の "TN" で答える terminfo の名前
    pub fn set_terminfo_name(&mut self, name: &str) -> Result<()> {
        let s = ffi::GhosttyString { ptr: name.as_ptr(), len: name.len() };
        check(unsafe { ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_TERMINFO_NAME, &s as *const _ as *const c_void) })
    }

    fn viewport_ref(&self, x: u16, y: u16) -> Result<ffi::GhosttyGridRef> {
        let point = ffi::GhosttyPoint {
            tag: ffi::GHOSTTY_POINT_TAG_VIEWPORT,
            value: ffi::GhosttyPointValue { coordinate: ffi::GhosttyPointCoordinate { x, y: y as u32 } },
        };
        let mut r = ffi::GhosttyGridRef { node: ptr::null_mut(), x: 0, y: 0 };
        check(unsafe { ffi::ghostty_terminal_grid_ref(self.raw, point, &mut r) })?;
        Ok(r)
    }

    /// ビューポートの (桁, 行) で選択範囲を決める（両端を含む）。`None` で消す。
    /// 選択はスクロールバックの中の位置として持つので、スクロールしても同じ文字に付いて動く
    pub fn set_selection(&mut self, range: Option<((u16, u16), (u16, u16))>) -> Result<()> {
        match range {
            None => check(unsafe { ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_SELECTION, ptr::null()) }),
            Some((a, b)) => {
                let sel = ffi::GhosttySelection {
                    start: self.viewport_ref(a.0, a.1)?,
                    end: self.viewport_ref(b.0, b.1)?,
                    rectangle: false,
                };
                check(unsafe {
                    ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_SELECTION, &sel as *const _ as *const c_void)
                })
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            ffi::ghostty_terminal_free(self.raw);
            drop(Box::from_raw(self.inner));
        }
    }
}

// ---- コールバック。C の境界を panic で越えないよう catch_unwind で包む ----

unsafe fn inner<'a>(userdata: *mut c_void) -> Option<&'a mut Inner> {
    unsafe { (userdata as *mut Inner).as_mut() }
}

unsafe extern "C" fn cb_write_pty(_t: ffi::GhosttyTerminal, userdata: *mut c_void, data: *const u8, len: usize) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        if let Some(i) = inner(userdata)
            && !data.is_null()
        {
            i.effects.write_pty(std::slice::from_raw_parts(data, len));
        }
    }));
}

unsafe extern "C" fn cb_bell(_t: ffi::GhosttyTerminal, userdata: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        if let Some(i) = inner(userdata) {
            i.effects.bell();
        }
    }));
}

unsafe fn terminal_string(t: ffi::GhosttyTerminal, data: c_int) -> String {
    let mut s = ffi::GhosttyString { ptr: ptr::null(), len: 0 };
    let r = unsafe { ffi::ghostty_terminal_get(t, data, &mut s as *mut _ as *mut c_void) };
    if r != ffi::GHOSTTY_SUCCESS || s.ptr.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(s.ptr, s.len) }).into_owned()
}

unsafe extern "C" fn cb_title_changed(t: ffi::GhosttyTerminal, userdata: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        if let Some(i) = inner(userdata) {
            let title = terminal_string(t, ffi::GHOSTTY_TERMINAL_DATA_TITLE);
            i.effects.title_changed(&title);
        }
    }));
}

unsafe extern "C" fn cb_pwd_changed(t: ffi::GhosttyTerminal, userdata: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        if let Some(i) = inner(userdata) {
            let pwd = terminal_string(t, ffi::GHOSTTY_TERMINAL_DATA_PWD);
            i.effects.pwd_changed(&pwd);
        }
    }));
}

/// `CSI 14/16/18/19 t` と mode 2048 の答え。桁数・行数は端末から、セルの画素は [`Terminal::resize`] で覚えた値
unsafe extern "C" fn cb_size(t: ffi::GhosttyTerminal, userdata: *mut c_void, out: *mut ffi::GhosttySizeReportSize) -> bool {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        let (Some(i), Some(out)) = (inner(userdata), out.as_mut()) else { return false };
        let mut cols: u16 = 0;
        let mut rows: u16 = 0;
        ffi::ghostty_terminal_get(t, ffi::GHOSTTY_TERMINAL_DATA_COLS, &mut cols as *mut _ as *mut c_void);
        ffi::ghostty_terminal_get(t, ffi::GHOSTTY_TERMINAL_DATA_ROWS, &mut rows as *mut _ as *mut c_void);
        out.rows = rows;
        out.columns = cols;
        out.cell_width = i.cell_px.0;
        out.cell_height = i.cell_px.1;
        true
    }))
    .unwrap_or(false)
}

// ---- render state ----

/// どれだけ描き直すか
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dirty {
    /// 変わっていない
    Clean,
    /// 一部の行が変わった
    Partial,
    /// 全体を描き直す
    Full,
}

/// カーソルの形
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Bar,
    Block,
    Underline,
    BlockHollow,
}

/// render state のカーソル
#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    /// ビューポートの中での位置。スクロールしてビューポートの外に出ていれば `None`
    pub pos: Option<(u16, u16)>,
    /// 幅 2 の文字の右半分にいるか
    pub wide_tail: bool,
    /// 端末のモード（DECTCEM）で見えるか
    pub visible: bool,
    pub blinking: bool,
    pub shape: CursorShape,
}

/// render state の色
#[derive(Clone, Copy)]
pub struct Colors {
    pub background: Rgb,
    pub foreground: Rgb,
    /// アプリ（OSC 12）か既定で決まったカーソルの色
    pub cursor: Option<Rgb>,
    pub palette: [Rgb; 256],
}

/// セルの幅の種類
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Wide {
    /// 1 桁
    #[default]
    Narrow,
    /// 2 桁の文字の左半分（文字を持つ）
    Wide,
    /// 2 桁の文字の右半分（何も描かない）
    SpacerTail,
    /// 行末に入りきらなかった 2 桁の文字の代わりの空き
    SpacerHead,
}

/// セルの見た目（style.h の `GhosttyStyle` そのもの）
pub type Style = ffi::GhosttyStyle;

/// 下線の種類（sgr.h の `GhosttySgrUnderline`）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Underline {
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// [`Style`] の読み出し
pub trait StyleExt {
    fn bold(&self) -> bool;
    fn italic(&self) -> bool;
    fn faint(&self) -> bool;
    fn inverse(&self) -> bool;
    fn invisible(&self) -> bool;
    fn strikethrough(&self) -> bool;
    fn overline(&self) -> bool;
    fn underline(&self) -> Underline;
    /// 下線の色。指定が無ければ `None`（前景色で描く）
    fn underline_color(&self, palette: &[Rgb; 256]) -> Option<Rgb>;
}

impl StyleExt for Style {
    fn bold(&self) -> bool {
        self.flags & ffi::GHOSTTY_STYLE_FLAG_BOLD != 0
    }
    fn italic(&self) -> bool {
        self.flags & ffi::GHOSTTY_STYLE_FLAG_ITALIC != 0
    }
    fn faint(&self) -> bool {
        self.flags & ffi::GHOSTTY_STYLE_FLAG_FAINT != 0
    }
    fn inverse(&self) -> bool {
        self.flags & ffi::GHOSTTY_STYLE_FLAG_INVERSE != 0
    }
    fn invisible(&self) -> bool {
        self.flags & ffi::GHOSTTY_STYLE_FLAG_INVISIBLE != 0
    }
    fn strikethrough(&self) -> bool {
        self.flags & ffi::GHOSTTY_STYLE_FLAG_STRIKETHROUGH != 0
    }
    fn overline(&self) -> bool {
        self.flags & ffi::GHOSTTY_STYLE_FLAG_OVERLINE != 0
    }
    fn underline(&self) -> Underline {
        match (self.flags & ffi::GHOSTTY_STYLE_UNDERLINE_MASK) >> ffi::GHOSTTY_STYLE_UNDERLINE_SHIFT {
            0 => Underline::None,
            1 => Underline::Single,
            2 => Underline::Double,
            3 => Underline::Curly,
            4 => Underline::Dotted,
            5 => Underline::Dashed,
            _ => Underline::Single,
        }
    }
    fn underline_color(&self, palette: &[Rgb; 256]) -> Option<Rgb> {
        let c = self.underline_color;
        match c.tag {
            ffi::GHOSTTY_STYLE_COLOR_PALETTE => Some(palette[c.value[0] as usize]),
            ffi::GHOSTTY_STYLE_COLOR_RGB => Some(Rgb { r: c.value[0], g: c.value[1], b: c.value[2] }),
            _ => None,
        }
    }
}

/// 描画用の写し。行の反復子とセルの入れ物は作ったものを使い回す
pub struct RenderState {
    raw: ffi::GhosttyRenderState,
    rows: ffi::GhosttyRenderStateRowIterator,
    cells: ffi::GhosttyRenderStateRowCells,
}

unsafe impl Send for RenderState {}

impl RenderState {
    pub fn new() -> Result<Self> {
        // 途中で失敗したら Drop が作れた分だけ解放する（どの free も NULL を受け付ける）
        let mut s = Self { raw: ptr::null_mut(), rows: ptr::null_mut(), cells: ptr::null_mut() };
        unsafe {
            check(ffi::ghostty_render_state_new(ptr::null(), &mut s.raw))?;
            check(ffi::ghostty_render_state_row_iterator_new(ptr::null(), &mut s.rows))?;
            check(ffi::ghostty_render_state_row_cells_new(ptr::null(), &mut s.cells))?;
        }
        Ok(s)
    }

    /// 端末から写す。端末のロックを持っている間に呼ぶ。写したあとはロックを外して読んでよい
    pub fn update(&mut self, terminal: &Terminal) -> Result<()> {
        check(unsafe { ffi::ghostty_render_state_update(self.raw, terminal.raw) })
    }

    /// 1 フレームを描き終えたら呼ぶ（全体と行ごとの dirty を落とす）
    pub fn clean(&mut self) {
        unsafe { ffi::ghostty_render_state_clean(self.raw) };
    }

    pub fn dirty(&self) -> Dirty {
        let mut d: c_int = 0;
        unsafe { ffi::ghostty_render_state_get(self.raw, ffi::GHOSTTY_RENDER_STATE_DATA_DIRTY, &mut d as *mut _ as *mut c_void) };
        match d {
            ffi::GHOSTTY_RENDER_STATE_DIRTY_FALSE => Dirty::Clean,
            ffi::GHOSTTY_RENDER_STATE_DIRTY_PARTIAL => Dirty::Partial,
            _ => Dirty::Full,
        }
    }

    /// 桁数と行数
    pub fn size(&self) -> (u16, u16) {
        let mut cols: u16 = 0;
        let mut rows: u16 = 0;
        unsafe {
            ffi::ghostty_render_state_get(self.raw, ffi::GHOSTTY_RENDER_STATE_DATA_COLS, &mut cols as *mut _ as *mut c_void);
            ffi::ghostty_render_state_get(self.raw, ffi::GHOSTTY_RENDER_STATE_DATA_ROWS, &mut rows as *mut _ as *mut c_void);
        }
        (cols, rows)
    }

    pub fn colors(&self) -> Colors {
        let mut c = ffi::GhosttyRenderStateColors {
            size: std::mem::size_of::<ffi::GhosttyRenderStateColors>(),
            background: Rgb::default(),
            foreground: Rgb::default(),
            cursor: Rgb::default(),
            cursor_has_value: false,
            palette: [Rgb::default(); 256],
        };
        unsafe { ffi::ghostty_render_state_get(self.raw, ffi::GHOSTTY_RENDER_STATE_DATA_COLORS, &mut c as *mut _ as *mut c_void) };
        Colors {
            background: c.background,
            foreground: c.foreground,
            cursor: c.cursor_has_value.then_some(c.cursor),
            palette: c.palette,
        }
    }

    pub fn cursor(&self) -> Cursor {
        let mut c = ffi::GhosttyRenderStateCursor {
            size: std::mem::size_of::<ffi::GhosttyRenderStateCursor>(),
            viewport_has_value: false,
            viewport_x: 0,
            viewport_y: 0,
            wide_tail: false,
            visible: false,
            blinking: false,
            password_input: false,
            visual_style: ffi::GHOSTTY_CURSOR_STYLE_BLOCK,
        };
        unsafe { ffi::ghostty_render_state_get(self.raw, ffi::GHOSTTY_RENDER_STATE_DATA_CURSOR, &mut c as *mut _ as *mut c_void) };
        Cursor {
            pos: c.viewport_has_value.then_some((c.viewport_x, c.viewport_y)),
            wide_tail: c.viewport_has_value && c.wide_tail,
            visible: c.visible,
            blinking: c.blinking,
            shape: match c.visual_style {
                ffi::GHOSTTY_CURSOR_STYLE_BAR => CursorShape::Bar,
                ffi::GHOSTTY_CURSOR_STYLE_UNDERLINE => CursorShape::Underline,
                ffi::GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW => CursorShape::BlockHollow,
                _ => CursorShape::Block,
            },
        }
    }

    /// 行の反復を始める。描き直しの要る行（全体が dirty なら全部の行）だけを上から順に返す
    pub fn dirty_rows(&mut self) -> Rows<'_> {
        unsafe {
            ffi::ghostty_render_state_get(self.raw, ffi::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &mut self.rows as *mut _ as *mut c_void)
        };
        Rows { state: self }
    }
}

impl Drop for RenderState {
    fn drop(&mut self) {
        unsafe {
            ffi::ghostty_render_state_row_cells_free(self.cells);
            ffi::ghostty_render_state_row_iterator_free(self.rows);
            ffi::ghostty_render_state_free(self.raw);
        }
    }
}

/// 行の反復子（[`RenderState::dirty_rows`]）
pub struct Rows<'a> {
    state: &'a mut RenderState,
}

impl Rows<'_> {
    /// 次の描き直しの要る行へ進み、その行の y（ビューポートの行番号）を返す
    pub fn next_dirty(&mut self) -> Option<u16> {
        let mut y: u16 = 0;
        unsafe { ffi::ghostty_render_state_row_iterator_next_dirty(self.state.rows, &mut y) }.then_some(y)
    }

    /// 今の行の選択範囲（両端を含む桁）。選択と重ならなければ `None`
    pub fn selection(&self) -> Option<(u16, u16)> {
        let mut s = ffi::GhosttyRenderStateRowSelection {
            size: std::mem::size_of::<ffi::GhosttyRenderStateRowSelection>(),
            start_x: 0,
            end_x: 0,
        };
        let r = unsafe {
            ffi::ghostty_render_state_row_get(self.state.rows, ffi::GHOSTTY_RENDER_STATE_ROW_DATA_SELECTION, &mut s as *mut _ as *mut c_void)
        };
        (r == ffi::GHOSTTY_SUCCESS).then_some((s.start_x, s.end_x))
    }

    /// 今の行のセルの反復を始める
    pub fn cells(&mut self) -> Cells<'_> {
        unsafe {
            ffi::ghostty_render_state_row_get(
                self.state.rows,
                ffi::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                &mut self.state.cells as *mut _ as *mut c_void,
            )
        };
        Cells { raw: self.state.cells, _rows: std::marker::PhantomData }
    }
}

/// セルの反復子（[`Rows::cells`]）
pub struct Cells<'a> {
    raw: ffi::GhosttyRenderStateRowCells,
    _rows: std::marker::PhantomData<&'a mut ()>,
}

impl Cells<'_> {
    /// 次のセルへ進む。行の終わりなら false
    pub fn next_cell(&mut self) -> bool {
        unsafe { ffi::ghostty_render_state_row_cells_next(self.raw) }
    }

    /// 今のセルの書記素（基底の文字と結合文字など）を `out` に入れる。空のセルなら空になる
    pub fn graphemes(&self, out: &mut Vec<char>) {
        out.clear();
        let mut buf = [0u32; 16];
        let mut b = ffi::GhosttyCodepointBuffer { ptr: buf.as_mut_ptr(), cap: buf.len(), len: 0 };
        let r = unsafe {
            ffi::ghostty_render_state_row_cells_get(self.raw, ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF, &mut b as *mut _ as *mut c_void)
        };
        if r == ffi::GHOSTTY_OUT_OF_SPACE {
            // 長い書記素（ZWJ の絵文字の列など）は大きい入れ物で取り直す
            let mut big = vec![0u32; b.len];
            let mut b2 = ffi::GhosttyCodepointBuffer { ptr: big.as_mut_ptr(), cap: big.len(), len: 0 };
            unsafe {
                ffi::ghostty_render_state_row_cells_get(self.raw, ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF, &mut b2 as *mut _ as *mut c_void)
            };
            out.extend(big[..b2.len.min(big.len())].iter().filter_map(|&c| char::from_u32(c)));
        } else if r == ffi::GHOSTTY_SUCCESS {
            out.extend(buf[..b.len.min(buf.len())].iter().filter_map(|&c| char::from_u32(c)));
        }
    }

    pub fn style(&self) -> Style {
        let mut s = Style::default();
        unsafe {
            ffi::ghostty_render_state_row_cells_get(self.raw, ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &mut s as *mut _ as *mut c_void)
        };
        s
    }

    /// 前景色（パレットは解決済み）。指定が無ければ `None`（既定の前景色で描く）
    pub fn fg(&self) -> Option<Rgb> {
        self.color(ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR)
    }

    /// 背景色（パレットは解決済み）。指定が無ければ `None`（既定の背景色で描く）
    pub fn bg(&self) -> Option<Rgb> {
        self.color(ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR)
    }

    fn color(&self, data: c_int) -> Option<Rgb> {
        let mut c = Rgb::default();
        let r = unsafe { ffi::ghostty_render_state_row_cells_get(self.raw, data, &mut c as *mut _ as *mut c_void) };
        (r == ffi::GHOSTTY_SUCCESS).then_some(c)
    }

    pub fn wide(&self) -> Wide {
        let mut cell: ffi::GhosttyCell = 0;
        let mut w: c_int = 0;
        unsafe {
            ffi::ghostty_render_state_row_cells_get(self.raw, ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW, &mut cell as *mut _ as *mut c_void);
            ffi::ghostty_cell_get(cell, ffi::GHOSTTY_CELL_DATA_WIDE, &mut w as *mut _ as *mut c_void);
        }
        match w {
            ffi::GHOSTTY_CELL_WIDE_WIDE => Wide::Wide,
            ffi::GHOSTTY_CELL_WIDE_SPACER_TAIL => Wide::SpacerTail,
            ffi::GHOSTTY_CELL_WIDE_SPACER_HEAD => Wide::SpacerHead,
            _ => Wide::Narrow,
        }
    }
}

// ---- キー ----

/// 特殊キー
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Enter,
    Backspace,
    Tab,
    Escape,
    /// F1〜F12（1 始まり）
    F(u8),
}

/// 修飾キー
#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// libghostty-vt のキーエンコーダ。端末のモード（DECCKM など）は [`KeyEncoder::encode`] のたびに端末から読む
pub struct KeyEncoder {
    raw: ffi::GhosttyKeyEncoder,
    event: ffi::GhosttyKeyEvent,
}

unsafe impl Send for KeyEncoder {}

impl KeyEncoder {
    pub fn new() -> Result<Self> {
        let mut raw = ptr::null_mut();
        let mut event = ptr::null_mut();
        unsafe {
            check(ffi::ghostty_key_encoder_new(ptr::null(), &mut raw))?;
            if let Err(e) = check(ffi::ghostty_key_event_new(ptr::null(), &mut event)) {
                ffi::ghostty_key_encoder_free(raw);
                return Err(e);
            }
        }
        Ok(Self { raw, event })
    }

    /// キーを押したときに PTY へ送る列。端末のロックを持って呼ぶ（モードを読むため）
    pub fn encode(&mut self, terminal: &Terminal, key: Key, mods: Mods) -> Vec<u8> {
        let k = match key {
            Key::Up => ffi::GHOSTTY_KEY_ARROW_UP,
            Key::Down => ffi::GHOSTTY_KEY_ARROW_DOWN,
            Key::Left => ffi::GHOSTTY_KEY_ARROW_LEFT,
            Key::Right => ffi::GHOSTTY_KEY_ARROW_RIGHT,
            Key::Home => ffi::GHOSTTY_KEY_HOME,
            Key::End => ffi::GHOSTTY_KEY_END,
            Key::PageUp => ffi::GHOSTTY_KEY_PAGE_UP,
            Key::PageDown => ffi::GHOSTTY_KEY_PAGE_DOWN,
            Key::Insert => ffi::GHOSTTY_KEY_INSERT,
            Key::Delete => ffi::GHOSTTY_KEY_DELETE,
            Key::Enter => ffi::GHOSTTY_KEY_ENTER,
            Key::Backspace => ffi::GHOSTTY_KEY_BACKSPACE,
            Key::Tab => ffi::GHOSTTY_KEY_TAB,
            Key::Escape => ffi::GHOSTTY_KEY_ESCAPE,
            Key::F(n) if (1..=12).contains(&n) => ffi::GHOSTTY_KEY_F1 + (n as c_int - 1),
            Key::F(_) => return Vec::new(),
        };
        let mut m = 0u16;
        if mods.shift {
            m |= ffi::GHOSTTY_MODS_SHIFT;
        }
        if mods.ctrl {
            m |= ffi::GHOSTTY_MODS_CTRL;
        }
        if mods.alt {
            m |= ffi::GHOSTTY_MODS_ALT;
        }
        let mut buf = [0u8; 64];
        let mut written = 0usize;
        unsafe {
            ffi::ghostty_key_encoder_set_from_terminal(self.raw, terminal.raw);
            ffi::ghostty_key_event_set_action(self.event, ffi::GHOSTTY_KEY_ACTION_PRESS);
            ffi::ghostty_key_event_set_key(self.event, k);
            ffi::ghostty_key_event_set_mods(self.event, m);
            if ffi::ghostty_key_encoder_encode(self.raw, self.event, buf.as_mut_ptr(), buf.len(), &mut written)
                != ffi::GHOSTTY_SUCCESS
            {
                return Vec::new();
            }
        }
        buf[..written.min(buf.len())].to_vec()
    }
}

impl Drop for KeyEncoder {
    fn drop(&mut self) {
        unsafe {
            ffi::ghostty_key_event_free(self.event);
            ffi::ghostty_key_encoder_free(self.raw);
        }
    }
}
