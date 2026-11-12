//! et-render: HWND に端末を描く（DXGI のフリップモデルのスワップチェーン + Direct2D / DirectWrite。T-008）。
//!
//! - [`Frame`]: 描く中身。libghostty-vt の render state から変わった行だけを写す（[`Frame::update`]）
//! - [`Renderer`]: HWND 1 枚に描く。フォントからセルの大きさを決め、背景・文字・装飾・カーソル・選択範囲を描く
//!
//! 描き方の要点
//! - 座標は物理ピクセル。セルは整数の画素に置く。DPI はフォントの大きさのほうに掛ける
//! - 背景は、セル 1 つを 1 画素にしたビットマップを最近傍で拡大して 1 回で描く（T-054。セルごとに塗ると色の細かい画面で重い）
//! - 文字は 1 セルずつ決まった位置に置く（フォントの送り幅には頼らない）。主フォントが持つ文字は
//!   （フェイス, 大きさ, 色）ごとに行をまたいでまとめ、位置はグリフのオフセットで渡して `DrawGlyphRun` にする
//!   （1 回あたり `MAX_RUN_GLYPHS` まで。T-054）。持たない文字は DirectWrite の
//!   レイアウトに任せる（システムのフォールバック、色付きの絵文字）
//! - 描くたびに画面全体を描く（変わった行だけを描くことはしない。T-077 の測定で決めた）。
//!   スワップチェーンは「次のフレームを受け付けるか」を待てる形（[`Renderer::try_acquire_frame`]。T-068）
//! - GPU デバイスはプロセスで 1 つを分け合う（[`Gpu::shared`]。T-067）
//! - **幅 2 のセルは 2 セル分**の枠に描く。**幅 1 と数えたのにグリフが広い文字（`①` `★` `※`）**は
//!   [`NarrowWide`] で選ぶ（T-013）
//!   - `Overlap`（既定）: 縮めずにセルの左端から描き、右隣へはみ出させる（WezTerm と同じ見え方）
//!   - `Shrink`: 1 セルの枠に収まるよう縮める（幅・上・下の比の最小）。前後が空白で孤立しているときは
//!     右隣の空白を借りて 2 セル分の枠にする（ghostel と同じ規則）
//! - 描くのは呼ばれたときだけ。呼ぶ側は render state が dirty のときだけ呼ぶ
//! - カーソルの点滅（T-035）は [`Renderer::draw_blink`]。スワップチェーンの 2 枚のバッファがどちらも同じ中身に
//!   なってからは、カーソルのセルだけを描き直す（クリップの中で行を描く）。全体を描いたらまた 1 回は全体を描く
//! - Kitty graphics の画像（T-088）は [`images`] モジュール。層（背景色の下・文字の下・文字の上）ごとに背景・文字と重ね、
//!   GPU のビットマップは画像の世代ごとにキャッシュする
//!
//! unsafe（COM の呼び出し）はこのクレートの中に閉じ込める。

mod font;
mod frame;
mod gpu;
mod images;
mod png;
mod surface;
mod view;

use std::collections::HashMap;
use std::path::Path;

use et_vt::{CursorShape, ImageLayer, Rgb, StyleExt, Underline, Wide};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::Result;
use windows_numerics::{Matrix3x2, Vector2};

pub use font::FontSpec;
pub use frame::{Cell, Frame, ImagePixels, PLACEHOLDER, PlacedImage};
pub use view::FrameView;
pub use gpu::{Gpu, current_generation, live_devices};

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
    /// 幅 1 と数えたのにグリフが広い文字（`①` `★` `※`）の描き方（T-013）
    pub narrow_wide: NarrowWide,
    /// 孤立した広いグリフに右隣の空白を貸すか（T-013 の ghostel の規則。`NarrowWide::Shrink` のときだけ効く）
    pub borrow_right: bool,
}

/// 幅 1 と数えたのにグリフが広い文字（East Asian Ambiguous の `①` `★` `※` など）の描き方（T-013）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NarrowWide {
    /// 1 セルの枠に収まるよう縮める（ghostel と同じ）
    Shrink,
    /// 縮めずに元の大きさのまま、セルの左端から描いて右隣へはみ出させる（WezTerm と同じ見え方）。
    /// 桁送りは 1 セルのままなので、続けて並ぶと後の文字が前の文字の右半分に重なる
    #[default]
    Overlap,
}

