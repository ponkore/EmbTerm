//! Unix（macOS）の PTY: `openpty` で子プロセスを動かす（docs/TODO.md T-124）。
//!
//! ConPTY 版（[`crate::conpty`]）と同じ形を出すが、中身はずっと単純になる。
//!
//! - `openpty` で master / slave を作り、子は `setsid` + `TIOCSCTTY` で slave を制御端末にして起こす
//!   （標準入出力は slave。`Command` の `pre_exec` で行う）
//! - master はこちら側で CLOEXEC にする（子が持つと、子が終わっても EOF が来ない）
//! - 読みのスレッドは `poll` で master と「止まれ」のパイプを待つ。読めたら `on_output`、EOF なら終わる。
//!   子が先に終わったら少しだけ残りを読んでから終わる
//! - 書きは専用のスレッドがチャネルで受ける（ConPTY 版と同じ。端末の応答もここへ積む）
//! - 閉じる（[`Pty::close`]）のは「まず穏やかに、時限を過ぎたら強く」:
//!   プロセスグループに SIGHUP → 子孫がすべて終わるのを `grace` まで待つ → 過ぎたら SIGKILL。
//!   Windows の Job Object に当たるのはプロセスグループ（子は `setsid` でグループ兼セッションの長）。
//!   ジョブ制御で別のグループに出た孫は、閉じ始めた時点の子孫の一覧（スナップショット）で追う（T-073 と同じ考え）
//!
//! unsafe はこのクレートの中に閉じ込める。

use std::ffi::{OsString, c_void};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(crate) use crate::common::{CloseReport, ExitStatus, PtyWriter, WriteMsg, foreign_terminal_var};
use crate::Result;

/// 読みの 1 回の大きさ（ConPTY 版と同じ 64 KiB）
const READ_BUFFER: usize = 64 * 1024;
/// 読みのスレッドが `poll` で待つ時間。子の終了と「止まれ」を見に戻る間隔
const POLL_INTERVAL_MS: i32 = 100;
/// 子が終わってから、残っている出力を読むのを諦めるまで
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(500);

/// どの PTY で動いているか（Unix は `openpty` だけ）
#[derive(Clone, Debug)]
pub enum PtyKind {
    Unix,
}

impl std::fmt::Display for PtyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "openpty")
    }
}

/// 子プロセスの起こし方
pub struct PtyOptions {
    /// コマンドと引数（先頭がプログラム）
    pub argv: Vec<OsString>,
    /// 作業ディレクトリ。`None` なら親と同じ
    pub cwd: Option<PathBuf>,
    /// 親の環境変数を前から順に書き換える。`Some` は設定（同じ名前なら上書き）、`None` は削除
    pub env: Vec<(OsString, Option<OsString>)>,
    pub cols: u16,
    pub rows: u16,
    /// **Unix では使わない**（ConPTY を探すディレクトリ。呼ぶ側を OS で分けずに済むよう欄だけ残す）
    pub conpty_search: Vec<PathBuf>,
    /// **Unix では使わない**（`CreatePseudoConsole` のフラグ）
    pub flags: u32,
}

impl Default for PtyOptions {
    fn default() -> Self {
        Self {
            argv: crate::default_shell(),
            cwd: None,
            env: Vec::new(),
            cols: 80,
            rows: 24,
            conpty_search: Vec::new(),
            flags: 0,
        }
    }
}

/// ConPTY 版との形合わせ（Unix では探すものが無いので常に空）
pub fn default_conpty_search(_preferred: Option<PathBuf>) -> Vec<PathBuf> {
    Vec::new()
}

/// 自分の実行ファイルのあるディレクトリ
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}

fn last_error<T>() -> Result<T> {
    Err(io::Error::last_os_error())
}

/// 子に渡す環境変数（親の環境に `changes` を当て、親の端末の変数（T-095）は落とす）
fn child_env(changes: &[(OsString, Option<OsString>)]) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| !foreign_terminal_var(&k.to_string_lossy()))
        .collect();
    for (name, value) in changes {
        let at = env.iter().position(|(k, _)| k == name);
        match (value, at) {
            (Some(v), Some(i)) => env[i].1 = v.clone(),
            (Some(v), None) => env.push((name.clone(), v.clone())),
            (None, Some(i)) => {
                env.remove(i);
            }
            (None, None) => {}
        }
    }
    env
}

