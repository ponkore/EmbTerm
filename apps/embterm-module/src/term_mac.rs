//! macOS の端末 1 つ分（T-126）。Emacs のフレームのビュー（`EmacsView`）のレイヤーに、端末を描く
//! **CAMetalLayer を子レイヤーとして足す**。
//!
//! ```text
//! Lisp（main スレッド）           module                                   描画スレッド（端末ごとに 1 本）
//! ─────────────────────           ──────────────────────                   ────────────────────────────────
//! embterm-module-place ─────────▶ MacApp（thread_local。NSView とレイヤー）
//!   …-send-key           ─────▶  Session（et-session。PTY と VT）  ◀────   フレームを写して Metal で描く
//!   …-poll               ◀─────  Shared（状態と知らせ）           ◀────   描いたら世代を上げる
//! ```
//!
//! Windows 版（[`crate::term`]）との違い（docs/notes/t126-macos-module.md）
//!
//! - **UI スレッドを作らない。** AppKit のビューは main スレッド（= Lisp のスレッド）からしか触れないので、
//!   Lisp からの頼みごとはその場で片づける（レイヤーの付け替えと大きさ合わせは数十 µs）
//! - **描画だけ別のスレッド**。`nextDrawable` は垂直同期まで待つので、Lisp のスレッドで描いてはいけない（T-125）
//! - 子ウィンドウ（HWND）ではなく**レイヤー**なので、マウスの当たり判定に出ない。クリック・ホイールは Emacs のまま
//!   （端末へのマウスの報告・ドラッグでの選択は T-127）
//! - フレームと NSWindow の対応は、Lisp が渡す「z 順の番号 : フレームの幅 : 高さ」で引く（`window-id` は使えない。T-121 §3.7）

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc};
use std::time::{Duration, Instant};

use et_render::{FontSpec, Frame, FrameView, NarrowWide, Renderer, Theme};
use et_session::keys_mac;
use et_session::{CLOSE_TIMEOUT, Colors, CursorDefault, Event as SessionEvent, Session, SessionOptions};
use et_vt::{KeyEncoder, RenderState, Rgb, Scroll};
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSView, NSWindow};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_quartz_core::{CALayer, CAMetalLayer};

use crate::log;

const DEFAULT_FG: Rgb = Rgb { r: 0xd4, g: 0xd4, b: 0xd4 };
const DEFAULT_BG: Rgb = Rgb { r: 0x1e, g: 0x1e, b: 0x1e };
/// ペインがまだ無いうちに起動するときのセルの大きさ（仮）
const FALLBACK_CELL: (u32, u32) = (8, 16);
const DEFAULT_SCROLLBACK: usize = 10_000;
/// ホイール 1 ノッチで動かす行数
const WHEEL_LINES: i64 = 3;
/// 同期出力（DEC 2026）の hold の時限（T-074）
const RENDER_HOLD_TIMEOUT: Duration = Duration::from_secs(1);
/// Lisp がまだ読んでいない知らせの上限
const MAX_PENDING_EVENTS: usize = 1000;
/// 描画スレッドが何も起きなくても見回る間隔
const RENDER_TICK: Duration = Duration::from_millis(50);

// ---- Lisp とやりとりする型（Windows 版と同じ形）----

/// ペインのスクロールのしかた
#[derive(Clone, Copy, Debug)]
pub enum ScrollAmount {
    Lines(i64),
    Pages(i64),
    Top,
    Bottom,
    Wheel(i32),
    Prompt(i64),
}

/// Lisp からの頼みごと
pub enum Request {
    /// ペインをフレームのビューの中に置いて見せる。`parent` は「z 順の番号 : 幅 : 高さ」（論理ポイント）
    Place { pane: i64, parent: String, x: i32, y: i32, w: i32, h: i32 },
    Hide(Option<i64>),
    Remove(i64),
    Select(Option<i64>),
    Start { cols: u16, rows: u16 },
    SendString(String),
    SendKey { key: String, mods: String },
    Paste(String),
    Scroll { pane: i64, amount: ScrollAmount },
    ClearSelection,
    SetScrollback(usize),
    SetFont(FontSpec),
    SetNarrowWide(NarrowWide),
    Focus(bool),
    Copy(Option<i64>),
    Screenshot { pane: Option<i64>, path: PathBuf },
    PaneText { pane: i64, reply: mpsc::Sender<Option<String>> },
    SetColors(ColorConfig),
    SetCursor(CursorDefault),
    SetClipboardPolicy { write: bool, read: bool },
    SetImages(et_session::ImageSettings),
    LinkAt { pane: i64, x: i32, y: i32, reply: mpsc::Sender<Option<String>> },
    DebugHover { pane: i64, x: i32, y: i32 },
    DebugInfo(mpsc::Sender<DebugInfo>),
    Perf { reset: bool, reply: mpsc::Sender<String> },
    Close,
}

/// Lisp から受けた色（T-034）
#[derive(Clone, Copy, Debug, Default)]
pub struct ColorConfig {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub selection_bg: Option<Rgb>,
    pub selection_fg: Option<Rgb>,
    pub palette: [Option<Rgb>; 16],
}

impl ColorConfig {
    fn term_colors(&self) -> Colors {
        Colors {
            foreground: self.foreground.unwrap_or(DEFAULT_FG),
            background: self.background.unwrap_or(DEFAULT_BG),
            cursor: self.cursor,
            palette: self.palette,
        }
    }

