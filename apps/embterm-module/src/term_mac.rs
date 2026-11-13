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
/// 点滅の半周期（カーソルと SGR 5。xterm の既定に近い値。T-035 / T-112）
const BLINK_MS: u64 = 600;

// ---- Lisp とやりとりする型（Windows 版と同じ形）----

/// マウスの出来事（Lisp から来る）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    Up,
    /// ボタンを押したままの移動（ドラッグ）
    Drag,
    /// ボタンを押していない移動
    Move,
}

impl std::str::FromStr for MouseKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "down" => Ok(Self::Down),
            "up" => Ok(Self::Up),
            "drag" => Ok(Self::Drag),
            "move" => Ok(Self::Move),
            _ => Err(format!("down / up / drag / move のどれか（{s}）")),
        }
    }
}

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
    /// マウス（T-127）。`x` `y` はペインの中の論理ポイント。Emacs が受けたイベントを Lisp が渡す
    Mouse { pane: i64, x: i32, y: i32, kind: MouseKind, button: i32, mods: String },
    /// IME の変換中の文字列（T-127）。空なら消す
    SetPreedit(String),
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

/// copy モード用のテキスト（`Event::CopyReady` のあと Lisp が分けて受け取る。T-086 / T-127）
struct CopyText {
    text: String,
    /// Lisp に渡し終えたバイト数
    sent: usize,
    /// 見ているペインの先頭の行（テキストの行番号。0 から）
    top: i64,
    /// ポイントを置く行
    point: i64,
    /// 端末が折り返した行の番号
    wraps: Vec<i64>,
}

/// 1 回に Lisp へ渡す大きさ（T-086。大きなスクロールバックで Emacs を止めない）
const COPY_CHUNK: usize = 1 << 20;

/// copy モードのテキストの一部
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
    /// 置いてある `EmacsView` のアドレスと、その中での置き場所（論理ポイント。IME の位置に使う。T-129）
    view_ptr: usize,
    origin: (i32, i32),
    /// デバイス画素
    size: (u32, u32),
    cols: u16,
    rows: u16,
    /// 遡っている行数（0 なら最新）
    scroll: i64,
    visible: bool,
    /// 選んでいるペイン（端末の大きさを決める）
    selected: bool,
    /// マウスの下のハイパーリンクの範囲（ペインの行, 始めの桁, 終わりの桁。T-101 / T-129）
    hover: Option<(u16, u16, u16)>,
}

// SAFETY: 中身は Metal のオブジェクト（デバイス・キュー・テクスチャ）と CAMetalLayer。
// どれも別のスレッドから描いてよい（Apple の決まり）。ビュー階層には触らない
unsafe impl Send for PaneRender {}

pub struct Shared {
    state: Mutex<State>,
    events: Mutex<Vec<Event>>,
    session: OnceLock<Arc<Session>>,
    selection: Mutex<Option<String>>,
    /// copy モードのテキスト（T-127）
    copy: Mutex<Option<CopyText>>,
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
    /// 点滅を切り替えた回数（デバッグ用）
    blink: AtomicU64,
    /// Kitty graphics のアニメーションを進めた回数（デバッグ用。T-092）
    animation: AtomicU64,
    /// 入力から表示までの遅れの測定（T-054 / T-128）
    probe: Mutex<LatencyProbe>,
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

    /// copy モードのテキストを少しずつ渡す（T-086 と同じ形）
    pub fn take_copy(&self) -> Option<CopyChunk> {
        let mut slot = self.copy.lock().unwrap_or_else(|e| e.into_inner());
        let c = slot.as_mut()?;
        let rest = &c.text[c.sent..];
        let mut end = rest.len();
        if end > COPY_CHUNK {
            end = rest[..COPY_CHUNK].rfind('\n').map_or_else(
                || (COPY_CHUNK..rest.len()).find(|&i| rest.is_char_boundary(i)).unwrap_or(rest.len()),
                |i| i + 1,
            );
        }
        let chunk = CopyChunk {
            text: rest[..end].to_string(),
            top: c.top,
            point: c.point,
            wraps: if c.sent == 0 { std::mem::take(&mut c.wraps) } else { Vec::new() },
            more: c.sent + end < c.text.len(),
        };
        c.sent += end;
        if !chunk.more {
            *slot = None;
        }
        Some(chunk)
    }
}

