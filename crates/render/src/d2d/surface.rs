//! ウィンドウのスワップチェーンと、それに描くデバイスコンテキスト。
//!
//! GenPreview 2432604 の `crates/render/src/surface.rs` から（T-015）。違い:
//! - 端末はセルを画素に合わせて置きたいので、デバイスコンテキストの DPI は常に 96 にして、
//!   座標を物理ピクセルで扱う（フォントの大きさのほうを DPI で拡大する）
//! - 描いた直後の後ろのバッファを CPU で読めるビットマップに写せる（デバッグ用の画面の保存）
//! - 描き終える（`end`）と画面に出す（`present`）を分けた
//! - **待てるスワップチェーン**（`FRAME_LATENCY_WAITABLE_OBJECT`、最大の遅れ 1 フレーム。T-068）。`Present(1)` は
//!   前のフレームが画面に出るまで UI スレッドを止めていた（出力が続くと 1 回 16 ms）。今は描く前に [`Surface::try_acquire`] で
//!   「次のフレームを受け付けるか」を待たずに確かめ、まだなら描かずに後回しにする（呼ぶ側がハンドル [`Surface::waitable`] を
//!   メッセージと一緒に待つ）。受け付けたあとの `Present` はほとんど待たない

use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::{Interface, Result};

use crate::d2d::gpu::Gpu;

/// スワップチェーンの作り方の印（作るときと `ResizeBuffers` で同じものを渡す）
const SWAP_FLAGS: DXGI_SWAP_CHAIN_FLAG = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;

pub struct Surface {
    pub dc: ID2D1DeviceContext,
    gpu: Gpu,
    /// [`Surface::release`] したら `None`
    swapchain: Option<IDXGISwapChain1>,
    target: Option<ID2D1Bitmap1>,
    /// バッファの大きさ（物理ピクセル）
    size: (u32, u32),
    /// 次のフレームを受け付けると知らされたら立つ（`waitable` のセマフォを 1 つ取った）。`present` で落とす
    waitable: HANDLE,
    acquired: bool,
}

impl Drop for Surface {
    fn drop(&mut self) {
        if !self.waitable.is_invalid() && !self.waitable.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.waitable);
            }
        }
    }
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
                Flags: SWAP_FLAGS.0 as u32,
                ..Default::default()
            };
            // スワップチェーン（D3D の即時コンテキスト）に触るところは、デバイスを分け合うほかの端末と排他にする（T-067）
            let (swapchain, waitable) = {
                let _g = gpu.lock();
                let swapchain = factory.CreateSwapChainForHwnd(&gpu.d3d, hwnd, &desc, None, None)?;
                let sc2: IDXGISwapChain2 = swapchain.cast()?;
                sc2.SetMaximumFrameLatency(1)?;
                (swapchain, sc2.GetFrameLatencyWaitableObject())
            };
            // Alt+Enter での全画面化は要らない
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            let dc = gpu.create_dc()?;
            dc.SetDpi(96.0, 96.0);
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            let mut s =
                Self { dc, gpu: gpu.clone(), swapchain: Some(swapchain), target: None, size: (width.max(1), height.max(1)), waitable, acquired: false };
            s.create_target()?;
            Ok(s)
        }
    }

    fn swapchain(&self) -> Result<&IDXGISwapChain1> {
        self.swapchain.as_ref().ok_or_else(|| windows::core::Error::from(E_FAIL))
    }

    /// スワップチェーンを手放す（別のデバイスで作り直す前に。HWND に結び付くフリップモデルのスワップチェーンは 1 つだけで、
    /// 前のものが残っていると `CreateSwapChainForHwnd` が E_ACCESSDENIED になる。T-067）。以後この `Surface` には描けない
    pub fn release(&mut self) {
        unsafe { self.dc.SetTarget(None) };
        self.target = None;
        if let Some(sc) = self.swapchain.take() {
            let _g = self.gpu.lock();
            drop(sc);
            // スワップチェーンの破棄は遅らせて行われるので、Direct2D が持つ資源を手放し、即時コンテキストを空にして流す
            unsafe {
                self.gpu.d2d.ClearResources(0);
                if let Ok(ctx) = self.gpu.d3d.GetImmediateContext() {
                    ctx.ClearState();
                    ctx.Flush();
                }
            }
        }
    }

    fn create_target(&mut self) -> Result<()> {
        unsafe {
            let buffer: IDXGISurface = self.swapchain()?.GetBuffer(0)?;
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
            let _g = self.gpu.lock();
            self.swapchain()?.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, SWAP_FLAGS)?;
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

    /// 次のフレームを受け付けるか（待たない）。受け付けるなら true で、続けて描いて `present` してよい。
    /// false なら [`Surface::waitable`] が知らされるまで描くのを後回しにする（そのときは [`Surface::set_acquired`]）
    pub fn try_acquire(&mut self) -> bool {
        if !self.acquired && unsafe { WaitForSingleObject(self.waitable, 0) } == WAIT_OBJECT_0 {
            self.acquired = true;
        }
        self.acquired
    }

    /// 呼ぶ側が `waitable` を待って知らされた（セマフォを 1 つ取った）
    pub fn set_acquired(&mut self) {
        self.acquired = true;
    }

    /// 次のフレームを受け付けると知らせるハンドル（セマフォ。待つと 1 つ取る）。持ち主はこの `Surface`
    pub fn waitable(&self) -> HANDLE {
        self.waitable
    }

    /// 画面に出す（垂直同期に合わせる）。`try_acquire` で受け付けを確かめてから呼べば、ほとんど待たない
    /// （確かめずに呼ぶと前のフレームが出るまで待つ。T-068）
    pub fn present(&mut self) -> Result<()> {
        let _g = self.gpu.lock();
        self.acquired = false;
        unsafe { self.swapchain()?.Present(1, DXGI_PRESENT(0)).ok() }
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
