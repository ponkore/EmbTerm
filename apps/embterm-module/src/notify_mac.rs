//! module → Lisp の知らせ（macOS。T-126）。
//!
//! Windows 版（[`crate::notify`]）と同じ役目で、ずっと単純になる。Lisp は端末の状態をポーリングで読むが、
//! 50 ms ごとに全端末を見回ると何もしていないときでも CPU を食う（T-104）。そこで、状態が変わったら
//! Emacs の `open_channel`（Emacs 28〜）のパイプに 1 バイト書き、Lisp に読みに来させる。
//!
//! Windows では「module の CRT と Emacs の CRT が違う」ための工夫（`_get_osfhandle` を Emacs の CRT から取る）が
//! 要ったが、**Unix では `open_channel` が返すのはそのまま OS のファイル記述子**なので `write(2)` するだけでよい。
//! 記述子は閉じない（Emacs 1 つにつき 1 本）。

use std::ffi::c_int;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use emacs::Env;
use emacs::raw::emacs_env;

static CHANNEL: Mutex<Option<c_int>> = Mutex::new(None);
/// 知らせを書いたが、Lisp がまだ読み始めていない
static PENDING: AtomicBool = AtomicBool::new(false);

/// `emacs_funcall_exit_return`（非局所脱出なし）
const EXIT_RETURN: u32 = 0;

/// 状態が変わったことを Lisp に知らせる（どのスレッドからでも。Lisp が読み始めるまでは 1 回だけ書く）
pub fn notify() {
    if PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    let fd = *CHANNEL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(fd) = fd else {
        PENDING.store(false, Ordering::Release);
        return;
    };
    let byte = b"\n";
    // SAFETY: パイプの書き込み口。1 バイトだけ書く（満杯なら諦める）
    let n = unsafe { libc::write(fd, byte.as_ptr() as *const _, 1) };
    if n != 1 {
        PENDING.store(false, Ordering::Release);
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
        *CHANNEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(fd);
        PENDING.store(false, Ordering::Release);
        Ok(())
    }
}
