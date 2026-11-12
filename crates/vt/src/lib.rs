//! et-vt: libghostty-vt（VT の解釈）の FFI と安全なラッパ。
//!
//! - [`Terminal`]: 端末の状態。PTY から読んだバイト列を [`Terminal::vt_write`] で渡す。
//!   端末からの知らせ（PTY への応答・ベル・タイトル・カレントディレクトリ）は [`Effects`] で受ける
//! - [`RenderState`]: 描画用の写し。端末のロックを持っている間に [`RenderState::update`] し、
//!   ロックを外してから行とセルを読んで描く（docs/TODO.md T-006）
//! - [`KeyEncoder`]: 特殊キー（矢印・ファンクションキーなど）を端末のモードに合わせた VT の列にする
//! - 画像（Kitty graphics。T-088）: どこに何を描くかは [`RenderState::images`]（ロックの外で読める）、
//!   画素は [`Terminal::kitty_image`]（ロックの中で読む）。PNG のデコードは [`install_png_decoder`]（WIC）
//!
//! unsafe はこのクレートの中に閉じ込める。libghostty-vt の端末はスレッド安全ではないので、
//! 複数のスレッドから使うときは呼ぶ側が `Mutex` で守る（[`Terminal`] は `Send` だが `Sync` ではない）。

pub mod ffi;
mod png;
#[cfg(test)]
mod tests;

use std::ffi::{CStr, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::time::{Duration, Instant};

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

pub use png::{PngDecodeStats, install_png_decoder, png_decode_stats};

fn check(r: ffi::GhosttyResult) -> Result<()> {
    if r == ffi::GHOSTTY_SUCCESS { Ok(()) } else { Err(Error(r)) }
}

/// libghostty-vt の版などを JSON で返す（構造体の配置の照合に使う）
pub fn type_json() -> &'static str {
    unsafe { CStr::from_ptr(ffi::ghostty_type_json()) }.to_str().unwrap_or("")
}

/// libghostty-vt の版（`ghostty_build_info`）。取れなければ空
pub fn version() -> String {
    let mut s = ffi::GhosttyString { ptr: ptr::null(), len: 0 };
    let r = unsafe { ffi::ghostty_build_info(ffi::GHOSTTY_BUILD_INFO_VERSION_STRING, &mut s as *mut _ as *mut c_void) };
    if r != ffi::GHOSTTY_SUCCESS || s.ptr.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(s.ptr, s.len) }).into_owned()
}

/// 1 つのコードポイントが何桁か（端末が使う表と同じもの。eaw は 1）
pub fn codepoint_width(c: char) -> u8 {
    unsafe { ffi::ghostty_unicode_codepoint_width(c as u32) }
}

/// Kitty graphics の Unicode placeholder（`U+10EEEE`）。画像の場所を示すだけの文字
pub const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// テキストから Kitty graphics の Unicode placeholder を除く（T-094）。placeholder の後ろに付く、画像の行・桁・ID の
/// 上位を表す結合文字（幅 0）も一緒に除く。画像を含む行をコピーしたときに私用領域の文字が混じらないように
pub fn strip_kitty_placeholders(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(KITTY_PLACEHOLDER) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut after_placeholder = false;
    for c in text.chars() {
        if c == KITTY_PLACEHOLDER {
            after_placeholder = true;
            continue;
        }
        if after_placeholder && !c.is_ascii() && codepoint_width(c) == 0 {
            continue;
        }
        after_placeholder = false;
        out.push(c);
    }
    std::borrow::Cow::Owned(out)
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
    /// アプリがクリップボードへ書こうとした（OSC 52 など。T-039）。テキストの表現だけを渡す。
    /// 受け入れたら true（OSC 5522 なら答えに使う。OSC 52 は答えを捨てる）
    fn clipboard_write(&mut self, _text: &str) -> bool {
        false
    }
    /// アプリがクリップボードを読もうとした（OSC 52 の `?`）。[`Terminal::set_clipboard_read_enabled`] で
    /// 有効にしたときだけ呼ばれる。`None` なら読めなかった（空として答える）
    fn clipboard_read(&mut self) -> Option<String> {
        None
    }
    /// デスクトップ通知（OSC 9 / 777）。タイトルが無ければ空
    fn desktop_notification(&mut self, _title: &str, _body: &str) {}
    /// 進捗（OSC 9;4）。`percent` は省かれていれば `None`
    fn progress(&mut self, _state: ProgressState, _percent: Option<u8>) {}
    /// libghostty-vt が扱わない OSC（`ESC ]` と終端の間。番号を含む）。長すぎて切れたものは `truncated`（T-118）
    fn unknown_osc(&mut self, _content: &[u8], _truncated: bool) {}
}

/// 進捗の状態（OSC 9;4。`GhosttyTerminalProgressState`）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressState {
    /// 表示を消す
    Remove,
    /// 進捗（割合）
    Set,
    /// 失敗
    Error,
    /// 割合の分からない進捗
    Indeterminate,
    /// 一時停止
    Pause,
}

impl ProgressState {
    /// Lisp に渡す名前
    pub fn name(self) -> &'static str {
        match self {
            Self::Remove => "remove",
            Self::Set => "set",
            Self::Error => "error",
            Self::Indeterminate => "indeterminate",
            Self::Pause => "pause",
        }
    }
}

/// 何もしない [`Effects`]
pub struct NoEffects;
impl Effects for NoEffects {}

/// コールバックに渡す userdata。`Box::into_raw` したものを端末に渡し、[`Terminal`] の `Drop` で戻す
struct Inner {
    effects: Box<dyn Effects>,
    /// `CSI 16 t` などで答えるセルの画素の大きさ。0 だと Kitty graphics を使わないアプリがあるので 1 以上にする
    cell_px: (u32, u32),
    /// XTVERSION（`CSI > q`）の答え（T-071）。コールバックが指すので、設定したら `Terminal` が閉じるまで変えない
    xtversion: String,
    /// 同期出力（DEC 2026）の hold の始まった時刻（T-074）。hold の間だけ `Some`
    hold_started: Option<Instant>,
    /// hold が始まった瞬間に写した render state（UI が [`Terminal::take_held_render_state`] で受け取る）
    hold_capture: Option<RenderState>,
    /// hold の render state に写す余分の行（ペインの行が端末より多いとき）
    hold_overscan: (u16, u16),
    /// libghostty-vt の組み込みの 256 色（最初に既定のパレットを変える前に読んで覚える。T-034）
    builtin_palette: Option<[Rgb; 256]>,
    /// 配色がダークか（`CSI ? 996 n` の答えと mode 2031 の知らせ。T-102）
    dark: bool,
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
    /// スクロールバックの先頭から数えた行（画面の座標）を、ビューポートの先頭にする
    Row(u64),
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
    pub const MOUSE_ALTERNATE_SCROLL: Mode = Mode { value: 1007, ansi: false };
    pub const BRACKETED_PASTE: Mode = Mode { value: 2004, ansi: false };
    pub const SYNCHRONIZED_OUTPUT: Mode = Mode { value: 2026, ansi: false };
    /// 配色が変わったら知らせる（T-102）
    pub const REPORT_COLOR_SCHEME: Mode = Mode { value: 2031, ansi: false };
    pub const GRAPHEME_CLUSTER: Mode = Mode { value: 2027, ansi: false };

    fn raw(self) -> u16 {
        (self.value & 0x7fff) | ((self.ansi as u16) << 15)
    }
}

/// 端末 1 つ
pub struct Terminal {
    raw: ffi::GhosttyTerminal,
    inner: *mut Inner,
    /// ドラッグの選択の解釈（最初に使うときに作る）
    gesture: Option<Gesture>,
}

