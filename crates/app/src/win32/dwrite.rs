//! DirectWrite text measurement and shaping — the P1/P2 backend of the
//! renderer rewrite.
//!
//! Widths are the editor's integer cell grid, not free-form DWrite metrics:
//! each code point is mapped to a glyph index in the editor face, the glyph's
//! design advance is scaled by `emPx / designUnitsPerEm`, rounded half-up and
//! clamped to `0..=254` — the same arithmetic the GDI measurer uses for its
//! measure helper (`advance * emPx / units + 0.5`, clamp `0..254`). Keeping the
//! integer grid means `LineMetrics`, wrap, hit-test and the caret never see the
//! backend change.
//!
//! `emPx` is the same `|lfHeight|` the GDI path asks `CreateFontW` for, and the
//! heights come from the face's design metrics, so a backend swap does not move
//! a single row.
//!
//! Shaping is where the rewrite stops imitating GDI's font-linking: a code
//! point the editor face has no glyph for is resolved through DirectWrite's
//! own fallback chain (`IDWriteFontFallback::MapCharacters`), the same one the
//! draw path then uses, and [`TextShaper::shape`] groups consecutive
//! code points that landed on the same face into one glyph run. The advances
//! in those runs are the editor's grid advances, not the faces' design
//! advances, so glyphs land exactly under the columns [`dg_render`] laid out.

use std::collections::HashMap;

use dg_layout::measure::{attaches_to_previous, Font, TextMeasurer};
use windows::core::{implement, Interface, BOOL, HSTRING, PCWSTR};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_GLYPH_METRICS,
    DWRITE_READING_DIRECTION, DWRITE_READING_DIRECTION_LEFT_TO_RIGHT, DWriteCreateFactory,
    IDWriteFactory, IDWriteFactory2,
    IDWriteFont, IDWriteFontCollection, IDWriteFontFace, IDWriteFontFace2, IDWriteFontFallback,
    IDWriteNumberSubstitution, IDWriteTextAnalysisSource, IDWriteTextAnalysisSource_Impl,
};

use crate::win32::gdi::{EDITOR_FACE, EditorFace, pixel_height};

/// `IDWriteTextAnalysisSource` over one UTF-16 buffer: the bridge
/// `MapCharacters` needs in order to look at "the text at position n" without
/// a text layout. The analysis is forward-only and single-locale, which is all
/// a fallback probe needs.
#[implement(IDWriteTextAnalysisSource)]
struct RunText {
    utf16: Vec<u16>,
    /// NUL-terminated: DirectWrite reads the locale name as a string.
    locale: Vec<u16>,
}

impl IDWriteTextAnalysisSource_Impl for RunText_Impl {
    fn GetTextAtPosition(
        &self,
        textposition: u32,
        textstring: *mut *mut u16,
        textlength: *mut u32,
    ) -> windows::core::Result<()> {
        let pos = textposition as usize;
        unsafe {
            if pos >= self.utf16.len() {
                *textstring = std::ptr::null_mut();
                *textlength = 0;
            } else {
                *textstring = self.utf16.as_ptr().add(pos) as *mut u16;
                *textlength = (self.utf16.len() - pos) as u32;
            }
        }
        Ok(())
    }

    fn GetTextBeforePosition(
        &self,
        textposition: u32,
        textstring: *mut *mut u16,
        textlength: *mut u32,
    ) -> windows::core::Result<()> {
        let pos = textposition as usize;
        unsafe {
            if pos == 0 {
                *textstring = std::ptr::null_mut();
                *textlength = 0;
            } else {
                let end = pos.min(self.utf16.len());
                *textstring = self.utf16.as_ptr() as *mut u16;
                *textlength = end as u32;
            }
        }
        Ok(())
    }

    fn GetParagraphReadingDirection(&self) -> DWRITE_READING_DIRECTION {
        DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }

    fn GetLocaleName(
        &self,
        textposition: u32,
        textlength: *mut u32,
        localename: *mut *mut u16,
    ) -> windows::core::Result<()> {
        unsafe {
            *localename = self.locale.as_ptr() as *mut u16;
            *textlength = self.utf16.len().saturating_sub(textposition as usize) as u32;
        }
        Ok(())
    }

    fn GetNumberSubstitution(
        &self,
        _textposition: u32,
        _textlength: *mut u32,
        numbersubstitution: windows::core::OutRef<IDWriteNumberSubstitution>,
    ) -> windows::core::Result<()> {
        numbersubstitution.write(None)
    }
}

/// One baseline's worth of glyphs that share a face: what `DrawGlyphRun`
/// takes. `advances` are the editor grid's, so the run ends where the layout
/// says the text ends.
pub struct ShapedRun {
    pub face_key: usize,
    pub glyphs: Vec<u16>,
    pub advances: Vec<f32>,
}

/// The editor face plus every face DirectWrite's fallback resolved for a code
/// point seen so far. Face 0 is the editor face; a run's `face_key` indexes
/// [`Self::face`].
pub struct TextShaper {
    #[allow(dead_code)]
    factory: IDWriteFactory,
    factory2: Option<IDWriteFactory2>,
    fallback: Option<IDWriteFontFallback>,
    faces: Vec<IDWriteFontFace>,
    units: Vec<f32>,
    /// 每个 face 自己的 em 像素：第二只字体（`[FontN] CJKFont=`）带着自己的
    /// 字号，量它的 advance 得用它自己的 em，不能用主字体的。
    ems: Vec<f32>,
    /// Whether a face carries colour glyphs — only those are worth a
    /// `TranslateColorGlyphRun` pass per run.
    color: Vec<bool>,
    /// 「对中日韩字符使用单独的字体」勾上时第二只字体的 face 下标；没勾、
    /// 没写 `CJKFont=` 行或字面没装时是 `None` —— 回到单字体 + 系统回退。
    cjk_face_key: Option<usize>,
    family_name: HSTRING,
    /// 家族在系统集合里的下标 —— 按字形位挑变体时还要再查一次同一族。
    collection: IDWriteFontCollection,
    family_index: u32,
    /// 主题给的基准字重（`[Fonts] mono.weight`）：粗体从它往上取。
    base_weight: i32,
    /// `[正体, 粗, 斜, 粗斜]` → [`Self::faces`] 的下标。`usize::MAX` 表示
    /// 还没建；建不出来（家族没有这个变体）时记 0，退回编辑器那只。
    style_faces: [usize; 4],
    face_key_by_family: HashMap<String, usize>,
    face_key_by_cp: HashMap<u32, usize>,
    /// Measured advance per code point: the row pass asks for the same
    /// characters every frame, and only the first ask needs DirectWrite.
    advance_by_cp: HashMap<u32, u8>,
    /// The em the text is drawn at: `|lfHeight|` for the same point size.
    pub em_px: f32,
    /// Ascent in device pixels: the baseline inside a row.
    pub ascent_px: f32,
    /// `tmHeight`'s equivalent: what this implementation keeps as the font cell height.
    pub tm_height: i32,
}

