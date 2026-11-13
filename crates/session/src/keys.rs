//! キー入力の変換（T-069）。アプリの求めに合わせて 3 通りの形で送る（Windows Terminal と同じ切り替え。[`crate::Session::key_protocol`]）。
//!
//! 1. **KKP**（Kitty keyboard protocol）: libghostty-vt のエンコーダ（[`et_vt::KeyEncoder::encode_input`]）
//! 2. **win32-input-mode**（`CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`。`KEY_EVENT_RECORD` そのもの）: 自前（[`KeyEvent::win32`]）
//! 3. **レガシーの VT**: 文字は UTF-8、Ctrl は制御文字、Alt は ESC を前置。特殊キーは libghostty-vt のエンコーダ
//!
//! キーの来かたは 2 通りある。どちらも [`KeyEvent`] に揃えて、送り方ごとの形を作る。
//! - embterm.exe（[`KeyEvent::from_win32`]）: `WM_KEYDOWN` の本物の仮想キー・スキャンコード
//! - Emacs module（[`KeyEvent::from_emacs`]）: Emacs のイベント（1 文字か `up` `return` `f5` など）+ 修飾キー。
//!   `VkKeyScanW` / `MapVirtualKeyW` で作り直す（キーを離したイベント・AltGr・テンキーの区別は無い）。
//!   キーボードで打てない文字（IME の確定文字、絵文字）は仮想キー 0 と UTF-16 の 1 単位ずつで送る
//!
//! 貼り付けなどの長い文字列は win32-input-mode にしない（1 文字 2 列で重い）。[`paste_bytes`] のとおり素の文字列で送る。

use et_vt::{KeyAction, KeyEncoder, KeyInput, Terminal};
pub use et_vt::{Key, Mods};
pub use crate::keys_common::{encode_char, parse_mods, paste_bytes};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// キーの送り方（[`crate::Session::key_protocol`]）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyProtocol {
    Kitty,
    Win32,
    Legacy,
}

/// キーの動き
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyPhase {
    Press,
    Repeat,
    Release,
    /// 押して離す（Emacs から来たキー。離したイベントは無いので合成する）
    Tap,
}

/// レガシーの VT で送るときの形
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Legacy {
    Special(Key),
    Char(char),
    None,
}

// dwControlKeyState の値
const RIGHT_ALT_PRESSED: u32 = 0x0001;
const LEFT_ALT_PRESSED: u32 = 0x0002;
const RIGHT_CTRL_PRESSED: u32 = 0x0004;
const LEFT_CTRL_PRESSED: u32 = 0x0008;
const SHIFT_PRESSED: u32 = 0x0010;
const NUMLOCK_ON: u32 = 0x0020;
const SCROLLLOCK_ON: u32 = 0x0040;
const CAPSLOCK_ON: u32 = 0x0080;
const ENHANCED_KEY: u32 = 0x0100;

/// キーのイベント 1 つ。[`encode`] が送り方ごとのバイト列にする
#[derive(Clone, Copy, Debug)]
pub struct KeyEvent {
    /// 仮想キー。0 はキーボードで打てない文字（`VK_PACKET` 相当。`text` を UTF-16 で送る）
    pub vk: u16,
    /// スキャンコード（拡張キーのプレフィックスは含めない）
    pub scan: u16,
    /// 拡張キー（矢印など）。`dwControlKeyState` の `ENHANCED_KEY` になる
    pub extended: bool,
    /// Ctrl / Alt / Shift の状態（VkKeyScan が必要とする Shift を含む）
    pub mods: Mods,
    /// `dwControlKeyState`（[`Self::finish_state`] で作る）
    pub control_state: u32,
    /// キーが作る文字（Ctrl / Alt を足す前。Shift は反映済み）。特殊キーは `None`
    pub text: Option<char>,
    /// Shift を外したときの文字（0 は無し）
    pub unshifted: u32,
    /// キーを押すと文字（`WM_CHAR`）が作られるか（embterm.exe で続く `WM_CHAR` を捨てるため）
    pub produces_char: bool,
    /// 繰り返しの回数（`KEY_EVENT_RECORD.wRepeatCount`）
    pub repeat: u16,
    legacy: Legacy,
    /// レガシーの VT で使う修飾キー（VkKeyScan が足した Shift は含めない）
    legacy_mods: Mods,
}