    fn apply_theme(&self, theme: &mut Theme) {
        let d = Theme::default();
        theme.selection_bg = self.selection_bg.unwrap_or(d.selection_bg);
        theme.selection_fg = self.selection_fg;
        theme.cursor = self.cursor.unwrap_or(d.cursor);
    }
}

/// 描画とカーソルの状態（デバッグ・自動確認用）
#[derive(Clone, Debug, Default)]
pub struct DebugInfo {
    pub blink_timer: bool,
    pub blink_on: bool,
    pub blink_ticks: u64,
    pub full_draws: u64,
    pub partial_draws: u64,
    pub cursor_shape: &'static str,
    pub cursor_blinking: bool,
    pub cursor_visible: bool,
    pub foreground: Rgb,
    pub background: Rgb,
    pub palette1: Rgb,
    pub cell: (u32, u32),
    pub padding: u32,
    pub hover: Option<(u16, u16, u16)>,
    pub text_blink_timer: bool,
    pub text_blink_on: bool,
    pub animation_ticks: u64,
    pub animation_playing: bool,
    pub images_converted: u64,
    pub images_pending: usize,
}

/// Lisp に渡す知らせ
#[derive(Clone, Debug)]
pub enum Event {
    Clicked(i64),
    Bell,
    Exited(Option<u32>),
    Screenshot(String),
    Selected(String),
    CopyReady,
    Error(String),
    Clipboard(String),
    Notification { title: String, body: String },
    Progress { state: &'static str, percent: Option<u8> },
    Link(String),
    Hover(Option<String>),
    ShellCommand(String),
    CommandEnd(Option<i32>),
}

/// Lisp に見せるペインの情報
#[derive(Clone, Default)]
pub struct PaneInfo {
    pub id: i64,
    /// Windows の子 HWND に当たるもの（macOS ではレイヤーのアドレス。デバッグ用）
    pub hwnd: isize,
    pub cols: u16,
    pub rows: u16,
    pub scrolled: bool,
}

/// Lisp がポーリングで写して読む状態
#[derive(Clone, Default)]
pub struct State {
    pub generation: u64,
    pub alive: bool,
    pub view: bool,
    pub hwnd: isize,
    pub panes: Vec<PaneInfo>,
    pub started: bool,
    pub pid: u32,
    pub conpty: String,
    pub font: String,
    pub title: String,
    pub pwd: String,
    pub exited: bool,
    pub exit_code: Option<u32>,
    pub closed: bool,
    pub cols: u16,
    pub rows: u16,
    pub cell: (u32, u32),
    pub cursor: Option<(i32, i32, i32, i32)>,
}

/// 起動の指定
pub struct SpawnOptions {
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, Option<OsString>)>,
    /// Windows の ConPTY の置き場（macOS では使わない）
    pub conpty_dir: Option<PathBuf>,
    pub font: FontSpec,
    pub narrow_wide: NarrowWide,
    pub scrollback_lines: Option<usize>,
}

/// copy モードのテキストの一部（T-127 で中身を入れる）
pub struct CopyChunk {
    pub text: String,
    pub top: i64,
    pub point: i64,
    pub wraps: Vec<i64>,
    pub more: bool,
}

/// フォントの指定を作る
pub fn font_spec(families: Vec<String>, size: Option<f32>, fallback: Vec<String>) -> FontSpec {
    let mut spec = FontSpec::default();
    spec.families.splice(0..0, families);
    if let Some(s) = size {
        spec.size_pt = s;
    }
    spec.fallback = fallback;
    spec
}

// ---- 分け合う状態 ----

/// 描画スレッドと main スレッドで分け合うペイン
struct PaneRender {
    renderer: Renderer,
    /// デバイス画素
    size: (u32, u32),
    cols: u16,
    rows: u16,
    /// 遡っている行数（0 なら最新）
    scroll: i64,
    visible: bool,
    /// 選んでいるペイン（端末の大きさを決める）
    selected: bool,
}

// SAFETY: 中身は Metal のオブジェクト（デバイス・キュー・テクスチャ）と CAMetalLayer。
// どれも別のスレッドから描いてよい（Apple の決まり）。ビュー階層には触らない
unsafe impl Send for PaneRender {}

