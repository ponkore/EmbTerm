//! 端末 1 つ分の UI スレッドと、Emacs のフレームに重ねる子 HWND（T-028）。
//!
//! ```text
//! Lisp のスレッド                     端末の UI スレッド（1 端末に 1 本）                 et-session のスレッド
//! ─────────────                     ─────────────────────────────                    ─────────────────────
//! embterm-module-place など           制御用のウィンドウ（HWND_MESSAGE、端末が生きている間）   PTY の読み → vt_write
//!   Shared.requests に積む              WM_APP_REQUEST: 列を全部取り出して片付ける             └ PostMessage(制御, WM_APP_OUTPUT)
//!   PostMessage(制御, WM_APP_REQUEST) ▶  WM_APP_OUTPUT: 写して描く                           子の終了など → PostMessage(制御, WM_APP_SESSION)
//! embterm-module-poll ◀────────────── Shared.state / events を書く                        閉じる（session-close）→ Closed
//!                                     表示用の子 HWND（WS_CHILD、親 = Emacs のフレーム）
//!                                       作り直し・付け替え（SetParent）は Place のたび
//! ```
//!
//! - **Lisp → 端末は待たない**（列に積んで HWND 宛ての `PostMessage`。モーダルループ中も取りこぼさない）
//! - **端末 → Lisp は共有の状態に書くだけ**（Lisp がタイマーでポーリングする。T-014 の初版）
//! - 子 HWND は親（フレーム）が消されると一緒に消える。**端末（[`Session`]）と UI スレッドは残り**、
//!   次の Place で子 HWND を作り直す（T-066）。制御用のウィンドウはメッセージ専用なので親に巻き込まれない
//! - 子 HWND は作ったら `HWND_BOTTOM`、動かすときは `SWP_NOZORDER`。`WS_EX_NOACTIVATE` と `MA_NOACTIVATE` で
//!   フォーカスを取らない（キーは Emacs が受けて module に送る。T-009 の案 A）
//! - UI スレッドの wndproc とスレッドの本体は `catch_unwind` で包む（T-031）

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use et_render::{FontSpec, Frame, NarrowWide, Renderer};
use et_session::{Event as SessionEvent, Session, SessionOptions, keys};
use et_vt::{KeyEncoder, Mode, RenderState, Rgb, Scroll};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::log;

const CONTROL_CLASS: PCWSTR = w!("EmbTermControl");
const VIEW_CLASS: PCWSTR = w!("EmbTermView");
/// 頼みごとが積まれた（制御用のウィンドウ宛て）
const WM_APP_REQUEST: u32 = WM_APP + 1;
/// 端末の中身が変わった（制御用のウィンドウ宛て）
const WM_APP_OUTPUT: u32 = WM_APP + 2;
/// セッションからの知らせ（wParam が [`SessionEvent`] の番号）
const WM_APP_SESSION: u32 = WM_APP + 3;

const DEFAULT_FG: Rgb = Rgb { r: 0xd4, g: 0xd4, b: 0xd4 };
const DEFAULT_BG: Rgb = Rgb { r: 0x1e, g: 0x1e, b: 0x1e };
/// 子 HWND が無いうちに起動するときのセルの大きさ（仮）
const FALLBACK_CELL: (u32, u32) = (8, 16);

/// Lisp からの頼みごと
pub enum Request {
    /// 親（フレームの HWND）のクライアント座標（物理ピクセル）に置いて見せる
    Place { parent: isize, x: i32, y: i32, w: i32, h: i32 },
    Hide,
    /// 文字列をそのまま送る（IME で確定した文字も）
    SendString(String),
    /// キーを 1 つ送る。`key` は 1 文字か Emacs のファンクションキーの名前、`mods` は `C` `M` `S` の組
    SendKey { key: String, mods: String },
    /// 貼り付け（改行を CR にし、ブラケットペーストのモードなら包む）
    Paste(String),
    /// ビューポートを行数だけ動かす（負は上）。`None` ならいちばん下へ
    Scroll(Option<isize>),
    SetFont { family: Option<String>, size: Option<f32> },
    SetNarrowWide(NarrowWide),
    /// フォーカスの出入り（端末へ `CSI I` / `CSI O` を送り、カーソルの描き方を変える）
    Focus(bool),
    /// 今の画面を PNG に保存する（デバッグ用）
    Screenshot(PathBuf),
    Close,
}

