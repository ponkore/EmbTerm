//! macOS の PNG のデコード（T-127）。ImageIO（`CGImageSource`）を使う。
//!
//! libghostty-vt はデコーダを持たず `ghostty_sys_set` で受け取る（T-088）。Windows の WIC 版（[`crate::png`] の
//! Windows 側）と同じ形で、**RGBA8（ストレートアルファ）**を libghostty-vt のアロケータで確保して返す。
//!
//! 入力はアプリから来たバイト列なので、形式の自動判別はせず **PNG に決めて読む**
//! （`CGImageCreateWithPNGDataProvider`。ImageIO の自動判別を通さないので、ほかの形式のデコーダは動かない）。

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGContext, CGDataProvider, CGImage,
    CGImageAlphaInfo,
};

use crate::ffi;

/// PNG のデコーダを登録する。プロセスで最初の端末を作る前に呼ぶ（sys.h の決まり）。2 回目以降は何もしない
pub fn install_png_decoder() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let f: ffi::GhosttySysDecodePngFn = decode_png;
        unsafe { ffi::ghostty_sys_set(ffi::GHOSTTY_SYS_OPT_DECODE_PNG, f as *const c_void) };
    });
}

static DECODE_COUNT: AtomicU64 = AtomicU64::new(0);
static DECODE_TOTAL_US: AtomicU64 = AtomicU64::new(0);
static DECODE_MAX_US: AtomicU64 = AtomicU64::new(0);
static DECODE_LAST_US: AtomicU64 = AtomicU64::new(0);
static DECODE_LAST_PIXELS: AtomicU64 = AtomicU64::new(0);

/// PNG のデコードの測定（[`png_decode_stats`]）
#[derive(Clone, Copy, Debug, Default)]
pub struct PngDecodeStats {
    pub count: u64,
    pub total_us: u64,
    pub max_us: u64,
    pub last_us: u64,
    /// 最後にデコードした画像の画素数（幅 x 高さ）
    pub last_pixels: u64,
}

/// プロセス全体の PNG のデコードの回数と時間（T-096）
pub fn png_decode_stats() -> PngDecodeStats {
    PngDecodeStats {
        count: DECODE_COUNT.load(Ordering::Relaxed),
        total_us: DECODE_TOTAL_US.load(Ordering::Relaxed),
        max_us: DECODE_MAX_US.load(Ordering::Relaxed),
        last_us: DECODE_LAST_US.load(Ordering::Relaxed),
        last_pixels: DECODE_LAST_PIXELS.load(Ordering::Relaxed),
    }
}

unsafe extern "C" fn decode_png(
    _userdata: *mut c_void,
    allocator: *const ffi::GhosttyAllocator,
    data: *const u8,
    data_len: usize,
    out: *mut ffi::GhosttySysImage,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        let Some(out) = out.as_mut() else { return false };
        let bytes = std::slice::from_raw_parts(data, data_len);
        let t0 = Instant::now();
        let ok = decode(bytes, allocator, out);
        let us = t0.elapsed().as_micros() as u64;
        DECODE_COUNT.fetch_add(1, Ordering::Relaxed);
        DECODE_TOTAL_US.fetch_add(us, Ordering::Relaxed);
        DECODE_MAX_US.fetch_max(us, Ordering::Relaxed);
        DECODE_LAST_US.store(us, Ordering::Relaxed);
        if ok {
            DECODE_LAST_PIXELS.store(out.width as u64 * out.height as u64, Ordering::Relaxed);
        }
        ok
    }))
    .unwrap_or(false)
}

