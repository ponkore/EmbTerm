//! et-session: 端末 1 つ分の VT と PTY のつなぎ（T-075。スレッドの構成は docs/TODO.md T-006 の初版）。
//!
//! ```text
//! PTY の読みのスレッド ── 読む → 端末をロックして vt_write → ロックを外す
//!                         └ 描き直しの知らせ（AtomicBool でまとめて 1 回だけ）→ notify(Event::Output)
//! 端末の応答（WRITE_PTY）── 書きのスレッドのチャネルに積むだけ（ロックを持ったまま PTY に書かない）
//! UI スレッド ── take_output() → 端末をロック → render state を update → ロックを外して描く
//! 閉じる ── 別のスレッド（session-close）で Pty::close を待ち、終わったら notify(Event::Closed)（T-076）
//! ```
//!
//! - HWND には依存しない（知らせ方は `notify` に任せる）。embterm.exe と Emacs module の両方が使う
//! - **端末の状態（[`Session`]）は表示（HWND）より長生きにする**（T-066）。メソッドはすべて `&self` なので、
//!   `Arc<Session>` にして UI スレッドと呼び出し側（Lisp のスレッド）で分け合える
//! - キーの変換（レガシーの VT）は [`keys`]

pub mod keys;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use et_pty::{ConptyKind, ExitStatus, Pty, PtyOptions, PtyWriter};
use et_vt::{Effects, Mode, Rgb, Terminal};

/// 呼び出し側（UI）に知らせること
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// 端末の中身が変わった（描き直す）
    Output,
    /// タイトルが変わった
    Title,
    /// カレントディレクトリ（OSC 7 など）が変わった
    Pwd,
    /// ベル
    Bell,
    /// 子プロセスが終わった
    Exited,
    /// [`Session::close`] の後始末が終わった（PTY のスレッドも止まった）
    Closed,
}

/// 知らせを受ける関数。**PTY の読みのスレッドや閉じるスレッドから呼ばれる**（端末のロックを持ったままのこともある）。
/// `PostMessage` するか、フラグを立てるくらいにとどめる
pub type Notify = Arc<dyn Fn(Event) + Send + Sync>;

/// 端末を閉じるときに後始末を待つ時間（ClosePseudoConsole → EOF。過ぎたら Job ごと止める）
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

pub struct SessionOptions {
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
    pub cell_px: (u32, u32),
    pub conpty_search: Vec<PathBuf>,
    pub foreground: Rgb,
    pub background: Rgb,
    pub scrollback_lines: usize,
}

/// 端末からの知らせを受けるもの（読みのスレッドで、端末のロックを持ったまま呼ばれる）
struct TermEffects {
    writer: Arc<OnceLock<PtyWriter>>,
    title: Arc<Mutex<String>>,
    pwd: Arc<Mutex<String>>,
    notify: Notify,
}

impl Effects for TermEffects {
    fn write_pty(&mut self, data: &[u8]) {
        // 書きのスレッドのチャネルに積むだけ
        if let Some(w) = self.writer.get() {
            w.write(data);
        }
    }
    fn bell(&mut self) {
        (self.notify)(Event::Bell);
    }
    fn title_changed(&mut self, title: &str) {
        *self.title.lock().unwrap_or_else(|e| e.into_inner()) = title.to_string();
        (self.notify)(Event::Title);
    }
    fn pwd_changed(&mut self, pwd: &str) {
        *self.pwd.lock().unwrap_or_else(|e| e.into_inner()) = pwd.to_string();
        (self.notify)(Event::Pwd);
    }
}

/// 読みのスレッドと UI スレッドで共有するもの
struct Shared {
    term: Mutex<Terminal>,
    /// 描き直しの知らせを出したまま、UI がまだ受け取っていない
    pending: AtomicBool,
    /// 起動直後の出力に `CSI c`（DA1 の要求）が来たか。来れば再配布版の ConPTY（docs/research/conpty.md §2.4）
    saw_da1: AtomicBool,
    bytes_seen: AtomicUsize,
    exit: Mutex<Option<ExitStatus>>,
}

/// 閉じる処理の待ち合わせ。待たなくてよい（終わると [`Event::Closed`] も届く）
pub struct Closing {
    done: Option<Receiver<()>>,
}

impl Closing {
    /// 後始末が終わるまで最大 `timeout` 待つ。終わっていれば true
    pub fn wait(&self, timeout: Duration) -> bool {
        match &self.done {
            None => true,
            Some(rx) => !matches!(rx.recv_timeout(timeout), Err(mpsc::RecvTimeoutError::Timeout)),
        }
    }
}

/// 端末 1 つ（VT + PTY + 読み書きのスレッド）
pub struct Session {
    shared: Arc<Shared>,
    title: Arc<Mutex<String>>,
    pwd: Arc<Mutex<String>>,
    writer: PtyWriter,
    /// 閉じたら `None`。閉じる処理は別のスレッドへ持っていく
    pty: Mutex<Option<Pty>>,
    kind: ConptyKind,
    pid: u32,
    warnings: Vec<String>,
    notify: Notify,
}