pub struct Shared {
    state: Mutex<State>,
    events: Mutex<Vec<Event>>,
    session: OnceLock<Arc<Session>>,
    selection: Mutex<Option<String>>,
    /// 描くもの（描画スレッドが使う）
    panes: Mutex<Vec<(i64, Arc<Mutex<PaneRender>>)>>,
    /// 描き直しが要る
    dirty: Mutex<bool>,
    wake: Condvar,
    stop: AtomicBool,
    /// 描いた回数（デバッグ用）
    draws: AtomicU64,
    /// 最後に描いたときのカーソル（見えるか・点滅か・形。デバッグ用）
    cursor_dbg: Mutex<(bool, bool, &'static str)>,
    /// 最後に描いたときの色（前景・背景・パレットの 1 番。デバッグ用）
    colors_dbg: Mutex<(Rgb, Rgb, Rgb)>,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut s);
        s.generation += 1;
    }

    fn emit(&self, e: Event) {
        let mut v = self.events.lock().unwrap_or_else(|e| e.into_inner());
        if v.len() < MAX_PENDING_EVENTS {
            v.push(e);
        }
        drop(v);
        self.update(|_| {});
    }

    fn mark_dirty(&self) {
        *self.dirty.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.wake.notify_all();
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn session(&self) -> Option<Arc<Session>> {
        self.session.get().cloned()
    }

    pub fn selection_text(&self) -> Option<String> {
        self.selection.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// copy モードは T-127（まだ中身が無い）
    pub fn take_copy(&self) -> Option<CopyChunk> {
        None
    }
}

/// 動いている端末（Emacs を終えるときに閉じる。T-076）
static SESSIONS: Mutex<Vec<Weak<Session>>> = Mutex::new(Vec::new());

/// Emacs を終えるときに全部の端末を閉じる
pub fn shutdown(grace: Duration) -> (usize, usize) {
    let sessions: Vec<Arc<Session>> =
        SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).iter().filter_map(Weak::upgrade).collect();
    let closings: Vec<_> = sessions.iter().map(|s| s.close_with(grace)).collect();
    let deadline = Instant::now() + grace + Duration::from_millis(300);
    let done = closings.iter().filter(|c| c.wait(deadline.saturating_duration_since(Instant::now()))).count();
    (done, closings.len())
}

/// GPU デバイスを失ったことにする（Windows の T-067 の確認用。macOS では何もしない）
pub fn debug_lose_device() {
    log("macOS では GPU デバイスの作り直しの確認は無い（T-126）");
}

// ---- main スレッドの状態 ----

