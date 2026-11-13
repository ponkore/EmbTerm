//! macOS のキーの変換（T-126）。
//!
//! Emacs が受けたキー（`embterm.el` が送る「キーの名前 + 修飾キー」）を PTY に送るバイト列にする。
//!
//! - アプリが **KKP**（Kitty keyboard protocol）を求めていれば libghostty-vt のエンコーダ
//!   （[`et_vt::KeyEncoder::encode_input`]）、そうでなければレガシーの VT（[`crate::keys_common`]）
//! - Windows 版（[`crate::keys`]）との違い: **win32-input-mode は無い**（ConPTY のためのもの）。
//!   仮想キー・スキャンコードも要らないので、Emacs のキーの名前から直に組み立てる
//! - 押したときだけ送る（Emacs からは離したキーが分からない。KKP の「離した」の報告は満たせない。T-009 と同じ）

use et_vt::{Key, KeyAction, KeyEncoder, KeyInput, Mods, Terminal};

pub use crate::keys_common::{encode_char, parse_mods, paste_bytes};

/// Emacs のファンクションキーの名前（`event-basic-type` の名前）を特殊キーにする。
/// `backtab`（Shift+Tab）は Tab + Shift にする
pub fn key_by_name(name: &str) -> Option<(Key, bool)> {
    let k = |k: Key| Some((k, false));
    match name {
        "up" | "kp-up" => k(Key::Up),
        "down" | "kp-down" => k(Key::Down),
        "left" | "kp-left" => k(Key::Left),
        "right" | "kp-right" => k(Key::Right),
        "home" | "kp-home" | "begin" => k(Key::Home),
        "end" | "kp-end" => k(Key::End),
        "prior" | "pageup" | "kp-prior" => k(Key::PageUp),
        "next" | "pagedown" | "kp-next" => k(Key::PageDown),
        "insert" | "kp-insert" => k(Key::Insert),
        "delete" | "deletechar" | "kp-delete" => k(Key::Delete),
        "return" | "enter" | "kp-enter" => k(Key::Enter),
        "backspace" => k(Key::Backspace),
        "tab" | "kp-tab" => k(Key::Tab),
        "backtab" => Some((Key::Tab, true)),
        "escape" => k(Key::Escape),
        _ => {
            let n = name.strip_prefix('f')?.parse::<u8>().ok()?;
            (1..=24).contains(&n).then_some((Key::F(n), false))
        }
    }
}

/// 特殊キーの KKP の文字（KKP では「キーが作る文字」も送る）
fn key_text(key: Key) -> &'static str {
    match key {
        Key::Enter => "\r",
        Key::Tab => "\t",
        Key::Backspace => "\x7f",
        Key::Escape => "\x1b",
        _ => "",
    }
}

/// libghostty-vt の KKP のエンコーダに渡す形（特殊キー）
fn kitty_special(key: Key, mods: Mods) -> KeyInput<'static> {
    let ghostty_key = match key {
        Key::Up => et_vt::ffi::GHOSTTY_KEY_ARROW_UP,
        Key::Down => et_vt::ffi::GHOSTTY_KEY_ARROW_DOWN,
        Key::Left => et_vt::ffi::GHOSTTY_KEY_ARROW_LEFT,
        Key::Right => et_vt::ffi::GHOSTTY_KEY_ARROW_RIGHT,
        Key::Home => et_vt::ffi::GHOSTTY_KEY_HOME,
        Key::End => et_vt::ffi::GHOSTTY_KEY_END,
        Key::PageUp => et_vt::ffi::GHOSTTY_KEY_PAGE_UP,
        Key::PageDown => et_vt::ffi::GHOSTTY_KEY_PAGE_DOWN,
        Key::Insert => et_vt::ffi::GHOSTTY_KEY_INSERT,
        Key::Delete => et_vt::ffi::GHOSTTY_KEY_DELETE,
        Key::Enter => et_vt::ffi::GHOSTTY_KEY_ENTER,
        Key::Backspace => et_vt::ffi::GHOSTTY_KEY_BACKSPACE,
        Key::Tab => et_vt::ffi::GHOSTTY_KEY_TAB,
        Key::Escape => et_vt::ffi::GHOSTTY_KEY_ESCAPE,
        Key::F(n) => et_vt::ffi::GHOSTTY_KEY_F1 + (n as i32 - 1),
    };
    KeyInput {
        key: ghostty_key,
        action: KeyAction::Press,
        mods,
        consumed_shift: false,
        text: key_text(key),
        unshifted: 0,
    }
}

/// 文字キーを KKP のエンコーダに渡す形にする
fn kitty_char<'a>(ch: char, mods: Mods, tmp: &'a mut [u8; 4]) -> KeyInput<'a> {
    // 物理キーは分からない（Emacs のイベントには無い）ので、文字そのものを unshifted として渡す。
    // libghostty-vt は key が UNIDENTIFIED でも text / unshifted から組み立てる
    let lower = ch.to_lowercase().next().unwrap_or(ch);
    let text: &str = if mods.ctrl || mods.alt {
        // Ctrl / Alt を足す前の文字を渡す（エンコーダが修飾キーを付ける）
        ""
    } else {
        ch.encode_utf8(tmp)
    };
    KeyInput {
        key: et_vt::ffi::GHOSTTY_KEY_UNIDENTIFIED,
        action: KeyAction::Press,
        mods,
        consumed_shift: mods.shift && ch.is_uppercase(),
        text,
        unshifted: lower as u32,
    }
}

/// Emacs のキー（名前か 1 文字）と修飾キーを、いま端末が求めている形でバイト列にする。
/// **端末のロックを持って呼ぶ**（モードを読むため）
pub fn encode_emacs_key(term: &Terminal, encoder: &mut KeyEncoder, key: &str, mods: &str) -> Vec<u8> {
    let mut mods = parse_mods(mods);
    let kitty = term.kitty_keyboard_flags() != 0;

    if let Some((k, shift)) = key_by_name(key) {
        if shift {
            mods.shift = true;
        }
        return if kitty { encoder.encode_input(term, &kitty_special(k, mods)) } else { encoder.encode(term, k, mods) };
    }

    let mut chars = key.chars();
    let (Some(ch), None) = (chars.next(), chars.next()) else {
        // 1 文字でも知っている名前でもないもの（押しただけの修飾キーなど）は送らない
        return Vec::new();
    };
    if kitty {
        let mut tmp = [0u8; 4];
        return encoder.encode_input(term, &kitty_char(ch, mods, &mut tmp));
    }
    encode_char(ch, mods)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 名前から特殊キー() {
        assert_eq!(key_by_name("up"), Some((Key::Up, false)));
        assert_eq!(key_by_name("f12"), Some((Key::F(12), false)));
        assert_eq!(key_by_name("backtab"), Some((Key::Tab, true)));
        assert_eq!(key_by_name("f25"), None);
        assert_eq!(key_by_name("a"), None);
    }

    #[test]
    fn レガシーの文字() {
        let m = |s: &str| parse_mods(s);
        assert_eq!(encode_char('a', m("")), b"a");
        assert_eq!(encode_char('a', m("C")), b"\x01");
        assert_eq!(encode_char('a', m("M")), b"\x1ba");
        assert_eq!(encode_char(' ', m("C")), b"\0");
        assert_eq!(encode_char('あ', m("M")), "\x1bあ".as_bytes());
    }
}
