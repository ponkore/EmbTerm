//! Metal のデバイス・キュー・パイプライン（T-125）。
//!
//! Direct2D 版の `Gpu`（T-067）と同じく、**デバイスはプロセスで 1 つを分け合う**（端末を何個も開くため）。
//! シェーダはソースから実行時にコンパイルする（ビルドの手順を増やさないため。数十 ms で、デバイスと一緒に 1 回だけ）。

use std::sync::{Arc, Mutex, Weak};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary,
    MTLPixelFormat, MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLTexture,
};

/// 四角 1 枚ずつのインスタンス描画。背景・グリフ・装飾・カーソルを同じ形で描く
const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Inst {
    float4 rect;    // x, y, 幅, 高さ（デバイス画素）
    float4 color;   // RGBA
    float4 uv;      // 左上・右下（0..1）
    uint   mode;    // 0 = 塗り, 1 = マスクのグリフ, 2 = 色付きのグリフ
    // MSL の uint3 は 16 バイト境界なので、uint 3 つで埋める（Rust 側の #[repr(C)] と同じ 64 バイトにする）
    uint   pad0;
    uint   pad1;
    uint   pad2;
};

struct VOut {
    float4 pos [[position]];
    float2 uv;
    float4 color;
    uint   mode;
};

vertex VOut v_main(uint vid [[vertex_id]],
                   uint iid [[instance_id]],
                   const device Inst *insts [[buffer(0)]],
                   constant float2 &viewport [[buffer(1)]]) {
    Inst i = insts[iid];
    float2 corner = float2((vid & 1) ? 1.0 : 0.0, (vid & 2) ? 1.0 : 0.0);
    float2 p = i.rect.xy + corner * i.rect.zw;
    VOut o;
    o.pos = float4(p.x / viewport.x * 2.0 - 1.0, 1.0 - p.y / viewport.y * 2.0, 0.0, 1.0);
    o.uv = mix(i.uv.xy, i.uv.zw, corner);
    o.color = i.color;
    o.mode = i.mode;
    return o;
}

fragment float4 f_main(VOut in [[stage_in]], texture2d<float> atlas [[texture(0)]]) {
    constexpr sampler smp(filter::nearest, address::clamp_to_edge);
    if (in.mode == 0u) {
        return in.color;
    }
    float4 t = atlas.sample(smp, in.uv);
    if (in.mode == 2u) {
        return t;                                   // 色付きのグリフ（premultiplied のまま）
    }
    return float4(in.color.rgb * t.a, t.a);          // マスクのグリフを前景色で塗る
}
"#;

/// 分け合っている中身（デバイス・キュー・パイプライン）
pub struct Inner {
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pub pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
}

/// 分け合っている Metal のデバイス
#[derive(Clone)]
pub struct Gpu(pub(crate) Arc<Inner>);

static SHARED: Mutex<Weak<Inner>> = Mutex::new(Weak::new());

impl std::ops::Deref for Gpu {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Gpu {
    /// プロセスで 1 つのデバイスを取る（まだ無ければ作る）
    pub fn shared() -> Result<Gpu, String> {
        let mut g = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(inner) = g.upgrade() {
            return Ok(Gpu(inner));
        }
        let inner = Arc::new(Inner::new()?);
        *g = Arc::downgrade(&inner);
        Ok(Gpu(inner))
    }

    pub(crate) fn offscreen_texture(
        &self,
        width: u32,
        height: u32,
    ) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, String> {
        super::make_offscreen(&self.device, width, height)
    }
}

impl Inner {
    fn new() -> Result<Self, String> {
        let device = MTLCreateSystemDefaultDevice().ok_or("Metal のデバイスがありません")?;
        let queue = device.newCommandQueue().ok_or("コマンドキューを作れません")?;
        let src = NSString::from_str(SHADER);
        let lib = device
            .newLibraryWithSource_options_error(&src, None)
            .map_err(|e| format!("シェーダをコンパイルできません: {e}"))?;
        let vf = lib.newFunctionWithName(&NSString::from_str("v_main")).ok_or("v_main がありません")?;
        let ff = lib.newFunctionWithName(&NSString::from_str("f_main")).ok_or("f_main がありません")?;

        let pd = MTLRenderPipelineDescriptor::new();
        pd.setVertexFunction(Some(&vf));
        pd.setFragmentFunction(Some(&ff));
        let att = unsafe { pd.colorAttachments().objectAtIndexedSubscript(0) };
        att.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        // premultiplied alpha（グリフのマスクと色付きのグリフを同じ式で重ねる）
        att.setBlendingEnabled(true);
        att.setRgbBlendOperation(MTLBlendOperation::Add);
        att.setAlphaBlendOperation(MTLBlendOperation::Add);
        att.setSourceRGBBlendFactor(MTLBlendFactor::One);
        att.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        att.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        att.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&pd)
            .map_err(|e| format!("パイプラインを作れません: {e}"))?;
        Ok(Self { device, queue, pipeline })
    }
}
