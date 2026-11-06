//! The editor face and the GDI pieces that remain after the renderer rewrite:
//! `CreateFontW` for the IME composition font and the dialogs. Nothing on the
//! editor's paint or measure path uses GDI any more.

use windows::Win32::Foundation::COLORREF;
#[cfg(test)]
use dg_layout::measure::{Font, TextMeasurer};
#[cfg(test)]
use windows::Win32::Foundation::SIZE;
#[cfg(test)]
use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC, GetTextExtentPoint32W, HDC};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DEFAULT_QUALITY,
    DeleteObject, FIXED_PITCH, GetDC, GetTextMetricsW, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS,
    ReleaseDC, RestoreDC, SaveDC, SelectObject, TEXTMETRICW,
};
use windows::core::PCWSTR;

/// The face the shipped `[Font0] Font=Consolas,10,0,1,0` names.
pub const EDITOR_FACE: &str = "Consolas";
/// Its point size, same source
/// 默认字体(Consolas,10) menu row.
pub const EDITOR_POINTS: f32 = 10.0;

/// `dg_render::Color` -> `COLORREF` (`0x00BBGGRR`).
///
/// This lives in a leaf module so the pure skin replay layer can use it
/// without depending on the frame/window module.
pub(super) fn to_colorref(c: dg_render::theme::Color) -> COLORREF {
    COLORREF((c.r as u32) | ((c.g as u32) << 8) | ((c.b as u32) << 16))
}

/// It loads the size in tenths of a point, scales it by `GetDeviceCaps(DC,
/// LOGPIXELSY)` and takes the absolute value. Negative `lfHeight` means "this is
/// the cell height", so the result is the magnitude.
pub fn pixel_height(dpi: u32, points: f32) -> i32 {
    let tenths = (points * 10.0).round() as i64;
    ((dpi as i64 * tenths + 360) / 720) as i32
}

/// 编辑器那只字体的完整描述：字面、字号、字重、斜体。
///
/// 绘制（[`create_editor_font`]）和测量（`DWriteMeasurer`）各拿一份**同样的**
/// 值 —— 两条路算出同一个单元格，光标才不会从自己的字形上滑下去。
///
/// 内置值就是 `Consolas` / [`EDITOR_POINTS`] / 400 / 不斜；主题 `[Fonts] mono`
/// 和〔设置字体〕的 `[FontN] Font=` 都能整套换掉它，前者更优先（见
/// [`EditorFace::current`]）。**没有「一份等于内置值的默认配置」**：没写就是
/// 没覆盖，将来内置值改了它跟着改。
#[derive(Clone, Debug, PartialEq)]
pub struct EditorFace {
    /// 字面名。空串不会出现 —— `[Fonts] mono` 只写字号时落回 [`EDITOR_FACE`]。
    pub name: String,
    /// 字号，pt。
    pub points: f32,
    /// 100–900。
    pub weight: u16,
    /// 斜体。
    pub italic: bool,
}

impl EditorFace {
    /// 内置那只：`Consolas`，指定字号，常规、不斜。
    pub fn builtin(points: f32) -> EditorFace {
        EditorFace {
            name: EDITOR_FACE.to_string(),
            points,
            weight: 400,
            italic: false,
        }
    }

    /// 当前该用的那只。优先级：**主题 `[Fonts] mono` > 〔设置字体〕
    /// `[FontN] Font=` > 内置 `Consolas`**。
    ///
    /// 与 `uifont::create_for` 同一条规矩：主题是作者给整套外观定的字体，比
    /// 用户在对话框里挑的那只更具体，两处都写时以主题为准；主题没声明
    /// `mono` 时，槽里的配置才生效。token 一旦声明就**整条**归它 —— 只写
    /// `mono.size` 时字面为空，落回 [`EDITOR_FACE`] 而不是把槽里的字面捡回来。
    ///
    /// 字号一律取 `points`（文档自己的缩放；基准见 [`base_points`]）。等宽字体
    /// 没有「系统默认」这一说 —— `NONCLIENTMETRICS` 给的是界面字体，拿它排
    /// 代码会把整张单元格网格变成比例宽度。
    pub fn current(points: f32) -> EditorFace {
        let theme = crate::skin::font_spec(dg_skin::FontToken::Mono);
        let slot = super::fontcfg::view_spec();
        face_from(theme.as_ref(), slot.as_ref(), points)
    }
}

