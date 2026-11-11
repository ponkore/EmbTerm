//! 端末 1 つ分の UI スレッドと、Emacs のウィンドウごとに重ねる子 HWND（T-028 / T-063）。
//!
//! ```text
//! Lisp のスレッド                     端末の UI スレッド（1 端末に 1 本）                 et-session のスレッド
//! ─────────────                     ─────────────────────────────                    ─────────────────────
//! embterm-module-place など           制御用のウィンドウ（HWND_MESSAGE、端末が生きている間）   PTY の読み → vt_write
//!   Shared.requests に積む              WM_APP_REQUEST: 列を全部取り出して片付ける             └ PostMessage(制御, WM_APP_OUTPUT)
//!   PostMessage(制御, WM_APP_REQUEST) ▶  WM_APP_OUTPUT: 写して描く                           子の終了など → PostMessage(制御, WM_APP_SESSION)
//! embterm-module-poll ◀────────────── Shared.state / events を書く                        閉じる（session-close）→ Closed
//!                                     ペイン（Emacs のウィンドウごとに子 HWND を 1 枚、T-063）
//! ```
//!
//! - **Lisp → 端末は待たない**（列に積んで HWND 宛ての `PostMessage`。モーダルループ中も取りこぼさない）
//! - **端末 → Lisp は共有の状態に書くだけ**（Lisp がタイマーでポーリングする。T-014 の初版）
//! - 端末 1 つにペイン 0〜N 枚（T-063 候補 B）。端末の状態（[`Session`]）は共有し、ペインごとにスクロール位置を持つ。
//!   libghostty-vt のビューポートは 1 つなので、描くときはロックの中で「そのペインの位置へ動かす → そのペインの render state に写す →
//!   いちばん下へ戻す」とする（[`App::refresh`]）。いちばん下を追っているペインは、まとめて 1 枚の [`Frame`] を見る（余計な負荷をかけない）
//! - 端末の大きさ（桁・行）は ConPTY に 1 つだけで、選んでいるペインに合わせる。ほかのペインは自分の大きさで切り取って描く
//! - 子 HWND は親（フレーム）が消されると一緒に消える。**端末と UI スレッドは残り**、次の Place で作り直す（T-066）。
//!   制御用のウィンドウはメッセージ専用なので親に巻き込まれない
//! - 子 HWND は作ったら `HWND_BOTTOM`、動かすときは `SWP_NOZORDER`。`WS_EX_NOACTIVATE` と `MA_NOACTIVATE` で
//!   フォーカスを取らない（キーは Emacs が受けて module に送る。T-009 の案 A）
//! - UI スレッドの wndproc とスレッドの本体は `catch_unwind` で包む（T-031）
//! - スクロール位置・選択は [`et_vt::TrackedRef`] / libghostty の選択で持つので、出力が増えても同じ行を指す。
//!   これらはすべて端末のロックの中で読み書きする

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use et_render::{FontSpec, Frame, FrameView, Gpu, NarrowWide, Renderer};
use et_session::keys::{KeyEvent, KeyPhase};
use et_session::{Event as SessionEvent, Session, SessionOptions, keys};
use et_vt::{Geometry, GridRef, Key, KeyEncoder, Mode, Mods, MouseAction, MouseButton, MouseEncoder, MouseGeometry, RenderState, Rgb, Scroll, Terminal, TrackedRef};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT, ScreenToClient};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, VK_MENU, VK_SHIFT};
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
/// スクロールバックの既定の行数
const DEFAULT_SCROLLBACK: usize = 10_000;
/// ホイール 1 ノッチで動かす行数
const WHEEL_LINES: i64 = 3;
/// 同期出力（DEC 2026）の hold の時限（アプリが落ちて `?2026l` が来ないときに表示を止めっぱなしにしない。T-074）
const RENDER_HOLD_TIMEOUT: Duration = Duration::from_secs(1);
/// 制御用のウィンドウの hold の時限のタイマー
const TIMER_RENDER_HOLD: usize = 1;
// マウスのメッセージの wParam の修飾キー（Win32_System_SystemServices の MK_* と同じ値）
const MK_LBUTTON: u32 = 0x0001;
const MK_RBUTTON: u32 = 0x0002;
const MK_SHIFT: u32 = 0x0004;
const MK_CONTROL: u32 = 0x0008;
const MK_MBUTTON: u32 = 0x0010;

/// ペインのスクロールのしかた
#[derive(Clone, Copy, Debug)]
pub enum ScrollAmount {
    /// 行数（負は上）
    Lines(i64),
    /// ページ（ペインの行数 - 1 を 1 ページ。負は上）
    Pages(i64),
    /// スクロールバックの先頭へ
    Top,
    /// いちばん下へ（追いかける状態に戻る）
    Bottom,
    /// ホイールのノッチ（負は上。子 HWND が受けたときと同じ扱い。マウスの報告が有効ならアプリへ送る）
    Wheel(i32),
}

/// Lisp からの頼みごと
pub enum Request {
    /// ペインを親（フレームの HWND）のクライアント座標（物理ピクセル）に置いて見せる。無ければ作る
    Place { pane: i64, parent: isize, x: i32, y: i32, w: i32, h: i32 },
    /// ペインを隠す（`None` なら全部）。スクロール位置は残す
    Hide(Option<i64>),
    /// ペインを消す（Emacs のウィンドウが無くなった）
    Remove(i64),
    /// 選んでいるペイン（`None` は端末のウィンドウを選んでいない）。端末の大きさはこのペインに合わせる
    Select(Option<i64>),
    /// 文字列をそのまま送る（IME で確定した文字も）
    SendString(String),
    /// キーを 1 つ送る。`key` は 1 文字か Emacs のファンクションキーの名前、`mods` は `C` `M` `S` の組
    SendKey { key: String, mods: String },
    /// 貼り付け（改行を CR にし、ブラケットペーストのモードなら包む）
    Paste(String),
    /// ペインをスクロールする
    Scroll { pane: i64, amount: ScrollAmount },
    /// 選択を消す
    ClearSelection,
    /// スクロールバックの行数の上限
    SetScrollback(usize),
    SetFont { family: Option<String>, size: Option<f32> },
    SetNarrowWide(NarrowWide),
    /// フォーカスの出入り（端末へ `CSI I` / `CSI O` を送り、カーソルの描き方を変える）
    Focus(bool),
    /// copy モードの準備（スクロールバックごとのテキストを作る。できたら [`Event::CopyReady`]）。ペインのスクロール位置に合わせる
    Copy(Option<i64>),
    /// ペインの今の画面を PNG に保存する（`None` は選んでいるペイン。デバッグ用）
    Screenshot { pane: Option<i64>, path: PathBuf },
    /// ペインの見えている行のテキスト（デバッグ用）
    PaneText { pane: i64, reply: mpsc::Sender<Option<String>> },
    Close,
}