impl Session {
    pub fn start(opts: SessionOptions, notify: Notify) -> Result<Session, String> {
        let writer_cell: Arc<OnceLock<PtyWriter>> = Arc::new(OnceLock::new());
        let title = Arc::new(Mutex::new(String::new()));
        let pwd = Arc::new(Mutex::new(String::new()));
        let effects = TermEffects { writer: writer_cell.clone(), title: title.clone(), pwd: pwd.clone(), notify: notify.clone() };
        let mut term = Terminal::new(opts.cols, opts.rows, Box::new(effects)).map_err(|e| e.to_string())?;
        // ConPTY は自分の画面（スクロールバック無し）を持つので、リサイズで行をスクロールバックから戻さない
        term.set_resize_pull_scrollback(false).map_err(|e| e.to_string())?;
        // 書記素クラスタで幅を数える（再配布版の ConPTY の既定と揃える。ghostel と同じ）
        term.set_mode(Mode::GRAPHEME_CLUSTER, true, true).map_err(|e| e.to_string())?;
        term.set_default_colors(opts.foreground, opts.background, None).map_err(|e| e.to_string())?;
        term.set_terminfo_name("xterm-256color").map_err(|e| e.to_string())?;
        term.set_scrollback_max_lines(opts.scrollback_lines).map_err(|e| e.to_string())?;
        term.resize(opts.cols, opts.rows, opts.cell_px.0, opts.cell_px.1).map_err(|e| e.to_string())?;

        let shared = Arc::new(Shared {
            term: Mutex::new(term),
            pending: AtomicBool::new(false),
            saw_da1: AtomicBool::new(false),
            bytes_seen: AtomicUsize::new(0),
            exit: Mutex::new(None),
        });

        let pty_opts = PtyOptions {
            argv: if opts.argv.is_empty() { vec!["cmd.exe".into()] } else { opts.argv },
            cwd: opts.cwd,
            env: et_pty::terminal_env(),
            cols: opts.cols,
            rows: opts.rows,
            conpty_search: opts.conpty_search,
            flags: 0,
        };
        let on_output = {
            let (shared, notify, ready) = (shared.clone(), notify.clone(), writer_cell.clone());
            move |data: &[u8]| {
                // 書く口ができるまで待つ（端末の応答を落とさないため。起動の直後に 1 回だけ待つ）
                ready.wait();
                if shared.bytes_seen.fetch_add(data.len(), Ordering::Relaxed) < 64 * 1024
                    && data.windows(3).any(|w| w == b"\x1b[c")
                {
                    shared.saw_da1.store(true, Ordering::Relaxed);
                }
                shared.term.lock().unwrap_or_else(|e| e.into_inner()).vt_write(data);
                if !shared.pending.swap(true, Ordering::AcqRel) {
                    notify(Event::Output);
                }
            }
        };
        let on_exit = {
            let (shared, notify) = (shared.clone(), notify.clone());
            move |status: ExitStatus| {
                *shared.exit.lock().unwrap_or_else(|e| e.into_inner()) = Some(status);
                notify(Event::Exited);
            }
        };
        let pty = Pty::spawn(&pty_opts, on_output, on_exit).map_err(|e| format!("子プロセスを起動できません: {e}"))?;
        let writer = pty.writer();
        let _ = writer_cell.set(writer.clone());
        let kind = pty.kind().clone();
        let pid = pty.pid();
        let warnings = pty.warnings().to_vec();
        Ok(Session { shared, title, pwd, writer, pty: Mutex::new(Some(pty)), kind, pid, warnings, notify })
    }

    /// 描き直しの知らせを受け取った（次の出力でまた知らせが来るようにする）。端末をロックする前に呼ぶ
    pub fn take_output(&self) {
        self.shared.pending.store(false, Ordering::Release);
    }

    /// 端末をロックする。持っている間は読みのスレッドが止まるので、短くすること
    pub fn lock(&self) -> MutexGuard<'_, Terminal> {
        self.shared.term.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// PTY に書く（待たない）
    pub fn write(&self, data: &[u8]) {
        self.writer.write(data);
    }

    /// 端末と ConPTY の両方の大きさを変える（ghostel と同じく端末が先）
    pub fn resize(&self, cols: u16, rows: u16, cell_px: (u32, u32)) {
        let _ = self.lock().resize(cols, rows, cell_px.0, cell_px.1);
        if let Some(p) = self.pty.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = p.resize(cols, rows);
        }
    }

    /// 端末の桁数と行数
    pub fn size(&self) -> (u16, u16) {
        self.lock().size()
    }

    pub fn title(&self) -> String {
        self.title.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn pwd(&self) -> String {
        self.pwd.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn exit_status(&self) -> Option<ExitStatus> {
        *self.shared.exit.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn saw_da1(&self) -> bool {
        self.shared.saw_da1.load(Ordering::Relaxed)
    }

    pub fn conpty_kind(&self) -> &ConptyKind {
        &self.kind
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// 閉じているか（[`Session::close`] を呼んだあと）
    pub fn is_closed(&self) -> bool {
        self.pty.lock().unwrap_or_else(|e| e.into_inner()).is_none()
    }

    /// 閉じる（T-076）。**待たない。** ClosePseudoConsole → EOF まで読む → 子が残っていれば Job ごと止める、を
    /// 別のスレッドで行い（時限 [`CLOSE_TIMEOUT`]）、終わったら [`Event::Closed`] を知らせる。
    /// 待ちたいときは戻り値の [`Closing::wait`]。2 回目以降は何もしない（すぐ終わった扱いの `Closing`）
    pub fn close(&self) -> Closing {
        let Some(mut pty) = self.pty.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return Closing { done: None };
        };
        let (tx, rx) = mpsc::channel();
        let notify = self.notify.clone();
        let spawned = std::thread::Builder::new().name("session-close".into()).spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pty.close(CLOSE_TIMEOUT)));
            drop(pty);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| notify(Event::Closed)));
            let _ = tx.send(());
        });
        match spawned {
            Ok(_) => Closing { done: Some(rx) },
            // スレッドを作れないときだけ、ここで待つ（Pty はクロージャごと捨てられて Drop で閉じる）
            Err(_) => Closing { done: None },
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // 待たない（UI スレッドや Emacs の GC から落とされても止まらないように）
        let _ = self.close();
    }
}
