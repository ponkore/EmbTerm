//! グリフのアトラス（T-125）。
//!
//! CoreText でグリフを 1 つずつビットマップに描き、1 枚の Metal のテクスチャに棚（shelf）詰めで載せる。
//! 鍵は（フェイス, グリフ番号, 縮める段）。縮める比（T-013 の shrink）は**その大きさで描き直す**ので、
//! 拡大縮小でぼけない（Direct2D 版がフォントの大きさを変えて `DrawGlyphRun` するのと同じ）。

use std::collections::HashMap;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo};
use objc2_core_text::{CTFont, CTFontOrientation};
use objc2_metal::{
    MTLDevice, MTLOrigin, MTLPixelFormat, MTLRegion, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
    MTLTextureUsage,
};

use super::font::{FontSet, GlyphKey};

/// アトラスの 1 辺（画素）。12pt / 拡大率 2 で 1 万グリフ以上入る
const ATLAS_SIZE: usize = 2048;
/// グリフの周りに空ける余白（にじみ防止）
const PAD: usize = 1;

/// アトラスに入っているグリフ 1 つ
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    /// テクスチャの中の位置（0..1）。左上・右下
    pub uv: [f32; 4],
    /// ペンの位置（ベースラインの左端）からの差（デバイス画素。下が正）と大きさ
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// 色付きのグリフ（絵文字）。前景色で塗らない
    pub color: bool,
}

pub struct Atlas {
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    entries: HashMap<GlyphKey, Option<Entry>>,
    /// 棚詰めの現在地
    x: usize,
    y: usize,
    shelf_h: usize,
}

impl Atlas {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Result<Self, String> {
        let td = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                ATLAS_SIZE,
                ATLAS_SIZE,
                false,
            )
        };
        td.setUsage(MTLTextureUsage::ShaderRead);
        td.setStorageMode(MTLStorageMode::Shared);
        let texture = device.newTextureWithDescriptor(&td).ok_or("アトラスのテクスチャを作れません")?;
        Ok(Self { texture, entries: HashMap::new(), x: PAD, y: PAD, shelf_h: 0 })
    }

    pub fn texture(&self) -> &ProtocolObject<dyn MTLTexture> {
        &self.texture
    }

    /// グリフを引く（無ければ描いて載せる）。空白など中身の無いグリフは `None`
    pub fn get(&mut self, fonts: &mut FontSet, key: GlyphKey) -> Option<Entry> {
        if let Some(e) = self.entries.get(&key) {
            return *e;
        }
        let entry = self.rasterize(fonts, key);
        self.entries.insert(key, entry);
        entry
    }

    fn rasterize(&mut self, fonts: &mut FontSet, key: GlyphKey) -> Option<Entry> {
        let scale = key.scale_q as f32 / 256.0;
        let base = fonts.face(key.face);
        let size = (fonts.px_size * scale).max(1.0);
        // 縮めるときは、その大きさのフォントで描き直す（拡大縮小でぼけない）
        let scaled = ((scale - 1.0).abs() > 1e-3)
            .then(|| unsafe { base.copy_with_attributes(size as f64, std::ptr::null(), None) });
        let font: &CTFont = scaled.as_deref().unwrap_or(base);
        let glyphs = [key.glyph];
        let mut rects = [CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0)); 1];
        unsafe {
            font.bounding_rects_for_glyphs(CTFontOrientation::Default, NonNull::from(&glyphs[0]), rects.as_mut_ptr(), 1)
        };
        let r = rects[0];
        let w = (r.size.width.ceil() as usize) + 2;
        let h = (r.size.height.ceil() as usize) + 2;
        if r.size.width <= 0.0 || r.size.height <= 0.0 || w > ATLAS_SIZE || h > ATLAS_SIZE {
            return None; // 空白など、描くものが無い
        }

        // CoreText でビットマップに描く（白・不透明。色付きのフォントはそのまま色が出る）
        let mut pixels = vec![0u8; w * h * 4];
        let cs = CGColorSpace::new_device_rgb()?;
        let ctx = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr() as *mut _,
                w,
                h,
                8,
                w * 4,
                Some(&cs),
                CGImageAlphaInfo::PremultipliedLast.0,
            )
        }?;
        let ox = -r.origin.x + 1.0;
        let oy = -r.origin.y + 1.0;
        unsafe {
            CGContext::set_rgb_fill_color(Some(&ctx), 1.0, 1.0, 1.0, 1.0);
            CGContext::set_should_antialias(Some(&ctx), true);
            // サブピクセルのアンチエイリアスは使わない（アトラスは単色のマスクとして持つ）
            CGContext::set_should_smooth_fonts(Some(&ctx), false);
            let pos = [CGPoint::new(ox, oy)];
            font.draw_glyphs(NonNull::from(&glyphs[0]), NonNull::from(&pos[0]), 1, &ctx);
        }

        let (ax, ay) = self.alloc(w, h)?;
        unsafe {
            self.texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                MTLRegion {
                    origin: MTLOrigin { x: ax, y: ay, z: 0 },
                    size: MTLSize { width: w, height: h, depth: 1 },
                },
                0,
                NonNull::new(pixels.as_mut_ptr() as *mut _)?,
                w * 4,
            );
        }
        let s = ATLAS_SIZE as f32;
        let metrics = fonts.measure_glyph(key.face, key.glyph);
        Some(Entry {
            uv: [ax as f32 / s, ay as f32 / s, (ax + w) as f32 / s, (ay + h) as f32 / s],
            left: (r.origin.x - 1.0) as f32,
            // CoreText は上が正、描くほうは下が正
            top: -((r.origin.y + r.size.height) as f32) - 1.0,
            width: w as f32,
            height: h as f32,
            color: metrics.color,
        })
    }

    /// 棚詰めで場所を取る（いっぱいなら `None`）
    fn alloc(&mut self, w: usize, h: usize) -> Option<(usize, usize)> {
        if self.x + w + PAD > ATLAS_SIZE {
            self.x = PAD;
            self.y += self.shelf_h + PAD;
            self.shelf_h = 0;
        }
        if self.y + h + PAD > ATLAS_SIZE {
            return None; // いっぱい（フォントを変えたときなどに起こりうる。T-125 の段階では諦める）
        }
        let at = (self.x, self.y);
        self.x += w + PAD;
        self.shelf_h = self.shelf_h.max(h);
        Some(at)
    }

    /// フォントを変えたら全部捨てる
    pub fn clear(&mut self) {
        self.entries.clear();
        self.x = PAD;
        self.y = PAD;
        self.shelf_h = 0;
    }
}
