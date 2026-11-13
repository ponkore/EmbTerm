//! キーの変換のうち、OS に依らない部分（T-126）。
//!
//! Windows（[`crate::keys`]）と macOS（[`crate::keys_mac`]）の両方が使う。

use et_vt::Mods;

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