/// 画面の中のセルを指すもの。次に端末が変わるまで有効
#[derive(Clone, Copy, Debug)]
pub struct GridRef(ffi::GhosttyGridRef);

/// 選択の範囲（[`Terminal::gesture_press`] など）。次に端末が変わるまで有効
#[derive(Clone, Copy, Debug)]
pub struct Selection(ffi::GhosttySelection);

/// ドラッグを解釈するための表示の形（画素）
#[derive(Clone, Copy, Debug, Default)]
pub struct Geometry {
    pub columns: u32,
    pub cell_width: u32,
    pub padding_left: u32,
    pub screen_height: u32,
}

/// 端末が変わっても同じセルを指し続ける位置（[`Terminal::track`]）。**端末のロックを持っている間に読み、落とすこと**
pub struct TrackedRef {
    raw: ffi::GhosttyTrackedGridRef,
}

unsafe impl Send for TrackedRef {}

impl TrackedRef {
    /// 今の画面の座標での行。スクロールバックの上限で行が消えていれば `None`。
    /// `_terminal` はロックを持っていることの確認（同じ端末を渡す）
    pub fn screen_row(&self, _terminal: &Terminal) -> Option<u32> {
        let mut c = ffi::GhosttyPointCoordinate::default();
        let r = unsafe { ffi::ghostty_tracked_grid_ref_point(self.raw, ffi::GHOSTTY_POINT_TAG_SCREEN, &mut c) };
        (r == ffi::GHOSTTY_SUCCESS).then_some(c.y)
    }
}

impl Drop for TrackedRef {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_tracked_grid_ref_free(self.raw) };
    }
}

// libghostty-vt の端末は別のスレッドへ動かしてよい（同時に触らなければよい）。同時に触らないことは
// `&mut self` と、呼ぶ側の `Mutex` で守る
unsafe impl Send for Terminal {}

