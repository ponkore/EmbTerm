//! キー入力の最小の変換（レガシーの VT）。win32-input-mode は T-069 で作る。
//!
//! - 文字は UTF-8 にしてそのまま送る。Ctrl+文字は制御文字（0x00〜0x1f）、Alt（Meta）+文字は ESC を前に付ける
//! - 特殊キーは libghostty-vt のキーエンコーダで、端末のモード（DECCKM など）に合わせて符号化する
//!
//! キーの来かたは 2 通りある。
//! - embterm.exe: `WM_KEYDOWN` の仮想キー（[`special_key`]）と `WM_CHAR`。Enter・Backspace・Tab・Esc は
//!   エンコーダで送り、続いて来る `WM_CHAR` は捨てる（[`produces_char`]。Backspace を 0x7f にするため）
//! - Emacs module: Emacs のイベントの名前（[`key_by_name`]。`up` `return` `f5` など）か 1 文字と、修飾キー

use et_vt::{Key, Mods};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// 仮想キーを特殊キーにする。文字のキーなら `None`（`WM_CHAR` で送る）
pub fn special_key(vk: u16) -> Option<Key> {
    let vk = VIRTUAL_KEY(vk);
    Some(match vk {
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_INSERT => Key::Insert,
        VK_DELETE => Key::Delete,
        VK_RETURN => Key::Enter,
        VK_BACK => Key::Backspace,
        VK_TAB => Key::Tab,
        VK_ESCAPE => Key::Escape,
        _ if (VK_F1.0..=VK_F12.0).contains(&vk.0) => Key::F((vk.0 - VK_F1.0 + 1) as u8),
        _ => return None,
    })
}

/// 続いて来る `WM_CHAR` / `WM_SYSCHAR` を捨てるキーか（エンコーダで送り済みのもの）
pub fn produces_char(key: Key) -> bool {
    matches!(key, Key::Enter | Key::Backspace | Key::Tab | Key::Escape)
}

fn down(vk: VIRTUAL_KEY) -> bool {
    (unsafe { GetKeyState(vk.0 as i32) } as u16 & 0x8000) != 0
}

/// 今の修飾キーの状態（`WM_KEYDOWN` を受けたスレッドで読む）
pub fn mods(alt_from_message: bool) -> Mods {
    Mods { shift: down(VK_SHIFT), ctrl: down(VK_CONTROL), alt: alt_from_message || down(VK_MENU) }
}

/// Emacs のファンクションキーの名前（`event-basic-type` の名前）を特殊キーにする。
/// `backtab`（Shift+Tab）は Tab と Shift の組にする
pub fn key_by_name(name: &str) -> Option<(Key, bool)> {
    let key = match name {
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "prior" | "pageup" => Key::PageUp,
        "next" | "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" | "deletechar" => Key::Delete,
        "return" | "enter" | "kp-enter" => Key::Enter,
        "backspace" => Key::Backspace,
        "tab" => Key::Tab,
        "backtab" => return Some((Key::Tab, true)),
        "escape" => Key::Escape,
        _ => {
            let n: u8 = name.strip_prefix('f')?.parse().ok()?;
            if !(1..=12).contains(&n) {
                return None;
            }
            Key::F(n)
        }
    };
    Some((key, false))
}

/// 文字 1 つを、修飾キー付きでレガシーの VT の列にする。
/// - Ctrl: `@`〜`_` と英字は 0x00〜0x1f、空白は NUL、`/` は 0x1f、`?` は 0x7f。それ以外の文字は Ctrl を無視する
/// - Shift: Ctrl が無い英小文字は大文字にする（Emacs は `S-a` のように分けて渡すことがある）
/// - Alt（Meta）: ESC を前に付ける
pub fn encode_char(ch: char, mods: Mods) -> Vec<u8> {
    let mut c = ch;
    if mods.shift && !mods.ctrl && c.is_ascii_lowercase() {
        c = c.to_ascii_uppercase();
    }
    if mods.ctrl {
        c = match c {
            ' ' | '@' | '2' => '\0',
            '/' | '_' | '7' => '\x1f',
            '?' | '8' => '\x7f',
            '3' | '[' => '\x1b',
            '4' | '\\' => '\x1c',
            '5' | ']' => '\x1d',
            '6' | '^' | '~' => '\x1e',
            'a'..='z' => char::from(c as u8 - b'a' + 1),
            'A'..='Z' => char::from(c as u8 - b'A' + 1),
            _ => c,
        };
    }
    let mut out = Vec::with_capacity(5);
    if mods.alt {
        out.push(0x1b);
    }
    let mut tmp = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
    out
}

/// 修飾キーの文字列（`C` = Ctrl、`M` = Alt / Meta、`S` = Shift。順不同）を [`Mods`] にする
pub fn parse_mods(s: &str) -> Mods {
    Mods { ctrl: s.contains('C'), alt: s.contains('M'), shift: s.contains('S') }
}

/// 貼り付ける文字列を PTY に送る形にする。改行は CR にし（端末への貼り付けの慣習）、
/// ブラケットペーストのモードなら `ESC [200~` 〜 `ESC [201~` で包む（中の終わりの印は取り除く）
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    let mut out = Vec::with_capacity(text.len() + 12);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.replace("\x1b[201~", "").as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(text.as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Mods {
        parse_mods(s)
    }

    #[test]
    fn chars_with_modifiers() {
        assert_eq!(encode_char('a', m("")), b"a");
        assert_eq!(encode_char('a', m("S")), b"A");
        assert_eq!(encode_char('a', m("C")), b"\x01");
        assert_eq!(encode_char('z', m("CS")), b"\x1a");
        assert_eq!(encode_char(' ', m("C")), b"\0");
        assert_eq!(encode_char('/', m("C")), b"\x1f");
        assert_eq!(encode_char('x', m("M")), b"\x1bx");
        assert_eq!(encode_char('a', m("CM")), b"\x1b\x01");
        assert_eq!(encode_char('あ', m("")), "あ".as_bytes());
        assert_eq!(encode_char('①', m("C")), "①".as_bytes());
    }

    #[test]
    fn names() {
        assert_eq!(key_by_name("up"), Some((Key::Up, false)));
        assert_eq!(key_by_name("prior"), Some((Key::PageUp, false)));
        assert_eq!(key_by_name("deletechar"), Some((Key::Delete, false)));
        assert_eq!(key_by_name("backtab"), Some((Key::Tab, true)));
        assert_eq!(key_by_name("f12"), Some((Key::F(12), false)));
        assert_eq!(key_by_name("f13"), None);
        assert_eq!(key_by_name("a"), None);
    }

    #[test]
    fn paste() {
        assert_eq!(paste_bytes("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~");
    }
}