/// Lisp に渡す知らせ
#[derive(Clone, Debug)]
pub enum Event {
    /// ペインがクリックされた（Lisp はそのウィンドウを選ぶ）
    Clicked(i64),
    Bell,
    /// 子プロセスが終わった（終了コード）
    Exited(Option<u32>),
    /// 画面を保存した（パス）
    Screenshot(String),
    /// 選択を終えた（選択のテキスト。`embterm-copy-on-select' が有効なら Lisp が kill-ring へ）
    Selected(String),
    /// copy モードのテキストができた（`embterm-module-take-copy' で受け取る）
    CopyReady,
    Error(String),
}

/// Lisp に見せるペインの情報
#[derive(Clone, Default)]
pub struct PaneInfo {
    pub id: i64,
    pub hwnd: isize,
    pub cols: u16,
    pub rows: u16,
    pub scrolled: bool,
}

/// Lisp がポーリングで写して読む状態
#[derive(Clone, Default)]
pub struct State {
    /// 何か変わるたびに増える（カーソルの位置は除く）。Lisp は前の値と比べて読み直す
    pub generation: u64,
    /// UI スレッドが生きているか
    pub alive: bool,
    /// 子 HWND が 1 枚でもあるか（フレームを消すと一緒に消える）
    pub view: bool,
    /// 選んでいるペインの子 HWND の値（デバッグ・自動確認用。無ければ最初のペイン、それも無ければ 0）
    pub hwnd: isize,
    /// ペインの一覧
    pub panes: Vec<PaneInfo>,
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
    /// カーソルの矩形（選んでいるペインの子 HWND のクライアント座標の画素。x y w h）。見えなければ `None`。
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
    pub scrollback_lines: Option<usize>,
}

/// copy モード用のテキスト（`Event::CopyReady` のあと Lisp が受け取る）
pub struct CopyText {
    pub text: String,
    /// 見ているペインの先頭の行（テキストの行番号。0 から）
    pub top: i64,
    /// ポイントを置く行（スクロールしていなければカーソルの行、遡っていればペインの先頭の行）
    pub point: i64,
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
    /// 選択のテキスト（選択を終えたときに UI スレッドが作って置く。Lisp はロックせずに読める）
    selection: Mutex<Option<String>>,
    /// copy モードのテキスト
    copy: Mutex<Option<CopyText>>,
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

    /// 選択を終えたときのテキスト（無ければ `None`）。端末をロックしない
    pub fn selection_text(&self) -> Option<String> {
        self.selection.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_selection_text(&self, text: Option<String>) {
        *self.selection.lock().unwrap_or_else(|e| e.into_inner()) = text;
    }

    pub fn take_copy(&self) -> Option<CopyText> {
        self.copy.lock().unwrap_or_else(|e| e.into_inner()).take()
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
    // 端末のスレッドが libghostty-vt を使い始める前に登録する（T-088）
    et_vt::install_png_decoder();
    let shared = Arc::new(Shared {
        state: Mutex::new(State::default()),
        events: Mutex::new(Vec::new()),
        requests: Mutex::new(Vec::new()),
        request_posted: AtomicBool::new(false),
        control: AtomicIsize::new(0),
        session: OnceLock::new(),
        selection: Mutex::new(None),
        copy: Mutex::new(None),
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
            // CS_DBLCLKS は付けない（2 回目も WM_LBUTTONDOWN で受け、クリックの回数は libghostty の選択の解釈に任せる）
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
        if let Some(mut app) = APP.with(|a| a.borrow_mut().take()) {
            if let Some(s) = app.session.clone() {
                let _ = s.close();
                // ペインの TrackedRef は端末のロックを持って落とす
                let term = s.lock();
                app.panes.clear();
                drop(term);
            }
            drop(app);
        }
    }));
    shared.update(|s| {
        s.alive = false;
        s.view = false;
        s.hwnd = 0;
        s.panes.clear();
    });
}

/// 表示用の子 HWND と、それに描くもの
struct View {
    hwnd: HWND,
    parent: HWND,
    renderer: Renderer,
}

/// ペインのスクロール位置
enum Position {
    /// いちばん下を追う（ふつうの状態）。描くのは端末全体で 1 枚の [`Frame`]（[`App::master`]）
    Follow,
    /// スクロールバックを遡っている。ペインの先頭の行を追いかけて指す（出力が増えても同じ行を指す。
    /// スクロールバックの上限で行が消えると値を失い、そのときはいちばん上へ寄せる）
    Anchored(TrackedRef),
}

/// スクロールバックを遡っているペインの描く中身。出力に合わせて写し直す必要があるときだけ作り直す
struct ScrolledFrame {
    frame: Frame,
    /// ペインの先頭の行が `frame` の何行目か
    top: i32,
}

/// Emacs のウィンドウ 1 つに対応する描画の単位（T-063）
struct Pane {
    id: i64,
    view: Option<View>,
    /// 見せているか（Hide で false）
    shown: bool,
    position: Position,
    scrolled: Option<ScrolledFrame>,
    /// 桁数と行数（このペインの大きさ。端末の大きさとは違うことがある）
    grid: (u16, u16),
    wheel: i32,
    /// スクロール位置・選択などが変わったので、写し直す必要がある（スクロールバックを遡っているペイン）
    needs_sync: bool,
    /// 描き直す必要がある
    needs_draw: bool,
}

impl Pane {
    /// 端末の下端を追っている（代替画面にいる間はスクロールバックが無いので、遡っていたペインも追う扱いにする）
    fn follows(&self, alt: bool) -> bool {
        alt || matches!(self.position, Position::Follow)
    }

    fn visible(&self) -> bool {
        self.shown && self.view.is_some()
    }

    fn scrolled_position(&self) -> bool {
        matches!(self.position, Position::Anchored(_))
    }