impl Terminal {
    /// 端末を作る。`effects` が端末からの知らせを受ける
    pub fn new(cols: u16, rows: u16, effects: Box<dyn Effects>) -> Result<Self> {
        let mut raw = ptr::null_mut();
        check(unsafe { ffi::ghostty_terminal_new(ptr::null(), &mut raw, cols.max(1), rows.max(1)) })?;
        let inner = Box::into_raw(Box::new(Inner {
            effects,
            cell_px: (1, 1),
            xtversion: String::new(),
            hold_started: None,
            hold_capture: None,
            hold_overscan: (0, 0),
            builtin_palette: None,
            dark: false,
        }));
        let t = Self { raw, inner, gesture: None };
        unsafe {
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_USERDATA, inner as *const c_void))?;
            let write_pty: ffi::GhosttyTerminalWritePtyFn = cb_write_pty;
            let bell: ffi::GhosttyTerminalBellFn = cb_bell;
            let title: ffi::GhosttyTerminalTitleChangedFn = cb_title_changed;
            let pwd: ffi::GhosttyTerminalPwdChangedFn = cb_pwd_changed;
            let size: ffi::GhosttyTerminalSizeFn = cb_size;
            let hold: ffi::GhosttyTerminalRenderHoldFn = cb_render_hold;
            let clip: ffi::GhosttyTerminalClipboardWriteFn = cb_clipboard_write;
            let notify: ffi::GhosttyTerminalDesktopNotificationFn = cb_desktop_notification;
            let progress: ffi::GhosttyTerminalProgressReportFn = cb_progress_report;
            let scheme: ffi::GhosttyTerminalColorSchemeFn = cb_color_scheme;
            let unknown: ffi::GhosttyTerminalUnknownSequenceFn = cb_unknown_sequence;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_WRITE_PTY, write_pty as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_BELL, bell as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_TITLE_CHANGED, title as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_PWD_CHANGED, pwd as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_SIZE, size as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_RENDER_HOLD, hold as *const c_void))?;
            // OSC 52 の書き込み・OSC 9 / 777・OSC 9;4（T-039）。読み取りは既定で入れない（set_clipboard_read_enabled）
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE, clip as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_DESKTOP_NOTIFICATION, notify as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_PROGRESS_REPORT, progress as *const c_void))?;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_COLOR_SCHEME, scheme as *const c_void))?;
            // 未知の OSC（OSC 51;E などの EmbTerm 独自のもの。T-118）。上限まで中身を取っておく
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_UNKNOWN_SEQUENCE, unknown as *const c_void))?;
            let max: usize = UNKNOWN_MAX_BYTES;
            check(ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_UNKNOWN_MAX_BYTES, &max as *const usize as *const c_void))?;
        }
        Ok(t)
    }

    /// クリップボードの読み取り（OSC 52 の `?`）を受けるか（T-039）。**既定は受けない**（コールバックを入れないので、
    /// libghostty-vt は OSC 52 の読み取りを無視し、OSC 5522 の読み取りを EPERM で断る）。
    /// 受けるときは [`Effects::clipboard_read`] が呼ばれる
    pub fn set_clipboard_read_enabled(&mut self, enabled: bool) -> Result<()> {
        let f: ffi::GhosttyTerminalClipboardReadFn = cb_clipboard_read;
        let value = if enabled { f as *const c_void } else { ptr::null() };
        check(unsafe { ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_CLIPBOARD_READ, value) })
    }

    /// 既定の 16 色を変える（T-034）。`None` の色は libghostty-vt の組み込みの色。16〜255 は組み込みのまま。
    /// アプリが OSC 4 で変えた色はそのまま残る（libghostty-vt は既定と上書きを分けて持つ）
    pub fn set_default_palette16(&mut self, colors: &[Option<Rgb>; 16]) -> Result<()> {
        let inner = unsafe { &mut *self.inner };
        let base = match inner.builtin_palette {
            Some(p) => p,
            None => {
                let mut p = [Rgb::default(); 256];
                check(unsafe {
                    ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_COLOR_PALETTE_DEFAULT, p.as_mut_ptr() as *mut c_void)
                })?;
                inner.builtin_palette = Some(p);
                p
            }
        };
        let mut full = base;
        for (dst, c) in full.iter_mut().zip(colors) {
            if let Some(c) = c {
                *dst = *c;
            }
        }
        check(unsafe { ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, full.as_ptr() as *const c_void) })
    }

    /// DECSCUSR の既定（`CSI 0 q` で戻る形と点滅。T-035）。アプリが DECSCUSR で形を決めていない間は、今のカーソルもこれに従う
    /// （決めたあとはアプリの形のまま。libghostty-vt の動き。tests.rs で確かめた）
    pub fn set_default_cursor(&mut self, shape: CursorShape, blink: bool) -> Result<()> {
        let style: c_int = match shape {
            CursorShape::Bar => ffi::GHOSTTY_CURSOR_STYLE_BAR,
            CursorShape::Block => ffi::GHOSTTY_CURSOR_STYLE_BLOCK,
            CursorShape::Underline => ffi::GHOSTTY_CURSOR_STYLE_UNDERLINE,
            CursorShape::BlockHollow => ffi::GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW,
        };
        unsafe {
            check(ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_STYLE, &style as *const _ as *const c_void))?;
            check(ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_BLINK, &blink as *const _ as *const c_void))
        }
    }

    /// セルのハイパーリンク（OSC 8）の URI。無ければ `None`（T-039）
    pub fn hyperlink_uri(&self, at: GridRef) -> Option<String> {
        let mut len = 0usize;
        let r = unsafe { ffi::ghostty_grid_ref_hyperlink_uri(&at.0, ptr::null_mut(), 0, &mut len) };
        if r == ffi::GHOSTTY_SUCCESS || len == 0 {
            return None;
        }
        if r != ffi::GHOSTTY_OUT_OF_SPACE {
            return None;
        }
        let mut buf = vec![0u8; len];
        let r = unsafe { ffi::ghostty_grid_ref_hyperlink_uri(&at.0, buf.as_mut_ptr(), buf.len(), &mut len) };
        (r == ffi::GHOSTTY_SUCCESS && len > 0).then(|| String::from_utf8_lossy(&buf[..len.min(buf.len())]).into_owned())
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

    /// [`Terminal::resize`] で渡したセルの画素の大きさ（画像の配置はこの大きさで計算される）
    pub fn cell_px(&self) -> (u32, u32) {
        unsafe { (*self.inner).cell_px }
    }

    /// Kitty graphics の画像を置ける量（バイト。主画面と代替画面でそれぞれ）。0 にすると Kitty graphics を止める
    pub fn set_kitty_image_storage_limit(&mut self, bytes: u64) -> Result<()> {
        check(unsafe {
            ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, &bytes as *const _ as *const c_void)
        })
    }

    /// Kitty graphics のファイルでの転送（`t=f`）を許すか。既定は不許可
    /// Kitty graphics の一時ファイル（`t=t`）を `dir` の中のものに限って読む（`None` なら読まない。T-093）。
    /// libghostty-vt は読んだあとそのファイルを消す（Kitty の決まり）
    pub fn set_kitty_medium_temp_file(&mut self, dir: Option<&str>) -> Result<()> {
        let s = dir.map(|d| ffi::GhosttyString { ptr: d.as_ptr(), len: d.len() });
        let p = s.as_ref().map_or(ptr::null(), |s| s as *const _ as *const c_void);
        check(unsafe { ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE, p) })
    }

    pub fn set_kitty_medium_file(&mut self, allow: bool) -> Result<()> {
        check(unsafe {
            ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE, &allow as *const _ as *const c_void)
        })
    }

    /// Kitty graphics のアニメーション（`a=f` / `a=a`）を `now_ms` の時点まで進める（T-092）。描く前に毎回 1 回呼ぶ。
    /// 再生中のものがあれば次のフレームの時刻（同じ時計の ms）を返す。進んだ画像は世代が変わる
    pub fn kitty_animation_tick(&mut self, now_ms: u64) -> Option<u64> {
        let mut graphics: ffi::GhosttyKittyGraphics = ptr::null_mut();
        let r = unsafe { ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS, &mut graphics as *mut _ as *mut c_void) };
        if r != ffi::GHOSTTY_SUCCESS || graphics.is_null() {
            return None;
        }
        let mut next = 0u64;
        (unsafe { ffi::ghostty_kitty_graphics_animation_tick(graphics, now_ms, &mut next) } == ffi::GHOSTTY_SUCCESS).then_some(next)
    }

    /// Kitty graphics の画像を引く。`generation` が [`ImagePlacement::generation`] と同じなら、同じ画素である。
    /// 無ければ（消された、画素がまだ届いていない）`None`。返す値の画素は端末の中を指すので、次に端末が変わるまで有効
    pub fn kitty_image(&self, id: u32) -> Option<KittyImage<'_>> {
        let mut graphics: ffi::GhosttyKittyGraphics = ptr::null_mut();
        let r = unsafe { ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS, &mut graphics as *mut _ as *mut c_void) };
        if r != ffi::GHOSTTY_SUCCESS || graphics.is_null() {
            return None;
        }
        let image = unsafe { ffi::ghostty_kitty_graphics_image(graphics, id) };
        if image.is_null() {
            return None;
        }
        let (mut width, mut height, mut format, mut generation) = (0u32, 0u32, 0 as c_int, 0u64);
        let mut pixels = ffi::GhosttyString { ptr: ptr::null(), len: 0 };
        let keys = [
            ffi::GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_WIDTH,
            ffi::GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_HEIGHT,
            ffi::GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_FORMAT,
            ffi::GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_GENERATION,
            ffi::GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_PIXELS,
        ];
        let mut values: [*mut c_void; 5] = [
            (&mut width as *mut u32).cast(),
            (&mut height as *mut u32).cast(),
            (&mut format as *mut c_int).cast(),
            (&mut generation as *mut u64).cast(),
            (&mut pixels as *mut ffi::GhosttyString).cast(),
        ];
        let r = unsafe { ffi::ghostty_kitty_graphics_image_get_multi(image, keys.len(), keys.as_ptr(), values.as_mut_ptr(), ptr::null_mut()) };
        // 画素がまだ届いていなければ NO_VALUE（ここでは「描くものが無い」と同じ）
        if r != ffi::GHOSTTY_SUCCESS || pixels.ptr.is_null() {
            return None;
        }
        let format = match format {
            ffi::GHOSTTY_KITTY_IMAGE_FORMAT_RGB => ImageFormat::Rgb,
            ffi::GHOSTTY_KITTY_IMAGE_FORMAT_RGBA => ImageFormat::Rgba,
            ffi::GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA => ImageFormat::GrayAlpha,
            ffi::GHOSTTY_KITTY_IMAGE_FORMAT_GRAY => ImageFormat::Gray,
            _ => return None,
        };
        Some(KittyImage { width, height, format, generation, pixels: unsafe { std::slice::from_raw_parts(pixels.ptr, pixels.len) } })
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

    /// 今の画面の Kitty keyboard protocol のフラグ（GhosttyKittyKeyFlags。0 なら無効）。
    /// スタックは主画面と代替画面で別々に持つ（libghostty-vt の仕様）
    pub fn kitty_keyboard_flags(&self) -> u8 {
        let mut v: u8 = 0;
        unsafe {
            ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_KITTY_KEYBOARD_FLAGS, &mut v as *mut _ as *mut c_void)
        };
        v
    }

    /// 同期出力（DEC 2026）の hold の残り時間（T-074）。hold していなければ `None`。
    /// `timeout` を過ぎていれば hold を解いて `None` を返す（アプリが落ちて `?2026l` が来ないとき）
    pub fn render_hold(&mut self, timeout: Duration) -> Option<Duration> {
        let inner = unsafe { &mut *self.inner };
        let started = inner.hold_started?;
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            // このときコールバックは呼ばれない（ヘッダの注）
            let c = ffi::GhosttyTerminalModeConfig { mode: Mode::SYNCHRONIZED_OUTPUT.raw(), value: false };
            unsafe { ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_MODE, &c as *const _ as *const c_void) };
            inner.hold_started = None;
            return None;
        }
        Some(timeout - elapsed)
    }

    /// hold が始まった瞬間に写した render state を受け取る（無ければ `None`）。
    /// **受け取った render state は、それまで使っていた render state の代わりにする**（端末の dirty は
    /// この render state が受け取ったので、古い render state へ差分は来ない）
    pub fn take_held_render_state(&mut self) -> Option<RenderState> {
        unsafe { (*self.inner).hold_capture.take() }
    }

    /// hold の render state に写す余分の行（[`RenderState::set_overscan`] と同じ値を渡す）
    pub fn set_hold_overscan(&mut self, above: u16, below: u16) {
        unsafe { (*self.inner).hold_overscan = (above, below) };
    }

    /// XTVERSION（`CSI > q`）で答える文字列（T-071）。ヘッダの既定は "libghostty"
    pub fn set_xtversion(&mut self, version: &str) -> Result<()> {
        unsafe { (*self.inner).xtversion = version.to_string() };
        let f: ffi::GhosttyTerminalXtversionFn = cb_xtversion;
        check(unsafe { ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_XTVERSION, f as *const c_void) })
    }

    /// スクロールバックを含めた全体の行数（[`Scrollbar::total`]。[`Scroll::Row`] の行の範囲）
    pub fn total_rows(&self) -> u64 {
        self.scrollbar().total
    }

    /// マウスの報告（X10 / 通常 / ボタン / すべて）のどれかが有効か
    pub fn mouse_tracking(&self) -> bool {
        let mut v = false;
        unsafe { ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING, &mut v as *mut _ as *mut c_void) };
        v
    }

    /// 選択があるか
    pub fn has_selection(&self) -> bool {
        let mut sel = empty_selection();
        let r = unsafe { ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_SELECTION, &mut sel as *mut _ as *mut c_void) };
        r == ffi::GHOSTTY_SUCCESS
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
            Scroll::Row(r) => (ffi::GHOSTTY_TERMINAL_SCROLL_VIEWPORT_ROW, r as isize),
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

    /// スクロールバックのバイト数の上限（`usize::MAX` で無制限）。**既定は 10,000 バイトと小さい**ので、
    /// 行数の上限（[`Terminal::set_scrollback_max_lines`]）で決めたいときは無制限にする
    pub fn set_scrollback_max_bytes(&mut self, bytes: usize) -> Result<()> {
        check(unsafe {
            ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &bytes as *const _ as *const c_void)
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

    /// 配色がダークかを決める（T-102）。`CSI ? 996 n` にはこれで答え、変わったときにアプリが mode 2031 を立てていれば
    /// `CSI ? 997 ; 1 n`（ダーク）/ `; 2 n`（ライト）を PTY に送る
    pub fn set_dark(&mut self, dark: bool) {
        let i = unsafe { &mut *self.inner };
        if i.dark == dark {
            return;
        }
        i.dark = dark;
        if self.mode(Mode::REPORT_COLOR_SCHEME) {
            let report: &[u8] = if dark { b"\x1b[?997;1n" } else { b"\x1b[?997;2n" };
            i.effects.write_pty(report);
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

    /// 画面の座標（スクロールバックの先頭からの行）の (桁, 行) のセルを指す。次に端末が変わるまで有効
    pub fn grid_ref(&self, x: u16, y: u32) -> Result<GridRef> {
        let point = ffi::GhosttyPoint {
            tag: ffi::GHOSTTY_POINT_TAG_SCREEN,
            value: ffi::GhosttyPointValue { coordinate: ffi::GhosttyPointCoordinate { x, y } },
        };
        let mut r = ffi::GhosttyGridRef { node: ptr::null_mut(), x: 0, y: 0 };
        check(unsafe { ffi::ghostty_terminal_grid_ref(self.raw, point, &mut r) })?;
        Ok(GridRef(r))
    }

    /// 画面の座標（スクロールバックの先頭からの行）の行 `y` が、次の行へ折り返している（soft wrap）か（T-086）。
    /// 行が無ければ false
    pub fn row_wraps(&self, y: u32) -> bool {
        let Ok(r) = self.grid_ref(0, y) else { return false };
        let mut row: ffi::GhosttyRow = 0;
        let mut wrap = false;
        unsafe {
            ffi::ghostty_grid_ref_row(&r.0, &mut row) == ffi::GHOSTTY_SUCCESS
                && ffi::ghostty_row_get(row, ffi::GHOSTTY_ROW_DATA_WRAP, &mut wrap as *mut bool as *mut c_void) == ffi::GHOSTTY_SUCCESS
                && wrap
        }
    }

    /// 画面の座標の (桁, 行) のセルを、端末が変わっても追いかけて指し続ける（[`TrackedRef`]）。
    /// スクロールバックの上限で行が消えると、その [`TrackedRef`] は値を失う
    pub fn track(&self, x: u16, y: u32) -> Result<TrackedRef> {
        let point = ffi::GhosttyPoint {
            tag: ffi::GHOSTTY_POINT_TAG_SCREEN,
            value: ffi::GhosttyPointValue { coordinate: ffi::GhosttyPointCoordinate { x, y } },
        };
        let mut raw: ffi::GhosttyTrackedGridRef = ptr::null_mut();
        check(unsafe { ffi::ghostty_terminal_grid_ref_track(self.raw, point, &mut raw) })?;
        Ok(TrackedRef { raw })
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

impl Terminal {
    /// 選択（[`Selection`]）を端末の現在の選択にする。`None` で消す
    pub fn apply_selection(&mut self, selection: Option<Selection>) -> Result<()> {
        match selection {
            None => check(unsafe { ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_SELECTION, ptr::null()) }),
            Some(s) => check(unsafe {
                ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_SELECTION, &s.0 as *const _ as *const c_void)
            }),
        }
    }

    fn gesture(&mut self) -> Result<&mut Gesture> {
        if self.gesture.is_none() {
            self.gesture = Some(Gesture::new()?);
        }
        Ok(self.gesture.as_mut().expect("直前に作った"))
    }

    /// 左ボタンを押した。単語・行の選択（ダブル・トリプルクリック）なら選択を返す。`time_ns` は単調な時刻、
    /// `repeat_ns` はクリックの繰り返しとみなす間隔
    pub fn gesture_press(&mut self, at: GridRef, pos: (f64, f64), time_ns: u64, repeat_ns: u64) -> Result<Option<Selection>> {
        let raw = self.raw;
        let g = self.gesture()?;
        let event = g.press;
        unsafe {
            let position = ffi::GhosttySurfacePosition { x: pos.0, y: pos.1 };
            check(ffi::ghostty_selection_gesture_event_set(event, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REF, &at.0 as *const _ as *const c_void))?;
            check(ffi::ghostty_selection_gesture_event_set(event, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_POSITION, &position as *const _ as *const c_void))?;
            check(ffi::ghostty_selection_gesture_event_set(event, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_TIME_NS, &time_ns as *const _ as *const c_void))?;
            check(ffi::ghostty_selection_gesture_event_set(event, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REPEAT_INTERVAL_NS, &repeat_ns as *const _ as *const c_void))?;
        }
        gesture_result(g.raw, raw, event)
    }

    /// 押したままマウスを動かした
    pub fn gesture_drag(&mut self, at: GridRef, pos: (f64, f64), geometry: Geometry) -> Result<Option<Selection>> {
        let raw = self.raw;
        let g = self.gesture()?;
        let event = g.drag;
        let geo = ffi::GhosttySelectionGestureGeometry {
            columns: geometry.columns.max(1),
            cell_width: geometry.cell_width.max(1),
            padding_left: geometry.padding_left,
            screen_height: geometry.screen_height.max(1),
        };
        unsafe {
            let position = ffi::GhosttySurfacePosition { x: pos.0, y: pos.1 };
            check(ffi::ghostty_selection_gesture_event_set(event, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REF, &at.0 as *const _ as *const c_void))?;
            check(ffi::ghostty_selection_gesture_event_set(event, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_POSITION, &position as *const _ as *const c_void))?;
            check(ffi::ghostty_selection_gesture_event_set(event, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_GEOMETRY, &geo as *const _ as *const c_void))?;
        }
        gesture_result(g.raw, raw, event)
    }

    /// ボタンを離した
    pub fn gesture_release(&mut self, at: Option<GridRef>) -> Result<()> {
        let raw = self.raw;
        let g = self.gesture()?;
        let event = g.release;
        unsafe {
            let value = at.as_ref().map_or(ptr::null(), |r| &r.0 as *const _ as *const c_void);
            check(ffi::ghostty_selection_gesture_event_set(event, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REF, value))?;
        }
        gesture_result(g.raw, raw, event).map(|_| ())
    }

    /// テキストを取り出す。`selection` が true なら今の選択を、false なら画面全体（スクロールバックを含む）。
    /// `unwrap` は端末が折り返した行をつなげる（クリップボードに入れるときの形）。選択が無ければ `None`。
    /// **端末のロックを持っている間に呼ぶ**
    pub fn format_plain(&self, selection: bool, unwrap: bool) -> Result<Option<String>> {
        let mut sel = empty_selection();
        if selection {
            let r = unsafe { ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_SELECTION, &mut sel as *mut _ as *mut c_void) };
            if r == ffi::GHOSTTY_NO_VALUE {
                return Ok(None);
            }
            check(r)?;
        }
        let mut f: ffi::GhosttyFormatter = ptr::null_mut();
        check(unsafe { ffi::ghostty_formatter_terminal_new(ptr::null(), &mut f, self.raw) })?;
        let result = (|| unsafe {
            let emit = ffi::GHOSTTY_FORMATTER_FORMAT_PLAIN;
            check(ffi::ghostty_formatter_set(f, ffi::GHOSTTY_FORMATTER_OPT_EMIT, &emit as *const _ as *const c_void))?;
            let trim = true;
            check(ffi::ghostty_formatter_set(f, ffi::GHOSTTY_FORMATTER_OPT_TRIM, &trim as *const _ as *const c_void))?;
            check(ffi::ghostty_formatter_set(f, ffi::GHOSTTY_FORMATTER_OPT_UNWRAP, &unwrap as *const _ as *const c_void))?;
            if selection {
                check(ffi::ghostty_formatter_set(f, ffi::GHOSTTY_FORMATTER_OPT_SELECTION, &sel as *const _ as *const c_void))?;
            }
            let mut buf: *mut u8 = ptr::null_mut();
            let mut len = 0usize;
            check(ffi::ghostty_formatter_format_alloc(f, ptr::null(), &mut buf, &mut len))?;
            let text = if buf.is_null() || len == 0 {
                String::new()
            } else {
                strip_kitty_placeholders(&String::from_utf8_lossy(std::slice::from_raw_parts(buf, len))).into_owned()
            };
            ffi::ghostty_free(ptr::null(), buf, len);
            Ok(text)
        })();
        unsafe { ffi::ghostty_formatter_free(f) };
        result.map(Some)
    }
}

/// 貼り付けの安全確認（ghostty_paste_is_safe）。改行やブラケットペーストの終わりの列を含むと false
pub fn paste_is_safe(text: &str) -> bool {
    unsafe { ffi::ghostty_paste_is_safe(text.as_ptr() as *const std::ffi::c_char, text.len()) }
}

fn empty_selection() -> ffi::GhosttySelection {
    ffi::GhosttySelection {
        start: ffi::GhosttyGridRef { node: ptr::null_mut(), x: 0, y: 0 },
        end: ffi::GhosttyGridRef { node: ptr::null_mut(), x: 0, y: 0 },
        rectangle: false,
    }
}

fn gesture_result(gesture: ffi::GhosttySelectionGesture, terminal: ffi::GhosttyTerminal, event: ffi::GhosttySelectionGestureEvent) -> Result<Option<Selection>> {
    let mut sel = empty_selection();
    let r = unsafe { ffi::ghostty_selection_gesture_event(gesture, terminal, event, &mut sel) };
    match r {
        ffi::GHOSTTY_NO_VALUE => Ok(None),
        _ => {
            check(r)?;
            Ok(Some(Selection(sel)))
        }
    }
}

/// 選択のドラッグを解釈する状態（端末につき 1 つ）
struct Gesture {
    raw: ffi::GhosttySelectionGesture,
    press: ffi::GhosttySelectionGestureEvent,
    drag: ffi::GhosttySelectionGestureEvent,
    release: ffi::GhosttySelectionGestureEvent,
}

impl Gesture {
    fn new() -> Result<Self> {
        let mut g = Self { raw: ptr::null_mut(), press: ptr::null_mut(), drag: ptr::null_mut(), release: ptr::null_mut() };
        unsafe {
            if let Err(e) = (|| {
                check(ffi::ghostty_selection_gesture_new(ptr::null(), &mut g.raw))?;
                check(ffi::ghostty_selection_gesture_event_new(ptr::null(), &mut g.press, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_PRESS))?;
                check(ffi::ghostty_selection_gesture_event_new(ptr::null(), &mut g.drag, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_DRAG))?;
                check(ffi::ghostty_selection_gesture_event_new(ptr::null(), &mut g.release, ffi::GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_RELEASE))?;
                Ok::<(), Error>(())
            })() {
                g.free(ptr::null_mut());
                return Err(e);
            }
        }
        Ok(g)
    }

    /// `terminal` が生きていれば渡す（無ければ NULL）
    unsafe fn free(&mut self, terminal: ffi::GhosttyTerminal) {
        unsafe {
            ffi::ghostty_selection_gesture_event_free(self.release);
            ffi::ghostty_selection_gesture_event_free(self.drag);
            ffi::ghostty_selection_gesture_event_free(self.press);
            ffi::ghostty_selection_gesture_free(self.raw, terminal);
        }
    }
}

/// マウスの出来事を VT の列にするもの（マウスの報告が有効なアプリのとき）
pub struct MouseEncoder {
    raw: ffi::GhosttyMouseEncoder,
    event: ffi::GhosttyMouseEvent,
}

unsafe impl Send for MouseEncoder {}

/// マウスの出来事の種類
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// マウスのボタン
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// ホイールを上へ
    WheelUp,
    /// ホイールを下へ
    WheelDown,
}

/// エンコーダに渡す大きさの情報（画素）
#[derive(Clone, Copy, Debug, Default)]
pub struct MouseGeometry {
    pub screen_width: u32,
    pub screen_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding: u32,
}

impl MouseEncoder {
    pub fn new() -> Result<Self> {
        let mut raw = ptr::null_mut();
        let mut event = ptr::null_mut();
        unsafe {
            check(ffi::ghostty_mouse_encoder_new(ptr::null(), &mut raw))?;
            if let Err(e) = check(ffi::ghostty_mouse_event_new(ptr::null(), &mut event)) {
                ffi::ghostty_mouse_encoder_free(raw);
                return Err(e);
            }
        }
        let track = true;
        unsafe { ffi::ghostty_mouse_encoder_set(raw, ffi::GHOSTTY_MOUSE_ENCODER_OPT_TRACK_LAST_CELL, &track as *const _ as *const c_void) };
        Ok(Self { raw, event })
    }

    /// 出来事を VT の列にする。端末のロックを持って呼ぶ（モードを読むため）。何も送らないときは空
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        terminal: &Terminal,
        geometry: MouseGeometry,
        action: MouseAction,
        button: Option<MouseButton>,
        any_pressed: bool,
        pos: (f32, f32),
        mods: Mods,
    ) -> Vec<u8> {
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
        let size = ffi::GhosttyMouseEncoderSize {
            size: std::mem::size_of::<ffi::GhosttyMouseEncoderSize>(),
            screen_width: geometry.screen_width.max(1),
            screen_height: geometry.screen_height.max(1),
            cell_width: geometry.cell_width.max(1),
            cell_height: geometry.cell_height.max(1),
            padding_top: geometry.padding,
            padding_bottom: geometry.padding,
            padding_right: geometry.padding,
            padding_left: geometry.padding,
        };
        let mut buf = [0u8; 128];
        let mut written = 0usize;
        unsafe {
            ffi::ghostty_mouse_encoder_set_from_terminal(self.raw, terminal.raw);
            ffi::ghostty_mouse_encoder_set(self.raw, ffi::GHOSTTY_MOUSE_ENCODER_OPT_SIZE, &size as *const _ as *const c_void);
            let any = any_pressed;
            ffi::ghostty_mouse_encoder_set(self.raw, ffi::GHOSTTY_MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED, &any as *const _ as *const c_void);
            ffi::ghostty_mouse_event_set_action(
                self.event,
                match action {
                    MouseAction::Press => ffi::GHOSTTY_MOUSE_ACTION_PRESS,
                    MouseAction::Release => ffi::GHOSTTY_MOUSE_ACTION_RELEASE,
                    MouseAction::Motion => ffi::GHOSTTY_MOUSE_ACTION_MOTION,
                },
            );
            match button {
                Some(b) => {
                    ffi::ghostty_mouse_event_set_button(
                        self.event,
                        match b {
                            MouseButton::Left => ffi::GHOSTTY_MOUSE_BUTTON_LEFT,
                            MouseButton::Right => ffi::GHOSTTY_MOUSE_BUTTON_RIGHT,
                            MouseButton::Middle => ffi::GHOSTTY_MOUSE_BUTTON_MIDDLE,
                            MouseButton::WheelUp => ffi::GHOSTTY_MOUSE_BUTTON_FOUR,
                            MouseButton::WheelDown => ffi::GHOSTTY_MOUSE_BUTTON_FIVE,
                        },
                    );
                }
                None => {
                    ffi::ghostty_mouse_event_clear_button(self.event);
                }
            }
            ffi::ghostty_mouse_event_set_mods(self.event, m);
            ffi::ghostty_mouse_event_set_position(self.event, ffi::GhosttyMousePosition { x: pos.0, y: pos.1 });
            if ffi::ghostty_mouse_encoder_encode(self.raw, self.event, buf.as_mut_ptr(), buf.len(), &mut written) != ffi::GHOSTTY_SUCCESS {
                return Vec::new();
            }
        }
        buf[..written.min(buf.len())].to_vec()
    }
}

