//! module → Lisp の知らせ（T-104）。
//!
//! Lisp は端末の状態をポーリングで読むが、50 ms ごとに全端末を見回ると、何もしていないときでも端末 15 個で CPU 4〜5% かかる。
//! そこで、状態が変わったら Emacs の `open_channel`（Emacs 28〜）のパイプに 1 バイト書き、Lisp に読みに来させる。
//! Lisp が読み始める（[`ack`]）までは書き足さないので、パイプが詰まることはない。
//!
//! **CRT の扱い**: `open_channel` が返すのは Emacs の CRT の fd。module は CRT を静的にリンクしている（.cargo/config.toml）ので、
//! 自分の CRT では fd を引けない（GenPreview ADR 0006）。ghostel と同じく、**Emacs の exe の import 表から `_get_osfhandle` を
//! 取り込んでいる CRT の DLL を探し**、その CRT の `_get_osfhandle` で OS のハンドルにする（docs/research/ghostel.md §5.3）。
//! ハンドルは自分用に複製して使う。
//!
//! **fd は閉じない**: Windows の Emacs は fd ごとの情報（w32.c の `fd_info`。パイプか、など）を持っていて、`open_channel` の `dup` で
//! それが写される。module が CRT の `_close` で直接閉じると、その情報だけが残り、同じ番号でファイルを開いた Emacs が
//! パイプと取り違えて読めなくなった（`insert-file-contents` が `Bad file descriptor`。最初の版で踏んだ）。Emacs 1 つにつき 1 本なので持ち続ける

use std::ffi::{CStr, c_char, c_int};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use emacs::Env;
use emacs::raw::emacs_env;
use windows::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, HMODULE};
use windows::Win32::Storage::FileSystem::WriteFile;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::core::{PCSTR, s};

struct Channel(HANDLE);
// SAFETY: パイプの書き込み口のハンドル。どのスレッドから WriteFile してもよい
unsafe impl Send for Channel {}

static CHANNEL: Mutex<Option<Channel>> = Mutex::new(None);
/// 知らせを書いたが、Lisp がまだ読み始めていない
static PENDING: AtomicBool = AtomicBool::new(false);

/// 状態が変わったことを Lisp に知らせる（どのスレッドからでも。Lisp が読み始めるまでは 1 回だけ書く）
pub fn notify() {
    if PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    let mut ch = CHANNEL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(c) = ch.as_ref() else {
        PENDING.store(false, Ordering::Release);
        return;
    };
    let mut written = 0u32;
    if unsafe { WriteFile(c.0, Some(b"!"), Some(&mut written), None) }.is_err() {
        // Lisp のプロセスが消された。以後はポーリングだけ
        if let Some(c) = ch.take() {
            unsafe {
                let _ = CloseHandle(c.0);
            }
        }
    }
}

/// Lisp が読み始めた。以後の変化はまた知らせる
pub fn ack() {
    PENDING.store(false, Ordering::Release);
}

