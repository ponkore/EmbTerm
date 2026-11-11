//! D3D11 / Direct2D / DirectWrite のデバイス。
//!
//! GenPreview 2432604 の `crates/render/src/gpu.rs` から（T-015: 薄い層をコピーして作り替える）。
//! 違い: エラーの型を `windows::core::Result` にした。**デバイスはプロセスで 1 つを分け合う**（T-067。[`Gpu::shared`]）。
//!
//! - 端末ごと（GenPreview はビューアごと）に作ると、端末 1 つにつきメモリ・GPU の共有メモリ・ドライバのスレッドが増える
//!   （15 個で GPU の共有メモリ 230 MB・スレッド 218 本。T-054）。今は最初に要ったときに 1 つ作り、使う人がいなくなったら捨てる
//! - 描く UI スレッドは端末ごとに別なので、1 つのデバイスを複数のスレッドで使う。Direct2D のファクトリは `MULTI_THREADED` で、
//!   Direct2D の呼び出しは Direct2D が排他にする。Direct2D を通さずに D3D の即時コンテキストに触るもの（スワップチェーンの作成・
//!   `ResizeBuffers`・`Present`）は [`Gpu::lock`]（`ID2D1Multithread::Enter`）で囲む。D3D の即時コンテキストにも
//!   `SetMultithreadProtected` を掛けておく
//! - デバイスを失ったら（`EndDraw` / `Present` が失敗）[`Gpu::invalidate`] で共有のデバイスを捨て、次の [`Gpu::shared`] で作り直す。
//!   ほかの端末は描く前に [`Gpu::is_current`] で世代を見比べて、古ければ新しいデバイスに載せ替える（全員で作り直す）

use std::ops::Deref;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::{Interface, Result};

/// 生きているデバイスの数（T-054 / T-067 の確認用）
static LIVE_DEVICES: AtomicUsize = AtomicUsize::new(0);
/// 最後に作ったデバイスの世代（1 から。捨てたら 0）
static CURRENT: AtomicU64 = AtomicU64::new(0);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// 分け合っているデバイス（使う人がいなくなれば消える）
static SHARED: Mutex<Weak<GpuInner>> = Mutex::new(Weak::new());

/// いま生きている GPU デバイスの数
pub fn live_devices() -> usize {
    LIVE_DEVICES.load(Ordering::Relaxed)
}

/// いま分け合っているデバイスの世代（無い・捨てたばかりなら 0）
pub fn current_generation() -> u64 {
    CURRENT.load(Ordering::Acquire)
}

pub struct GpuInner {
    pub d3d: ID3D11Device,
    pub d2d_factory: ID2D1Factory1,
    pub d2d: ID2D1Device,
    pub dwrite: IDWriteFactory,
    /// ハードウェアが使えず WARP（ソフトウェア）で動いているか
    pub warp: bool,
    multithread: ID2D1Multithread,
    generation: u64,
}

// D2D のファクトリはマルチスレッド用に作り、D3D11 デバイスはフリースレッド、DirectWrite の共有ファクトリもスレッドセーフ。
// Direct2D を通さない D3D の即時コンテキストの使用は Gpu::lock で排他にする
unsafe impl Send for GpuInner {}
unsafe impl Sync for GpuInner {}

impl Drop for GpuInner {
    fn drop(&mut self) {
        LIVE_DEVICES.fetch_sub(1, Ordering::Relaxed);
    }
}

/// デバイス（複製は同じデバイスを指す）
#[derive(Clone)]
pub struct Gpu(Arc<GpuInner>);

impl Deref for Gpu {
    type Target = GpuInner;
    fn deref(&self) -> &GpuInner {
        &self.0
    }
}

/// [`Gpu::lock`] の戻り値。落とすと排他を外す
pub struct GpuLock<'a>(&'a ID2D1Multithread);

impl Drop for GpuLock<'_> {
    fn drop(&mut self) {
        unsafe { self.0.Leave() };
    }
}

impl Gpu {
    /// 新しいデバイスを作る（分け合わない）。数十〜百 ms かかる
    pub fn create() -> Result<Self> {
        unsafe {
            let (d3d, warp) = match create_d3d(D3D_DRIVER_TYPE_HARDWARE) {
                Ok(d) => (d, false),
                Err(_) => (create_d3d(D3D_DRIVER_TYPE_WARP)?, true),
            };
            // 即時コンテキストを複数のスレッドから使っても壊れないように（Present などは Gpu::lock でも囲む）
            if let Ok(ctx) = d3d.GetImmediateContext()
                && let Ok(mt) = ctx.cast::<ID3D11Multithread>()
            {
                let _ = mt.SetMultithreadProtected(true);
            }
            let d2d_factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)?;
            let multithread: ID2D1Multithread = d2d_factory.cast()?;
            let dxgi: IDXGIDevice = d3d.cast()?;
            let d2d = d2d_factory.CreateDevice(&dxgi)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            LIVE_DEVICES.fetch_add(1, Ordering::Relaxed);
            let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
            Ok(Self(Arc::new(GpuInner { d3d, d2d_factory, d2d, dwrite, warp, multithread, generation })))
        }
    }

    /// プロセスで分け合うデバイス。無ければ（初めて・使う人がいなくなった・失った後）作る
    pub fn shared() -> Result<Self> {
        let mut g = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(inner) = g.upgrade()
            && CURRENT.load(Ordering::Acquire) == inner.generation
        {
            return Ok(Self(inner));
        }
        let gpu = Self::create()?;
        *g = Arc::downgrade(&gpu.0);
        CURRENT.store(gpu.generation, Ordering::Release);
        Ok(gpu)
    }

    /// デバイスの世代（作り直すたびに増える）
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// これがいま分け合っているデバイスか（false なら、ほかの誰かが失って作り直した。[`Gpu::shared`] に載せ替える）
    pub fn is_current(&self) -> bool {
        CURRENT.load(Ordering::Acquire) == self.generation
    }

    /// このデバイスを失った。分け合っているのがこれなら捨て、次の [`Gpu::shared`] で作り直させる
    pub fn invalidate(&self) {
        let mut g = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        if CURRENT.compare_exchange(self.generation, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            *g = Weak::new();
        }
    }

    /// Direct2D を通さずに D3D の即時コンテキストに触る間（スワップチェーンの作成・ResizeBuffers・Present）の排他
    pub fn lock(&self) -> GpuLock<'_> {
        unsafe { self.multithread.Enter() };
        GpuLock(&self.multithread)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 分け合うデバイスは 1 つ。失ったら作り直し、古いものは `is_current` が false になる。使う人がいなくなれば消える
    #[test]
    fn shared_device_generations() {
        let a = Gpu::shared().unwrap();
        let b = Gpu::shared().unwrap();
        assert_eq!(a.generation(), b.generation());
        assert!(a.is_current());
        {
            let _g = a.lock();
            let _h = b.lock(); // 同じスレッドなら入れ子にできる（ID2D1Multithread は再入できる）
        }
        a.invalidate();
        assert!(!b.is_current());
        let c = Gpu::shared().unwrap();
        assert_ne!(c.generation(), a.generation());
        assert!(c.is_current());
        // 古い世代で invalidate しても今のデバイスは捨てない
        a.invalidate();
        assert!(c.is_current());
        let gen_c = c.generation();
        drop((a, b, c));
        // 使う人がいなくなったので、次は新しく作る
        let d = Gpu::shared().unwrap();
        assert_ne!(d.generation(), gen_c);
    }
}
