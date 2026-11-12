//! Kitty graphics の画像を Direct2D のビットマップにして描く（T-088）。
//!
//! 配置は libghostty-vt の render state が解決した結果（[`et_vt::ImagePlacement`]）をそのまま使う。Unicode placeholder の
//! 走査も相対配置の解決も向こうの仕事で、ここではビューの行へ移して切り取るだけ。
//!
//! 大きな画像（T-105）: 表示が元の半分より小さければ、表示の大きさまで縮めてから GPU に載せる。画素の多い画像は、
//! BGRA への変換と縮小を別のスレッドで行い、終わったら持ち主に知らせて描き直してもらう（UI スレッドを塞がない）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use et_vt::{ImageFormat, ImageLayer};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::frame::{Frame, ImagePixels, PlacedImage};
use crate::view::FrameView;
use crate::{Renderer, rect};

/// GPU のビットマップの鍵（画像の世代, 載せた幅, 高さ）
pub(crate) type BitmapKey = (u64, u32, u32);

/// 載せる大きさがこれより画素の多い画像は、変換を別のスレッドで行う（T-105）
const ASYNC_PIXELS: u64 = 1024 * 1024;

type Finished = (BitmapKey, Option<Vec<u8>>);

/// 画像の変換を別のスレッドで行う口（T-105）。終わったら `ready` を呼んで持ち主に描き直してもらう
#[derive(Default)]
pub(crate) struct ImageJobs {
    ready: Option<Arc<dyn Fn() + Send + Sync>>,
    tx: Option<Sender<Finished>>,
    rx: Option<Receiver<Finished>>,
    /// 変換中のもの
    pending: HashSet<BitmapKey>,
}

impl Renderer {
    /// 大きな画像の変換を別のスレッドで行うようにする（T-105）。終わったら `ready` が（そのスレッドから）呼ばれるので、
    /// 持ち主は描き直す。設定しなければ UI スレッドでその場で変換する（embterm.exe）
    pub fn set_image_ready(&mut self, ready: impl Fn() + Send + Sync + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.image_jobs = ImageJobs { ready: Some(Arc::new(ready)), tx: Some(tx), rx: Some(rx), pending: HashSet::new() };
    }

    /// 別のスレッドで変換中の画像があるか（自動確認用）
    pub fn images_pending(&self) -> usize {
        self.image_jobs.pending.len()
    }