/// 入力から表示までの遅れの測定（T-054 の macOS 版。T-128）。キーを 1 つ追いかける:
/// Lisp が送った → 読みのスレッドが端末に書いた（出力が来た）→ 描画スレッドが描き終えた
#[derive(Default)]
struct LatencyProbe {
    sent: Option<Instant>,
    /// 送ってから出力が届くまで（ms）
    echo_ms: Vec<f64>,
    /// 送ってから描き終わるまで（ms）
    draw_ms: Vec<f64>,
}

impl LatencyProbe {
    fn line(name: &str, v: &mut Vec<f64>) -> String {
        if v.is_empty() {
            return format!("{name}: 無し");
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let q = |x: f64| v[((x * (v.len() - 1) as f64).round() as usize).min(v.len() - 1)];
        format!("{name}: n={} p50={:.2} p90={:.2} max={:.2} ms", v.len(), q(0.5), q(0.9), q(1.0))
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
    /// 選択のドラッグの始め（ペインの番号と、画面の座標の桁・行）
    drag: Option<(i64, (u16, u32))>,
    /// いまマウスの下にあるハイパーリンク（変わったときだけ Lisp に知らせる）
    hover_uri: Option<String>,
    mouse: Option<et_vt::MouseEncoder>,
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

    /// いま `EmacsView` が IME に返す矩形（画面座標）と、差し替えが入っているか（自動確認用。T-129）
    pub fn ime_rect(&self) -> Option<(f64, f64, f64, f64, bool)> {
        let id = self.id;
        APPS.with(|apps| {
            let apps = apps.borrow();
            let app = apps.get(&id)?;
            let pane = app.current_pane()?;
            let host = pane.host.as_ref()?;
            Some((
                crate::ime_mac::ask_view_rect(host).0,
                crate::ime_mac::ask_view_rect(host).1,
                crate::ime_mac::ask_view_rect(host).2,
                crate::ime_mac::ask_view_rect(host).3,
                crate::ime_mac::installed(),
            ))
        })
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
        copy: Mutex::new(None),
        panes: Mutex::new(Vec::new()),
        dirty: Mutex::new(false),
        wake: Condvar::new(),
        stop: AtomicBool::new(false),
        draws: AtomicU64::new(0),
        cursor_dbg: Mutex::new((false, false, "block")),
        colors_dbg: Mutex::new((DEFAULT_FG, DEFAULT_BG, Rgb::default())),
        blink: AtomicU64::new(0),
        animation: AtomicU64::new(0),
        probe: Mutex::new(LatencyProbe::default()),
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
        drag: None,
        hover_uri: None,
        mouse: et_vt::MouseEncoder::new().ok(),
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
            Request::Mouse { pane, x, y, kind, button, mods } => self.mouse(pane, x, y, kind, button, &mods),
            Request::SetPreedit(text) => self.set_preedit(text),
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
                if !on {
                    crate::ime_mac::clear();
                }
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
                let _ = reply.send(self.perf_report(reset));
            }
            Request::Close => self.close(),
            Request::Copy(pane) => self.prepare_copy(pane),
            Request::LinkAt { pane, x, y, reply } => {
                let _ = reply.send(self.link_at(pane, x, y));
            }
            Request::DebugHover { pane, x, y } => self.update_hover(pane, Some((x, y))),
        }
    }

    /// 出力のあと、描画スレッドを起こす。入力の時刻も記録する（T-128 の測定）
    fn follow_output(&self) {
        {
            let mut p = self.shared.probe.lock().unwrap_or_else(|e| e.into_inner());
            // エコーが来なかった（表示の変わらないキー）ものは 1 秒で捨てて測り直す
            if p.sent.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
                p.sent = Some(Instant::now());
            }
        }
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

