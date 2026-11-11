//! T-020: libghostty-vt を Rust から呼ぶ最小のスパイク。
//! 端末を作り、文字列を書き、カーソルの桁を読む。eaw（`①` など）の桁数もここで分かる。

use std::ffi::{CStr, c_char, c_int, c_void};

#[allow(non_camel_case_types)]
type GhosttyTerminal = *mut c_void;

const SUCCESS: c_int = 0;
const DATA_COLS: c_int = 1;
const DATA_CURSOR_X: c_int = 3;

unsafe extern "C" {
    fn ghostty_version() -> *const c_char;
    fn ghostty_unicode_codepoint_width(cp: u32) -> u8;
    fn ghostty_terminal_new(alloc: *const c_void, out: *mut GhosttyTerminal, cols: u16, rows: u16) -> c_int;
    fn ghostty_terminal_free(t: GhosttyTerminal);
    fn ghostty_terminal_vt_write(t: GhosttyTerminal, data: *const u8, len: usize);
    fn ghostty_terminal_get(t: GhosttyTerminal, data: c_int, out: *mut c_void) -> c_int;
}

fn cursor_x(t: GhosttyTerminal) -> u16 {
    let mut x: u16 = 0;
    let r = unsafe { ghostty_terminal_get(t, DATA_CURSOR_X, &mut x as *mut u16 as *mut c_void) };
    assert_eq!(r, SUCCESS);
    x
}

fn main() {
    let ver = unsafe { CStr::from_ptr(ghostty_version()) };
    println!("libghostty-vt {}", ver.to_string_lossy());

    // 1 文字ずつの幅（端末が使う表と同じもの）
    for ch in ['a', 'あ', '①', '★', '※', '─', '│', '○', '…', '😀', '\u{e0b0}'] {
        let w = unsafe { ghostty_unicode_codepoint_width(ch as u32) };
        println!("width U+{:04X} {ch} = {w}", ch as u32);
    }

    let mut t: GhosttyTerminal = std::ptr::null_mut();
    let r = unsafe { ghostty_terminal_new(std::ptr::null(), &mut t, 80, 24) };
    assert_eq!(r, SUCCESS, "ghostty_terminal_new");
    let mut cols: u16 = 0;
    unsafe { ghostty_terminal_get(t, DATA_COLS, &mut cols as *mut u16 as *mut c_void) };
    println!("cols = {cols}");

    // 書いてはカーソルの桁を読む
    for s in ["a", "あ", "①", "★", "\x1b[31m色\x1b[0m", "\r\n"] {
        let before = cursor_x(t);
        unsafe { ghostty_terminal_vt_write(t, s.as_ptr(), s.len()) };
        println!("write {:?}: cursor x {} -> {}", s, before, cursor_x(t));
    }
    unsafe { ghostty_terminal_free(t) };
    println!("ok");
}
