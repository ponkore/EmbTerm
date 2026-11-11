//! et-render: HWND に端末を描く（DXGI のフリップモデルのスワップチェーン + Direct2D / DirectWrite。T-008）。
//!
//! - [`Frame`]: 描く中身。libghostty-vt の render state から変わった行だけを写す（[`Frame::update`]）
//! - [`Renderer`]: HWND 1 枚に描く。フォントからセルの大きさを決め、背景・文字・装飾・カーソル・選択範囲を描く
//!
//! 描き方の要点
//! - 座標は物理ピクセル。セルは整数の画素に置く。DPI はフォントの大きさのほうに掛ける
//! - 背景は、同じ色が続くセルをまとめて塗る
//! - 文字は 1 セルずつ決まった位置に置く（フォントの送り幅には頼らない）。主フォントが持つ文字は
//!   同じ色・同じフェイスが続く範囲を 1 回の `DrawGlyphRun` にまとめる。持たない文字は DirectWrite の
//!   レイアウトに任せる（システムのフォールバック、色付きの絵文字）
//! - **幅 2 のセルは 2 セル分**の枠に、**幅 1 と数えたのにグリフが広い文字（`①` `★` `※`）は 1 セルの枠に
//!   収まるよう縮める**（T-013。幅・上・下の比の最小）。前後が空白で孤立しているときは右隣の空白を借りて
//!   2 セル分の枠にする（ghostel と同じ規則）
//! - 描くのは呼ばれたときだけ。呼ぶ側は render state が dirty のときだけ呼ぶ
//!
//! unsafe（COM の呼び出し）はこのクレートの中に閉じ込める。

mod font;
mod frame;
mod gpu;
mod png;
mod surface;

use std::collections::HashMap;
use std::path::Path;

use et_vt::{CursorShape, Rgb, StyleExt, Underline, Wide};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::Result;
use windows_numerics::{Matrix3x2, Vector2};

pub use font::FontSpec;
pub use frame::{Cell, Frame};
pub use gpu::Gpu;

use font::{FontSet, fit_scale, style_index};
use surface::Surface;

/// 色の設定（端末の既定の前景・背景は libghostty-vt 側で持つ）
#[derive(Clone, Debug)]
pub struct Theme {
    /// 選択範囲の背景
    pub selection_bg: Rgb,
    /// 選択範囲の文字の色。`None` ならセルの色のまま
    pub selection_fg: Option<Rgb>,
    /// カーソルの色（アプリが OSC 12 で決めていなければこれ）
    pub cursor: Rgb,
    /// ブロックのカーソルの上の文字の色。`None` なら既定の背景色
    pub cursor_text: Option<Rgb>,
    /// 端末の周りの余白（px、DPI 96 のとき。DPI に合わせて拡大する）
    pub padding: u32,
    /// 孤立した広いグリフに右隣の空白を貸すか（T-013 の ghostel の規則）
    pub borrow_right: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            selection_bg: Rgb { r: 0x26, g: 0x4f, b: 0x78 },
            selection_fg: None,
            cursor: Rgb { r: 0xff, g: 0xcc, b: 0x00 },
            cursor_text: None,
            padding: 4,
            borrow_right: true,
        }
    }
}

fn color_f(c: Rgb) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: 1.0 }
}

