//! CoreText のフォントと、セル・グリフの寸法（T-125）。
//!
//! - セルの幅と高さは、主フォント（指定のファミリの通常の太さ）の `M` の送り幅と ascent + descent + leading から決める
//! - 太字・斜体は同じファミリの別のフェイス（`CTFontCreateCopyWithSymbolicTraits`）。無ければ通常のフェイスで代用する
//! - 主フォントに無い文字は [`FontSpec::fallback`] のファミリ → **CoreText のシステムのフォールバック**
//!   （`CTFontCreateForString`）の順に探す（Windows 版の DirectWrite のフォールバックに当たる）
//! - 合字は作らない（セルごとに描く。T-033）
//!
//! 寸法はすべてデバイス画素（フォントの大きさに拡大率を掛けてある）。

use std::collections::HashMap;
use std::ptr::NonNull;

use objc2_core_foundation::{CFRange, CFRetained, CFString, CGPoint, CGRect, CGSize};
use objc2_core_text::{CTFont, CTFontOrientation, CTFontSymbolicTraits};

/// フォントの指定
#[derive(Clone, Debug)]
pub struct FontSpec {
    /// ファミリ名の候補（前から順に、見つかったものを使う）
    pub families: Vec<String>,
    /// 大きさ（ポイント）
    pub size_pt: f32,
    /// 主フォントに無い文字に、システムのフォールバックより先に使うファミリ（前から順に。T-033）
    pub fallback: Vec<String>,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            families: vec!["HackGen Console NF".into(), "Menlo".into(), "Monaco".into()],
            size_pt: 12.0,
            fallback: Vec::new(),
        }
    }
}

/// 太さ・斜体の組み合わせ（0 = 通常、1 = 太字、2 = 斜体、3 = 太字 + 斜体）
pub fn style_index(bold: bool, italic: bool) -> usize {
    usize::from(bold) | (usize::from(italic) << 1)
}

/// アトラスに入れるグリフの見分け（フォント・グリフ番号・縮小の段）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// [`FontSet::face`] の番号（0..4 は主フォント、4 以降はフォールバック）
    pub face: usize,
    pub glyph: u16,
    /// 縮める比を 1/256 で量子化したもの（T-013 の shrink。1.0 なら 256）
    pub scale_q: u16,
}

/// グリフの寸法（デバイス画素）
#[derive(Clone, Copy, Debug)]
pub struct GlyphMetrics {
    pub advance: f32,
    /// インクの上端（ベースラインから上が正）
    pub ink_ascent: f32,
    /// インクの下端（ベースラインから下が正）
    pub ink_descent: f32,
    /// 色付きのグリフ（絵文字）。前景色で塗らずにそのまま描く
    pub color: bool,
}

/// フォント 1 つ分（主フォントの 4 フェイスとフォールバック）
pub struct FontSet {
    pub spec: FontSpec,
    /// 実際に使えたファミリ名
    pub family: String,
    /// デバイス画素でのフォントの大きさ
    pub px_size: f32,
    /// 0..4 は主フォントの（通常・太字・斜体・太字斜体）、4 以降は見つけたフォールバック
    faces: Vec<CFRetained<CTFont>>,
    /// フォールバックのファミリ名（faces の 4 以降と対応。同じものを二度作らない）
    fallback_names: Vec<String>,
    pub cell_w: u32,
    pub cell_h: u32,
    pub baseline: f32,
    pub underline_pos: f32,
    pub underline_thick: f32,
    pub strike_pos: f32,
    pub strike_thick: f32,
    /// 文字 → （フェイス, グリフ番号）。見つからなければ `None`
    lookup: HashMap<(char, usize), Option<(usize, u16)>>,
    metrics: HashMap<(usize, u16), GlyphMetrics>,
}

/// `CTFontCreateWithName` は知らない名前でも別のフォントを返すので、ファミリ名が一致したものだけ使う
fn face_for_family(family: &str, px_size: f32) -> Option<CFRetained<CTFont>> {
    let name = CFString::from_str(family);
    let font = unsafe { CTFont::with_name(&name, px_size as f64, std::ptr::null()) };
    let got = unsafe { font.family_name() }.to_string();
    got.eq_ignore_ascii_case(family).then_some(font)
}

fn with_traits(font: &CTFont, px_size: f32, bold: bool, italic: bool) -> Option<CFRetained<CTFont>> {
    let mut value = CTFontSymbolicTraits::empty();
    let mut mask = CTFontSymbolicTraits::empty();
    if bold {
        value |= CTFontSymbolicTraits::TraitBold;
    }
    if italic {
        value |= CTFontSymbolicTraits::TraitItalic;
    }
    mask |= CTFontSymbolicTraits::TraitBold | CTFontSymbolicTraits::TraitItalic;
    unsafe { font.copy_with_symbolic_traits(px_size as f64, std::ptr::null(), value, mask) }
}

impl FontSet {
    /// `scale` は論理ポイント → デバイス画素の拡大率（Retina なら 2.0）
    pub fn new(spec: FontSpec, scale: f32) -> Result<Self, String> {
        let px_size = (spec.size_pt * scale).max(1.0);
        let (family, regular) = spec
            .families
            .iter()
            .find_map(|f| face_for_family(f, px_size).map(|font| (f.clone(), font)))
            .or_else(|| face_for_family("Menlo", px_size).map(|f| ("Menlo".to_string(), f)))
            .ok_or_else(|| format!("フォントが見つかりません: {:?}", spec.families))?;

        let mut faces = Vec::with_capacity(4);
        for si in 0..4 {
            let (bold, italic) = (si & 1 != 0, si & 2 != 0);
            let face = if si == 0 {
                regular.clone()
            } else {
                with_traits(&regular, px_size, bold, italic).unwrap_or_else(|| regular.clone())
            };
            faces.push(face);
        }

        let mut set = Self {
            spec,
            family,
            px_size,
            faces,
            fallback_names: Vec::new(),
            cell_w: 1,
            cell_h: 1,
            baseline: 0.0,
            underline_pos: 0.0,
            underline_thick: 1.0,
            strike_pos: 0.0,
            strike_thick: 1.0,
            lookup: HashMap::new(),
            metrics: HashMap::new(),
        };
        set.measure();
        Ok(set)
    }