impl std::str::FromStr for NarrowWide {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "shrink" => Ok(Self::Shrink),
            "overlap" => Ok(Self::Overlap),
            _ => Err(format!("shrink か overlap を指定する（{s}）")),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            selection_bg: Rgb { r: 0x26, g: 0x4f, b: 0x78 },
            selection_fg: None,
            cursor: Rgb { r: 0xff, g: 0xcc, b: 0x00 },
            cursor_text: None,
            padding: 4,
            narrow_wide: NarrowWide::default(),
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

/// 1 回の `DrawGlyphRun` にまとめるグリフの数の上限（T-054 の測定で決めた）。長い並びほど Direct2D の処理が
/// 1 グリフあたりで重くなる（314x70 の画面で、2 万グリフを 1 回 → 170 ms、1024 ずつ → 20 ms、128 ずつ → 5〜7 ms）
const MAX_RUN_GLYPHS: usize = 128;

/// `DrawGlyphRun` にまとめる途中のグリフの並び（同じフェイス・大きさ・色。行をまたいでもよい）
struct Run {
    face: usize,
    em: f32,
    color: Rgb,
    /// 最初のグリフの位置（ベースライン）。ほかのグリフはここからのオフセット
    origin: (f32, f32),
    indices: Vec<u16>,
    offsets: Vec<DWRITE_GLYPH_OFFSET>,
}

/// 行を描くときに一度決めたものの持ち回し
struct RowContext {
    cols: usize,
    cursor: Option<(u16, u16, usize)>,
    block_cursor: bool,
    cursor_color: Rgb,
}

/// [`Renderer::draw_rows`] で何を描くか
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowPass {
    Both,
    Backgrounds,
    Text,
}

/// 描いた回数と、最後に描いたときの時間（自動確認・測定用。T-054 / T-068）
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawStats {
    /// 全体を描いた回数
    pub full: u64,
    /// カーソルのセルだけを描いた回数（点滅）
    pub partial: u64,
    /// 最後の描画の `BeginDraw` から `EndDraw` まで（CPU で命令を組んで GPU に渡すまで）
    pub last_draw: std::time::Duration,
    /// 最後の `Present` にかかった時間（垂直同期を待つとここが延びる）
    pub last_present: std::time::Duration,
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
    /// 画像の GPU のビットマップ（画像の世代が鍵。世代が変わると一覧から外れて捨てられる。デバイスと一緒に捨てる）
    bitmaps: HashMap<u64, ID2D1Bitmap1>,
    /// 背景のビットマップ（セル 1 つが 1 画素。大きさ（桁, 行）ごと。デバイスと一緒に捨てる）
    bg_bitmaps: Vec<((u32, u32), ID2D1Bitmap1)>,
    /// 背景の画素の作業場所（描くたびに確保しない）
    bg_pixels: Vec<u32>,
    pub theme: Theme,
    /// 点滅でカーソルのセルだけを描き直してよいか（2 枚のバッファが、カーソルのセルのほかは同じ中身）
    partial_ready: bool,
    pub stats: DrawStats,
}

impl Renderer {
    /// `width` / `height` はクライアント領域の物理ピクセル、`dpi` はウィンドウの DPI。デバイスはプロセスで分け合う（[`Gpu::shared`]）
    pub fn new(hwnd: HWND, width: u32, height: u32, dpi: u32, spec: FontSpec) -> Result<Self> {
        Self::with_gpu(Gpu::shared()?, hwnd, width, height, dpi, spec)
    }