/// 今のキーボード配列（前面のウィンドウ = Emacs のスレッドのもの）
fn keyboard_layout() -> HKL {
    unsafe {
        let hwnd = GetForegroundWindow();
        let thread = if hwnd.is_invalid() { 0 } else { GetWindowThreadProcessId(hwnd, None) };
        GetKeyboardLayout(thread)
    }
}

/// 仮想キー `vk` と修飾キーが作る文字（UTF-16）。特殊キーや死にキーでは空。`keys` は Win32 の仮想キーの状態の配列
fn to_unicode(vk: u16, scan: u16, keys: &[u8; 256]) -> Vec<u16> {
    let mut buf = [0u16; 8];
    // フラグ 0x4: キーボードの状態（死にキー）を変えない（Windows 10 1607 以降）
    let n = unsafe { ToUnicodeEx(vk as u32, scan as u32, keys, &mut buf, 0x4, Some(keyboard_layout())) };
    if n > 0 { buf[..(n as usize).min(buf.len())].to_vec() } else { Vec::new() }
}

/// [`Mods`] と AltGr から仮想キーの状態の配列を作る
fn key_state(mods: Mods, altgr: bool) -> [u8; 256] {
    let mut s = [0u8; 256];
    if mods.shift {
        s[VK_SHIFT.0 as usize] = 0x80;
        s[VK_LSHIFT.0 as usize] = 0x80;
    }
    if mods.ctrl || altgr {
        s[VK_CONTROL.0 as usize] = 0x80;
        s[VK_LCONTROL.0 as usize] = 0x80;
    }
    if mods.alt && !altgr {
        s[VK_MENU.0 as usize] = 0x80;
        s[VK_LMENU.0 as usize] = 0x80;
    }
    if altgr {
        s[VK_MENU.0 as usize] = 0x80;
        s[VK_RMENU.0 as usize] = 0x80;
    }
    s
}

fn scan_code(vk: u16) -> u16 {
    unsafe { MapVirtualKeyExW(vk as u32, MAPVK_VK_TO_VSC, Some(keyboard_layout())) as u16 }
}

fn control_state(mods: Mods, altgr: bool, extended: bool) -> u32 {
    let mut cs = 0;
    if mods.shift {
        cs |= SHIFT_PRESSED;
    }
    if mods.ctrl && !altgr {
        cs |= LEFT_CTRL_PRESSED;
    }
    if mods.alt && !altgr {
        cs |= LEFT_ALT_PRESSED;
    }
    if altgr {
        cs |= RIGHT_ALT_PRESSED | LEFT_CTRL_PRESSED;
    }
    if extended {
        cs |= ENHANCED_KEY;
    }
    cs
}

/// `name` のキーの定義（Emacs のファンクションキーの名前）
struct NamedKey {
    vk: VIRTUAL_KEY,
    extended: bool,
    shift: bool,
    legacy: Legacy,
}