impl Drop for MouseEncoder {
    fn drop(&mut self) {
        unsafe {
            ffi::ghostty_mouse_event_free(self.event);
            ffi::ghostty_mouse_encoder_free(self.raw);
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            if let Some(mut g) = self.gesture.take() {
                g.free(self.raw);
            }
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

unsafe fn ghostty_str<'a>(s: &ffi::GhosttyString) -> &'a [u8] {
    if s.ptr.is_null() || s.len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(s.ptr, s.len) } }
}

/// クリップボードへの書き込み（OSC 52 / OSC 1337 Copy / OSC 5522。T-039）。テキストの表現を 1 つ選んで渡す。
/// 答えはコールバックの中で返す（返さなければ拒んだことになる）
unsafe extern "C" fn cb_clipboard_write(_t: ffi::GhosttyTerminal, userdata: *mut c_void, write: *const ffi::GhosttyClipboardWrite) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        let (Some(i), Some(w)) = (inner(userdata), write.as_ref()) else { return };
        let contents: &[ffi::GhosttyClipboardContent] =
            if w.contents.is_null() || w.contents_len == 0 { &[] } else { std::slice::from_raw_parts(w.contents, w.contents_len) };
        // text/plain を先に、無ければほかの text/*。消す（contents_len = 0）頼みは受けない
        let pick = contents
            .iter()
            .find(|c| ghostty_str(&c.mime).starts_with(b"text/plain"))
            .or_else(|| contents.iter().find(|c| ghostty_str(&c.mime).starts_with(b"text/")));
        let result = match pick {
            Some(c) => {
                let text = String::from_utf8_lossy(ghostty_str(&c.data)).into_owned();
                if i.effects.clipboard_write(&text) {
                    ffi::GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS
                } else {
                    ffi::GHOSTTY_CLIPBOARD_WRITE_RESULT_DENIED
                }
            }
            None => ffi::GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED,
        };
        let reply = ffi::GhosttyClipboardWriteReply { size: std::mem::size_of::<ffi::GhosttyClipboardWriteReply>(), result, remember: false };
        (w.reply)(write, &reply);
    }));
}

