//! キー入力の最小の変換（レガシーの VT）。win32-input-mode は T-069 で作る。
//!
//! - 文字（`WM_CHAR`）は UTF-8 にしてそのまま送る。Ctrl+文字は Windows が制御文字（0x01〜0x1a）にしてくれる
//! - 特殊キー（`WM_KEYDOWN`）は libghostty-vt のキーエンコーダで、端末のモード（DECCKM など）に合わせて符号化する
//! - Enter・Backspace・Tab・Esc もエンコーダで送り、続いて来る `WM_CHAR` は捨てる（Backspace は 0x7f にするため）

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

/// 今の修飾キーの状態
pub fn mods(alt_from_message: bool) -> Mods {
    Mods { shift: down(VK_SHIFT), ctrl: down(VK_CONTROL), alt: alt_from_message || down(VK_MENU) }
}