        let host_ptr = Retained::as_ptr(&host) as usize;
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
            r.view_ptr = host_ptr;
            r.origin = (x, y);
            (c, rw)
        };
        if self.selected.is_none() {
            self.selected = Some(id);
        }
        // IME の候補ウィンドウを端末のカーソルに合わせる（T-129。1 回だけ差し替える）
        crate::ime_mac::install();
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
            view_ptr: 0,
            origin: (0, 0),
            size: (1, 1),
            cols: 1,
            rows: 1,
            scroll: 0,
            visible: false,
            selected: false,
            hover: None,
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
                crate::ime_mac::clear();
            }
            self.sync_panes();
            self.publish_panes();
        }
    }

    fn select(&mut self, id: Option<i64>) {
        if id.is_none() {
            // 端末のウィンドウを選んでいない間は、IME の位置を元に戻す
            crate::ime_mac::clear();
        }
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
            self.follow_output();
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
            ScrollAmount::Prompt(n) => {
                // シェルのプロンプト（OSC 133）へ飛ぶ（T-119）。その行をペインの一番上にする
                let here = {
                    let t = s.lock();
                    let sb = t.scrollbar();
                    (sb.total as i64 - sb.len as i64 - self.pane_scroll(id)).max(0)
                };
                let mut row = here;
                for _ in 0..n.unsigned_abs().max(1) {
                    match s.find_prompt(row, n < 0) {
                        Some(r) => row = r,
                        None => break,
                    }
                }
                let max = {
                    let t = s.lock();
                    let sb = t.scrollbar();
                    (sb.total as i64 - sb.len as i64).max(0)
                };
                if let Some(p) = self.panes.iter().find(|p| p.id == id) {
                    p.render.lock().unwrap_or_else(|e| e.into_inner()).scroll = (max - row).clamp(0, max);
                }
                self.publish_panes();
                self.shared.mark_dirty();
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

    fn pane_scroll(&self, id: i64) -> i64 {
        self.panes
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.render.lock().unwrap_or_else(|e| e.into_inner()).scroll)
            .unwrap_or(0)
    }

    /// copy モードのテキストを作る（T-086 / T-127）。できたら [`Event::CopyReady`]
    fn prepare_copy(&mut self, pane: Option<i64>) {
        let Some(s) = self.shared.session() else {
            *self.shared.copy.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(CopyText { text: String::new(), sent: 0, top: 0, point: 0, wraps: Vec::new() });
            self.shared.emit(Event::CopyReady);
            return;
        };
        let id = pane.or(self.selected).or(self.panes.first().map(|p| p.id));
        let scroll = id.map(|i| self.pane_scroll(i)).unwrap_or(0);
        let rows = id
            .and_then(|i| self.panes.iter().find(|p| p.id == i))
            .map(|p| p.render.lock().unwrap_or_else(|e| e.into_inner()).rows as i64)
            .unwrap_or(24);
        let t0 = Instant::now();
        let (text, top, point, wraps) = {
            let term = s.lock();
            let text = term.format_plain(false, false).ok().flatten().unwrap_or_default();
            let (cols, _) = term.size();
            // 折り返した行（T-086）。折り返す行はほぼ端まで埋まっているので、短い行は端末に尋ねない
            let wraps: Vec<i64> = text
                .split('\n')
                .enumerate()
                .filter(|(_, l)| l.chars().count() * 2 + 2 >= cols as usize)
                .filter(|(i, _)| term.row_wraps(*i as u32))
                .map(|(i, _)| i as i64)
                .collect();
            let total = term.total_rows() as i64;
            let cursor_row = total - rows + term.cursor().1 as i64;
            let top = (total - rows - scroll).max(0);
            let point = if scroll > 0 { top } else { cursor_row };
            (text, top, point, wraps)
        };
        log(&format!(
            "copy モードのテキスト: {} バイト、折り返し {} 行、{} ms",
            text.len(),
            wraps.len(),
            t0.elapsed().as_millis()
        ));
        *self.shared.copy.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(CopyText { text, sent: 0, top, point, wraps });
        self.shared.emit(Event::CopyReady);
    }

    /// マウスの下のハイパーリンク（OSC 8）に下線を引く（T-101 の macOS 版。T-129）。
    /// レイヤーはマウスの動きを受けないので、Lisp がポーリングで位置を送る
    fn update_hover(&mut self, id: i64, at: Option<(i32, i32)>) {
        let found = at.and_then(|(x, y)| {
            let s = self.shared.session()?;
            let p = self.panes.iter().find(|p| p.id == id)?;
            let (cols, rows, scroll, scale) = {
                let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
                (r.cols, r.rows, r.scroll, r.renderer.scale())
            };
            let (cx, cy) = {
                let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
                r.renderer.cell_at((x as f32 * scale) as i32, (y as f32 * scale) as i32, cols, rows)
            };
            let term = s.lock();
            let row = screen_row(&term, rows, scroll, cy);
            let uri_at = |x: u16| term.grid_ref(x, row).ok().and_then(|r| term.hyperlink_uri(r));
            let uri = uri_at(cx)?;
            // 同じ URI が続く範囲を左右に広げる
            let mut start = cx;
            while start > 0 && uri_at(start - 1).as_deref() == Some(uri.as_str()) {
                start -= 1;
            }
            let mut end = cx + 1;
            while end < cols && uri_at(end).as_deref() == Some(uri.as_str()) {
                end += 1;
            }
            Some((uri, (cy, start, end)))
        });
        let span = found.as_ref().map(|f| f.1);
        let mut changed = false;
        for p in &self.panes {
            let want = if p.id == id { span } else { None };
            let mut r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            if r.hover != want {
                r.hover = want;
                changed = true;
            }
        }
        let uri = found.map(|f| f.0);
        if uri != self.hover_uri {
            self.hover_uri = uri.clone();
            self.shared.emit(Event::Hover(uri));
        }
        if changed {
            self.shared.mark_dirty();
        }
    }

    /// ペインの画素の位置にあるハイパーリンク（OSC 8。T-127）
    fn link_at(&self, id: i64, x: i32, y: i32) -> Option<String> {
        let s = self.shared.session()?;
        let p = self.panes.iter().find(|p| p.id == id)?;
        let (cols, rows, scroll, scale) = {
            let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            (r.cols, r.rows, r.scroll, r.renderer.scale())
        };
        let (cx, cy) = {
            let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            r.renderer.cell_at((x as f32 * scale) as i32, (y as f32 * scale) as i32, cols, rows)
        };
        let term = s.lock();
        let row = screen_row(&term, rows, scroll, cy);
        let at = term.grid_ref(cx, row).ok()?;
        term.hyperlink_uri(at)
    }

    /// マウス（T-127）。アプリがマウスを見ていれば送り、そうでなければ選択に使う
    fn mouse(&mut self, id: i64, x: i32, y: i32, kind: MouseKind, button: i32, mods: &str) {
        let Some(s) = self.shared.session() else { return };
        let Some(p) = self.panes.iter().find(|p| p.id == id) else { return };
        let (cols, rows, scroll, cell, pad, scale, size) = {
            let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            (r.cols, r.rows, r.scroll, r.renderer.cell_size(), r.renderer.padding(), r.renderer.scale(), r.size)
        };
        // Lisp がくれるのは論理ポイント。描画はデバイス画素
        let (px, py) = ((x as f32 * scale) as i32, (y as f32 * scale) as i32);
        let m = keys_mac::parse_mods(mods);

        // Ctrl+クリックしたのがハイパーリンクなら、開くのは Lisp に任せる（選択もアプリへの報告もしない）
        if kind == MouseKind::Down
            && m.ctrl
            && let Some(uri) = self.link_at(id, x, y)
        {
            log(&format!("リンクを Ctrl+クリック: {uri}"));
            self.shared.emit(Event::Link(uri));
            return;
        }

        let tracking = { s.lock().mouse_tracking() };
        if tracking && !m.shift {
            // アプリへ報告する（Shift を押している間は Emacs 側の選択。端末の慣習）
            let bytes = {
                let term = s.lock();
                let geometry = et_vt::MouseGeometry {
                    screen_width: size.0,
                    screen_height: size.1,
                    cell_width: cell.0,
                    cell_height: cell.1,
                    padding: pad,
                };
                let action = match kind {
                    MouseKind::Down => et_vt::MouseAction::Press,
                    MouseKind::Up => et_vt::MouseAction::Release,
                    _ => et_vt::MouseAction::Motion,
                };
                let btn = match button {
                    1 => Some(et_vt::MouseButton::Left),
                    2 => Some(et_vt::MouseButton::Middle),
                    3 => Some(et_vt::MouseButton::Right),
                    _ => None,
                };
                let any = matches!(kind, MouseKind::Down | MouseKind::Drag);
                match self.mouse.as_mut() {
                    Some(enc) => enc.encode(&term, geometry, action, btn, any, (px as f32, py as f32), m),
                    None => Vec::new(),
                }
            };
            if !bytes.is_empty() {
                s.write(&bytes);
            }
            return;
        }

        // 選択（遡っているペインでも効く。画面の座標に直す。T-129）
        let (cx, cy) = {
            let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            r.renderer.cell_at(px, py, cols, rows)
        };
        let row = {
            let t = s.lock();
            screen_row(&t, rows, scroll, cy)
        };
        match kind {
            MouseKind::Down => {
                self.drag = Some((id, (cx, row)));
                let _ = s.lock().set_selection(None);
                *self.shared.selection.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            MouseKind::Drag => {
                if let Some((start_pane, start)) = self.drag
                    && start_pane == id
                {
                    let _ = s.lock().set_selection_screen(Some((start, (cx, row))));
                }
            }
            MouseKind::Up => {
                if let Some((start_pane, start)) = self.drag.take()
                    && start_pane == id
                {
                    let mut term = s.lock();
                    let _ = term.set_selection_screen(Some((start, (cx, row))));
                    let text = term.format_plain(true, true).ok().flatten();
                    drop(term);
                    if let Some(text) = text.filter(|t| !t.is_empty()) {
                        *self.shared.selection.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.clone());
                        self.shared.emit(Event::Selected(text));
                    }
                }
            }
            // 負の位置は「ペインから外れた」の印（Lisp がそう送る）
            MouseKind::Move => self.update_hover(id, (x >= 0 && y >= 0).then_some((x, y))),
        }
        self.shared.mark_dirty();
    }

    /// IME の変換中の文字列（T-127）
    fn set_preedit(&mut self, text: String) {
        let text = if text.is_empty() { None } else { Some(text) };
        for p in &self.panes {
            let mut r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            r.renderer.set_preedit(text.clone());
        }
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
        let mut info = DebugInfo {
            full_draws: self.shared.draws.load(Ordering::Relaxed),
            blink_ticks: self.shared.blink.load(Ordering::Relaxed),
            blink_timer: self.shared.blink.load(Ordering::Relaxed) > 0,
            animation_ticks: self.shared.animation.load(Ordering::Relaxed),
            animation_playing: self.shared.animation.load(Ordering::Relaxed) > 0,
            ..DebugInfo::default()
        };
        if let Some(p) = self.current_pane() {
            let r = p.render.lock().unwrap_or_else(|e| e.into_inner());
            info.cell = r.renderer.cell_size();
            info.padding = r.renderer.padding();
            info.hover = r.hover;
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

    /// 性能の測定の報告（T-054 の macOS 版。読みのスレッド + 描画 + 入力の遅れ + PNG）
    fn perf_report(&self, reset: bool) -> String {
        let mut out = self.shared.session().map(|s| s.perf_report(reset)).unwrap_or_default();
        if let Some(p) = self.current_pane() {
            let st = p.render.lock().unwrap_or_else(|e| e.into_inner()).renderer.stats();
            let avg = |d: Duration, n: u64| if n == 0 { 0.0 } else { d.as_secs_f64() * 1000.0 / n as f64 };
            out.push_str(&format!(
                "draw.count={} present={} skipped={} instances={}\n\
                 draw.ms avg={:.2} max={:.2}（うち垂直同期の待ち avg={:.2} max={:.2}）\n",
                st.drawn,
                st.presented,
                st.skipped,
                st.instances,
                avg(st.total_draw, st.drawn),
                st.max_draw.as_secs_f64() * 1000.0,
                avg(st.total_wait, st.drawn),
                st.max_wait.as_secs_f64() * 1000.0,
            ));
        }
        {
            let mut probe = self.shared.probe.lock().unwrap_or_else(|e| e.into_inner());
            let echo = LatencyProbe::line("input.echo", &mut probe.echo_ms);
            let draw = LatencyProbe::line("input.draw", &mut probe.draw_ms);
            out.push_str(&format!("{echo}\n{draw}\n"));
            if reset {
                *probe = LatencyProbe::default();
            }
        }
        let png = et_vt::png_decode_stats();
        if png.count > 0 {
            out.push_str(&format!(
                "png.count={} total={:.1} ms max={:.1} ms last={:.1} ms last_pixels={}\n",
                png.count,
                png.total_us as f64 / 1000.0,
                png.max_us as f64 / 1000.0,
                png.last_us as f64 / 1000.0,
                png.last_pixels
            ));
        }
        out
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
        crate::ime_mac::clear();
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
        SessionEvent::Output => {
            {
                let mut p = shared.probe.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(t) = p.sent {
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    p.echo_ms.push(ms);
                }
            }
            shared.mark_dirty();
        }
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

/// ペインの行 `cy` が、端末の画面（スクロールバックを含む）の何行目か（T-129）
fn screen_row(term: &et_vt::Terminal, rows: u16, scroll: i64, cy: u16) -> u32 {
    let total = term.total_rows() as i64;
    (total - rows as i64 - scroll + cy as i64).clamp(0, total.saturating_sub(1)).max(0) as u32
}

/// ペインのスクロール位置に合わせた描き方
fn pane_view(r: &PaneRender, frame: &Frame) -> FrameView {
    FrameView {
        top: frame.viewport_top as i32 - r.scroll as i32,
        cols: r.cols,
        rows: r.rows,
        show_cursor: r.scroll == 0,
        hover: r.hover,
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
    // 点滅（カーソルと SGR 5）。見せている side と、最後に切り替えた時刻
    let mut blink_on = true;
    let mut blink_at = Instant::now();
    let mut blink_ticks = 0u64;
    // Kitty graphics のアニメーション（T-092）。次のフレームの時刻（epoch からの ms）
    let epoch = Instant::now();
    let mut animation_next: Option<u64> = None;
    let mut animation_ticks = 0u64;
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
            // Kitty graphics のアニメーションを今の時刻まで進める（T-092。進んだ画像は世代が変わる）
            let now_ms = epoch.elapsed().as_millis() as u64;
            let next = term.kitty_animation_tick(now_ms);
            if let Some(next_ms) = next {
                animation_next = Some(next_ms);
                animation_ticks += 1;
            } else {
                animation_next = None;
            }
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
        // 点滅（カーソル・SGR 5）。見えているものがあるときだけ切り替える
        let blinking = frame.cursor.blinking && frame.cursor.visible;
        let text_blink = frame.has_blink();
        let mut blinked = false;
        if (blinking || text_blink) && blink_at.elapsed() >= Duration::from_millis(BLINK_MS) {
            blink_on = !blink_on;
            blink_at = Instant::now();
            blink_ticks += 1;
            blinked = true;
        } else if !blinking && !text_blink && !blink_on {
            blink_on = true;
            blinked = true;
        }
        if !changed && !blinked && animation_next.is_none() && hold.is_none() {
            // 点滅があるときは次の切り替えのために起こし直す
            if blinking || text_blink {
                shared.mark_dirty();
                std::thread::sleep(Duration::from_millis(BLINK_MS / 6));
            }
            continue;
        }
        let mut cursor = None;
        let mut ime: Option<(usize, objc2_core_foundation::CGRect)> = None;
        for (_, render) in &panes {
            let mut r = render.lock().unwrap_or_else(|e| e.into_inner());
            if !r.visible {
                continue;
            }
            let mut view = pane_view(&r, &frame);
            view.blink_off = text_blink && !blink_on;
            view.show_cursor = view.show_cursor && (!blinking || blink_on);
            if r.selected || cursor.is_none() {
                cursor = cursor_rect(&r, &frame, &view);
                // IME の候補ウィンドウの位置（ビュー座標 = ペインの置き場所 + カーソル。T-129）
                ime = cursor.filter(|_| r.selected || panes.len() == 1).map(|(x, y, w, h)| {
                    (
                        r.view_ptr,
                        objc2_core_foundation::CGRect::new(
                            objc2_core_foundation::CGPoint::new((r.origin.0 + x) as f64, (r.origin.1 + y) as f64),
                            objc2_core_foundation::CGSize::new(w as f64, h as f64),
                        ),
                    )
                });
            }
            if let Err(e) = r.renderer.draw_view(&frame, &view, true, None) {
                log(&format!("描画に失敗: {e}"));
            }
        }
        shared.draws.fetch_add(1, Ordering::Relaxed);
        shared.blink.store(blink_ticks, Ordering::Relaxed);
        {
            let mut p = shared.probe.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(t) = p.sent.take() {
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                p.draw_ms.push(ms);
            }
        }
        if blinking || text_blink {
            // 次の切り替えのために起こす
            shared.mark_dirty();
        }
        shared.animation.store(animation_ticks, Ordering::Relaxed);
        if let Some(next_ms) = animation_next {
            // アニメーションの次のフレームの時刻に起こす（長くても 1 秒で見回る）
            let now = epoch.elapsed().as_millis() as u64;
            let wait = next_ms.saturating_sub(now).clamp(8, 1000);
            let shared2 = shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(wait));
                shared2.mark_dirty();
            });
        }
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
        match ime {
            Some((view, rect)) => crate::ime_mac::set_rect_raw(view, Some(rect)),
            None => crate::ime_mac::clear(),
        }
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