    /// `layer` の画像を描く。ビューの桁・行の範囲に切り取る（ペインより大きい画像・画面の外にはみ出す画像）
    pub(crate) fn draw_images(&mut self, dc: &ID2D1DeviceContext, frame: &Frame, view: &FrameView, cols: usize, layer: ImageLayer) {
        if !frame.images.iter().any(|i| i.placement.layer == layer) {
            return;
        }
        self.take_finished_images(dc);
        let (cw, ch) = (self.font.cell_w as f32, self.font.cell_h as f32);
        let pad = self.padding() as f32;
        // 配置は端末のセルの大きさで計算されている。ペインのセルが違えば（DPI の違うモニタ）拡大縮小する
        let sx = cw / frame.image_cell.0.max(1) as f32;
        let sy = ch / frame.image_cell.1.max(1) as f32;
        let clip = rect(pad, pad, pad + cols as f32 * cw, pad + view.rows as f32 * ch);
        unsafe { dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED) };
        for image in frame.images.iter().filter(|i| i.placement.layer == layer) {
            let p = image.placement;
            let (dw, dh) = (p.width as f32 * sx, p.height as f32 * sy);
            let Some((bitmap, bw, bh)) = self.image_bitmap(dc, image, dw, dh) else { continue };
            let row = frame.viewport_top as i32 + p.row - view.top;
            let left = pad + p.col as f32 * cw + p.offset_x as f32 * sx;
            let top = pad + row as f32 * ch + p.offset_y as f32 * sy;
            let dest = rect(left, top, left + dw, top + dh);
            // 縮めて載せたビットマップでは、元の画素の座標を縮めた分だけ直す
            let kx = bw as f32 / image.pixels.width.max(1) as f32;
            let ky = bh as f32 / image.pixels.height.max(1) as f32;
            let src = rect(
                p.source_x as f32 * kx,
                p.source_y as f32 * ky,
                (p.source_x + p.source_width) as f32 * kx,
                (p.source_y + p.source_height) as f32 * ky,
            );
            // 拡大縮小しないときは画素のまま（補間でにじませない）
            let mode = if (dw, dh) == (src.right - src.left, src.bottom - src.top) {
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR
            } else {
                D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC
            };
            unsafe { dc.DrawBitmap(&bitmap, Some(&dest), 1.0, mode, Some(&src), None) };
        }
        unsafe { dc.PopAxisAlignedClip() };
    }

    /// 別のスレッドで変換し終えた画素を GPU のビットマップにする
    fn take_finished_images(&mut self, dc: &ID2D1DeviceContext) {
        let Some(rx) = &self.image_jobs.rx else { return };
        let done: Vec<Finished> = rx.try_iter().collect();
        for (key, bgra) in done {
            self.image_jobs.pending.remove(&key);
            if let Some(bgra) = bgra
                && let Some(b) = create_bitmap(dc, key.1, key.2, &bgra)
            {
                self.bitmaps.insert(key, b);
            }
        }
    }

    /// 画像を表示の大きさ `dw` x `dh`（画素）で描くためのビットマップと、その幅・高さ。無ければ作る。
    /// - 表示が元の半分より小さければ、表示の大きさまで縮めて載せる（GPU に載せる量と時間を減らす。T-105）
    /// - 載せる画素の多い画像は別のスレッドで変換し、その間は前に作った同じ画像のビットマップ（大きさ違い）で描くか、描かない
    ///
    /// 作れないものは `None`（Direct2D の上限を超える画像。その画像だけ描かない）。
    /// デバイスを失っているときも `None` になり、その失敗は `EndDraw` の結果で分かる
    fn image_bitmap(&mut self, dc: &ID2D1DeviceContext, image: &PlacedImage, dw: f32, dh: f32) -> Option<(ID2D1Bitmap1, u32, u32)> {
        let generation = image.placement.generation;
        let pixels = &image.pixels;
        let p = image.placement;
        let (w, h) = target_size(pixels.width, pixels.height, p.source_width, p.source_height, dw, dh);
        let key = (generation, w, h);
        if let Some(b) = self.bitmaps.get(&key) {
            return Some((b.clone(), w, h));
        }
        let max = unsafe { dc.GetMaximumBitmapSize() };
        if w == 0 || h == 0 || w > max || h > max {
            return None;
        }
        let heavy = w as u64 * h as u64 >= ASYNC_PIXELS || pixels.width as u64 * pixels.height as u64 >= ASYNC_PIXELS;
        if heavy && let (Some(tx), Some(ready)) = (&self.image_jobs.tx, &self.image_jobs.ready) {
            if self.image_jobs.pending.insert(key) {
                let (tx, ready, pixels) = (tx.clone(), ready.clone(), pixels.clone());
                let spawned = std::thread::Builder::new().name("image-convert".into()).spawn(move || {
                    let _ = tx.send((key, scaled_premultiplied_bgra(&pixels, w, h)));
                    ready();
                });
                if spawned.is_err() {
                    self.image_jobs.pending.remove(&key);
                }
            }
            // できるまでは、同じ画像の別の大きさのビットマップがあればそれで描く
            return self.bitmaps.iter().find(|(k, _)| k.0 == generation).map(|(k, b)| (b.clone(), k.1, k.2));
        }
        let bgra = scaled_premultiplied_bgra(pixels, w, h)?;
        let bitmap = create_bitmap(dc, w, h, &bgra)?;
        self.bitmaps.insert(key, bitmap.clone());
        Some((bitmap, w, h))
    }

    /// このフレームで使わないビットマップを捨てる（画像の世代が変わった、画像が消された）。
    /// 同じ画像の大きさ違いは 2 つまで残す（ペインの大きさを行き来したとき・縮めたものを作る間に描くため）
    pub(crate) fn prune_images(&mut self, frame: &Frame) {
        self.bitmaps.retain(|k, _| frame.images.iter().any(|i| i.placement.generation == k.0));
        let mut by_gen: HashMap<u64, Vec<BitmapKey>> = HashMap::new();
        for k in self.bitmaps.keys() {
            by_gen.entry(k.0).or_default().push(*k);
        }
        for (_, mut keys) in by_gen {
            if keys.len() > 2 {
                // 大きいものから 2 つを残す
                keys.sort_by_key(|k| std::cmp::Reverse(k.1 as u64 * k.2 as u64));
                for k in &keys[2..] {
                    self.bitmaps.remove(k);
                }
            }
        }
    }
}

/// GPU のビットマップを作る（BGRA、アルファ乗算済み）
fn create_bitmap(dc: &ID2D1DeviceContext, w: u32, h: u32, bgra: &[u8]) -> Option<ID2D1Bitmap1> {
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    let size = D2D_SIZE_U { width: w, height: h };
    unsafe { dc.CreateBitmap(size, Some(bgra.as_ptr().cast()), w * 4, &props) }.ok()
}