fn named_key(name: &str) -> Option<NamedKey> {
    use Legacy::{Char, Special};
    fn k(vk: VIRTUAL_KEY, extended: bool, legacy: Legacy) -> Option<NamedKey> {
        Some(NamedKey { vk, extended, shift: false, legacy })
    }
    match name {
        "up" => k(VK_UP, true, Special(Key::Up)),
        "down" => k(VK_DOWN, true, Special(Key::Down)),
        "left" => k(VK_LEFT, true, Special(Key::Left)),
        "right" => k(VK_RIGHT, true, Special(Key::Right)),
        "home" => k(VK_HOME, true, Special(Key::Home)),
        "end" => k(VK_END, true, Special(Key::End)),
        "prior" | "pageup" => k(VK_PRIOR, true, Special(Key::PageUp)),
        "next" | "pagedown" => k(VK_NEXT, true, Special(Key::PageDown)),
        "insert" => k(VK_INSERT, true, Special(Key::Insert)),
        "delete" | "deletechar" => k(VK_DELETE, true, Special(Key::Delete)),
        "return" | "enter" => k(VK_RETURN, false, Special(Key::Enter)),
        "kp-enter" => k(VK_RETURN, true, Special(Key::Enter)),
        "backspace" => k(VK_BACK, false, Special(Key::Backspace)),
        "tab" | "kp-tab" => k(VK_TAB, false, Special(Key::Tab)),
        "backtab" => Some(NamedKey { vk: VK_TAB, extended: false, shift: true, legacy: Special(Key::Tab) }),
        "escape" => k(VK_ESCAPE, false, Special(Key::Escape)),
        "kp-home" => k(VK_HOME, false, Special(Key::Home)),
        "kp-end" => k(VK_END, false, Special(Key::End)),
        "kp-prior" => k(VK_PRIOR, false, Special(Key::PageUp)),
        "kp-next" => k(VK_NEXT, false, Special(Key::PageDown)),
        "kp-up" => k(VK_UP, false, Special(Key::Up)),
        "kp-down" => k(VK_DOWN, false, Special(Key::Down)),
        "kp-left" => k(VK_LEFT, false, Special(Key::Left)),
        "kp-right" => k(VK_RIGHT, false, Special(Key::Right)),
        "kp-insert" => k(VK_INSERT, false, Special(Key::Insert)),
        "kp-delete" => k(VK_DELETE, false, Special(Key::Delete)),
        "kp-begin" | "kp-5-nl" => k(VK_CLEAR, false, Legacy::None),
        "kp-add" => k(VK_ADD, false, Char('+')),
        "kp-subtract" => k(VK_SUBTRACT, false, Char('-')),
        "kp-multiply" => k(VK_MULTIPLY, false, Char('*')),
        "kp-divide" => k(VK_DIVIDE, true, Char('/')),
        "kp-decimal" => k(VK_DECIMAL, false, Char('.')),
        "kp-separator" => k(VK_SEPARATOR, false, Char(',')),
        "print" => k(VK_SNAPSHOT, true, Legacy::None),
        "pause" => k(VK_PAUSE, false, Legacy::None),
        "scroll-lock" => k(VK_SCROLL, false, Legacy::None),
        "apps" | "menu" => k(VK_APPS, true, Legacy::None),
        _ => {
            if let Some(d) = name.strip_prefix("kp-").and_then(|d| d.parse::<u8>().ok()).filter(|d| *d <= 9) {
                return k(VIRTUAL_KEY(VK_NUMPAD0.0 + d as u16), false, Char((b'0' + d) as char));
            }
            let n: u8 = name.strip_prefix('f')?.parse().ok()?;
            if !(1..=24).contains(&n) {
                return None;
            }
            k(VIRTUAL_KEY(VK_F1.0 + n as u16 - 1), false, Special(Key::F(n)))
        }
    }
}

impl KeyEvent {
    fn named(n: NamedKey, mut mods: Mods) -> Self {
        let legacy_mods = Mods { shift: mods.shift || n.shift, ..mods };
        mods.shift |= n.shift;
        let units = to_unicode(n.vk.0, 0, &key_state(mods, false));
        Self {
            vk: n.vk.0,
            scan: scan_code(n.vk.0),
            extended: n.extended,
            mods,
            control_state: control_state(mods, false, n.extended),
            text: None,
            unshifted: 0,
            produces_char: !units.is_empty(),
            repeat: 1,
            legacy: n.legacy,
            legacy_mods,
        }
    }