thread_local! {
    /// 端末の番号 → main スレッドの状態（ビューとレイヤー）
    static APPS: RefCell<HashMap<u64, MacApp>> = RefCell::new(HashMap::new());
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct MacPane {
    id: i64,
    layer: Retained<CAMetalLayer>,
    /// 置いてある親（`EmacsView`）
    host: Option<Retained<NSView>>,
    render: Arc<Mutex<PaneRender>>,
}

struct MacApp {
    shared: Arc<Shared>,
    panes: Vec<MacPane>,
    selected: Option<i64>,
    font: FontSpec,
    theme: Theme,
    colors: ColorConfig,
    cursor: CursorDefault,
    images: et_session::ImageSettings,
    scrollback: usize,
    clipboard: (bool, bool),
    focused: bool,
    opts: Option<SpawnOptions>,
    encoder: Option<KeyEncoder>,
    closed: bool,
}

/// 端末の取っ手（Lisp の user-ptr の中身）
pub struct Term {
    id: u64,
    shared: Arc<Shared>,
}

impl Term {
    pub fn shared(&self) -> &Shared {
        &self.shared
    }

    /// 頼みごとをその場で片づける（AppKit は main スレッドだけ。描画は別のスレッドが持つ）
    pub fn request(&self, r: Request) {
        let id = self.id;
        let _ = catch_unwind(AssertUnwindSafe(|| {
            APPS.with(|apps| {
                let mut apps = apps.borrow_mut();
                if let Some(app) = apps.get_mut(&id) {
                    app.handle(r);
                }
            })
        }));
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.request(Request::Close);
        let id = self.id;
        let _ = catch_unwind(AssertUnwindSafe(|| APPS.with(|apps| apps.borrow_mut().remove(&id))));
    }
}

/// 端末を作る（描画スレッドを立てる）。子プロセスの起動は最初の Place か Start のとき
pub fn spawn(opts: SpawnOptions) -> Result<Term, String> {
    et_vt::install_png_decoder();
    let shared = Arc::new(Shared {
        state: Mutex::new(State { alive: true, conpty: "openpty".into(), ..State::default() }),
        events: Mutex::new(Vec::new()),
        session: OnceLock::new(),
        selection: Mutex::new(None),
        panes: Mutex::new(Vec::new()),
        dirty: Mutex::new(false),
        wake: Condvar::new(),
        stop: AtomicBool::new(false),
        draws: AtomicU64::new(0),
        cursor_dbg: Mutex::new((false, false, "block")),
        colors_dbg: Mutex::new((DEFAULT_FG, DEFAULT_BG, Rgb::default())),
    });
    let mut theme = Theme::default();
    theme.narrow_wide = opts.narrow_wide;
    let app = MacApp {
        shared: shared.clone(),
        panes: Vec::new(),
        selected: None,
        font: opts.font.clone(),
        theme,
        colors: ColorConfig::default(),
        cursor: CursorDefault::default(),
        images: et_session::ImageSettings::default(),
        scrollback: opts.scrollback_lines.unwrap_or(DEFAULT_SCROLLBACK),
        clipboard: (false, false),
        focused: false,
        opts: Some(opts),
        encoder: None,
        closed: false,
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    APPS.with(|apps| apps.borrow_mut().insert(id, app));

    let s = shared.clone();
    std::thread::Builder::new()
        .name("embterm-render".into())
        .spawn(move || {
            let _ = catch_unwind(AssertUnwindSafe(|| render_main(s)));
        })
        .map_err(|e| format!("描画のスレッドを作れません: {e}"))?;
    Ok(Term { id, shared })
}

impl MacApp {
    fn handle(&mut self, r: Request) {
        match r {
            Request::Place { pane, parent, x, y, w, h } => self.place(pane, &parent, x, y, w, h),
            Request::Hide(pane) => self.hide(pane),
            Request::Remove(pane) => self.remove(pane),
            Request::Select(pane) => self.select(pane),
            Request::Start { cols, rows } => {
                if self.shared.session().is_none() {
                    self.start(cols.max(1), rows.max(1));
                }
            }
            Request::SendString(text) => {
                if let Some(s) = self.shared.session() {
                    s.write(text.as_bytes());
                    self.follow_output();
                }
            }
            Request::SendKey { key, mods } => self.send_key(&key, &mods),
            Request::Paste(text) => {
                if let Some(s) = self.shared.session() {
                    let bracketed = s.lock().mode(et_vt::Mode::BRACKETED_PASTE);
                    s.write(&keys_mac::paste_bytes(&text, bracketed));
                    self.follow_output();
                }
            }
            Request::Scroll { pane, amount } => self.scroll(pane, amount),
            Request::ClearSelection => {
                if let Some(s) = self.shared.session() {
                    let _ = s.lock().set_selection(None);
                }
                *self.shared.selection.lock().unwrap_or_else(|e| e.into_inner()) = None;
                self.shared.mark_dirty();
            }
            Request::SetScrollback(lines) => {
                self.scrollback = lines;
                if let Some(s) = self.shared.session() {
                    let _ = s.lock().set_scrollback_max_lines(lines);
                }
            }
            Request::SetFont(spec) => self.set_font(spec),
            Request::SetNarrowWide(mode) => {
                self.theme.narrow_wide = mode;
                self.apply_theme();
            }
            Request::Focus(on) => {
                self.focused = on;
                // フォーカスの出入りは、アプリが求めているとき（mode 1004）だけ送る。
                // Windows は ConPTY が必ず求めるので常に送っている（T-070）が、macOS では
                // 求めていないシェルにそのまま文字として出てしまう
                if let Some(s) = self.shared.session()
                    && s.lock().mode(et_vt::Mode::FOCUS_EVENT)
                {
                    s.write(if on { b"\x1b[I" } else { b"\x1b[O" });
                }
                self.shared.mark_dirty();
            }
            Request::SetColors(c) => {
                self.colors = c;
                c.apply_theme(&mut self.theme);
                if let Some(s) = self.shared.session() {
                    s.set_colors(&c.term_colors());
                }
                self.apply_theme();
            }
            Request::SetCursor(c) => {
                self.cursor = c;
                if let Some(s) = self.shared.session() {
                    s.set_cursor_default(c);
                }
                self.shared.mark_dirty();
            }
            Request::SetClipboardPolicy { write, read } => {
                self.clipboard = (write, read);
                if let Some(s) = self.shared.session() {
                    s.set_clipboard_policy(write, read);
                }
            }
            Request::SetImages(s) => {
                self.images = s.clone();
                if let Some(sess) = self.shared.session() {
                    sess.set_images(&s);
                }
            }
            Request::Screenshot { pane, path } => self.screenshot(pane, &path),
            Request::PaneText { pane, reply } => {
                let _ = reply.send(self.pane_text(pane));
            }
            Request::DebugInfo(reply) => {
                let _ = reply.send(self.debug_info());
            }
            Request::Perf { reset, reply } => {
                let text = self.shared.session().map(|s| s.perf_report(reset)).unwrap_or_default();
                let _ = reply.send(text);
            }
            Request::Close => self.close(),
            // T-127 で作る（copy モード・リンク・マウスの下の下線）
            Request::Copy(_) => log("copy モードは macOS では未対応（T-127）"),
            Request::LinkAt { reply, .. } => {
                let _ = reply.send(None);
            }
            Request::DebugHover { .. } => {}
        }
    }

    /// 出力のあと、描画スレッドを起こす
    fn follow_output(&self) {
        self.shared.mark_dirty();
    }

    fn pane(&mut self, id: i64) -> Option<&mut MacPane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    /// ペインを置く（無ければ作る）
    fn place(&mut self, id: i64, parent: &str, x: i32, y: i32, w: i32, h: i32) {
        let Some((host, scale)) = emacs_view(parent) else {
            self.shared.emit(Event::Error(format!("フレームのビューが見つかりません: {parent}")));
            return;
        };
        let (w, h) = (w.max(1), h.max(1));
        if self.pane(id).is_none() {
            match self.create_pane(id, scale) {
                Ok(p) => self.panes.push(p),
                Err(e) => {
                    self.shared.emit(Event::Error(format!("ペインを作れません: {e}")));
                    return;
                }
            }
        }
        let Some(pane) = self.pane(id) else { return };
        // 親が違えば付け替える（別のフレームへ移った）
        let same_host = pane.host.as_ref().is_some_and(|v| *v == host);
        if !same_host {
            pane.layer.removeFromSuperlayer();
            if let Some(layer) = host.layer() {
                layer.addSublayer(&pane.layer);
                log(&format!(
                    "ペイン {id} を {} のレイヤー（{}、{}x{}、geometryFlipped {}）に足した。置き場所 {x},{y} {w}x{h}",
                    host.class().name().to_string_lossy(),
                    layer.class().name().to_string_lossy(),
                    layer.bounds().size.width,
                    layer.bounds().size.height,
                    layer.isGeometryFlipped(),
                ));
            } else {
                log(&format!("ペイン {id}: 親のビューにレイヤーがありません（layer-backed でない？）"));
            }
            pane.host = Some(host.clone());
        }
        let rect = CGRect::new(CGPoint::new(x as f64, y as f64), CGSize::new(w as f64, h as f64));
        pane.layer.setFrame(rect);
        pane.layer.setHidden(false);
        pane.layer.setContentsScale(scale as f64);

        let px = ((w as f32 * scale).round() as u32, (h as f32 * scale).round() as u32);
        let render = pane.render.clone();
        let (cols, rows) = {
            let mut r = render.lock().unwrap_or_else(|e| e.into_inner());
            if r.size != px {
                let _ = r.renderer.resize(px.0, px.1, scale);
                r.size = px;
            }
            let (c, rw) = r.renderer.grid_size(px.0, px.1);
            r.cols = c;
            r.rows = rw;
            r.visible = true;
            (c, rw)
        };
        if self.selected.is_none() {
            self.selected = Some(id);
        }
        self.sync_panes();
        if self.shared.session().is_none() {
            self.start(cols, rows);
        } else if self.selected == Some(id) {
            self.resize_session(cols, rows);
        }
        self.publish_panes();
        self.shared.mark_dirty();
    }

    fn create_pane(&mut self, id: i64, scale: f32) -> Result<MacPane, String> {
        let layer = CAMetalLayer::new();
        // レイヤーの原点は親（EmacsView のレイヤー）に合わせる。EmacsView は左上原点
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        let renderer = Renderer::for_layer(&layer, 1, 1, scale, self.font.clone())?;
        let mut render = PaneRender {
            renderer,
            size: (1, 1),
            cols: 1,
            rows: 1,
            scroll: 0,
            visible: false,
            selected: false,
        };
        render.renderer.set_theme(self.theme.clone());
        Ok(MacPane { id, layer, host: None, render: Arc::new(Mutex::new(render)) })
    }

    /// 描画スレッドに今のペインの一覧を渡す
    fn sync_panes(&self) {
        let list: Vec<(i64, Arc<Mutex<PaneRender>>)> = self.panes.iter().map(|p| (p.id, p.render.clone())).collect();
        *self.shared.panes.lock().unwrap_or_else(|e| e.into_inner()) = list;
    }

    fn hide(&mut self, id: Option<i64>) {
        for p in self.panes.iter_mut().filter(|p| id.is_none_or(|i| i == p.id)) {
            p.layer.setHidden(true);
            p.render.lock().unwrap_or_else(|e| e.into_inner()).visible = false;
        }
        self.publish_panes();
    }

    fn remove(&mut self, id: i64) {
        if let Some(i) = self.panes.iter().position(|p| p.id == id) {
            let p = self.panes.remove(i);
            p.layer.removeFromSuperlayer();
            if self.selected == Some(id) {
                self.selected = self.panes.first().map(|p| p.id);
            }
            self.sync_panes();
            self.publish_panes();
        }
    }

    fn select(&mut self, id: Option<i64>) {
        self.selected = id.or(self.selected);
        for p in &self.panes {
            let mut r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            r.selected = Some(p.id) == id;
        }
        if let Some(sel) = id
            && let Some(p) = self.panes.iter().find(|p| p.id == sel)
        {
            let (c, r) = {
                let g = p.render.lock().unwrap_or_else(|e| e.into_inner());
                (g.cols, g.rows)
            };
            self.resize_session(c, r);
        }
        self.shared.mark_dirty();
    }

    /// 子プロセスを起こす
    fn start(&mut self, cols: u16, rows: u16) {
        let Some(opts) = self.opts.take() else { return };
        let cell = self.cell_size();
        let notify_shared = self.shared.clone();
        let sopts = SessionOptions {
            argv: opts.argv,
            cwd: opts.cwd,
            env: opts.env,
            cols,
            rows,
            cell_px: cell,
            conpty_search: Vec::new(),
            colors: self.colors.term_colors(),
            cursor: self.cursor,
            scrollback_lines: self.scrollback,
            clipboard_write: self.clipboard.0,
            clipboard_read: self.clipboard.1,
            images: self.images.clone(),
        };
        let session = match Session::start(sopts, Arc::new(move |e| on_session_event(&notify_shared, e))) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                self.shared.emit(Event::Error(format!("子プロセスを起動できません: {e}")));
                return;
            }
        };
        SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).push(Arc::downgrade(&session));
        let (pid, kind) = (session.pid(), session.pty_kind().to_string());
        let font = self.font_name();
        log(&format!("起動: pid {pid} / {cols}x{rows} / PTY: {kind} / フォント {font}"));
        let _ = self.shared.session.set(session);
        self.encoder = KeyEncoder::new().ok();
        self.shared.update(|s| {
            s.started = true;
            s.pid = pid;
            s.conpty = kind;
            s.font = font;
            s.cols = cols;
            s.rows = rows;
            s.cell = cell;
        });
        self.shared.mark_dirty();
    }

    fn resize_session(&self, cols: u16, rows: u16) {
        let Some(s) = self.shared.session() else { return };
        let cell = self.cell_size();
        if s.size() != (cols, rows) {
            s.resize(cols, rows, cell);
            self.shared.update(|st| {
                st.cols = cols;
                st.rows = rows;
                st.cell = cell;
            });
        }
    }

    /// 選んでいる（無ければ最初の）ペインのセルの大きさ（デバイス画素）
    fn cell_size(&self) -> (u32, u32) {
        self.current_pane()
            .map(|p| {
                let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
                r.renderer.cell_size()
            })
            .unwrap_or(FALLBACK_CELL)
    }

    fn current_pane(&self) -> Option<&MacPane> {
        self.selected
            .and_then(|id| self.panes.iter().find(|p| p.id == id))
            .or_else(|| self.panes.first())
    }

    fn send_key(&mut self, key: &str, mods: &str) {
        let Some(s) = self.shared.session() else { return };
        let bytes = {
            let term = s.lock();
            match self.encoder.as_mut() {
                Some(enc) => keys_mac::encode_emacs_key(&term, enc, key, mods),
                None => Vec::new(),
            }
        };
        if !bytes.is_empty() {
            s.write(&bytes);
            // キーを打ったら最新の画面に戻す
            for p in &self.panes {
                p.render.lock().unwrap_or_else(|e| e.into_inner()).scroll = 0;
            }
            self.follow_output();
        }
    }

    fn scroll(&mut self, id: i64, amount: ScrollAmount) {
        let Some(s) = self.shared.session() else { return };
        let rows = self.panes.iter().find(|p| p.id == id).map(|p| {
            let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            r.rows as i64
        });
        let Some(rows) = rows else { return };
        let mouse = { s.lock().mouse_tracking() };
        let delta = match amount {
            ScrollAmount::Lines(n) => -n,
            ScrollAmount::Pages(n) => -n * (rows - 1).max(1),
            ScrollAmount::Wheel(n) => {
                if mouse {
                    // アプリがマウスを見ているなら、スクロールはアプリへ送る（T-038 と同じ考え）
                    let seq: &[u8] = if n < 0 { b"\x1b[A" } else { b"\x1b[B" };
                    for _ in 0..n.unsigned_abs().min(10) {
                        s.write(seq);
                    }
                    return;
                }
                -(n as i64) * WHEEL_LINES
            }
            ScrollAmount::Top => i64::MAX,
            ScrollAmount::Bottom => i64::MIN,
            ScrollAmount::Prompt(_) => {
                log("プロンプトへの移動は macOS では未対応（T-127）");
                return;
            }
        };
        // 遡れる行数 = スクロールバックを含む全部の行 - 画面の行数
        let max = {
            let t = s.lock();
            let sb = t.scrollbar();
            (sb.total as i64 - sb.len as i64).max(0)
        };
        if let Some(p) = self.panes.iter().find(|p| p.id == id) {
            let mut r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            r.scroll = match amount {
                ScrollAmount::Top => max,
                ScrollAmount::Bottom => 0,
                _ => (r.scroll + delta).clamp(0, max),
            };
        }
        self.publish_panes();
        self.shared.mark_dirty();
    }

    fn set_font(&mut self, spec: FontSpec) {
        self.font = spec.clone();
        for p in &self.panes {
            let mut r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = r.renderer.set_font(spec.clone()) {
                log(&format!("フォントを変えられません: {e}"));
            }
            let (c, rows) = r.renderer.grid_size(r.size.0, r.size.1);
            r.cols = c;
            r.rows = rows;
        }
        let font = self.font_name();
        self.shared.update(|s| s.font = font);
        if let Some(p) = self.current_pane() {
            let (c, r) = {
                let g = p.render.lock().unwrap_or_else(|e| e.into_inner());
                (g.cols, g.rows)
            };
            self.resize_session(c, r);
        }
        self.shared.mark_dirty();
    }

    fn apply_theme(&self) {
        for p in &self.panes {
            p.render.lock().unwrap_or_else(|e| e.into_inner()).renderer.set_theme(self.theme.clone());
        }
        self.shared.mark_dirty();
    }

    fn font_name(&self) -> String {
        self.current_pane()
            .map(|p| {
                let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
                r.renderer.font_family().to_string()
            })
            .unwrap_or_else(|| self.font.families.first().cloned().unwrap_or_default())
    }

    fn screenshot(&mut self, pane: Option<i64>, path: &PathBuf) {
        let Some(s) = self.shared.session() else { return };
        let id = pane.or(self.selected).or(self.panes.first().map(|p| p.id));
        let Some(p) = id.and_then(|i| self.panes.iter().find(|p| p.id == i)) else {
            self.shared.emit(Event::Error("ペインがありません".into()));
            return;
        };
        let mut rs = match RenderState::new() {
            Ok(rs) => rs,
            Err(e) => {
                self.shared.emit(Event::Error(e.to_string()));
                return;
            }
        };
        let mut frame = Frame::default();
        {
            let term = s.lock();
            let _ = rs.update(&term);
            frame.capture_images(&mut rs, &term);
        }
        frame.update(&mut rs);
        let mut r = p.render.lock().unwrap_or_else(|e| e.into_inner());
        let view = pane_view(&r, &frame);
        match r.renderer.draw_view(&frame, &view, true, Some(path)) {
            Ok(()) => {
                drop(r);
                self.shared.emit(Event::Screenshot(path.display().to_string()));
            }
            Err(e) => {
                drop(r);
                self.shared.emit(Event::Error(format!("画面を保存できません: {e}")));
            }
        }
    }

    fn pane_text(&self, id: i64) -> Option<String> {
        let s = self.shared.session()?;
        let p = self.panes.iter().find(|p| p.id == id)?;
        let (rows, scroll) = {
            let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            (r.rows, r.scroll)
        };
        let term = s.lock();
        let (_, trows) = term.size();
        let first = -(scroll as i64);
        let _ = trows;
        term.text_rows(first.max(0) as u32, (first.max(0) as u32) + rows.saturating_sub(1) as u32, false)
    }

    fn debug_info(&self) -> DebugInfo {
        let mut info = DebugInfo { full_draws: self.shared.draws.load(Ordering::Relaxed), ..DebugInfo::default() };
        if let Some(p) = self.current_pane() {
            let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            info.cell = r.renderer.cell_size();
            info.padding = r.renderer.padding();
        }
        let (visible, blinking, shape) = *self.shared.cursor_dbg.lock().unwrap_or_else(|e| e.into_inner());
        info.cursor_visible = visible;
        info.cursor_blinking = blinking;
        info.cursor_shape = shape;
        // 色は描くときに写したフレームから（Terminal からは render state 経由でしか読めない）
        let c = *self.shared.colors_dbg.lock().unwrap_or_else(|e| e.into_inner());
        info.foreground = c.0;
        info.background = c.1;
        info.palette1 = c.2;
        info
    }

    fn publish_panes(&self) {
        let panes: Vec<PaneInfo> = self
            .panes
            .iter()
            .map(|p| {
                let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
                PaneInfo {
                    id: p.id,
                    hwnd: Retained::as_ptr(&p.layer) as isize,
                    cols: r.cols,
                    rows: r.rows,
                    scrolled: r.scroll > 0,
                }
            })
            .collect();
        let any = !panes.is_empty();
        let first = panes.first().map(|p| p.hwnd).unwrap_or(0);
        self.shared.update(|s| {
            s.panes = panes;
            s.view = any;
            s.hwnd = first;
        });
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        for p in &self.panes {
            p.layer.removeFromSuperlayer();
            p.render.lock().unwrap_or_else(|e| e.into_inner()).visible = false;
        }
        self.panes.clear();
        self.sync_panes();
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        if let Some(s) = self.shared.session() {
            let shared = self.shared.clone();
            // 閉じるのは待たない（PTY の後始末は別のスレッド。T-076）
            std::thread::spawn(move || {
                let closing = s.close_with(CLOSE_TIMEOUT);
                closing.wait(CLOSE_TIMEOUT + Duration::from_secs(2));
                shared.update(|st| st.closed = true);
            });
        } else {
            self.shared.update(|st| st.closed = true);
        }
        self.shared.update(|st| st.alive = false);
    }
}