    /// デバイスを渡して作る
    pub fn with_gpu(gpu: Gpu, hwnd: HWND, width: u32, height: u32, dpi: u32, spec: FontSpec) -> Result<Self> {
        let surface = Surface::new(&gpu, hwnd, width, height)?;
        let font = FontSet::new(&gpu.dwrite, &spec, dpi)?;
        Ok(Self {
            hwnd,
            gpu,
            surface,
            font,
            spec,
            dpi,
            brushes: HashMap::new(),
            bitmaps: HashMap::new(),
            bg_bitmaps: Vec::new(),
            bg_pixels: Vec::new(),
            theme: Theme::default(),
            partial_ready: false,
            stats: DrawStats::default(),
        })
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

    /// フォントを替える（セルの大きさが変わる）。作れなければ今のフォントのまま
    pub fn set_font(&mut self, spec: FontSpec) -> Result<()> {
        self.font = FontSet::new(&self.gpu.dwrite, &spec, self.dpi)?;
        self.spec = spec;
        Ok(())
    }

    /// クライアント領域の大きさが変わった（物理ピクセル）
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if (width.max(1), height.max(1)) != self.surface.size() {
            self.partial_ready = false;
        }
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

    /// 次のフレームを受け付けるか（待たない。T-068）。true なら描いてよい（`Present` はほとんど待たない）。
    /// false なら [`Renderer::frame_waitable`] が知らされるのを待ってから [`Renderer::set_frame_acquired`] して描く
    pub fn try_acquire_frame(&mut self) -> bool {
        self.surface.try_acquire()
    }

    /// 次のフレームを受け付けると知らせるハンドル（待つとセマフォを 1 つ取る。閉じない）
    pub fn frame_waitable(&self) -> windows::Win32::Foundation::HANDLE {
        self.surface.waitable()
    }

    /// 呼ぶ側が [`Renderer::frame_waitable`] を待って知らされた
    pub fn set_frame_acquired(&mut self) {
        self.surface.set_acquired();
    }

    /// ほかの誰かがデバイスを失って作り直していたら、新しいデバイスに載せ替える（T-067）。載せ替えたら true
    pub fn follow_shared_device(&mut self) -> Result<bool> {
        if self.gpu.is_current() {
            return Ok(false);
        }
        self.adopt_gpu(Gpu::shared()?)?;
        Ok(true)
    }

    /// 使っているデバイスの世代
    pub fn gpu_generation(&self) -> u64 {
        self.gpu.generation()
    }

    /// 新しいデバイス（`Gpu`）に載せ替える。失敗したら描けないままになり、次に描くときにまた作り直す
    pub fn adopt_gpu(&mut self, gpu: Gpu) -> Result<()> {
        let size = self.surface.size();
        // 前のスワップチェーンを先に手放す（残っていると同じ HWND に作れない）
        self.surface.release();
        let surface = Surface::new(&gpu, self.hwnd, size.0, size.1)?;
        self.gpu = gpu;
        self.surface = surface;
        self.brushes.clear();
        self.bitmaps.clear();
        self.bg_bitmaps.clear();
        self.partial_ready = false;
        Ok(())
    }

    /// 全部の行を描く（embterm.exe 用。`frame` の大きさに合わせる）
    pub fn draw(&mut self, frame: &Frame, focused: bool, capture: Option<&Path>) -> Result<()> {
        self.draw_view(frame, &FrameView::whole(frame), focused, capture)
    }

    /// `frame` の一部を描く（ペインの大きさに合わせて切り取る / 空ける）。`capture` を渡すと、出す前の画面を PNG に保存する（デバッグ用）。
    /// デバイスを失ったら作り直して、次の呼び出しで描けるようにする
    pub fn draw_view(&mut self, frame: &Frame, view: &FrameView, focused: bool, capture: Option<&Path>) -> Result<()> {
        // WM_SIZE の取りこぼしに備え、描く前にスワップチェーンの大きさを確かめるのは呼ぶ側（GenPreview §1.3 #1）
        self.follow_shared_device()?;
        self.partial_ready = false;
        self.stats.full += 1;
        let t0 = std::time::Instant::now();
        self.surface.begin();
        let r = self.draw_frame(frame, view, focused);
        let end = self.surface.end();
        self.stats.last_draw = t0.elapsed();
        if let Err(e) = r.and(end) {
            self.recreate_device();
            return Err(e);
        }
        if let Some(path) = capture {
            let (w, h) = self.surface.size();
            let (data, pitch) = self.surface.read_back()?;
            png::write_bgra(path, w, h, &data, pitch).map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string()))?;
        }
        let t1 = std::time::Instant::now();
        let presented = self.surface.present();
        self.stats.last_present = t1.elapsed();
        if let Err(e) = presented {
            self.recreate_device();
            return Err(e);
        }
        Ok(())
    }