    /// Emacs のイベント（`key` は 1 文字か、ファンクションキーの名前）から作る。知らないキーは `None`
    pub fn from_emacs(key: &str, mods: Mods) -> Option<Self> {
        let mut it = key.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => Some(Self::from_char(c, mods)),
            _ => named_key(key).map(|n| Self::named(n, mods)),
        }
    }

    /// 文字 1 つから作る（Emacs の文字のイベント、IME の確定文字）。キーボードで打てない文字は仮想キー 0
    pub fn from_char(c: char, mods: Mods) -> Self {
        // Emacs が制御文字のまま渡してきたとき（C-a など）は「Ctrl + 文字」に直す。Backspace などは特殊キー
        match c {
            '\u{7f}' | '\x08' if !mods.ctrl => return Self::named(named_key("backspace").expect("定義済み"), mods),
            '\r' | '\n' if !mods.ctrl => return Self::named(named_key("return").expect("定義済み"), mods),
            '\t' if !mods.ctrl => return Self::named(named_key("tab").expect("定義済み"), mods),
            '\x1b' if !mods.ctrl => return Self::named(named_key("escape").expect("定義済み"), mods),
            '\0'..='\x1f' if !mods.ctrl => {
                let base = char::from(c as u8 + 0x60);
                return Self::from_char(base, Mods { ctrl: true, ..mods });
            }
            _ => {}
        }
        // Emacs が `S-a` のように分けて渡したときは、Shift を文字に反映する
        let c = if mods.shift && !mods.ctrl && c.is_lowercase() { c.to_uppercase().next().unwrap_or(c) } else { c };
        let layout = keyboard_layout();
        let scan = if (c as u32) < 0x10000 { unsafe { VkKeyScanExW(c as u16, layout) } } else { -1 };
        let (vk, shift_bits) = ((scan as u16 & 0xff) as u8, ((scan as u16) >> 8) as u8);
        // VkKeyScanEx の上位バイト: 1 = Shift、2 = Ctrl、4 = Alt、8 = ひらがな/カタカナ（それ以外は打てない扱い）
        if scan == -1 || vk == 0xff || shift_bits & !0x7 != 0 {
            return Self {
                vk: 0,
                scan: 0,
                extended: false,
                mods,
                control_state: control_state(mods, false, false),
                text: Some(c),
                unshifted: 0,
                produces_char: true,
                repeat: 1,
                legacy: Legacy::Char(c),
                legacy_mods: mods,
            };
        }
        let vk16 = vk as u16;
        let altgr = shift_bits & 6 == 6;
        let mut full = mods;
        full.shift |= shift_bits & 1 != 0;
        let unshifted = to_unicode(vk16, scan_code(vk16), &key_state(Mods::default(), false)).first().map_or(0, |&u| {
            char::from_u32(u as u32).map_or(u as u32, |ch| ch.to_lowercase().next().map_or(u as u32, |l| l as u32))
        });
        Self {
            vk: vk16,
            scan: scan_code(vk16),
            extended: false,
            mods: full,
            control_state: control_state(full, altgr, false),
            text: Some(c),
            unshifted,
            produces_char: true,
            repeat: 1,
            legacy: Legacy::Char(c),
            legacy_mods: mods,
        }
    }

    /// `WM_KEYDOWN` / `WM_KEYUP` から作る（embterm.exe）。`repeat` は押しっぱなしの回数
    pub fn from_win32(vk: u16, scan: u16, extended: bool, repeat: u16) -> Self {
        let mut keys = [0u8; 256];
        unsafe {
            let _ = GetKeyboardState(&mut keys);
        }
        let down = |v: VIRTUAL_KEY| keys[v.0 as usize] & 0x80 != 0;
        let mods = Mods { shift: down(VK_SHIFT), ctrl: down(VK_CONTROL), alt: down(VK_MENU) };
        let mut cs = 0;
        if mods.shift {
            cs |= SHIFT_PRESSED;
        }
        if down(VK_LCONTROL) {
            cs |= LEFT_CTRL_PRESSED;
        }
        if down(VK_RCONTROL) {
            cs |= RIGHT_CTRL_PRESSED;
        }
        if down(VK_LMENU) {
            cs |= LEFT_ALT_PRESSED;
        }
        if down(VK_RMENU) {
            cs |= RIGHT_ALT_PRESSED;
        }
        if extended {
            cs |= ENHANCED_KEY;
        }
        if keys[VK_NUMLOCK.0 as usize] & 1 != 0 {
            cs |= NUMLOCK_ON;
        }
        if keys[VK_CAPITAL.0 as usize] & 1 != 0 {
            cs |= CAPSLOCK_ON;
        }
        if keys[VK_SCROLL.0 as usize] & 1 != 0 {
            cs |= SCROLLLOCK_ON;
        }
        let units = to_unicode(vk, scan, &keys);
        let text = (units.len() == 1).then(|| units[0]).and_then(|u| char::from_u32(u as u32)).filter(|c| !c.is_control());
        let mut plain = keys;
        for v in [VK_SHIFT, VK_LSHIFT, VK_RSHIFT, VK_CONTROL, VK_LCONTROL, VK_RCONTROL, VK_MENU, VK_LMENU, VK_RMENU] {
            plain[v.0 as usize] = 0;
        }
        let unshifted = text.map_or(0, |_| to_unicode(vk, scan, &plain).first().copied().unwrap_or(0) as u32);
        let legacy = match special_key(vk) {
            Some(k) => Legacy::Special(k),
            None => text.map_or(Legacy::None, Legacy::Char),
        };
        Self {
            vk,
            scan,
            extended,
            mods,
            control_state: cs,
            text,
            unshifted,
            produces_char: !units.is_empty(),
            repeat: repeat.max(1),
            legacy,
            legacy_mods: mods,
        }
    }

    /// 文字が仮想キーを持たない（IME の確定文字など）。`VK_PACKET` 相当
    pub fn is_packet(&self) -> bool {
        self.vk == 0
    }

    /// win32-input-mode の列にする。`key_down` は押した / 離した
    fn win32(&self, key_down: bool) -> Vec<u8> {
        let mut units = [0u16; 2];
        let n: &[u16] = match self.text {
            // キーボードで打てない文字（Vk = 0）は UTF-16 の 1 単位ずつ
            Some(c) if self.vk == 0 => c.encode_utf16(&mut units),
            // 修飾なしの文字（Shift は反映済み）はそのまま
            Some(c) if !self.mods.ctrl && !self.mods.alt => c.encode_utf16(&mut units),
            _ => {
                let v = self.unicode_units();
                let len = v.len().min(1);
                units[..len].copy_from_slice(&v[..len]);
                &units[..len]
            }
        };
        let mut out = Vec::with_capacity(32);
        let kd = key_down as u8;
        for &uc in if n.is_empty() { &[0u16][..] } else { n } {
            let vk = self.vk;
            let sc = self.scan;
            let cs = self.control_state;
            let rc = self.repeat;
            out.extend_from_slice(format!("\x1b[{vk};{sc};{uc};{kd};{cs};{rc}_").as_bytes());
        }
        out
    }

    /// 修飾キーも含めて、キーが作る文字（UTF-16）。ToUnicode に任せる（Ctrl+A = 1、Ctrl+Enter = 10 など、本物のキーと同じ値になる）
    fn unicode_units(&self) -> Vec<u16> {
        let altgr = self.control_state & (RIGHT_ALT_PRESSED | LEFT_CTRL_PRESSED) == (RIGHT_ALT_PRESSED | LEFT_CTRL_PRESSED);
        to_unicode(self.vk, self.scan, &key_state(self.mods, altgr))
    }

    /// GhosttyKey（物理キー）。文字のキーは VK から求める
    fn ghostty_key(&self) -> i32 {
        ghostty_key(self.vk, self.extended)
    }

    fn kitty_input<'a>(&self, action: KeyAction, tmp: &'a mut [u8; 4]) -> KeyInput<'a> {
        let text = self.text.map_or("", |c| c.encode_utf8(tmp));
        KeyInput {
            key: self.ghostty_key(),
            action,
            mods: self.mods,
            consumed_shift: self.mods.shift && self.text.is_some(),
            text,
            // キーボードで打てない文字（仮想キー 0）は物理キーが無いので、文字そのものをキーの番号にする
            // （KKP の決まり。M-あ が `CSI 12354 ; 3 u` になる。T-089）
            unshifted: if self.vk == 0 { self.text.map_or(0, |c| c as u32) } else { self.unshifted },
        }
    }
}