/// 三选一的纯函数：把优先级从进程级的主题表 / 配置缓存里摘出来，测试可以
/// 直接钉住三层，不必去动两份全局状态。
fn face_from(
    theme: Option<&dg_skin::FontSpec>,
    slot: Option<&super::fontcfg::FontSpec>,
    points: f32,
) -> EditorFace {
    if let Some(spec) = theme {
        return EditorFace {
            name: if spec.face.is_empty() {
                EDITOR_FACE.to_string()
            } else {
                spec.face.clone()
            },
            points,
            weight: spec.weight,
            italic: spec.italic,
        };
    }
    if let Some(spec) = slot {
        return spec.into_face(points);
    }
    EditorFace::builtin(points)
}

/// 编辑器字号的基准，和 [`EditorFace::current`] **同一套优先级**：主题
/// `[Fonts] mono.size` > 〔设置字体〕槽里的 pt > [`EDITOR_POINTS`]。
///
/// 「原始字号」命令和 `font_points` 的清空条件都读这一个数 —— 字面和基准
/// 必须来自同一层，否则会拼出「主题的字面 + 对话框的字号」这种字体。
pub fn base_points() -> f32 {
    let theme = crate::skin::font_spec(dg_skin::FontToken::Mono);
    base_from(theme.as_ref(), super::fontcfg::base_points())
}

fn base_from(theme: Option<&dg_skin::FontSpec>, slot: Option<f32>) -> f32 {
    if let Some(spec) = theme {
        return spec.size;
    }
    slot.unwrap_or(EDITOR_POINTS)
}

/// 「对中日韩字符使用单独的字体」勾上后的第二只字体，〔设置字体〕第二页
/// （中文字体）写进 `[FontN] CJKFont=` 的那一行。
///
/// 字面/字重/斜体整行归 `CJKFont=`；字号取这一行自己的 pt，再乘视图字号与
/// 基准字号的比 —— 主字体被文档字号覆盖时第二只跟着同比例走，缩放不拆散
/// 两只字体的相对大小。这一层**没有主题参与**：`[Fonts]` token 只有 mono，
/// 第二只字体只来自对话框，没勾或没写行（`fontcfg::view_cjk` 回 `None`）
/// 就继续单字体。
pub fn cjk_face(points: f32) -> Option<EditorFace> {
    cjk_from(super::fontcfg::view_cjk().as_ref(), base_points(), points)
}

/// [`cjk_face`] 的纯函数半边：判据在这里钉住，测试不必动进程级的表。
fn cjk_from(slot: Option<&super::fontcfg::FontSpec>, base: f32, points: f32) -> Option<EditorFace> {
    let spec = slot?;
    let scale = if base >= 1.0 { points / base } else { 1.0 };
    let scaled = (spec.points as f32 * scale).max(1.0);
    Some(spec.into_face(scaled))
}

/// The editor font: the face's name/size/weight/slant, scaled by `dpi`,
/// fixed pitch, ClearType. The same face both measurers measure
/// against, so text drawn through
/// the DrawList lands on the
/// measured cell grid.
pub fn create_editor_font(dpi: u32, face: &EditorFace) -> Option<HFONT> {
    unsafe {
        let name: Vec<u16> = face.name.encode_utf16().chain([0]).collect();
        let font = CreateFontW(
            -pixel_height(dpi, face.points).max(1),
            0,
            0,
            0,
            i32::from(face.weight),
            u32::from(face.italic),
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            FIXED_PITCH.0 as u32,
            PCWSTR::from_raw(name.as_ptr()),
        );
        if font.0.is_null() { None } else { Some(font) }
    }
}

/// The folding-arrow font's size
/// with `lfHeight = 120` tenths of a point and the face `Marlett`.
const ARROW_POINTS: f32 = 12.0;

static ARROW_CELL: std::sync::Mutex<Option<(u32, i32)>> = std::sync::Mutex::new(None);