/// クリップボードの読み取り（[`Terminal::set_clipboard_read_enabled`] のときだけ入れる）
unsafe extern "C" fn cb_clipboard_read(_t: ffi::GhosttyTerminal, userdata: *mut c_void, read: *const ffi::GhosttyClipboardRead) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        let (Some(i), Some(r)) = (inner(userdata), read.as_ref()) else { return };
        let text = i.effects.clipboard_read();
        let mime = "text/plain";
        let content = text.as_ref().map(|t| ffi::GhosttyClipboardContent {
            mime: ffi::GhosttyString { ptr: mime.as_ptr(), len: mime.len() },
            data: ffi::GhosttyString { ptr: t.as_ptr(), len: t.len() },
        });
        let reply = ffi::GhosttyClipboardReadReply {
            size: std::mem::size_of::<ffi::GhosttyClipboardReadReply>(),
            result: if content.is_some() { ffi::GHOSTTY_CLIPBOARD_READ_RESULT_SUCCESS } else { ffi::GHOSTTY_CLIPBOARD_READ_RESULT_BUSY },
            contents: content.as_ref().map_or(ptr::null(), |c| c as *const _),
            contents_len: content.is_some() as usize,
            available: ptr::null(),
            available_len: 0,
            remember: false,
        };
        (r.reply)(read, &reply);
    }));
}