impl TextShaper {
    /// `desc` 是绘制那条路用的同一份描述 —— 字面、字重、斜体、字号全从它来，
    /// 这样测量出的单元格和画出来的字形是同一只字体。
    ///
    /// 皮肤写的字面名系统里没有时**退回内置那只**：`remeasure_at` 拿不到
    /// 测量后端就整个不跑，行高、单元格、光标会一起停在旧字体上 —— 一个打错
    /// 的字面名不该换来这种局面。字面名只有启动时读一次，这行 stderr 不会刷屏。
    pub fn new(dpi: u32, desc: &EditorFace) -> Option<Self> {
        if let Some(shaper) = Self::with_family(dpi, desc, &desc.name) {
            return Some(shaper);
        }
        if desc.name != EDITOR_FACE {
            eprintln!(
                "[theme] 字体 {:?} 没装，编辑器退回 {EDITOR_FACE}",
                desc.name
            );
            return Self::with_family(dpi, desc, EDITOR_FACE);
        }
        None
    }

    fn with_family(dpi: u32, desc: &EditorFace, family_name: &str) -> Option<Self> {
        unsafe {
            let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            let mut collection = None;
            factory.GetSystemFontCollection(&mut collection, false).ok()?;
            let collection = collection?;
            let family = HSTRING::from(family_name);
            let mut index = 0u32;
            let mut exists = BOOL(0);
            collection
                .FindFamilyName(PCWSTR::from_raw(family.as_ptr()), &mut index, &mut exists)
                .ok()?;
            if !exists.as_bool() {
                return None;
            }
            let font = collection
                .GetFontFamily(index)
                .ok()?
                .GetFirstMatchingFont(
                    DWRITE_FONT_WEIGHT(i32::from(desc.weight)),
                    DWRITE_FONT_STRETCH_NORMAL,
                    if desc.italic {
                        DWRITE_FONT_STYLE_ITALIC
                    } else {
                        DWRITE_FONT_STYLE_NORMAL
                    },
                )
                .ok()?;
            let face = font.CreateFontFace().ok()?;
            let mut metrics = DWRITE_FONT_METRICS::default();
            face.GetMetrics(&mut metrics);
            if metrics.designUnitsPerEm == 0 {
                return None;
            }
            let face_color = is_color_face(&face);
            let em_px = pixel_height(dpi, desc.points).max(1) as f32;
            let units = metrics.designUnitsPerEm as f32;
            // GDI's `tmHeight` is the ascent+descent cell, rounded to the
            // device pixel grid the same way the glyph advances are.
            let tm_height = (((metrics.ascent as f32 + metrics.descent as f32) * em_px / units) + 0.5)
                .max(1.0) as i32;
            let ascent_px = metrics.ascent as f32 * em_px / units;
            // IDWriteFactory2 is what carries both `GetSystemFontFallback` and
            // `TranslateColorGlyphRun`; a system without it still measures,
            // it just cannot resolve missing glyphs or draw colour layers.
            let factory2 = factory.cast::<IDWriteFactory2>().ok();
            let fallback = factory2
                .as_ref()
                .and_then(|f| f.GetSystemFontFallback().ok());
            Some(Self {
                factory,
                factory2,
                fallback,
                faces: vec![face],
                units: vec![units],
                ems: vec![em_px],
                cjk_face_key: None,
                color: vec![face_color],
                family_name: family,
                collection,
                family_index: index,
                base_weight: i32::from(desc.weight),
                style_faces: [usize::MAX; 4],
                face_key_by_family: HashMap::new(),
                face_key_by_cp: HashMap::new(),
                advance_by_cp: HashMap::new(),
                em_px,
                ascent_px,
                tm_height,
            })
        }
    }

    /// 挂上「对中日韩字符使用单独的字体」的第二只字体：字面/字重/斜体整行
    /// 来自〔设置字体〕`CJKFont=` 那行（见 `gdi::cjk_face`），字号是它自己的。
    ///
    /// 字面系统里没装、或建不出 face 时**只**放弃第二只 —— 一行 stderr 之后
    /// `cjk_face_key` 保持 `None`，主字体的 shaper 原样工作，U+2000 以上的字
    /// 继续走系统回退。第二只字体建不起来不该连累整个编辑器停下来。
    pub fn with_cjk(mut self, dpi: u32, cjk: &EditorFace) -> Self {
        if self.build_cjk_face(dpi, cjk).is_none() {
            eprintln!(
                "[font] 中日韩第二字体 {:?} 没装或建不出来，继续用系统回退",
                cjk.name
            );
        }
        self
    }

    fn build_cjk_face(&mut self, dpi: u32, cjk: &EditorFace) -> Option<()> {
        unsafe {
            let family = HSTRING::from(cjk.name.as_str());
            let mut index = 0u32;
            let mut exists = BOOL(0);
            self.collection
                .FindFamilyName(PCWSTR::from_raw(family.as_ptr()), &mut index, &mut exists)
                .ok()?;
            if !exists.as_bool() {
                return None;
            }
            let font = self
                .collection
                .GetFontFamily(index)
                .ok()?
                .GetFirstMatchingFont(
                    DWRITE_FONT_WEIGHT(i32::from(cjk.weight)),
                    DWRITE_FONT_STRETCH_NORMAL,
                    if cjk.italic {
                        DWRITE_FONT_STYLE_ITALIC
                    } else {
                        DWRITE_FONT_STYLE_NORMAL
                    },
                )
                .ok()?;
            let face = font.CreateFontFace().ok()?;
            let mut metrics = DWRITE_FONT_METRICS::default();
            face.GetMetrics(&mut metrics);
            if metrics.designUnitsPerEm == 0 {
                return None;
            }
            let em = pixel_height(dpi, cjk.points).max(1) as f32;
            let face_color = is_color_face(&face);
            self.cjk_face_key = Some(self.faces.len());
            self.faces.push(face);
            self.units.push(metrics.designUnitsPerEm as f32);
            self.ems.push(em);
            self.color.push(face_color);
            Some(())
        }
    }