/// PNG を RGBA8 にして `out` に入れる（画素は libghostty-vt のアロケータで確保する）
unsafe fn decode(bytes: &[u8], allocator: *const ffi::GhosttyAllocator, out: &mut ffi::GhosttySysImage) -> bool {
    let Some(image) = (unsafe { decode_cgimage(bytes) }) else { return false };
    let width = CGImage::width(Some(&image)) as u32;
    let height = CGImage::height(Some(&image)) as u32;
    let (Some(stride), Some(len)) = (
        width.checked_mul(4),
        (width as usize).checked_mul(4).and_then(|s| s.checked_mul(height as usize)),
    ) else {
        return false;
    };
    if width == 0 || height == 0 {
        return false;
    }
    // 画素は libghostty-vt のアロケータで（大きすぎる画像はここで断られる）
    let pixels = unsafe { ffi::ghostty_alloc(allocator, len) };
    if pixels.is_null() {
        return false;
    }
    let ok = unsafe { draw_into(&image, pixels, width, height, stride as usize) };
    if !ok {
        unsafe { ffi::ghostty_free(allocator, pixels, len) };
        return false;
    }
    *out = ffi::GhosttySysImage { width, height, data: pixels, data_len: len };
    true
}

/// PNG のバイト列を `CGImage` にする（PNG のデコーダに直に渡す）
unsafe fn decode_cgimage(bytes: &[u8]) -> Option<CFRetained<CGImage>> {
    // データはこの関数の間だけ生きていればよい（CGImage を作るときに読み切られる）
    let provider = unsafe {
        CGDataProvider::with_data(ptr::null_mut(), bytes.as_ptr() as *const c_void, bytes.len(), None)
    }?;
    unsafe {
        CGImage::with_png_data_provider(Some(&provider), ptr::null(), false, CGColorRenderingIntent::RenderingIntentDefault)
    }
}

/// `CGImage` を RGBA8（ストレートアルファ）で `pixels` に描く。
///
/// CoreGraphics のビットマップの文脈は**ストレートアルファ（`kCGImageAlphaLast`）を受け付けない**ので、
/// premultiplied で描いてから戻す（libghostty-vt はストレートアルファを期待する。Windows の WIC 版は
/// `32bppRGBA` で直に受け取れる）
unsafe fn draw_into(image: &CGImage, pixels: *mut u8, width: u32, height: u32, stride: usize) -> bool {
    let Some(cs) = CGColorSpace::new_device_rgb() else { return false };
    let info = CGImageAlphaInfo::PremultipliedLast.0 | CGBitmapInfo::ByteOrderDefault.0;
    let ctx = unsafe {
        CGBitmapContextCreate(pixels as *mut c_void, width as usize, height as usize, 8, stride, Some(&cs), info)
    };
    let Some(ctx) = ctx else { return false };
    let rect = objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(0.0, 0.0),
        objc2_core_foundation::CGSize::new(width as f64, height as f64),
    );
    unsafe {
        // ビットマップの文脈は、記憶の先頭の行が画像の上の行になる（座標系は上下が逆だが、そのまま描けばよい）
        CGContext::set_blend_mode(Some(&ctx), objc2_core_graphics::CGBlendMode::Copy);
        CGContext::draw_image(Some(&ctx), rect, Some(image));
        CGContext::flush(Some(&ctx));
    }
    // premultiplied → ストレートアルファ
    let n = (width as usize) * (height as usize);
    let buf = unsafe { std::slice::from_raw_parts_mut(pixels, n * 4) };
    for px in buf.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else if a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1x1 の白い PNG（tests.rs の PNG_1X1 と同じ）
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00,
        0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0, 0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF,
        0x89, 0x99, 0x3D, 0x1D, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn png_を_cgimage_にできる() {
        let image = unsafe { decode_cgimage(PNG_1X1) }.expect("PNG をデコードできない");
        assert_eq!(CGImage::width(Some(&image)), 1);
        assert_eq!(CGImage::height(Some(&image)), 1);
    }

    #[test]
    fn png_を_rgba_にできる() {
        let image = unsafe { decode_cgimage(PNG_1X1) }.expect("PNG をデコードできない");
        let mut pixels = [0u8; 4];
        let ok = unsafe { draw_into(&image, pixels.as_mut_ptr(), 1, 1, 4) };
        assert!(ok, "ビットマップに描けない");
        assert_eq!(pixels[3], 0xff, "不透明: {pixels:?}");
    }
}
