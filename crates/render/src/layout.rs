//! 描く前の計算（OS に依らない）。Direct2D 版（Windows）と Metal 版（macOS）で同じ規則を使う。
//!
//! - [`row_colors`]: セルの実際の前景・背景（反転・選択・塗りつぶしのカーソル・薄い）
//! - [`cursor_cell`]: カーソルがビューのどのセルにいるか
//! - [`fit_scale`]: 1 セルの枠に収まるよう縮める比（T-013）
//! - [`blend`]: 2 色を混ぜる（SGR 2 の「薄い」）

use et_vt::{Rgb, StyleExt, Wide};

use crate::frame::{Cell, Frame};
use crate::theme::Theme;
use crate::view::FrameView;

/// 塗りつぶしのカーソルの情報（[`row_colors`] に渡す）
#[derive(Clone, Copy, Debug)]
pub struct RowCursor {
    /// カーソルのセル（ビューの中の桁・行、幅）。見えないなら `None`
    pub cursor: Option<(u16, u16, usize)>,
    /// ブロックのカーソルで、フォーカスがある（セルの色を反転する）
    pub block: bool,
    pub color: Rgb,
}

/// 2 色を混ぜる
pub fn blend(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Rgb { r: m(a.r, b.r), g: m(a.g, b.g), b: m(a.b, b.b) }
}

/// 広いグリフを枠に収める比。丸めの誤差で縮めないよう、0.5px までのはみ出しは許す
pub fn fit_scale(advance: f32, ink_ascent: f32, ink_descent: f32, slot_w: f32, asc: f32, desc: f32) -> f32 {
    let sw = if advance > slot_w + 0.5 { slot_w / advance } else { 1.0 };
    let sa = if ink_ascent > asc + 0.5 { asc / ink_ascent } else { 1.0 };
    let sd = if ink_descent > desc + 0.5 { desc / ink_descent } else { 1.0 };
    sw.min(sa).min(sd).clamp(0.05, 1.0)
}

/// ビューの行 `y` が写すフレームの行。上や下に足りない行は `None`（背景のまま）
pub fn frame_row(frame: &Frame, view: &FrameView, y: u16) -> Option<u16> {
    let fy = view.top + y as i32;
    (0..frame.rows as i32).contains(&fy).then_some(fy as u16)
}

/// カーソルのセル（ビューの中の桁・行と幅）。`view.show_cursor` は見ない。見えなければ `None`
pub fn cursor_cell(frame: &Frame, view: &FrameView, cols: usize) -> Option<(u16, u16, usize)> {
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

/// 1 行のセルの実際の前景・背景（反転・選択・塗りつぶしのカーソル・薄いを当てたもの）を `fgs` / `bgs` に入れる。
/// `y` はビューの行、`fy` はフレームの行
#[allow(clippy::too_many_arguments)]
pub fn row_colors(
    theme: &Theme,
    frame: &Frame,
    row: &[Cell],
    fy: u16,
    y: u16,
    cur: &RowCursor,
    fgs: &mut [Rgb],
    bgs: &mut [Option<Rgb>],
) {
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
            bg = Some(theme.selection_bg);
            if let Some(f) = theme.selection_fg {
                fg = f;
            }
        }
        if cur.block
            && let Some((cx, cy, span)) = cur.cursor
            && cy == y
            && (cx as usize..cx as usize + span).contains(&x)
        {
            bg = Some(cur.color);
            fg = theme.cursor_text.unwrap_or(frame.background);
        }
        if c.style.faint() {
            fg = blend(fg, bg.unwrap_or(frame.background), 0.5);
        }
        fgs[x] = fg;
        bgs[x] = bg;
    }
}

#[cfg(test)]
mod tests {
    use super::fit_scale;

    #[test]
    fn scale_rules() {
        // 収まる
        assert_eq!(fit_scale(10.0, 12.0, 3.0, 10.0, 14.0, 4.0), 1.0);
        // 幅が 2 倍 → 半分
        assert_eq!(fit_scale(20.0, 12.0, 3.0, 10.0, 14.0, 4.0), 0.5);
        // 上にはみ出す → 上の比
        assert!((fit_scale(10.0, 28.0, 3.0, 10.0, 14.0, 4.0) - 0.5).abs() < 1e-6);
    }
}