/// Lisp に渡す知らせ
#[derive(Clone, Debug)]
pub enum Event {
    /// 子 HWND がクリックされた（Lisp はそのウィンドウを選ぶ）
    Clicked,
    Bell,
    /// 子プロセスが終わった（終了コード）
    Exited(Option<u32>),
    /// 画面を保存した（パス）
    Screenshot(String),
    Error(String),
}

/// Lisp がポーリングで写して読む状態
#[derive(Clone, Default)]
pub struct State {
    /// 何か変わるたびに増える（カーソルの位置は除く）。Lisp は前の値と比べて読み直す
    pub generation: u64,
    /// UI スレッドが生きているか
    pub alive: bool,
    /// 子 HWND があるか（フレームを消すと一緒に消える）
    pub view: bool,
    /// 子 HWND の値（デバッグ・自動確認用。無ければ 0）
    pub hwnd: isize,
    /// 子プロセスを起動したか
    pub started: bool,
    pub pid: u32,
    pub conpty: String,
    pub font: String,
    pub title: String,
    pub pwd: String,
    pub exited: bool,
    pub exit_code: Option<u32>,
    /// 閉じる処理（PTY の後始末）まで終わったか
    pub closed: bool,
    pub cols: u16,
    pub rows: u16,
    pub cell: (u32, u32),
    /// カーソルの矩形（子 HWND のクライアント座標の画素。x y w h）。見えなければ `None`。
    /// 世代は増やさない（出力のたびに変わるので）
    pub cursor: Option<(i32, i32, i32, i32)>,
}

/// 起動の指定
pub struct SpawnOptions {
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub conpty_dir: Option<PathBuf>,
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub narrow_wide: NarrowWide,
}

/// UI スレッドと Lisp のスレッドで分け合うもの
pub struct Shared {
    state: Mutex<State>,
    events: Mutex<Vec<Event>>,
    requests: Mutex<Vec<Request>>,
    /// WM_APP_REQUEST を投げたまま、UI スレッドがまだ列を取り出していない（投げすぎない）
    request_posted: AtomicBool,
    /// 制御用のウィンドウ（0 なら UI スレッドが終わった）
    control: AtomicIsize,
    session: OnceLock<Arc<Session>>,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut s);
        s.generation += 1;
    }

    fn emit(&self, e: Event) {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).push(e);
        self.update(|_| {});
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
}

fn post(hwnd: isize, msg: u32, wparam: usize) -> bool {
    hwnd != 0 && unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(wparam), LPARAM(0)) }.is_ok()
}

/// 端末の取っ手（Lisp の user-ptr の中身）。落とす（GC で回収される）と端末を閉じる
pub struct Term {
    shared: Arc<Shared>,
}

impl Term {
    pub fn shared(&self) -> &Shared {
        &self.shared
    }

    /// 頼みごとを積んで、UI スレッドを起こす（待たない）。UI スレッドがもういなければ、
    /// 文字の送信だけはここで書く（PTY への書き込みはチャネルに積むだけなので待たない）
    pub fn request(&self, r: Request) {
        let control = self.shared.control.load(Ordering::Acquire);
        if control == 0 {
            if let (Some(s), Request::SendString(text)) = (self.shared.session(), &r) {
                s.write(text.as_bytes());
            }
            return;
        }
        self.shared.requests.lock().unwrap_or_else(|e| e.into_inner()).push(r);
        if !self.shared.request_posted.swap(true, Ordering::AcqRel) && !post(control, WM_APP_REQUEST, 0) {
            self.shared.request_posted.store(false, Ordering::Release);
        }
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        // GC の finalizer から呼ばれる。panic を外へ出さない（T-031）
        let _ = catch_unwind(AssertUnwindSafe(|| self.request(Request::Close)));
    }
}