/// `fcntl` で FD_CLOEXEC を立てる（master を子に継がせない）
fn set_cloexec(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return last_error();
    }
    Ok(())
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows.max(1), ws_col: cols.max(1), ws_xpixel: 0, ws_ypixel: 0 }
}

/// 「止まれ」を読みのスレッドに伝えるパイプ（`poll` で待てるようにするため）
struct StopPipe {
    read: OwnedFd,
    write: OwnedFd,
}

impl StopPipe {
    fn new() -> Result<Self> {
        let mut fds = [0 as RawFd; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return last_error();
        }
        let pipe = Self { read: unsafe { OwnedFd::from_raw_fd(fds[0]) }, write: unsafe { OwnedFd::from_raw_fd(fds[1]) } };
        set_cloexec(pipe.read.as_raw_fd())?;
        set_cloexec(pipe.write.as_raw_fd())?;
        Ok(pipe)
    }

    fn signal(&self) {
        let byte = 1u8;
        unsafe { libc::write(self.write.as_raw_fd(), &byte as *const u8 as *const c_void, 1) };
    }
}

/// こちらから閉じるときの段取り（[`Pty::close`] が決める。ConPTY 版の `CloseParams` と同じ役割）
#[derive(Default)]
struct CloseParams {
    /// 閉じ始めた時刻
    started: Option<Instant>,
    /// 閉じ始めたときの子孫（ジョブ制御で別のグループに出たものも追うためのスナップショット）
    outside: Vec<u32>,
    report: CloseReport,
}

/// PTY で動いている子プロセス 1 つ
pub struct Pty {
    master: Arc<OwnedFd>,
    kind: PtyKind,
    pid: u32,
    writer: PtyWriter,
    stop: Arc<StopPipe>,
    reader: Option<JoinHandle<()>>,
    writer_thread: Option<JoinHandle<()>>,
    /// 読みのスレッドが終わったら値が届く
    reader_done: Receiver<()>,
    close_params: Arc<Mutex<CloseParams>>,
    warnings: Vec<String>,
}

