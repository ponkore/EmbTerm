//! et-pty: ConPTY で子プロセスを動かす（docs/TODO.md T-007）。
//!
//! 手順は ghostel の `src/ConPtyProcess.zig` に倣う（docs/research/ghostel.md §4）。
//!
//! - `conpty.dll`（再配布版）を指定のディレクトリの順に `LoadLibraryExW` し、無ければ kernel32 の
//!   `CreatePseudoConsole`（OS 同梱）を使う。再配布版は自分の隣か `x64\` の `OpenConsole.exe` を使い、
//!   無いと黙って OS 同梱の conhost になる（docs/research/conpty.md §1.3）
//! - パイプは名前付きパイプで、こちら側の端だけ overlapped にする（読みのキャンセルと待ち合わせのため）
//! - 子は `STARTF_USESTDHANDLES`（ハンドルは NULL）+ `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` で起動する。
//!   親の標準入出力がリダイレクトされていても子に継がせないため
//! - 子は Job Object（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`）に入れ、孫まで束ねる。ghostel には無い（T-007）。
//!   抜け出しを許す印（`BREAKAWAY_OK` / `SILENT_BREAKAWAY_OK`）は付けない（子孫を残さない。T-073）
//! - **パッケージのアプリ（MSIX。Microsoft Store の pwsh など）が起こす子は、既定では Job の外に作られる**
//!   （デスクトップアプリの実行環境から抜ける既定の決まり）。子を作るときに `PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY` で
//!   木全体のその抜け出しを止める（T-073）。それでも、子孫が「アプリの実行エイリアス」でパッケージのアプリを起こすと
//!   （cmd の中で `pwsh` と打つ）、システムが代わりにプロセスを作るので Job の外になる → 閉じるときに親子関係で辿って止める
//! - 読みのスレッドは子のハンドルも待ち、子が終わったら `ClosePseudoConsole` を別スレッドで呼んで
//!   EOF まで読む（古い Windows では `ClosePseudoConsole` が出力を吐き切るまで戻らないため）。待ちは時限付き
//! - 閉じる（[`Pty::close`]）ときは「まず穏やかに、時限を過ぎたら Job ごと」: `ClosePseudoConsole` で中のアプリに
//!   CTRL_CLOSE_EVENT を送り、子・Job の中・Job の外の子孫がすべて終わるのを猶予まで待ち、過ぎたら止める（T-076）
//! - 書きは専用のスレッドがチャネルで受ける。端末の応答（libghostty-vt の `write_pty`）もここへ積む
//!
//! unsafe はこのクレートの中に閉じ込める。

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Console::{COORD, HPCON};
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleW, GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
};
use windows::Win32::System::Pipes::{CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE};
use windows::Win32::System::Threading::*;
use windows::core::{HRESULT, HSTRING, PCWSTR, PWSTR, s, w};

/// `PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY` の値（winbase.h。windows クレートに無い）: 作るプロセスとその子孫が起こす子を、
/// デスクトップアプリ（MSIX）の実行環境の中に作る（T-073）
const PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_DISABLE_PROCESS_TREE: u32 = 0x02;

/// 読みの 1 回の大きさ（ghostel と同じ 64 KiB）
const READ_BUFFER: usize = 64 * 1024;
/// パイプのバッファ（ghostel と同じ）
const PIPE_BUFFER: u32 = 4096;

pub type Result<T> = std::io::Result<T>;

fn win_err(e: windows::core::Error) -> std::io::Error {
    std::io::Error::other(e)
}

/// 閉じるハンドル
struct Handle(HANDLE);

// カーネルのハンドルはどのスレッドから使ってもよい
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

type CreateFn = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
type ResizeFn = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type CloseFn = unsafe extern "system" fn(HPCON);

/// どの ConPTY を使っているか
#[derive(Clone, Debug)]
pub enum ConptyKind {
    /// 再配布版の `conpty.dll`。`open_console` は隣か `x64\` に `OpenConsole.exe` があるか
    /// （無いと conpty.dll は黙って OS 同梱の conhost を使う）
    Bundled { dll: PathBuf, open_console: bool },
    /// kernel32 の `CreatePseudoConsole`（OS 同梱）
    System,
}

impl std::fmt::Display for ConptyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConptyKind::Bundled { dll, open_console: true } => write!(f, "再配布版 {}", dll.display()),
            ConptyKind::Bundled { dll, open_console: false } => {
                write!(f, "再配布版 {}（OpenConsole.exe が無いので OS 同梱の conhost になる）", dll.display())
            }
            ConptyKind::System => write!(f, "OS 同梱（kernel32）"),
        }
    }
}

#[derive(Clone, Copy)]
struct Api {
    create: CreateFn,
    resize: ResizeFn,
    close: CloseFn,
}

unsafe fn proc_addr<T>(module: HMODULE, name: windows::core::PCSTR) -> Option<T> {
    let p = unsafe { GetProcAddress(module, name) }?;
    Some(unsafe { std::mem::transmute_copy::<_, T>(&p) })
}