/// デスクトップ通知（OSC 9 / 777）。空の通知は捨てる（ghostel と同じ）
unsafe extern "C" fn cb_desktop_notification(
    _t: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    n: *const ffi::GhosttyTerminalDesktopNotification,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        let (Some(i), Some(n)) = (inner(userdata), n.as_ref()) else { return };
        let title = String::from_utf8_lossy(ghostty_str(&n.title)).into_owned();
        let body = String::from_utf8_lossy(ghostty_str(&n.body)).into_owned();
        if title.is_empty() && body.is_empty() {
            return;
        }
        i.effects.desktop_notification(&title, &body);
    }));
}

/// 進捗（OSC 9;4）
unsafe extern "C" fn cb_progress_report(_t: ffi::GhosttyTerminal, userdata: *mut c_void, r: *const ffi::GhosttyTerminalProgressReport) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        let (Some(i), Some(r)) = (inner(userdata), r.as_ref()) else { return };
        let state = match r.state {
            ffi::GHOSTTY_TERMINAL_PROGRESS_STATE_REMOVE => ProgressState::Remove,
            ffi::GHOSTTY_TERMINAL_PROGRESS_STATE_SET => ProgressState::Set,
            ffi::GHOSTTY_TERMINAL_PROGRESS_STATE_ERROR => ProgressState::Error,
            ffi::GHOSTTY_TERMINAL_PROGRESS_STATE_INDETERMINATE => ProgressState::Indeterminate,
            ffi::GHOSTTY_TERMINAL_PROGRESS_STATE_PAUSE => ProgressState::Pause,
            _ => return,
        };
        i.effects.progress(state, (r.progress >= 0).then_some(r.progress as u8));
    }));
}

/// `CSI 14/16/18/19 t` と mode 2048 の答え。桁数・行数は端末から、セルの画素は [`Terminal::resize`] で覚えた値
/// 未知の OSC として取っておく中身の上限（バイト）。2048 までは追加の確保が無いが、長いパスを渡せるよう大きめにする（T-118）
pub const UNKNOWN_MAX_BYTES: usize = 8192;