    /// カーソルの点滅の 1 回分を描く（T-035）。`view.show_cursor` が点いている / 消えている状態。
    /// 2 枚のバッファがそろっていればカーソルのセルだけを描き直し、そうでなければ全体を描く
    /// （フリップモデルの 2 枚のバッファは交互に出るので、全体を描いたあとの 1 回目は全体を描いて 2 枚目もそろえる）
    pub fn draw_blink(&mut self, frame: &Frame, view: &FrameView, focused: bool) -> Result<()> {
        let cols = (frame.cols as usize).min(view.cols as usize);
        let cell = self.cursor_cell(frame, view, cols);
        let Some((cx, cy, span)) = cell.filter(|_| self.partial_ready && frame.images.is_empty() && self.gpu.is_current()) else {
            self.draw_view(frame, view, focused, None)?;
            self.partial_ready = true;
            return Ok(());
        };
        self.stats.partial += 1;
        let dc = self.surface.dc.clone();
        self.surface.begin();
        let (cw, ch) = (self.font.cell_w as f32, self.font.cell_h as f32);
        let pad = self.padding() as f32;
        let (l, t) = (pad + cx as f32 * cw, pad + cy as f32 * ch);
        let clip = rect(l, t, l + span as f32 * cw, t + ch);
        let cursor_color = frame.cursor_color.unwrap_or(self.theme.cursor);
        let shown = view.show_cursor.then_some((cx, cy, span));
        let block_cursor = focused && frame.cursor.shape == CursorShape::Block;
        let ctx = RowContext { cols, cursor: shown, block_cursor, cursor_color };
        unsafe {
            dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            // Clear はクリップの中だけを塗る
            dc.Clear(Some(&color_f(frame.background)));
        }
        let r = self.draw_rows(&dc, frame, view, &ctx, RowPass::Both, cy..cy + 1).and_then(|_| self.draw_cursor(&dc, frame, &ctx, focused));
        unsafe { dc.PopAxisAlignedClip() };
        let end = self.surface.end();
        if let Err(e) = r.and(end).and_then(|_| self.surface.present()) {
            self.recreate_device();
            return Err(e);
        }
        Ok(())
    }

    /// カーソルのセル（ビューの中の桁・行と幅）。`view.show_cursor` は見ない。見えなければ `None`
    fn cursor_cell(&self, frame: &Frame, view: &FrameView, cols: usize) -> Option<(u16, u16, usize)> {
        let (x, y) = frame.cursor.pos.filter(|_| frame.cursor.visible)?;
        let fy = frame.viewport_top as i32 + y as i32;
        let py = fy - view.top;
        if !(0..view.rows as i32).contains(&py) {
            return None;
        }
        let x = if frame.cursor.wide_tail { x.saturating_sub(1) } else { x };
        if x as usize >= cols {
            return None;
        }
        let span = if x < frame.cols && (fy as u16) < frame.rows && frame.cell(x, fy as u16).wide == Wide::Wide { 2 } else { 1 };
        Some((x, py as u16, span))
    }

    /// デバイスを失った。分け合っているデバイスを捨てて作り直し、載せ替える（ほかの端末は描く前に世代の違いで気づく。T-067）
    fn recreate_device(&mut self) {
        self.partial_ready = false;
        self.gpu.invalidate();
        if let Ok(gpu) = Gpu::shared() {
            let _ = self.adopt_gpu(gpu);
        }
    }

    fn draw_frame(&mut self, frame: &Frame, view: &FrameView, focused: bool) -> Result<()> {
        let dc = self.surface.dc.clone();
        unsafe { dc.Clear(Some(&color_f(frame.background))) };
        if frame.cols == 0 || frame.rows == 0 || view.cols == 0 || view.rows == 0 {
            return Ok(());
        }
        let (cw, ch) = (self.font.cell_w as f32, self.font.cell_h as f32);
        let pad = self.padding() as f32;
        // 描く桁数（フレームより狭ければ右を切り取り、広ければ右側は背景のまま）
        let cols = (frame.cols as usize).min(view.cols as usize);

        // カーソルの位置と幅（ビューの中の行にする。見えなければ出さない）
        let cursor_color = frame.cursor_color.unwrap_or(self.theme.cursor);
        let cursor = self.cursor_cell(frame, view, cols).filter(|_| view.show_cursor);
        let block_cursor = focused && frame.cursor.shape == CursorShape::Block;
        let ctx = RowContext { cols, cursor, block_cursor, cursor_color };

        if frame.images.is_empty() {
            // 画像が無いときは行ごとに背景と文字を描く
            self.draw_rows(&dc, frame, view, &ctx, RowPass::Both, 0..view.rows)?;
        } else {
            // 画像の層（T-088）: 背景色より下 → 背景色 → 文字より下 → 文字 → 文字より上（文字と背景は別に一周する）
            self.draw_images(&dc, frame, view, cols, ImageLayer::BelowBackground);
            self.draw_rows(&dc, frame, view, &ctx, RowPass::Backgrounds, 0..view.rows)?;
            self.draw_images(&dc, frame, view, cols, ImageLayer::BelowText);
            // 塗りつぶしのカーソル（セルの背景として塗り済み）が、文字より下の画像に隠れないよう塗り直す
            if block_cursor && let Some((cx, cy, span)) = cursor {
                let b = self.brush(cursor_color)?;
                let (l, t) = (pad + cx as f32 * cw, pad + cy as f32 * ch);
                unsafe { dc.FillRectangle(&rect(l, t, l + span as f32 * cw, t + ch), &b) };
            }
            self.draw_rows(&dc, frame, view, &ctx, RowPass::Text, 0..view.rows)?;
            self.draw_images(&dc, frame, view, cols, ImageLayer::AboveText);
        }
        self.prune_images(frame);
        self.draw_cursor(&dc, frame, &ctx, focused)
    }

