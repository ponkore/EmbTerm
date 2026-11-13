//! 描き方の設定（OS に依らない）。
//!
//! 色の決め方・余白・幅 1 と数えたのにグリフが広い文字の扱い（T-013）は、Direct2D 版（Windows）と
//! Metal 版（macOS）で同じものを使う。

use et_vt::Rgb;

/// 色の設定（端末の既定の前景・背景は libghostty-vt 側で持つ）
#[derive(Clone, Debug)]
pub struct Theme {
    /// 選択範囲の背景
    pub selection_bg: Rgb,
    /// 選択範囲の文字の色。`None` ならセルの色のまま
    pub selection_fg: Option<Rgb>,
    /// カーソルの色（アプリが OSC 12 で決めていなければこれ）
    pub cursor: Rgb,
    /// ブロックのカーソルの上の文字の色。`None` なら既定の背景色
    pub cursor_text: Option<Rgb>,
    /// 端末の周りの余白（px、DPI 96 のとき。DPI に合わせて拡大する）
    pub padding: u32,
    /// 幅 1 と数えたのにグリフが広い文字（`①` `★` `※`）の描き方（T-013）
    pub narrow_wide: NarrowWide,
    /// 孤立した広いグリフに右隣の空白を貸すか（T-013 の ghostel の規則。`NarrowWide::Shrink` のときだけ効く）
    pub borrow_right: bool,
}

/// 幅 1 と数えたのにグリフが広い文字（East Asian Ambiguous の `①` `★` `※` など）の描き方（T-013）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NarrowWide {
    /// 1 セルの枠に収まるよう縮める（ghostel と同じ）
    Shrink,
    /// 縮めずに元の大きさのまま、セルの左端から描いて右隣へはみ出させる（WezTerm と同じ見え方）。
    /// 桁送りは 1 セルのままなので、続けて並ぶと後の文字が前の文字の右半分に重なる
    #[default]
    Overlap,
}

impl std::str::FromStr for NarrowWide {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "shrink" => Ok(Self::Shrink),
            "overlap" => Ok(Self::Overlap),
            _ => Err(format!("shrink か overlap を指定する（{s}）")),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            selection_bg: Rgb { r: 0x26, g: 0x4f, b: 0x78 },
            selection_fg: None,
            cursor: Rgb { r: 0xff, g: 0xcc, b: 0x00 },
            cursor_text: None,
            padding: 4,
            narrow_wide: NarrowWide::default(),
            borrow_right: true,
        }
    }
}