/// `dirs` の順に `conpty.dll` を探して読み、無ければ kernel32 を使う
fn load_api(dirs: &[PathBuf]) -> Result<(Api, ConptyKind)> {
    for dir in dirs {
        let dll = dir.join("conpty.dll");
        if !dll.is_file() {
            continue;
        }
        // 隣の DLL から依存を探すよう LOAD_WITH_ALTERED_SEARCH_PATH で読む（パスは \ 区切りのフルパス）
        let full = std::path::absolute(&dll).unwrap_or(dll.clone());
        let Ok(module) = (unsafe { LoadLibraryExW(&HSTRING::from(full.as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH) })
        else {
            continue;
        };
        unsafe {
            if let (Some(create), Some(resize), Some(close)) = (
                proc_addr::<CreateFn>(module, s!("ConptyCreatePseudoConsole")),
                proc_addr::<ResizeFn>(module, s!("ConptyResizePseudoConsole")),
                proc_addr::<CloseFn>(module, s!("ConptyClosePseudoConsole")),
            ) {
                let open_console = dir.join("OpenConsole.exe").is_file() || dir.join("x64").join("OpenConsole.exe").is_file();
                return Ok((Api { create, resize, close }, ConptyKind::Bundled { dll: full, open_console }));
            }
        }
    }
    unsafe {
        let k32 = GetModuleHandleW(w!("kernel32.dll")).map_err(win_err)?;
        match (
            proc_addr::<CreateFn>(k32, s!("CreatePseudoConsole")),
            proc_addr::<ResizeFn>(k32, s!("ResizePseudoConsole")),
            proc_addr::<CloseFn>(k32, s!("ClosePseudoConsole")),
        ) {
            (Some(create), Some(resize), Some(close)) => Ok((Api { create, resize, close }, ConptyKind::System)),
            _ => Err(std::io::Error::other("この Windows には ConPTY がありません")),
        }
    }
}

/// 実行中の exe のディレクトリ
pub fn exe_dir() -> Option<PathBuf> {
    let mut buf = vec![0u16; 32768];
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    PathBuf::from(OsString::from_wide(&buf[..n])).parent().map(Path::to_path_buf)
}

/// 起動の指定
#[derive(Clone, Debug)]
pub struct PtyOptions {
    /// コマンドと引数（先頭がプログラム）。MSVCRT の規則でクォートして 1 本のコマンドラインにする
    pub argv: Vec<OsString>,
    /// 作業ディレクトリ。`None` なら親と同じ
    pub cwd: Option<PathBuf>,
    /// 親の環境変数を前から順に書き換える。`Some` は設定（同じ名前なら上書き）、`None` は削除。
    /// 名前の大文字・小文字は区別しない
    pub env: Vec<(OsString, Option<OsString>)>,
    pub cols: u16,
    pub rows: u16,
    /// `conpty.dll` を探すディレクトリ（前から順に）。[`default_conpty_search`] を参照
    pub conpty_search: Vec<PathBuf>,
    /// `CreatePseudoConsole` のフラグ。既定は 0（書記素クラスタで数え、eaw は 1 桁。docs/research/conpty.md §1.3）
    pub flags: u32,
}

impl Default for PtyOptions {
    fn default() -> Self {
        Self {
            argv: vec!["cmd.exe".into()],
            cwd: None,
            env: Vec::new(),
            cols: 80,
            rows: 24,
            conpty_search: default_conpty_search(None),
            flags: 0,
        }
    }
}

/// `conpty.dll` を探す既定の順: 指定のディレクトリ → exe の隣
pub fn default_conpty_search(preferred: Option<PathBuf>) -> Vec<PathBuf> {
    preferred.into_iter().chain(exe_dir()).collect()
}

/// 端末用に足す環境変数（`TERM=xterm-256color`、`COLORTERM=truecolor`、`TERM_PROGRAM=EmbTerm`）
pub fn terminal_env() -> Vec<(OsString, Option<OsString>)> {
    [("TERM", "xterm-256color"), ("COLORTERM", "truecolor"), ("TERM_PROGRAM", "EmbTerm")]
        .into_iter()
        .map(|(k, v)| (k.into(), Some(v.into())))
        .collect()
}

/// 子プロセスの終わり方
#[derive(Clone, Copy, Debug)]
pub struct ExitStatus {
    /// 終了コード。取れなければ `None`
    pub code: Option<u32>,
    /// こちらから閉じた（[`Pty::close`]）か
    pub closed_by_us: bool,
}

enum WriteMsg {
    Data(Vec<u8>),
    Stop,
}

/// PTY に書く口。複製して別のスレッドへ渡してよい。書く処理は書きのスレッドが行うので、送るだけで待たない
#[derive(Clone)]
pub struct PtyWriter {
    tx: Sender<WriteMsg>,
}

impl PtyWriter {
    /// 送る。書きのスレッドがもう終わっていれば false
    pub fn write(&self, data: &[u8]) -> bool {
        data.is_empty() || self.tx.send(WriteMsg::Data(data.to_vec())).is_ok()
    }
}

/// 擬似コンソールの持ち主。閉じるのは 1 回だけ（読みのスレッドか [`Pty::close`] のどちらか先に取ったほう）
struct Console {
    api: Api,
    hpc: Mutex<Option<HPCON>>,
}

unsafe impl Send for Console {}
unsafe impl Sync for Console {}

impl Console {
    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let g = self.hpc.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = *g {
            let size = COORD { X: cols.max(1) as i16, Y: rows.max(1) as i16 };
            unsafe { (self.api.resize)(h, size) }.ok().map_err(win_err)?;
        }
        Ok(())
    }

    /// `ClosePseudoConsole` を別スレッドで呼ぶ。もう閉じていれば `None`
    fn start_close(&self) -> Option<JoinHandle<()>> {
        let h = self.hpc.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        let close = self.api.close;
        let raw = h.0;
        thread::Builder::new()
            .name("conpty-close".into())
            .spawn(move || unsafe { close(HPCON(raw)) })
            .ok()
    }
}

