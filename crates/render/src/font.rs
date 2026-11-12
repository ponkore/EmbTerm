//! フォントとセルの大きさ、グリフの寸法のキャッシュ。
//!
//! - セルの幅と高さは、主フォント（指定のファミリの通常の太さ）の `M` の送り幅と ascent + descent + lineGap から決める
//! - 主フォントが持つ 1 コードポイントの文字は、グリフの番号と寸法を引いて `DrawGlyphRun` で描く（速い道）
//! - 主フォントに無い文字・書記素クラスタ（結合文字、ZWJ の絵文字）は `IDWriteTextLayout` に任せる（遅い道）。
//!   フォールバック（[`FontSpec::fallback`] のフォント → DirectWrite のシステムのフォールバック）と色付きの絵文字は
//!   こちらで効く。レイアウトは文字列とスタイルで覚えておき、色はブラシで描くときに決める
//! - 合字は作らない（セルごとに描くので、隣のセルの文字とつながらない。T-033）
//!
//! 座標はすべて物理ピクセル（デバイスコンテキストの DPI は 96 にしてある）。

use std::collections::HashMap;

use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{HSTRING, Interface, PCWSTR, Result, w};

/// フォントの指定
#[derive(Clone, Debug)]
pub struct FontSpec {
    /// ファミリ名の候補（前から順に、見つかったものを使う）
    pub families: Vec<String>,
    /// 大きさ（ポイント）
    pub size_pt: f32,
    /// 主フォントに無い文字に、システムのフォールバックより先に使うファミリ（前から順に。T-033）。
    /// 無いファミリは飛ばす
    pub fallback: Vec<String>,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            families: vec!["HackGen Console NF".into(), "Cascadia Mono".into(), "Consolas".into()],
            size_pt: 12.0,
            fallback: Vec::new(),
        }
    }
}

/// 主フォントで引いたグリフ
#[derive(Clone, Copy, Debug)]
pub(crate) struct Glyph {
    pub index: u16,
    /// 送り幅（px）
    pub advance: f32,
    /// インクの上端の基準線からの高さと、下端の深さ（px）
    pub ink_ascent: f32,
    pub ink_descent: f32,
}

/// フォールバックに任せる文字のレイアウト
pub(crate) struct Layout {
    pub layout: IDWriteTextLayout,
    pub advance: f32,
    /// レイアウトの上端から基準線まで（px）
    pub baseline: f32,
    pub ink_ascent: f32,
    pub ink_descent: f32,
}

/// スタイルの番号（太字 = 1、斜体 = 2）
pub(crate) fn style_index(bold: bool, italic: bool) -> usize {
    bold as usize | ((italic as usize) << 1)
}

pub(crate) struct FontSet {
    pub family: String,
    pub em_px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    /// セルの上端から基準線まで（px）
    pub baseline: f32,
    /// 下線の位置（基準線から下へ）と太さ
    pub underline_pos: f32,
    pub underline_thick: f32,
    /// 取り消し線の位置（基準線から上へ）と太さ
    pub strike_pos: f32,
    pub strike_thick: f32,
    pub faces: [IDWriteFontFace; 4],
    /// 各フェイスの 1 デザイン単位あたりの px
    scale: [f32; 4],
    formats: [IDWriteTextFormat; 4],
    glyphs: HashMap<(char, u8), Option<Glyph>>,
    layouts: HashMap<(String, u8), Layout>,
}

/// 見つからないときに使う最後の候補（DirectWrite のシステムのフォールバックがあるので、ここは何でもよい）
const LAST_RESORT: &str = "Consolas";

fn find_family(collection: &IDWriteFontCollection, name: &str) -> Option<IDWriteFontFamily> {
    unsafe {
        let mut index = 0u32;
        let mut exists = windows::core::BOOL(0);
        collection.FindFamilyName(&HSTRING::from(name), &mut index, &mut exists).ok()?;
        if !exists.as_bool() {
            return None;
        }
        collection.GetFontFamily(index).ok()
    }
}