/// libghostty-vt が扱わないシーケンス（T-118）。OSC だけ渡す
unsafe extern "C" fn cb_unknown_sequence(_t: ffi::GhosttyTerminal, userdata: *mut c_void, sequence: *const ffi::GhosttyTerminalUnknownSequence) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        let (Some(i), Some(seq)) = (inner(userdata), sequence.as_ref()) else { return };
        if seq.tag != ffi::GHOSTTY_TERMINAL_UNKNOWN_SEQUENCE_OSC {
            return;
        }
        let osc = seq.value.osc;
        i.effects.unknown_osc(ghostty_str(&osc.content), osc.truncated);
    }));
}

/// `CSI ? 996 n`（配色の問い合わせ。T-102）
unsafe extern "C" fn cb_color_scheme(_t: ffi::GhosttyTerminal, userdata: *mut c_void, out: *mut c_int) -> bool {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        let (Some(i), Some(out)) = (inner(userdata), out.as_mut()) else { return false };
        *out = if i.dark { ffi::GHOSTTY_COLOR_SCHEME_DARK } else { ffi::GHOSTTY_COLOR_SCHEME_LIGHT };
        true
    }))
    .unwrap_or(false)
}

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

/// XTVERSION の答え（T-071）。`Inner` の文字列を指す（呼び出しの間は有効）
unsafe extern "C" fn cb_xtversion(_t: ffi::GhosttyTerminal, userdata: *mut c_void) -> ffi::GhosttyString {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Some(i) = inner(userdata) else { return ffi::GhosttyString { ptr: ptr::null(), len: 0 } };
        ffi::GhosttyString { ptr: i.xtversion.as_ptr(), len: i.xtversion.len() }
    }))
    .unwrap_or(ffi::GhosttyString { ptr: ptr::null(), len: 0 })
}

/// 同期出力の hold の出入り（T-074）。始まったときに、そのときの画面を写す（文書 guide-rendering の Render holds）。
/// vt_write の中から同期で呼ばれ、端末のロックは呼ぶ側が持っている
unsafe extern "C" fn cb_render_hold(t: ffi::GhosttyTerminal, userdata: *mut c_void, held: bool) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        let Some(i) = inner(userdata) else { return };
        if held {
            i.hold_started = Some(Instant::now());
            if let Ok(mut rs) = RenderState::new() {
                let (above, below) = i.hold_overscan;
                let _ = rs.set_overscan(above, below);
                if rs.update_raw(t).is_ok() {
                    i.hold_capture = Some(rs);
                }
            }
        } else {
            i.hold_started = None;
        }
    }));
}

// ---- 画像（Kitty graphics。T-088） ----

/// 画素の並び（[`KittyImage`]）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Rgb,
    Rgba,
    GrayAlpha,
    Gray,
}

/// 端末が持っている画像（[`Terminal::kitty_image`]）。画素は端末の中を指す
pub struct KittyImage<'a> {
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
    /// 画素が変わると変わる値（別の画像・別の端末でも再利用されない）
    pub generation: u64,
    /// 展開済みの画素（`width * height * 1 画素のバイト数`）
    pub pixels: &'a [u8],
}

/// 画像を描く層。小さいものから順に、背景・文字と重ねて描く
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImageLayer {
    /// セルの背景色より下
    BelowBackground,
    /// セルの背景色より上で、文字より下
    BelowText,
    /// 文字より上
    AboveText,
}

/// 1 枚の画像の 1 回の配置（[`RenderState::images`]）。この値のとおりに描けばよい:
/// `(col, row)` のセルから `offset_*` だけずらした位置に、画像の `source_*` の部分を `width` x `height` の画素で描く
/// （`col` と `row` はビューポートの左上からのセルで、負にも、画面の外にもなりうる。描くときに切り取る）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImagePlacement {
    pub image_id: u32,
    /// [`KittyImage::generation`] と同じ値（キャッシュの鍵に使う）
    pub generation: u64,
    pub layer: ImageLayer,
    pub z: i32,
    pub col: i32,
    pub row: i32,
    pub offset_x: u32,
    pub offset_y: u32,
    pub width: u32,
    pub height: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
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
    /// 点滅（SGR 5。T-112）
    fn blink(&self) -> bool;
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
    fn blink(&self) -> bool {
        self.flags & ffi::GHOSTTY_STYLE_FLAG_BLINK != 0
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
    image_iter: ffi::GhosttyRenderStateImageIterator,
}

unsafe impl Send for RenderState {}

impl RenderState {
    pub fn new() -> Result<Self> {
        // 途中で失敗したら Drop が作れた分だけ解放する（どの free も NULL を受け付ける）
        let mut s = Self { raw: ptr::null_mut(), rows: ptr::null_mut(), cells: ptr::null_mut(), image_iter: ptr::null_mut() };
        unsafe {
            check(ffi::ghostty_render_state_new(ptr::null(), &mut s.raw))?;
            check(ffi::ghostty_render_state_row_iterator_new(ptr::null(), &mut s.rows))?;
            check(ffi::ghostty_render_state_row_cells_new(ptr::null(), &mut s.cells))?;
            check(ffi::ghostty_render_state_image_iterator_new(ptr::null(), &mut s.image_iter))?;
        }
        Ok(s)
    }

    /// 端末から写す。端末のロックを持っている間に呼ぶ。写したあとはロックを外して読んでよい
    pub fn update(&mut self, terminal: &Terminal) -> Result<()> {
        unsafe { self.update_raw(terminal.raw) }
    }