/// こちらから閉じるときの段取り（[`Pty::close`] が決めて、読みのスレッドが従う。T-076）
#[derive(Default)]
struct CloseParams {
    /// 閉じ始めた時刻
    started: Option<Instant>,
    /// 穏やかに終わるのを待つ時間。過ぎたら Job ごと止める
    grace: Duration,
    /// 閉じ始めたときに見つけた、Job の外にいる子孫（親子関係で辿った。T-073）
    outside: Vec<Handle>,
    /// 段取りの記録（読みのスレッドが書く）
    report: CloseReport,
}

/// 閉じたときの段取りの記録（T-076 の調べ用）。時間は閉じ始めてから
#[derive(Clone, Debug, Default)]
pub struct CloseReport {
    /// `ClosePseudoConsole` が戻った（中のアプリに CTRL_CLOSE_EVENT を送り、conhost が終わった）
    pub console_closed: Option<Duration>,
    /// 出力を EOF まで読み終えた
    pub eof: Option<Duration>,
    /// 子と Job の中のプロセス（と Job の外の子孫）がすべて終わった。時限を過ぎたら `None`
    pub all_exited: Option<Duration>,
    /// 時限を過ぎて止めたプロセスの数（Job の中 + 外）
    pub forced: usize,
    /// 閉じ始めたときに Job の外にいた子孫の数
    pub outside: usize,
}

impl std::fmt::Display for CloseReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ms = |d: Option<Duration>| d.map_or("-".to_string(), |d| format!("{} ms", d.as_millis()));
        write!(
            f,
            "ClosePseudoConsole の戻り {} / EOF {} / 全員の終了 {} / Job の外の子孫 {} / 強制終了 {}",
            ms(self.console_closed),
            ms(self.eof),
            ms(self.all_exited),
            self.outside,
            self.forced
        )
    }
}

/// ConPTY で動いている子プロセス 1 つ
pub struct Pty {
    console: Arc<Console>,
    kind: ConptyKind,
    pid: u32,
    writer: PtyWriter,
    stop: Arc<Handle>,
    job: Arc<Handle>,
    reader: Option<JoinHandle<()>>,
    writer_thread: Option<JoinHandle<()>>,
    /// 読みのスレッドが終わったら値が届く
    reader_done: Receiver<()>,
    close_params: Arc<Mutex<CloseParams>>,
    warnings: Vec<String>,
}

/// プロセスの木の子孫の PID を求める（T-069 の KKP の安全策）。`root` は含めない。失敗すれば空。
/// Job から抜けたプロセス（パッケージのアプリを実行エイリアスで起こしたものなど。T-073）も含めたいので、Job ではなく親子関係で求める
pub fn descendant_pids(root: u32) -> Vec<u32> {
    let mut edges = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Vec::new() };
        let snap = Handle(snap);
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap.0, &mut e).is_ok() {
            loop {
                edges.push((e.th32ParentProcessID, e.th32ProcessID));
                if Process32NextW(snap.0, &mut e).is_err() {
                    break;
                }
            }
        }
    }
    let mut result = Vec::new();
    let mut frontier = vec![root];
    while let Some(p) = frontier.pop() {
        for &(parent, child) in &edges {
            if parent == p && child != root && !result.contains(&child) {
                result.push(child);
                frontier.push(child);
            }
        }
    }
    result
}

/// 子と子孫のプロセスを「名前(PID)」で並べる。Job に入っていないものには `!` を付ける（T-073 の調べ用）
pub fn describe_tree(root: u32, job: &[u32]) -> String {
    let mut names = std::collections::HashMap::new();
    unsafe {
        if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            let snap = Handle(snap);
            let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            if Process32FirstW(snap.0, &mut e).is_ok() {
                loop {
                    let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                    names.insert(e.th32ProcessID, String::from_utf16_lossy(&e.szExeFile[..n]));
                    if Process32NextW(snap.0, &mut e).is_err() {
                        break;
                    }
                }
            }
        }
    }
    std::iter::once(root)
        .chain(descendant_pids(root))
        .map(|p| format!("{}({p}){}", names.get(&p).map_or("?", |s| s.as_str()), if job.contains(&p) { "" } else { "!" }))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Job に入っているプロセスの ID
fn job_pids(job: &Handle) -> Vec<u32> {
    // JOBOBJECT_BASIC_PROCESS_ID_LIST: 割り当て数・入っている数（u32 x 2）のあとに ULONG_PTR の配列
    let mut buf = vec![0usize; 2 + 1024];
    let ok = unsafe {
        QueryInformationJobObject(
            Some(job.0),
            JobObjectBasicProcessIdList,
            buf.as_mut_ptr() as *mut c_void,
            (buf.len() * std::mem::size_of::<usize>()) as u32,
            None,
        )
    };
    if ok.is_err() {
        return Vec::new();
    }
    let count = (buf[0] >> 32) as usize; // NumberOfProcessIdsInList（2 つ目の u32）
    buf[1..1 + count.min(1024)].iter().map(|&p| p as u32).collect()
}