    /// The face a run's `face_key` names.
    pub fn face(&self, key: usize) -> &IDWriteFontFace {
        &self.faces[key.min(self.faces.len() - 1)]
    }

    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    pub fn factory2(&self) -> Option<&IDWriteFactory2> {
        self.factory2.as_ref()
    }

    /// Whether a face carries colour glyphs (a bitmap/COLR/CPAL font): only
    /// those runs need the colour-layer translation at draw time.
    pub fn is_color(&self, key: usize) -> bool {
        self.color.get(key).copied().unwrap_or(false)
    }

    /// The em this face draws at: the draw half asks per run, because the
    /// second font carries its own point size while every other face follows
    /// the editor's.
    pub fn em_of(&self, key: usize) -> f32 {
        self.ems.get(key).copied().unwrap_or(self.em_px)
    }

    /// 「对中日韩字符使用单独的字体」第二只字体的 face 下标；没生效是
    /// `None`（见 [`Self::with_cjk`]）。
    pub fn cjk_face_key(&self) -> Option<usize> {
        self.cjk_face_key
    }

    /// Resolve which face draws `ch`, caching per code point: `MapCharacters`
    /// is the expensive part, not the glyph lookup.
    pub fn face_key(&mut self, ch: char) -> usize {
        let cp = ch as u32;
        if let Some(&key) = self.face_key_by_cp.get(&cp) {
            return key;
        }
        let key = self.resolve_face_key(ch);
        self.face_key_by_cp.insert(cp, key);
        key
    }

    /// 选 face 的规则，cp 的纯函数（两个按 cp 的缓存因此保持正确）：
    ///
    /// 勾上「对中日韩字符使用单独的字体」后，U+2000 以上的字符先问第二只
    /// 字体 —— 与 [`CachedMeasurer`](dg_layout::measure::CachedMeasurer) 给
    /// 字符选字体的谓词是同一条，所以换脸不动网格。第二只没有的字形（彩色
    /// emoji 是大头）落回主字体 / 系统回退，那些字的画法与没勾时完全一致。
    fn resolve_face_key(&mut self, ch: char) -> usize {
        let cp = ch as u32;
        if cp >= 0x2000 && let Some(key) = self.cjk_face_key {
            let mut glyph = 0u16;
            if unsafe { self.faces[key].GetGlyphIndices(&cp, 1, &mut glyph) }.is_ok()
                && glyph != 0
            {
                return key;
            }
        }
        let mut glyph = 0u16;
        if unsafe { self.faces[0].GetGlyphIndices(&cp, 1, &mut glyph) }.is_ok() && glyph != 0 {
            return 0;
        }
        self.fallback_key(ch)
    }

    /// DirectWrite's fallback answer for one code point: the family it picked
    /// (cached as a face) or the editor face when it picked nothing.
    fn fallback_key(&mut self, ch: char) -> usize {
        let Some(fallback) = self.fallback.as_ref() else {
            return 0;
        };
        let mut buf = [0u16; 2];
        let utf16 = ch.encode_utf16(&mut buf).to_vec();
        let len = utf16.len() as u32;
        let source: IDWriteTextAnalysisSource = RunText {
            utf16,
            locale: "en-us\0".encode_utf16().collect(),
        }
        .into();
        let mut mapped_length = 0u32;
        let mut mapped: Option<IDWriteFont> = None;
        let mut scale = 1.0f32;
        let mapped_ok = unsafe {
            fallback
                .MapCharacters(
                    &source,
                    0,
                    len,
                    None::<&IDWriteFontCollection>,
                    &self.family_name,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    &mut mapped_length,
                    &mut mapped,
                    &mut scale,
                )
                .is_ok()
        };
        if !mapped_ok {
            return 0;
        }
        match mapped {
            Some(font) => self.face_for_font(&font),
            None => 0,
        }
    }

    fn face_for_font(&mut self, font: &IDWriteFont) -> usize {
        let Some(name) = family_name(font) else {
            return 0;
        };
        if let Some(&key) = self.face_key_by_family.get(&name) {
            return key;
        }
        let Ok(face) = (unsafe { font.CreateFontFace() }) else {
            return 0;
        };
        let mut metrics = DWRITE_FONT_METRICS::default();
        unsafe { face.GetMetrics(&mut metrics) };
        if metrics.designUnitsPerEm == 0 {
            return 0;
        }
        let face_color = is_color_face(&face);
        self.faces.push(face);
        self.units.push(metrics.designUnitsPerEm as f32);
        self.ems.push(self.em_px);
        self.color.push(face_color);
        let key = self.faces.len() - 1;
        self.face_key_by_family.insert(name, key);
        key
    }

    /// 字形位 → 画它用的 face 下标。`style & 3` 只有两位：粗体、斜体。
    ///
    /// 变体按**同一家族**再查一次系统集合（`GetFirstMatchingFont` 会给出
    /// 最接近的那一个，家族没有粗体时就是常规体 —— 不会画不出来）。建好的
    /// face 进 `faces`，和回退 face 共用一个池子，`ShapedRun::face_key`
    /// 对两者一视同仁。
    fn style_face(&mut self, style: u8) -> usize {
        let slot = (style & 3) as usize;
        if slot == 0 {
            return 0;
        }
        if self.style_faces[slot] != usize::MAX {
            return self.style_faces[slot];
        }
        let key = self.build_style_face(slot).unwrap_or(0);
        self.style_faces[slot] = key;
        key
    }