/// 端末の UI スレッドを立てる。制御用のウィンドウができるまでは待つ（数 ms）。子プロセスの起動は最初の Place のとき
pub fn spawn(opts: SpawnOptions) -> Result<Term, String> {
    let shared = Arc::new(Shared {
        state: Mutex::new(State::default()),
        events: Mutex::new(Vec::new()),
        requests: Mutex::new(Vec::new()),
        request_posted: AtomicBool::new(false),
        control: AtomicIsize::new(0),
        session: OnceLock::new(),
    });
    let (tx, rx) = mpsc::channel();
    let s = shared.clone();
    std::thread::Builder::new()
        .name("embterm-term".into())
        .spawn(move || term_main(s, opts, tx))
        .map_err(|e| format!("端末のスレッドを作れません: {e}"))?;
    rx.recv_timeout(Duration::from_secs(5)).map_err(|e| format!("端末のスレッドが応答しません: {e}"))??;
    Ok(Term { shared })
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// 借用中に壊れた子 HWND（App を借りられないときの控え）
    static DESTROYED_VIEW: Cell<isize> = const { Cell::new(0) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn register_classes() {
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let cursor = LoadCursorW(None, IDC_IBEAM).unwrap_or_default();
        // 2 回目以降（2 つ目の端末）は失敗するが、登録済みなので構わない
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(control_proc),
            hInstance: instance.into(),
            lpszClassName: CONTROL_CLASS,
            ..Default::default()
        });
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(view_proc),
            hInstance: instance.into(),
            hCursor: cursor,
            lpszClassName: VIEW_CLASS,
            ..Default::default()
        });
    }
}

fn term_main(shared: Arc<Shared>, opts: SpawnOptions, ready: mpsc::Sender<Result<(), String>>) {
    let ready2 = ready.clone();
    let r = catch_unwind(AssertUnwindSafe(|| {
        register_classes();
        let control = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CONTROL_CLASS,
                w!("EmbTerm control"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                GetModuleHandleW(None).ok().map(|h| h.into()),
                None,
            )
        };
        let control = match control {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(format!("制御用のウィンドウを作れません: {e}")));
                return;
            }
        };
        let app = match App::new(control, shared.clone(), opts) {
            Ok(a) => a,
            Err(e) => {
                unsafe {
                    let _ = DestroyWindow(control);
                }
                let _ = ready.send(Err(e));
                return;
            }
        };
        APP.with(|a| *a.borrow_mut() = Some(app));
        shared.control.store(control.0 as isize, Ordering::Release);
        shared.update(|s| s.alive = true);
        let _ = ready.send(Ok(()));

        let mut msg = MSG::default();
        unsafe {
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }));
    if r.is_err() {
        let _ = ready2.send(Err("端末のスレッドで panic".into()));
        log("端末のスレッドで panic（スレッドを終える）");
        shared.emit(Event::Error("端末のスレッドで panic しました".into()));
    }
    shared.control.store(0, Ordering::Release);
    // App（描画の資源と、端末への参照）を落とす。端末を閉じていなければ閉じる（待たない）
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(app) = APP.with(|a| a.borrow_mut().take()) {
            if let Some(s) = &app.session {
                let _ = s.close();
            }
            drop(app);
        }
    }));
    shared.update(|s| {
        s.alive = false;
        s.view = false;
        s.hwnd = 0;
    });
}

/// 表示用の子 HWND と、それに描くもの
struct View {
    hwnd: HWND,
    parent: HWND,
    renderer: Renderer,
}

/// UI スレッドの状態（スレッドローカル）
struct App {
    control: HWND,
    shared: Arc<Shared>,
    /// 起動の指定（最初の Place で子プロセスを起動するときに取り出す）
    pending: Option<SpawnOptions>,
    session: Option<Arc<Session>>,
    view: Option<View>,
    /// 見せているか（Hide で false）
    shown: bool,
    font: FontSpec,
    narrow_wide: NarrowWide,
    rs: RenderState,
    frame: Frame,
    encoder: KeyEncoder,
    grid: (u16, u16),
    cell: (u32, u32),
    focused: bool,
    wheel: i32,
    drag_anchor: Option<(u16, u16)>,
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
}

