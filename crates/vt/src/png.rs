//! Kitty graphics の PNG のデコード（libghostty-vt はデコーダを持たず、`ghostty_sys_set` で受け取る。T-088）。
//!
//! Windows の WIC を使う（外部クレートを増やさない）。受け取ったバイト列はアプリからの入力なので、
//! 形式の自動判別はせず、PNG のデコーダに決めて読む（インストールされている他のコーデックを動かさない）。

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Once;

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
        decode(bytes, allocator, out).is_ok()
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