    /// 5. カーソル（ブロックで focus があるときは色を反転して描き済み）
    fn draw_cursor(&mut self, dc: &ID2D1DeviceContext, frame: &Frame, ctx: &RowContext, focused: bool) -> Result<()> {
        let (cw, ch) = (self.font.cell_w as f32, self.font.cell_h as f32);
        let pad = self.padding() as f32;
        if let Some((cx, cy, span)) = ctx.cursor
            && cy < frame.rows
            && (cx as usize) < ctx.cols
        {
            let b = self.brush(ctx.cursor_color)?;
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

    /// ビューの行 `y` が写すフレームの行。上や下に足りない行は `None`（背景のまま）
    fn frame_row(frame: &Frame, view: &FrameView, y: u16) -> Option<u16> {
        let fy = view.top + y as i32;
        (0..frame.rows as i32).contains(&fy).then_some(fy as u16)
    }

    /// 1 行のセルの実際の前景・背景（反転・選択・塗りつぶしのカーソル・薄いを当てたもの）を `fgs` / `bgs` に入れる。
    /// `y` はビューの行、`fy` はフレームの行
    #[allow(clippy::too_many_arguments)]
    fn row_colors(&self, frame: &Frame, row: &[Cell], fy: u16, y: u16, ctx: &RowContext, fgs: &mut [Rgb], bgs: &mut [Option<Rgb>]) {
        let sel = frame.selection.get(fy as usize).copied().flatten();
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
            if ctx.block_cursor
                && let Some((cx, cy, span)) = ctx.cursor
                && cy == y
                && (cx as usize..cx as usize + span).contains(&x)
            {
                bg = Some(ctx.cursor_color);
                fg = self.theme.cursor_text.unwrap_or(frame.background);
            }
            if c.style.faint() {
                fg = blend(fg, bg.unwrap_or(frame.background), 0.5);
            }
            fgs[x] = fg;
            bgs[x] = bg;
        }
    }

    /// ビューの行を描く。`pass` で背景と文字のどちらか（画像を間に挟むとき）か、両方（ふつう）かを選ぶ。
    /// 両方のときも、範囲の行の背景をすべて塗ってから文字を描く（文字は色ごとにまとめて描くため。T-008 / T-054）
    fn draw_rows(
        &mut self,
        dc: &ID2D1DeviceContext,
        frame: &Frame,
        view: &FrameView,
        ctx: &RowContext,
        pass: RowPass,
        rows: std::ops::Range<u16>,
    ) -> Result<()> {
        if pass != RowPass::Text {
            self.draw_backgrounds(dc, frame, view, ctx, rows.clone())?;
        }
        if pass != RowPass::Backgrounds {
            self.draw_text(dc, frame, view, ctx, rows)?;
        }
        Ok(())
    }

    /// 背景を描く。セル 1 つを 1 画素にしたビットマップ（既定の背景のセルは透明）を作り、セルの大きさへ最近傍で拡大して 1 回で描く。
    /// セルごとに `FillRectangle` すると、色が細かく変わる画面（314x70 でセルごとに色が違う）で 10 ms かかっていた（T-054）
    fn draw_backgrounds(&mut self, dc: &ID2D1DeviceContext, frame: &Frame, view: &FrameView, ctx: &RowContext, rows: std::ops::Range<u16>) -> Result<()> {
        let cols = ctx.cols;
        let n = rows.len();
        if cols == 0 || n == 0 {
            return Ok(());
        }
        let mut fgs = vec![Rgb::default(); cols];
        let mut bgs: Vec<Option<Rgb>> = vec![None; cols];
        let mut px = std::mem::take(&mut self.bg_pixels);
        px.clear();
        px.resize(cols * n, 0);
        let mut any = false;
        for (i, y) in rows.clone().enumerate() {
            let Some(fy) = Self::frame_row(frame, view, y) else { continue };
            let row = &frame.row(fy)[..cols];
            self.row_colors(frame, row, fy, y, ctx, &mut fgs, &mut bgs);
            for (x, bg) in bgs.iter().enumerate() {
                if let Some(c) = bg {
                    // B8G8R8A8（リトルエンディアンの u32 で 0xAARRGGBB）、不透明
                    px[i * cols + x] = 0xff00_0000 | (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32;
                    any = true;
                }
            }
        }
        let r = if any { self.blit_backgrounds(dc, &px, cols, n, rows.start) } else { Ok(()) };
        self.bg_pixels = px;
        r
    }

    /// 背景のビットマップ（`cols` x `n` 画素）を `y0` 行目から描く
    fn blit_backgrounds(&mut self, dc: &ID2D1DeviceContext, px: &[u32], cols: usize, n: usize, y0: u16) -> Result<()> {
        let size = (cols as u32, n as u32);
        let bitmap = match self.bg_bitmaps.iter().find(|(s, _)| *s == size) {
            Some((_, b)) => b.clone(),
            None => {
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let b = unsafe { dc.CreateBitmap(D2D_SIZE_U { width: size.0, height: size.1 }, None, 0, &props)? };
                // 全体（ペインの大きさ）と点滅（1 行）くらいしか使わないので、少しだけ覚える
                if self.bg_bitmaps.len() >= 4 {
                    self.bg_bitmaps.remove(0);
                }
                self.bg_bitmaps.push((size, b.clone()));
                b
            }
        };
        let (cw, ch) = (self.font.cell_w as f32, self.font.cell_h as f32);
        let pad = self.padding() as f32;
        let top = pad + y0 as f32 * ch;
        let dest = rect(pad, top, pad + cols as f32 * cw, top + n as f32 * ch);
        unsafe {
            bitmap.CopyFromMemory(None, px.as_ptr().cast(), cols as u32 * 4)?;
            dc.DrawBitmap(&bitmap, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
        }
        Ok(())
    }

    /// 文字と装飾（下線・取り消し線・上線）を描く。主フォントのグリフは（フェイス, 大きさ, 色）ごとに範囲の全部の行をまとめ、
    /// グリフの位置はオフセットで渡して 1 回の `DrawGlyphRun` にする（セルごとに色が違う画面で 70 ms → 数 ms。T-054）。
    /// 主フォントに無い文字（レイアウト）はその場で描く。装飾は文字の後
    fn draw_text(&mut self, dc: &ID2D1DeviceContext, frame: &Frame, view: &FrameView, ctx: &RowContext, rows: std::ops::Range<u16>) -> Result<()> {
        let (cw, ch) = (self.font.cell_w as f32, self.font.cell_h as f32);
        let pad = self.padding() as f32;
        let cols = ctx.cols;
        let mut fgs = vec![Rgb::default(); cols];
        let mut bgs: Vec<Option<Rgb>> = vec![None; cols];
        let mut text = String::new();
        let mut runs: Vec<Run> = Vec::new();
        let mut index: HashMap<(usize, u32, Rgb), usize> = HashMap::new();

        for y in rows.clone() {
            let Some(fy) = Self::frame_row(frame, view, y) else { continue };
            let row = &frame.row(fy)[..cols];
            let top = pad + y as f32 * ch;
            let base_y = top + self.font.baseline;
            self.row_colors(frame, row, fy, y, ctx, &mut fgs, &mut bgs);
            for x in 0..cols {
                let c = &row[x];
                if matches!(c.wide, Wide::SpacerTail | Wide::SpacerHead) || c.is_blank() || c.style.invisible() || c.ch == PLACEHOLDER {
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
                // 幅 1 のセルに広いグリフ。Overlap なら縮めず、セルの左端から右隣へはみ出させる
                // （背景は先に塗ってあるので、はみ出した部分は隣のセルの上に重なる）
                let overlap = span == 1 && advance > cw + 0.5 && self.theme.narrow_wide == NarrowWide::Overlap;
                // 孤立した広いグリフは右隣の空白を借りて 2 セル分の枠にする
                if span == 1
                    && !overlap
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
                let scale = if overlap || (glyph.is_some() && advance <= slot + 0.5) {
                    1.0
                } else {
                    fit_scale(advance, ink_a, ink_d, slot, asc, desc)
                };
                let gx = if overlap { left } else { left + (slot - advance * scale) / 2.0 };

                match glyph {
                    Some(g) => {
                        let em = self.font.em_px * scale;
                        let key = (si, em.to_bits(), fg);
                        let i = *index.entry(key).or_insert_with(|| {
                            runs.push(Run { face: si, em, color: fg, origin: (gx, base_y), indices: Vec::new(), offsets: Vec::new() });
                            runs.len() - 1
                        });
                        let r = &mut runs[i];
                        r.indices.push(g.index);
                        // 送り幅は 0 にして、位置はすべてオフセットで決める（上向きが正）
                        r.offsets.push(DWRITE_GLYPH_OFFSET { advanceOffset: gx - r.origin.0, ascenderOffset: r.origin.1 - base_y });
                        // 1 つの並びが長すぎると Direct2D が遅くなる（2 万グリフで 1 回 170 ms）ので、溜まったら描いて次を始める
                        if r.indices.len() >= MAX_RUN_GLYPHS {
                            let full = std::mem::replace(r, Run { face: si, em, color: fg, origin: (gx, base_y), indices: Vec::new(), offsets: Vec::new() });
                            self.flush_run(dc, full)?;
                            index.remove(&key);
                        }
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
        }
        for r in runs {
            self.flush_run(dc, r)?;
        }

        // 下線・取り消し線・上線（placeholder の下線の色は配置の ID で、線としては描かない）。
        // マウスの下のハイパーリンク（`view.hover`）にも下線を引く（T-101）
        for y in rows {
            let Some(fy) = Self::frame_row(frame, view, y) else { continue };
            let row = &frame.row(fy)[..cols];
            let hover = view.hover.filter(|h| h.0 == y).map(|h| (h.1 as usize, (h.2 as usize).min(cols)));
            if hover.is_none() && !row.iter().any(|c| c.underline() != Underline::None || c.style.strikethrough() || c.style.overline()) {
                continue;
            }
            let top = pad + y as f32 * ch;
            let base_y = top + self.font.baseline;
            self.row_colors(frame, row, fy, y, ctx, &mut fgs, &mut bgs);
            for x in 0..cols {
                let c = &row[x];
                let hovered = hover.is_some_and(|(s, e)| (s..e).contains(&x));
                let ul = if hovered && c.underline() == Underline::None { Underline::Single } else { c.underline() };
                if (ul == Underline::None && !c.style.strikethrough() && !c.style.overline()) || c.ch == PLACEHOLDER {
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
        Ok(())
    }

    fn flush_run(&mut self, dc: &ID2D1DeviceContext, r: Run) -> Result<()> {
        if r.indices.is_empty() {
            return Ok(());
        }
        let brush = self.brush(r.color)?;
        let advances = vec![0.0f32; r.indices.len()];
        let run = DWRITE_GLYPH_RUN {
            fontFace: std::mem::ManuallyDrop::new(Some(self.font.faces[r.face].clone())),
            fontEmSize: r.em,
            glyphCount: r.indices.len() as u32,
            glyphIndices: r.indices.as_ptr(),
            glyphAdvances: advances.as_ptr(),
            glyphOffsets: r.offsets.as_ptr(),
            isSideways: false.into(),
            bidiLevel: 0,
        };
        unsafe {
            dc.DrawGlyphRun(Vector2 { X: r.origin.0, Y: r.origin.1 }, &run, None, &brush, DWRITE_MEASURING_MODE_NATURAL);
        }
        // ManuallyDrop の中のフェイスの参照を戻す
        drop(std::mem::ManuallyDrop::into_inner(run.fontFace));
        Ok(())
    }
}