    /// セルの大きさと線の位置を主フォントから決める
    fn measure(&mut self) {
        let font = &self.faces[0];
        let (ascent, descent, leading) = unsafe { (font.ascent() as f32, font.descent() as f32, font.leading() as f32) };
        self.cell_h = (ascent + descent + leading).round().max(1.0) as u32;
        self.baseline = (ascent + leading / 2.0).round();
        // 送り幅は `M` から（等幅のフォントなら全部同じ）
        let w = self
            .glyph_index(0, 'M')
            .map(|g| self.measure_glyph(0, g).advance)
            .filter(|a| *a > 0.5)
            .unwrap_or(self.px_size * 0.6);
        self.cell_w = w.round().max(1.0) as u32;
        let up = unsafe { font.underline_position() as f32 };
        let ut = unsafe { font.underline_thickness() as f32 };
        // CoreText の下線の位置はベースラインから上が正。描くほうは下が正
        self.underline_pos = (-up).max(1.0).round();
        self.underline_thick = ut.round().max(1.0);
        let xh = unsafe { font.x_height() as f32 };
        self.strike_pos = (xh / 2.0).round();
        self.strike_thick = self.underline_thick;
    }

    pub fn face(&self, i: usize) -> &CTFont {
        &self.faces[i.min(self.faces.len() - 1)]
    }

    /// そのフォントが持つグリフ番号（持たなければ `None`）
    fn glyph_index(&self, face: usize, ch: char) -> Option<u16> {
        let font = &self.faces[face];
        let mut utf16 = [0u16; 2];
        let units = ch.encode_utf16(&mut utf16).len();
        let mut glyphs = [0u16; 2];
        let ok = unsafe {
            font.glyphs_for_characters(NonNull::from(&utf16[0]), NonNull::from(&mut glyphs[0]), units as isize)
        };
        (ok && glyphs[0] != 0).then_some(glyphs[0])
    }

    /// 文字を描くフェイスとグリフ番号を決める（主フォント → 指定のフォールバック → システムのフォールバック）
    pub fn glyph(&mut self, ch: char, si: usize) -> Option<(usize, u16)> {
        if let Some(hit) = self.lookup.get(&(ch, si)) {
            return *hit;
        }
        let found = self.find_glyph(ch, si);
        self.lookup.insert((ch, si), found);
        found
    }

    fn find_glyph(&mut self, ch: char, si: usize) -> Option<(usize, u16)> {
        if let Some(g) = self.glyph_index(si, ch) {
            return Some((si, g));
        }
        // すでに作ったフォールバックのフェイス
        for i in 4..self.faces.len() {
            if let Some(g) = self.glyph_index(i, ch) {
                return Some((i, g));
            }
        }
        // 設定で指定されたファミリ
        for name in self.spec.fallback.clone() {
            if self.fallback_names.iter().any(|n| *n == name) {
                continue;
            }
            let Some(font) = face_for_family(&name, self.px_size) else { continue };
            let i = self.faces.len();
            self.faces.push(font);
            self.fallback_names.push(name);
            if let Some(g) = self.glyph_index(i, ch) {
                return Some((i, g));
            }
        }
        // CoreText のシステムのフォールバック（絵文字や、入れていない言語の文字）
        let s = CFString::from_str(&ch.to_string());
        let len = ch.len_utf16() as isize;
        let font = unsafe { self.faces[si].for_string(&s, CFRange { location: 0, length: len }) };
        let name = unsafe { font.family_name() }.to_string();
        if let Some(i) = self.fallback_names.iter().position(|n| *n == name) {
            let i = i + 4;
            return self.glyph_index(i, ch).map(|g| (i, g));
        }
        let i = self.faces.len();
        self.faces.push(font);
        self.fallback_names.push(name);
        self.glyph_index(i, ch).map(|g| (i, g))
    }

    /// グリフの寸法（キャッシュする）
    pub fn measure_glyph(&self, face: usize, glyph: u16) -> GlyphMetrics {
        if let Some(m) = self.metrics.get(&(face, glyph)) {
            return *m;
        }
        let font = &self.faces[face];
        let glyphs = [glyph];
        let mut adv = [CGSize::new(0.0, 0.0); 1];
        unsafe {
            font.advances_for_glyphs(CTFontOrientation::Default, NonNull::from(&glyphs[0]), adv.as_mut_ptr(), 1)
        };
        let mut rects = [CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0)); 1];
        unsafe {
            font.bounding_rects_for_glyphs(CTFontOrientation::Default, NonNull::from(&glyphs[0]), rects.as_mut_ptr(), 1)
        };
        let r = rects[0];
        let traits = unsafe { font.symbolic_traits() };
        GlyphMetrics {
            advance: adv[0].width as f32,
            ink_ascent: (r.origin.y + r.size.height) as f32,
            ink_descent: (-r.origin.y) as f32,
            color: traits.contains(CTFontSymbolicTraits::TraitColorGlyphs),
        }
    }

    /// 測った寸法を覚える（`&mut` を持てる場所から呼ぶ）
    pub fn cache_metrics(&mut self, face: usize, glyph: u16) -> GlyphMetrics {
        let m = self.measure_glyph(face, glyph);
        self.metrics.insert((face, glyph), m);
        m
    }
}
