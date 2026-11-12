//! 描く 1 フレームの中身。libghostty-vt の render state から、変わった行だけを写して持つ。
//!
//! render state は「次の update まで」しか読めないので、描画のほうでは使い回せる形に写しておく。
//! 写すのは UI スレッドで、端末のロックを外したあと（T-006）。

use std::sync::Arc;

use et_vt::{Cursor, CursorShape, ImageFormat, ImagePlacement, RenderState, Rgb, Style, StyleExt, Terminal, Underline, Wide};

/// セル 1 つ
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    /// 基底の文字。空のセルは `'\0'`
    pub ch: char,
    /// 結合文字など、2 つ目以降のコードポイント（たいていは空で、確保もしない）
    pub extra: Vec<char>,
    /// 前景色・背景色（パレットは解決済み）。`None` は既定の色
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub style: Style,
    pub wide: Wide,
}

impl Default for Cell {
    fn default() -> Self {
        Self { ch: '\0', extra: Vec::new(), fg: None, bg: None, style: Style::default(), wide: Wide::Narrow }
    }
}

impl Cell {
    /// 描く文字が無い（空か空白）
    pub fn is_blank(&self) -> bool {
        (self.ch == '\0' || self.ch == ' ') && self.extra.is_empty()
    }

    pub fn bold(&self) -> bool {
        self.style.bold()
    }
    pub fn italic(&self) -> bool {
        self.style.italic()
    }
    pub fn underline(&self) -> Underline {
        self.style.underline()
    }
}

/// Kitty graphics の Unicode placeholder（T-088）。画像は render state が解決するので、文字としては描かない
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// 端末から写した画像の画素（[`Frame::capture_images`]）
#[derive(Debug)]
pub struct ImagePixels {
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
    pub data: Vec<u8>,
}

/// 描く画像 1 枚（配置 1 つ）
#[derive(Clone, Debug)]
pub struct PlacedImage {
    pub placement: ImagePlacement,
    pub pixels: Arc<ImagePixels>,
}

/// フレーム全体
#[derive(Clone)]
pub struct Frame {
    pub cols: u16,
    /// 写した行数（overscan の分を含む）
    pub rows: u16,
    /// ビューポートの先頭の行が `cells` の何行目か（overscan の上の行数）
    pub viewport_top: u16,
    /// 行ごとに `cols` 個
    pub cells: Vec<Cell>,
    /// 行ごとの選択範囲（両端を含む桁）
    pub selection: Vec<Option<(u16, u16)>>,
    pub cursor: Cursor,
    pub foreground: Rgb,
    pub background: Rgb,
    /// アプリ（OSC 12）か既定で決まったカーソルの色
    pub cursor_color: Option<Rgb>,
    pub palette: [Rgb; 256],
    /// 描く画像（層、z-index の順。[`Frame::capture_images`] で写す）
    pub images: Vec<PlacedImage>,
    /// `images` の配置が計算されたときのセルの画素の大きさ（[`Terminal::cell_px`]）。DPI の違うペインで拡大縮小に使う
    pub image_cell: (u32, u32),
    /// `images` が変わったので描き直しが要る（次の [`Frame::update`] が受け取る）
    images_changed: bool,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            cols: 0,
            rows: 0,
            viewport_top: 0,
            cells: Vec::new(),
            selection: Vec::new(),
            cursor: Cursor { pos: None, wide_tail: false, visible: false, blinking: false, shape: CursorShape::Block },
            foreground: Rgb { r: 0xd4, g: 0xd4, b: 0xd4 },
            background: Rgb { r: 0x1e, g: 0x1e, b: 0x1e },
            cursor_color: None,
            palette: [Rgb::default(); 256],
            images: Vec::new(),
            image_cell: (1, 1),
            images_changed: false,
        }
    }
}

impl Frame {
    pub fn cell(&self, x: u16, y: u16) -> &Cell {
        &self.cells[y as usize * self.cols as usize + x as usize]
    }

    /// 点滅（SGR 5）の文字があるか（T-112。点滅のタイマーを動かすか決める）
    pub fn has_blink(&self) -> bool {
        self.cells.iter().any(|c| c.style.blink() && !c.is_blank())
    }

    pub fn row(&self, y: u16) -> &[Cell] {
        let c = self.cols as usize;
        &self.cells[y as usize * c..(y as usize + 1) * c]
    }