    fn build_style_face(&mut self, slot: usize) -> Option<usize> {
        let bold = slot & 1 != 0;
        let italic = slot & 2 != 0;
        unsafe {
            let family = self.collection.GetFontFamily(self.family_index).ok()?;
            let font = family
                .GetFirstMatchingFont(
                    DWRITE_FONT_WEIGHT(if bold { 700 } else { self.base_weight }),
                    DWRITE_FONT_STRETCH_NORMAL,
                    if italic {
                        DWRITE_FONT_STYLE_ITALIC
                    } else {
                        DWRITE_FONT_STYLE_NORMAL
                    },
                )
                .ok()?;
            let face = font.CreateFontFace().ok()?;
            let mut metrics = DWRITE_FONT_METRICS::default();
            face.GetMetrics(&mut metrics);
            if metrics.designUnitsPerEm == 0 {
                return None;
            }
            let face_color = is_color_face(&face);
            self.faces.push(face);
            self.units.push(metrics.designUnitsPerEm as f32);
            self.ems.push(self.em_px);
            self.color.push(face_color);
            Some(self.faces.len() - 1)
        }
    }

    /// 带字形位的 face 选择：编辑器那只字面换变体，回退 face 不换 ——
    /// 一个 emoji 或中日韩字形没有粗体这一说，硬换只会换成方框。
    fn face_key_styled(&mut self, ch: char, style: u8) -> usize {
        let key = self.face_key(ch);
        if key != 0 || style & 3 == 0 {
            return key;
        }
        self.style_face(style)
    }

    /// One code point's grid advance, on whichever face resolves it: the
    /// measure half of what [`Self::shape`] draws. A modifier has none — see
    /// [`attaches_to_previous`].
    pub fn advance(&mut self, ch: char) -> u8 {
        if attaches_to_previous(ch) {
            return 0;
        }
        let cp = ch as u32;
        if let Some(&advance) = self.advance_by_cp.get(&cp) {
            return advance;
        }
        let advance = self.measure_advance(ch);
        self.advance_by_cp.insert(cp, advance);
        advance
    }

    fn measure_advance(&mut self, ch: char) -> u8 {
        let key = self.face_key(ch);
        let units = self.units[key];
        // 第二只字体带着自己的字号，advance 得按它自己的 em 折算。
        let em = self.ems[key];
        let glyph = self.glyph(key, ch);
        if glyph == 0 {
            return 0;
        }
        let mut metrics = DWRITE_GLYPH_METRICS::default();
        if unsafe { self.faces[key].GetDesignGlyphMetrics(&glyph, 1, &mut metrics, false) }.is_err() {
            return 0;
        }
        let px = (metrics.advanceWidth as f32 * em / units) + 0.5;
        px.max(0.0).min(254.0) as u8
    }

    /// A glyph index for one character on one face.
    ///
    /// Deliberately *not* memoised here. A
    /// `HashMap<(usize, char), u16>` was tried, on the theory that one `GetGlyphIndices`
    /// per character per frame was the row pass's cost. Measured, it is not:
    /// DirectWrite keeps its own glyph-index cache per face, so the call is
    /// already a lookup, and adding ours on top measured 54 ns/char without it
    /// against 57 ns/char with it (`glyph_shaping_costs`). The cache was
    /// removed rather than kept behind a switch.
    fn glyph(&self, key: usize, ch: char) -> u16 {
        let cp = ch as u32;
        let mut glyph = 0u16;
        unsafe {
            let _ = self.faces[key].GetGlyphIndices(&cp, 1, &mut glyph);
        }
        glyph
    }

    /// Splits `text` into face runs, each glyph carrying the editor's own
    /// advance (`x` below the U+2000 boundary, `wide` from there up). The grid is the
    /// contract with [`dg_render`]'s `LineMetrics`: a run's glyphs must land
    /// on the columns the caret and hit-test use, even where the face's design
    /// advance differs. `measured` is the other half of that contract — the
    /// characters the layout measured off the grid (a colour emoji's real
    /// advance), which take that number here too.
    ///
    /// Control characters never reach a text run (the row splitter cuts at
    /// tabs), but a document can hold other controls: they take the grid
    /// advance and the space glyph, so the text after them stays aligned.
    ///
    /// A modifier ([`attaches_to_previous`]) is not a glyph and not a cell: it
    /// is skipped, so the run continues across it and the glyph it modifies
    /// keeps its place. Handing it to `GetGlyphIndices` is what drew a
    /// missing-glyph box after every emoji.
    ///
    /// `style` picks the **face** only. The advance of every character stays
    /// the layout grid's, so a bold or italic run lands on exactly the columns
    /// the caret and the wrap array computed — bold changes the outlines, not
    /// one pixel of the layout.
    pub fn shape(
        &mut self,
        text: &str,
        style: u8,
        x: f32,
        wide: f32,
        measured: &[(char, i32)],
    ) -> Vec<ShapedRun> {
        let mut runs: Vec<ShapedRun> = Vec::new();
        for ch in text.chars() {
            if attaches_to_previous(ch) {
                continue;
            }
            let cp = ch as u32;
            let key = self.face_key_styled(ch, style);
            let advance = match measured.binary_search_by_key(&ch, |&(c, _)| c) {
                Ok(i) => measured[i].1 as f32,
                Err(_) => {
                    if cp >= 0x2000 {
                        wide
                    } else {
                        x
                    }
                }
            };
            let glyph = if cp < 0x20 {
                self.glyph(key, ' ')
            } else {
                self.glyph(key, ch)
            };
            match runs.last_mut() {
                Some(run) if run.face_key == key => {
                    run.glyphs.push(glyph);
                    run.advances.push(advance);
                }
                _ => runs.push(ShapedRun {
                    face_key: key,
                    glyphs: vec![glyph],
                    advances: vec![advance],
                }),
            }
        }
        runs
    }
}

fn is_color_face(face: &IDWriteFontFace) -> bool {
    unsafe {
        face.cast::<IDWriteFontFace2>()
            .map(|f| f.IsColorFont().as_bool())
            .unwrap_or(false)
    }
}

