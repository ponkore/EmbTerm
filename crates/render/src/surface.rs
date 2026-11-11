//! ウィンドウのスワップチェーンと、それに描くデバイスコンテキスト。
//!
//! GenPreview 2432604 の `crates/render/src/surface.rs` から（T-015）。違い:
//! - 端末はセルを画素に合わせて置きたいので、デバイスコンテキストの DPI は常に 96 にして、
//!   座標を物理ピクセルで扱う（フォントの大きさのほうを DPI で拡大する）
//! - 描いた直後の後ろのバッファを CPU で読めるビットマップに写せる（デバッグ用の画面の保存）
//! - 描き終える（`end`）と画面に出す（`present`）を分けた

use windows::Win32::Foundation::{E_FAIL, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Interface, Result};

use crate::gpu::Gpu;

pub struct Surface {
    pub dc: ID2D1DeviceContext,
    swapchain: IDXGISwapChain1,
    target: Option<ID2D1Bitmap1>,
    /// バッファの大きさ（物理ピクセル）
    size: (u32, u32),
}

fn bitmap_props(options: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
    D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: options,
        colorContext: std::mem::ManuallyDrop::new(None),
    }
}

impl Surface {
    /// `width` / `height` は物理ピクセル
    pub fn new(gpu: &Gpu, hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let dxgi: IDXGIDevice = gpu.d3d.cast()?;
            let adapter = dxgi.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width.max(1),
                Height: height.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: 0,
                ..Default::default()
            };
            let swapchain = factory.CreateSwapChainForHwnd(&gpu.d3d, hwnd, &desc, None, None)?;
            // Alt+Enter での全画面化は要らない
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            let dc = gpu.create_dc()?;
            dc.SetDpi(96.0, 96.0);
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            let mut s = Self { dc, swapchain, target: None, size: (width.max(1), height.max(1)) };
            s.create_target()?;
            Ok(s)
        }
    }

    fn create_target(&mut self) -> Result<()> {
        unsafe {
            let buffer: IDXGISurface = self.swapchain.GetBuffer(0)?;
            let props = bitmap_props(D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
            let target = self.dc.CreateBitmapFromDxgiSurface(&buffer, Some(&props))?;
            self.dc.SetTarget(&target);
            self.target = Some(target);
        }
        Ok(())
    }

    /// 大きさを変える（物理ピクセル）。同じなら何もしない
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let (w, h) = (width.max(1), height.max(1));
        if (w, h) == self.size {
            return Ok(());
        }
        unsafe {
            self.dc.SetTarget(None);
            self.target = None;
            self.swapchain.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
            self.size = (w, h);
        }
        self.create_target()
    }

    /// バッファの大きさ（物理ピクセル）
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn begin(&self) {
        unsafe { self.dc.BeginDraw() }
    }

    /// 描き終える。デバイスが失われたら `Err` を返すので、作り直すこと
    pub fn end(&self) -> Result<()> {
        unsafe { self.dc.EndDraw(None, None) }
    }

    /// 画面に出す。`Present(1)` は垂直同期を 1 回待つ（UI スレッドが最大 1 フレーム止まる。T-068 で測る）
    pub fn present(&self) -> Result<()> {
        unsafe { self.swapchain.Present(1, DXGI_PRESENT(0)).ok() }
    }

    /// 描き終えた後ろのバッファを読む（`end` のあと `present` の前に呼ぶ）。BGRA の画素と 1 行のバイト数を返す
    pub fn read_back(&self) -> Result<(Vec<u8>, usize)> {
        let target = self.target.as_ref().ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        unsafe {
            let (w, h) = self.size;
            let props = bitmap_props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
            let staging = self.dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props)?;
            staging.CopyFromBitmap(None, target, None)?;
            let mapped = staging.Map(D2D1_MAP_OPTIONS_READ)?;
            let pitch = mapped.pitch as usize;
            let data = std::slice::from_raw_parts(mapped.bits, pitch * h as usize).to_vec();
            staging.Unmap()?;
            Ok((data, pitch))
        }
    }
}
