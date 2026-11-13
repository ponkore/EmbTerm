//! Kitty graphics の画像を Metal のテクスチャにして描く（T-127）。
//!
//! 配置は libghostty-vt の render state が解決した結果（[`et_vt::ImagePlacement`]）をそのまま使う。
//! Unicode placeholder の走査も相対配置の解決も向こうの仕事で、ここではビューの行へ移して切り取るだけ
//! （Direct2D 版の [`crate::d2d`] と同じ考え）。
//!
//! Direct2D 版との違い（T-127 の段階）
//! - 画像 1 枚につき**描画の呼び出しを 1 回**（テクスチャを差し替えるため）。文字のアトラスとは別のテクスチャ
//! - **縮めて載せる工夫（T-105）と、別のスレッドでの変換はまだ無い**。元の大きさのまま載せる

use std::collections::HashMap;
use std::ptr::NonNull;

use et_vt::{ImageFormat, ImageLayer};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLDevice, MTLOrigin, MTLPixelFormat, MTLRegion, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
    MTLTextureUsage,
};

use crate::frame::{Frame, PlacedImage};
use crate::view::FrameView;

/// テクスチャの鍵（画像の世代）
pub(crate) type ImageKey = u64;

/// 1 枚の画像を描く指示（[`super::Renderer`] が組み立てる）
pub(crate) struct ImageDraw {
    pub texture: Retained<ProtocolObject<dyn MTLTexture>>,
    /// 置く場所（デバイス画素）
    pub rect: [f32; 4],
    /// 画像の中の切り取り（0..1）
    pub uv: [f32; 4],
}

#[derive(Default)]
pub(crate) struct ImageCache {
    textures: HashMap<ImageKey, (Retained<ProtocolObject<dyn MTLTexture>>, u32, u32)>,
}

// SAFETY: Metal のテクスチャは別のスレッドから使ってよい
unsafe impl Send for ImageCache {}

impl ImageCache {
    /// 画像のテクスチャ（無ければ作る）
    fn texture(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        image: &PlacedImage,
    ) -> Option<(Retained<ProtocolObject<dyn MTLTexture>>, u32, u32)> {
        let key = image.placement.generation;
        if let Some((t, w, h)) = self.textures.get(&key) {
            return Some((t.clone(), *w, *h));
        }
        let (w, h) = (image.pixels.width.max(1), image.pixels.height.max(1));
        let mut rgba = to_rgba(&image.pixels.data, w, h, image.pixels.format)?;
        let td = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                w as usize,
                h as usize,
                false,
            )
        };
        td.setUsage(MTLTextureUsage::ShaderRead);
        td.setStorageMode(MTLStorageMode::Shared);
        let texture = device.newTextureWithDescriptor(&td)?;
        unsafe {
            texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                MTLRegion {
                    origin: MTLOrigin { x: 0, y: 0, z: 0 },
                    size: MTLSize { width: w as usize, height: h as usize, depth: 1 },
                },
                0,
                NonNull::new(rgba.as_mut_ptr() as *mut _)?,
                w as usize * 4,
            );
        }
        self.textures.insert(key, (texture.clone(), w, h));
        Some((texture, w, h))
    }

    /// フレームに無くなった画像のテクスチャを捨てる
    pub(crate) fn prune(&mut self, frame: &Frame) {
        self.textures.retain(|k, _| frame.images.iter().any(|i| i.placement.generation == *k));
    }

    /// 生きているテクスチャの数（自動確認用）
    pub(crate) fn len(&self) -> usize {
        self.textures.len()
    }

    /// `layer` の画像を、ビューの行へ移して置き場所を決める
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draws(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        frame: &Frame,
        view: &FrameView,
        layer: ImageLayer,
        cell: (f32, f32),
        pad: f32,
        cols: usize,
    ) -> Vec<ImageDraw> {
        let mut out = Vec::new();
        if !frame.images.iter().any(|i| i.placement.layer == layer) {
            return out;
        }
        let (cw, ch) = cell;
        // 配置は端末のセルの大きさで計算されている。ペインのセルが違えば拡大縮小する
        let sx = cw / frame.image_cell.0.max(1) as f32;
        let sy = ch / frame.image_cell.1.max(1) as f32;
        for image in frame.images.iter().filter(|i| i.placement.layer == layer) {
            let p = image.placement;
            let Some((texture, tw, th)) = self.texture(device, image) else { continue };
            let (dw, dh) = (p.width as f32 * sx, p.height as f32 * sy);
            let row = frame.viewport_top as i32 + p.row - view.top;
            let left = pad + p.col as f32 * cw + p.offset_x as f32 * sx;
            let top = pad + row as f32 * ch + p.offset_y as f32 * sy;
            // ペインの外にはみ出す分は切る（右と下だけ。左・上は端末が負の行を出さない）
            let max_x = pad + cols as f32 * cw;
            let max_y = pad + view.rows as f32 * ch;
            if left >= max_x || top >= max_y || dw <= 0.0 || dh <= 0.0 {
                continue;
            }
            let vis_w = dw.min(max_x - left);
            let vis_h = dh.min(max_y - top);
            let (u0, v0) = (p.source_x as f32 / tw as f32, p.source_y as f32 / th as f32);
            let (u1, v1) = (
                (p.source_x + p.source_width) as f32 / tw as f32,
                (p.source_y + p.source_height) as f32 / th as f32,
            );
            // 切った分だけ切り取りも縮める
            let u1 = u0 + (u1 - u0) * (vis_w / dw);
            let v1 = v0 + (v1 - v0) * (vis_h / dh);
            out.push(ImageDraw { texture, rect: [left, top, vis_w, vis_h], uv: [u0, v0, u1, v1] });
        }
        out
    }
}

/// 端末の画素を RGBA8 にする
fn to_rgba(data: &[u8], w: u32, h: u32, format: ImageFormat) -> Option<Vec<u8>> {
    let n = (w as usize).checked_mul(h as usize)?;
    let stride = match format {
        ImageFormat::Rgb => 3,
        ImageFormat::Rgba => 4,
        ImageFormat::GrayAlpha => 2,
        ImageFormat::Gray => 1,
    };
    if data.len() < n * stride {
        return None;
    }
    let mut out = vec![0u8; n * 4];
    for i in 0..n {
        let c = &data[i * stride..i * stride + stride];
        let (r, g, b, a) = match format {
            ImageFormat::Rgba => (c[0], c[1], c[2], c[3]),
            ImageFormat::Rgb => (c[0], c[1], c[2], 255),
            ImageFormat::GrayAlpha => (c[0], c[0], c[0], c[1]),
            ImageFormat::Gray => (c[0], c[0], c[0], 255),
        };
        // シェーダは premultiplied で重ねる
        let m = |v: u8| ((v as u16 * a as u16) / 255) as u8;
        out[i * 4] = m(r);
        out[i * 4 + 1] = m(g);
        out[i * 4 + 2] = m(b);
        out[i * 4 + 3] = a;
    }
    Some(out)
}