/// 載せる大きさ。画像の切り取る範囲（`src_w` x `src_h`）が表示（`dw` x `dh` 画素）の 2 倍より大きければ、
/// 表示の大きさに合うまで画像全体を縮める。そうでなければ元の大きさ
pub(crate) fn target_size(width: u32, height: u32, src_w: u32, src_h: u32, dw: f32, dh: f32) -> (u32, u32) {
    let f = (dw / src_w.max(1) as f32).min(dh / src_h.max(1) as f32);
    if !(f > 0.0 && f < 0.5) {
        return (width, height);
    }
    (((width as f32 * f).ceil() as u32).max(1), ((height as f32 * f).ceil() as u32).max(1))
}

fn premultiply(c: u8, a: u8) -> u8 {
    if a == 255 {
        c
    } else {
        ((c as u32 * a as u32 + 127) / 255) as u8
    }
}

fn bytes_per_pixel(f: ImageFormat) -> usize {
    match f {
        ImageFormat::Rgb => 3,
        ImageFormat::Rgba => 4,
        ImageFormat::GrayAlpha => 2,
        ImageFormat::Gray => 1,
    }
}

/// 端末の画素（RGB / RGBA / 灰色）を Direct2D の BGRA（アルファ乗算済み）にする。大きさが合わなければ `None`
pub(crate) fn to_premultiplied_bgra(p: &ImagePixels) -> Option<Vec<u8>> {
    let n = p.width as usize * p.height as usize;
    if p.data.len() != n.checked_mul(bytes_per_pixel(p.format))? {
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

/// 端末の画素を `w` x `h` の BGRA（アルファ乗算済み）にする。縮めるときは面積の平均（乗算済みの値で平均する）
pub(crate) fn scaled_premultiplied_bgra(p: &ImagePixels, w: u32, h: u32) -> Option<Vec<u8>> {
    if (w, h) == (p.width, p.height) {
        return to_premultiplied_bgra(p);
    }
    let (sw, sh) = (p.width as usize, p.height as usize);
    let bytes = bytes_per_pixel(p.format);
    if w == 0 || h == 0 || p.data.len() != sw.checked_mul(sh)?.checked_mul(bytes)? {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    // 縮めた画素ごとの B・G・R・A の和と、足した数
    let mut sums = vec![[0u32; 5]; w * h];
    let col_of: Vec<usize> = (0..sw).map(|x| (x * w / sw).min(w - 1)).collect();
    for y in 0..sh {
        let row = &mut sums[(y * h / sh).min(h - 1) * w..][..w];
        let line = &p.data[y * sw * bytes..][..sw * bytes];
        for (x, c) in line.chunks_exact(bytes).enumerate() {
            let (r, g, b, a) = match p.format {
                ImageFormat::Rgba => (c[0], c[1], c[2], c[3]),
                ImageFormat::Rgb => (c[0], c[1], c[2], 255),
                ImageFormat::GrayAlpha => (c[0], c[0], c[0], c[1]),
                ImageFormat::Gray => (c[0], c[0], c[0], 255),
            };
            let s = &mut row[col_of[x]];
            s[0] += premultiply(b, a) as u32;
            s[1] += premultiply(g, a) as u32;
            s[2] += premultiply(r, a) as u32;
            s[3] += a as u32;
            s[4] += 1;
        }
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for s in sums {
        let n = s[4].max(1);
        out.extend_from_slice(&[(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8, (s[3] / n) as u8]);
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
    fn scales_down_by_area_average() {
        // 4x2 の RGBA（左半分が赤、右半分が青）を 2x1 に縮めると、赤と青が 1 画素ずつ（BGRA）
        let mut data = Vec::new();
        for _ in 0..2 {
            data.extend_from_slice(&[255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 255]);
        }
        let p = ImagePixels { width: 4, height: 2, format: ImageFormat::Rgba, data };
        assert_eq!(scaled_premultiplied_bgra(&p, 2, 1).unwrap(), [0, 0, 255, 255, 255, 0, 0, 255]);
        // 同じ大きさなら変換だけ
        assert_eq!(scaled_premultiplied_bgra(&p, 4, 2).unwrap().len(), 4 * 2 * 4);
    }

    #[test]
    fn target_size_shrinks_only_when_much_larger() {
        // 4000x4000 を 320x360 で見せる → 表示に合わせて縮める
        assert_eq!(target_size(4000, 4000, 4000, 4000, 320.0, 360.0), (320, 320));
        // 表示が半分以上なら元の大きさ
        assert_eq!(target_size(100, 80, 100, 80, 60.0, 50.0), (100, 80));
        // 一部だけ見せるときは、その範囲が表示に合うように全体を縮める
        assert_eq!(target_size(4000, 4000, 1000, 1000, 100.0, 100.0), (400, 400));
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(to_premultiplied_bgra(&px(ImageFormat::Rgba, vec![0; 3])).is_none());
    }
}