/// プロセスの組を見張る（[`ProcessWatch::new`]）
pub struct ProcessWatch {
    handles: Vec<Handle>,
}

impl ProcessWatch {
    /// PID の組を見張る（開けないものは飛ばす）
    pub fn new(pids: &[u32]) -> Self {
        let handles = pids
            .iter()
            .filter_map(|&p| unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, p) }.ok().map(Handle))
            .collect();
        Self { handles }
    }

    /// 見張るプロセスが無いか
    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// 見張っているプロセスがすべて終わったか（空なら false）
    pub fn all_exited(&self) -> bool {
        !self.handles.is_empty()
            && self.handles.iter().all(|h| unsafe { WaitForSingleObject(h.0, 0) } == WAIT_OBJECT_0)
    }
}

static PIPE_SERIAL: AtomicU64 = AtomicU64::new(0);

/// 名前付きパイプを 1 本作る。サーバ側（読む側、PIPE_ACCESS_INBOUND）とクライアント側（書く側）を返す。
/// `overlapped_read` / `overlapped_write` でどちらの端を overlapped にするかを決める
fn create_pipe(overlapped_read: bool, overlapped_write: bool) -> Result<(Handle, Handle)> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = format!(
        r"\\.\pipe\embterm-conpty-{}-{}-{:x}",
        std::process::id(),
        PIPE_SERIAL.fetch_add(1, Ordering::Relaxed),
        nanos & 0xffff_ffff
    );
    let name = HSTRING::from(name);
    unsafe {
        let mut read_flags = PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE;
        if overlapped_read {
            read_flags |= FILE_FLAG_OVERLAPPED;
        }
        let read = CreateNamedPipeW(&name, read_flags, PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS, 1, PIPE_BUFFER, PIPE_BUFFER, 0, None);
        if read.is_invalid() {
            return Err(std::io::Error::last_os_error());
        }
        let read = Handle(read);
        let write_flags = if overlapped_write { FILE_FLAG_OVERLAPPED } else { FILE_ATTRIBUTE_NORMAL };
        let write = CreateFileW(&name, GENERIC_WRITE.0, FILE_SHARE_NONE, None, OPEN_EXISTING, write_flags, None).map_err(win_err)?;
        Ok((read, Handle(write)))
    }
}

/// MSVCRT の規則で 1 つの引数をクォートして `out` に足す
fn quote_arg(arg: &OsStr, out: &mut Vec<u16>) {
    let wide: Vec<u16> = arg.encode_wide().collect();
    let needs = wide.is_empty() || wide.iter().any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == b'\n' as u16 || c == 0x0b || c == b'"' as u16);
    if !needs {
        out.extend_from_slice(&wide);
        return;
    }
    out.push(b'"' as u16);
    let mut backslashes = 0usize;
    for &c in &wide {
        if c == b'\\' as u16 {
            backslashes += 1;
        } else if c == b'"' as u16 {
            out.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
            out.push(c);
            backslashes = 0;
        } else {
            out.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            out.push(c);
            backslashes = 0;
        }
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    out.push(b'"' as u16);
}

/// argv を 1 本のコマンドライン（NUL 終端の UTF-16）にする
pub fn command_line(argv: &[OsString]) -> Vec<u16> {
    let mut out = Vec::new();
    for (i, a) in argv.iter().enumerate() {
        if i > 0 {
            out.push(b' ' as u16);
        }
        quote_arg(a, &mut out);
    }
    out.push(0);
    out
}