fn blend(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Rgb { r: m(a.r, b.r), g: m(a.g, b.g), b: m(a.b, b.b) }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// `DrawGlyphRun` にまとめる途中のグリフの並び
struct Run {
    face: usize,
    em: f32,
    color: Rgb,
    origin_x: f32,
    indices: Vec<u16>,
    xs: Vec<f32>,
}

/// HWND 1 枚に端末を描くもの
pub struct Renderer {
    hwnd: HWND,
    gpu: Gpu,
    surface: Surface,
    font: FontSet,
    spec: FontSpec,
    dpi: u32,
    brushes: HashMap<Rgb, ID2D1SolidColorBrush>,
    pub theme: Theme,
}

impl Renderer {
    /// `width` / `height` はクライアント領域の物理ピクセル、`dpi` はウィンドウの DPI
    pub fn new(hwnd: HWND, width: u32, height: u32, dpi: u32, spec: FontSpec) -> Result<Self> {
        let gpu = Gpu::create()?;
        let surface = Surface::new(&gpu, hwnd, width, height)?;
        let font = FontSet::new(&gpu.dwrite, &spec, dpi)?;
        Ok(Self { hwnd, gpu, surface, font, spec, dpi, brushes: HashMap::new(), theme: Theme::default() })
    }

    /// 使っているフォントのファミリ名
    pub fn font_family(&self) -> &str {
        &self.font.family
    }

    /// GPU が WARP（ソフトウェア）か
    pub fn is_warp(&self) -> bool {
        self.gpu.warp
    }

    /// セルの大きさ（物理ピクセル）
    pub fn cell_size(&self) -> (u32, u32) {
        (self.font.cell_w, self.font.cell_h)
    }

    /// 余白（物理ピクセル）
    pub fn padding(&self) -> u32 {
        (self.theme.padding * self.dpi).div_ceil(96)
    }

    /// クライアント領域（物理ピクセル）に入る桁数と行数
    pub fn grid_size(&self, width: u32, height: u32) -> (u16, u16) {
        let p = self.padding() * 2;
        let cols = width.saturating_sub(p) / self.font.cell_w;
        let rows = height.saturating_sub(p) / self.font.cell_h;
        (cols.clamp(1, u16::MAX as u32) as u16, rows.clamp(1, u16::MAX as u32) as u16)
    }

    /// 桁数と行数が入るクライアント領域の大きさ（物理ピクセル）
    pub fn client_size_for(&self, cols: u16, rows: u16) -> (u32, u32) {
        let p = self.padding() * 2;
        (cols as u32 * self.font.cell_w + p, rows as u32 * self.font.cell_h + p)
    }

    /// クライアント座標（物理ピクセル）がどのセルか。外なら端に寄せる
    pub fn cell_at(&self, x: i32, y: i32, cols: u16, rows: u16) -> (u16, u16) {
        let p = self.padding() as i32;
        let cx = ((x - p).max(0) / self.font.cell_w as i32).min(cols.saturating_sub(1) as i32);
        let cy = ((y - p).max(0) / self.font.cell_h as i32).min(rows.saturating_sub(1) as i32);
        (cx as u16, cy as u16)
    }

    /// DPI が変わった。フォントを作り直す（セルの大きさが変わる）
    pub fn set_dpi(&mut self, dpi: u32) -> Result<()> {
        if dpi != self.dpi {
            self.font = FontSet::new(&self.gpu.dwrite, &self.spec, dpi)?;
            self.dpi = dpi;
        }
        Ok(())
    }

    /// クライアント領域の大きさが変わった（物理ピクセル）
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.surface.resize(width, height)
    }

    fn brush(&mut self, c: Rgb) -> Result<ID2D1SolidColorBrush> {
        if let Some(b) = self.brushes.get(&c) {
            return Ok(b.clone());
        }
        let b = unsafe { self.surface.dc.CreateSolidColorBrush(&color_f(c), None)? };
        if self.brushes.len() > 1024 {
            self.brushes.clear();
        }
        self.brushes.insert(c, b.clone());
        Ok(b)
    }

    /// フレームを描いて画面に出す。`capture` を渡すと、出す前の画面を PNG に保存する（デバッグ用）。
    /// デバイスを失ったら作り直して、次の呼び出しで描けるようにする
    pub fn draw(&mut self, frame: &Frame, focused: bool, capture: Option<&Path>) -> Result<()> {
        // WM_SIZE の取りこぼしに備え、描く前にスワップチェーンの大きさを確かめるのは呼ぶ側（GenPreview §1.3 #1）
        self.surface.begin();
        let r = self.draw_frame(frame, focused);
        let end = self.surface.end();
        if let Err(e) = r.and(end) {
            self.recreate_device();
            return Err(e);
        }
        if let Some(path) = capture {
            let (w, h) = self.surface.size();
            let (data, pitch) = self.surface.read_back()?;
            png::write_bgra(path, w, h, &data, pitch).map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string()))?;
        }
        if let Err(e) = self.surface.present() {
            self.recreate_device();
            return Err(e);
        }
        Ok(())
    }

    fn recreate_device(&mut self) {
        let size = self.surface.size();
        if let Ok(gpu) = Gpu::create()
            && let Ok(surface) = Surface::new(&gpu, self.hwnd, size.0, size.1)
        {
            self.gpu = gpu;
            self.surface = surface;
            self.brushes.clear();
        }
    }

    fn draw_frame(&mut self, frame: &Frame, focused: bool) -> Result<()> {
        let dc = self.surface.dc.clone();
        unsafe { dc.Clear(Some(&color_f(frame.background))) };
        if frame.cols == 0 || frame.rows == 0 {
            return Ok(());
        }
        let (cw, ch) = (self.font.cell_w as f32, self.font.cell_h as f32);
        let pad = self.padding() as f32;
        let cols = frame.cols as usize;

        // カーソルの位置と幅
        let cursor_color = frame.cursor_color.unwrap_or(self.theme.cursor);
        let cursor = frame.cursor.pos.filter(|_| frame.cursor.visible).map(|(x, y)| {
            let x = if frame.cursor.wide_tail { x.saturating_sub(1) } else { x };
            let span = if x < frame.cols && y < frame.rows && frame.cell(x, y).wide == Wide::Wide { 2 } else { 1 };
            (x, y, span)
        });
        let block_cursor = focused && frame.cursor.shape == CursorShape::Block;

        let mut fgs = vec![Rgb::default(); cols];
        let mut bgs: Vec<Option<Rgb>> = vec![None; cols];
        let mut text = String::new();

        for y in 0..frame.rows {
            let row = frame.row(y);
            let top = pad + y as f32 * ch;
            let base_y = top + self.font.baseline;
            let sel = frame.selection.get(y as usize).copied().flatten();

            // 1. セルごとの実際の前景・背景
            for (x, c) in row.iter().enumerate() {
                let mut fg = c.fg.unwrap_or(frame.foreground);
                let mut bg = c.bg;
                if c.style.inverse() {
                    let b = bg.unwrap_or(frame.background);
                    bg = Some(fg);
                    fg = b;
                }
                if let Some((s, e)) = sel
                    && (s as usize..=e as usize).contains(&x)
                {
                    bg = Some(self.theme.selection_bg);
                    if let Some(f) = self.theme.selection_fg {
                        fg = f;
                    }
                }
                if block_cursor
                    && let Some((cx, cy, span)) = cursor
                    && cy == y
                    && (cx as usize..cx as usize + span).contains(&x)
                {
                    bg = Some(cursor_color);
                    fg = self.theme.cursor_text.unwrap_or(frame.background);
                }
                if c.style.faint() {
                    fg = blend(fg, bg.unwrap_or(frame.background), 0.5);
                }
                fgs[x] = fg;
                bgs[x] = bg;
            }

            // 2. 背景（同じ色が続くところをまとめて塗る）
            let mut x = 0;
            while x < cols {
                let Some(bg) = bgs[x] else {
                    x += 1;
                    continue;
                };
                let start = x;
                while x < cols && bgs[x] == Some(bg) {
                    x += 1;
                }
                let brush = self.brush(bg)?;
                let r = rect(pad + start as f32 * cw, top, pad + x as f32 * cw, top + ch);
                unsafe { dc.FillRectangle(&r, &brush) };
            }

            // 3. 文字
            let mut run: Option<Run> = None;
            for x in 0..cols {
                let c = &row[x];
                if matches!(c.wide, Wide::SpacerTail | Wide::SpacerHead) || c.is_blank() || c.style.invisible() {
                    continue;
                }
                let si = style_index(c.bold(), c.italic());
                let mut span = if c.wide == Wide::Wide { 2 } else { 1 };
                let left = pad + x as f32 * cw;
                let asc = self.font.baseline;
                let desc = ch - self.font.baseline;
                let fg = fgs[x];

                let glyph = if c.extra.is_empty() { self.font.glyph(c.ch, si) } else { None };
                let (advance, ink_a, ink_d) = match glyph {
                    Some(g) => (g.advance, g.ink_ascent, g.ink_descent),
                    None => {
                        let mut s = String::new();
                        s.push(c.ch);
                        s.extend(&c.extra);
                        text = s;
                        match self.font.layout(&self.gpu.dwrite, &text, si) {
                            Some(l) => (l.advance, l.ink_ascent, l.ink_descent),
                            None => continue,
                        }
                    }
                };
                // 孤立した広いグリフは右隣の空白を借りて 2 セル分の枠にする
                if span == 1
                    && self.theme.borrow_right
                    && advance > cw + 0.5
                    && x + 1 < cols
                    && row[x + 1].is_blank()
                    && row[x + 1].wide == Wide::Narrow
                    && (x == 0 || row[x - 1].is_blank())
                {
                    span = 2;
                }
                let slot = span as f32 * cw;
                // 主フォントのグリフで幅が枠に収まるなら縮めない。罫線（┌─┐）や Powerline の記号は
                // 上下いっぱいに描かれていて、縮めると隣とつながらなくなる（上下のはみ出しは許す）
                let scale = if glyph.is_some() && advance <= slot + 0.5 {
                    1.0
                } else {
                    fit_scale(advance, ink_a, ink_d, slot, asc, desc)
                };
                let gx = left + (slot - advance * scale) / 2.0;

                match glyph {
                    Some(g) => {
                        let em = self.font.em_px * scale;
                        let same = run.as_ref().is_some_and(|r| r.face == si && r.em == em && r.color == fg);
                        if !same {
                            if let Some(r) = run.take() {
                                self.flush_run(&dc, r, base_y)?;
                            }
                            run = Some(Run { face: si, em, color: fg, origin_x: gx, indices: Vec::new(), xs: Vec::new() });
                        }
                        let r = run.as_mut().expect("直前に作った");
                        r.indices.push(g.index);
                        r.xs.push(gx);
                    }
                    None => {
                        let brush = self.brush(fg)?;
                        let Some(l) = self.font.layout(&self.gpu.dwrite, &text, si) else { continue };
                        let opts = D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT;
                        unsafe {
                            if scale < 1.0 {
                                dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: gx, M32: base_y });
                                dc.DrawTextLayout(Vector2 { X: 0.0, Y: -l.baseline }, &l.layout, &brush, opts);
                                dc.SetTransform(&Matrix3x2::identity());
                            } else {
                                dc.DrawTextLayout(Vector2 { X: gx, Y: base_y - l.baseline }, &l.layout, &brush, opts);
                            }
                        }
                    }
                }
            }
            if let Some(r) = run.take() {
                self.flush_run(&dc, r, base_y)?;
            }

            // 4. 下線・取り消し線・上線
            for x in 0..cols {
                let c = &row[x];
                let ul = c.underline();
                if ul == Underline::None && !c.style.strikethrough() && !c.style.overline() {
                    continue;
                }
                let span = if c.wide == Wide::Wide { 2.0 } else { 1.0 };
                let (l, r) = (pad + x as f32 * cw, pad + (x as f32 + span) * cw);
                let fg = fgs[x];
                let ucolor = c.style.underline_color(&frame.palette).unwrap_or(fg);
                let t = self.font.underline_thick;
                let uy = (base_y + self.font.underline_pos).min(top + ch - t);
                match ul {
                    Underline::None => {}
                    Underline::Double => {
                        let b = self.brush(ucolor)?;
                        let uy2 = (uy + t * 2.0).min(top + ch - t);
                        let uy1 = uy2 - t * 2.0;
                        unsafe {
                            dc.FillRectangle(&rect(l, uy1, r, uy1 + t), &b);
                            dc.FillRectangle(&rect(l, uy2, r, uy2 + t), &b);
                        }
                    }
                    Underline::Curly => {
                        let b = self.brush(ucolor)?;
                        let amp = (t * 1.5).max(1.5);
                        let step = (cw / 2.0).max(2.0);
                        let mut px = l;
                        let mut up = true;
                        while px < r {
                            let nx = (px + step).min(r);
                            let (y0, y1) = if up { (uy + amp, uy - amp) } else { (uy - amp, uy + amp) };
                            unsafe { dc.DrawLine(Vector2 { X: px, Y: y0 }, Vector2 { X: nx, Y: y1 }, &b, t, None) };
                            px = nx;
                            up = !up;
                        }
                    }
                    _ => {
                        let b = self.brush(ucolor)?;
                        unsafe { dc.FillRectangle(&rect(l, uy, r, uy + t), &b) };
                    }
                }
                if c.style.strikethrough() {
                    let b = self.brush(fg)?;
                    let st = self.font.strike_thick;
                    let sy = base_y - self.font.strike_pos - st / 2.0;
                    unsafe { dc.FillRectangle(&rect(l, sy, r, sy + st), &b) };
                }
                if c.style.overline() {
                    let b = self.brush(fg)?;
                    unsafe { dc.FillRectangle(&rect(l, top, r, top + t), &b) };
                }
            }
        }

        // 5. カーソル（ブロックで focus があるときは色を反転して描き済み）
        if let Some((cx, cy, span)) = cursor
            && cy < frame.rows
            && (cx as usize) < cols
        {
            let b = self.brush(cursor_color)?;
            let l = pad + cx as f32 * cw;
            let t = pad + cy as f32 * ch;
            let w = span as f32 * cw;
            let thick = ((self.dpi as f32 / 96.0) * 2.0).round().max(1.0);
            unsafe {
                match frame.cursor.shape {
                    CursorShape::Bar => dc.FillRectangle(&rect(l, t, l + thick, t + ch), &b),
                    CursorShape::Underline => dc.FillRectangle(&rect(l, t + ch - thick, l + w, t + ch), &b),
                    CursorShape::Block if focused => {}
                    CursorShape::Block | CursorShape::BlockHollow => {
                        let h = thick / 2.0;
                        dc.DrawRectangle(&rect(l + h, t + h, l + w - h, t + ch - h), &b, thick.min(2.0), None)
                    }
                }
            }
        }
        Ok(())
    }

    fn flush_run(&mut self, dc: &ID2D1DeviceContext, r: Run, base_y: f32) -> Result<()> {
        if r.indices.is_empty() {
            return Ok(());
        }
        let brush = self.brush(r.color)?;
        let mut advances: Vec<f32> = r.xs.windows(2).map(|w| w[1] - w[0]).collect();
        advances.push(0.0);
        let run = DWRITE_GLYPH_RUN {
            fontFace: std::mem::ManuallyDrop::new(Some(self.font.faces[r.face].clone())),
            fontEmSize: r.em,
            glyphCount: r.indices.len() as u32,
            glyphIndices: r.indices.as_ptr(),
            glyphAdvances: advances.as_ptr(),
            glyphOffsets: std::ptr::null(),
            isSideways: false.into(),
            bidiLevel: 0,
        };
        unsafe {
            dc.DrawGlyphRun(Vector2 { X: r.origin_x, Y: base_y }, &run, None, &brush, DWRITE_MEASURING_MODE_NATURAL);
        }
        // ManuallyDrop の中のフェイスの参照を戻す
        drop(std::mem::ManuallyDrop::into_inner(run.fontFace));
        Ok(())
    }
}
