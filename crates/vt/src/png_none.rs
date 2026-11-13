//! Windows 以外の PNG のデコード（T-124 の段階では持たない）。
//!
//! libghostty-vt はデコーダを持たず `ghostty_sys_set` で受け取る（T-088）。登録しなければ、PNG で送られた
//! Kitty graphics の画像は libghostty-vt が受け付けないだけで、端末の動きには影響しない。
//! macOS では ImageIO（`CGImageSourceCreateWithData`）に差し替える（T-127）。

/// PNG のデコーダを登録する（ここでは何もしない）
pub fn install_png_decoder() {}

/// PNG のデコードの測定（[`png_decode_stats`]）
#[derive(Clone, Copy, Debug, Default)]
pub struct PngDecodeStats {
    pub count: u64,
    pub total_us: u64,
    pub max_us: u64,
    pub last_us: u64,
    pub last_pixels: u64,
}

/// デコードの測定（デコーダが無いので常に 0）
pub fn png_decode_stats() -> PngDecodeStats {
    PngDecodeStats::default()
}