/// 仮想キーから GhosttyKey（物理キー）を求める
fn ghostty_key(vk: u16, extended: bool) -> i32 {
    use et_vt::ffi as f;
    const fn range(vk: u16, first: u16, base: i32) -> i32 {
        base + (vk - first) as i32
    }
    let k = VIRTUAL_KEY(vk);
    match k {
        VK_BACK => f::GHOSTTY_KEY_BACKSPACE,
        VK_TAB => f::GHOSTTY_KEY_TAB,
        VK_RETURN => {
            if extended {
                f::GHOSTTY_KEY_NUMPAD_ENTER
            } else {
                f::GHOSTTY_KEY_ENTER
            }
        }
        VK_ESCAPE => f::GHOSTTY_KEY_ESCAPE,
        VK_SPACE => f::GHOSTTY_KEY_SPACE,
        VK_PRIOR => numpad_or(extended, f::GHOSTTY_KEY_PAGE_UP, f::GHOSTTY_KEY_NUMPAD_PAGE_UP),
        VK_NEXT => numpad_or(extended, f::GHOSTTY_KEY_PAGE_DOWN, f::GHOSTTY_KEY_NUMPAD_PAGE_DOWN),
        VK_END => numpad_or(extended, f::GHOSTTY_KEY_END, f::GHOSTTY_KEY_NUMPAD_END),
        VK_HOME => numpad_or(extended, f::GHOSTTY_KEY_HOME, f::GHOSTTY_KEY_NUMPAD_HOME),
        VK_LEFT => numpad_or(extended, f::GHOSTTY_KEY_ARROW_LEFT, f::GHOSTTY_KEY_NUMPAD_LEFT),
        VK_UP => numpad_or(extended, f::GHOSTTY_KEY_ARROW_UP, f::GHOSTTY_KEY_NUMPAD_UP),
        VK_RIGHT => numpad_or(extended, f::GHOSTTY_KEY_ARROW_RIGHT, f::GHOSTTY_KEY_NUMPAD_RIGHT),
        VK_DOWN => numpad_or(extended, f::GHOSTTY_KEY_ARROW_DOWN, f::GHOSTTY_KEY_NUMPAD_DOWN),
        VK_INSERT => numpad_or(extended, f::GHOSTTY_KEY_INSERT, f::GHOSTTY_KEY_NUMPAD_INSERT),
        VK_DELETE => numpad_or(extended, f::GHOSTTY_KEY_DELETE, f::GHOSTTY_KEY_NUMPAD_DELETE),
        VK_SNAPSHOT => f::GHOSTTY_KEY_PRINT_SCREEN,
        VK_SCROLL => f::GHOSTTY_KEY_SCROLL_LOCK,
        VK_PAUSE => f::GHOSTTY_KEY_PAUSE,
        VK_APPS => f::GHOSTTY_KEY_CONTEXT_MENU,
        VK_ADD => f::GHOSTTY_KEY_NUMPAD_ADD,
        VK_SUBTRACT => f::GHOSTTY_KEY_NUMPAD_SUBTRACT,
        VK_MULTIPLY => f::GHOSTTY_KEY_NUMPAD_MULTIPLY,
        VK_DIVIDE => f::GHOSTTY_KEY_NUMPAD_DIVIDE,
        VK_DECIMAL => f::GHOSTTY_KEY_NUMPAD_DECIMAL,
        VK_OEM_MINUS => f::GHOSTTY_KEY_MINUS,
        VK_OEM_PLUS => f::GHOSTTY_KEY_EQUAL,
        VK_OEM_COMMA => f::GHOSTTY_KEY_COMMA,
        VK_OEM_PERIOD => f::GHOSTTY_KEY_PERIOD,
        VK_OEM_1 => f::GHOSTTY_KEY_SEMICOLON,
        VK_OEM_2 => f::GHOSTTY_KEY_SLASH,
        VK_OEM_3 => f::GHOSTTY_KEY_BACKQUOTE,
        VK_OEM_4 => f::GHOSTTY_KEY_BRACKET_LEFT,
        VK_OEM_5 => f::GHOSTTY_KEY_BACKSLASH,
        VK_OEM_6 => f::GHOSTTY_KEY_BRACKET_RIGHT,
        VK_OEM_7 => f::GHOSTTY_KEY_QUOTE,
        VK_OEM_102 => f::GHOSTTY_KEY_INTL_BACKSLASH,
        _ if (0x30..=0x39).contains(&vk) => range(vk, 0x30, f::GHOSTTY_KEY_DIGIT_0),
        _ if (0x41..=0x5A).contains(&vk) => range(vk, 0x41, f::GHOSTTY_KEY_A),
        _ if (VK_NUMPAD0.0..=VK_NUMPAD9.0).contains(&vk) => range(vk, VK_NUMPAD0.0, f::GHOSTTY_KEY_NUMPAD_0),
        _ if (VK_F1.0..=VK_F24.0).contains(&vk) => range(vk, VK_F1.0, f::GHOSTTY_KEY_F1),
        _ => f::GHOSTTY_KEY_UNIDENTIFIED,
    }
}

