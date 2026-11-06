//! The UI font, shared by the status strip, the menus and the dialogs. It reads
//! `Setting/UIFont` — the drop-down id the 选项 row writes — and either names a face
//! at 8 or 9 pt (`Tahoma`/`Segoe UI`/`Meiryo`/`Verdana`, turning the
//! tenths-of-a-point size into a cell height with `MulDiv(LOGPIXELSY, tenths,
//! 720)`) or, for `系统默认`, takes `SPI_GETNONCLIENTMETRICS`' **menu** font — the
//! `LOGFONTW` at offset 224 of the 500-byte block it passes — with its weight
//! forced to `FW_NORMAL`. That is Microsoft YaHei UI 9 pt on a Chinese desktop,
//! which is what the earlier strip measurement saw: its glyph box at 125% is YaHei
//! UI at -15 px. The row can honestly say 需重启. The fallback chain after the menu
//! font is not decoration: `cbSize` decides how much of the block the system
//! fills, and a size it does not recognise makes `SPI_GETNONCLIENTMETRICS` fail
//! outright (measured here: the pre-Vista 492-byte size returns FALSE with every
//! font zeroed). The icon-title font is the same 9 pt UI face, and the legacy
//! dialog font is scaled to the DPI.

#![allow(unsafe_op_in_unsafe_fn)]

/// Shared DPI rule; re-exported so `uifont::scaled` callers keep their path.
pub(super) use crate::win32::dpi::scaled;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DEFAULT_CHARSET, DEFAULT_GUI_FONT, GetObjectW, GetStockObject, HFONT,
    LOGFONTW,
};
use windows::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    NONCLIENTMETRICSW, SPI_GETICONTITLELOGFONT, SPI_GETNONCLIENTMETRICS,
};

/// the UI-font helper's switch over the preset ids: the face and the size in
/// tenths of a point. `1200` back to — is absent, which is the system font.
fn preset(id: u32) -> Option<(&'static str, i32)> {
    Some(match id {
        1 => ("Tahoma", 80),
        2 => ("Segoe UI", 80),
        3 => ("Segoe UI", 90),
        4 => ("Meiryo", 80),
        5 => ("Meiryo", 90),
        6 => ("Verdana", 80),
        _ => return None,
    })
}

/// The named preset font
/// the tenths, weight left to the font, default charset.
unsafe fn named(face: &str, tenths: i32, dpi: i32) -> HFONT {
    let mut lf = LOGFONTW::default();
    lf.lfHeight = crate::win32::gdi::pixel_height(dpi as u32, tenths as f32 / 10.0).max(1);
    // A preset names a face and a size only: the weight stays at the LOGFONT's own
    // "do not care" default and the charset is DEFAULT_CHARSET.
    lf.lfCharSet = DEFAULT_CHARSET;
    let units: Vec<u16> = face.encode_utf16().collect();
    let len = units.len().min(lf.lfFaceName.len() - 1);
    lf.lfFaceName[..len].copy_from_slice(&units[..len]);
    CreateFontIndirectW(&lf)
}

/// The system UI `LOGFONT` for `dpi`: the metrics' menu font at `FW_NORMAL`,
/// following the fallback chain described above.
unsafe fn logfont(dpi: i32) -> Option<LOGFONTW> {
    let mut ncm = NONCLIENTMETRICSW::default();
    ncm.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
    let menu_ok = SystemParametersInfoForDpi(
        SPI_GETNONCLIENTMETRICS.0,
        ncm.cbSize,
        Some(&mut ncm as *mut NONCLIENTMETRICSW as *mut core::ffi::c_void),
        0,
        dpi as u32,
    )
    .is_ok()
        && ncm.lfMenuFont.lfHeight != 0;
    if menu_ok {
        let mut lf = ncm.lfMenuFont;
        // The metrics' own weight is dropped: the UI font is normal weight (400).
        lf.lfWeight = 400;
        return Some(lf);
    }

    let mut lf = LOGFONTW::default();
    let icon_ok = SystemParametersInfoForDpi(
        SPI_GETICONTITLELOGFONT.0,
        std::mem::size_of::<LOGFONTW>() as u32,
        Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
        0,
        dpi as u32,
    )
    .is_ok()
        && lf.lfHeight != 0;
    if icon_ok {
        return Some(lf);
    }

    let stock = GetStockObject(DEFAULT_GUI_FONT);
    let copied = GetObjectW(
        stock,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
    );
    if copied == 0 {
        return None;
    }
    let h = lf.lfHeight;
    lf.lfHeight = if h == 0 {
        -scaled(11, dpi)
    } else {
        h.signum() * scaled(h.abs(), dpi)
    };
    Some(lf)
}

/// Creates the UI font for `dpi`. The caller owns the handle.
///
/// 这是「界面正文」那一档：对话框、提示条、打开文档列表…… 有自己 token 的
/// 区域（菜单、状态栏）走 [`create_for`]。
pub(super) unsafe fn create(dpi: i32) -> HFONT {
    create_for(dg_skin::FontToken::Ui, dpi)
}

