//! Kitty graphics の PNG のデコード（libghostty-vt はデコーダを持たず、`ghostty_sys_set` で受け取る。T-088）。
//!
//! Windows の WIC を使う（外部クレートを増やさない）。受け取ったバイト列はアプリからの入力なので、
//! 形式の自動判別はせず、PNG のデコーダに決めて読む（インストールされている他のコーデックを動かさない）。

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::core::Result;

use crate::ffi;

/// PNG のデコーダを登録する。プロセスで最初の端末を作る前に、ほかのスレッドが libghostty-vt を使っていないときに
/// 呼ぶ（sys.h の決まり）。2 回目以降は何もしない
pub fn install_png_decoder() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let f: ffi::GhosttySysDecodePngFn = decode_png;
        unsafe { ffi::ghostty_sys_set(ffi::GHOSTTY_SYS_OPT_DECODE_PNG, f as *const c_void) };
    });
}

/// デコードの回数・時間の合計と最大（マイクロ秒）・最後の画素数（T-096 の測定）
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

/// プロセス全体の PNG のデコードの回数と時間（T-096。デコードは端末のロックの中で走る）
pub fn png_decode_stats() -> PngDecodeStats {
    PngDecodeStats {
        count: DECODE_COUNT.load(Ordering::Relaxed),
        total_us: DECODE_TOTAL_US.load(Ordering::Relaxed),
        max_us: DECODE_MAX_US.load(Ordering::Relaxed),
        last_us: DECODE_LAST_US.load(Ordering::Relaxed),
        last_pixels: DECODE_LAST_PIXELS.load(Ordering::Relaxed),
    }
}

/// `CoInitializeEx` の対。その呼び出しが成功したときだけ `CoUninitialize` する
struct ComGuard(bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
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
        let ok = decode(bytes, allocator, out).is_ok();
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

/// `allocator` で確保した画素を `out` に入れる。失敗したら確保したものは解放して `Err` を返す
unsafe fn decode(bytes: &[u8], allocator: *const ffi::GhosttyAllocator, out: &mut ffi::GhosttySysImage) -> Result<()> {
    // PTY の読みのスレッド（COM を初期化していない）から呼ばれる。ほかの方式で初期化済みなら失敗するが、そのまま使える
    let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let _guard = ComGuard(init.is_ok());
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = factory.CreateStream()?;
        stream.InitializeFromMemory(bytes)?;
        let decoder = factory.CreateDecoder(&GUID_ContainerFormatPng, ptr::null())?;
        decoder.Initialize(&stream, WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(&frame, &GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let (mut width, mut height) = (0u32, 0u32);
        converter.GetSize(&mut width, &mut height)?;
        let invalid = || windows::core::Error::from(windows::Win32::Foundation::E_INVALIDARG);
        let stride = width.checked_mul(4).ok_or_else(invalid)?;
        let len = (stride as usize).checked_mul(height as usize).ok_or_else(invalid)?;
        // 大きすぎる画像はアロケータが断る（libghostty-vt の上限）
        let pixels = ffi::ghostty_alloc(allocator, len);
        if pixels.is_null() {
            return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
        }
        if let Err(e) = converter.CopyPixels(ptr::null(), stride, std::slice::from_raw_parts_mut(pixels, len)) {
            ffi::ghostty_free(allocator, pixels, len);
            return Err(e);
        }
        *out = ffi::GhosttySysImage { width, height, data: pixels, data_len: len };
    }
    Ok(())
}