const fn numpad_or(extended: bool, normal: i32, numpad: i32) -> i32 {
    if extended { normal } else { numpad }
}

/// キーのイベントを、送り方 `proto` のバイト列にする。端末のロックを持って呼ぶ
pub fn encode(ev: &KeyEvent, proto: KeyProtocol, phase: KeyPhase, term: &Terminal, encoder: &mut KeyEncoder) -> Vec<u8> {
    match proto {
        KeyProtocol::Legacy => {
            if phase == KeyPhase::Release {
                return Vec::new();
            }
            match ev.legacy {
                Legacy::Special(k) => encoder.encode(term, k, ev.legacy_mods),
                Legacy::Char(c) => encode_char(c, ev.legacy_mods),
                Legacy::None => Vec::new(),
            }
        }
        KeyProtocol::Win32 => {
            let mut out = Vec::with_capacity(64);
            // キーボードで打てない文字（仮想キー 0）に Alt を付けても conhost は Alt を捨てる（M-あ が あ になる）。
            // Meta の慣習どおり ESC を押して離してから、Alt を外した文字を送る（T-089）
            if ev.is_packet() && ev.mods.alt && phase != KeyPhase::Release {
                if let Some(esc) = named_key("escape").map(|n| KeyEvent::named(n, Mods::default())) {
                    out.extend(esc.win32(true));
                    out.extend(esc.win32(false));
                }
                let plain = KeyEvent::from_char(ev.text.unwrap_or('\0'), Mods { alt: false, ..ev.mods });
                out.extend(plain.win32(true));
                if phase == KeyPhase::Tap {
                    out.extend(plain.win32(false));
                }
                return out;
            }
            if phase != KeyPhase::Release {
                out.extend(ev.win32(true));
            }
            if matches!(phase, KeyPhase::Release | KeyPhase::Tap) {
                out.extend(ev.win32(false));
            }
            out
        }
        KeyProtocol::Kitty => {
            let mut tmp = [0u8; 4];
            let mut out = Vec::new();
            match phase {
                KeyPhase::Press | KeyPhase::Tap => out.extend(encoder.encode_input(term, &ev.kitty_input(KeyAction::Press, &mut tmp))),
                KeyPhase::Repeat => out.extend(encoder.encode_input(term, &ev.kitty_input(KeyAction::Repeat, &mut tmp))),
                KeyPhase::Release => out.extend(encoder.encode_input(term, &ev.kitty_input(KeyAction::Release, &mut tmp))),
            }
            if phase == KeyPhase::Tap {
                let mut tmp = [0u8; 4];
                out.extend(encoder.encode_input(term, &ev.kitty_input(KeyAction::Release, &mut tmp)));
            }
            out
        }
    }
}

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
        _ if (VK_F1.0..=VK_F24.0).contains(&vk.0) => Key::F((vk.0 - VK_F1.0 + 1) as u8),
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