    /// ペインの先頭の行（画面の座標）。上に足りないときは負（`Follow` で端末より行が多いとき）
    fn top_row(&self, term: &Terminal, alt: bool) -> i64 {
        match &self.position {
            Position::Anchored(a) if !alt => a.screen_row(term).map_or(0, |y| y as i64),
            _ => term.total_rows() as i64 - self.grid.1 as i64,
        }
    }
}

/// マウスのドラッグの状態
#[derive(Clone, Copy, PartialEq)]
enum Drag {
    /// 選択（ペインの番号）
    Selecting(i64),
    /// アプリへ報告している（ペインの番号と押したボタン）
    Reporting(i64, MouseButton),
}

/// UI スレッドの状態（スレッドローカル）
struct App {
    control: HWND,
    shared: Arc<Shared>,
    /// 起動の指定（最初の Place で子プロセスを起動するときに取り出す）
    pending: Option<SpawnOptions>,
    session: Option<Arc<Session>>,
    panes: Vec<Pane>,
    /// 端末の大きさを合わせるペイン（選んでいるペイン。端末のウィンドウを選んでいないときは最後に選んだペイン）
    active: Option<i64>,
    /// 端末のウィンドウを選んでいるか（カーソルの塗りつぶしと IME の位置合わせ）
    selected: Option<i64>,
    /// GPU デバイス（端末につき 1 つ。ペインで分け合う。T-067 の第一歩）
    gpu: Option<Gpu>,
    font: FontSpec,
    narrow_wide: NarrowWide,
    /// いちばん下を追っているペインが見る render state（端末全体で 1 つ）。overscan でスクロールバックを補う
    main_rs: RenderState,
    main_overscan: u16,
    /// `main_rs` から写したフレーム（端末の行 + overscan の上の行）。追っているペインは同じものを切り取って描く
    master: Frame,
    encoder: KeyEncoder,
    mouse: MouseEncoder,
    /// 端末の桁数と行数（ConPTY に伝えた大きさ）
    grid: (u16, u16),
    cell: (u32, u32),
    focused: bool,
    /// 前の refresh のあとに出力が来た
    output_pending: bool,
    /// 代替画面にいるか（refresh のロックの中で読む）
    alt_screen: bool,
    drag: Option<Drag>,
    /// 最初のクリックからの単調な時刻の基準（libghostty の選択の解釈に渡す）
    epoch: Instant,
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
}

/// lparam の x, y（符号付き 16 ビット）。クライアント座標
fn point_of(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xffff) as i16 as i32, ((lparam.0 >> 16) & 0xffff) as i16 as i32)
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
            panes: Vec::new(),
            active: None,
            selected: None,
            gpu: None,
            font,
            main_rs: RenderState::new().map_err(|e| e.to_string())?,
            main_overscan: 0,
            master: Frame::default(),
            encoder: KeyEncoder::new().map_err(|e| e.to_string())?,
            mouse: MouseEncoder::new().map_err(|e| e.to_string())?,
            grid: (0, 0),
            cell: FALLBACK_CELL,
            focused: false,
            output_pending: false,
            alt_screen: false,
            drag: None,
            epoch: Instant::now(),
        })
    }

    fn pane_mut(&mut self, id: i64) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    fn pane_index_by_hwnd(&self, hwnd: HWND) -> Option<usize> {
        self.panes.iter().position(|p| p.view.as_ref().is_some_and(|v| v.hwnd == hwnd))
    }

    /// 子 HWND がまだ生きているか確かめる（親のフレームごと消されていれば捨てる）
    fn check_views(&mut self) {
        let dead = DESTROYED_VIEW.with(|d| d.replace(0));
        let gone: Vec<i64> = self
            .panes
            .iter()
            .filter(|p| p.view.as_ref().is_some_and(|v| v.hwnd.0 as isize == dead || !unsafe { IsWindow(Some(v.hwnd)) }.as_bool()))
            .map(|p| p.id)
            .collect();
        for id in gone {
            self.view_destroyed(id);
        }
    }

    fn view_destroyed(&mut self, id: i64) {
        if let Some(p) = self.pane_mut(id)
            && p.view.take().is_some()
        {
            log(&format!("ペイン {id} の子 HWND が壊れた（親のフレームが消された）。端末は残す"));
            if matches!(self.drag, Some(Drag::Selecting(p) | Drag::Reporting(p, _)) if p == id) {
                self.drag = None;
            }
            self.publish_panes();
        }
    }

    /// Lisp に見せるペインの情報と、子 HWND の有無を書く
    fn publish_panes(&self) {
        let panes: Vec<PaneInfo> = self
            .panes
            .iter()
            .map(|p| PaneInfo {
                id: p.id,
                hwnd: p.view.as_ref().map_or(0, |v| v.hwnd.0 as isize),
                cols: p.grid.0,
                rows: p.grid.1,
                scrolled: p.scrolled_position(),
            })
            .collect();
        let view = panes.iter().any(|p| p.hwnd != 0);
        let hwnd = self
            .active
            .and_then(|a| panes.iter().find(|p| p.id == a))
            .or(panes.first())
            .map_or(0, |p| p.hwnd);
        self.shared.update(|s| {
            s.panes = panes;
            s.view = view;
            s.hwnd = hwnd;
            if !view {
                s.cursor = None;
            }
        });
    }

    fn drain_requests(&mut self) {
        self.shared.request_posted.store(false, Ordering::Release);
        let requests = std::mem::take(&mut *self.shared.requests.lock().unwrap_or_else(|e| e.into_inner()));
        for r in requests {
            self.apply(r);
        }
    }

    fn apply(&mut self, r: Request) {
        self.check_views();
        match r {
            Request::Place { pane, parent, x, y, w, h } => self.place(pane, HWND(parent as *mut _), x, y, w.max(1), h.max(1)),
            Request::Hide(pane) => {
                for p in &mut self.panes {
                    if pane.is_none_or(|id| id == p.id) {
                        p.shown = false;
                        if let Some(v) = &p.view {
                            unsafe {
                                let _ = ShowWindow(v.hwnd, SW_HIDE);
                            }
                        }
                    }
                }
            }
            Request::Remove(pane) => self.remove_pane(pane),
            Request::Select(pane) => self.select_pane(pane),
            Request::SendString(text) => self.send_input(text.as_bytes()),
            Request::SendKey { key, mods } => self.send_key(&key, &mods),
            Request::Paste(text) => {
                let Some(s) = self.ensure_session() else { return };
                let bracketed = s.lock().mode(Mode::BRACKETED_PASTE);
                self.send_input(&keys::paste_bytes(&text, bracketed));
            }
            Request::Scroll { pane, amount } => self.scroll_pane(pane, amount),
            Request::ClearSelection => self.clear_selection(),
            Request::SetScrollback(lines) => {
                if let Some(s) = &self.session
                    && let Err(e) = s.lock().set_scrollback_max_lines(lines)
                {
                    log(&format!("スクロールバックの行数を変えられません: {e}"));
                }
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
                let mut family = None;
                for p in &mut self.panes {
                    let Some(v) = &mut p.view else { continue };
                    if let Err(e) = v.renderer.set_font(spec.clone()) {
                        self.shared.emit(Event::Error(format!("フォントを作れません: {e}")));
                    }
                    family = Some(v.renderer.font_family().to_string());
                    p.needs_draw = true;
                }
                if let Some(family) = family {
                    self.shared.update(|s| s.font = family);
                }
                self.refresh(true);
            }
            Request::SetNarrowWide(n) => {
                self.narrow_wide = n;
                for p in &mut self.panes {
                    if let Some(v) = &mut p.view {
                        v.renderer.theme.narrow_wide = n;
                    }
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
            Request::Copy(pane) => self.prepare_copy(pane),
            Request::Screenshot { pane, path } => self.screenshot(pane, path),
            Request::PaneText { pane, reply } => {
                let text = self.pane_text(pane);
                let _ = reply.send(text);
            }
            Request::Close => self.close(),
        }
    }

    fn select_pane(&mut self, pane: Option<i64>) {
        self.selected = pane;
        if pane.is_some() {
            self.active = pane;
        }
        self.publish_panes();
        self.refresh(true);
    }

    fn remove_pane(&mut self, id: i64) {
        let Some(i) = self.panes.iter().position(|p| p.id == id) else { return };
        // TrackedRef は端末のロックを持って落とす
        let pane = {
            let term = self.session.as_ref().map(|s| s.lock());
            let p = self.panes.remove(i);
            drop(term);
            p
        };
        if let Some(v) = &pane.view
            && unsafe { IsWindow(Some(v.hwnd)) }.as_bool()
        {
            unsafe {
                let _ = DestroyWindow(v.hwnd);
            }
        }
        if self.active == Some(id) {
            self.active = self.panes.first().map(|p| p.id);
        }
        if self.selected == Some(id) {
            self.selected = None;
        }
        if matches!(self.drag, Some(Drag::Selecting(p) | Drag::Reporting(p, _)) if p == id) {
            self.drag = None;
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        self.publish_panes();
        self.refresh(false);
    }

    /// 子プロセスを（まだなら）起動する。桁数・行数は今の端末の大きさから求める（無ければ 80x24）
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
                scrollback_lines: opts.scrollback_lines.unwrap_or(DEFAULT_SCROLLBACK),
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
                // すでにフォーカスがあれば知らせる（ConPTY は 1004 を必ず求める。T-070）
                if self.focused {
                    s.write(b"\x1b[I");
                }
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
        // GPU デバイスは端末につき 1 つ。最初のペインのときだけ作る（T-067 の第一歩）
        if self.gpu.is_none() {
            match Gpu::create() {
                Ok(g) => self.gpu = Some(g),
                Err(e) => {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                    return Err(format!("GPU デバイスを作れません: {e}"));
                }
            }
        }
        let gpu = self.gpu.clone().expect("直前に作った");
        match Renderer::with_gpu(gpu, hwnd, cw.max(1), ch.max(1), dpi, self.font.clone()) {
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

    fn place(&mut self, id: i64, parent: HWND, x: i32, y: i32, w: i32, h: i32) {
        if !unsafe { IsWindow(Some(parent)) }.as_bool() {
            self.shared.emit(Event::Error(format!("親のウィンドウがありません: 0x{:x}", parent.0 as isize)));
            return;
        }
        if self.pane_mut(id).is_none() {
            self.panes.push(Pane {
                id,
                view: None,
                shown: false,
                position: Position::Follow,
                scrolled: None,
                grid: (1, 1),
                wheel: 0,
                needs_sync: false,
                needs_draw: true,
            });
            if self.active.is_none() {
                self.active = Some(id);
            }
        }
        // 子 HWND の用意
        let has_view = self.pane_mut(id).is_some_and(|p| p.view.is_some());
        if !has_view {
            match self.create_view(parent, x, y, w, h) {
                Ok(v) => {
                    let family = v.renderer.font_family().to_string();
                    let cell = v.renderer.cell_size();
                    if let Some(p) = self.pane_mut(id) {
                        p.view = Some(v);
                    }
                    self.cell = cell;
                    self.shared.update(|s| s.font = family);
                }
                Err(e) => {
                    log(&e);
                    self.shared.emit(Event::Error(e));
                    // 表示はできなくても、端末は動かしておく
                    self.ensure_session();
                    return;
                }
            }
        } else if let Some(p) = self.pane_mut(id)
            && let Some(v) = &mut p.view
            && v.parent != parent
        {
            unsafe {
                // 別のフレームへ付け替える。兄弟の一番下に置き直す
                let _ = SetParent(v.hwnd, Some(parent));
                let _ = SetWindowPos(v.hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                v.parent = parent;
                log(&format!("ペイン {id} の子 HWND を付け替えた: 親 0x{:x}", parent.0 as isize));
            }
        }
        if let Some(p) = self.pane_mut(id) {
            p.shown = true;
            p.needs_draw = true;
            if let Some(v) = &p.view {
                unsafe {
                    // SetWindowPos は WM_SIZE を同期で送る（App を借りているので届かない）。大きさは下の refresh の sync_sizes で拾う
                    let _ = SetWindowPos(v.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW);
                }
            }
        }
        // 桁数・行数を決めてから起動する（起動直後のリサイズを避ける）
        self.sync_sizes();
        self.ensure_session();
        self.publish_panes();
        self.refresh(true);
    }

    /// 子 HWND の大きさとスワップチェーンの大きさ・ペインの桁数を合わせ（取りこぼした WM_SIZE も拾う）、
    /// 端末の大きさを選んでいるペインに合わせる（同じなら送らない）
    fn sync_sizes(&mut self) {
        let mut changed = false;
        for p in &mut self.panes {
            let Some(v) = &mut p.view else { continue };
            let (w, h) = client_size(v.hwnd);
            if w == 0 || h == 0 {
                continue;
            }
            if let Err(e) = v.renderer.resize(w, h) {
                log(&format!("スワップチェーンの大きさを変えられません: {e}"));
            }
            let grid = v.renderer.grid_size(w, h);
            if grid != p.grid {
                p.grid = grid;
                p.needs_draw = true;
                p.needs_sync = true;
                changed = true;
            }
        }
        // 端末の大きさは選んでいるペイン（無ければ最後に選んだペイン）に合わせる。
        // 隠れているペインは合わせる先にしない（copy モードの間に大きさが変わらないよう、見えているペインだけ）
        let target = self
            .active
            .and_then(|a| self.panes.iter().find(|p| p.id == a && p.visible()))
            .or_else(|| self.panes.iter().find(|p| p.visible()));
        let cell = target.and_then(|p| p.view.as_ref()).map(|v| v.renderer.cell_size());
        let target_grid = target.map(|p| p.grid);
        if let (Some(grid), Some(cell)) = (target_grid, cell)
            && (grid != self.grid || cell != self.cell)
        {
            self.grid = grid;
            self.cell = cell;
            if let Some(s) = &self.session {
                s.resize(grid.0, grid.1, cell);
                log(&format!("リサイズ: {}x{}（セル {}x{} px）", grid.0, grid.1, cell.0, cell.1));
            }
            for p in &mut self.panes {
                p.needs_sync = true;
                p.needs_draw = true;
            }
            self.shared.update(|s| {
                s.cols = grid.0;
                s.rows = grid.1;
                s.cell = cell;
            });
        }
        if changed {
            self.publish_panes();
        }
    }

    /// 端末から写して、変わったペインを描く。`force` なら全部描く
    ///
    /// ロックを持っている間に、いちばん下を追うペイン用の render state（1 つ）を更新し、
    /// スクロールバックを遡っているペインは必要なときだけ自分の位置へビューポートを動かして写す。
    fn refresh(&mut self, force: bool) {
        self.check_views();
        self.sync_sizes();
        let Some(session) = self.session.clone() else { return };
        session.take_output();
        let output = std::mem::take(&mut self.output_pending);
        // （ペインの番号, そのペインに写した render state, ペインの先頭が `Frame` の何行目か）
        let mut synced: Vec<(usize, RenderState, i32)> = Vec::new();
        let hold;
        {
            let mut term = session.lock();
            // 同期出力（T-074）: hold の間はどのペインも写し直さない。hold の始まりの画面は読みのスレッドが写してある
            hold = term.render_hold(RENDER_HOLD_TIMEOUT);
            if let Some(rs) = term.take_held_render_state() {
                self.main_rs = rs;
            }
            let (_, rows) = term.size();
            let alt = term.is_alternate_screen();
            self.alt_screen = alt;
            // 追っているペインのうち、端末より行が多いものの分だけ上の行を余分に写す
            let need_above =
                self.panes.iter().filter(|p| p.visible() && p.follows(alt)).map(|p| p.grid.1.saturating_sub(rows)).max().unwrap_or(0);
            if hold.is_none() && need_above != self.main_overscan {
                self.main_overscan = need_above;
                if let Err(e) = self.main_rs.set_overscan(need_above, 0) {
                    log(&format!("overscan を設定できません: {e}"));
                }
                term.set_hold_overscan(need_above, 0);
            }
            if hold.is_some() {
                // 出力があったことは hold が終わってから反映する
                self.output_pending |= output;
            } else if let Err(e) = self.main_rs.update(&term) {
                log(&format!("render state を更新できません: {e}"));
                return;
            }
            // 画像の画素は render state に無いので、端末のロックの中で写す（T-088）
            self.master.capture_images(&mut self.main_rs, &term);
            let total = term.total_rows() as i64;
            let active_top = total - rows as i64;
            let mut moved = false;
            for (i, p) in self.panes.iter_mut().enumerate() {
                if !p.visible() || alt || hold.is_some() {
                    continue;
                }
                let Position::Anchored(anchor) = &p.position else { continue };
                let pr = p.grid.1 as i64;
                let t = match anchor.screen_row(&term) {
                    Some(t) => t as i64,
                    None => {
                        // 指していた行がスクロールバックの上限で消えた。いちばん古い行へ寄せる
                        match term.track(0, 0) {
                            Ok(a) => p.position = Position::Anchored(a),
                            Err(e) => {
                                log(&format!("スクロール位置を作れません: {e}"));
                                p.position = Position::Follow;
                                continue;
                            }
                        }
                        p.needs_sync = true;
                        0
                    }
                };
                // ペインに見える範囲がアクティブ領域にかかるときだけ、出力で中身が変わる
                let overlaps_active = t + pr > active_top;
                if !(p.needs_sync || force || (output && overlaps_active)) {
                    continue;
                }
                p.needs_sync = false;
                let v = t.min(active_top.max(0)).max(0);
                term.scroll_viewport(Scroll::Row(v as u64));
                moved = true;
                // ペインの行が端末より多ければ、下の行を余分に写す
                let below = (t + pr - (v + rows as i64)).max(0).min(u16::MAX as i64) as u16;
                // render state は端末の dirty を読むと落としてしまう（先に main_rs が受け取っている）ので、
                // 遡っているペインは毎回作り直して全部の行を写す（作るのは必要なときだけ）
                let mut rs = match RenderState::new() {
                    Ok(rs) => rs,
                    Err(e) => {
                        log(&format!("render state を作れません: {e}"));
                        continue;
                    }
                };
                if below > 0 && rs.set_overscan(0, below).is_err() {
                    continue;
                }
                if let Err(e) = rs.update(&term) {
                    log(&format!("render state を更新できません: {e}"));
                    continue;
                }
                // 遡っているペインの画像も、位置を動かしたこのときに写す（T-088）
                p.scrolled.get_or_insert_with(|| ScrolledFrame { frame: Frame::default(), top: 0 }).frame.capture_images(&mut rs, &term);
                synced.push((i, rs, (t - v) as i32));
            }
            if moved {
                // ビューポートの決まりは「いちばん下」（読みのスレッドが動く前に戻す）
                term.scroll_viewport(Scroll::Bottom);
            }
        }
        // hold の時限が来たら描き直せるように、タイマーで起こす（出力が来なくなったアプリのとき）
        if let Some(rest) = hold {
            unsafe {
                SetTimer(Some(self.control), TIMER_RENDER_HOLD, rest.as_millis() as u32 + 2, None);
            }
        }
        let master_changed = self.master.update(&mut self.main_rs);
        for (i, mut rs, top) in synced {
            let p = &mut self.panes[i];
            let frame = p.scrolled.get_or_insert_with(|| ScrolledFrame { frame: Frame::default(), top: 0 });
            frame.frame.update(&mut rs);
            frame.top = top;
            p.needs_draw = true;
        }
        self.publish_cursor();
        for i in 0..self.panes.len() {
            let p = &self.panes[i];
            if !p.visible() {
                continue;
            }
            let follows = p.follows(self.alt_screen);
            if force || p.needs_draw || (follows && master_changed) {
                self.draw_pane(i, None);
            }
        }
    }

    /// ペインの今の中身を描く
    fn draw_pane(&mut self, i: usize, capture: Option<&std::path::Path>) -> bool {
        let alt = self.alt_screen;
        let focused = self.focused && self.selected == Some(self.panes[i].id);
        let mut lost_device = false;
        let result = {
            let pane = &mut self.panes[i];
            let follows = pane.follows(alt);
            let Some(v) = pane.view.as_mut() else { return false };
            if !unsafe { IsWindowVisible(v.hwnd) }.as_bool() && capture.is_none() {
                return false;
            }
            let (cols, rows) = pane.grid;
            let result = if follows {
                // 追っているペイン: 端末の下の端をペインの下の端に揃える（上に足りなければ空き）
                let view = FrameView { top: self.master.rows as i32 - rows as i32, cols, rows, show_cursor: true };
                v.renderer.draw_view(&self.master, &view, focused, capture)
            } else if let Some(s) = &pane.scrolled {
                let view = FrameView { top: s.top, cols, rows, show_cursor: false };
                v.renderer.draw_view(&s.frame, &view, focused, capture)
            } else {
                Ok(())
            };
            if result.is_ok() {
                pane.needs_draw = false;
            }
            match result {
                Ok(()) => true,
                Err(e) => {
                    log(&format!("描画に失敗（デバイスを作り直した）: {e}"));
                    lost_device = true;
                    false
                }
            }
        };
        if lost_device {
            self.rebuild_device();
        }
        result
    }

    /// デバイスが失われたら、端末の全ペインで新しいデバイスに載せ替える
    fn rebuild_device(&mut self) {
        match Gpu::create() {
            Ok(g) => {
                self.gpu = Some(g.clone());
                for p in &mut self.panes {
                    if let Some(v) = &mut p.view {
                        if let Err(e) = v.renderer.adopt_gpu(g.clone()) {
                            log(&format!("描画を作り直せません: {e}"));
                        }
                        p.needs_draw = true;
                    }
                }
            }
            Err(e) => log(&format!("GPU デバイスを作り直せません: {e}")),
        }
    }

    fn publish_cursor(&self) {
        // 選んでいるペインがいちばん下を追っているときだけ（IME の位置合わせ）
        let rect = self.selected.and_then(|id| self.panes.iter().find(|p| p.id == id)).and_then(|p| {
            if !p.visible() || p.scrolled_position() {
                return None;
            }
            let v = p.view.as_ref()?;
            let (x, y) = self.master.cursor.pos?;
            let fy = self.master.viewport_top as i32 + y as i32;
            let py = fy - (self.master.rows as i32 - p.grid.1 as i32);
            if !(0..p.grid.1 as i32).contains(&py) {
                return None;
            }
            let pad = v.renderer.padding() as i32;
            let (cw, ch) = v.renderer.cell_size();
            Some((pad + x as i32 * cw as i32, pad + py * ch as i32, cw as i32, ch as i32))
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

    /// 選択を消す
    fn clear_selection(&mut self) {
        if let Some(s) = &self.session {
            let _ = s.lock().apply_selection(None);
        }
        self.shared.set_selection_text(None);
        self.mark_selection_changed();
        self.refresh(false);
    }

    fn mark_selection_changed(&mut self) {
        for p in &mut self.panes {
            p.needs_sync = true;
            p.needs_draw = true;
        }
    }

    /// 入力の前に、選んでいるペインのスクロールを戻し、選択を消す
    fn prepare_for_input(&mut self, s: &Session) {
        let mut term = s.lock();
        let mut changed = term.has_selection();
        if changed {
            let _ = term.apply_selection(None);
        }
        if let Some(id) = self.active {
            for p in &mut self.panes {
                if p.id == id && p.scrolled_position() {
                    // TrackedRef はロックの中で落とす
                    p.position = Position::Follow;
                    p.needs_draw = true;
                    changed = true;
                }
            }
        }
        drop(term);
        if changed {
            self.shared.set_selection_text(None);
            self.mark_selection_changed();
            self.publish_panes();
        }
    }

    /// 端末へ入力を送る（選択を消し、選んでいるペインをいちばん下へ戻す）
    fn send_input(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let Some(s) = self.ensure_session() else { return };
        self.prepare_for_input(&s);
        s.write(data);
        self.refresh(false);
    }

    /// キーを送る。送り方（KKP / win32-input-mode / レガシー）は [`Session::key_protocol`] が決める（T-069）
    fn send_key(&mut self, key: &str, mods: &str) {
        let Some(ev) = KeyEvent::from_emacs(key, keys::parse_mods(mods)) else {
            log(&format!("知らないキー: {key} ({mods})"));
            return;
        };
        let Some(s) = self.ensure_session() else { return };
        let bytes = s.encode_key(&mut self.encoder, &ev, KeyPhase::Tap);
        self.send_input(&bytes);
    }

    /// ペインをスクロールする（位置は「先頭の行を指す追いかける位置」で持つ）
    fn scroll_pane(&mut self, id: i64, amount: ScrollAmount) {
        if let ScrollAmount::Wheel(n) = amount {
            // Emacs 側に来たホイール（ペインの中央に来たものとして扱う）
            let center = self
                .panes
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.view.as_ref())
                .map(|v| {
                    let (w, h) = client_size(v.hwnd);
                    (w as i32 / 2, h as i32 / 2)
                })
                .unwrap_or((0, 0));
            self.wheel_pane(id, n * WHEEL_DELTA as i32, center, Mods::default());
            return;
        }
        let Some(session) = self.session.clone() else { return };
        {
            let term = session.lock();
            let alt = term.is_alternate_screen();
            let Some(p) = self.panes.iter_mut().find(|p| p.id == id) else { return };
            if alt {
                return;
            }
            let total = term.total_rows() as i64;
            let pr = p.grid.1 as i64;
            let max_t = (total - pr).max(0);
            let cur = p.top_row(&term, alt).clamp(0, max_t);
            let target = match amount {
                ScrollAmount::Lines(n) => cur + n,
                ScrollAmount::Pages(n) => cur + n * (pr - 1).max(1),
                ScrollAmount::Top => 0,
                ScrollAmount::Bottom => max_t,
                ScrollAmount::Wheel(_) => cur,
            }
            .clamp(0, max_t);
            if target >= max_t {
                // TrackedRef はロックの中で落とす
                p.position = Position::Follow;
                p.scrolled = None;
            } else {
                match term.track(0, target as u32) {
                    Ok(a) => {
                        p.position = Position::Anchored(a);
                        p.needs_sync = true;
                    }
                    Err(e) => log(&format!("スクロール位置を作れません: {e}")),
                }
            }
            p.needs_draw = true;
            drop(term);
        }
        self.publish_panes();
        self.refresh(false);
    }

    /// `delta` は WHEEL_DELTA 単位（負は下へ＝新しいほうへではなく Windows の値のまま）。ノッチになったら、
    /// マウスの報告が有効なアプリならアプリへ送り、そうでなければペインをスクロールする
    fn wheel_pane(&mut self, id: i64, delta: i32, client: (i32, i32), mods: Mods) {
        let Some(p) = self.pane_mut(id) else { return };
        p.wheel += delta;
        let notches = p.wheel / WHEEL_DELTA as i32;
        if notches == 0 {
            return;
        }
        p.wheel -= notches * WHEEL_DELTA as i32;
        if self.should_report(id, mods.shift) {
            let button = if notches > 0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
            for _ in 0..notches.unsigned_abs() {
                self.send_mouse(id, MouseAction::Press, Some(button), false, client, mods);
            }
            return;
        }
        // 代替画面（報告なし）: Ghostty と同じく、DEC 1007（alternate scroll。既定で有効）なら矢印キーにする。
        // 代替画面にスクロールバックは無いので、Shift のときは何もしない
        if let Some(s) = self.session.clone() {
            let (alt, alt_scroll) = {
                let t = s.lock();
                (t.is_alternate_screen(), t.mode(Mode::MOUSE_ALTERNATE_SCROLL))
            };
            if alt {
                if alt_scroll && !mods.shift {
                    let key = if notches > 0 { Key::Up } else { Key::Down };
                    let mut bytes = Vec::new();
                    {
                        let t = s.lock();
                        for _ in 0..notches.unsigned_abs() as i64 * WHEEL_LINES {
                            bytes.extend(self.encoder.encode(&t, key, Mods::default()));
                        }
                    }
                    s.write(&bytes);
                }
                return;
            }
        }
        // WM_MOUSEWHEEL は上（遠ざける）が正。スクロールバックの行は上（古いほう）が負
        self.scroll_pane(id, ScrollAmount::Lines(-(notches as i64) * WHEEL_LINES));
    }

    /// このペインでマウスをアプリへ報告するか（報告が有効で、Shift なし、ペインが下を追っている）
    fn should_report(&self, id: i64, shift: bool) -> bool {
        if shift {
            return false;
        }
        let Some(s) = &self.session else { return false };
        let term = s.lock();
        let alt = term.is_alternate_screen();
        term.mouse_tracking() && self.panes.iter().find(|p| p.id == id).is_some_and(|p| p.follows(alt))
    }

    fn send_mouse(&mut self, id: i64, action: MouseAction, button: Option<MouseButton>, any_pressed: bool, client: (i32, i32), mods: Mods) {
        let Some(s) = self.session.clone() else { return };
        let Some(p) = self.panes.iter().find(|p| p.id == id) else { return };
        let Some(v) = &p.view else { return };
        let (w, h) = client_size(v.hwnd);
        let (cw, ch) = v.renderer.cell_size();
        let pad = v.renderer.padding();
        let geometry = MouseGeometry { screen_width: w, screen_height: h, cell_width: cw, cell_height: ch, padding: pad };
        let bytes = {
            let term = s.lock();
            self.mouse.encode(&term, geometry, action, button, any_pressed, (client.0 as f32, client.1 as f32), mods)
        };
        if !bytes.is_empty() {
            s.write(&bytes);
        }
    }

    fn screenshot(&mut self, pane: Option<i64>, path: PathBuf) {
        let id = pane.or(self.active);
        let Some(i) = id.and_then(|id| self.panes.iter().position(|p| p.id == id)).filter(|&i| self.panes[i].view.is_some()) else {
            self.shared.emit(Event::Error("画面を保存できません（子 HWND がありません）".into()));
            return;
        };
        self.panes[i].needs_sync = true;
        self.refresh(false);
        if self.draw_pane(i, Some(&path)) {
            log(&format!("画面を保存: {}", path.display()));
            self.shared.emit(Event::Screenshot(path.display().to_string()));
        } else {
            self.shared.emit(Event::Error(format!("画面を保存できません: {}", path.display())));
        }
    }

    /// ペインの見えている行のテキスト（デバッグ用）
    fn pane_text(&mut self, id: i64) -> Option<String> {
        self.refresh(false);
        let alt = self.alt_screen;
        let p = self.panes.iter().find(|p| p.id == id)?;
        let (frame, top) = if p.follows(alt) {
            (&self.master, self.master.rows as i32 - p.grid.1 as i32)
        } else {
            let s = p.scrolled.as_ref()?;
            (&s.frame, s.top)
        };
        let mut text = String::new();
        for y in 0..p.grid.1 as i32 {
            let fy = top + y;
            if fy >= 0 && fy < frame.rows as i32 {
                text.push_str(&frame.row_text(fy as u16));
            }
            text.push('\n');
        }
        Some(text)
    }

    /// copy モードのテキストをスクロールバックごと作り、Lisp に知らせる（テキストは [`Shared`] に置く）
    fn prepare_copy(&mut self, pane: Option<i64>) {
        let Some(s) = self.session.clone() else {
            *self.shared.copy.lock().unwrap_or_else(|e| e.into_inner()) = Some(CopyText { text: String::new(), top: 0, point: 0 });
            self.shared.emit(Event::CopyReady);
            return;
        };
        let t0 = Instant::now();
        let (text, top, point) = {
            let term = s.lock();
            let alt = term.is_alternate_screen();
            let text = term.format_plain(false, false).ok().flatten().unwrap_or_default();
            let total = term.total_rows() as i64;
            let (_, rows) = term.size();
            let cursor_row = total - rows as i64 + term.cursor().1 as i64;
            let id = pane.or(self.active);
            match id.and_then(|id| self.panes.iter().find(|p| p.id == id)) {
                Some(p) if !p.follows(alt) => {
                    let t = p.top_row(&term, alt).max(0);
                    (text, t, t)
                }
                Some(p) => (text, (total - p.grid.1 as i64).max(0), cursor_row),
                None => (text, (total - rows as i64).max(0), cursor_row),
            }
        };
        log(&format!("copy モードのテキスト: {} バイト、{} ms", text.len(), t0.elapsed().as_millis()));
        *self.shared.copy.lock().unwrap_or_else(|e| e.into_inner()) = Some(CopyText { text, top, point });
        self.shared.emit(Event::CopyReady);
    }

    /// 閉じる。子 HWND を壊し、端末の後始末は待たずに別のスレッドへ任せ（T-076）、UI スレッドを終える
    fn close(&mut self) {
        for p in &mut self.panes {
            if let Some(v) = p.view.take() {
                unsafe {
                    let _ = DestroyWindow(v.hwnd);
                }
            }
        }
        // TrackedRef はロックの中で落とす
        if let Some(s) = self.session.clone() {
            let term = s.lock();
            self.panes.clear();
            drop(term);
        } else {
            self.panes.clear();
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
            s.panes.clear();
        });
        unsafe {
            let _ = DestroyWindow(self.control);
        }
    }

    fn pane_cell(&self, id: i64, client: (i32, i32)) -> Option<(u16, u16)> {
        let p = self.panes.iter().find(|p| p.id == id)?;
        let v = p.view.as_ref()?;
        Some(v.renderer.cell_at(client.0, client.1, p.grid.0, p.grid.1))
    }

    /// ペインのセル（桁, 行）が、端末のどのセルか（画面の座標）。**ロックの中で呼ぶ**
    fn pane_ref(pane: &Pane, term: &Terminal, cell: (u16, u16), alt: bool) -> Option<GridRef> {
        let total = term.total_rows() as i64;
        let (cols, _) = term.size();
        let y = (pane.top_row(term, alt) + cell.1 as i64).clamp(0, (total - 1).max(0));
        let x = cell.0.min(cols.saturating_sub(1));
        term.grid_ref(x, y as u32).ok()
    }

    fn lbutton_down(&mut self, id: i64, hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
        let client = point_of(lparam);
        let mods = mods_of(wparam);
        if self.should_report(id, mods.shift) {
            self.drag = Some(Drag::Reporting(id, MouseButton::Left));
            unsafe { SetCapture(hwnd) };
            self.send_mouse(id, MouseAction::Press, Some(MouseButton::Left), true, client, mods);
            return;
        }
        let Some(cell) = self.pane_cell(id, client) else { return };
        let Some(session) = self.session.clone() else { return };
        let time_ns = self.epoch.elapsed().as_nanos() as u64;
        let repeat_ns = unsafe { GetDoubleClickTime() } as u64 * 1_000_000;
        let selection_made = {
            let mut term = session.lock();
            let alt = term.is_alternate_screen();
            let Some(p) = self.panes.iter().find(|p| p.id == id) else { return };
            let Some(at) = Self::pane_ref(p, &term, cell, alt) else { return };
            match term.gesture_press(at, (client.0 as f64, client.1 as f64), time_ns, repeat_ns) {
                Ok(Some(sel)) => {
                    let _ = term.apply_selection(Some(sel));
                    true
                }
                Ok(None) => {
                    // ふつうのクリック（ドラッグしなければ選択を消す）
                    let _ = term.apply_selection(None);
                    false
                }
                Err(e) => {
                    log(&format!("選択の操作に失敗: {e}"));
                    false
                }
            }
        };
        if !selection_made {
            self.shared.set_selection_text(None);
        }
        self.drag = Some(Drag::Selecting(id));
        unsafe { SetCapture(hwnd) };
        self.mark_selection_changed();
        self.refresh(false);
    }

    fn mouse_move(&mut self, id: i64, wparam: WPARAM, lparam: LPARAM) {
        let client = point_of(lparam);
        let mods = mods_of(wparam);
        let any_pressed = wparam.0 as u32 & (MK_LBUTTON | MK_RBUTTON | MK_MBUTTON) != 0;
        match self.drag {
            Some(Drag::Selecting(p)) if p == id => {
                let Some(cell) = self.pane_cell(id, client) else { return };
                let Some(session) = self.session.clone() else { return };
                let Some((cols, cw, pad, h)) = self.panes.iter().find(|p| p.id == id).and_then(|p| {
                    let v = p.view.as_ref()?;
                    let (cw, _) = v.renderer.cell_size();
                    let (_, h) = client_size(v.hwnd);
                    Some((p.grid.0, cw, v.renderer.padding(), h))
                }) else {
                    return;
                };
                let geometry = Geometry { columns: cols as u32, cell_width: cw, padding_left: pad, screen_height: h };
                {
                    let mut term = session.lock();
                    let alt = term.is_alternate_screen();
                    let Some(p) = self.panes.iter().find(|p| p.id == id) else { return };
                    let Some(at) = Self::pane_ref(p, &term, cell, alt) else { return };
                    if let Ok(Some(sel)) = term.gesture_drag(at, (client.0 as f64, client.1 as f64), geometry) {
                        let _ = term.apply_selection(Some(sel));
                    }
                }
                self.mark_selection_changed();
                self.refresh(false);
            }
            Some(Drag::Reporting(p, b)) if p == id => {
                self.send_mouse(id, MouseAction::Motion, Some(b), true, client, mods);
            }
            // 押していない動き（any-event トラッキング）。どのボタンも押していなければボタンなしで送る
            None if self.should_report(id, mods.shift) => {
                self.send_mouse(id, MouseAction::Motion, None, any_pressed, client, mods);
            }
            _ => {}
        }
    }

    fn lbutton_up(&mut self, id: i64, wparam: WPARAM, lparam: LPARAM) {
        let client = point_of(lparam);
        let mods = mods_of(wparam);
        match self.drag.take() {
            Some(Drag::Selecting(p)) if p == id => unsafe {
                let _ = ReleaseCapture();
                self.finish_selection(id, client);
            },
            Some(Drag::Reporting(p, b)) if p == id && b == MouseButton::Left => unsafe {
                let _ = ReleaseCapture();
                self.send_mouse(id, MouseAction::Release, Some(b), false, client, mods);
            },
            other => {
                self.drag = other;
            }
        }
    }

    /// 選択のドラッグを終えた。選択があればテキストを作り、Lisp に知らせる（kill-ring はそちらで）
    fn finish_selection(&mut self, id: i64, client: (i32, i32)) {
        let Some(cell) = self.pane_cell(id, client) else { return };
        let Some(session) = self.session.clone() else { return };
        let text = {
            let mut term = session.lock();
            let alt = term.is_alternate_screen();
            let at = self.panes.iter().find(|p| p.id == id).and_then(|p| Self::pane_ref(p, &term, cell, alt));
            let _ = term.gesture_release(at);
            term.format_plain(true, true).ok().flatten()
        };
        match text {
            Some(t) if !t.is_empty() => {
                self.shared.set_selection_text(Some(t.clone()));
                self.shared.emit(Event::Selected(t));
            }
            _ => self.shared.set_selection_text(None),
        }
    }

    /// 子 HWND のメッセージ。`None` なら DefWindowProc に任せる
    fn view_message(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let i = self.pane_index_by_hwnd(hwnd)?;
        let id = self.panes[i].id;
        match msg {
            WM_PAINT => unsafe {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                self.panes[i].needs_draw = true;
                self.refresh(false);
            },
            WM_SIZE => {
                self.panes[i].needs_draw = true;
                self.refresh(false);
            }
            WM_ERASEBKGND => return Some(LRESULT(1)),
            WM_LBUTTONDOWN => {
                // クリックしたら Emacs にそのウィンドウを選ばせる（子 HWND はフォーカスを取らない）
                self.shared.emit(Event::Clicked(id));
                self.lbutton_down(id, hwnd, wparam, lparam);
            }
            WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                self.shared.emit(Event::Clicked(id));
                let mods = mods_of(wparam);
                if self.should_report(id, mods.shift) {
                    let b = if msg == WM_RBUTTONDOWN { MouseButton::Right } else { MouseButton::Middle };
                    self.drag = Some(Drag::Reporting(id, b));
                    unsafe { SetCapture(hwnd) };
                    self.send_mouse(id, MouseAction::Press, Some(b), true, point_of(lparam), mods);
                }
                // 右クリックはそれ以上何もしない（メニューも貼り付けも無し）
            }
            WM_RBUTTONUP | WM_MBUTTONUP => {
                let mods = mods_of(wparam);
                let b = if msg == WM_RBUTTONUP { MouseButton::Right } else { MouseButton::Middle };
                if matches!(self.drag, Some(Drag::Reporting(p, d)) if p == id && d == b) {
                    self.drag = None;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    self.send_mouse(id, MouseAction::Release, Some(b), false, point_of(lparam), mods);
                } else if self.should_report(id, mods.shift) {
                    self.send_mouse(id, MouseAction::Release, Some(b), false, point_of(lparam), mods);
                }
            }
            WM_SETCURSOR => {
                // マウスの形（T-038）: テキストの上は I ビーム、アプリへ報告しているときは矢印
                if (lparam.0 & 0xffff) as u32 == HTCLIENT {
                    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                    let name = if self.should_report(id, shift) { IDC_ARROW } else { IDC_IBEAM };
                    unsafe {
                        SetCursor(LoadCursorW(None, name).ok());
                    }
                    return Some(LRESULT(1));
                }
                return None;
            }
            WM_MOUSEMOVE => self.mouse_move(id, wparam, lparam),
            WM_LBUTTONUP => self.lbutton_up(id, wparam, lparam),
            WM_CAPTURECHANGED => {
                if let Some(Drag::Selecting(p)) = self.drag
                    && p == id
                {
                    // ボタンを離さないまま取り上げられた（フォーカスの変化など）。選択を確定して終える
                    self.drag = None;
                    self.finish_selection(id, point_of(lparam));
                }
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                let mut pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
                unsafe {
                    let _ = ScreenToClient(hwnd, &mut pt);
                }
                self.wheel_pane(id, delta, (pt.x, pt.y), mods_of(wparam));
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }
}

/// マウスのメッセージの wParam の修飾キー（Alt は wParam に無いので GetKeyState で読む）
fn mods_of(wparam: WPARAM) -> Mods {
    let keys = wparam.0 as u32 & 0xffff;
    Mods {
        shift: keys & MK_SHIFT != 0,
        ctrl: keys & MK_CONTROL != 0,
        alt: unsafe { GetKeyState(VK_MENU.0 as i32) } < 0,
    }
}

extern "system" fn control_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let r = catch_unwind(AssertUnwindSafe(|| match msg {
        WM_APP_REQUEST => with_app(|a| a.drain_requests()).map(|_| LRESULT(0)),
        WM_APP_OUTPUT => with_app(|a| {
            a.output_pending = true;
            a.refresh(false);
        })
        .map(|_| LRESULT(0)),
        WM_APP_SESSION => with_app(|a| a.session_changed()).map(|_| LRESULT(0)),
        WM_TIMER if wparam.0 == TIMER_RENDER_HOLD => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_RENDER_HOLD);
            }
            with_app(|a| a.refresh(false)).map(|_| LRESULT(0))
        }
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
                if let Some(i) = a.pane_index_by_hwnd(hwnd) {
                    let id = a.panes[i].id;
                    a.view_destroyed(id);
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