/// 親の環境変数を `changes` で書き換えた環境ブロック（`名前=値\0 … \0`、名前の大文字小文字を無視して並べる）。
/// `changes` は前から順に当てる（`Some` は設定、`None` は削除）
fn environment_block(changes: &[(OsString, Option<OsString>)]) -> Vec<u16> {
    let upper = |s: &OsStr| s.to_string_lossy().to_uppercase();
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (k, v) in changes {
        let ku = upper(k);
        vars.retain(|(k2, _)| upper(k2) != ku);
        if let Some(v) = v {
            vars.push((k.clone(), v.clone()));
        }
    }
    vars.sort_by_key(|(k, _)| upper(k));
    let mut block = Vec::new();
    for (k, v) in vars {
        block.extend(k.encode_wide());
        block.push(b'=' as u16);
        block.extend(v.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// 子プロセスを入れる Job。ハンドルを閉じると中のプロセスがすべて終わる
fn create_job() -> Result<Handle> {
    unsafe {
        let job = Handle(CreateJobObjectW(None, PCWSTR::null()).map_err(win_err)?);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .map_err(win_err)?;
        Ok(job)
    }
}

impl Pty {
    /// 子プロセスを ConPTY で起動する。
    ///
    /// - `on_output`: 読みのスレッドで、ConPTY の出力を受けるたびに呼ばれる
    /// - `on_exit`: 子が終わり、出力を EOF まで読み終えたとき（または [`Pty::close`] で閉じたとき）に
    ///   読みのスレッドで 1 回だけ呼ばれる
    pub fn spawn(
        opts: &PtyOptions,
        mut on_output: impl FnMut(&[u8]) + Send + 'static,
        on_exit: impl FnOnce(ExitStatus) + Send + 'static,
    ) -> Result<Pty> {
        let (api, kind) = load_api(&opts.conpty_search)?;
        let mut warnings = Vec::new();
        if let ConptyKind::Bundled { open_console: false, .. } = kind {
            warnings.push(format!("{kind}"));
        }

        // 入力: ConPTY が読む端（同期）と、こちらが書く端（overlapped）
        let (in_read, in_write) = create_pipe(false, true)?;
        // 出力: こちらが読む端（overlapped）と、ConPTY が書く端（同期）
        let (out_read, out_write) = create_pipe(true, false)?;

        let mut hpc = HPCON::default();
        let size = COORD { X: opts.cols.max(1) as i16, Y: opts.rows.max(1) as i16 };
        unsafe { (api.create)(size, in_read.0, out_write.0, opts.flags, &mut hpc) }.ok().map_err(win_err)?;
        // ConPTY 側の端は ConPTY が複製して持つので、こちらでは閉じる
        drop(in_read);
        drop(out_write);
        let console = Arc::new(Console { api, hpc: Mutex::new(Some(hpc)) });

        let job = Arc::new(create_job()?);
        let process = match spawn_child(opts, hpc, &job, &mut warnings) {
            Ok(p) => p,
            Err(e) => {
                if let Some(t) = console.start_close() {
                    let _ = t.join();
                }
                return Err(e);
            }
        };
        let pid = unsafe { GetProcessId(process.0) };

        let stop = Arc::new(Handle(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(win_err)?));

        // 書きのスレッド
        let (tx, rx) = mpsc::channel::<WriteMsg>();
        let writer_thread = {
            let stop = stop.clone();
            thread::Builder::new().name("pty-write".into()).spawn(move || write_loop(in_write, rx, stop))?
        };

        // 読みのスレッド
        let (done_tx, reader_done) = mpsc::channel();
        let close_params = Arc::new(Mutex::new(CloseParams::default()));
        let reader = {
            let (console, stop, job, params) = (console.clone(), stop.clone(), job.clone(), close_params.clone());
            thread::Builder::new().name("pty-read".into()).spawn(move || {
                let status = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    read_loop(&out_read, &process, &stop, &console, &job, &params, &mut on_output)
                }))
                .unwrap_or(ExitStatus { code: None, closed_by_us: false });
                drop(out_read);
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_exit(status)));
                let _ = done_tx.send(());
            })?
        };

        Ok(Pty {
            console,
            kind,
            pid,
            writer: PtyWriter { tx },
            stop,
            job,
            reader: Some(reader),
            writer_thread: Some(writer_thread),
            reader_done,
            close_params,
            warnings,
        })
    }

    /// 使っている ConPTY
    pub fn kind(&self) -> &ConptyKind {
        &self.kind
    }

    /// 子プロセスの ID
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// 起動のときの注意（OpenConsole.exe が無い、Job に入れられなかった、など）
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// 書く口（複製して別のスレッドへ渡せる）
    pub fn writer(&self) -> PtyWriter {
        self.writer.clone()
    }

    /// 書く（送るだけで待たない）
    pub fn write(&self, data: &[u8]) -> bool {
        self.writer.write(data)
    }

    /// Job に入っているプロセスの ID（T-073 の調べ用）。取れなければ空
    pub fn job_pids(&self) -> Vec<u32> {
        job_pids(&self.job)
    }

    /// 擬似コンソールの大きさを変える（ピクセルは渡さない）
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.console.resize(cols, rows)
    }

    /// 閉じる（T-076）。**最大 `grace` + 2 秒待つので、UI スレッドや Lisp のスレッドでは呼ばない**（et-session は別のスレッドで呼ぶ）。
    ///
    /// 1. Job の外にいる子孫（Job から抜けたもの。T-073）を、まだ親子関係で辿れるうちに控える
    /// 2. `ClosePseudoConsole` で中のアプリに CTRL_CLOSE_EVENT を送り（端末を閉じたときの知らせ。Windows Terminal と同じ）、EOF まで読む
    /// 3. 子・Job の中・控えた子孫がすべて終わるのを `grace` まで待つ（穏やかに終わる猶予）
    /// 4. 過ぎたら Job ごと止め、控えた子孫も止める
    pub fn close(&mut self, grace: Duration) -> CloseReport {
        {
            let mut p = self.close_params.lock().unwrap_or_else(|e| e.into_inner());
            if p.started.is_none() {
                let job = job_pids(&self.job);
                p.outside = descendant_pids(self.pid)
                    .into_iter()
                    .filter(|pid| !job.contains(pid))
                    .filter_map(|pid| unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) }.ok().map(Handle))
                    .collect();
                p.report.outside = p.outside.len();
                p.grace = grace;
                p.started = Some(Instant::now());
            }
        }
        unsafe {
            let _ = SetEvent(self.stop.0);
        }
        let _ = self.writer.tx.send(WriteMsg::Stop);
        if let Some(r) = self.reader.take() {
            // 読みのスレッドが段取りを済ませる。ClosePseudoConsole が戻らないなどで間に合わなければ、ここで止める
            if self.reader_done.recv_timeout(grace + Duration::from_secs(2)).is_ok() {
                let _ = r.join();
            } else {
                let mut p = self.close_params.lock().unwrap_or_else(|e| e.into_inner());
                let outside = std::mem::take(&mut p.outside);
                p.report.forced += force_kill(&self.job, &outside);
            }
        }
        if let Some(w) = self.writer_thread.take() {
            let _ = w.join();
        }
        self.close_params.lock().unwrap_or_else(|e| e.into_inner()).report.clone()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.close(Duration::from_secs(3));
    }
}

