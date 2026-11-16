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
//! - **UI スレッドは `Present` で止まらない**（T-068）。ペインのスワップチェーンが次のフレームを受け付けるまで描くのを後回しにし、
//!   メッセージのループ（[`message_loop`]）がそのハンドルもメッセージと一緒に待つ。描けるペインが無い間は出力の知らせでも写さない
//!   （読みのスレッドの「知らせを出した」印を立てたままにして、知らせを止める）。描くのは画面の更新の速さ（60 Hz）まで
//! - GPU デバイスはプロセスで 1 つを分け合う（T-067）。失ったら作り直して、ほかの端末にも [`WM_APP_DEVICE`] で知らせる
//! - 測定（T-054）: [`Perf`]（UI スレッドの 1 フレームの内訳・頼みごとの遅れ・入力から表示まで）。`embterm-module-perf` で読む
//! - 色（T-034）・カーソルの既定（T-035）・クリップボードの許可（T-039）は Lisp が頼みごとで送る。子プロセスを起動する前に
//!   届いたものは [`SessionOptions`] に入れ、起動後のものは [`Session`] に反映する
//! - カーソルの点滅（T-035）は制御用のウィンドウのタイマー。**選んでいるペインが見えていて、フォーカスがあり、カーソルが
//!   点滅するときだけ**動かし、1 回ごとにカーソルのセルだけを描き直す（[`Renderer::draw_blink`]）
//! - OSC 52 / 9 / 777 / 9;4 と、Ctrl+クリックしたハイパーリンク（OSC 8）は [`Event`] で Lisp に渡す（T-039）
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

use et_render::{FontSpec, Frame, FrameView, Gpu, NarrowWide, Renderer, Theme};
use et_session::keys::{KeyEvent, KeyPhase};
use et_session::perf::Samples;
use et_session::{Colors, CursorDefault, Event as SessionEvent, Session, SessionOptions, keys};
use et_vt::{Geometry, GridRef, Key, KeyEncoder, Mode, Mods, MouseAction, MouseButton, MouseEncoder, MouseGeometry, RenderState, Rgb, Scroll, Terminal, TrackedRef};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT, ScreenToClient};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::INFINITE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_MENU, VK_SHIFT,
};
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
/// 分け合う GPU デバイスが替わった（ほかの端末がデバイスを失って作り直した。T-067）
const WM_APP_DEVICE: u32 = WM_APP + 4;
/// 大きな画像の変換が別のスレッドで終わった（描き直す。T-105）
const WM_APP_IMAGE: u32 = WM_APP + 5;
/// スワップチェーンがフレームを受け付けるのを待つ時間の上限（ms）。過ぎたら待たずに描く（T-068）
const FRAME_WAIT_TIMEOUT_MS: u32 = 250;

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
/// カーソルの点滅のタイマー（T-035）
const TIMER_CURSOR_BLINK: usize = 2;
/// 選択のドラッグがペインの上下の外に出ている間、ペインをスクロールするタイマー（T-084）
const TIMER_AUTOSCROLL: usize = 3;
/// 文字の点滅（SGR 5。T-112）のタイマー
const TIMER_TEXT_BLINK: usize = 4;
/// Kitty graphics のアニメーションの次のフレーム（T-092。1 回ごとに掛け直す）
const TIMER_ANIMATION: usize = 5;
/// 文字の点滅の半周期（ms。xterm の既定 600 ms に近い値）
const TEXT_BLINK_MS: u32 = 600;
/// マウスがウィンドウから出た（`TrackMouseEvent` の `TME_LEAVE`）。windows クレートでは Controls の feature にあるので値を書く
const WM_MOUSELEAVE: u32 = 0x02A3;
/// 自動スクロールの間隔（ms）。1 回に動かす行数は外へ出た距離で増やす
const AUTOSCROLL_MS: u32 = 50;
/// Lisp がまだ読んでいない知らせの上限（アプリが OSC を大量に出したときに溜め込みすぎない）
const MAX_PENDING_EVENTS: usize = 1000;
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
    /// シェルのプロンプト（OSC 133）を N 個（負は上）。プロンプトの行をペインの一番上にする（T-119）
    Prompt(i64),
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
    /// ペインを置く前に子プロセスを起動する（T-082）。まだ大きさが決まっていなければ COLS x ROWS で。
    /// 後でペインを置けばその大きさに合わせる
    Start { cols: u16, rows: u16 },
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
    SetFont(FontSpec),
    SetNarrowWide(NarrowWide),
    /// フォーカスの出入り（端末へ `CSI I` / `CSI O` を送り、カーソルの描き方を変える）
    Focus(bool),
    /// copy モードの準備（スクロールバックごとのテキストを作る。できたら [`Event::CopyReady`]）。ペインのスクロール位置に合わせる
    Copy(Option<i64>),
    /// ペインの今の画面を PNG に保存する（`None` は選んでいるペイン。デバッグ用）
    Screenshot { pane: Option<i64>, path: PathBuf },
    /// ペインの見えている行のテキスト（デバッグ用）
    PaneText { pane: i64, reply: mpsc::Sender<Option<String>> },
    /// 既定の色（T-034）
    SetColors(ColorConfig),
    /// カーソルの既定（T-035）
    SetCursor(CursorDefault),
    /// クリップボードの書き込み・読み取りを許すか（T-039）
    SetClipboardPolicy { write: bool, read: bool },
    /// Kitty graphics の画像の保存量の上限と、一時ファイル（`t=t`）を読むディレクトリ（T-093）
    SetImages(et_session::ImageSettings),
    /// ペインの画素の位置のハイパーリンク（デバッグ・自動確認用）
    LinkAt { pane: i64, x: i32, y: i32, reply: mpsc::Sender<Option<String>> },
    /// マウスがペインのクライアント座標 (x, y) にあるものとしてリンクの下線を調べ直す（自動確認用。T-101。
    /// PostMessage の WM_MOUSEMOVE では、本物のカーソルが外にあるので WM_MOUSELEAVE がすぐ来て消えてしまう）
    DebugHover { pane: i64, x: i32, y: i32 },
    /// 描画とカーソルの状態（デバッグ・自動確認用）
    DebugInfo(mpsc::Sender<DebugInfo>),
    /// 性能の測定の報告（T-054。`reset` なら報告のあと消す）
    Perf { reset: bool, reply: mpsc::Sender<String> },
    Close,
}

/// Lisp から受けた色（T-034）。端末の既定の色と、描く側の選択の色。`None` は module の既定
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

    /// 描く側の色（選択とカーソル）を `theme` に入れる
    fn apply_theme(&self, theme: &mut Theme) {
        let d = Theme::default();
        theme.selection_bg = self.selection_bg.unwrap_or(d.selection_bg);
        theme.selection_fg = self.selection_fg;
        theme.cursor = self.cursor.unwrap_or(d.cursor);
    }
}