/// セッションからの知らせ（PTY の読みのスレッドから呼ばれる）
fn on_session_event(shared: &Arc<Shared>, e: SessionEvent) {
    match e {
        SessionEvent::Output => shared.mark_dirty(),
        SessionEvent::Title => {
            if let Some(s) = shared.session() {
                let t = s.title();
                shared.update(|st| st.title = t);
            }
        }
        SessionEvent::Pwd => {
            if let Some(s) = shared.session() {
                let p = s.pwd();
                shared.update(|st| st.pwd = p);
            }
        }
        SessionEvent::Bell => shared.emit(Event::Bell),
        SessionEvent::Exited => {
            let code = shared.session().and_then(|s| s.exit_status()).and_then(|e| e.code);
            shared.update(|st| {
                st.exited = true;
                st.exit_code = code;
            });
            shared.emit(Event::Exited(code));
        }
        SessionEvent::Closed => shared.update(|st| st.closed = true),
        SessionEvent::ClipboardWrite(t) => shared.emit(Event::Clipboard(t)),
        SessionEvent::Notification { title, body } => shared.emit(Event::Notification { title, body }),
        SessionEvent::Progress { state, percent } => shared.emit(Event::Progress {
            state: match state {
                et_vt::ProgressState::Set => "set",
                et_vt::ProgressState::Remove => "remove",
                et_vt::ProgressState::Error => "error",
                et_vt::ProgressState::Indeterminate => "indeterminate",
                et_vt::ProgressState::Pause => "pause",
            },
            percent,
        }),
        SessionEvent::ShellCommand(c) => shared.emit(Event::ShellCommand(c)),
        SessionEvent::CommandEnd(code) => shared.emit(Event::CommandEnd(code)),
    }
}