/// Job の中と `outside` のプロセスのうち、まだ動いているものの数
fn alive_count(job: &Handle, outside: &[Handle]) -> usize {
    job_pids(job).len() + outside.iter().filter(|h| unsafe { WaitForSingleObject(h.0, 0) } != WAIT_OBJECT_0).count()
}

/// Job ごと止め、`outside` のプロセスも止める。止めた数を返す
fn force_kill(job: &Handle, outside: &[Handle]) -> usize {
    let n = alive_count(job, outside);
    unsafe {
        let _ = TerminateJobObject(job.0, 1);
        for h in outside {
            if WaitForSingleObject(h.0, 0) != WAIT_OBJECT_0 {
                let _ = TerminateProcess(h.0, 1);
            }
        }
    }
    n
}

fn spawn_child(opts: &PtyOptions, hpc: HPCON, job: &Handle, warnings: &mut Vec<String>) -> Result<Handle> {
    let mut cmdline = command_line(&opts.argv);
    let env = environment_block(&opts.env);
    let cwd = opts.cwd.as_ref().map(|p| HSTRING::from(p.as_os_str()));
    unsafe {
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 2, None, &mut size);
        let mut attr_buf = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
        InitializeProcThreadAttributeList(Some(list), 2, None, &mut size).map_err(win_err)?;
        struct AttrList(LPPROC_THREAD_ATTRIBUTE_LIST);
        impl Drop for AttrList {
            fn drop(&mut self) {
                unsafe { DeleteProcThreadAttributeList(self.0) }
            }
        }
        let _list_guard = AttrList(list);
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            Some(hpc.0 as *const c_void),
            std::mem::size_of::<HPCON>(),
            None,
            None,
        )
        .map_err(win_err)?;
        // T-073: パッケージのアプリ（Microsoft Store の pwsh など、MSIX）が起こす子は、既定では「デスクトップアプリの実行環境の外」に
        // 作られ、そのとき Job からも抜ける。子孫の木全体でそれをやめさせる（子孫に引き継がれる）。古い Windows で使えなければ諦める
        let desktop_app_policy: u32 = PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_DISABLE_PROCESS_TREE;
        if let Err(e) = UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY as usize,
            Some(&desktop_app_policy as *const u32 as *const c_void),
            std::mem::size_of::<u32>(),
            None,
            None,
        ) {
            warnings.push(format!("DESKTOP_APP_POLICY を設定できません（パッケージのアプリの子が Job から抜けうる）: {e}"));
        }

        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // 親の標準入出力（リダイレクトされていることがある）を子に継がせない。コンソールは擬似コンソールから得る
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = list;
        let mut pi = PROCESS_INFORMATION::default();
        // Job に入れる前に孫を作られないよう、止めた状態で作ってから入れて動かす
        let flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED;
        let cwd_p = cwd.as_ref().map_or(PCWSTR::null(), |h| PCWSTR(h.as_ptr()));
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            false,
            flags,
            Some(env.as_ptr() as *const c_void),
            cwd_p,
            &si.StartupInfo,
            &mut pi,
        )
        .map_err(win_err)?;
        let process = Handle(pi.hProcess);
        let thread = Handle(pi.hThread);
        if let Err(e) = AssignProcessToJobObject(job.0, process.0) {
            warnings.push(format!("子を Job に入れられませんでした（孫が残りうる）: {e}"));
        }
        ResumeThread(thread.0);
        Ok(process)
    }
}

/// overlapped の読み 1 回の結果
enum ReadResult {
    Data(usize),
    Eof,
    /// 待ちを打ち切った（子の終了・止める知らせ・時間切れ）。読みは取り消してある
    Interrupted(usize),
}

/// overlapped で 1 回読む。`interrupts` のどれかが知らされたら読みを取り消す。`timeout` は ms
fn read_once(pipe: &Handle, event: &Handle, buf: &mut [u8], interrupts: &[HANDLE], timeout: u32) -> ReadResult {
    unsafe {
        let _ = ResetEvent(event.0);
        let mut ov = OVERLAPPED { hEvent: event.0, ..Default::default() };
        let mut n = 0u32;
        match ReadFile(pipe.0, Some(buf), None, Some(&mut ov)) {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                let mut handles = vec![event.0];
                handles.extend_from_slice(interrupts);
                let r = WaitForMultipleObjects(&handles, false, timeout);
                if r != WAIT_OBJECT_0 {
                    // 子が終わった・止める・時間切れ。取り消して、取り消す前に読めた分があれば返す
                    let _ = CancelIoEx(pipe.0, Some(&ov));
                    let got = GetOverlappedResult(pipe.0, &ov, &mut n, true).map(|_| n as usize).unwrap_or(0);
                    return ReadResult::Interrupted(got);
                }
            }
            Err(_) => return ReadResult::Eof,
        }
        match GetOverlappedResult(pipe.0, &ov, &mut n, true) {
            Ok(()) if n > 0 => ReadResult::Data(n as usize),
            Ok(()) => ReadResult::Data(0),
            Err(_) => ReadResult::Eof,
        }
    }
}