/// 描画とカーソルの状態（[`Request::DebugInfo`]）
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
    /// 選んでいる（無ければ最後に選んだ）ペインのセルの大きさと余白（画素）
    pub cell: (u32, u32),
    pub padding: u32,
    /// 下線を引いているリンクの範囲（ペインの行, 始めの桁, 終わりの桁。T-101）
    pub hover: Option<(u16, u16, u16)>,
    /// 文字の点滅のタイマーが動いているか、今は見せている側か（T-112）
    pub text_blink_timer: bool,
    pub text_blink_on: bool,
    /// Kitty graphics のアニメーションを進めた回数と、次のフレームの予定があるか（T-092）
    pub animation_ticks: u64,
    pub animation_playing: bool,
    /// 別のスレッドでの画像の変換が終わった回数と、変換中の数（T-105）
    pub images_converted: u64,
    pub images_pending: usize,
    /// 端末のバッファにフォーカスがあるか（T-134）
    pub focused: bool,
    /// IME の変換中の文字列（macOS だけ。Windows では IME 自身のウィンドウが描く。T-133）
    pub preedit: Option<String>,
    /// macOS だけ（CALayer のアニメーションの数。T-132）
    pub layer_animations: usize,
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
    /// アプリがクリップボードへ書いた（OSC 52。T-039）
    Clipboard(String),
    /// デスクトップ通知（OSC 9 / 777）
    Notification { title: String, body: String },
    /// 進捗（OSC 9;4）。状態の名前（"set" など）と割合
    Progress { state: &'static str, percent: Option<u8> },
    /// ハイパーリンク（OSC 8）が Ctrl+クリックされた（URI）
    Link(String),
    /// マウスの下のハイパーリンクが変わった（URI。外れたら `None`。T-101）
    Hover(Option<String>),
    /// シェルから Emacs の関数を呼ぶ（OSC 51;E の中身。T-118）
    ShellCommand(String),
    /// シェルがコマンドの終わりを知らせた（OSC 133;D の終了コード。T-119）
    CommandEnd(Option<i32>),
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
    /// Emacs の環境変数への変更（`Some` は設定、`None` は削除。T-109）
    pub env: Vec<(OsString, Option<OsString>)>,
    pub conpty_dir: Option<PathBuf>,
    pub font: FontSpec,
    pub narrow_wide: NarrowWide,
    pub scrollback_lines: Option<usize>,
}

/// フォントの指定を作る。FAMILIES は前から順に候補で、その後ろに既定の候補が続く。SIZE（ポイント）が `None` なら既定。
/// FALLBACK は主フォントに無い文字に、システムのフォールバックより先に使うファミリ（T-033）
pub fn font_spec(families: Vec<String>, size: Option<f32>, fallback: Vec<String>) -> FontSpec {
    let mut spec = FontSpec::default();
    spec.families.splice(0..0, families);
    if let Some(s) = size {
        spec.size_pt = s;
    }
    spec.fallback = fallback;
    spec
}

/// copy モード用のテキスト（`Event::CopyReady` のあと Lisp が分けて受け取る。T-086）
pub struct CopyText {
    pub text: String,
    /// Lisp に渡し終えたバイト数
    pub sent: usize,
    /// 見ているペインの先頭の行（テキストの行番号。0 から）
    pub top: i64,
    /// ポイントを置く行（スクロールしていなければカーソルの行、遡っていればペインの先頭の行）
    pub point: i64,
    /// 端末が折り返した行（行末の改行が本当の改行ではない行）の番号
    pub wraps: Vec<i64>,
}

/// copy モードのテキストの一部（[`Shared::take_copy`]）
pub struct CopyChunk {
    pub text: String,
    pub top: i64,
    pub point: i64,
    /// 最初の一部だけに入れる（以後は空）
    pub wraps: Vec<i64>,
    /// まだ残りがある
    pub more: bool,
}

/// copy モードのテキストを一度に渡す量の上限（行の区切りで切る）。大きなスクロールバックでも Emacs を長く止めない
const COPY_CHUNK: usize = 1 << 20;

/// UI スレッドと Lisp のスレッドで分け合うもの
pub struct Shared {
    state: Mutex<State>,
    events: Mutex<Vec<Event>>,
    /// 頼みごとと、積んだ時刻（UI スレッドが取り出すまでの遅れを測る。T-054）
    requests: Mutex<Vec<(Instant, Request)>>,
    /// WM_APP_REQUEST を投げたまま、UI スレッドがまだ列を取り出していない（投げすぎない）
    request_posted: AtomicBool,
    /// 制御用のウィンドウ（0 なら UI スレッドが終わった）
    control: AtomicIsize,
    session: OnceLock<Arc<Session>>,
    /// 選択のテキスト（選択を終えたときに UI スレッドが作って置く。Lisp はロックせずに読める）
    selection: Mutex<Option<String>>,
    /// copy モードのテキスト
    copy: Mutex<Option<CopyText>>,
    /// 入力から表示までの遅れの測定（T-054）
    probe: Mutex<LatencyProbe>,
}

/// 入力から表示までの遅れの測定（T-054）。キーを 1 つ追いかける:
/// Lisp が頼んだ → UI スレッドが PTY へ書いた → 読みのスレッドがエコーを端末に書いた → UI スレッドが描いて Present を終えた
#[derive(Default)]
struct LatencyProbe {
    sent: Option<Instant>,
    ui: Option<Instant>,
    output: Option<Instant>,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut State)) {
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut s);
            s.generation += 1;
        }
        crate::notify::notify();
    }

    fn emit(&self, e: Event) {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).push(e);
        self.update(|_| {});
    }

    /// アプリが出したものの知らせ（ベル・OSC）。まだ読まれていない同じ知らせは重ねない（ベルは 1 つに、進捗は最後のものに）。
    /// 溜まりすぎたら捨てる（大量に出すアプリで Emacs を詰まらせない。T-040）
    fn emit_from_app(&self, e: Event) {
        {
            let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
            match (events.last_mut(), &e) {
                (Some(Event::Bell), Event::Bell) => return,
                (Some(last @ Event::Progress { .. }), Event::Progress { .. }) => {
                    *last = e;
                    drop(events);
                    self.update(|_| {});
                    return;
                }
                _ => {}
            }
            if events.len() >= MAX_PENDING_EVENTS {
                return;
            }
            events.push(e);
        }
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

    /// copy モードのテキストの次の一部（[`COPY_CHUNK`] まで。行の区切りで切る）。渡し終えたら捨てる
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

/// 生きている端末の制御用のウィンドウ（GPU デバイスが替わったことを全員に知らせる。T-067）
static CONTROLS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// 分け合う GPU デバイスが替わったことを、`except` 以外の端末に知らせる
fn broadcast_device_changed(except: HWND) {
    let controls = CONTROLS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for h in controls {
        if h != except.0 as isize {
            post(h, WM_APP_DEVICE, 0);
        }
    }
}

/// 動いている端末（Emacs を終えるときに閉じる。T-076）
static SESSIONS: Mutex<Vec<Weak<Session>>> = Mutex::new(Vec::new());

/// Emacs を終えるとき（`kill-emacs-hook`）に、全部の端末を閉じる（T-076）。**Lisp のスレッドで最大 `grace` + 0.3 秒待つ。**
///
/// 待たずに Emacs が終わると、Job のハンドルが閉じて中のアプリが**いきなり**止められる（CTRL_CLOSE_EVENT も届かない）うえ、
/// Job の外の子孫は残る。ここでは各端末で ClosePseudoConsole（アプリに CTRL_CLOSE_EVENT）を送り、`grace` まで穏やかに終わるのを
/// 待ち、過ぎたら Job の外の子孫も含めて止める。戻り値は（時間内に閉じ終えた数, 端末の数）
pub fn shutdown(grace: Duration) -> (usize, usize) {
    let sessions: Vec<Arc<Session>> = SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).iter().filter_map(Weak::upgrade).collect();
    let closings: Vec<_> = sessions.iter().map(|s| s.close_with(grace)).collect();
    let deadline = Instant::now() + grace + Duration::from_millis(300);
    let done = closings.iter().filter(|c| c.wait(deadline.saturating_duration_since(Instant::now()))).count();
    for s in &sessions {
        if let Some(r) = s.close_report() {
            log(&format!("Emacs を終える前に閉じた: {r}"));
        }
    }
    (done, closings.len())
}

