//! macOS の描画: Metal + CoreText のグリフアトラス（T-125）。
//!
//! Direct2D 版（[`crate::d2d`]）との違い
//!
//! - 文字は**グリフのアトラス**（[`atlas`]）に焼いて、四角 1 枚ずつのインスタンス描画で貼る。
//!   Direct2D の `DrawGlyphRun` に当たる近道が macOS には無いため（T-125 の判断）
//! - 背景・文字・装飾・カーソルを**同じ 1 つのインスタンスの列**に順に積み、1 回の描画で出す。
//!   並べた順がそのまま重なりの順になる
//! - 描く先は CAMetalLayer（窓）か、オフスクリーンのテクスチャ（PNG の保存・自動確認）
//! - 座標はすべてデバイス画素。論理ポイント → デバイス画素の拡大率（Retina なら 2.0）は
//!   フォントの大きさと余白に掛ける（Windows の DPI と同じ考え）
//!
//! 画像（Kitty graphics。T-088）はまだ描かない（T-127）。
//!
//! unsafe（Objective-C の呼び出し）はこのモジュールの中に閉じ込める。

mod atlas;
mod font;
mod gpu;
mod images;

use std::path::Path;

use et_vt::{CursorShape, ImageLayer, Rgb, StyleExt, Underline, Wide};
use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLDevice, MTLLoadAction, MTLOrigin,
    MTLPixelFormat, MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLResourceOptions,
    MTLSize, MTLStorageMode, MTLStoreAction, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

use crate::frame::{Cell, Frame, PLACEHOLDER};
use crate::layout::{self, RowCursor};
use crate::png;
use crate::theme::{NarrowWide, Theme};
use crate::view::FrameView;

use atlas::Atlas;
use images::{ImageCache, ImageDraw};
use font::{GlyphKey, style_index};
pub use font::FontSpec;
pub use gpu::Gpu;

/// 描く四角 1 つ（シェーダの `Inst` と同じ並び）
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Inst {
    /// x, y, 幅, 高さ（デバイス画素）
    rect: [f32; 4],
    color: [f32; 4],
    /// アトラスの中の位置（左上・右下、0..1）
    uv: [f32; 4],
    /// 0 = 塗り、1 = マスクのグリフ（前景色で塗る）、2 = 色付きのグリフ
    mode: u32,
    _pad: [u32; 3],
}

impl Inst {
    fn solid(x: f32, y: f32, w: f32, h: f32, c: Rgb) -> Self {
        Self { rect: [x, y, w, h], color: rgba(c), uv: [0.0; 4], mode: 0, _pad: [0; 3] }
    }
}

fn rgba(c: Rgb) -> [f32; 4] {
    [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, 1.0]
}

/// 描く先
enum Target {
    /// 窓（CAMetalLayer）
    Layer(Retained<CAMetalLayer>),
    /// オフスクリーン（PNG の保存・自動確認）
    Offscreen(Retained<ProtocolObject<dyn MTLTexture>>),
}

/// 端末 1 つ分の描画
pub struct Renderer {
    gpu: Gpu,
    target: Target,
    atlas: Atlas,
    fonts: font::FontSet,
    theme: Theme,
    /// 描く先の大きさ（デバイス画素）
    size: (u32, u32),
    /// 論理ポイント → デバイス画素
    scale: f32,
    insts: Vec<Inst>,
    /// IME の変換中の文字列（T-127）。カーソルの位置に重ねて描く
    preedit: Option<String>,
    /// Kitty graphics の画像（T-127）
    images: ImageCache,
    /// 1 フレーム分の画像の描画（`insts` の何番目の後ろに描くか）
    image_draws: Vec<(usize, ImageDraw)>,
    /// 描いた回数と、実際に画面に出した回数（T-125 の確認用）
    stats: DrawStats,
}

/// 描画の回数（デバッグ・自動確認用）
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawStats {
    /// 組み立てて描いた回数
    pub drawn: u64,
    /// CAMetalLayer の drawable に出した回数
    pub presented: u64,
    /// drawable を取れずに見送った回数
    pub skipped: u64,
    /// 最後の 1 枚のインスタンスの数（四角の枚数）
    pub instances: usize,
    pub last_draw: std::time::Duration,
    /// 1 枚にかかった時間の最大と合計（T-125 の測定。画面の保存を含む回は数えない）
    pub max_draw: std::time::Duration,
    pub total_draw: std::time::Duration,
    /// そのうち `nextDrawable`（垂直同期の待ち）の合計と最大。**描く仕事そのものは `total_draw - total_wait`**
    pub total_wait: std::time::Duration,
    pub max_wait: std::time::Duration,
}