/// 指定のファミリ（ある物だけ）を先に、システムのフォールバックを後に引くフォールバック。指定が無ければ `None`
/// （書式の既定 = システムのフォールバックのまま）
fn make_fallback(dwrite: &IDWriteFactory, collection: &IDWriteFontCollection, families: &[String]) -> Option<IDWriteFontFallback> {
    let names: Vec<HSTRING> = families.iter().filter(|n| find_family(collection, n).is_some()).map(|n| HSTRING::from(n.as_str())).collect();
    if names.is_empty() {
        return None;
    }
    unsafe {
        let f2 = dwrite.cast::<IDWriteFactory2>().ok()?;
        let builder = f2.CreateFontFallbackBuilder().ok()?;
        let ptrs: Vec<*const u16> = names.iter().map(|n| n.as_ptr()).collect();
        let all = [DWRITE_UNICODE_RANGE { first: 0, last: 0x10FFFF }];
        builder.AddMapping(&all, &ptrs, collection, PCWSTR::null(), PCWSTR::null(), 1.0).ok()?;
        // 指定のファミリに無い文字はシステムのフォールバックへ
        builder.AddMappings(&f2.GetSystemFontFallback().ok()?).ok()?;
        builder.CreateFontFallback().ok()
    }
}

impl FontSet {
    pub fn new(dwrite: &IDWriteFactory, spec: &FontSpec, dpi: u32) -> Result<Self> {
        unsafe {
            let mut collection = None;
            dwrite.GetSystemFontCollection(&mut collection, false)?;
            let collection = collection.expect("GetSystemFontCollection が成功すればある");
            let (family_name, family) = spec
                .families
                .iter()
                .map(String::as_str)
                .chain([LAST_RESORT])
                .find_map(|n| find_family(&collection, n).map(|f| (n.to_string(), f)))
                .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;

            let fallback = make_fallback(dwrite, &collection, &spec.fallback);
            let em_px = spec.size_pt * dpi as f32 / 72.0;
            let mut faces = Vec::with_capacity(4);
            let mut formats = Vec::with_capacity(4);
            let mut scale = [0f32; 4];
            for (i, sc) in scale.iter_mut().enumerate() {
                let bold = i & 1 != 0;
                let italic = i & 2 != 0;
                let weight = if bold { DWRITE_FONT_WEIGHT_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
                let style = if italic { DWRITE_FONT_STYLE_ITALIC } else { DWRITE_FONT_STYLE_NORMAL };
                let font = family.GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, style)?;
                let face = font.CreateFontFace()?;
                let mut m = DWRITE_FONT_METRICS::default();
                face.GetMetrics(&mut m);
                *sc = em_px / m.designUnitsPerEm.max(1) as f32;
                faces.push(face);
                let format = dwrite.CreateTextFormat(
                    &HSTRING::from(family_name.as_str()),
                    None,
                    weight,
                    style,
                    DWRITE_FONT_STRETCH_NORMAL,
                    em_px,
                    w!("ja-jp"),
                )?;
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                if let Some(fb) = &fallback {
                    format.cast::<IDWriteTextFormat1>()?.SetFontFallback(fb)?;
                }
                formats.push(format);
            }
            let (Ok(faces), Ok(formats)) = (<[IDWriteFontFace; 4]>::try_from(faces), <[IDWriteTextFormat; 4]>::try_from(formats))
            else {
                unreachable!("4 つずつ作った")
            };

            let mut m = DWRITE_FONT_METRICS::default();
            faces[0].GetMetrics(&mut m);
            let s = scale[0];
            // セルの幅: M の送り幅（等幅フォントならどの半角文字も同じ）
            let mut idx = [0u16];
            faces[0].GetGlyphIndices(&('M' as u32), 1, idx.as_mut_ptr())?;
            let mut gm = [DWRITE_GLYPH_METRICS::default()];
            faces[0].GetDesignGlyphMetrics(idx.as_ptr(), 1, gm.as_mut_ptr(), false)?;
            let adv = gm[0].advanceWidth as f32 * s;
            let cell_w = adv.round().max(1.0) as u32;
            let ascent = m.ascent as f32 * s;
            let descent = m.descent as f32 * s;
            let line_gap = (m.lineGap.max(0)) as f32 * s;
            let cell_h = (ascent + descent + line_gap).round().max(1.0) as u32;
            // ascent + descent をセルの高さの中で上下の真ん中に置く
            let baseline = ((cell_h as f32 - (ascent + descent)) / 2.0 + ascent).round();
            let underline_thick = (m.underlineThickness as f32 * s).round().max(1.0);
            let underline_pos = (-(m.underlinePosition as f32) * s).round().max(1.0);
            let strike_thick = (m.strikethroughThickness as f32 * s).round().max(1.0);
            let strike_pos = (m.strikethroughPosition as f32 * s).round();
            Ok(Self {
                family: family_name,
                em_px,
                cell_w,
                cell_h,
                baseline,
                underline_pos,
                underline_thick,
                strike_pos,
                strike_thick,
                faces,
                scale,
                formats,
                glyphs: HashMap::new(),
                layouts: HashMap::new(),
            })
        }
    }