/// ペインのスクロール位置に合わせた描き方
fn pane_view(r: &PaneRender, frame: &Frame) -> FrameView {
    FrameView {
        top: frame.viewport_top as i32 - r.scroll as i32,
        cols: r.cols,
        rows: r.rows,
        show_cursor: r.scroll == 0,
        hover: None,
        blink_off: false,
    }
}

/// 描画スレッド（端末ごとに 1 本）。`nextDrawable` で垂直同期まで待つので Lisp のスレッドでは描かない
fn render_main(shared: Arc<Shared>) {
    let mut rs = match RenderState::new() {
        Ok(rs) => rs,
        Err(e) => {
            shared.emit(Event::Error(format!("render state を作れません: {e}")));
            return;
        }
    };
    let mut frame = Frame::default();
    loop {
        {
            let mut dirty = shared.dirty.lock().unwrap_or_else(|e| e.into_inner());
            while !*dirty && !shared.stop.load(Ordering::Acquire) {
                let (d, _) = shared.wake.wait_timeout(dirty, RENDER_TICK).unwrap_or_else(|e| e.into_inner());
                dirty = d;
                break;
            }
            *dirty = false;
        }
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let Some(session) = shared.session() else { continue };
        let panes = shared.panes.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if panes.is_empty() {
            continue;
        }
        session.take_output();
        let hold = {
            let mut term = session.lock();
            let hold = term.render_hold(RENDER_HOLD_TIMEOUT);
            if let Some(held) = term.take_held_render_state() {
                rs = held;
            }
            if hold.is_none() && let Err(e) = rs.update(&term) {
                log(&format!("render state を更新できません: {e}"));
                continue;
            }
            frame.capture_images(&mut rs, &term);
            hold
        };
        let changed = frame.update(&mut rs);
        if !changed && hold.is_none() {
            continue;
        }
        let mut cursor = None;
        for (_, render) in &panes {
            let mut r = render.lock().unwrap_or_else(|e| e.into_inner());
            if !r.visible {
                continue;
            }
            let view = pane_view(&r, &frame);
            if r.selected || cursor.is_none() {
                cursor = cursor_rect(&r, &frame, &view);
            }
            if let Err(e) = r.renderer.draw_view(&frame, &view, true, None) {
                log(&format!("描画に失敗: {e}"));
            }
        }
        shared.draws.fetch_add(1, Ordering::Relaxed);
        *shared.colors_dbg.lock().unwrap_or_else(|e| e.into_inner()) =
            (frame.foreground, frame.background, frame.palette[1]);
        *shared.cursor_dbg.lock().unwrap_or_else(|e| e.into_inner()) = (
            frame.cursor.visible,
            frame.cursor.blinking,
            match frame.cursor.shape {
                et_vt::CursorShape::Block => "block",
                et_vt::CursorShape::BlockHollow => "block-hollow",
                et_vt::CursorShape::Bar => "bar",
                et_vt::CursorShape::Underline => "underline",
            },
        );
        // カーソルの位置は世代を上げない（出力のたびに変わる）
        shared.state.lock().unwrap_or_else(|e| e.into_inner()).cursor = cursor;
        if hold.is_some() {
            shared.mark_dirty();
            std::thread::sleep(Duration::from_millis(16));
        }
    }
}