fn read_loop(
    pipe: &Handle,
    process: &Handle,
    stop: &Handle,
    console: &Console,
    job: &Handle,
    params: &Mutex<CloseParams>,
    on_output: &mut dyn FnMut(&[u8]),
) -> ExitStatus {
    let Ok(ev) = (unsafe { CreateEventW(None, true, false, PCWSTR::null()) }) else {
        return ExitStatus { code: None, closed_by_us: false };
    };
    let event = Handle(ev);
    let mut buf = vec![0u8; READ_BUFFER];
    let mut eof = false;
    // 1. 子が生きている間: 読みと、子の終了と、止める知らせを待つ
    loop {
        match read_once(pipe, &event, &mut buf, &[process.0, stop.0], INFINITE) {
            ReadResult::Data(n) => on_output(&buf[..n]),
            ReadResult::Eof => {
                eof = true;
                break;
            }
            ReadResult::Interrupted(n) => {
                on_output(&buf[..n]);
                break;
            }
        }
    }
    let closed_by_us = unsafe { WaitForSingleObject(stop.0, 0) } == WAIT_OBJECT_0;
    // こちらから閉じたときは Pty::close の段取り（猶予）に従う。子が自分で終わったときは、残りの出力を読むだけ（3 秒まで）
    let (t0, grace) = {
        let p = params.lock().unwrap_or_else(|e| e.into_inner());
        match (closed_by_us, p.started) {
            (true, Some(t)) => (t, p.grace),
            _ => (Instant::now(), Duration::from_secs(3)),
        }
    };
    let deadline = t0 + grace;
    let mut report = CloseReport::default();

    // 2. ClosePseudoConsole を別スレッドで呼び（中のアプリに CTRL_CLOSE_EVENT が届く）、EOF まで読む（時限付き）。
    //    古い Windows の ClosePseudoConsole は出力を吐き切るまで戻らないので、読みながら待つ
    let closed_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let closer = console.start_close().and_then(|c| {
        let closed_at = closed_at.clone();
        thread::Builder::new()
            .name("conpty-close-wait".into())
            .spawn(move || {
                let _ = c.join();
                *closed_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
            })
            .ok()
    });
    if !eof {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match read_once(pipe, &event, &mut buf, &[], left.as_millis() as u32) {
                ReadResult::Data(n) => on_output(&buf[..n]),
                ReadResult::Eof => {
                    eof = true;
                    break;
                }
                ReadResult::Interrupted(n) => {
                    on_output(&buf[..n]);
                    break;
                }
            }
        }
    }
    if eof {
        report.eof = Some(t0.elapsed());
    }

    // 3. こちらから閉じたとき: 子・Job の中・Job の外の子孫がすべて終わるのを猶予まで待ち、過ぎたら止める。
    //    子が自分で終わったとき: 子が終わったのを確かめるだけ（Job に残ったものは Pty を落とすときに Job ごと止まる）
    let outside: Vec<Handle> =
        if closed_by_us { std::mem::take(&mut params.lock().unwrap_or_else(|e| e.into_inner()).outside) } else { Vec::new() };
    loop {
        let child_done = unsafe { WaitForSingleObject(process.0, 0) } == WAIT_OBJECT_0;
        if child_done && (!closed_by_us || alive_count(job, &outside) == 0) {
            report.all_exited = Some(t0.elapsed());
            break;
        }
        if Instant::now() >= deadline {
            report.forced = force_kill(job, &outside);
            unsafe {
                let _ = WaitForSingleObject(process.0, 1000);
            }
            break;
        }
        unsafe {
            let _ = WaitForSingleObject(process.0, 20);
        }
    }
    // ClosePseudoConsole は戻らないことがあるので、待つのは少しだけ
    if let Some(c) = closer {
        let until = Instant::now() + Duration::from_millis(500);
        while !c.is_finished() && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
    }
    report.console_closed = closed_at.lock().unwrap_or_else(|e| e.into_inner()).map(|t| t.saturating_duration_since(t0));
    let mut code = None;
    unsafe {
        let mut c = 0u32;
        if GetExitCodeProcess(process.0, &mut c).is_ok() && c != STILL_ACTIVE.0 as u32 {
            code = Some(c);
        }
    }
    if closed_by_us {
        let mut p = params.lock().unwrap_or_else(|e| e.into_inner());
        let outside = p.report.outside;
        p.report = CloseReport { outside, ..report };
    }
    ExitStatus { code, closed_by_us }
}