/// デバイスを失ったことにする（デバッグ・自動確認用。T-067）。分け合うデバイスを捨て、全部の端末に載せ替えさせる
pub fn debug_lose_device() {
    if let Ok(g) = Gpu::shared() {
        log(&format!("GPU デバイスの世代 {} を失ったことにする", g.generation()));
        g.invalidate();
    }
    broadcast_device_changed(HWND(std::ptr::null_mut()));
}

fn post(hwnd: isize, msg: u32, wparam: usize) -> bool {
    hwnd != 0 && unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(wparam), LPARAM(0)) }.is_ok()
}

/// Emacs のフレームの HWND に `WM_IME_STARTCOMPOSITION` を投げる（T-137）。
///
/// tr-ime のサブクラスが `ImmSetCompositionWindow (compform_)` を呼ぶのは、このメッセージを受けたときだけ
/// （docs/research/emacs-ime.md §3.4）。Emacs は**カーソルを描き直すたびに**自分でこれを投げている
/// （§2.3 の 2、`w32term.c:6685`）が、EmbTerm の端末は子 HWND に描くので Emacs の再表示が起きず、
/// `tr-ime-modadv--set-composition-window` で位置を入れ直しても IME に届かない。そこで同じ手を自分で行う。
///
/// Emacs と同じく **Lisp のスレッドから `PostMessage`**（受けるのはフレームを持つウィンドウのスレッド）。
/// 変換を始めていないときに投げても、Emacs 自身が毎回そうしているので害は無い。
pub fn post_ime_start_composition(hwnd: isize) -> bool {
    hwnd != 0 && unsafe { IsWindow(Some(HWND(hwnd as *mut _))) }.as_bool() && post(hwnd, WM_IME_STARTCOMPOSITION, 0)
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
        if matches!(r, Request::SendKey { .. } | Request::SendString(_) | Request::Paste(_)) {
            let mut p = self.shared.probe.lock().unwrap_or_else(|e| e.into_inner());
            // エコーが来なかった（表示の変わらないキー）ものは 1 秒で捨てて測り直す
            if p.sent.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
                *p = LatencyProbe { sent: Some(Instant::now()), ..Default::default() };
            }
        }
        let control = self.shared.control.load(Ordering::Acquire);
        if control == 0 {
            if let (Some(s), Request::SendString(text)) = (self.shared.session(), &r) {
                s.write(text.as_bytes());
            }
            return;
        }
        self.shared.requests.lock().unwrap_or_else(|e| e.into_inner()).push((Instant::now(), r));
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
        probe: Mutex::new(LatencyProbe::default()),
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
        CONTROLS.lock().unwrap_or_else(|e| e.into_inner()).push(control.0 as isize);
        shared.update(|s| s.alive = true);
        let _ = ready.send(Ok(()));
        message_loop();
    }));
    if r.is_err() {
        let _ = ready2.send(Err("端末のスレッドで panic".into()));
        log("端末のスレッドで panic（スレッドを終える）");
        shared.emit(Event::Error("端末のスレッドで panic しました".into()));
    }
    let control = shared.control.swap(0, Ordering::AcqRel);
    CONTROLS.lock().unwrap_or_else(|e| e.into_inner()).retain(|&h| h != control);
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