    /// render state の画像の一覧と、端末が持つ画素を写す（T-088）。**端末のロックの中で、`rs.update` のあとに呼ぶ**
    /// （render state は画素を持たない。画素は `(generation)` が同じあいだは写し直さない）。
    /// 一覧は毎回読んで持っているものと比べる（配置の世代は同じ render state の間でしか比べられず、遡るペインや hold では
    /// render state が替わるうえ、一覧は短い）
    pub fn capture_images(&mut self, rs: &mut RenderState, term: &Terminal) {
        let placements = rs.images();
        let same = placements.len() == self.images.len() && placements.iter().zip(&self.images).all(|(p, i)| *p == i.placement);
        let cell = term.cell_px();
        if same {
            if cell != self.image_cell {
                self.image_cell = cell;
                self.images_changed = true;
            }
            return;
        }
        let mut images: Vec<PlacedImage> = Vec::with_capacity(placements.len());
        for placement in placements {
            let known = images.iter().chain(&self.images).find(|i| i.placement.generation == placement.generation).map(|i| i.pixels.clone());
            let pixels = known.or_else(|| {
                let image = term.kitty_image(placement.image_id).filter(|im| im.generation == placement.generation)?;
                Some(Arc::new(ImagePixels { width: image.width, height: image.height, format: image.format, data: image.pixels.to_vec() }))
            });
            // 画素が読めない（その間に消された）配置は描かない。次の update で一覧から消える
            if let Some(pixels) = pixels {
                images.push(PlacedImage { placement, pixels });
            }
        }
        self.images = images;
        self.image_cell = cell;
        self.images_changed = true;
    }

    /// render state から写す。変わった行だけを読み、最後に render state の dirty を落とす。
    /// 何か変わったら true（描き直しが要る）
    pub fn update(&mut self, rs: &mut RenderState) -> bool {
        let dirty = rs.dirty();
        let (cols, vrows) = rs.size();
        let (above, below) = rs.overscan();
        let rows = vrows + above + below;
        self.viewport_top = above;
        // 大きさが変わると中身が消えるので、全部の行を写し直す（部分の dirty のままでも）
        let resized = (cols, rows) != (self.cols, self.rows);
        if resized {
            self.cols = cols;
            self.rows = rows;
            self.cells = vec![Cell::default(); cols as usize * rows as usize];
            self.selection = vec![None; rows as usize];
        }
        let colors = rs.colors();
        let cursor = rs.cursor();
        // パレットが変わった（T-034 のテーマの追従、OSC 4）。セルの色は写すときに解決済みなので、全部の行を写し直す
        let palette_changed = colors.palette != self.palette;
        let mut changed = dirty != et_vt::Dirty::Clean
            || palette_changed
            || colors.foreground != self.foreground
            || colors.background != self.background
            || colors.cursor != self.cursor_color
            || cursor.pos != self.cursor.pos
            || cursor.visible != self.cursor.visible
            || cursor.shape != self.cursor.shape
            || cursor.blinking != self.cursor.blinking
            || std::mem::take(&mut self.images_changed);
        self.foreground = colors.foreground;
        self.background = colors.background;
        self.cursor_color = colors.cursor;
        self.palette = colors.palette;
        self.cursor = cursor;

        let all = resized || palette_changed;
        if dirty != et_vt::Dirty::Clean || all {
            let mut text = Vec::new();
            let mut rows_it = if all { rs.all_rows() } else { rs.dirty_rows() };
            while let Some(y) = if all { rows_it.next_row() } else { rows_it.next_dirty() } {
                if y >= rows {
                    continue;
                }
                self.selection[y as usize] = rows_it.selection();
                let base = y as usize * cols as usize;
                let mut cells = rows_it.cells();
                let mut x = 0usize;
                while x < cols as usize && cells.next_cell() {
                    cells.graphemes(&mut text);
                    let c = &mut self.cells[base + x];
                    c.ch = text.first().copied().unwrap_or('\0');
                    c.extra.clear();
                    if text.len() > 1 {
                        c.extra.extend_from_slice(&text[1..]);
                    }
                    c.style = cells.style();
                    c.fg = cells.fg();
                    c.bg = cells.bg();
                    c.wide = cells.wide();
                    x += 1;
                }
            }
            changed = true;
        }
        rs.clean();
        changed
    }

    /// 画面の 1 行をテキストにする（テスト・ログ用）。幅 2 の文字の右半分は飛ばす
    pub fn row_text(&self, y: u16) -> String {
        let mut s = String::new();
        for c in self.row(y) {
            if c.wide == Wide::SpacerTail {
                continue;
            }
            s.push(if c.ch == '\0' { ' ' } else { c.ch });
            s.extend(&c.extra);
        }
        s.trim_end().to_string()
    }
}