impl App {
    fn new(control: HWND, shared: Arc<Shared>, opts: SpawnOptions) -> Result<Self, String> {
        let mut font = FontSpec::default();
        if let Some(f) = &opts.font_family {
            font.families.insert(0, f.clone());
        }
        if let Some(s) = opts.font_size {
            font.size_pt = s;
        }
        Ok(Self {
            control,
            shared,
            narrow_wide: opts.narrow_wide,
            pending: Some(opts),
            session: None,
            view: None,
            shown: false,
            font,
            rs: RenderState::new().map_err(|e| e.to_string())?,
            frame: Frame::default(),
            encoder: KeyEncoder::new().map_err(|e| e.to_string())?,
            grid: (0, 0),
            cell: FALLBACK_CELL,
            focused: false,
            wheel: 0,
            drag_anchor: None,
        })
    }

    /// 子 HWND がまだ生きているか確かめる（親のフレームごと消されていれば捨てる）
    fn check_view(&mut self) {
        let dead = DESTROYED_VIEW.with(|d| d.replace(0));
        if let Some(v) = &self.view
            && (v.hwnd.0 as isize == dead || !unsafe { IsWindow(Some(v.hwnd)) }.as_bool())
        {
            self.view_destroyed();
        }
    }

    fn view_destroyed(&mut self) {
        if self.view.take().is_some() {
            log("子 HWND が壊れた（親のフレームが消された）。端末は残す");
            self.drag_anchor = None;
            self.shared.update(|s| {
                s.view = false;
                s.hwnd = 0;
                s.cursor = None;
            });
        }
    }

    fn drain_requests(&mut self) {
        self.shared.request_posted.store(false, Ordering::Release);
        let requests = std::mem::take(&mut *self.shared.requests.lock().unwrap_or_else(|e| e.into_inner()));
        for r in requests {
            self.apply(r);
        }
    }

    fn apply(&mut self, r: Request) {
        self.check_view();
        match r {
            Request::Place { parent, x, y, w, h } => self.place(HWND(parent as *mut _), x, y, w.max(1), h.max(1)),
            Request::Hide => {
                self.shown = false;
                if let Some(v) = &self.view {
                    unsafe {
                        let _ = ShowWindow(v.hwnd, SW_HIDE);
                    }
                }
            }
            Request::SendString(text) => self.send(text.as_bytes()),
            Request::SendKey { key, mods } => self.send_key(&key, &mods),
            Request::Paste(text) => {
                let Some(s) = self.ensure_session() else { return };
                let bracketed = s.lock().mode(Mode::BRACKETED_PASTE);
                self.send(&keys::paste_bytes(&text, bracketed));
            }
            Request::Scroll(n) => {
                if let Some(s) = &self.session {
                    s.lock().scroll_viewport(match n {
                        Some(n) => Scroll::Delta(n),
                        None => Scroll::Bottom,
                    });
                }
                self.refresh(false);
            }
            Request::SetFont { family, size } => {
                let mut spec = FontSpec::default();
                if let Some(f) = family {
                    spec.families.insert(0, f);
                }
                if let Some(s) = size {
                    spec.size_pt = s;
                }
                self.font = spec.clone();
                if let Some(v) = &mut self.view {
                    if let Err(e) = v.renderer.set_font(spec) {
                        self.shared.emit(Event::Error(format!("フォントを作れません: {e}")));
                    }
                    let family = v.renderer.font_family().to_string();
                    self.shared.update(|s| s.font = family);
                }
                self.refresh(true);
            }
            Request::SetNarrowWide(n) => {
                self.narrow_wide = n;
                if let Some(v) = &mut self.view {
                    v.renderer.theme.narrow_wide = n;
                }
                self.refresh(true);
            }
            Request::Focus(f) => {
                self.focused = f;
                // ConPTY は起動時に ?1004h を求めるので、フォーカスの出入りは必ず送る（T-070）
                if let Some(s) = &self.session {
                    s.write(if f { b"\x1b[I" } else { b"\x1b[O" });
                }
                self.refresh(true);
            }
            Request::Screenshot(path) => self.screenshot(path),
            Request::Close => self.close(),
        }
    }