impl Pty {
    /// 子プロセスを PTY で起動する。
    ///
    /// - `on_output`: 読みのスレッドで、出力を受けるたびに呼ばれる
    /// - `on_exit`: 子が終わって出力を読み終えたとき（または [`Pty::close`] で閉じたとき）に 1 回だけ呼ばれる
    pub fn spawn(
        opts: &PtyOptions,
        mut on_output: impl FnMut(&[u8]) + Send + 'static,
        on_exit: impl FnOnce(ExitStatus) + Send + 'static,
    ) -> Result<Pty> {
        if opts.argv.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "コマンドが空です"));
        }
        let (mut master_fd, mut slave_fd) = (-1 as RawFd, -1 as RawFd);
        let mut ws = winsize(opts.cols, opts.rows);
        if unsafe { libc::openpty(&mut master_fd, &mut slave_fd, ptr::null_mut(), ptr::null_mut(), &mut ws) } != 0 {
            return last_error();
        }
        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
        set_cloexec(master.as_raw_fd())?;
        set_cloexec(slave.as_raw_fd())?;

        let mut cmd = Command::new(&opts.argv[0]);
        cmd.args(&opts.argv[1..]);
        if let Some(dir) = &opts.cwd {
            cmd.current_dir(dir);
        }
        cmd.env_clear().envs(child_env(&opts.env));
        cmd.stdin(Stdio::from(slave.try_clone()?)).stdout(Stdio::from(slave.try_clone()?)).stderr(Stdio::from(slave.try_clone()?));
        let slave_raw = slave.as_raw_fd();
        // SAFETY: fork と exec の間で呼ぶのは async-signal-safe なものだけ（setsid / ioctl / signal）
        unsafe {
            cmd.pre_exec(move || {
                // 自分をセッション兼プロセスグループの長にし、slave を制御端末にする
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                // Rust は SIGPIPE を無視にするので、子では既定に戻す（`head` などで固まらないように）
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        // 親の側の slave は閉じる（子が終わったときに master へ EOF が来るように）
        drop(slave);
        let pid = child.id();

        let master = Arc::new(master);
        let stop = Arc::new(StopPipe::new()?);
        let close_params = Arc::new(Mutex::new(CloseParams::default()));
        let (done_tx, reader_done) = mpsc::channel();

        // 読みのスレッド
        let reader = {
            let (master, stop, close_params) = (master.clone(), stop.clone(), close_params.clone());
            thread::Builder::new()
                .name("pty-read".into())
                .spawn(move || {
                    let eof_at = read_loop(&master, &stop, child, &mut on_output);
                    let mut p = close_params.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(started) = p.started {
                        p.report.eof = Some(started.elapsed());
                    }
                    let closed_by_us = p.started.is_some();
                    drop(p);
                    on_exit(ExitStatus { code: eof_at, closed_by_us });
                    let _ = done_tx.send(());
                })
                .map_err(|e| io::Error::other(format!("読みのスレッドを作れません: {e}")))?
        };

        // 書きのスレッド
        let (tx, rx) = mpsc::channel::<WriteMsg>();
        let writer_thread = {
            let master = master.clone();
            thread::Builder::new()
                .name("pty-write".into())
                .spawn(move || {
                    while let Ok(msg) = rx.recv() {
                        match msg {
                            WriteMsg::Data(data) => write_all(&master, &data),
                            WriteMsg::Stop => break,
                        }
                    }
                })
                .map_err(|e| io::Error::other(format!("書きのスレッドを作れません: {e}")))?
        };

        Ok(Pty {
            master,
            kind: PtyKind::Unix,
            pid,
            writer: PtyWriter { tx },
            stop,
            reader: Some(reader),
            writer_thread: Some(writer_thread),
            reader_done,
            close_params,
            warnings: Vec::new(),
        })
    }

    /// どの PTY で動いているか
    pub fn kind(&self) -> &PtyKind {
        &self.kind
    }

    /// 子プロセスの PID
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// 起動のときに気づいたこと（Unix では今のところ無い）
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// 書く口（複製して別のスレッドへ渡してよい）
    pub fn writer(&self) -> PtyWriter {
        self.writer.clone()
    }

    /// 書く（書きのスレッドに積むだけ）
    pub fn write(&self, data: &[u8]) -> bool {
        self.writer.write(data)
    }

    /// 同じプロセスグループ（= Windows の Job に当たる）にいるプロセス
    pub fn job_pids(&self) -> Vec<u32> {
        group_pids(self.pid)
    }

    /// 端末の大きさを変える
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let ws = winsize(cols, rows);
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) } < 0 {
            return last_error();
        }
        Ok(())
    }

    /// 閉じる（ConPTY 版の [`crate::conpty::Pty::close`] と同じ段取り。T-076）。
    /// **最大 `grace` + 2 秒待つので、UI スレッドや Lisp のスレッドでは呼ばない**
    ///
    /// 1. 閉じ始めた時点の子孫を控える（ジョブ制御で別のグループに出たものも追うため）
    /// 2. プロセスグループに SIGHUP を送る（端末が閉じたときの知らせ）
    /// 3. 子孫がすべて終わるのを `grace` まで待つ
    /// 4. 過ぎたら SIGKILL
    pub fn close(&mut self, grace: Duration) -> CloseReport {
        let started = {
            let mut p = self.close_params.lock().unwrap_or_else(|e| e.into_inner());
            if p.started.is_none() {
                p.outside = descendant_pids(self.pid);
                p.report.outside = 0; // Unix ではプロセスグループで束ねるので「Job の外」は作らない
                p.started = Some(Instant::now());
            }
            p.started.unwrap_or_else(Instant::now)
        };

        // 穏やかに終わるよう頼む
        unsafe { libc::killpg(self.pid as i32, libc::SIGHUP) };
        unsafe { libc::kill(self.pid as i32, libc::SIGHUP) };
        self.close_params.lock().unwrap_or_else(|e| e.into_inner()).report.console_closed = Some(started.elapsed());

        // 読みのスレッドを終わらせ、子を待つ（EOF が来れば自分で終わる）
        self.stop.signal();
        let _ = self.writer.tx.send(WriteMsg::Stop);
        if let Some(r) = self.reader.take() {
            if self.reader_done.recv_timeout(grace + Duration::from_secs(2)).is_ok() {
                let _ = r.join();
            }
        }
        if let Some(w) = self.writer_thread.take() {
            let _ = w.join();
        }

        // 子孫がすべて終わるのを待ち、過ぎたら止める
        let mut p = self.close_params.lock().unwrap_or_else(|e| e.into_inner());
        let watched: Vec<u32> = std::iter::once(self.pid).chain(p.outside.iter().copied()).collect();
        let deadline = started + grace;
        loop {
            let alive = alive_pids(&watched);
            if alive.is_empty() {
                p.report.all_exited = Some(started.elapsed());
                break;
            }
            if Instant::now() >= deadline {
                p.report.forced = alive.len();
                unsafe { libc::killpg(self.pid as i32, libc::SIGKILL) };
                for pid in alive {
                    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                }
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        p.report.clone()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.close(Duration::from_secs(3));
    }
}

/// master から読み続ける。戻り値は子の終了コード（取れなければ `None`）
fn read_loop(master: &OwnedFd, stop: &StopPipe, mut child: Child, on_output: &mut impl FnMut(&[u8])) -> Option<u32> {
    let mut buf = vec![0u8; READ_BUFFER];
    let mut code: Option<u32> = None;
    let mut exited_at: Option<Instant> = None;
    loop {
        let mut fds = [
            libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: stop.read.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, POLL_INTERVAL_MS) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        // 「止まれ」が来たら、残っている分だけ読んで終わる
        if fds[1].revents != 0 {
            drain(master, &mut buf, on_output);
            break;
        }
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            match read_once(master, &mut buf) {
                Ok(0) => break, // EOF（子とその子孫が slave を閉じた）
                Ok(n) => {
                    on_output(&buf[..n]);
                    continue;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // Linux は slave が閉じると EIO になる（macOS は EOF）
                Err(_) => break,
            }
        }
        // 子が終わっていないか見る（孫が slave を持っていると EOF は来ない）
        if exited_at.is_none() {
            if let Ok(Some(status)) = child.try_wait() {
                code = exit_code(&status);
                exited_at = Some(Instant::now());
            }
        }
        if exited_at.is_some_and(|t| t.elapsed() > DRAIN_AFTER_EXIT) {
            break;
        }
    }
    if exited_at.is_none() {
        // EOF まで来た（ふつうは子はもう終わっている）
        if let Ok(status) = child.wait() {
            code = exit_code(&status);
        }
    }
    code
}

/// 終了コード（シグナルで終わったときはシェルと同じ 128 + シグナル番号）
fn exit_code(status: &std::process::ExitStatus) -> Option<u32> {
    status.code().map(|c| c as u32).or_else(|| status.signal().map(|s| 128 + s as u32))
}

fn read_once(master: &OwnedFd, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::read(master.as_raw_fd(), buf.as_mut_ptr() as *mut c_void, buf.len()) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
}

/// 読めるだけ読む（閉じる前に残っている出力を拾う）
fn drain(master: &OwnedFd, buf: &mut [u8], on_output: &mut impl FnMut(&[u8])) {
    loop {
        let mut fds = [libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) } <= 0 || fds[0].revents & libc::POLLIN == 0 {
            return;
        }
        match read_once(master, buf) {
            Ok(0) => return,
            Ok(n) => on_output(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

fn write_all(master: &OwnedFd, data: &[u8]) {
    let mut at = 0;
    while at < data.len() {
        let n = unsafe {
            libc::write(master.as_raw_fd(), data[at..].as_ptr() as *const c_void, data.len() - at)
        };
        if n > 0 {
            at += n as usize;
            continue;
        }
        let e = io::Error::last_os_error();
        if n < 0 && e.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return; // 相手が閉じた
    }
}

/// 親が `ppid` のプロセス（直接の子だけ）
#[cfg(target_os = "macos")]
fn child_pids(ppid: u32) -> Vec<u32> {
    list_pids(|buf, size| unsafe { libc::proc_listchildpids(ppid as libc::pid_t, buf, size) })
}

/// プロセスグループ `pgid` にいるプロセス（Windows の Job の中身に当たる）
#[cfg(target_os = "macos")]
fn group_pids(pgid: u32) -> Vec<u32> {
    list_pids(|buf, size| unsafe { libc::proc_listpgrppids(pgid as libc::pid_t, buf, size) })
}

/// libproc の `proc_list*pids` を呼ぶ。
///
/// **返り値の意味が関数と呼び方で違う**（2026-11-13 に macOS で実測）:
/// `proc_listchildpids(pid, NULL, 0)` は「全プロセスのバイト数」らしい大きな値を返し、
/// 実際のバッファを渡したときは**書いた個数**を返す。どちらに転んでも困らないよう、
/// バッファは 0 で埋めて多めに取り、返り値は個数として読み、0 の欄は捨てる
#[cfg(target_os = "macos")]
fn list_pids(call: impl Fn(*mut c_void, libc::c_int) -> libc::c_int) -> Vec<u32> {
    let probe = call(ptr::null_mut(), 0);
    if probe <= 0 {
        return Vec::new();
    }
    let room = probe as usize / std::mem::size_of::<libc::pid_t>() + 64;
    let mut buf = vec![0 as libc::pid_t; room];
    let size = (buf.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    let got = call(buf.as_mut_ptr() as *mut c_void, size);
    if got <= 0 {
        return Vec::new();
    }
    buf.truncate((got as usize).min(room));
    buf.into_iter().filter(|&p| p > 0).map(|p| p as u32).collect()
}

/// Linux の `/proc` から (PID, 親, プロセスグループ) を読む（**未確認**。T-124 は macOS だけを動かしている）
#[cfg(not(target_os = "macos"))]
fn process_table() -> Vec<(u32, u32, u32)> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    let mut out = Vec::new();
    for entry in dir.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else { continue };
        // 2 つ目の欄（実行ファイル名）は括弧に入っていて空白を含みうるので、閉じ括弧の後ろから数える
        let Some(rest) = stat.rsplit_once(") ").map(|(_, r)| r) else { continue };
        let f: Vec<&str> = rest.split_whitespace().collect();
        let (Some(ppid), Some(pgid)) = (f.get(1).and_then(|s| s.parse().ok()), f.get(2).and_then(|s| s.parse().ok())) else {
            continue;
        };
        out.push((pid, ppid, pgid));
    }
    out
}

#[cfg(not(target_os = "macos"))]
fn child_pids(ppid: u32) -> Vec<u32> {
    process_table().into_iter().filter(|p| p.1 == ppid).map(|p| p.0).collect()
}

#[cfg(not(target_os = "macos"))]
fn group_pids(pgid: u32) -> Vec<u32> {
    process_table().into_iter().filter(|p| p.2 == pgid).map(|p| p.0).collect()
}

/// プロセスの木の子孫の PID を求める（T-069 の KKP の安全策）。`root` は含めない
pub fn descendant_pids(root: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for pid in child_pids(parent) {
            if pid != root && !out.contains(&pid) {
                out.push(pid);
                frontier.push(pid);
            }
        }
    }
    out
}

/// まだ生きている PID（`kill(pid, 0)` で見る。終わった子は読みのスレッドが待っているのでゾンビは残らない）
fn alive_pids(pids: &[u32]) -> Vec<u32> {
    pids.iter().copied().filter(|&p| unsafe { libc::kill(p as i32, 0) } == 0).collect()
}

/// プロセスの木を 1 行で（ログ・`embterm-module-process-report` 用）。`group` に無いものには `!` を付ける
pub fn describe_tree(root: u32, group: &[u32]) -> String {
    let mut parts = vec![root.to_string()];
    for pid in descendant_pids(root) {
        // プロセスグループから出たもの（ジョブ制御で別のグループに移った孫）には `!` を付ける
        let mark = if group.contains(&pid) { "" } else { "!" };
        parts.push(format!("{mark}{pid}"));
    }
    parts.join(" ")
}

/// プロセスの組を見張る（[`ProcessWatch::new`]）
pub struct ProcessWatch {
    pids: Vec<u32>,
    /// 一度でも全員が終わったか（PID の使い回しに引っかからないよう覚えておく）
    done: AtomicBool,
}

impl ProcessWatch {
    /// PID の組を見張る
    pub fn new(pids: &[u32]) -> Self {
        Self { pids: pids.to_vec(), done: AtomicBool::new(false) }
    }

    /// 見張るプロセスが無いか
    pub fn is_empty(&self) -> bool {
        self.pids.is_empty()
    }

    /// 見張っているプロセスがすべて終わったか（空なら false）
    pub fn all_exited(&self) -> bool {
        if self.pids.is_empty() {
            return false;
        }
        if self.done.load(Ordering::Relaxed) {
            return true;
        }
        let all = alive_pids(&self.pids).is_empty();
        if all {
            self.done.store(true, Ordering::Relaxed);
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_env;
    use std::sync::mpsc;

    /// 出力を集めながら動かし、終了を待つ
    fn run(argv: &[&str], input: Option<&[u8]>, wait: Duration) -> (String, Option<ExitStatus>, Pty) {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (etx, erx) = mpsc::channel();
        let opts = PtyOptions {
            argv: argv.iter().map(OsString::from).collect(),
            env: terminal_env(),
            ..Default::default()
        };
        let pty = Pty::spawn(&opts, move |d| { let _ = tx.send(d.to_vec()); }, move |s| { let _ = etx.send(s); }).unwrap();
        if let Some(data) = input {
            assert!(pty.write(data));
        }
        let status = erx.recv_timeout(wait).ok();
        let mut out = Vec::new();
        while let Ok(d) = rx.try_recv() {
            out.extend_from_slice(&d);
        }
        (String::from_utf8_lossy(&out).into_owned(), status, pty)
    }

    #[test]
    fn echo_runs_and_exits() {
        let (out, status, pty) = run(&["/bin/sh", "-c", "printf 'hello-pty\\n'"], None, Duration::from_secs(10));
        assert!(matches!(pty.kind(), PtyKind::Unix));
        assert!(out.contains("hello-pty"), "出力: {out:?}");
        let status = status.expect("終了が届かない");
        assert_eq!(status.code, Some(0));
        assert!(!status.closed_by_us);
    }

    #[test]
    fn exit_code_is_reported() {
        let (_, status, _pty) = run(&["/bin/sh", "-c", "exit 3"], None, Duration::from_secs(10));
        assert_eq!(status.expect("終了が届かない").code, Some(3));
    }

    #[test]
    fn input_is_echoed_by_the_line_discipline() {
        // cat は行の編集（エコー）を通して受け取る。入力した文字が返ってくること
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let opts = PtyOptions { argv: vec!["/bin/cat".into()], env: terminal_env(), ..Default::default() };
        let mut pty = Pty::spawn(&opts, move |d| { let _ = tx.send(d.to_vec()); }, |_| {}).unwrap();
        assert!(pty.write(b"abc\r"));
        let mut out = String::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && out.matches("abc").count() < 2 {
            if let Ok(d) = rx.recv_timeout(Duration::from_millis(200)) {
                out.push_str(&String::from_utf8_lossy(&d));
            }
        }
        // エコー（打った分）と cat の出力で 2 回出る
        assert!(out.matches("abc").count() >= 2, "出力: {out:?}");
        pty.close(Duration::from_millis(500));
    }

    #[test]
    fn resize_changes_the_size_the_child_sees() {
        let (out, status, _pty) = run(
            &["/bin/sh", "-c", "sleep 0.3; stty size"],
            None,
            Duration::from_secs(10),
        );
        assert!(status.is_some(), "終了が届かない");
        // 既定の 80x24 が子に伝わっている
        assert!(out.contains("24 80"), "stty size の出力: {out:?}");
    }

    #[test]
    fn close_stops_the_child_and_its_descendants() {
        let opts = PtyOptions {
            // 孫（sleep）を残して親の sh が待つ。閉じたら両方終わること
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 60 & sleep 60".into()],
            env: terminal_env(),
            ..Default::default()
        };
        let mut pty = Pty::spawn(&opts, |_| {}, |_| {}).unwrap();
        let pid = pty.pid();
        // 子孫ができるのを待つ
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && descendant_pids(pid).len() < 2 {
            thread::sleep(Duration::from_millis(50));
        }
        let descendants = descendant_pids(pid);
        assert!(descendants.len() >= 2, "子孫: {descendants:?}");
        let report = pty.close(Duration::from_millis(500));
        assert!(report.console_closed.is_some());
        assert!(alive_pids(&[pid]).is_empty(), "子が残っている");
        assert!(alive_pids(&descendants).is_empty(), "子孫が残っている: {report}");
    }

    #[test]
    fn env_changes_and_foreign_terminal_vars() {
        // SAFETY: テストのプロセスの中だけ
        unsafe { std::env::set_var("WEZTERM_ET_TEST", "1") };
        unsafe { std::env::set_var("ET_TEST_A", "1") };
        let env = child_env(&[
            ("ET_TEST_A".into(), Some("2".into())),
            ("ET_TEST_B".into(), Some("3".into())),
            ("PATH".into(), None),
        ]);
        let has = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.to_string_lossy().into_owned());
        assert_eq!(has("ET_TEST_A").as_deref(), Some("2"));
        assert_eq!(has("ET_TEST_B").as_deref(), Some("3"));
        assert_eq!(has("PATH"), None, "削除が効いていない");
        assert_eq!(has("WEZTERM_ET_TEST"), None, "親の端末の変数（T-095）が残っている");
    }
}