/// Emacs のファンクションキーの名前（`event-basic-type` の名前）を特殊キーにする（レガシーの VT の形）。
/// `backtab`（Shift+Tab）は Tab と Shift の組にする
pub fn key_by_name(name: &str) -> Option<(Key, bool)> {
    let n = named_key(name)?;
    match n.legacy {
        Legacy::Special(k) => Some((k, n.shift)),
        _ => None,
    }
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
        assert_eq!(key_by_name("f24"), Some((Key::F(24), false)));
        assert_eq!(key_by_name("f25"), None);
        assert_eq!(key_by_name("kp-enter"), Some((Key::Enter, false)));
        assert_eq!(key_by_name("kp-home"), Some((Key::Home, false)));
        assert_eq!(key_by_name("a"), None);
    }

    #[test]
    fn paste() {
        assert_eq!(paste_bytes("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~");
    }

    fn win32_str(ev: &KeyEvent) -> String {
        format!("{};{};{}", ev.vk, ev.scan, String::from_utf8_lossy(&ev.win32(true)))
    }

    #[test]
    fn win32_named_keys() {
        // 配列に依存しないキーだけ確かめる
        let up = KeyEvent::from_emacs("up", m("")).unwrap();
        assert_eq!((up.vk, up.extended, up.control_state), (VK_UP.0, true, 0x100));
        let f = KeyEvent::from_emacs("f5", m("C")).unwrap();
        assert_eq!((f.vk, f.control_state), (VK_F5.0, LEFT_CTRL_PRESSED));
        // S-<return> は VK_RETURN + SHIFT で Uc は CR（本物のキーと同じ）
        let ret = KeyEvent::from_emacs("return", m("S")).unwrap();
        assert_eq!((ret.vk, ret.control_state), (VK_RETURN.0, SHIFT_PRESSED));
        assert!(String::from_utf8_lossy(&ret.win32(true)).contains(";13;1;16;1_"), "{}", win32_str(&ret));
        // C-<return> は LF を作る
        let cret = KeyEvent::from_emacs("return", m("C")).unwrap();
        assert!(String::from_utf8_lossy(&cret.win32(true)).contains(";10;1;8;1_"), "{}", win32_str(&cret));
        // 打ち直し: 押した + 離した
        let bytes = String::from_utf8_lossy(&ret.win32(false)).into_owned();
        assert!(bytes.contains(";13;0;16;1_"), "{bytes}");
    }

    #[test]
    fn win32_packet_chars() {
        // キーボードに無い文字は仮想キー 0、スキャン 0、UTF-16 の 1 単位ずつ
        let ev = KeyEvent::from_char('😀', Mods::default());
        assert!(ev.is_packet());
        let s = String::from_utf8_lossy(&ev.win32(true)).into_owned();
        assert_eq!(s, "\x1b[0;0;55357;1;0;1_\x1b[0;0;56832;1;0;1_");
    }

    #[test]
    fn meta_with_packet_chars() {
        // T-089: M-あ。win32-input-mode では ESC を押して離してから Alt を外した あ
        let ev = KeyEvent::from_emacs("あ", m("M")).unwrap();
        assert!(ev.is_packet());
        let (mut term, mut enc) = (Terminal::new(80, 24, Box::new(NoEffects)).unwrap(), KeyEncoder::new().unwrap());
        let s = String::from_utf8_lossy(&encode(&ev, KeyProtocol::Win32, KeyPhase::Tap, &term, &mut enc)).into_owned();
        assert_eq!(s, "\x1b[27;1;27;1;0;1_\x1b[27;1;27;0;0;1_\x1b[0;0;12354;1;0;1_\x1b[0;0;12354;0;0;1_", "{s:?}");
        // レガシーは ESC + あ
        assert_eq!(encode(&ev, KeyProtocol::Legacy, KeyPhase::Tap, &term, &mut enc), "\x1bあ".as_bytes());
        // KKP（CSI >1u）では文字をキーの番号にして Alt を付ける
        term.vt_write(b"\x1b[>1u");
        let s = String::from_utf8_lossy(&encode(&ev, KeyProtocol::Kitty, KeyPhase::Press, &term, &mut enc)).into_owned();
        assert_eq!(s, "\x1b[12354;3u", "{s:?}");
    }

    struct NoEffects;
    impl et_vt::Effects for NoEffects {}

    #[test]
    fn from_char_handles_control_chars() {
        let ev = KeyEvent::from_char('\x01', Mods::default());
        assert!(ev.mods.ctrl, "C-a を文字で渡されたら Ctrl + a に直す");
        assert_eq!(ev.vk, 0x41);
        let bs = KeyEvent::from_char('\x7f', Mods::default());
        assert_eq!(bs.vk, VK_BACK.0);
    }

    #[test]
    fn scanner_finds_9001() {
        let mut s = crate::Win32ModeScannerForTest::default();
        assert_eq!(s.feed(b"\x1b[?1004h"), None);
        assert_eq!(s.feed(b"\x1b[?9001h"), Some(true));
        assert_eq!(s.feed(b"abc\x1b[?1004;9001l"), Some(false));
        assert_eq!(s.feed(b"\x1b[?90"), None);
        assert_eq!(s.feed(b"01h"), Some(true), "読みの途中で切れてもよい");
        assert_eq!(s.feed(b"\x1b[?9001;1000l"), Some(false));
    }
}