    /// 子プロセスを（まだなら）起動する。桁数・行数は今の子 HWND から求める（無ければ 80x24）
    fn ensure_session(&mut self) -> Option<Arc<Session>> {
        if let Some(s) = &self.session {
            return Some(s.clone());
        }
        let opts = self.pending.take()?;
        let (cols, rows) = if self.grid.0 > 0 { self.grid } else { (80, 24) };
        self.grid = (cols, rows);
        let mut search = Vec::new();
        if let Some(d) = &opts.conpty_dir {
            search.push(d.clone());
        }
        let control = self.control.0 as isize;
        let weak: Weak<Shared> = Arc::downgrade(&self.shared);
        let notify = Arc::new(move |e: SessionEvent| match e {
            SessionEvent::Output => {
                post(control, WM_APP_OUTPUT, 0);
            }
            // UI スレッドがもう終わっていても届くように、ここで書く（Weak: 端末 → notify → Shared の循環を作らない）
            SessionEvent::Closed => {
                if let Some(s) = weak.upgrade() {
                    s.update(|st| st.closed = true);
                }
            }
            SessionEvent::Bell => {
                if let Some(s) = weak.upgrade() {
                    s.emit(Event::Bell);
                }
            }
            SessionEvent::Title | SessionEvent::Pwd | SessionEvent::Exited => {
                post(control, WM_APP_SESSION, 0);
            }
        });
        let started = Session::start(
            SessionOptions {
                argv: opts.argv.clone(),
                cwd: opts.cwd.clone(),
                cols,
                rows,
                cell_px: self.cell,
                conpty_search: search,
                foreground: DEFAULT_FG,
                background: DEFAULT_BG,
                scrollback_lines: 10_000,
            },
            notify,
        );
        match started {
            Ok(s) => {
                let s = Arc::new(s);
                log(&format!("起動: {:?} / {cols}x{rows} / pid {} / ConPTY: {}", opts.argv, s.pid(), s.conpty_kind()));
                for w in s.warnings() {
                    log(&format!("注意: {w}"));
                }
                let _ = self.shared.session.set(s.clone());
                let (pid, kind) = (s.pid(), s.conpty_kind().to_string());
                self.shared.update(|st| {
                    st.started = true;
                    st.pid = pid;
                    st.conpty = kind;
                    st.cols = cols;
                    st.rows = rows;
                });
                self.session = Some(s.clone());
                Some(s)
            }
            Err(e) => {
                log(&format!("起動できない: {e}"));
                self.shared.emit(Event::Error(e));
                self.shared.update(|st| {
                    st.exited = true;
                    st.exit_code = None;
                });
                self.shared.emit(Event::Exited(None));
                None
            }
        }
    }

