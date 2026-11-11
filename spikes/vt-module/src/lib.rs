//! T-020: libghostty-vt を静的リンクした Emacs module のスパイク。

use emacs::{Env, Result, defun};
use std::ffi::{CStr, c_char, c_int, c_void};

emacs::plugin_is_GPL_compatible!();

#[emacs::module(name = "spike-vt-module", defun_prefix = "spike-vt", separator = "-")]
fn init(_: &Env) -> Result<()> {
    Ok(())
}

unsafe extern "C" {
    fn ghostty_version() -> *const c_char;
    fn ghostty_terminal_new(alloc: *const c_void, out: *mut *mut c_void, cols: u16, rows: u16) -> c_int;
    fn ghostty_terminal_free(t: *mut c_void);
    fn ghostty_terminal_vt_write(t: *mut c_void, data: *const u8, len: usize);
    fn ghostty_terminal_get(t: *mut c_void, data: c_int, out: *mut c_void) -> c_int;
}

/// libghostty-vt の版を返す。
#[defun]
fn version() -> Result<String> {
    Ok(unsafe { CStr::from_ptr(ghostty_version()) }.to_string_lossy().into_owned())
}

/// S を 80x24 の端末に書いたあとのカーソルの桁を返す。
#[defun]
fn cursor_after(s: String) -> Result<i64> {
    unsafe {
        let mut t = std::ptr::null_mut();
        assert_eq!(ghostty_terminal_new(std::ptr::null(), &mut t, 80, 24), 0);
        ghostty_terminal_vt_write(t, s.as_ptr(), s.len());
        let mut x: u16 = 0;
        ghostty_terminal_get(t, 3, &mut x as *mut u16 as *mut c_void);
        ghostty_terminal_free(t);
        Ok(x as i64)
    }
}