    unsafe fn update_raw(&mut self, terminal: ffi::GhosttyTerminal) -> Result<()> {
        check(unsafe { ffi::ghostty_render_state_update(self.raw, terminal) })
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

    /// 次の update で、ビューポートの上下に余分に写す行数を求める（スクロールバックを埋めるのに使う）。
    /// 変えると次の update は全体の描き直しになる
    pub fn set_overscan(&mut self, above: u16, below: u16) -> Result<()> {
        let v = ffi::GhosttyRenderStateOverscan { above, below };
        check(unsafe { ffi::ghostty_render_state_set(self.raw, ffi::GHOSTTY_RENDER_STATE_OPT_OVERSCAN, &v as *const _ as *const c_void) })
    }

    /// 前の update で実際に写せた余分の行数（上, 下）
    pub fn overscan(&self) -> (u16, u16) {
        let mut v = ffi::GhosttyRenderStateOverscan::default();
        unsafe { ffi::ghostty_render_state_get(self.raw, ffi::GHOSTTY_RENDER_STATE_DATA_OVERSCAN, &mut v as *mut _ as *mut c_void) };
        (v.above, v.below)
    }

    /// 前の update で写した画像の配置を、描く順（層、z-index の順）に返す。端末のロックは要らない。
    /// Unicode placeholder（U=1）も相対配置も、ここまでに解決されている（placeholder は 1 行ごとに 1 つの配置）
    pub fn images(&mut self) -> Vec<ImagePlacement> {
        let mut list = Vec::new();
        unsafe {
            ffi::ghostty_render_state_get(self.raw, ffi::GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR, &mut self.image_iter as *mut _ as *mut c_void);
            while ffi::ghostty_render_state_image_iterator_next(self.image_iter) {
                if let Some(p) = self.current_image() {
                    list.push(p);
                }
            }
        }
        // 反復の順は描く順ではない（render.h）。同じ層・z なら読んだ順のまま
        list.sort_by_key(|p| (p.layer, p.z));
        list
    }

    /// 反復子の今の配置を読む。知らない種類（将来の別のプロトコルなど）は `None`
    unsafe fn current_image(&self) -> Option<ImagePlacement> {
        let (mut protocol, mut image_id, mut generation) = (0 as c_int, 0u32, 0u64);
        let (mut layer, mut z, mut col, mut row) = (0 as c_int, 0i32, 0i32, 0i32);
        let (mut offset_x, mut offset_y, mut width, mut height) = (0u32, 0u32, 0u32, 0u32);
        let (mut source_x, mut source_y, mut source_width, mut source_height) = (0u32, 0u32, 0u32, 0u32);
        let keys = [
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_PROTOCOL,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_ID,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_GENERATION,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_LAYER,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_Z,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_COL,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_ROW,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_X,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_Y,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_WIDTH,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_HEIGHT,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_X,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_Y,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_WIDTH,
            ffi::GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_HEIGHT,
        ];
        let mut values: [*mut c_void; 15] = [
            (&mut protocol as *mut c_int).cast(),
            (&mut image_id as *mut u32).cast(),
            (&mut generation as *mut u64).cast(),
            (&mut layer as *mut c_int).cast(),
            (&mut z as *mut i32).cast(),
            (&mut col as *mut i32).cast(),
            (&mut row as *mut i32).cast(),
            (&mut offset_x as *mut u32).cast(),
            (&mut offset_y as *mut u32).cast(),
            (&mut width as *mut u32).cast(),
            (&mut height as *mut u32).cast(),
            (&mut source_x as *mut u32).cast(),
            (&mut source_y as *mut u32).cast(),
            (&mut source_width as *mut u32).cast(),
            (&mut source_height as *mut u32).cast(),
        ];
        let r = unsafe { ffi::ghostty_render_state_image_get_multi(self.image_iter, keys.len(), keys.as_ptr(), values.as_mut_ptr(), ptr::null_mut()) };
        if r != ffi::GHOSTTY_SUCCESS || protocol != ffi::GHOSTTY_IMAGE_PROTOCOL_KITTY {
            return None;
        }
        let layer = match layer {
            ffi::GHOSTTY_IMAGE_LAYER_BELOW_BACKGROUND => ImageLayer::BelowBackground,
            ffi::GHOSTTY_IMAGE_LAYER_BELOW_TEXT => ImageLayer::BelowText,
            ffi::GHOSTTY_IMAGE_LAYER_ABOVE_TEXT => ImageLayer::AboveText,
            _ => return None,
        };
        Some(ImagePlacement {
            image_id,
            generation,
            layer,
            z,
            col,
            row,
            offset_x,
            offset_y,
            width,
            height,
            source_x,
            source_y,
            source_width,
            source_height,
        })
    }

    /// 行の反復を始める。写した行（overscan を含む）を上から順にすべて返す
    pub fn all_rows(&mut self) -> Rows<'_> {
        unsafe {
            ffi::ghostty_render_state_get(self.raw, ffi::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &mut self.rows as *mut _ as *mut c_void)
        };
        Rows { state: self, index: 0 }
    }

    /// 行の反復を始める。描き直しの要る行（全体が dirty なら全部の行）だけを上から順に返す
    pub fn dirty_rows(&mut self) -> Rows<'_> {
        unsafe {
            ffi::ghostty_render_state_get(self.raw, ffi::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &mut self.rows as *mut _ as *mut c_void)
        };
        Rows { state: self, index: 0 }
    }
}

impl Drop for RenderState {
    fn drop(&mut self) {
        unsafe {
            ffi::ghostty_render_state_row_cells_free(self.cells);
            ffi::ghostty_render_state_image_iterator_free(self.image_iter);
            ffi::ghostty_render_state_row_iterator_free(self.rows);
            ffi::ghostty_render_state_free(self.raw);
        }
    }
}

/// 行の反復子（[`RenderState::dirty_rows`]）
pub struct Rows<'a> {
    state: &'a mut RenderState,
    /// `next_row` で数えた行の位置
    index: u16,
}

impl Rows<'_> {
    /// 次の描き直しの要る行へ進み、その行の y（反復の先頭（overscan があれば一番上の余分の行）からの位置）を返す
    pub fn next_dirty(&mut self) -> Option<u16> {
        let mut y: u16 = 0;
        unsafe { ffi::ghostty_render_state_row_iterator_next_dirty(self.state.rows, &mut y) }.then_some(y)
    }

    /// 次の行へ進み、その行の位置（反復の先頭からの番号）を返す
    pub fn next_row(&mut self) -> Option<u16> {
        unsafe { ffi::ghostty_render_state_row_iterator_next(self.state.rows) }.then(|| {
            let y = self.index;
            self.index += 1;
            y
        })
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
    /// F1〜F24（1 始まり）
    F(u8),
}

/// 修飾キー
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
            Key::F(n) if (1..=24).contains(&n) => ffi::GHOSTTY_KEY_F1 + (n as c_int - 1),
            Key::F(_) => return Vec::new(),
        };
        self.encode_input(terminal, &KeyInput { key: k, action: KeyAction::Press, mods, consumed_shift: false, text: "", unshifted: 0 })
    }

    /// キーのイベントを VT の列にする（KKP でも使う。[`KeyInput`]）。端末のロックを持って呼ぶ。何も送らないときは空
    pub fn encode_input(&mut self, terminal: &Terminal, input: &KeyInput<'_>) -> Vec<u8> {
        let m = mods_bits(input.mods);
        let consumed = if input.consumed_shift { ffi::GHOSTTY_MODS_SHIFT } else { 0 };
        let action = match input.action {
            KeyAction::Press => ffi::GHOSTTY_KEY_ACTION_PRESS,
            KeyAction::Release => ffi::GHOSTTY_KEY_ACTION_RELEASE,
            KeyAction::Repeat => ffi::GHOSTTY_KEY_ACTION_REPEAT,
        };
        let text = ffi::GhosttyString { ptr: input.text.as_ptr(), len: input.text.len() };
        let mut buf = [0u8; 128];
        let mut written = 0usize;
        unsafe {
            ffi::ghostty_key_encoder_set_from_terminal(self.raw, terminal.raw);
            ffi::ghostty_key_event_set_action(self.event, action);
            ffi::ghostty_key_event_set_key(self.event, input.key);
            ffi::ghostty_key_event_set_mods(self.event, m);
            ffi::ghostty_key_event_set_consumed_mods(self.event, consumed);
            ffi::ghostty_key_event_set_composing(self.event, false);
            ffi::ghostty_key_event_set_utf8(self.event, text);
            ffi::ghostty_key_event_set_unshifted_codepoint(self.event, input.unshifted);
            if ffi::ghostty_key_encoder_encode(self.raw, self.event, buf.as_mut_ptr(), buf.len(), &mut written)
                != ffi::GHOSTTY_SUCCESS
            {
                return Vec::new();
            }
            // イベントが文字列を指したままにならないよう外す
            ffi::ghostty_key_event_set_utf8(self.event, ffi::GhosttyString { ptr: ptr::null(), len: 0 });
        }
        buf[..written.min(buf.len())].to_vec()
    }
}

fn mods_bits(mods: Mods) -> u16 {
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
    m
}

/// キーの動き
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Press,
    Release,
    Repeat,
}

/// キーのイベント（[`KeyEncoder::encode_input`]。KKP で使う）
#[derive(Clone, Copy, Debug)]
pub struct KeyInput<'a> {
    /// GhosttyKey（key/event.h。物理キー）
    pub key: c_int,
    pub action: KeyAction,
    pub mods: Mods,
    /// `text` を作るのに Shift を使ったか（consumed mods）
    pub consumed_shift: bool,
    /// キーが作る文字（Ctrl / Alt を足す前。空でもよい）。C0 制御文字は入れない
    pub text: &'a str,
    /// Shift を外したときの文字のコードポイント（0 は無し）
    pub unshifted: u32,
}

impl Drop for KeyEncoder {
    fn drop(&mut self) {
        unsafe {
            ffi::ghostty_key_event_free(self.event);
            ffi::ghostty_key_encoder_free(self.raw);
        }
    }
}