/// `tm.tmAveCharWidth` of the arrow font at `dpi`. The strip is therefore the arrow's
/// cell, which is far wider than the editor's. The number only depends on the DPI, and
/// the DPI only changes on a monitor move.
pub fn arrow_cell_width(dpi: u32) -> i32 {
    if let Ok(guard) = ARROW_CELL.lock() {
        if let Some((at, w)) = *guard {
            if at == dpi {
                return w;
            }
        }
    }
    let width = unsafe {
        // That matters: the screen DC carries the process DPI, so a memory DC
        // hands back a cell scaled for 96 DPI.
        let hdc = GetDC(None);
        if hdc.0.is_null() {
            0
        } else {
            let saved = SaveDC(hdc);
            let face: Vec<u16> = "Marlett".encode_utf16().chain([0]).collect();
            // Weight, quality and pitch stay DONTCARE.
            let font = CreateFontW(
                -pixel_height(dpi, ARROW_POINTS).max(1),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                DEFAULT_QUALITY,
                0,
                PCWSTR::from_raw(face.as_ptr()),
            );
            let old = SelectObject(hdc, HGDIOBJ(font.0));
            let mut tm = TEXTMETRICW::default();
            let _ = GetTextMetricsW(hdc, &mut tm);
            let _ = SelectObject(hdc, old);
            let _ = DeleteObject(HGDIOBJ(font.0));
            if saved != -1 {
                let _ = RestoreDC(hdc, saved);
            }
            let _ = ReleaseDC(None, hdc);
            tm.tmAveCharWidth
        }
    };
    if let Ok(mut guard) = ARROW_CELL.lock() {
        *guard = Some((dpi, width));
    }
    width
}

/// The folding strip's width: the arrow cell less 2 px, never below 12.
pub fn folding_strip_width(dpi: u32) -> f32 {
    12.0f32.max(arrow_cell_width(dpi).saturating_sub(2) as f32)
}

/// The GDI measurer, kept for the parity tests only. The editor measures and
/// draws with DirectWrite now. ASCII, row metrics and the average width — so
/// the integer cell grid stays pinned even though no production path asks GDI
/// for a width any more.
#[cfg(test)]
pub struct GdiMeasurer {
    hdc: HDC,
    font: HFONT,
    old_font: HGDIOBJ,
    avg: u8,
    tm_height: i32,
}

#[cfg(test)]
impl GdiMeasurer {
    /// Creates a memory DC and the editor font at `dpi`.
    pub fn new(dpi: u32, points: f32) -> Option<Self> {
        unsafe {
            let hdc = CreateCompatibleDC(None);
            if hdc.0.is_null() {
                return None;
            }
            let Some(font) = create_editor_font(dpi, &EditorFace::builtin(points)) else {
                let _ = DeleteDC(hdc);
                return None;
            };
            let old = SelectObject(hdc, HGDIOBJ(font.0));
            // the extent of a sample string is not the same number.
            let mut tm = TEXTMETRICW::default();
            let _ = GetTextMetricsW(hdc, &mut tm);
            let mut m = Self {
                hdc,
                font,
                old_font: old,
                avg: 8,
                tm_height: tm.tmHeight.max(1),
            };
            // average width over a representative sample
            let sample: Vec<u16> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
                .encode_utf16()
                .collect();
            let cx = m.measure_text(&sample);
            m.avg = ((cx / 52).max(1)).min(255) as u8;
            Some(m)
        }
    }

    fn measure_text(&self, utf16: &[u16]) -> i32 {
        if utf16.is_empty() {
            return 0;
        }
        let mut size = SIZE { cx: 0, cy: 0 };
        unsafe {
            let _ = GetTextExtentPoint32W(self.hdc, utf16, &mut size);
        }
        size.cx
    }

    /// The font cell height (`tmHeight`): what the
    /// ruler band is sized from, one row pitch less 1 px.
    pub fn font_height(&self) -> i32 {
        self.tm_height
    }

    /// The row pitch: `tmHeight + 1`
    /// adds to every row on top of a
    /// line-spacing setting this implementation does not model yet. Scales with the DPI, so
    /// a re-measure on `WM_DPICHANGED` keeps rows from overlapping.
    pub fn line_height(&self) -> i32 {
        self.tm_height + 1
    }
}

#[cfg(test)]
impl Drop for GdiMeasurer {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.hdc, self.old_font);
            let _ = DeleteObject(HGDIOBJ(self.font.0));
            let _ = DeleteDC(self.hdc);
        }
    }
}

#[cfg(test)]
impl TextMeasurer for GdiMeasurer {
    fn glyph_width(&mut self, ch: char, _font: Font) -> u8 {
        let mut buf = [0u16; 2];
        let s = ch.encode_utf16(&mut buf);
        self.measure_text(s).max(0).min(255) as u8
    }

    fn measure_run(&mut self, s: &str, _font: Font) -> f32 {
        let utf16: Vec<u16> = s.encode_utf16().collect();
        self.measure_text(&utf16) as f32
    }