/// 知らせの口がつながっているか
pub fn connected() -> bool {
    CHANNEL.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// `emacs_funcall_exit_return`（非局所脱出なし）
const EXIT_RETURN: u32 = 0;

type GetOsfHandle = unsafe extern "C" fn(c_int) -> isize;

/// Lisp の変数 `embterm--notify-process`（`make-pipe-process` で作ったもの）に `open_channel` して、知らせの口にする
pub fn open(env: &Env) -> Result<(), String> {
    unsafe {
        let e: *mut emacs_env = env.raw();
        let (Some(intern), Some(funcall), Some(open_channel), Some(check), Some(clear)) =
            ((*e).intern, (*e).funcall, (*e).open_channel, (*e).non_local_exit_check, (*e).non_local_exit_clear)
        else {
            return Err("この Emacs には open_channel がありません（Emacs 28 から）".into());
        };
        let sym = intern(e, c"embterm--notify-process".as_ptr());
        let symbol_value = intern(e, c"symbol-value".as_ptr());
        let mut args = [sym];
        let process = funcall(e, symbol_value, 1, args.as_mut_ptr());
        if check(e) != EXIT_RETURN {
            clear(e);
            return Err("embterm--notify-process を読めません".into());
        }
        let fd = open_channel(e, process);
        if check(e) != EXIT_RETURN || fd < 0 {
            clear(e);
            return Err("open_channel に失敗しました".into());
        }
        let get_osfhandle = emacs_crt().ok_or("Emacs の CRT（_get_osfhandle）が見つかりません")?;
        let raw = get_osfhandle(fd);
        if raw == -1 || raw == 0 {
            return Err(format!("open_channel の fd {fd} をハンドルにできません"));
        }
        // fd は閉じない（モジュールの説明を参照）。ハンドルを自分用に複製する
        let me = GetCurrentProcess();
        let mut own = HANDLE::default();
        DuplicateHandle(me, HANDLE(raw as _), me, &mut own, 0, false, DUPLICATE_SAME_ACCESS)
            .map_err(|e| format!("ハンドルを複製できません: {e}"))?;
        let mut ch = CHANNEL.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = ch.replace(Channel(own)) {
            let _ = CloseHandle(old.0);
        }
        PENDING.store(false, Ordering::Release);
        Ok(())
    }
}

/// Emacs の exe が `_get_osfhandle` を取り込んでいる CRT の DLL から、`_get_osfhandle` を引く
fn emacs_crt() -> Option<GetOsfHandle> {
    let dll = crt_dll_of_exe()?;
    unsafe {
        let mut module: HMODULE = GetModuleHandleA(PCSTR(dll.as_ptr() as *const u8)).ok()?;
        if module.is_invalid() && dll.to_bytes().to_ascii_lowercase().starts_with(b"api-ms-win-crt") {
            module = GetModuleHandleA(s!("ucrtbase.dll")).ok()?;
        }
        let g = GetProcAddress(module, s!("_get_osfhandle"))?;
        type Proc = unsafe extern "system" fn() -> isize;
        Some(std::mem::transmute::<Proc, GetOsfHandle>(g))
    }
}

/// exe（`GetModuleHandleW(NULL)`）の import 表を読み、`_get_osfhandle` を取り込んでいる DLL の名前を返す。
/// PE32+ の形をそのまま読む（DOS ヘッダの e_lfanew → NT ヘッダ → OptionalHeader の DataDirectory[1]）
fn crt_dll_of_exe() -> Option<std::ffi::CString> {
    unsafe {
        let base = GetModuleHandleW(None).ok()?.0 as *const u8;
        let rd32 = |p: *const u8| (p as *const u32).read_unaligned();
        let nt = base.add(rd32(base.add(0x3c)) as usize);
        if rd32(nt) != 0x0000_4550 {
            return None; // "PE\0\0"
        }
        let opt = nt.add(4 + 20);
        if (opt as *const u16).read_unaligned() != 0x20b {
            return None; // PE32+ だけ
        }
        let import_rva = rd32(opt.add(112 + 8));
        if import_rva == 0 {
            return None;
        }
        let mut desc = base.add(import_rva as usize);
        loop {
            let name_rva = rd32(desc.add(12));
            if name_rva == 0 {
                return None;
            }
            let thunk_rva = match rd32(desc) {
                0 => rd32(desc.add(16)),
                r => r,
            };
            let mut thunk = base.add(thunk_rva as usize) as *const u64;
            loop {
                let t = thunk.read_unaligned();
                if t == 0 {
                    break;
                }
                if t & (1 << 63) == 0 {
                    let fname = CStr::from_ptr(base.add((t as u32 as usize) + 2) as *const c_char);
                    if fname.to_bytes() == b"_get_osfhandle" {
                        return Some(CStr::from_ptr(base.add(name_rva as usize) as *const c_char).to_owned());
                    }
                }
                thunk = thunk.add(1);
            }
            desc = desc.add(20);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn exe_import_table_is_readable() {
        // テストの exe（Rust、CRT は静的）は _get_osfhandle を DLL から取り込まないので None。読んで落ちないことだけ確かめる
        let _ = super::crt_dll_of_exe();
    }
}
