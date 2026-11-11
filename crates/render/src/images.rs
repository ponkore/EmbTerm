//! Kitty graphics の画像を Direct2D のビットマップにして描く（T-088）。
//!
//! 配置は libghostty-vt の render state が解決した結果（[`et_vt::ImagePlacement`]）をそのまま使う。Unicode placeholder の
//! 走査も相対配置の解決も向こうの仕事で、ここではビューの行へ移して切り取るだけ。

use et_vt::{ImageFormat, ImageLayer};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::frame::{Frame, ImagePixels, PlacedImage};
use crate::view::FrameView;
use crate::{Renderer, rect};

impl Renderer {
    /// `layer` の画像を描く。ビューの桁・行の範囲に切り取る（ペインより大きい画像・画面の外にはみ出す画像）
    pub(crate) fn draw_images(&mut self, dc: &ID2D1DeviceContext, frame: &Frame, view: &FrameView, cols: usize, layer: ImageLayer) {
        if !frame.images.iter().any(|i| i.placement.layer == layer) {
            return;
        }
        let (cw, ch) = (self.font.cell_w as f32, self.font.cell_h as f32);
        let pad = self.padding() as f32;
        // 配置は端末のセルの大きさで計算されている。ペインのセルが違えば（DPI の違うモニタ）拡大縮小する
        let sx = cw / frame.image_cell.0.max(1) as f32;
        let sy = ch / frame.image_cell.1.max(1) as f32;
        let clip = rect(pad, pad, pad + cols as f32 * cw, pad + view.rows as f32 * ch);
        unsafe { dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED) };
        for image in frame.images.iter().filter(|i| i.placement.layer == layer) {
            let Some(bitmap) = self.image_bitmap(dc, image) else { continue };
            let p = image.placement;
            let row = frame.viewport_top as i32 + p.row - view.top;
            let left = pad + p.col as f32 * cw + p.offset_x as f32 * sx;
            let top = pad + row as f32 * ch + p.offset_y as f32 * sy;
            let dest = rect(left, top, left + p.width as f32 * sx, top + p.height as f32 * sy);
            let src = rect(
                p.source_x as f32,
                p.source_y as f32,
                (p.source_x + p.source_width) as f32,
                (p.source_y + p.source_height) as f32,
            );
            // 拡大縮小しないときは画素のまま（補間でにじませない）
            let mode = if (p.width, p.height) == (p.source_width, p.source_height) && (sx, sy) == (1.0, 1.0) {
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR
            } else {
                D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC
            };
            unsafe { dc.DrawBitmap(&bitmap, Some(&dest), 1.0, mode, Some(&src), None) };
        }
        unsafe { dc.PopAxisAlignedClip() };
    }

    /// 画像のビットマップ（無ければ作る）。作れないものは `None`（Direct2D の上限を超える画像。その画像だけ描かない）。
    /// デバイスを失っているときも `None` になり、その失敗は `EndDraw` の結果で分かる
    fn image_bitmap(&mut self, dc: &ID2D1DeviceContext, image: &PlacedImage) -> Option<ID2D1Bitmap1> {
        let generation = image.placement.generation;
        if let Some(b) = self.bitmaps.get(&generation) {
            return Some(b.clone());
        }
        let pixels = &image.pixels;
        let max = unsafe { dc.GetMaximumBitmapSize() };
        if pixels.width == 0 || pixels.height == 0 || pixels.width > max || pixels.height > max {
            return None;
        }
        let bgra = to_premultiplied_bgra(pixels)?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let size = D2D_SIZE_U { width: pixels.width, height: pixels.height };
        let bitmap = unsafe { dc.CreateBitmap(size, Some(bgra.as_ptr().cast()), pixels.width * 4, &props) }.ok()?;
        self.bitmaps.insert(generation, bitmap.clone());
        Some(bitmap)
    }

    /// このフレームで使わないビットマップを捨てる（画像の世代が変わった、画像が消された）
    pub(crate) fn prune_images(&mut self, frame: &Frame) {
        self.bitmaps.retain(|g, _| frame.images.iter().any(|i| i.placement.generation == *g));
    }
}

fn premultiply(c: u8, a: u8) -> u8 {
    if a == 255 {
        c
    } else {
        ((c as u32 * a as u32 + 127) / 255) as u8
    }
}

/// 端末の画素（RGB / RGBA / 灰色）を Direct2D の BGRA（アルファ乗算済み）にする。大きさが合わなければ `None`
pub(crate) fn to_premultiplied_bgra(p: &ImagePixels) -> Option<Vec<u8>> {
    let n = p.width as usize * p.height as usize;
    let bytes = match p.format {
        ImageFormat::Rgb => 3,
        ImageFormat::Rgba => 4,
        ImageFormat::GrayAlpha => 2,
        ImageFormat::Gray => 1,
    };
    if p.data.len() != n.checked_mul(bytes)? {
        return None;
    }
    let mut out = Vec::with_capacity(n * 4);
    match p.format {
        ImageFormat::Rgba => {
            for c in p.data.as_chunks::<4>().0 {
                out.extend_from_slice(&[premultiply(c[2], c[3]), premultiply(c[1], c[3]), premultiply(c[0], c[3]), c[3]]);
            }
        }
        ImageFormat::Rgb => {
            for c in p.data.as_chunks::<3>().0 {
                out.extend_from_slice(&[c[2], c[1], c[0], 255]);
            }
        }
        ImageFormat::GrayAlpha => {
            for c in p.data.as_chunks::<2>().0 {
                let g = premultiply(c[0], c[1]);
                out.extend_from_slice(&[g, g, g, c[1]]);
            }
        }
        ImageFormat::Gray => {
            for &g in &p.data {
                out.extend_from_slice(&[g, g, g, 255]);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(format: ImageFormat, data: Vec<u8>) -> ImagePixels {
        ImagePixels { width: 1, height: 1, format, data }
    }

    #[test]
    fn converts_each_format() {
        assert_eq!(to_premultiplied_bgra(&px(ImageFormat::Rgb, vec![1, 2, 3])).unwrap(), [3, 2, 1, 255]);
        assert_eq!(to_premultiplied_bgra(&px(ImageFormat::Rgba, vec![10, 20, 30, 255])).unwrap(), [30, 20, 10, 255]);
        // 半透明はアルファを掛ける
        assert_eq!(to_premultiplied_bgra(&px(ImageFormat::Rgba, vec![255, 128, 0, 128])).unwrap(), [0, 64, 128, 128]);
        assert_eq!(to_premultiplied_bgra(&px(ImageFormat::Gray, vec![9])).unwrap(), [9, 9, 9, 255]);
        assert_eq!(to_premultiplied_bgra(&px(ImageFormat::GrayAlpha, vec![255, 0])).unwrap(), [0, 0, 0, 0]);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(to_premultiplied_bgra(&px(ImageFormat::Rgba, vec![0; 3])).is_none());
    }
}