/// カーソルの矩形（ペインの中の画素。論理ポイント）
fn cursor_rect(r: &PaneRender, frame: &Frame, view: &FrameView) -> Option<(i32, i32, i32, i32)> {
    let (x, y) = frame.cursor.pos.filter(|_| frame.cursor.visible && view.show_cursor)?;
    let fy = frame.viewport_top as i32 + y as i32 - view.top;
    if !(0..view.rows as i32).contains(&fy) {
        return None;
    }
    let (cw, ch) = r.renderer.cell_size();
    let pad = r.renderer.padding();
    let scale = r.renderer.scale().max(0.01);
    let to_pt = |v: u32| (v as f32 / scale).round() as i32;
    Some((
        to_pt(pad + x as u32 * cw),
        to_pt(pad + fy as u32 * ch),
        to_pt(cw),
        to_pt(ch),
    ))
}

/// Lisp が渡す「z 順の番号 : フレームの幅 : 高さ（論理ポイント）」から `EmacsView` を引く（T-121 §3.7）。
/// 戻り値はビューと、そのウィンドウの拡大率
fn emacs_view(spec: &str) -> Option<(Retained<NSView>, f32)> {
    let mtm = objc2::MainThreadMarker::new()?;
    let mut it = spec.split(':');
    let index: usize = it.next()?.trim().parse().ok()?;
    let want_w: f64 = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);
    let want_h: f64 = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);

    let app = NSApplication::sharedApplication(mtm);
    let windows = app.orderedWindows();
    let mut found: Vec<(Retained<NSWindow>, Retained<NSView>)> = Vec::new();
    for w in windows.iter() {
        let Some(content) = w.contentView() else { continue };
        for v in content.subviews().iter() {
            if v.class().name().to_str().ok() == Some("EmacsView") {
                found.push((w.clone(), v.clone()));
                break;
            }
        }
    }
    if found.is_empty() {
        return None;
    }
    // 大きさが合うものを選ぶ（同じ大きさが複数あれば z 順の番号で）
    let fits = |v: &Retained<NSView>| {
        let b = v.frame().size;
        (b.width - want_w).abs() < 1.5 && (b.height - want_h).abs() < 1.5
    };
    let pick = found
        .get(index)
        .filter(|(_, v)| want_w <= 0.0 || fits(v))
        .or_else(|| found.iter().find(|(_, v)| fits(v)))
        .or_else(|| found.get(index))
        .or_else(|| found.first())?;
    let scale = pick.0.backingScaleFactor() as f32;
    Some((pick.1.clone(), scale))
}

/// 今の画面（ビューポート）のテキスト（デバッグ・自動確認用）
pub fn screen_text(session: &Session) -> Result<String, String> {
    let mut rs = RenderState::new().map_err(|e| e.to_string())?;
    {
        let t = session.lock();
        rs.update(&t).map_err(|e| e.to_string())?;
    }
    let mut frame = Frame::default();
    frame.update(&mut rs);
    let mut text = String::new();
    for y in 0..frame.rows {
        text.push_str(&frame.row_text(y));
        text.push('\n');
    }
    Ok(text)
}