    fn avg_width(&self, _font: Font) -> u8 {
        self.avg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_skin::FontSpec as ThemeFont;
    use super::super::fontcfg::FontSpec as SlotFont;

    fn theme(face: &str, size: f32, weight: u16) -> ThemeFont {
        ThemeFont {
            face: face.to_string(),
            size,
            weight,
            italic: false,
        }
    }

    fn slot(face: &str, points: i32) -> SlotFont {
        SlotFont {
            face: face.to_string(),
            points,
            bold: true,
            smooth: 1,
            italic: true,
        }
    }

    #[test]
    fn a_point_size_becomes_pixels_by_muldiv() {
        assert_eq!(pixel_height(96, 10.0), 13);
        assert_eq!(pixel_height(120, 10.0), 17);
        assert_eq!(pixel_height(144, 10.0), 20);
        assert_eq!(pixel_height(192, 10.0), 27);
        assert_eq!(pixel_height(96, 10.5), 14);
    }

    /// 主题声明了 `mono`：字面/字重/斜体/基准字号全归主题，〔设置字体〕那只
    /// 完全不参与。`current` / `base_points` 只是把这两份全局状态喂进来，
    /// 判据本身在这里钉住，测试不必去动进程级的表。
    #[test]
    fn theme_font_outranks_the_font_dialog() {
        let t = theme("JetBrains Mono", 12.0, 700);
        let s = slot("Georgia", 13);
        let face = face_from(Some(&t), Some(&s), 15.0);
        assert_eq!(face.name, "JetBrains Mono");
        assert_eq!(face.weight, 700);
        assert!(!face.italic, "主题的斜体也要盖掉槽里的");
        assert_eq!(face.points, 15.0, "文档缩放的字号永远由调用方给");
        assert_eq!(base_from(Some(&t), Some(13.0)), 12.0);
    }

    #[test]
    fn without_a_theme_token_the_dialog_slot_wins() {
        let s = slot("Georgia", 13);
        let face = face_from(None, Some(&s), 11.0);
        assert_eq!(face.name, "Georgia");
        assert_eq!(face.weight, 700);
        assert!(face.italic);
        assert_eq!(base_from(None, Some(13.0)), 13.0);
    }

    #[test]
    fn with_neither_layer_the_builtin_survives() {
        assert_eq!(face_from(None, None, 10.0), EditorFace::builtin(10.0));
        assert_eq!(base_from(None, None), EDITOR_POINTS);
    }

    /// 只写 `mono.size` 也算声明了 `mono`：字面回内置 `Consolas`，不把槽里的
    /// 字面捡回来 —— 否则就成了两层拼出来的字体（主题的字号 + 对话框的字面）。
    #[test]
    fn a_size_only_theme_token_owns_the_whole_face() {
        let t = theme("", 14.0, 400);
        let s = slot("Georgia", 13);
        let face = face_from(Some(&t), Some(&s), 14.0);
        assert_eq!(face.name, EDITOR_FACE);
        assert_eq!(face.weight, 400);
        assert_eq!(base_from(Some(&t), Some(13.0)), 14.0);
    }

    /// 第二只字体的字面/字重/斜体整行来自 `CJKFont=`，字号按视图与基准的比
    /// 缩放：基准 10 pt、行写 12 pt、视图缩到 20 pt 时，第二只在 24 pt 上。
    #[test]
    fn the_cjk_face_takes_the_line_and_scales_with_the_view() {
        let mut s = slot("Microsoft YaHei", 12);
        s.bold = false;
        s.italic = false;
        let face = cjk_from(Some(&s), 10.0, 20.0).unwrap();
        assert_eq!(face.name, "Microsoft YaHei");
        assert_eq!(face.weight, 400, "CJKFont= 行自己声明字重");
        assert!(!face.italic);
        assert_eq!(face.points, 24.0);
        // 没缩放（视图字号 == 基准）就是这一行自己的字号。
        assert_eq!(cjk_from(Some(&s), 10.0, 10.0).unwrap().points, 12.0);
        // 比例分母量不出来时不缩放。
        assert_eq!(cjk_from(Some(&s), 0.0, 10.0).unwrap().points, 12.0);
        // 缩出来的字号不会小于 1 pt。
        s.points = 1;
        assert_eq!(cjk_from(Some(&s), 1.0, 0.2).unwrap().points, 1.0);
    }

    /// 没勾「对中日韩字符使用单独的字体」（`fontcfg::view_cjk` 回 `None`）
    /// 就没有第二只 —— 主字体那条链完全不变。
    #[test]
    fn without_the_cjk_flag_there_is_no_second_face() {
        assert!(cjk_from(None, 10.0, 20.0).is_none());
    }
}