fn family_name(font: &IDWriteFont) -> Option<String> {
    unsafe {
        let family = font.GetFontFamily().ok()?;
        let names = family.GetFamilyNames().ok()?;
        let len = names.GetStringLength(0).ok()? as usize;
        let mut buf = vec![0u16; len + 1];
        names.GetString(0, &mut buf).ok()?;
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

pub struct DWriteMeasurer {
    shaper: TextShaper,
    avg: u8,
}

impl DWriteMeasurer {
    /// `cjk` 是「对中日韩字符使用单独的字体」的第二只（`gdi::cjk_face`
    /// 解析出的那只，字号已按视图缩放）；`None` 或字面没装时就是单字体。
    pub fn new(dpi: u32, face: &EditorFace, cjk: Option<&EditorFace>) -> Option<Self> {
        let mut shaper = TextShaper::new(dpi, face)?;
        if let Some(cjk) = cjk {
            shaper = shaper.with_cjk(dpi, cjk);
        }
        let sample = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let sum: u32 = sample
            .chars()
            .map(|c| shaper.advance(c) as u32)
            .sum();
        let avg = ((sum / sample.chars().count() as u32).max(1)).min(255) as u8;
        Some(Self { shaper, avg })
    }

    /// The font cell height (`tmHeight` in the GDI path).
    pub fn font_height(&self) -> i32 {
        self.shaper.tm_height
    }

    /// The row pitch: `tmHeight + 1`, the same `+ 1` the GDI path adds.
    pub fn line_height(&self) -> i32 {
        self.shaper.tm_height + 1
    }

    /// Where P2 puts the baseline inside the row.
    pub fn ascent_px(&self) -> f32 {
        self.shaper.ascent_px
    }

    /// The draw path's half: the shaper owns the faces the measurer resolved.
    pub fn shaper(&mut self) -> &mut TextShaper {
        &mut self.shaper
    }
}

impl TextMeasurer for DWriteMeasurer {
    fn glyph_width(&mut self, ch: char, _font: Font) -> u8 {
        // Layout characters (tab above all) are the layout's business, not a
        // glyph's; the GDI path measures them as zero too.
        if (ch as u32) < 0x20 {
            return 0;
        }
        self.shaper.advance(ch)
    }

    fn measure_run(&mut self, s: &str, _font: Font) -> f32 {
        s.chars().map(|c| self.char_width(c, Font::Primary) as f32).sum()
    }

    fn avg_width(&self, _font: Font) -> u8 {
        self.avg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win32::gdi::GdiMeasurer;
    use dg_syntax::{STYLE_BOLD, STYLE_ITALIC};

    /// 内置那只字体（`Consolas` 10 pt）—— 这些用例比的是测量后端，不是皮肤。
    fn builtin() -> EditorFace {
        EditorFace::builtin(crate::win32::gdi::EDITOR_POINTS)
    }

    fn samples() -> Vec<char> {
        let mut v: Vec<char> = (0x20u32..=0x7E).filter_map(char::from_u32).collect();
        v.extend(['\t', '\u{4E00}', '\u{4E8C}', '\u{56FD}', '\u{3042}', '\u{30A2}',
                  '\u{FF21}', '\u{FF0C}', '\u{00E9}', '\u{0416}', '\u{0391}',
                  '\u{2500}', '\u{2605}', '\u{1F600}']);
        v
    }

    /// What has to hold across DPIs: the ASCII cell (the editor grid), the row
    /// metrics and the average width. The GDI path draws its cells at the same
    /// numbers, so a backend swap cannot move a row or a column.
    #[test]
    fn dwrite_ascii_and_heights_match_gdi() {
        for dpi in [96u32, 120, 144, 192] {
            let Some(mut gdi) = GdiMeasurer::new(dpi, crate::win32::gdi::EDITOR_POINTS) else {
                eprintln!("no GDI measurer at {dpi} dpi; skipping");
                return;
            };
            let Some(mut dw) = DWriteMeasurer::new(dpi, &builtin(), None) else {
                eprintln!("no DWrite measurer at {dpi} dpi; skipping");
                return;
            };
            println!(
                "dpi {dpi}: gdi tm={} line={} x={} | dwrite tm={} line={} x={} cjk={} ascent={:.2}",
                gdi.font_height(),
                gdi.line_height(),
                gdi.char_width('x', Font::Primary),
                dw.font_height(),
                dw.line_height(),
                dw.char_width('x', Font::Primary),
                dw.char_width('\u{4E00}', Font::Primary),
                dw.ascent_px()
            );
            assert_eq!(dw.font_height(), gdi.font_height(), "font_height at {dpi} dpi");
            assert_eq!(dw.line_height(), gdi.line_height(), "line_height at {dpi} dpi");
            assert_eq!(dw.avg_width(Font::Primary), gdi.avg_width(Font::Primary),
                       "avg_width at {dpi} dpi");
            for ch in (0x20u32..=0x7E).filter_map(char::from_u32) {
                assert_eq!(
                    dw.char_width(ch, Font::Primary),
                    gdi.char_width(ch, Font::Primary),
                    "ASCII U+{:04X} at {dpi} dpi",
                    ch as u32
                );
            }
            // Layout characters carry no glyph advance in either backend.
            assert_eq!(dw.char_width('\t', Font::Primary), 0);
        }
    }

    /// Informational: the wide/symbol characters deliberately take DirectWrite's
    /// advances (the same ones the glyph runs will be drawn with), not GDI's
    /// font-linking plus even quantization. Recorded so the divergence from the
    /// GDI grid is visible, not accidental.
    #[test]
    fn dwrite_wide_char_widths_are_recorded() {
        let Some(mut gdi) = GdiMeasurer::new(96, crate::win32::gdi::EDITOR_POINTS) else {
            return;
        };
        let Some(mut dw) = DWriteMeasurer::new(96, &builtin(), None) else {
            return;
        };
        for ch in samples() {
            let g = gdi.char_width(ch, Font::Primary);
            let d = dw.char_width(ch, Font::Primary);
            if g != d {
                println!("U+{:04X} gdi={g} dwrite={d}", ch as u32);
            }
            assert!(d <= 254);
        }
    }

    #[test]
    fn dwrite_avg_matches_gdi() {
        let Some(gdi) = GdiMeasurer::new(120, crate::win32::gdi::EDITOR_POINTS) else {
            return;
        };
        let Some(dw) = DWriteMeasurer::new(120, &builtin(), None) else {
            return;
        };
        println!("avg gdi={} dwrite={}", gdi.avg_width(Font::Primary), dw.avg_width(Font::Primary));
        assert_eq!(gdi.avg_width(Font::Primary), dw.avg_width(Font::Primary));
    }

    /// A character the layout measured off the grid is drawn on that number
    /// too: the row walk and the glyph run must agree about the emoji's width,
    /// or the caret lands inside the coloured bitmap.
    #[test]
    fn dwrite_shape_honours_the_measured_advance() {
        let Some(mut dw) = DWriteMeasurer::new(96, &builtin(), None) else {
            return;
        };
        let shaper = dw.shaper();
        let emoji = '\u{1F600}';
        let real = shaper.advance(emoji) as i32;
        assert!(real > 14, "the emoji is wider than the CJK cell: {real}");
        let measured = [(emoji, real)];
        let total: f32 = shaper
            .shape("a\u{1F600}b", 0, 7.0, 14.0, &measured)
            .iter()
            .flat_map(|r| r.advances.iter())
            .sum();
        assert_eq!(total, 7.0 + real as f32 + 7.0);
        // Without the entry the same string keeps the grid: the table is what
        // moves the glyph, not the face lookup on its own.
        let total: f32 = shaper
            .shape("a\u{1F600}b", 0, 7.0, 14.0, &[])
            .iter()
            .flat_map(|r| r.advances.iter())
            .sum();
        assert_eq!(total, 7.0 + 14.0 + 7.0);
    }

    /// 字形位换的是**字面**，不是网格：同一段文字在正体 / 粗体 / 斜体下
    /// 推进量逐字相同，只有 `face_key` 不同。这是"加粗不会挪动任何一列"
    /// 的证据 —— 布局、光标、命中测试读的都是推进量。
    #[test]
    fn style_bits_swap_the_face_not_the_grid() {
        let Some(mut dw) = DWriteMeasurer::new(96, &builtin(), None) else {
            eprintln!("no DirectWrite here; skipping");
            return;
        };
        let shaper = dw.shaper();
        let advances = |runs: &[ShapedRun]| -> Vec<f32> {
            runs.iter()
                .flat_map(|r| r.advances.iter().copied())
                .collect()
        };
        let plain = shaper.shape("if x", 0, 7.0, 14.0, &[]);
        let bold = shaper.shape("if x", STYLE_BOLD, 7.0, 14.0, &[]);
        let italic = shaper.shape("if x", STYLE_ITALIC, 7.0, 14.0, &[]);
        let both = shaper.shape("if x", STYLE_BOLD | STYLE_ITALIC, 7.0, 14.0, &[]);
        for (name, runs) in [("粗体", &bold), ("斜体", &italic), ("粗斜", &both)] {
            assert_eq!(advances(&plain), advances(runs), "{name} 的推进量必须和正体一致");
            assert_eq!(runs.len(), 1, "{name} 是同一张字面，一条 run");
        }
        assert_ne!(bold[0].face_key, plain[0].face_key, "粗体换了字面");
        assert_ne!(italic[0].face_key, plain[0].face_key, "斜体换了字面");
        assert_ne!(both[0].face_key, bold[0].face_key, "粗斜是第三张字面");
        assert_ne!(both[0].face_key, italic[0].face_key, "粗斜也是另一张");
        // 回退 face 不跟着换：一个 emoji 没有粗体这一说，硬换只会换成方框。
        let cjk_plain = shaper.shape("\u{4E00}", 0, 7.0, 14.0, &[]);
        let cjk_bold = shaper.shape("\u{4E00}", STYLE_BOLD, 7.0, 14.0, &[]);
        assert_ne!(cjk_plain[0].face_key, 0, "CJK 走回退");
        assert_eq!(
            cjk_plain[0].face_key, cjk_bold[0].face_key,
            "回退 face 的原样保留"
        );
    }

    /// 「对中日韩字符使用单独的字体」勾上后的路由：U+2000 以上的字先问
    /// 第二只字体（`CJKFont=` 那行），ASCII 还是编辑器那只，第二只没有的
    /// 字形（彩色 emoji）落回系统回退，字形位也换不动第二只。网格照旧：
    /// 换脸不动任何一列。
    #[test]
    fn the_cjk_face_takes_the_wide_range() {
        let cjk = EditorFace {
            name: "Microsoft YaHei".into(),
            points: 12.0,
            weight: 400,
            italic: false,
        };
        let Some(mut dw) = DWriteMeasurer::new(96, &builtin(), Some(&cjk)) else {
            eprintln!("no DirectWrite here; skipping");
            return;
        };
        let shaper = dw.shaper();
        let Some(cjk_key) = shaper.cjk_face_key() else {
            panic!("Microsoft YaHei 是系统字体，第二只应当建得起来");
        };
        assert_ne!(cjk_key, 0);
        assert_ne!(shaper.em_of(cjk_key), shaper.em_of(0), "第二只带自己的字号");
        for ch in ['\u{4E00}', '\u{3042}', '\u{30A2}', '\u{FF21}'] {
            assert_eq!(shaper.face_key(ch), cjk_key, "U+{:04X} 走第二只", ch as u32);
        }
        // 第二只没有的字形（雅黑没有谚文）退回系统回退 —— 勾选不是把整个
        // U+2000 以上按死在第二只上，而是「先问它，没有再走老路」。
        assert_ne!(shaper.face_key('\u{AC00}'), cjk_key, "谚文走回退");
        assert_eq!(shaper.face_key('a'), 0, "ASCII 仍是编辑器那只");
        assert_ne!(
            shaper.face_key('\u{1F600}'),
            cjk_key,
            "彩色 emoji 不在第二只上"
        );
        assert_eq!(
            shaper.face_key('\u{4E00}'),
            shaper.face_key_styled('\u{4E00}', STYLE_BOLD),
            "第二只不吃语法加粗"
        );
        let runs = shaper.shape("a\u{4E00}\u{3042}", 0, 7.0, 14.0, &[]);
        let total: f32 = runs.iter().flat_map(|r| r.advances.iter()).sum();
        assert_eq!(total, 7.0 + 14.0 + 14.0, "推进量还是布局网格的");
        assert!(runs.iter().flat_map(|r| r.glyphs.iter()).all(|&g| g != 0));
        assert!(dw.char_width('\u{4E00}', Font::CJK) > 0);
    }

    /// 第二只字面没装：一行 stderr 之后照常工作，路由与没配时一致
    /// （`cjk_face_key` 是 `None`，U+2000 以上走回退）。
    #[test]
    fn a_missing_cjk_face_degrades_to_the_system_fallback() {
        let cjk = EditorFace {
            name: "No Such Face 0xE1F".into(),
            points: 12.0,
            weight: 400,
            italic: false,
        };
        let Some(mut dw) = DWriteMeasurer::new(96, &builtin(), Some(&cjk)) else {
            eprintln!("no DirectWrite here; skipping");
            return;
        };
        let shaper = dw.shaper();
        assert!(shaper.cjk_face_key().is_none());
        let Some(mut plain) = DWriteMeasurer::new(96, &builtin(), None) else {
            eprintln!("no DirectWrite here; skipping");
            return;
        };
        assert_eq!(
            shaper.face_key('\u{4E00}'),
            plain.shaper().face_key('\u{4E00}'),
            "第二只缺席时的路由和没配一样"
        );
        assert!(dw.char_width('\u{4E00}', Font::CJK) > 0);
    }

    /// The draw half's contract: one run per face, every glyph real, and the
    /// advances summing to the grid the layout walked — not to the faces'
    /// design advances, which would slide the caret off the text.
    #[test]
    fn dwrite_shape_keeps_the_grid_and_splits_faces() {
        let Some(mut dw) = DWriteMeasurer::new(96, &builtin(), None) else {
            return;
        };
        let shaper = dw.shaper();
        let runs = shaper.shape("ab\u{4E00}\u{1F600}", 0, 7.0, 14.0, &[]);
        let total: f32 = runs.iter().flat_map(|r| r.advances.iter()).sum();
        println!(
            "shape runs: {:?}",
            runs.iter().map(|r| (r.face_key, r.glyphs.len())).collect::<Vec<_>>()
        );
        assert_eq!(total, 7.0 + 7.0 + 14.0 + 14.0, "grid advances sum");
        assert_eq!(runs[0].face_key, 0);
        assert_eq!(runs[0].glyphs.len(), 2);
        assert!(
            runs.iter().any(|r| r.face_key != 0),
            "CJK/emoji must resolve through the fallback"
        );
        assert!(
            runs.iter().flat_map(|r| r.glyphs.iter()).all(|&g| g != 0),
            "every shaped glyph exists on its face"
        );
        // The emoji is its own face: no sharing with the CJK fallback.
        let emoji = runs.last().unwrap();
        assert_eq!(emoji.glyphs.len(), 1);
        assert_ne!(emoji.face_key, 0);
        assert_eq!(emoji.advances[0], 14.0);
    }

    /// `cargo test --release -p dg-app --lib glyph_shaping_costs -- --ignored --nocapture`
    ///
    /// What shaping a screenful costs, and the measurement that killed the glyph
    /// cache.
    ///
    /// The proposal was a `HashMap<(usize face, char), u16>` in front of
    /// `GetGlyphIndices`, on the theory that the row pass pays one COM call per
    /// character per frame. It does — but DirectWrite already keeps that lookup
    /// memoised per face, so a second map on top measures *slower* than doing
    /// nothing: 54 ns/char without, 57 ns/char with, on this machine. Hence no
    /// cache, and this test to stop the next person from adding one.
    ///
    /// Timed in-process on purpose. The same question asked from outside the
    /// process — total CPU around a flood of wheel messages, which is what
    /// an external measurement does — swings by 12x between identical
    /// runs, because the shaping is a few percent of what a message flood
    /// costs. That bench cannot resolve this class of change; this can.
    #[test]
    #[ignore = "bench, run explicitly with --ignored --release"]
    fn glyph_shaping_costs() {
        let Some(mut dw) = DWriteMeasurer::new(96, &builtin(), None) else {
            return;
        };
        let shaper = dw.shaper();

        // A 900x640 window: mostly ASCII, with the CJK line a document like
        // this has every few dozen rows.
        let ascii = "    let mut runs: Vec<ShapedRun> = Vec::new();  // 0123456789";
        let cjk = "    编辑器光标闪烁渲染性能测试文档滚动基准形状测量";
        let rows: Vec<&str> = (0..40)
            .map(|i| if i % 8 == 0 { cjk } else { ascii })
            .collect();
        let frames = 40;

        let mut run = || {
            let mut glyphs = 0usize;
            let t = std::time::Instant::now();
            for _ in 0..frames {
                for r in &rows {
                    glyphs += shaper
                        .shape(r, 0, 7.0, 14.0, &[])
                        .iter()
                        .map(|x| x.glyphs.len())
                        .sum::<usize>();
                }
            }
            (t.elapsed(), glyphs)
        };

        let _ = run(); // faces, fallback resolution, first-touch pages
        let mut runs = Vec::new();
        let mut glyphs = 0;
        for _ in 0..3 {
            let (d, n) = run();
            runs.push(d);
            glyphs = n;
        }
        runs.sort();

        let chars = (frames * rows.iter().map(|r| r.chars().count()).sum::<usize>()) as f64;
        let per_frame = rows.iter().map(|r| r.chars().count()).sum::<usize>() as f64;
        let per = |d: std::time::Duration| d.as_secs_f64() * 1e9 / chars;
        let med = runs[runs.len() / 2];
        println!(
            "shape: {:.0} ns/char  ({:.0} us per screenful of {:.0} chars)  glyphs={glyphs}  runs: {}",
            per(med),
            per(med) * per_frame / 1000.0,
            per_frame,
            runs.iter().map(|d| format!("{:.0}", per(*d))).collect::<Vec<_>>().join(", ")
        );
        assert_eq!(glyphs, 90800, "the bench shapes the same text every time");
        // The number above is the contract: shaping a screenful is tens of
        // microseconds, not milliseconds. A regression that changes the order
        // of magnitude is what this catches.
        assert!(
            per(med) < 500.0,
            "shaping got an order of magnitude slower: {:.0} ns/char",
            per(med)
        );
    }

    /// U+FE0F is a modifier, not a character: it asks for the emoji
    /// presentation of the code point before it and has no glyph of its own.
    /// `☕` is U+2615 alone while `☕️` is U+2615 followed by U+FE0F, so they are
    /// different strings, and the
    /// grid must not be able to tell them apart — the selector draws nothing,
    /// so a cell for it slides everything after it one column to the right.
    ///
    /// Measured before the fix, at 96 dpi: `a☕️b` shaped to four glyphs and
    /// 42 px against `a☕b`'s three glyphs and 28 px, the extra 14 px being the
    /// wide cell the selector was handed and the extra glyph being index 0 —
    /// a missing-glyph box, drawn after every emoji in the document.
    #[test]
    fn a_variation_selector_is_not_a_glyph() {
        let Some(mut dw) = DWriteMeasurer::new(96, &builtin(), None) else {
            return;
        };
        let shaper = dw.shaper();
        let mut shape_of = |s: &str| -> (usize, f32) {
            let runs = shaper.shape(s, 0, 7.0, 14.0, &[]);
            (
                runs.iter().map(|r| r.glyphs.len()).sum(),
                runs.iter().flat_map(|r| r.advances.iter()).sum(),
            )
        };
        let plain = shape_of("a\u{2615}b");
        assert_eq!(plain.0, 3, "a, the coffee, b");
        for s in ["a\u{2615}\u{FE0F}b", "a\u{2615}\u{FE0E}b"] {
            assert_eq!(shape_of(s), plain, "{s:?} lays out like the bare base");
        }
        // The keycap is the same story: `1️⃣` is `1` + VS16 + U+20E3, and
        // neither modifier may take a cell of its own.
        assert_eq!(shape_of("1\u{FE0F}\u{20E3}b"), shape_of("1b"));
        // No run may carry the missing glyph, and asking for a modifier's
        // advance must not reach the font at all.
        for s in ["a\u{2615}\u{FE0F}b", "1\u{FE0F}\u{20E3}b"] {
            for run in shaper.shape(s, 0, 7.0, 14.0, &[]) {
                assert!(
                    run.glyphs.iter().all(|&g| g != 0),
                    "{s:?} drew a missing-glyph box: {:?}",
                    run.glyphs
                );
            }
        }
        assert_eq!(shaper.advance('\u{FE0F}'), 0);
        assert_eq!(shaper.advance('\u{FE0E}'), 0);
        assert_eq!(shaper.advance('\u{20E3}'), 0);
    }

    /// What the fallback chain does with a selector, recorded so nobody tries
    /// to use it for presentation: `MapCharacters` is handed `☕`, `☕️` and
    /// `☕︎` and answers `Segoe UI Emoji` for all three, the text-presentation
    /// selector included. Presentation therefore cannot be delegated to the
    /// fallback — honouring VS15/VS16 would mean naming the two families
    /// ourselves and laying the base out on a cell its emoji glyph may not fit.
    #[test]
    fn the_fallback_ignores_the_selector() {
        let Some(mut dw) = DWriteMeasurer::new(96, &builtin(), None) else {
            return;
        };
        let shaper = dw.shaper();
        let Some(fallback) = shaper.fallback.as_ref() else {
            return;
        };
        let family_of = |s: &str| -> Option<String> {
            let utf16: Vec<u16> = s.encode_utf16().collect();
            let len = utf16.len() as u32;
            let source: IDWriteTextAnalysisSource = RunText {
                utf16,
                locale: "en-us\0".encode_utf16().collect(),
            }
            .into();
            let mut mapped_length = 0u32;
            let mut mapped: Option<IDWriteFont> = None;
            let mut scale = 1.0f32;
            unsafe {
                fallback
                    .MapCharacters(
                        &source,
                        0,
                        len,
                        None::<&IDWriteFontCollection>,
                        &shaper.family_name,
                        DWRITE_FONT_WEIGHT_NORMAL,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        &mut mapped_length,
                        &mut mapped,
                        &mut scale,
                    )
                    .ok()?;
            }
            mapped.as_ref().and_then(family_name)
        };
        let emoji = family_of("\u{2615}");
        assert_eq!(family_of("\u{2615}\u{FE0F}"), emoji, "VS16 picks the same face");
        assert_eq!(family_of("\u{2615}\u{FE0E}"), emoji, "so does VS15");
    }

    /// `[fonts] mono` 换字面/字号之后，量出来的东西**真的变了** —— 行高、单元格
    /// 宽度、平均宽度三样各自都要跟着走。只变一样就是哪条路没读新字体。
    #[test]
    fn a_different_face_measures_differently() {
        let Some(small) = DWriteMeasurer::new(96, &builtin(), None) else {
            eprintln!("no DirectWrite here; skipping");
            return;
        };
        let Some(big) = DWriteMeasurer::new(
            96,
            &EditorFace {
                name: "Courier New".into(),
                points: 14.0,
                weight: 400,
                italic: false,
            },
            None,
        ) else {
            eprintln!("Courier New not installed; skipping");
            return;
        };
        assert!(
            big.font_height() > small.font_height(),
            "14pt has to be taller than 10pt: {} vs {}",
            big.font_height(),
            small.font_height()
        );
        let mut small = small;
        let mut big = big;
        assert!(
            big.glyph_width('M', Font::Primary) > small.glyph_width('M', Font::Primary),
            "the cell has to widen with the font"
        );
        assert!(
            big.avg_width(Font::Primary) > small.avg_width(Font::Primary),
            "so does the average width the scroll range is computed from"
        );
    }

    /// 字面名打错时退回内置那只，量出来的数和内置**逐个相等**。
    ///
    /// 这条守的是「一个打错的字面名不能换来一张排错的版」：`TextShaper::new`
    /// 若回 `None`，`remeasure_at` 整个不跑，行高和光标会停在旧字体上。
    #[test]
    fn an_unknown_face_falls_back_to_the_built_in_one() {
        let Some(fallback) = DWriteMeasurer::new(
            96,
            &EditorFace {
                name: "No Such Face 9000".into(),
                points: crate::win32::gdi::EDITOR_POINTS,
                weight: 400,
                italic: false,
            },
            None,
        ) else {
            eprintln!("no DirectWrite here; skipping");
            return;
        };
        let Some(built_in) = DWriteMeasurer::new(96, &builtin(), None) else {
            eprintln!("no DirectWrite here; skipping");
            return;
        };
        assert_eq!(fallback.font_height(), built_in.font_height());
        let mut fallback = fallback;
        let mut built_in = built_in;
        for ch in ['M', 'x', '\u{4E00}'] {
            assert_eq!(
                fallback.glyph_width(ch, Font::Primary),
                built_in.glyph_width(ch, Font::Primary),
                "{ch} has to come from the same face"
            );
        }
    }
}
