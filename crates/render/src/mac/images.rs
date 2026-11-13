//! Kitty graphics の画像を Metal のテクスチャにして描く（T-127）。
//!
//! 配置は libghostty-vt の render state が解決した結果（[`et_vt::ImagePlacement`]）をそのまま使う。
//! Unicode placeholder の走査も相対配置の解決も向こうの仕事で、ここではビューの行へ移して切り取るだけ
//! （Direct2D 版の [`crate::d2d`] と同じ考え）。
//!
//! Direct2D 版との違い
//! - 画像 1 枚につき**描画の呼び出しを 1 回**（テクスチャを差し替えるため）。文字のアトラスとは別のテクスチャ
//! - **表示が元の半分より小さければ、表示の大きさまで縮めてから載せる**（T-105 と同じ考え。T-128）。
//!   縮小は箱フィルタ（平均）で CPU で行う
//! - 別のスレッドでの変換（T-105 の後半）はまだ無い（4000x4000 でも縮小込みで数十 ms）

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

/// テクスチャの鍵（画像の世代, 載せた幅, 高さ）
pub(crate) type ImageKey = (u64, u32, u32);

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
    textures: HashMap<ImageKey, Retained<ProtocolObject<dyn MTLTexture>>>,
}

// SAFETY: Metal のテクスチャは別のスレッドから使ってよい
unsafe impl Send for ImageCache {}

impl ImageCache {
    /// 画像のテクスチャ（無ければ作る）。`dw` `dh` は表示の大きさ（デバイス画素）
    fn texture(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        image: &PlacedImage,
        dw: f32,
        dh: f32,
    ) -> Option<(Retained<ProtocolObject<dyn MTLTexture>>, u32, u32)> {
        let (sw, sh) = (image.pixels.width.max(1), image.pixels.height.max(1));
        let (w, h) = target_size(sw, sh, image.placement.source_width, image.placement.source_height, dw, dh);
        let key = (image.placement.generation, w, h);
        if let Some(t) = self.textures.get(&key) {
            return Some((t.clone(), w, h));
        }
        let mut rgba = to_rgba(&image.pixels.data, sw, sh, image.pixels.format)?;
        if (w, h) != (sw, sh) {
            rgba = downscale(&rgba, sw, sh, w, h);
        }
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
        self.textures.insert(key, texture.clone());
        Some((texture, w, h))
    }

    /// フレームに無くなった画像のテクスチャを捨てる
    pub(crate) fn prune(&mut self, frame: &Frame) {
        self.textures.retain(|k, _| frame.images.iter().any(|i| i.placement.generation == k.0));
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
            let (dw, dh) = (p.width as f32 * sx, p.height as f32 * sy);
            let Some((texture, tw, th)) = self.texture(device, image, dw, dh) else { continue };
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
            // 縮めて載せたときは、元の画素の座標も同じだけ縮める
            let kx = tw as f32 / image.pixels.width.max(1) as f32;
            let ky = th as f32 / image.pixels.height.max(1) as f32;
            let (u0, v0) = (p.source_x as f32 * kx / tw as f32, p.source_y as f32 * ky / th as f32);
            let (u1, v1) = (
                (p.source_x + p.source_width) as f32 * kx / tw as f32,
                (p.source_y + p.source_height) as f32 * ky / th as f32,
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

/// 載せる大きさ（T-105 と同じ規則）。表示が元の半分より小さければ、表示の大きさまで縮める。
/// `source_*` が画像の一部なら、その割合で表示の大きさを割り戻してから比べる
fn target_size(sw: u32, sh: u32, src_w: u32, src_h: u32, dw: f32, dh: f32) -> (u32, u32) {
    if sw == 0 || sh == 0 || dw <= 0.0 || dh <= 0.0 {
        return (sw.max(1), sh.max(1));
    }
    // 画像全体を表示したときの大きさ
    let fx = if src_w > 0 { sw as f32 / src_w as f32 } else { 1.0 };
    let fy = if src_h > 0 { sh as f32 / src_h as f32 } else { 1.0 };
    let (full_w, full_h) = (dw * fx, dh * fy);
    if full_w * 2.0 >= sw as f32 || full_h * 2.0 >= sh as f32 {
        return (sw, sh); // 半分より大きく出すなら、そのまま載せる
    }
    (full_w.ceil().max(1.0) as u32, full_h.ceil().max(1.0) as u32)
}

/// 箱フィルタ（平均）で縮める。premultiplied のまま平均してよい
fn downscale(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    for y in 0..dh {
        let y0 = (y as u64 * sh as u64 / dh as u64) as u32;
        let y1 = (((y + 1) as u64 * sh as u64 / dh as u64) as u32).max(y0 + 1).min(sh);
        for x in 0..dw {
            let x0 = (x as u64 * sw as u64 / dw as u64) as u32;
            let x1 = (((x + 1) as u64 * sw as u64 / dw as u64) as u32).max(x0 + 1).min(sw);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = (sy as usize * sw as usize + sx as usize) * 4;
                    for c in 0..4 {
                        acc[c] += src[i + c] as u32;
                    }
                    n += 1;
                }
            }
            let o = (y as usize * dw as usize + x as usize) * 4;
            for c in 0..4 {
                out[o + c] = if n > 0 { (acc[c] / n) as u8 } else { 0 };
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{downscale, target_size};

    #[test]
    fn 大きすぎる画像は表示の大きさまで縮める() {
        // 4000x4000 を 200x200 で出す → 縮める
        assert_eq!(target_size(4000, 4000, 4000, 4000, 200.0, 200.0), (200, 200));
        // 半分より大きく出すならそのまま
        assert_eq!(target_size(400, 400, 400, 400, 300.0, 300.0), (400, 400));
        // 画像の一部（source）を出すときは、全体の大きさに直して比べる
        assert_eq!(target_size(1000, 1000, 500, 500, 100.0, 100.0), (200, 200));
    }

    #[test]
    fn 箱フィルタで平均する() {
        // 2x2（白・黒・黒・白）を 1x1 に → 平均
        let src = [255, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255];
        let out = downscale(&src, 2, 2, 1, 1);
        assert_eq!(out, vec![127, 127, 127, 255]);
    }
}