fn write_loop(pipe: Handle, rx: Receiver<WriteMsg>, stop: Arc<Handle>) {
    let Ok(ev) = (unsafe { CreateEventW(None, true, false, PCWSTR::null()) }) else { return };
    let event = Handle(ev);
    while let Ok(WriteMsg::Data(data)) = rx.recv() {
        let mut off = 0;
        while off < data.len() {
            unsafe {
                let _ = ResetEvent(event.0);
                let mut ov = OVERLAPPED { hEvent: event.0, ..Default::default() };
                let mut n = 0u32;
                match WriteFile(pipe.0, Some(&data[off..]), None, Some(&mut ov)) {
                    Ok(()) => {}
                    Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                        if WaitForMultipleObjects(&[event.0, stop.0], false, INFINITE) != WAIT_OBJECT_0 {
                            let _ = CancelIoEx(pipe.0, Some(&ov));
                            let _ = GetOverlappedResult(pipe.0, &ov, &mut n, true);
                            return;
                        }
                    }
                    // 子が終わってパイプが切れた。残りは捨てる
                    Err(_) => return,
                }
                match GetOverlappedResult(pipe.0, &ov, &mut n, true) {
                    Ok(()) if n > 0 => off += n as usize,
                    _ => return,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cl(args: &[&str]) -> String {
        let argv: Vec<OsString> = args.iter().map(OsString::from).collect();
        let w = command_line(&argv);
        String::from_utf16_lossy(&w[..w.len() - 1])
    }

    #[test]
    fn quoting() {
        assert_eq!(cl(&["cmd.exe"]), "cmd.exe");
        assert_eq!(cl(&["pwsh", "-NoLogo"]), "pwsh -NoLogo");
        assert_eq!(cl(&[r"C:\Program Files\x.exe", "a b"]), r#""C:\Program Files\x.exe" "a b""#);
        assert_eq!(cl(&["x", r#"say "hi""#]), r#"x "say \"hi\"""#);
        assert_eq!(cl(&["x", r"dir\"]), r"x dir\");
        assert_eq!(cl(&["x", r"a dir\"]), r#"x "a dir\\""#);
        assert_eq!(cl(&["x", ""]), r#"x """#);
    }

    #[test]
    fn env_block_overrides() {
        let b = environment_block(&[("TERM".into(), Some("xterm-256color".into()))]);
        let s = String::from_utf16_lossy(&b);
        assert!(s.contains("TERM=xterm-256color\0"));
        assert!(s.ends_with("\0\0"));
        assert_eq!(s.matches("\0TERM=").count() + s.starts_with("TERM=") as usize, 1);
        // 削除と、後から当てたものが勝つこと（PATH は親に必ずある）
        let b = environment_block(&[
            ("path".into(), None),
            ("ET_TEST_A".into(), Some("1".into())),
            ("et_test_a".into(), Some("2".into())),
        ]);
        let s = String::from_utf16_lossy(&b);
        assert!(!s.to_uppercase().contains("\0PATH="), "PATH が残っている");
        assert!(s.contains("et_test_a=2\0") && !s.contains("ET_TEST_A=1"));
    }

    /// OS 同梱の ConPTY で cmd を動かし、出力が届いて終了が分かること
    #[test]
    fn run_cmd_echo() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (etx, erx) = mpsc::channel();
        let opts = PtyOptions {
            argv: vec!["cmd.exe".into(), "/c".into(), "echo".into(), "hello-pty".into()],
            env: terminal_env(),
            conpty_search: Vec::new(),
            ..Default::default()
        };
        let pty = Pty::spawn(&opts, move |d| { let _ = tx.send(d.to_vec()); }, move |s| { let _ = etx.send(s); }).unwrap();
        assert!(matches!(pty.kind(), ConptyKind::System));
        let status = erx.recv_timeout(Duration::from_secs(10)).expect("終了が届かない");
        let out: Vec<u8> = rx.try_iter().flatten().collect();
        let out = String::from_utf8_lossy(&out);
        assert!(out.contains("hello-pty"), "出力: {out:?}");
        assert_eq!(status.code, Some(0));
        assert!(!status.closed_by_us);
        drop(pty);
    }

    /// 再配布版（`cargo xtask fetch-conpty` で vendor/conpty に置いたもの）なら起動直後に `CSI c` が届く。
    /// こちらから閉じたときに Job で子が止まること
    #[test]
    fn bundled_sends_da1_and_close_kills() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/conpty");
        if !dir.join("conpty.dll").exists() {
            eprintln!("vendor/conpty が無いので飛ばす");
            return;
        }
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (etx, erx) = mpsc::channel();
        let opts = PtyOptions {
            argv: vec!["cmd.exe".into(), "/k".into(), "echo".into(), "bundled".into()],
            conpty_search: vec![dir],
            ..Default::default()
        };
        let mut pty = Pty::spawn(&opts, move |d| { let _ = tx.send(d.to_vec()); }, move |s| { let _ = etx.send(s); }).unwrap();
        assert!(matches!(pty.kind(), ConptyKind::Bundled { open_console: true, .. }), "{}", pty.kind());
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !String::from_utf8_lossy(&out).contains("bundled") {
            if let Ok(d) = rx.recv_timeout(Duration::from_millis(100)) {
                out.extend(d);
            }
        }
        let s = String::from_utf8_lossy(&out).into_owned();
        assert!(s.contains("\x1b[c"), "DA1 の要求が来ない: {s:?}");
        assert!(s.contains("bundled"), "{s:?}");
        let pid = pty.pid();
        pty.close(Duration::from_secs(5));
        let status = erx.recv_timeout(Duration::from_secs(1)).expect("終了が届かない");
        assert!(status.closed_by_us);
        // 子はもういない
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) };
        if let Ok(h) = h {
            let h = Handle(h);
            assert_eq!(unsafe { WaitForSingleObject(h.0, 0) }, WAIT_OBJECT_0);
        }
    }
}