    fn create_view(&mut self, parent: HWND, x: i32, y: i32, w: i32, h: i32) -> Result<View, String> {
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_NOPARENTNOTIFY,
                VIEW_CLASS,
                w!("EmbTerm"),
                WS_CHILD | WS_CLIPSIBLINGS,
                x,
                y,
                w,
                h,
                Some(parent),
                None,
                GetModuleHandleW(None).ok().map(|h| h.into()),
                None,
            )
        }
        .map_err(|e| format!("子ウィンドウを作れません: {e}"))?;
        unsafe {
            // 子フレーム（corfu など）を隠さないよう、兄弟の一番下に置く。以後は上げない
            let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let (cw, ch) = client_size(hwnd);
        match Renderer::new(hwnd, cw.max(1), ch.max(1), dpi, self.font.clone()) {
            Ok(mut renderer) => {
                renderer.theme.narrow_wide = self.narrow_wide;
                log(&format!(
                    "子 HWND を作った: 0x{:x} / 親 0x{:x} / フォント {} / セル {:?} / DPI {dpi}{}",
                    hwnd.0 as isize,
                    parent.0 as isize,
                    renderer.font_family(),
                    renderer.cell_size(),
                    if renderer.is_warp() { " / WARP" } else { "" }
                ));
                Ok(View { hwnd, parent, renderer })
            }
            Err(e) => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                Err(format!("描画の準備ができません: {e}"))
            }
        }
    }

    fn place(&mut self, parent: HWND, x: i32, y: i32, w: i32, h: i32) {
        if !unsafe { IsWindow(Some(parent)) }.as_bool() {
            self.shared.emit(Event::Error(format!("親のウィンドウがありません: 0x{:x}", parent.0 as isize)));
            return;
        }
        match &mut self.view {
            Some(v) if v.parent == parent => {}
            Some(v) => unsafe {
                // 別のフレームへ付け替える。兄弟の一番下に置き直す
                let _ = SetParent(v.hwnd, Some(parent));
                let _ = SetWindowPos(v.hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                v.parent = parent;
                log(&format!("子 HWND を付け替えた: 親 0x{:x}", parent.0 as isize));
            },
            None => match self.create_view(parent, x, y, w, h) {
                Ok(v) => {
                    let family = v.renderer.font_family().to_string();
                    let hwnd = v.hwnd.0 as isize;
                    self.view = Some(v);
                    self.shared.update(|s| {
                        s.view = true;
                        s.hwnd = hwnd;
                        s.font = family;
                    });
                }
                Err(e) => {
                    log(&e);
                    self.shared.emit(Event::Error(e));
                    // 表示はできなくても、端末は動かしておく
                    self.ensure_session();
                    return;
                }
            },
        }
        self.shown = true;
        if let Some(v) = &self.view {
            unsafe {
                // SetWindowPos は WM_SIZE を同期で送る（App を借りているので届かない）。大きさは下の refresh の sync_size で拾う
                let _ = SetWindowPos(v.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
        }
        // 桁数・行数を決めてから起動する（起動直後のリサイズを避ける）
        self.sync_size();
        self.ensure_session();
        self.refresh(true);
    }

    /// 子 HWND の大きさとスワップチェーン・端末の大きさを合わせる（取りこぼした WM_SIZE も拾う）
    fn sync_size(&mut self) {
        let Some(v) = &mut self.view else { return };
        let (w, h) = client_size(v.hwnd);
        if w == 0 || h == 0 {
            return;
        }
        if let Err(e) = v.renderer.resize(w, h) {
            log(&format!("スワップチェーンの大きさを変えられません: {e}"));
        }
        let grid = v.renderer.grid_size(w, h);
        let cell = v.renderer.cell_size();
        if grid != self.grid || cell != self.cell {
            self.grid = grid;
            self.cell = cell;
            if let Some(s) = &self.session {
                s.resize(grid.0, grid.1, cell);
                log(&format!("リサイズ: {}x{}（セル {}x{} px）", grid.0, grid.1, cell.0, cell.1));
            }
            self.shared.update(|s| {
                s.cols = grid.0;
                s.rows = grid.1;
                s.cell = cell;
            });
        }
    }

    /// 端末から写して、変わっていれば描く。`force` なら変わっていなくても描く
    fn refresh(&mut self, force: bool) {
        self.check_view();
        self.sync_size();
        let Some(session) = self.session.clone() else { return };
        session.take_output();
        {
            let term = session.lock();
            if let Err(e) = self.rs.update(&term) {
                log(&format!("render state を更新できません: {e}"));
                return;
            }
        }
        let changed = self.frame.update(&mut self.rs);
        self.publish_cursor();
        let visible = self.view.as_ref().is_some_and(|v| unsafe { IsWindowVisible(v.hwnd) }.as_bool());
        if (changed || force) && visible {
            self.draw(None);
        }
    }

    fn draw(&mut self, capture: Option<&std::path::Path>) -> bool {
        let Some(v) = &mut self.view else { return false };
        match v.renderer.draw(&self.frame, self.focused, capture) {
            Ok(()) => true,
            Err(e) => {
                log(&format!("描画に失敗（デバイスを作り直した）: {e}"));
                false
            }
        }
    }

    fn publish_cursor(&self) {
        let rect = self.view.as_ref().and_then(|v| {
            let c = &self.frame.cursor;
            let (x, y) = c.pos?;
            let pad = v.renderer.padding() as i32;
            let (cw, ch) = v.renderer.cell_size();
            Some((pad + x as i32 * cw as i32, pad + y as i32 * ch as i32, cw as i32, ch as i32))
        });
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        s.cursor = rect;
    }

    /// 端末から知らせが来た（タイトル・カレントディレクトリ・子の終了）
    fn session_changed(&mut self) {
        let Some(s) = self.session.clone() else { return };
        let (title, pwd, exit) = (s.title(), s.pwd(), s.exit_status());
        let mut exited_now = None;
        self.shared.update(|st| {
            st.title = title;
            st.pwd = pwd;
            if let Some(e) = exit
                && !st.exited
            {
                st.exited = true;
                st.exit_code = e.code;
                exited_now = Some(e.code);
            }
        });
        if let Some(code) = exited_now {
            log(&format!("子プロセスが終わった: {code:?}"));
            self.shared.emit(Event::Exited(code));
            self.refresh(false);
        }
    }

    /// 入力の前に、スクロールバックを遡っていたらいちばん下へ戻す
    fn snap_to_bottom(&self, s: &Session) {
        let mut t = s.lock();
        if !t.viewport_at_bottom() {
            t.scroll_viewport(Scroll::Bottom);
        }
    }

    fn send(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let Some(s) = self.ensure_session() else { return };
        self.snap_to_bottom(&s);
        s.write(data);
    }

    fn send_key(&mut self, key: &str, mods: &str) {
        let mut m = keys::parse_mods(mods);
        let mut chars = key.chars();
        let bytes = match (chars.next(), chars.next()) {
            (Some(c), None) => keys::encode_char(c, m),
            _ => match keys::key_by_name(key) {
                Some((k, shift)) => {
                    let Some(s) = self.ensure_session() else { return };
                    m.shift |= shift;
                    let t = s.lock();
                    self.encoder.encode(&t, k, m)
                }
                None => {
                    log(&format!("知らないキー: {key} ({mods})"));
                    return;
                }
            },
        };
        self.send(&bytes);
    }

    fn screenshot(&mut self, path: PathBuf) {
        if self.view.is_none() {
            self.shared.emit(Event::Error("画面を保存できません（子 HWND がありません）".into()));
            return;
        }
        self.refresh(false);
        if self.draw(Some(&path)) {
            log(&format!("画面を保存: {}", path.display()));
            self.shared.emit(Event::Screenshot(path.display().to_string()));
        } else {
            self.shared.emit(Event::Error(format!("画面を保存できません: {}", path.display())));
        }
    }

    /// 閉じる。子 HWND を壊し、端末の後始末は待たずに別のスレッドへ任せ（T-076）、UI スレッドを終える
    fn close(&mut self) {
        if let Some(v) = self.view.take() {
            unsafe {
                let _ = DestroyWindow(v.hwnd);
            }
        }
        self.pending = None;
        if let Some(s) = &self.session {
            let _ = s.close();
        } else {
            self.shared.update(|st| st.closed = true);
        }
        self.shared.update(|s| {
            s.view = false;
            s.hwnd = 0;
        });
        unsafe {
            let _ = DestroyWindow(self.control);
        }
    }

    fn mouse_cell(&self, lparam: LPARAM) -> Option<(u16, u16)> {
        let v = self.view.as_ref()?;
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        Some(v.renderer.cell_at(x, y, self.grid.0, self.grid.1))
    }

    fn set_selection(&mut self, a: (u16, u16), b: (u16, u16)) {
        let (s, e) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
        if let Some(sess) = &self.session {
            let _ = sess.lock().set_selection(Some((s, e)));
        }
        self.refresh(false);
    }

    fn wheel(&mut self, delta: i32) {
        self.wheel += delta;
        let notches = self.wheel / WHEEL_DELTA as i32;
        if notches != 0 {
            self.wheel -= notches * WHEEL_DELTA as i32;
            if let Some(s) = &self.session {
                s.lock().scroll_viewport(Scroll::Delta(-(notches as isize) * 3));
            }
            self.refresh(false);
        }
    }

    /// 子 HWND のメッセージ。`None` なら DefWindowProc に任せる
    fn view_message(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if self.view.as_ref().map(|v| v.hwnd) != Some(hwnd) {
            return None;
        }
        match msg {
            WM_PAINT => unsafe {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                self.refresh(true);
            },
            WM_SIZE => self.refresh(true),
            WM_ERASEBKGND => return Some(LRESULT(1)),
            WM_LBUTTONDOWN => {
                // クリックしたら Emacs にそのウィンドウを選ばせる（子 HWND はフォーカスを取らない）
                self.shared.emit(Event::Clicked);
                if let Some(c) = self.mouse_cell(lparam) {
                    self.drag_anchor = Some(c);
                    unsafe { SetCapture(hwnd) };
                    if let Some(s) = &self.session {
                        let _ = s.lock().set_selection(None);
                    }
                    self.refresh(false);
                }
            }
            WM_RBUTTONDOWN | WM_MBUTTONDOWN => self.shared.emit(Event::Clicked),
            WM_MOUSEMOVE => {
                if let (Some(a), Some(c)) = (self.drag_anchor, self.mouse_cell(lparam))
                    && c != a
                {
                    self.set_selection(a, c);
                }
            }
            WM_LBUTTONUP => unsafe {
                self.drag_anchor = None;
                let _ = ReleaseCapture();
            },
            WM_MOUSEWHEEL => self.wheel(((wparam.0 >> 16) & 0xffff) as i16 as i32),
            _ => return None,
        }
        Some(LRESULT(0))
    }
}

extern "system" fn control_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let r = catch_unwind(AssertUnwindSafe(|| match msg {
        WM_APP_REQUEST => with_app(|a| a.drain_requests()).map(|_| LRESULT(0)),
        WM_APP_OUTPUT => with_app(|a| a.refresh(false)).map(|_| LRESULT(0)),
        WM_APP_SESSION => with_app(|a| a.session_changed()).map(|_| LRESULT(0)),
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        _ => None,
    }));
    match r {
        Ok(Some(l)) => l,
        Ok(None) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        Err(_) => {
            log(&format!("制御用のウィンドウで panic（メッセージ 0x{msg:x}）"));
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

extern "system" fn view_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // クリックされてもフォーカスを取らない（キーは Emacs が受ける）。App を見る前に返す
    if msg == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    let r = catch_unwind(AssertUnwindSafe(|| {
        if msg == WM_NCDESTROY {
            // 親のフレームごと壊された（または自分で壊した）。借りられなければ控えておき、次に App を使うときに捨てる
            if with_app(|a| {
                if a.view.as_ref().map(|v| v.hwnd) == Some(hwnd) {
                    a.view_destroyed();
                }
            })
            .is_none()
            {
                DESTROYED_VIEW.with(|d| d.set(hwnd.0 as isize));
            }
            return None;
        }
        with_app(|a| a.view_message(hwnd, msg, wparam, lparam)).flatten()
    }));
    match r {
        Ok(Some(l)) => l,
        Ok(None) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        Err(_) => {
            log(&format!("子 HWND のウィンドウプロシージャで panic（メッセージ 0x{msg:x}）"));
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// 端末の画面（ビューポート）のテキスト。**Lisp のスレッドで端末をロックする**（読みのスレッドが
/// 書いている最中なら、その分だけ待つ。デバッグ・自動確認用）
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