impl Renderer {
    /// 窓（CAMetalLayer）に描く
    pub fn for_layer(layer: &CAMetalLayer, width: u32, height: u32, scale: f32, spec: FontSpec) -> Result<Self, String> {
        let gpu = Gpu::shared()?;
        layer.setDevice(Some(&gpu.device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(true);
        layer.setContentsScale(scale as f64);
        let r = Self::with_target(gpu, Target::Layer(layer.retain()), width, height, scale, spec)?;
        r.set_drawable_size();
        Ok(r)
    }

    /// オフスクリーンに描く（窓を作らずに PNG を書く。T-124 の自動確認で使う）
    pub fn offscreen(width: u32, height: u32, scale: f32, spec: FontSpec) -> Result<Self, String> {
        let gpu = Gpu::shared()?;
        let texture = gpu.offscreen_texture(width.max(1), height.max(1))?;
        Self::with_target(gpu, Target::Offscreen(texture), width, height, scale, spec)
    }

    fn with_target(gpu: Gpu, target: Target, width: u32, height: u32, scale: f32, spec: FontSpec) -> Result<Self, String> {
        let atlas = Atlas::new(&gpu.device)?;
        let fonts = font::FontSet::new(spec, scale)?;
        Ok(Self {
            gpu,
            target,
            atlas,
            fonts,
            theme: Theme::default(),
            size: (width.max(1), height.max(1)),
            scale,
            insts: Vec::new(),
            preedit: None,
            images: ImageCache::default(),
            image_draws: Vec::new(),
            stats: DrawStats::default(),
        })
    }

    /// 描画の回数（T-125 の確認用）
    pub fn stats(&self) -> DrawStats {
        self.stats
    }

    /// IME の変換中の文字列（T-127）。`None` で消す。カーソルの位置から描き、下線を引く
    pub fn set_preedit(&mut self, text: Option<String>) {
        self.preedit = text.filter(|t| !t.is_empty());
    }

    /// IME の変換中の文字列（無ければ `None`。自動確認用。T-133）
    pub fn preedit(&self) -> Option<&str> {
        self.preedit.as_deref()
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn font_family(&self) -> &str {
        &self.fonts.family
    }

    /// セルの大きさ（デバイス画素）
    pub fn cell_size(&self) -> (u32, u32) {
        (self.fonts.cell_w, self.fonts.cell_h)
    }

    /// 論理ポイント → デバイス画素の拡大率
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// 端末の周りの余白（デバイス画素）
    pub fn padding(&self) -> u32 {
        (self.theme.padding as f32 * self.scale).round() as u32
    }

    /// 描く先の大きさ（デバイス画素）から桁数・行数を求める
    pub fn grid_size(&self, width: u32, height: u32) -> (u16, u16) {
        let pad = self.padding() * 2;
        let (cw, ch) = self.cell_size();
        let cols = width.saturating_sub(pad) / cw.max(1);
        let rows = height.saturating_sub(pad) / ch.max(1);
        (cols.clamp(1, u16::MAX as u32) as u16, rows.clamp(1, u16::MAX as u32) as u16)
    }

    /// 桁数・行数から描く先の大きさ（デバイス画素）を求める
    pub fn client_size_for(&self, cols: u16, rows: u16) -> (u32, u32) {
        let pad = self.padding() * 2;
        let (cw, ch) = self.cell_size();
        (cols as u32 * cw + pad, rows as u32 * ch + pad)
    }

    /// 画素の位置がどのセルか（ドラッグでの選択・マウスの報告用）
    pub fn cell_at(&self, x: i32, y: i32, cols: u16, rows: u16) -> (u16, u16) {
        let pad = self.padding() as i32;
        let (cw, ch) = self.cell_size();
        let cx = ((x - pad).max(0) / cw as i32).clamp(0, cols.saturating_sub(1) as i32);
        let cy = ((y - pad).max(0) / ch as i32).clamp(0, rows.saturating_sub(1) as i32);
        (cx as u16, cy as u16)
    }

    pub fn set_font(&mut self, spec: FontSpec) -> Result<(), String> {
        self.fonts = font::FontSet::new(spec, self.scale)?;
        self.atlas.clear();
        Ok(())
    }

    /// 描く先の大きさ（デバイス画素）と拡大率を変える
    pub fn resize(&mut self, width: u32, height: u32, scale: f32) -> Result<(), String> {
        let refont = (scale - self.scale).abs() > 1e-3;
        self.size = (width.max(1), height.max(1));
        self.scale = scale;
        if refont {
            let spec = self.fonts.spec.clone();
            self.set_font(spec)?;
        }
        match &self.target {
            Target::Layer(_) => self.set_drawable_size(),
            Target::Offscreen(_) => {
                self.target = Target::Offscreen(self.gpu.offscreen_texture(self.size.0, self.size.1)?);
            }
        }
        Ok(())
    }

    fn set_drawable_size(&self) {
        if let Target::Layer(layer) = &self.target {
            layer.setContentsScale(self.scale as f64);
            layer.setDrawableSize(objc2_core_foundation::CGSize::new(self.size.0 as f64, self.size.1 as f64));
        }
    }

    /// フレーム全体を描く（embterm 用）
    pub fn draw(&mut self, frame: &Frame, focused: bool, capture: Option<&Path>) -> Result<(), String> {
        self.draw_view(frame, &FrameView::whole(frame), focused, capture)
    }

    /// `frame` の一部を描く。`capture` を渡すと、同じ中身をオフスクリーンに描いて PNG に保存する
    pub fn draw_view(&mut self, frame: &Frame, view: &FrameView, focused: bool, capture: Option<&Path>) -> Result<(), String> {
        let t0 = std::time::Instant::now();
        self.build(frame, view, focused);
        self.stats.drawn += 1;
        self.stats.instances = self.insts.len();
        match &self.target {
            Target::Layer(layer) => {
                let layer = layer.clone();
                let tw = std::time::Instant::now();
                let drawable = layer.nextDrawable();
                let wait = tw.elapsed();
                self.stats.total_wait += wait;
                self.stats.max_wait = self.stats.max_wait.max(wait);
                let Some(drawable) = drawable else {
                    self.stats.skipped += 1;
                    return Ok(()); // まだ出せない（次のフレームで）
                };
                let texture = drawable.texture();
                self.encode(&texture, frame.background, false)?;
                let cb = self.gpu.queue.commandBuffer().ok_or("コマンドバッファを作れません")?;
                cb.presentDrawable(ProtocolObject::from_ref(&*drawable));
                cb.commit();
                self.stats.presented += 1;
            }
            Target::Offscreen(texture) => {
                let texture = texture.clone();
                self.encode(&texture, frame.background, true)?;
            }
        }
        if let Some(path) = capture {
            self.capture(frame, view, focused, path)?;
        }
        let dt = t0.elapsed();
        self.stats.last_draw = dt;
        if capture.is_none() {
            self.stats.max_draw = self.stats.max_draw.max(dt);
            self.stats.total_draw += dt;
        }
        Ok(())
    }

    /// 同じ中身をオフスクリーンに描いて PNG にする
    fn capture(&mut self, frame: &Frame, view: &FrameView, focused: bool, path: &Path) -> Result<(), String> {
        let (w, h, data) = self.read_pixels(frame, view, focused)?;
        png::write_bgra(path, w, h, &data, w as usize * 4).map_err(|e| e.to_string())
    }

    /// オフスクリーンに描いて画素を読み戻す（BGRA）。PNG の保存と自動確認で使う
    pub fn read_pixels(&mut self, frame: &Frame, view: &FrameView, focused: bool) -> Result<(u32, u32, Vec<u8>), String> {
        let (w, h) = self.size;
        let texture = self.gpu.offscreen_texture(w, h)?;
        self.build(frame, view, focused);
        self.encode(&texture, frame.background, true)?;
        let pitch = w as usize * 4;
        let mut data = vec![0u8; pitch * h as usize];
        unsafe {
            texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(data.as_mut_ptr() as *mut _).ok_or("読み戻しの領域")?,
                pitch,
                MTLRegion {
                    origin: MTLOrigin { x: 0, y: 0, z: 0 },
                    size: MTLSize { width: w as usize, height: h as usize, depth: 1 },
                },
                0,
            );
        }
        Ok((w, h, data))
    }

    /// 組み立てたインスタンスを描く。`wait` は GPU の終わりを待つ（読み戻すときだけ true。
    /// 窓に出すときは待たない = CPU を止めない）
    fn encode(&self, texture: &ProtocolObject<dyn MTLTexture>, bg: Rgb, wait: bool) -> Result<(), String> {
        let rp = MTLRenderPassDescriptor::renderPassDescriptor();
        let ca = unsafe { rp.colorAttachments().objectAtIndexedSubscript(0) };
        ca.setTexture(Some(texture));
        ca.setLoadAction(MTLLoadAction::Clear);
        ca.setStoreAction(MTLStoreAction::Store);
        ca.setClearColor(MTLClearColor {
            red: srgb(bg.r),
            green: srgb(bg.g),
            blue: srgb(bg.b),
            alpha: 1.0,
        });
        let cb = self.gpu.queue.commandBuffer().ok_or("コマンドバッファを作れません")?;
        let enc = cb.renderCommandEncoderWithDescriptor(&rp).ok_or("エンコーダを作れません")?;
        if !self.insts.is_empty() || !self.image_draws.is_empty() {
            let bytes = std::mem::size_of_val(&self.insts[..]);
            let buf = unsafe {
                self.gpu.device.newBufferWithBytes_length_options(
                    std::ptr::NonNull::new(self.insts.as_ptr() as *mut _).ok_or("インスタンスの領域")?,
                    bytes,
                    MTLResourceOptions::StorageModeShared,
                )
            }
            .ok_or("インスタンスのバッファを作れません")?;
            let viewport = [self.size.0 as f32, self.size.1 as f32];
            enc.setRenderPipelineState(&self.gpu.pipeline);
            unsafe {
                enc.setVertexBuffer_offset_atIndex(Some(&buf), 0, 0);
                enc.setVertexBytes_length_atIndex(
                    std::ptr::NonNull::new(viewport.as_ptr() as *mut _).ok_or("viewport")?,
                    std::mem::size_of_val(&viewport),
                    1,
                );
            }
            // 四角を順に描く。画像はその間に挟む（テクスチャを差し替えるので別の呼び出しになる）
            let mut from = 0usize;
            let mut images = self.image_draws.iter().peekable();
            while from < self.insts.len() || images.peek().is_some() {
                let until = images.peek().map(|(at, _)| *at).unwrap_or(self.insts.len()).min(self.insts.len());
                if until > from {
                    unsafe {
                        // 画像を描いた後は頂点バッファが差し替わっているので、戻してから描く
                        enc.setVertexBuffer_offset_atIndex(Some(&buf), 0, 0);
                        enc.setFragmentTexture_atIndex(Some(self.atlas.texture()), 0);
                        enc.drawPrimitives_vertexStart_vertexCount_instanceCount_baseInstance(
                            MTLPrimitiveType::TriangleStrip,
                            0,
                            4,
                            until - from,
                            from,
                        );
                    }
                    from = until;
                }
                let Some((_, draw)) = images.next() else { break };
                self.draw_image(&enc, draw, &viewport)?;
            }
        }
        enc.endEncoding();
        cb.commit();
        if wait {
            cb.waitUntilCompleted();
        }
        Ok(())
    }

    /// 画像 1 枚を描く（テクスチャを差し替えて四角 1 枚）
    fn draw_image(
        &self,
        enc: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        draw: &ImageDraw,
        viewport: &[f32; 2],
    ) -> Result<(), String> {
        let inst = Inst {
            rect: draw.rect,
            color: [1.0, 1.0, 1.0, 1.0],
            uv: draw.uv,
            mode: 2,
            _pad: [0; 3],
        };
        let buf = unsafe {
            self.gpu.device.newBufferWithBytes_length_options(
                std::ptr::NonNull::new(&inst as *const Inst as *mut _).ok_or("画像のインスタンス")?,
                std::mem::size_of::<Inst>(),
                MTLResourceOptions::StorageModeShared,
            )
        }
        .ok_or("画像のバッファを作れません")?;
        unsafe {
            enc.setVertexBuffer_offset_atIndex(Some(&buf), 0, 0);
            enc.setVertexBytes_length_atIndex(
                std::ptr::NonNull::new(viewport.as_ptr() as *mut _).ok_or("viewport")?,
                std::mem::size_of_val(viewport),
                1,
            );
            enc.setFragmentTexture_atIndex(Some(&*draw.texture), 0);
            enc.drawPrimitives_vertexStart_vertexCount_instanceCount(MTLPrimitiveType::TriangleStrip, 0, 4, 1);
        }
        Ok(())
    }

    /// 1 フレーム分のインスタンス（背景 → 文字 → 装飾 → カーソル）を組み立てる
    fn build(&mut self, frame: &Frame, view: &FrameView, focused: bool) {
        self.insts.clear();
        if frame.cols == 0 || frame.rows == 0 || view.cols == 0 || view.rows == 0 {
            return;
        }
        let (cw, ch) = (self.fonts.cell_w as f32, self.fonts.cell_h as f32);
        let pad = self.padding() as f32;
        let cols = (frame.cols as usize).min(view.cols as usize);
        let cursor_color = frame.cursor_color.unwrap_or(self.theme.cursor);
        let cursor = layout::cursor_cell(frame, view, cols).filter(|_| view.show_cursor);
        let block_cursor = focused && frame.cursor.shape == CursorShape::Block;
        let cur = RowCursor { cursor, block: block_cursor, color: cursor_color };

        let mut fgs = vec![Rgb::default(); cols];
        let mut bgs: Vec<Option<Rgb>> = vec![None; cols];
        self.image_draws.clear();
        let cell = (cw, ch);

        // 0. 背景色より下の画像（T-127）
        self.push_images(frame, view, ImageLayer::BelowBackground, cell, pad, cols);

        // 1. 背景
        for y in 0..view.rows {
            let Some(fy) = layout::frame_row(frame, view, y) else { continue };
            let row = &frame.row(fy)[..cols];
            layout::row_colors(&self.theme, frame, row, fy, y, &cur, &mut fgs, &mut bgs);
            let top = pad + y as f32 * ch;
            let mut x = 0;
            while x < cols {
                let Some(c) = bgs[x] else {
                    x += 1;
                    continue;
                };
                // 同じ色が続く間はまとめて 1 枚にする
                let start = x;
                while x < cols && bgs[x] == Some(c) {
                    x += 1;
                }
                let left = pad + start as f32 * cw;
                self.insts.push(Inst::solid(left, top, (x - start) as f32 * cw, ch, c));
            }
        }

        // 2. 文字より下の画像
        self.push_images(frame, view, ImageLayer::BelowText, cell, pad, cols);

        // 3. 文字
        for y in 0..view.rows {
            let Some(fy) = layout::frame_row(frame, view, y) else { continue };
            let row = &frame.row(fy)[..cols];
            layout::row_colors(&self.theme, frame, row, fy, y, &cur, &mut fgs, &mut bgs);
            let top = pad + y as f32 * ch;
            let base_y = top + self.fonts.baseline;
            for x in 0..cols {
                let c = &row[x];
                if matches!(c.wide, Wide::SpacerTail | Wide::SpacerHead)
                    || c.is_blank()
                    || c.style.invisible()
                    || (view.blink_off && c.style.blink())
                    || c.ch == PLACEHOLDER
                {
                    continue;
                }
                self.push_glyphs(row, x, cols, pad + x as f32 * cw, base_y, top, fgs[x]);
            }
        }

        // 4. 文字より上の画像
        self.push_images(frame, view, ImageLayer::AboveText, cell, pad, cols);

        // 5. 下線・取り消し線・上線
        for y in 0..view.rows {
            let Some(fy) = layout::frame_row(frame, view, y) else { continue };
            let row = &frame.row(fy)[..cols];
            let hover = view.hover.filter(|h| h.0 == y).map(|h| (h.1 as usize, (h.2 as usize).min(cols)));
            if hover.is_none()
                && !row.iter().any(|c| c.underline() != Underline::None || c.style.strikethrough() || c.style.overline())
            {
                continue;
            }
            layout::row_colors(&self.theme, frame, row, fy, y, &cur, &mut fgs, &mut bgs);
            let top = pad + y as f32 * ch;
            let base_y = top + self.fonts.baseline;
            for x in 0..cols {
                let c = &row[x];
                let hovered = hover.is_some_and(|(s, e)| (s..e).contains(&x));
                let ul = if hovered && c.underline() == Underline::None { Underline::Single } else { c.underline() };
                if (ul == Underline::None && !c.style.strikethrough() && !c.style.overline()) || c.ch == PLACEHOLDER {
                    continue;
                }
                let span = if c.wide == Wide::Wide { 2.0 } else { 1.0 };
                let (l, w) = (pad + x as f32 * cw, span * cw);
                let fg = fgs[x];
                let ucolor = c.style.underline_color(&frame.palette).unwrap_or(fg);
                let t = self.fonts.underline_thick;
                let uy = (base_y + self.fonts.underline_pos).min(top + ch - t);
                match ul {
                    Underline::None => {}
                    Underline::Double => {
                        let uy2 = (uy + t * 2.0).min(top + ch - t);
                        let uy1 = uy2 - t * 2.0;
                        self.insts.push(Inst::solid(l, uy1, w, t, ucolor));
                        self.insts.push(Inst::solid(l, uy2, w, t, ucolor));
                    }
                    Underline::Curly => {
                        // 波線は、短い段差の並びで近づける（Direct2D 版は線分で描いている）
                        let amp = (t * 1.5).max(1.5);
                        let step = (cw / 4.0).max(2.0);
                        let mut px = l;
                        let mut up = true;
                        while px < l + w {
                            let seg = step.min(l + w - px);
                            let yy = if up { uy - amp } else { uy + amp };
                            self.insts.push(Inst::solid(px, yy, seg, t, ucolor));
                            px += seg;
                            up = !up;
                        }
                    }
                    _ => self.insts.push(Inst::solid(l, uy, w, t, ucolor)),
                }
                if c.style.strikethrough() {
                    let st = self.fonts.strike_thick;
                    let sy = base_y - self.fonts.strike_pos - st / 2.0;
                    self.insts.push(Inst::solid(l, sy, w, st, fg));
                }
                if c.style.overline() {
                    self.insts.push(Inst::solid(l, top, w, t, fg));
                }
            }
        }

        // 6. IME の変換中の文字列（あればカーソルより先に描く。カーソルは文字列の終わりに出る）。
        // 点滅で消えている間も、アプリがカーソルを隠していても（claude のような TUI）出す（T-133）
        if self.preedit.is_some() {
            let at = layout::cursor_cell_any(frame, view, cols).filter(|_| view.cursor_row);
            self.push_preedit(frame, view, cols, at);
            return;
        }

        // 7. カーソル（塗りつぶしは背景として塗り済み）
        if let Some((cx, cy, span)) = cursor
            && (cx as usize) < cols
        {
            let l = pad + cx as f32 * cw;
            let t = pad + cy as f32 * ch;
            let w = span as f32 * cw;
            let thick = (self.scale * 2.0).round().max(1.0);
            match frame.cursor.shape {
                CursorShape::Bar => self.insts.push(Inst::solid(l, t, thick, ch, cursor_color)),
                CursorShape::Underline => self.insts.push(Inst::solid(l, t + ch - thick, w, thick, cursor_color)),
                CursorShape::Block if focused => {}
                CursorShape::Block | CursorShape::BlockHollow => {
                    // 枠だけ（4 本の線）
                    self.insts.push(Inst::solid(l, t, w, thick, cursor_color));
                    self.insts.push(Inst::solid(l, t + ch - thick, w, thick, cursor_color));
                    self.insts.push(Inst::solid(l, t, thick, ch, cursor_color));
                    self.insts.push(Inst::solid(l + w - thick, t, thick, ch, cursor_color));
                }
            }
        }
    }

    /// `layer` の画像を積む（`insts` の今の長さの後ろに描く）
    fn push_images(&mut self, frame: &Frame, view: &FrameView, layer: ImageLayer, cell: (f32, f32), pad: f32, cols: usize) {
        if frame.images.is_empty() {
            return;
        }
        let at = self.insts.len();
        let device = self.gpu.device.clone();
        let draws = self.images.draws(&device, frame, view, layer, cell, pad, cols);
        for d in draws {
            self.image_draws.push((at, d));
        }
    }

    /// IME の変換中の文字列をカーソルの位置から描く（T-127）。
    /// 端末の中身の上に、選択と同じ背景 + 下線で重ねる。はみ出す分は行の終わりで切る
    fn push_preedit(&mut self, frame: &Frame, view: &FrameView, cols: usize, cursor: Option<(u16, u16, usize)>) {
        let Some(text) = self.preedit.clone() else { return };
        let Some((cx, cy, _)) = cursor else { return };
        let (cw, ch) = (self.fonts.cell_w as f32, self.fonts.cell_h as f32);
        let pad = self.padding() as f32;
        let top = pad + cy as f32 * ch;
        let base_y = top + self.fonts.baseline;
        let fg = frame.foreground;
        let bg = self.theme.selection_bg;
        let mut x = cx as usize;
        for ch_c in text.chars() {
            if x >= cols {
                break;
            }
            let Some((face, glyph)) = self.fonts.glyph(ch_c, 0) else { continue };
            let m = self.fonts.cache_metrics(face, glyph);
            // 全角は 2 セル（送り幅で見分ける）
            let span = if m.advance > cw * 1.5 { 2 } else { 1 };
            let left = pad + x as f32 * cw;
            let w = span as f32 * cw;
            self.insts.push(Inst::solid(left, top, w, ch, bg));
            let gx = left + (w - m.advance.min(w)) / 2.0;
            self.push_glyph(face, glyph, 256, gx, base_y, fg);
            // 下線（変換中であることを示す）
            let t = self.fonts.underline_thick;
            let uy = (base_y + self.fonts.underline_pos).min(top + ch - t);
            self.insts.push(Inst::solid(left, uy, w, t, fg));
            x += span;
        }
        // 変換中はカーソルを文字列の終わりに出す
        let l = pad + (x.min(cols.saturating_sub(1))) as f32 * cw;
        let thick = (self.scale * 2.0).round().max(1.0);
        self.insts.push(Inst::solid(l, top, thick, ch, frame.cursor_color.unwrap_or(self.theme.cursor)));
        let _ = view;
    }

    /// セル 1 つ分のグリフ（基底 + 結合文字）を積む
    fn push_glyphs(&mut self, row: &[Cell], x: usize, cols: usize, left: f32, base_y: f32, top: f32, fg: Rgb) {
        let c = &row[x];
        let si = style_index(c.bold(), c.italic());
        let (cw, ch) = (self.fonts.cell_w as f32, self.fonts.cell_h as f32);
        let asc = self.fonts.baseline;
        let desc = ch - self.fonts.baseline;
        let mut span = if c.wide == Wide::Wide { 2 } else { 1 };

        let Some((face, glyph)) = self.fonts.glyph(c.ch, si) else { return };
        let m = self.fonts.cache_metrics(face, glyph);
        // 幅 1 のセルに広いグリフ。Overlap なら縮めず、セルの左端から右隣へはみ出させる（T-013）
        let overlap = span == 1 && m.advance > cw + 0.5 && self.theme.narrow_wide == NarrowWide::Overlap;
        // 孤立した広いグリフは右隣の空白を借りて 2 セル分の枠にする
        if span == 1
            && !overlap
            && self.theme.borrow_right
            && m.advance > cw + 0.5
            && x + 1 < cols
            && row[x + 1].is_blank()
            && row[x + 1].wide == Wide::Narrow
            && (x == 0 || row[x - 1].is_blank())
        {
            span = 2;
        }
        let slot = span as f32 * cw;
        let scale = if overlap || m.advance <= slot + 0.5 {
            1.0
        } else {
            layout::fit_scale(m.advance, m.ink_ascent, m.ink_descent, slot, asc, desc)
        };
        let gx = if overlap { left } else { left + (slot - m.advance * scale) / 2.0 };
        let scale_q = (scale * 256.0).round().clamp(1.0, 65535.0) as u16;

        self.push_glyph(face, glyph, scale_q, gx, base_y, fg);
        // 結合文字は同じ位置に重ねて描く（合字は作らない。T-033）
        for &extra in &c.extra.clone() {
            if let Some((f2, g2)) = self.fonts.glyph(extra, si) {
                self.push_glyph(f2, g2, scale_q, gx, base_y, fg);
            }
        }
        let _ = top;
    }

    fn push_glyph(&mut self, face: usize, glyph: u16, scale_q: u16, pen_x: f32, base_y: f32, fg: Rgb) {
        let key = GlyphKey { face, glyph, scale_q };
        let Some(e) = self.atlas.get(&mut self.fonts, key) else { return };
        self.insts.push(Inst {
            rect: [pen_x + e.left, base_y + e.top, e.width, e.height],
            color: rgba(fg),
            uv: e.uv,
            mode: if e.color { 2 } else { 1 },
            _pad: [0; 3],
        });
    }
}

/// Metal の clear 色は線形。sRGB の 8 bit をそのまま 0..1 にする（BGRA8Unorm は非線形のまま扱う）
fn srgb(v: u8) -> f64 {
    v as f64 / 255.0
}

/// オフスクリーンのテクスチャを作る（[`Gpu`] から使う）
pub(crate) fn make_offscreen(
    device: &ProtocolObject<dyn objc2_metal::MTLDevice>,
    width: u32,
    height: u32,
) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, String> {
    let td = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::BGRA8Unorm,
            width as usize,
            height as usize,
            false,
        )
    };
    td.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
    td.setStorageMode(MTLStorageMode::Shared);
    device.newTextureWithDescriptor(&td).ok_or_else(|| "オフスクリーンのテクスチャを作れません".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use et_vt::{Cursor, Rgb, Style};

    /// 文字と色を置いた 1 枚のフレームを作る
    fn frame_with(text: &str, fg: Rgb, bg: Rgb) -> Frame {
        let cols = text.chars().count() as u16 + 2;
        let mut frame = Frame::default();
        frame.cols = cols;
        frame.rows = 2;
        frame.foreground = Rgb { r: 0xd0, g: 0xd0, b: 0xd0 };
        frame.background = Rgb { r: 0x10, g: 0x10, b: 0x10 };
        frame.cells = vec![Cell::default(); cols as usize * 2];
        frame.selection = vec![None; 2];
        frame.cursor = Cursor { pos: None, wide_tail: false, visible: false, blinking: false, shape: CursorShape::Block };
        for (i, ch) in text.chars().enumerate() {
            frame.cells[i] = Cell { ch, extra: Vec::new(), fg: Some(fg), bg: Some(bg), style: Style::default(), wide: Wide::Narrow };
        }
        frame
    }

    /// (x, y) の画素（BGRA）
    fn px(data: &[u8], w: u32, x: u32, y: u32) -> (u8, u8, u8) {
        let i = (y * w + x) as usize * 4;
        (data[i + 2], data[i + 1], data[i]) // R, G, B
    }

    fn renderer_for(frame: &Frame) -> Renderer {
        let probe = Renderer::offscreen(16, 16, 1.0, FontSpec::default()).expect("Metal のデバイス");
        let (w, h) = probe.client_size_for(frame.cols, frame.rows);
        drop(probe);
        Renderer::offscreen(w, h, 1.0, FontSpec::default()).expect("Metal のデバイス")
    }

    #[test]
    fn cell_metrics_are_sane() {
        let r = Renderer::offscreen(64, 64, 1.0, FontSpec::default()).expect("Metal のデバイス");
        let (cw, ch) = r.cell_size();
        assert!(cw >= 4 && cw < 60, "セルの幅 {cw}");
        assert!(ch >= 8 && ch < 80, "セルの高さ {ch}");
        // 桁数・行数との往復
        let (w, h) = r.client_size_for(20, 5);
        assert_eq!(r.grid_size(w, h), (20, 5));
    }

    #[test]
    fn draws_背景と文字() {
        let fg = Rgb { r: 0xff, g: 0x00, b: 0x00 };
        let bg = Rgb { r: 0x00, g: 0x00, b: 0xff };
        let frame = frame_with("MMMM", fg, bg);
        let mut r = renderer_for(&frame);
        let (w, _h, data) = r.read_pixels(&frame, &FrameView::whole(&frame), true).expect("描画");
        let (cw, ch) = r.cell_size();
        let pad = r.padding();

        // 文字を置いたセルの背景は青
        let (bx, by) = (pad + cw / 2, pad + ch - 2);
        let p = px(&data, w, bx, by);
        assert_eq!(p, (0x00, 0x00, 0xff), "セルの背景が青ではない: {p:?}");

        // 何も置いていない右端のセルはフレームの背景色
        let p = px(&data, w, pad + cw * 5 - 2, pad + ch / 2);
        assert_eq!(p, (0x10, 0x10, 0x10), "空きのセルが既定の背景ではない: {p:?}");

        // 文字（赤）の画素がどこかにある
        let red = data
            .chunks(4)
            .filter(|p| p[2] > 0x80 && p[1] < 0x60 && p[0] < 0x60)
            .count();
        assert!(red > 20, "文字が描かれていない（赤い画素 {red}）");
    }

    #[test]
    fn draws_カーソル() {
        let frame_no = frame_with("ab", Rgb { r: 0xd0, g: 0xd0, b: 0xd0 }, Rgb { r: 0x10, g: 0x10, b: 0x10 });
        let mut frame = frame_no.clone();
        // 文字の無いセル（3 桁目）にカーソルを置く（文字の画素と混ざらないように）
        frame.cursor = Cursor { pos: Some((2, 0)), wide_tail: false, visible: true, blinking: false, shape: CursorShape::Block };
        let mut r = renderer_for(&frame);
        let theme = r.theme().clone();
        let (w, _h, data) = r.read_pixels(&frame, &FrameView::whole(&frame), true).expect("描画");
        let (cw, ch) = r.cell_size();
        let pad = r.padding();
        // 3 つ目のセルの真ん中がカーソルの色で塗られている
        let p = px(&data, w, pad + cw * 2 + cw / 2, pad + ch / 2);
        assert_eq!(p, (theme.cursor.r, theme.cursor.g, theme.cursor.b), "カーソルが塗られていない: {p:?}");
    }

    #[test]
    fn 変換中の文字列はカーソルが隠れていても描く() {
        // アプリがカーソルを隠している（DECTCEM off。claude のような TUI）状態
        let mut frame = frame_with("abcdef", Rgb { r: 0xd0, g: 0xd0, b: 0xd0 }, Rgb { r: 0x10, g: 0x10, b: 0x10 });
        frame.cursor = Cursor { pos: Some((6, 0)), wide_tail: false, visible: false, blinking: false, shape: CursorShape::Block };
        let mut r = renderer_for(&frame);
        let theme = r.theme().clone();
        let sel = (theme.selection_bg.r, theme.selection_bg.g, theme.selection_bg.b);
        let (cw, ch) = r.cell_size();
        let pad = r.padding();
        // セル (cx, 0) の中にある「選択と同じ背景」の画素の数（枠の 2 画素は数えない）
        let count = |data: &[u8], w: u32, cx: u32| {
            let mut n = 0;
            for y in pad + 2..pad + ch - 2 {
                for x in pad + cw * cx + 2..pad + cw * (cx + 1) - 2 {
                    if px(data, w, x, y) == sel {
                        n += 1;
                    }
                }
            }
            n
        };
        // 変換中の文字列を入れる前は、カーソルの位置に何も出ていない
        let (w, _h, data) = r.read_pixels(&frame, &FrameView::whole(&frame), true).expect("描画");
        assert_eq!(count(&data, w, 6), 0, "カーソルを隠しているのに何か描いている");
        // 入れると、カーソルの桁から選択と同じ背景で出る（「あ」は全角なので 6・7 桁目）
        r.set_preedit(Some("あ".into()));
        assert_eq!(r.preedit(), Some("あ"));
        let (w, _h, data) = r.read_pixels(&frame, &FrameView::whole(&frame), true).expect("描画");
        assert!(count(&data, w, 6) > 0, "変換中の文字列の背景が出ていない");
        assert!(count(&data, w, 7) > 0, "全角 1 文字の 2 セル目が出ていない");
        assert_eq!(count(&data, w, 5), 0, "カーソルより前の桁に描いている");
    }

    #[test]
    fn 幅の広いグリフを縮める() {
        // `★` は幅 1 と数えるのにグリフは全角（T-013）。shrink ではセルからはみ出さない
        let mut frame = frame_with("★ ", Rgb { r: 0xff, g: 0xff, b: 0xff }, Rgb { r: 0x00, g: 0x00, b: 0x00 });
        frame.cells[1] = Cell { ch: ' ', bg: Some(Rgb { r: 0x00, g: 0xff, b: 0x00 }), ..Cell::default() };
        let mut r = renderer_for(&frame);
        let mut theme = Theme::default();
        theme.narrow_wide = NarrowWide::Shrink;
        theme.borrow_right = false;
        r.set_theme(theme);
        let (w, _h, data) = r.read_pixels(&frame, &FrameView::whole(&frame), true).expect("描画");
        let (cw, ch) = r.cell_size();
        let pad = r.padding();
        // 2 つ目のセル（緑の背景）に白い画素がはみ出していない
        let mut white = 0;
        for y in pad..pad + ch {
            for x in pad + cw..pad + cw * 2 {
                let p = px(&data, w, x, y);
                if p.0 > 0xc0 && p.1 > 0xc0 && p.2 > 0xc0 {
                    white += 1;
                }
            }
        }
        assert_eq!(white, 0, "縮めたのに右隣へはみ出している（白い画素 {white}）");
    }
}
