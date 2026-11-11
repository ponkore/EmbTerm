//! 描く 1 フレームの中身。libghostty-vt の render state から、変わった行だけを写して持つ。
//!
//! render state は「次の update まで」しか読めないので、描画のほうでは使い回せる形に写しておく。
//! 写すのは UI スレッドで、端末のロックを外したあと（T-006）。

use et_vt::{Cursor, CursorShape, RenderState, Rgb, Style, StyleExt, Underline, Wide};

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

/// フレーム全体
#[derive(Clone)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
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
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            cols: 0,
            rows: 0,
            cells: Vec::new(),
            selection: Vec::new(),
            cursor: Cursor { pos: None, wide_tail: false, visible: false, blinking: false, shape: CursorShape::Block },
            foreground: Rgb { r: 0xd4, g: 0xd4, b: 0xd4 },
            background: Rgb { r: 0x1e, g: 0x1e, b: 0x1e },
            cursor_color: None,
            palette: [Rgb::default(); 256],
        }
    }
}

impl Frame {
    pub fn cell(&self, x: u16, y: u16) -> &Cell {
        &self.cells[y as usize * self.cols as usize + x as usize]
    }

    pub fn row(&self, y: u16) -> &[Cell] {
        let c = self.cols as usize;
        &self.cells[y as usize * c..(y as usize + 1) * c]
    }

    /// render state から写す。変わった行だけを読み、最後に render state の dirty を落とす。
    /// 何か変わったら true（描き直しが要る）
    pub fn update(&mut self, rs: &mut RenderState) -> bool {
        let dirty = rs.dirty();
        let (cols, rows) = rs.size();
        if (cols, rows) != (self.cols, self.rows) {
            self.cols = cols;
            self.rows = rows;
            self.cells = vec![Cell::default(); cols as usize * rows as usize];
            self.selection = vec![None; rows as usize];
        }
        let colors = rs.colors();
        let cursor = rs.cursor();
        let mut changed = dirty != et_vt::Dirty::Clean
            || colors.foreground != self.foreground
            || colors.background != self.background
            || colors.cursor != self.cursor_color
            || cursor.pos != self.cursor.pos
            || cursor.visible != self.cursor.visible
            || cursor.shape != self.cursor.shape;
        self.foreground = colors.foreground;
        self.background = colors.background;
        self.cursor_color = colors.cursor;
        self.palette = colors.palette;
        self.cursor = cursor;

        if dirty != et_vt::Dirty::Clean {
            let mut text = Vec::new();
            let mut rows_it = rs.dirty_rows();
            while let Some(y) = rows_it.next_dirty() {
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