/// メッセージのループ。描くのを待っているペインのスワップチェーンのハンドル（フレームを受け付けると知らせる）も一緒に待つ。
/// 知らされたらそのペインを描く。[`FRAME_WAIT_TIMEOUT_MS`] を過ぎても来なければ待たずに描く（T-068）
fn message_loop() {
    let mut msg = MSG::default();
    loop {
        let waiting = with_app(|a| a.waiting_frames()).unwrap_or_default();
        let handles: Vec<HANDLE> = waiting.iter().map(|w| w.1).collect();
        let timeout = if handles.is_empty() { INFINITE } else { FRAME_WAIT_TIMEOUT_MS };
        let r = unsafe { MsgWaitForMultipleObjectsEx(Some(&handles), timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        let signaled = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if signaled < handles.len() {
            let id = waiting[signaled].0;
            with_app(|a| a.frame_signaled(id, false));
        } else if r == WAIT_TIMEOUT {
            for (id, _) in &waiting {
                with_app(|a| a.frame_signaled(*id, true));
            }
        }
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                return;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
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
    /// 描こうとしたが、スワップチェーンが次のフレームをまだ受け付けない（知らせを待っている。T-068）
    waiting_frame: bool,
    /// マウスの下にあるハイパーリンクの範囲（行, 始めの桁, 終わりの桁（含まない））。下線を引く（T-101）
    hover: Option<(u16, u16, u16)>,
    /// WM_MOUSELEAVE を頼んだ（TrackMouseEvent。ペインから出たら下線を消す）
    tracking_leave: bool,
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
    /// 色（T-034）。起動前に届いたものは起動のときに使う
    colors: ColorConfig,
    /// カーソルの既定（T-035）
    cursor_default: CursorDefault,
    /// クリップボードの書き込み・読み取りを許すか（T-039）
    clipboard: (bool, bool),
    /// 点滅のタイマーが動いているか
    blink_timer: bool,
    /// 選択のドラッグがペインの上下の外に出ている（ペインの番号と最後のクライアント座標。T-084）
    autoscroll: Option<(i64, (i32, i32))>,
    /// マウスの下のハイパーリンクの URI（T-101。変わったら Lisp に知らせる）
    hover_uri: Option<String>,
    /// 点滅でカーソルが点いているか（タイマーが止まっているときは常に true）
    blink_on: bool,
    /// Kitty graphics の設定（T-093。起動前に来たら起動のときに当てる）
    images: et_session::ImageSettings,
    /// 文字の点滅（SGR 5）のタイマーが動いている・今は見せている側か（T-112）
    text_blink_timer: bool,
    text_blink_on: bool,
    /// Kitty graphics のアニメーションの次のフレームの時刻（`epoch` からの ms。T-092）と、進めた回数
    animation_next: Option<u64>,
    animation_ticks: u64,
    /// 別のスレッドでの画像の変換が終わった回数（T-105。自動確認用）
    image_ready_count: u64,
    /// 点滅の回数（自動確認用）
    blink_ticks: u64,
    /// 前に描いたカーソルの位置（動いたら点滅をやり直して点ける）
    blink_pos: Option<(u16, u16)>,
    /// 性能の測定（T-054）
    perf: Perf,
}

/// UI スレッドの測定（T-054 / T-068）。`refresh` 1 回ごとと、ペインを 1 枚描くごと
struct Perf {
    /// 端末のロックを待った時間
    lock_wait: Samples,
    /// 端末のロックを持っていた時間（render state の更新・画像の写し・遡ったペインの写し）
    lock_hold: Samples,
    /// render state から `Frame` へ写した時間（ロックの外）
    frame_update: Samples,
    /// ペイン 1 枚の `BeginDraw` 〜 `EndDraw`
    draw: Samples,
    /// ペイン 1 枚の `Present`
    present: Samples,
    /// `refresh` 全体（下を追うペインを描いたときだけ）
    refresh: Samples,
    /// Lisp が頼みごとを積んでから UI スレッドが取り出すまで（UI スレッドがほかの仕事で塞がっている時間）
    request_delay: Samples,
    /// 入力から表示まで（[`LatencyProbe`]）: Lisp → UI スレッドが書いた / → エコーが端末に入った / → 描き終えた / 全体
    input_ui: Samples,
    input_echo: Samples,
    input_draw: Samples,
    input_total: Samples,
}

impl Perf {
    fn new() -> Self {
        Self {
            lock_wait: Samples::new("ui.lock_wait"),
            lock_hold: Samples::new("ui.lock_hold"),
            frame_update: Samples::new("ui.frame_update"),
            draw: Samples::new("ui.draw"),
            present: Samples::new("ui.present"),
            refresh: Samples::new("ui.refresh"),
            request_delay: Samples::new("ui.request_delay"),
            input_ui: Samples::new("input.to_ui"),
            input_echo: Samples::new("input.to_echo"),
            input_draw: Samples::new("input.to_drawn"),
            input_total: Samples::new("input.total"),
        }
    }

    fn all(&self) -> [&Samples; 11] {
        [
            &self.request_delay,
            &self.lock_wait,
            &self.lock_hold,
            &self.frame_update,
            &self.draw,
            &self.present,
            &self.refresh,
            &self.input_ui,
            &self.input_echo,
            &self.input_draw,
            &self.input_total,
        ]
    }
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
        let font = opts.font.clone();
        Ok(Self {
            control,
            shared,
            narrow_wide: opts.narrow_wide,
            pending: Some(opts),
            session: None,
            panes: Vec::new(),
            active: None,
            selected: None,
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
            colors: ColorConfig::default(),
            cursor_default: CursorDefault::default(),
            clipboard: (false, false),
            blink_timer: false,
            autoscroll: None,
            hover_uri: None,
            blink_on: true,
            images: Default::default(),
            text_blink_timer: false,
            text_blink_on: true,
            animation_next: None,
            animation_ticks: 0,
            image_ready_count: 0,
            blink_ticks: 0,
            blink_pos: None,
            perf: Perf::new(),
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
        let now = Instant::now();
        for (at, r) in requests {
            self.perf.request_delay.add(now - at);
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
            Request::Start { cols, rows } => {
                if self.grid.0 == 0 {
                    self.grid = (cols.max(1), rows.max(1));
                }
                self.ensure_session();
            }
            Request::SetScrollback(lines) => {
                if let Some(s) = &self.session
                    && let Err(e) = s.lock().set_scrollback_max_lines(lines)
                {
                    log(&format!("スクロールバックの行数を変えられません: {e}"));
                }
            }
            Request::SetFont(spec) => {
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
            Request::SetColors(c) => {
                self.colors = c;
                for p in &mut self.panes {
                    if let Some(v) = &mut p.view {
                        c.apply_theme(&mut v.renderer.theme);
                    }
                }
                if let Some(s) = &self.session {
                    s.set_colors(&c.term_colors());
                }
                self.refresh(true);
            }
            Request::SetCursor(c) => {
                self.cursor_default = c;
                if let Some(s) = &self.session {
                    s.set_cursor_default(c);
                }
                self.refresh(true);
            }
            Request::SetClipboardPolicy { write, read } => {
                self.clipboard = (write, read);
                if let Some(s) = &self.session {
                    s.set_clipboard_policy(write, read);
                }
            }
            Request::SetImages(images) => {
                if let Some(s) = &self.session {
                    s.set_images(&images);
                }
                self.images = images;
            }
            Request::LinkAt { pane, x, y, reply } => {
                let _ = reply.send(self.link_at(pane, (x, y)));
            }
            Request::DebugHover { pane, x, y } => self.update_hover(pane, Some((x, y))),
            Request::DebugInfo(reply) => {
                let mut info = DebugInfo {
                    blink_timer: self.blink_timer,
                    blink_on: self.blink_on,
                    blink_ticks: self.blink_ticks,
                    cursor_shape: match self.master.cursor.shape {
                        et_vt::CursorShape::Bar => "bar",
                        et_vt::CursorShape::Block => "block",
                        et_vt::CursorShape::Underline => "underline",
                        et_vt::CursorShape::BlockHollow => "block-hollow",
                    },
                    cursor_blinking: self.master.cursor.blinking,
                    cursor_visible: self.master.cursor.visible,
                    foreground: self.master.foreground,
                    background: self.master.background,
                    palette1: self.master.palette[1],
                    text_blink_timer: self.text_blink_timer,
                    text_blink_on: self.text_blink_on,
                    animation_ticks: self.animation_ticks,
                    animation_playing: self.animation_next.is_some(),
                    images_converted: self.image_ready_count,
                    focused: self.focused,
                    ..Default::default()
                };
                for p in &self.panes {
                    if let Some(v) = &p.view {
                        info.full_draws += v.renderer.stats.full;
                        info.partial_draws += v.renderer.stats.partial;
                        if Some(p.id) == self.active {
                            info.cell = v.renderer.cell_size();
                            info.padding = v.renderer.padding();
                        }
                        info.hover = info.hover.or(p.hover);
                        info.images_pending += v.renderer.images_pending();
                    }
                }
                let _ = reply.send(info);
            }
            Request::Perf { reset, reply } => {
                let _ = reply.send(self.perf_report(reset));
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
                // 入力から表示までの測定: UI スレッドが書いたあとの最初の出力（エコー）
                if let Some(s) = weak.upgrade() {
                    let mut p = s.probe.lock().unwrap_or_else(|e| e.into_inner());
                    if p.ui.is_some() && p.output.is_none() {
                        p.output = Some(Instant::now());
                    }
                }
                post(control, WM_APP_OUTPUT, 0);
            }
            // UI スレッドがもう終わっていても届くように、ここで書く（Weak: 端末 → notify → Shared の循環を作らない）
            SessionEvent::Closed => {
                if let Some(s) = weak.upgrade() {
                    if let Some(r) = s.session().and_then(|sess| sess.close_report()) {
                        log(&format!("閉じた: {r}"));
                    }
                    s.update(|st| st.closed = true);
                }
            }
            // 読みのスレッドで、端末のロックを持ったまま呼ばれる。共有の列に積むだけ（T-039 / T-040）
            SessionEvent::Bell => {
                if let Some(s) = weak.upgrade() {
                    s.emit_from_app(Event::Bell);
                }
            }
            SessionEvent::ClipboardWrite(text) => {
                if let Some(s) = weak.upgrade() {
                    s.emit_from_app(Event::Clipboard(text));
                }
            }
            SessionEvent::Notification { title, body } => {
                if let Some(s) = weak.upgrade() {
                    s.emit_from_app(Event::Notification { title, body });
                }
            }
            SessionEvent::Progress { state, percent } => {
                if let Some(s) = weak.upgrade() {
                    s.emit_from_app(Event::Progress { state: state.name(), percent });
                }
            }
            SessionEvent::ShellCommand(c) => {
                if let Some(s) = weak.upgrade() {
                    s.emit_from_app(Event::ShellCommand(c));
                }
            }
            SessionEvent::CommandEnd(code) => {
                if let Some(s) = weak.upgrade() {
                    s.emit_from_app(Event::CommandEnd(code));
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
                env: opts.env.clone(),
                cols,
                rows,
                cell_px: self.cell,
                conpty_search: search,
                colors: self.colors.term_colors(),
                cursor: self.cursor_default,
                scrollback_lines: opts.scrollback_lines.unwrap_or(DEFAULT_SCROLLBACK),
                clipboard_write: self.clipboard.0,
                clipboard_read: self.clipboard.1,
                images: self.images.clone(),
            },
            notify,
        );
        match started {
            Ok(s) => {
                let s = Arc::new(s);
                log(&format!("起動: {:?} / {cols}x{rows} / pid {} / ConPTY: {}", opts.argv, s.pid(), s.pty_kind()));
                for w in s.warnings() {
                    log(&format!("注意: {w}"));
                }
                let _ = self.shared.session.set(s.clone());
                {
                    let mut all = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
                    all.retain(|w| w.strong_count() > 0);
                    all.push(Arc::downgrade(&s));
                }
                // すでにフォーカスがあれば知らせる（ConPTY は 1004 を必ず求める。T-070）
                if self.focused {
                    s.write(b"\x1b[I");
                }
                let (pid, kind) = (s.pid(), s.pty_kind().to_string());
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
        // GPU デバイスはプロセスで 1 つを分け合う（T-067）
        let gpu = match Gpu::shared() {
            Ok(g) => g,
            Err(e) => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Err(format!("GPU デバイスを作れません: {e}"));
            }
        };
        match Renderer::with_gpu(gpu, hwnd, cw.max(1), ch.max(1), dpi, self.font.clone()) {
            Ok(mut renderer) => {
                renderer.theme.narrow_wide = self.narrow_wide;
                // 大きな画像は別のスレッドで変換し、終わったら制御用のウィンドウに知らせて描き直す（T-105）
                let control = self.control.0 as isize;
                renderer.set_image_ready(move || {
                    post(control, WM_APP_IMAGE, 0);
                });
                self.colors.apply_theme(&mut renderer.theme);
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
                waiting_frame: false,
                hover: None,
                tracking_leave: false,
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
        } else if let (Some(grid), Some(cell), Some(s)) = (target_grid, cell, &self.session)
            && let Some(now) = s.size_drift(grid)
        {
            // アプリが DECCOLM で 80 / 132 桁にした（libghostty-vt と ConPTY の両方が従う）。ペインの大きさに戻す（T-055）
            s.resize(grid.0, grid.1, cell);
            log(&format!("端末の大きさが {}x{} に変わった（DECCOLM）のでペインの {}x{} に戻す", now.0, now.1, grid.0, grid.1));
            for p in &mut self.panes {
                p.needs_sync = true;
                p.needs_draw = true;
            }
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
        let t_start = Instant::now();
        session.take_output();
        let output = std::mem::take(&mut self.output_pending);
        // （ペインの番号, そのペインに写した render state, ペインの先頭が `Frame` の何行目か）
        let mut synced: Vec<(usize, RenderState, i32)> = Vec::new();
        let hold;
        let t_locked;
        {
            let mut term = session.lock();
            t_locked = Instant::now();
            self.perf.lock_wait.add(t_locked - t_start);
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
            // Kitty graphics のアニメーションを今の時刻まで進める（T-092。進んだ画像は世代が変わり、下で画素を写し直す）
            let now_ms = self.epoch.elapsed().as_millis() as u64;
            let next = term.kitty_animation_tick(now_ms);
            if next.is_some() || self.animation_next.is_some() {
                self.schedule_animation(now_ms, next);
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
        let t_unlocked = Instant::now();
        self.perf.lock_hold.add(t_unlocked - t_locked);
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
        self.perf.frame_update.add(t_unlocked.elapsed());
        self.publish_cursor();
        self.update_blink();
        self.update_text_blink();
        let mut drew_follow = false;
        for i in 0..self.panes.len() {
            let p = &self.panes[i];
            if !p.visible() {
                continue;
            }
            let follows = p.follows(self.alt_screen);
            if (force || p.needs_draw || (follows && master_changed)) && self.draw_pane(i, None) {
                drew_follow |= follows;
            }
        }
        if drew_follow {
            self.perf.refresh.add(t_start.elapsed());
            self.finish_probe();
        }
    }

    /// 入力から表示までの測定（T-054）: エコーが届いたあとで、下を追うペインを描き終えたら 1 件記録する
    fn finish_probe(&mut self) {
        let mut p = self.shared.probe.lock().unwrap_or_else(|e| e.into_inner());
        if let (Some(sent), Some(ui), Some(output)) = (p.sent, p.ui, p.output) {
            let now = Instant::now();
            self.perf.input_ui.add(ui - sent);
            self.perf.input_echo.add(output - ui);
            self.perf.input_draw.add(now - output);
            self.perf.input_total.add(now - sent);
            *p = LatencyProbe::default();
        }
    }

    /// 測定の報告（T-054）
    fn perf_report(&mut self, reset: bool) -> String {
        let mut out = String::new();
        for s in self.perf.all() {
            out.push_str(&s.line());
            out.push('\n');
        }
        if let Some(s) = &self.session {
            out.push_str(&s.perf_report(reset));
        }
        let png = et_vt::png_decode_stats();
        out.push_str(&format!(
            "png.decode n={} total={:.1}ms max={:.1}ms last={:.1}ms last_pixels={}\n",
            png.count,
            png.total_us as f64 / 1000.0,
            png.max_us as f64 / 1000.0,
            png.last_us as f64 / 1000.0,
            png.last_pixels
        ));
        let generations: Vec<u64> = self.panes.iter().filter_map(|p| p.view.as_ref().map(|v| v.renderer.gpu_generation())).collect();
        out.push_str(&format!(
            "gpu.live_devices={} generations={:?} panes={} visible={}\n",
            et_render::live_devices(),
            generations,
            self.panes.len(),
            self.panes.iter().filter(|p| p.visible()).count()
        ));
        if reset {
            self.perf = Perf::new();
        }
        out
    }

    /// 点滅させるペイン（選んでいて、見えていて、下を追っていて、フォーカスがあり、カーソルが見えて点滅する）
    fn blink_target(&self) -> Option<usize> {
        if !self.focused {
            return None;
        }
        let c = &self.master.cursor;
        if !(c.visible && c.blinking && c.pos.is_some()) {
            return None;
        }
        let id = self.selected?;
        self.panes.iter().position(|p| p.id == id && p.visible() && p.follows(self.alt_screen))
    }

    /// 点滅のタイマーを要るときだけ動かす（T-035）。カーソルが動いたら点けてやり直す。描く前に呼ぶ
    /// 大きな画像の変換が別のスレッドで終わった。画像を置いている見えているペインを描き直す（T-105）
    fn image_converted(&mut self) {
        self.image_ready_count += 1;
        let master_images = !self.master.images.is_empty();
        for p in &mut self.panes {
            let has = match &p.scrolled {
                Some(s) if !matches!(p.position, Position::Follow) => !s.frame.images.is_empty(),
                _ => master_images,
            };
            if p.visible() && has {
                p.needs_draw = true;
            }
        }
        self.refresh(false);
    }

    /// アニメーションの次のフレームの時刻にタイマーを掛ける（`None` なら止める。T-092）
    fn schedule_animation(&mut self, now_ms: u64, next: Option<u64>) {
        if next == self.animation_next {
            return;
        }
        self.animation_next = next;
        unsafe {
            match next {
                Some(t) => {
                    SetTimer(Some(self.control), TIMER_ANIMATION, (t.saturating_sub(now_ms)).clamp(10, 60_000) as u32, None);
                }
                None => {
                    let _ = KillTimer(Some(self.control), TIMER_ANIMATION);
                }
            }
        }
    }

    /// アニメーションのフレームの時刻になった。見えているペインを描き直す（refresh の中で次のフレームへ進め、次のタイマーを掛ける）
    fn animation_timer(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.control), TIMER_ANIMATION);
        }
        self.animation_next = None;
        self.animation_ticks += 1;
        // 画像を置いているペインだけ描き直す（画像が画面から消えても再生だけ続いていることがある）
        let master_images = !self.master.images.is_empty();
        for p in &mut self.panes {
            let has = match &p.scrolled {
                Some(s) if !matches!(p.position, Position::Follow) => !s.frame.images.is_empty(),
                _ => master_images,
            };
            if p.visible() && has {
                p.needs_draw = true;
                p.needs_sync = true;
            }
        }
        // 描かなくても refresh は呼ぶ（その中でアニメーションを進め、次のタイマーを掛ける）
        self.refresh(false);
    }

    /// 文字の点滅（SGR 5。T-112）のタイマーを、見えているペインに点滅の文字があるときだけ動かす
    fn update_text_blink(&mut self) {
        let any_visible = self.panes.iter().any(|p| p.visible());
        let want = any_visible
            && (self.master.has_blink() || self.panes.iter().any(|p| p.visible() && p.scrolled.as_ref().is_some_and(|s| s.frame.has_blink())));
        if want && !self.text_blink_timer {
            self.text_blink_timer = true;
            unsafe {
                SetTimer(Some(self.control), TIMER_TEXT_BLINK, TEXT_BLINK_MS, None);
            }
        } else if !want && self.text_blink_timer {
            self.text_blink_timer = false;
            unsafe {
                let _ = KillTimer(Some(self.control), TIMER_TEXT_BLINK);
            }
            if !self.text_blink_on {
                self.text_blink_on = true;
                for p in &mut self.panes {
                    p.needs_draw = true;
                }
            }
        }
    }

    /// 文字の点滅のタイマー: 見せる / 消すを切り替えて、点滅の文字があるペインを描き直す
    fn text_blink_tick(&mut self) {
        self.text_blink_on = !self.text_blink_on;
        let master_blink = self.master.has_blink();
        for p in &mut self.panes {
            let has = match &p.scrolled {
                Some(s) if !matches!(p.position, Position::Follow) => s.frame.has_blink(),
                _ => master_blink,
            };
            if p.visible() && has {
                p.needs_draw = true;
            }
        }
        self.refresh(false);
    }

    fn update_blink(&mut self) {
        let interval = unsafe { GetCaretBlinkTime() };
        // 0xFFFFFFFF（INFINITE）は「Windows の設定で点滅しない」
        let want = self.blink_target().is_some() && interval != u32::MAX && interval > 0;
        let pos = self.master.cursor.pos;
        if want {
            if !self.blink_timer || pos != self.blink_pos {
                if !self.blink_on {
                    self.mark_blink_pane_dirty();
                }
                self.blink_on = true;
                unsafe {
                    SetTimer(Some(self.control), TIMER_CURSOR_BLINK, interval, None);
                }
                self.blink_timer = true;
            }
        } else if self.blink_timer {
            unsafe {
                let _ = KillTimer(Some(self.control), TIMER_CURSOR_BLINK);
            }
            self.blink_timer = false;
            if !self.blink_on {
                self.blink_on = true;
                self.mark_blink_pane_dirty();
            }
        }
        self.blink_pos = pos;
    }

    /// 選んでいるペインを描き直す印を付ける（点滅で消えていたカーソルを点けるとき）
    fn mark_blink_pane_dirty(&mut self) {
        if let Some(id) = self.selected
            && let Some(p) = self.pane_mut(id)
        {
            p.needs_draw = true;
        }
    }

    /// 点滅のタイマー。カーソルのセルだけを描き直す
    fn blink_tick(&mut self) {
        self.check_views();
        let Some(i) = self.blink_target().filter(|_| self.blink_timer) else {
            self.update_blink();
            return;
        };
        self.blink_on = !self.blink_on;
        self.blink_ticks += 1;
        if self.panes[i].needs_draw {
            self.draw_pane(i, None);
            return;
        }
        let view = self.follow_view(i);
        let focused = self.focused && self.selected == Some(self.panes[i].id);
        let pane = &mut self.panes[i];
        let r = match pane.view.as_mut() {
            Some(v) if unsafe { IsWindowVisible(v.hwnd) }.as_bool() => {
                if v.renderer.try_acquire_frame() {
                    v.renderer.draw_blink(&self.master, &view, focused)
                } else {
                    // フレームを受け付けるまで後回し（全体を描き直す）
                    pane.needs_draw = true;
                    pane.waiting_frame = true;
                    Ok(())
                }
            }
            _ => Ok(()),
        };
        if let Err(e) = r {
            log(&format!("点滅の描画に失敗（デバイスを作り直した）: {e}"));
            self.rebuild_device();
        }
    }

    /// 下を追っているペイン `i` に描くビュー（端末の下の端をペインの下の端に揃える）
    fn follow_view(&self, i: usize) -> FrameView {
        let p = &self.panes[i];
        let (cols, rows) = p.grid;
        let focused = self.focused && self.selected == Some(p.id);
        // 点滅で消えているときはカーソルを描かない（フォーカスの無いペインは枠のまま点滅しない）
        let show_cursor = !(focused && self.blink_timer && !self.blink_on);
        FrameView {
            top: self.master.rows as i32 - rows as i32,
            cols,
            rows,
            show_cursor,
            cursor_row: true,
            hover: p.hover,
            blink_off: !self.text_blink_on,
        }
    }

    /// ペインの今の中身を描く
    fn draw_pane(&mut self, i: usize, capture: Option<&std::path::Path>) -> bool {
        let alt = self.alt_screen;
        let text_blink_on = self.text_blink_on;
        let focused = self.focused && self.selected == Some(self.panes[i].id);
        let mut lost_device = false;
        let result = {
            let pane = &mut self.panes[i];
            let follows = pane.follows(alt);
            let Some(v) = pane.view.as_mut() else { return false };
            if !unsafe { IsWindowVisible(v.hwnd) }.as_bool() && capture.is_none() {
                return false;
            }
            // 次のフレームを受け付けるまで描かない（Present で UI スレッドを止めない。T-068）。画面の保存はすぐ描く
            if capture.is_none() && !v.renderer.try_acquire_frame() {
                pane.needs_draw = true;
                pane.waiting_frame = true;
                return false;
            }
            pane.waiting_frame = false;
            let (cols, rows) = pane.grid;
            let result = if follows {
                // 追っているペイン: 端末の下の端をペインの下の端に揃える（上に足りなければ空き）
                let show_cursor = !(focused && self.blink_timer && !self.blink_on);
                let view = FrameView {
                    top: self.master.rows as i32 - rows as i32,
                    cols,
                    rows,
                    show_cursor,
                    cursor_row: true,
                    hover: pane.hover,
                    blink_off: !text_blink_on,
                };
                v.renderer.draw_view(&self.master, &view, focused, capture)
            } else if let Some(s) = &pane.scrolled {
                let view =
                    FrameView { top: s.top, cols, rows, show_cursor: false, cursor_row: false, hover: pane.hover, blink_off: !text_blink_on };
                v.renderer.draw_view(&s.frame, &view, focused, capture)
            } else {
                Ok(())
            };
            if result.is_ok() {
                pane.needs_draw = false;
                self.perf.draw.add(v.renderer.stats.last_draw);
                self.perf.present.add(v.renderer.stats.last_present);
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

    /// デバイスが失われた（描いたペインの Renderer が分け合うデバイスを捨てて作り直した）。この端末の全ペインを新しいデバイスに
    /// 載せ替え、ほかの端末にも知らせる（T-067。向こうは描く前にも世代の違いで気づく）
    fn rebuild_device(&mut self) {
        self.follow_device();
        // 知らせるのは新しい世代ごとに 1 回だけ（作り直しに失敗し続けても、端末どうしで知らせ合い続けない）
        static LAST_BROADCAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let g = et_render::current_generation();
        if g != 0 && LAST_BROADCAST.swap(g, Ordering::AcqRel) != g {
            broadcast_device_changed(self.control);
        }
    }

    /// 分け合うデバイスが替わっていたら、全ペインを載せ替えて描き直す
    fn follow_device(&mut self) {
        for p in &mut self.panes {
            if let Some(v) = &mut p.view {
                match v.renderer.follow_shared_device() {
                    Ok(true) => {
                        log(&format!("ペイン {} を GPU デバイスの世代 {} に載せ替えた", p.id, v.renderer.gpu_generation()));
                        p.needs_draw = true;
                    }
                    Ok(false) => {}
                    Err(e) => log(&format!("GPU デバイスを作り直せません: {e}")),
                }
            }
        }
    }

    /// いま描けるペインがあるか（見えていて、フレームの受け付けを待っていない）
    fn can_draw_now(&self) -> bool {
        self.panes.iter().any(|p| p.visible() && !p.waiting_frame)
    }

    /// フレームを受け付けるのを待っているペインの（番号, ハンドル）。メッセージのループが一緒に待つ（T-068）
    fn waiting_frames(&self) -> Vec<(i64, HANDLE)> {
        self.panes
            .iter()
            .filter(|p| p.waiting_frame && p.needs_draw && p.visible())
            .filter_map(|p| p.view.as_ref().map(|v| (p.id, v.renderer.frame_waitable())))
            .collect()
    }

    /// ペイン `id` のフレームを受け付けると知らされた（`timed_out` なら知らせが来ないまま時間切れで、受け付けを待たずに描く）
    fn frame_signaled(&mut self, id: i64, timed_out: bool) {
        if let Some(p) = self.pane_mut(id) {
            p.waiting_frame = false;
            if let Some(v) = &mut p.view {
                v.renderer.set_frame_acquired();
            }
            if timed_out {
                log(&format!("ペイン {id}: フレームの受け付けの知らせが来ないので待たずに描く"));
            }
        }
        self.refresh(false);
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
        if s.cursor != rect {
            s.cursor = rect;
            drop(s);
            // カーソルが動いた（IME の変換ウィンドウを合わせ直してもらう。T-104）
            crate::notify::notify();
        }
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
        {
            let mut p = self.shared.probe.lock().unwrap_or_else(|e| e.into_inner());
            if p.sent.is_some() && p.ui.is_none() {
                p.ui = Some(Instant::now());
            }
        }
        // 打ったらカーソルを点けて点滅をやり直す（次の update_blink でタイマーを掛け直す）
        if self.blink_timer && !self.blink_on {
            self.blink_on = true;
            self.mark_blink_pane_dirty();
        }
        self.blink_pos = None;
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
        // プロンプトへ（T-119）: 行に直してから下の「行へ」と同じに扱う。起点はペインの一番上の行（Ghostty の
        // jump_to_prompt と同じ）。最後の 1 画面の中のプロンプトは一番上にできない（下の端を越える）ので、上へは画面の
        // 外のプロンプトへ、下へは最後の画面に入ればいちばん下へ戻る
        let amount = match amount {
            ScrollAmount::Prompt(n) if n != 0 => {
                let alt = session.lock().is_alternate_screen();
                let Some(p) = self.panes.iter().find(|p| p.id == id) else { return };
                let mut row = p.top_row(&session.lock(), alt);
                let mut found = None;
                for _ in 0..n.unsigned_abs() {
                    match session.find_prompt(row, n < 0) {
                        Some(r) => {
                            row = r;
                            found = Some(r);
                        }
                        None => break,
                    }
                }
                match found {
                    Some(r) => ScrollAmount::Lines(r - p.top_row(&session.lock(), alt)),
                    // 下にもうプロンプトが無ければいちばん下へ。上に無ければ動かない
                    None if n > 0 => ScrollAmount::Bottom,
                    None => return,
                }
            }
            other => other,
        };
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
                ScrollAmount::Wheel(_) | ScrollAmount::Prompt(_) => cur,
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
            *self.shared.copy.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(CopyText { text: String::new(), sent: 0, top: 0, point: 0, wraps: Vec::new() });
            self.shared.emit(Event::CopyReady);
            return;
        };
        let t0 = Instant::now();
        let (text, top, point, wraps) = {
            let term = s.lock();
            let alt = term.is_alternate_screen();
            let text = term.format_plain(false, false).ok().flatten().unwrap_or_default();
            // 折り返した行（T-086）。折り返す行はほぼ端まで埋まっているので、短い行は端末に尋ねない
            let (cols, _) = term.size();
            let wraps: Vec<i64> = text
                .split('\n')
                .enumerate()
                .filter(|(_, l)| l.chars().count() * 2 + 2 >= cols as usize)
                .filter(|(i, _)| term.row_wraps(*i as u32))
                .map(|(i, _)| i as i64)
                .collect();
            let total = term.total_rows() as i64;
            let (_, rows) = term.size();
            let cursor_row = total - rows as i64 + term.cursor().1 as i64;
            let id = pane.or(self.active);
            match id.and_then(|id| self.panes.iter().find(|p| p.id == id)) {
                Some(p) if !p.follows(alt) => {
                    let t = p.top_row(&term, alt).max(0);
                    (text, t, t, wraps)
                }
                Some(p) => (text, (total - p.grid.1 as i64).max(0), cursor_row, wraps),
                None => (text, (total - rows as i64).max(0), cursor_row, wraps),
            }
        };
        log(&format!("copy モードのテキスト: {} バイト、折り返し {} 行、{} ms", text.len(), wraps.len(), t0.elapsed().as_millis()));
        *self.shared.copy.lock().unwrap_or_else(|e| e.into_inner()) = Some(CopyText { text, sent: 0, top, point, wraps });
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

    /// マウスの下のハイパーリンクを調べ直す（T-101）。`client` が `None` ならペインから出た。
    /// リンクの上なら、同じ URI が続く桁の範囲（同じ行の中）に下線を引き、URI が変わったら Lisp に知らせる
    fn update_hover(&mut self, id: i64, client: Option<(i32, i32)>) {
        let found = client.and_then(|c| {
            let cell = self.pane_cell(id, c)?;
            let s = self.session.as_ref()?;
            let term = s.lock();
            let alt = term.is_alternate_screen();
            let p = self.panes.iter().find(|p| p.id == id)?;
            let uri_at = |x: u16| Self::pane_ref(p, &term, (x, cell.1), alt).and_then(|r| term.hyperlink_uri(r));
            let uri = uri_at(cell.0)?;
            let mut start = cell.0;
            while start > 0 && uri_at(start - 1).as_deref() == Some(uri.as_str()) {
                start -= 1;
            }
            let mut end = cell.0 + 1;
            while end < p.grid.0 && uri_at(end).as_deref() == Some(uri.as_str()) {
                end += 1;
            }
            Some((uri, (cell.1, start, end)))
        });
        let span = found.as_ref().map(|f| f.1);
        let mut changed = false;
        for p in &mut self.panes {
            let want = if p.id == id { span } else { None };
            if p.hover != want {
                p.hover = want;
                p.needs_draw = true;
                changed = true;
            }
        }
        let uri = found.map(|f| f.0);
        if uri != self.hover_uri {
            self.hover_uri = uri.clone();
            self.shared.emit(Event::Hover(uri));
        }
        if changed {
            self.refresh(false);
        }
    }

    /// ペインのクライアント座標のセルのハイパーリンク（OSC 8）の URI（T-039）
    fn link_at(&self, id: i64, client: (i32, i32)) -> Option<String> {
        let cell = self.pane_cell(id, client)?;
        let s = self.session.as_ref()?;
        let term = s.lock();
        let alt = term.is_alternate_screen();
        let p = self.panes.iter().find(|p| p.id == id)?;
        let at = Self::pane_ref(p, &term, cell, alt)?;
        term.hyperlink_uri(at)
    }

    fn lbutton_down(&mut self, id: i64, hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
        let client = point_of(lparam);
        let mods = mods_of(wparam);
        // Ctrl+クリックしたのがハイパーリンクなら、開くのは Lisp に任せる（選択もアプリへの報告もしない）
        if mods.ctrl
            && let Some(uri) = self.link_at(id, client)
        {
            log(&format!("リンクを Ctrl+クリック: {uri}"));
            self.shared.emit(Event::Link(uri));
            return;
        }
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
        // マウスの下のリンク（T-101）。ペインから出たら WM_MOUSELEAVE で消す。アプリへ報告している間は Ctrl を押したときだけ
        if let Some(p) = self.panes.iter_mut().find(|p| p.id == id)
            && !p.tracking_leave
            && let Some(v) = &p.view
        {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: v.hwnd,
                dwHoverTime: 0,
            };
            p.tracking_leave = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        }
        let hover_ok = self.drag.is_none() && (mods.ctrl || !self.should_report(id, mods.shift));
        self.update_hover(id, hover_ok.then_some(client));
        match self.drag {
            Some(Drag::Selecting(p)) if p == id => self.drag_select(id, client),
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

    /// 選択のドラッグを `client` まで伸ばす。ペインの上下の外なら自動スクロールを始める（T-084）
    fn drag_select(&mut self, id: i64, client: (i32, i32)) {
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
        if client.1 < 0 || client.1 >= h as i32 {
            if self.autoscroll.is_none() {
                unsafe { SetTimer(Some(self.control), TIMER_AUTOSCROLL, AUTOSCROLL_MS, None) };
            }
            self.autoscroll = Some((id, client));
        } else {
            self.stop_autoscroll();
        }
        self.mark_selection_changed();
        self.refresh(false);
    }

    fn stop_autoscroll(&mut self) {
        if self.autoscroll.take().is_some() {
            unsafe {
                let _ = KillTimer(Some(self.control), TIMER_AUTOSCROLL);
            }
        }
    }

    /// 自動スクロールのタイマー: ペインを外へ出た向きにスクロールし、端の行までドラッグを伸ばし直す。
    /// 1 回に動かす行数は、外へ出た距離（セルの高さ 2 つごとに 1 行増やす。最大 5 行）
    fn autoscroll_tick(&mut self) {
        let Some((id, client)) = self.autoscroll else {
            self.stop_autoscroll();
            return;
        };
        if !matches!(self.drag, Some(Drag::Selecting(p)) if p == id) {
            self.stop_autoscroll();
            return;
        }
        let Some((ch, h)) = self.panes.iter().find(|p| p.id == id).and_then(|p| {
            let v = p.view.as_ref()?;
            Some((v.renderer.cell_size().1.max(1) as i32, client_size(v.hwnd).1 as i32))
        }) else {
            self.stop_autoscroll();
            return;
        };
        let (dir, dist) = if client.1 < 0 { (-1, -client.1) } else { (1, client.1 - h + 1) };
        let lines = (1 + dist / (ch * 2)).clamp(1, 5) as i64;
        self.scroll_pane(id, ScrollAmount::Lines(dir * lines));
        self.drag_select(id, client);
    }

    fn lbutton_up(&mut self, id: i64, wparam: WPARAM, lparam: LPARAM) {
        let client = point_of(lparam);
        let mods = mods_of(wparam);
        match self.drag.take() {
            Some(Drag::Selecting(p)) if p == id => unsafe {
                self.stop_autoscroll();
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
                // マウスの形（T-038）: テキストの上は I ビーム、アプリへ報告しているときは矢印、
                // ハイパーリンク（OSC 8）の上は手（報告しているときは Ctrl を押している間だけ。T-039）
                if (lparam.0 & 0xffff) as u32 == HTCLIENT {
                    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                    let report = self.should_report(id, shift);
                    let link = self.drag.is_none() && (!report || ctrl) && {
                        let mut pt = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut pt);
                            let _ = ScreenToClient(hwnd, &mut pt);
                        }
                        self.link_at(id, (pt.x, pt.y)).is_some()
                    };
                    let name = if link {
                        IDC_HAND
                    } else if report {
                        IDC_ARROW
                    } else {
                        IDC_IBEAM
                    };
                    unsafe {
                        SetCursor(LoadCursorW(None, name).ok());
                    }
                    return Some(LRESULT(1));
                }
                return None;
            }
            WM_MOUSEMOVE => self.mouse_move(id, wparam, lparam),
            WM_MOUSELEAVE => {
                if let Some(p) = self.pane_mut(id) {
                    p.tracking_leave = false;
                }
                self.update_hover(id, None);
            }
            WM_LBUTTONUP => self.lbutton_up(id, wparam, lparam),
            WM_CAPTURECHANGED => {
                if let Some(Drag::Selecting(p)) = self.drag
                    && p == id
                {
                    // ボタンを離さないまま取り上げられた（フォーカスの変化など）。選択を確定して終える
                    self.drag = None;
                    self.stop_autoscroll();
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
            // 描けるペインが無い（見えていない・どれもフレームの受け付けを待っている）なら写さない。端末の「知らせを出した」印は
            // 立てたままにして、読みのスレッドがもう知らせてこないようにする（ロックの取り合いと CPU を減らす。T-068）。
            // 次に描けるようになったとき（フレームの受け付け・Place）の refresh がまとめて写す
            if a.can_draw_now() {
                a.refresh(false);
            }
        })
        .map(|_| LRESULT(0)),
        WM_APP_SESSION => with_app(|a| a.session_changed()).map(|_| LRESULT(0)),
        WM_APP_IMAGE => with_app(|a| a.image_converted()).map(|_| LRESULT(0)),
        WM_APP_DEVICE => with_app(|a| {
            a.follow_device();
            a.refresh(true);
        })
        .map(|_| LRESULT(0)),
        WM_TIMER if wparam.0 == TIMER_RENDER_HOLD => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_RENDER_HOLD);
            }
            with_app(|a| a.refresh(false)).map(|_| LRESULT(0))
        }
        WM_TIMER if wparam.0 == TIMER_CURSOR_BLINK => with_app(|a| a.blink_tick()).map(|_| LRESULT(0)),
        WM_TIMER if wparam.0 == TIMER_AUTOSCROLL => with_app(|a| a.autoscroll_tick()).map(|_| LRESULT(0)),
        WM_TIMER if wparam.0 == TIMER_TEXT_BLINK => with_app(|a| a.text_blink_tick()).map(|_| LRESULT(0)),
        WM_TIMER if wparam.0 == TIMER_ANIMATION => with_app(|a| a.animation_timer()).map(|_| LRESULT(0)),
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
    // クリックされてもフォーカスを取らない（キーは Emacs が受ける）。App を見る前に返す。
    // ただしフレームが前面にないときは MA_ACTIVATE を返し、OS にトップレベルのフレームを前面にさせる
    // （他のアプリから戻るクリックで Emacs にフォーカスが入らない問題への対処）。
    // 親（Emacs の GUI スレッド）へ同期で送らないよう、DefWindowProc には回さない
    if msg == WM_MOUSEACTIVATE {
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        let active = if unsafe { GetForegroundWindow() } == root { MA_NOACTIVATE } else { MA_ACTIVATE };
        return LRESULT(active as isize);
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
