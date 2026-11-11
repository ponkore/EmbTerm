//! D3D11 / Direct2D / DirectWrite のデバイス。
//!
//! GenPreview 2432604 の `crates/render/src/gpu.rs` から（T-015: 薄い層をコピーして作り替える）。
//! 違い: エラーの型を `windows::core::Result` にした。

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::{Interface, Result};

#[derive(Clone)]
pub struct Gpu {
    pub d3d: ID3D11Device,
    pub d2d_factory: ID2D1Factory1,
    pub d2d: ID2D1Device,
    pub dwrite: IDWriteFactory,
    /// ハードウェアが使えず WARP（ソフトウェア）で動いているか
    pub warp: bool,
}

// D2D のファクトリはマルチスレッド用に作り、D3D11 デバイスはフリースレッドなので、
// 別のスレッドで作ったものを UI スレッドへ渡してよい
unsafe impl Send for Gpu {}

impl Gpu {
    /// デバイスを作る。数十〜百 ms かかる
    pub fn create() -> Result<Self> {
        unsafe {
            let (d3d, warp) = match create_d3d(D3D_DRIVER_TYPE_HARDWARE) {
                Ok(d) => (d, false),
                Err(_) => (create_d3d(D3D_DRIVER_TYPE_WARP)?, true),
            };
            let d2d_factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)?;
            let dxgi: IDXGIDevice = d3d.cast()?;
            let d2d = d2d_factory.CreateDevice(&dxgi)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Self { d3d, d2d_factory, d2d, dwrite, warp })
        }
    }

    pub fn create_dc(&self) -> Result<ID2D1DeviceContext> {
        unsafe { self.d2d.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE) }
    }
}

unsafe fn create_d3d(driver: D3D_DRIVER_TYPE) -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    Ok(device.expect("D3D11CreateDevice が成功すればデバイスはある"))
}