    /// 主フォント（のスタイル `si`）で 1 文字のグリフを引く。持っていなければ `None`
    pub fn glyph(&mut self, ch: char, si: usize) -> Option<Glyph> {
        if let Some(g) = self.glyphs.get(&(ch, si as u8)) {
            return *g;
        }
        let g = unsafe {
            let face = &self.faces[si];
            let mut idx = [0u16];
            let cp = ch as u32;
            match face.GetGlyphIndices(&cp, 1, idx.as_mut_ptr()) {
                Ok(()) if idx[0] != 0 => {
                    let mut gm = [DWRITE_GLYPH_METRICS::default()];
                    if face.GetDesignGlyphMetrics(idx.as_ptr(), 1, gm.as_mut_ptr(), false).is_ok() {
                        let s = self.scale[si];
                        let m = gm[0];
                        Some(Glyph {
                            index: idx[0],
                            advance: m.advanceWidth as f32 * s,
                            ink_ascent: (m.verticalOriginY - m.topSideBearing) as f32 * s,
                            ink_descent: (m.advanceHeight as i32 - m.bottomSideBearing - m.verticalOriginY) as f32 * s,
                        })
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };
        if self.glyphs.len() > 16384 {
            self.glyphs.clear();
        }
        self.glyphs.insert((ch, si as u8), g);
        g
    }

    /// 文字列（書記素クラスタ 1 つ）を DirectWrite のレイアウトにする。フォールバックはここで効く
    pub fn layout(&mut self, dwrite: &IDWriteFactory, text: &str, si: usize) -> Option<&Layout> {
        let key = (text.to_string(), si as u8);
        if !self.layouts.contains_key(&key) {
            if self.layouts.len() > 4096 {
                self.layouts.clear();
            }
            let l = unsafe { self.make_layout(dwrite, text, si) }.ok()?;
            self.layouts.insert(key.clone(), l);
        }
        self.layouts.get(&key)
    }

    unsafe fn make_layout(&self, dwrite: &IDWriteFactory, text: &str, si: usize) -> Result<Layout> {
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().collect();
            let layout = dwrite.CreateTextLayout(&wide, &self.formats[si], 10000.0, 10000.0)?;
            let mut tm = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut tm)?;
            let mut lines = [DWRITE_LINE_METRICS::default(); 1];
            let mut n = 0u32;
            let _ = layout.GetLineMetrics(Some(&mut lines), &mut n);
            let baseline = lines[0].baseline;
            // レイアウトの箱を文字の大きさにしてから、インクのはみ出しを測る
            layout.SetMaxWidth(tm.widthIncludingTrailingWhitespace.max(1.0))?;
            layout.SetMaxHeight(tm.height.max(1.0))?;
            let o = layout.GetOverhangMetrics()?;
            Ok(Layout {
                advance: tm.widthIncludingTrailingWhitespace,
                baseline,
                ink_ascent: baseline + o.top,
                ink_descent: tm.height + o.bottom - baseline,
                layout,
            })
        }
    }
}

/// セル（`slot_w` 幅、基準線から上が `asc`、下が `desc`）に収めるための縮め率（T-013）。
///
/// ghostel（`Renderer.zig` の `adjustGlyph`）と同じく、幅・上・下の比の最小で縮める（1 より大きくはしない）。
/// ghostel はフォントの ascent / descent で比べるが、ここではグリフのインクの範囲で比べる
/// （日本語のフォールバックのフォントは行の高さが大きいだけで、インクはセルに収まることが多いため）
pub(crate) fn fit_scale(advance: f32, ink_ascent: f32, ink_descent: f32, slot_w: f32, asc: f32, desc: f32) -> f32 {
    // 丸めの誤差で縮めないよう、0.5px までのはみ出しは許す
    let sw = if advance > slot_w + 0.5 { slot_w / advance } else { 1.0 };
    let sa = if ink_ascent > asc + 0.5 { asc / ink_ascent } else { 1.0 };
    let sd = if ink_descent > desc + 0.5 { desc / ink_descent } else { 1.0 };
    sw.min(sa).min(sd).clamp(0.05, 1.0)
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
        // 幅と下の両方 → 小さいほう
        assert!((fit_scale(15.0, 10.0, 8.0, 10.0, 14.0, 4.0) - 0.5).abs() < 1e-6);
    }
}