/// 给某个区域的 token 造字体：先问它自己，没写就退到 `ui`，再没有就还是
/// `Setting/UIFont` 那个下拉。
///
/// **退回 `ui` 是有意的**：皮肤只写 `[fonts] ui` 的时候，整套界面（含菜单和
/// 状态栏）一起换 —— 不会出现「对话框变大了、菜单还是 9 pt」那种半拉子。
/// 想只放大正文、把菜单钉住，就两条都写。
///
/// 皮肤写了 `[fonts]` 就整条跳过 `Setting/UIFont`：皮肤比那个下拉更具体，
/// 两处都写的时候以皮肤为准。
pub(super) unsafe fn create_for(token: dg_skin::FontToken, dpi: i32) -> HFONT {
    let spec = crate::skin::font_spec(token)
        .or_else(|| crate::skin::font_spec(dg_skin::FontToken::Ui));
    if let Some(spec) = spec {
        return from_spec(&spec, dpi);
    }
    let id = super::optionsdlg::ui_font_id();
    match preset(id) {
        Some((face, tenths)) => named(face, tenths, dpi),
        None => match logfont(dpi) {
            Some(lf) => CreateFontIndirectW(&lf),
            None => HFONT::default(),
        },
    }
}

/// `[fonts] ui` 那一路：从皮肤的字面描述造一只 UI 字体。
///
/// **只写 `size` 是「系统 UI 字面 + 皮肤的字号」**，不是「Consolas」。界面字体有
/// 系统默认这一说（`logfont`），和编辑器那只等宽字体正好相反 —— 拿 Consolas 排
/// 按钮和列表，中文会掉进回退字面，一个字一个宽度。
///
/// 字号只改 `lfHeight`：字面、字符集、质量都留着系统给的那套，越少动越不容易
/// 让某类控件显示不出来。
unsafe fn from_spec(spec: &dg_skin::FontSpec, dpi: i32) -> HFONT {
    let mut lf = if spec.face.is_empty() {
        logfont(dpi).unwrap_or_default()
    } else {
        let mut lf = LOGFONTW::default();
        lf.lfCharSet = DEFAULT_CHARSET;
        lf.lfQuality = windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY;
        let units: Vec<u16> = spec.face.encode_utf16().collect();
        let len = units.len().min(lf.lfFaceName.len() - 1);
        lf.lfFaceName[..len].copy_from_slice(&units[..len]);
        lf
    };
    // `pixel_height` 给的是正数（点数换算出来的像素），LOGFONT 要的是负的
    // 字高（「字面高度」而不是「单元格高度」）—— 和 `create_editor_font` 同一个
    // 换算，界面和编辑器才不会一个偏大一个偏小。
    lf.lfHeight = -crate::win32::gdi::pixel_height(dpi as u32, spec.size).max(1);
    lf.lfWeight = i32::from(spec.weight);
    lf.lfItalic = u8::from(spec.italic);
    CreateFontIndirectW(&lf)
}

/// The DPI of `hwnd`, never below 96.
pub(super) unsafe fn window_dpi(hwnd: HWND) -> i32 {
    windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::DeleteObject;

    /// The presets are the UI-font list's rows, in that list's own
    /// order.
    #[test]
    fn the_menu_ids_name_a_face_and_a_tenth_point_size() {
        assert_eq!(preset(1200), None);
        assert_eq!(preset(0), None);
        assert_eq!(preset(1), Some(("Tahoma", 80)));
        assert_eq!(preset(2), Some(("Segoe UI", 80)));
        assert_eq!(preset(3), Some(("Segoe UI", 90)));
        assert_eq!(preset(4), Some(("Meiryo", 80)));
        assert_eq!(preset(5), Some(("Meiryo", 90)));
        assert_eq!(preset(6), Some(("Verdana", 80)));
        assert_eq!(preset(7), None);
    }

    /// Tenths become the cell height `MulDiv(LOGPIXELSY, tenths, 720)` gives.
    #[test]
    fn preset_sizes_are_cells_not_ascents() {
        let mut lf = LOGFONTW::default();
        unsafe {
            let font = named("Segoe UI", 90, 120);
            GetObjectW(
                font.into(),
                std::mem::size_of::<LOGFONTW>() as i32,
                Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
            );
            let _ = DeleteObject(font.into());
        }
        assert_eq!(lf.lfHeight, 15);
        assert!(lf.lfHeight > 0);
        let face = String::from_utf16_lossy(
            &lf.lfFaceName[..lf.lfFaceName.iter().position(|&c| c == 0).unwrap()],
        );
        assert_eq!(face, "Segoe UI");
    }

    /// The system font is the metrics' menu font at normal weight.
    #[test]
    fn the_default_branch_is_the_menu_font() {
        unsafe {
            let lf = logfont(120).expect("the system answers for the desktop DPI");
            assert_eq!(lf.lfWeight, 400);
            assert_eq!(lf.lfHeight, -15, "9 pt at 120 DPI is 15 px of ascent");
        }
    }
}
