//! Popup menus and the bar as native `HMENU`s with every row owner-drawn. Two
//! things forced this shape. The frame is `WS_CLIPCHILDREN`, so a menu recorded
//! in the frame's draw list is clipped by exactly the windows it most needs to
//! cover, and a menu is modal: it has to own the mouse and keyboard until one
//! row is chosen. That is the contract below. The bar band is painted by
//! `MENUINFO`'s `hbrBack`; the rows themselves by `WM_DRAWITEM`. The system
//! still owns the band height, the popup placement, keyboard navigation and
//! `TPM_RETURNCMD`, so none of that is reimplemented. The palette and font are
//! the app's, not the OS menu theme's: the system UI font (see `uifont.rs`) and
//! `Theme`'s menu colours. The style snapshot lives in a thread-local because
//! `WM_MEASUREITEM`/`WM_DRAWITEM` are synchronous and can arrive while a caller
//! still holds the frame state borrow; they read the snapshot instead of
//! re-entering `state_mut`.

#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::{Cell, RefCell};

use dg_render::theme::Theme;
use dg_ui::menu::MenuItem;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, DT_HIDEPREFIX, DT_LEFT, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DrawTextW, FillRect, GetDC, GetMonitorInfoW, GetPixel,
    GetTextExtentPoint32W, GetTextMetricsW, HBRUSH, HDC, HFONT, HGDIOBJ, LineTo,
    MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, MoveToEx, PS_NULL, PS_SOLID, Polygon, ReleaseDC, ScreenToClient,
    SelectObject, SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::ODS_SELECTED;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateMenu, CreatePopupMenu, DefWindowProcW, DestroyMenu, GWLP_WNDPROC,
    GetClassNameW, GetMenuItemCount, GetMenuItemInfoW, GetMenuItemRect, GetSubMenu, HCBT_CREATEWND,
    HHOOK, HMENU, InsertMenuItemW, MENU_ITEM_STATE, MENU_ITEM_TYPE, MENUINFO, MENUITEMINFOW,
    MF_BYPOSITION, MFS_CHECKED, MFS_ENABLED, MFS_GRAYED, MFS_HILITE, MFT_OWNERDRAW, MFT_SEPARATOR,
    MFT_STRING, MIIM_DATA, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING, MIIM_SUBMENU,
    MIM_APPLYTOSUBMENUS, MIM_BACKGROUND, MN_GETHMENU, RemoveMenu, SendMessageW, SetTimer,
    SetForegroundWindow, SetMenu, SetMenuInfo, SetWindowLongPtrW, SetWindowsHookExW,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, UnhookWindowsHookEx, WH_CBT, WM_CREATE,
    WM_NCDESTROY, WM_PRINT, WM_PRINTCLIENT, WM_TIMER, WNDPROC,
};
use windows::core::PWSTR;

use crate::win32::uifont::scaled;

/// Marks a `dwItemData` block as one of ours
/// writes before converting a row.
const ROW_MAGIC: u32 = 0x1313;

/// One row's stable heap block. The menu holds the pointer in `dwItemData`;
/// `WM_DRAWITEM` and `WM_MEASUREITEM` get it back for free, which is what lets
/// both handlers know the label, shortcut and state without a lookup table.
pub(super) struct MenuRow {
    magic: u32,
    id: u32,
    label: Vec<u16>,
    shortcut: Vec<u16>,
    checked: bool,
    enabled: bool,
    separator: bool,
    bar: bool,
    /// First top-level bar row: the native band starts it at x=4, and the
    /// row's label has to line up with the toolbar's first icon (x=9 at
    /// 120 DPI), so it carries the small lead instead of the inter-item pad.
    first: bool,
    submenu: bool,
    /// This popup's shared right-hand accelerator column, measured once for
    /// every row of the menu (see `level_accel`).
    accel: i32,
}

impl MenuRow {
    fn new(item: &MenuItem, bar: bool, accel: i32) -> Box<MenuRow> {
        Self::from_parts(
            item.id,
            &item.label,
            item.enabled,
            item.separator,
            !item.children.is_empty(),
            item.checked,
            bar,
            accel,
        )
    }

    fn from_parts(
        id: u32,
        text: &str,
        enabled: bool,
        separator: bool,
        submenu: bool,
        checked: bool,
        bar: bool,
        accel: i32,
    ) -> Box<MenuRow> {
        // editmenu's labels already carry that form.
        let (label, shortcut) = match text.split_once('\t') {
            Some((label, shortcut)) => (label, shortcut),
            None => (text, ""),
        };
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        Box::new(MenuRow {
            magic: ROW_MAGIC,
            id,
            label: wide(label),
            shortcut: wide(shortcut),
            checked,
            enabled: enabled && !separator,
            separator,
            bar,
            first: false,
            submenu,
            accel,
        })
    }

    fn label_slice(&self) -> &[u16] {
        &self.label[..self.label.len().saturating_sub(1)]
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn shortcut_slice(&self) -> &[u16] {
        &self.shortcut[..self.shortcut.len().saturating_sub(1)]
    }
}

/// The colours and metrics a menu row is painted with. Copied into a
/// thread-local whenever the theme, font or DPI changes; draw/measure read it
/// instead of the frame state.
#[derive(Clone, Copy)]
pub(super) struct Style {
    font: HFONT,
    bg: COLORREF,
    fg: COLORREF,
    hover: COLORREF,
    disabled: COLORREF,
    separator: COLORREF,
    dpi: i32,
    row_h: i32,
    sep_h: i32,
    margin: i32,
    gutter: i32,
    bar_pad: i32,
    bar_lead: i32,
    right_pad: i32,
    shortcut_gap: i32,
    arrow_w: i32,
}

thread_local! {
    static STYLE: Cell<Option<Style>> = const { Cell::new(None) };
}

pub(super) fn set_style(style: Style) {
    STYLE.with(|s| s.set(Some(style)));
}

/// Forgets the font/palette snapshot when the frame tears its menus down.
pub(super) fn clear_style() {
    STYLE.with(|s| s.set(None));
}

fn current_style() -> Option<Style> {
    STYLE.with(|s| s.get())
}

/// The menu band's background, for the frame's non-client edge beside it.
pub(super) fn background_color() -> Option<COLORREF> {
    current_style().map(|s| s.bg)
}

fn color(c: dg_render::Color) -> COLORREF {
    COLORREF(((c.b as u32) << 16) | ((c.g as u32) << 8) | c.r as u32)
}

impl Style {
    pub(super) unsafe fn new(theme: &Theme, font: HFONT, dpi: i32) -> Style {
        let line = line_height(font, dpi);
        let gutter = scaled(30, dpi);
        // 皮肤能改行高和菜单栏的内边距（`[metrics] menu.row_h` / `menu.pad`）。
        // 0 表示「跟字体走」，所以出厂值下这两行算出来的数字和以前一样。
        let skin = crate::skin::metrics();
        let row_h = match skin.get(dg_skin::MetricId::MenuRowHeight) {
            h if h > 0.0 => scaled(h.round() as i32, dpi),
            _ => line + scaled(5, dpi),
        };
        let bar_pad = scaled(
            skin.get(dg_skin::MetricId::MenuPad).round() as i32,
            dpi,
        );
        Style {
            font,
            bg: color(theme.menu_bg),
            fg: color(theme.menu_fg),
            hover: color(theme.menu_hover),
            disabled: color(theme.menu_disabled),
            separator: color(theme.menu_separator),
            dpi,
            row_h,
            sep_h: scaled(9, dpi),
            margin: scaled(5, dpi),
            gutter,
            bar_pad,
            bar_lead: scaled(4, dpi),
            right_pad: scaled(10, dpi),
            shortcut_gap: scaled(24, dpi),
            arrow_w: scaled(15, dpi),
        }
    }
}

/// The line box of `font` at `dpi`: `tmHeight + tmExternalLeading`, or the
/// scaled stock size when a DC cannot be opened.
unsafe fn line_height(font: HFONT, dpi: i32) -> i32 {
    let hdc = GetDC(None);
    if hdc.0.is_null() {
        return scaled(16, dpi);
    }
    let old = SelectObject(hdc, HGDIOBJ(font.0));
    let mut tm = TEXTMETRICW::default();
    let ok = GetTextMetricsW(hdc, &mut tm).as_bool();
    if !old.0.is_null() {
        SelectObject(hdc, old);
    }
    ReleaseDC(None, hdc);
    if ok {
        (tm.tmHeight + tm.tmExternalLeading).max(1)
    } else {
        scaled(16, dpi)
    }
}

/// The accelerator column a popup shares across its rows. Its widest label
/// is `CR (MacOS)`, 86 px in the 15 px UI font, and the rest of the row is
/// fixed chrome: 12 px margins + 38 px gutter + 13 px right padding = 63.
/// That leaves 40 px reserved even with no shortcut to draw — twice the 20
/// px `SM_CXMENUCHECK` the system puts beside an ownerdraw row at this DPI.
/// The Edit menu's shortcuts need a wider column: the widest shortcut plus
/// the gap and the submenu arrow. So the column is the larger of that
/// minimum and (widest shortcut + gap + arrow when the menu has submenus).
unsafe fn level_accel(items: &[MenuItem]) -> i32 {
    let Some(style) = current_style() else {
        return 0;
    };
    let mut has_submenu = false;
    let mut widest = 0;
    for item in items {
        if !item.children.is_empty() {
            has_submenu = true;
        }
        if let Some((_, shortcut)) = item.label.split_once('\t') {
            let text: Vec<u16> = shortcut.encode_utf16().collect();
            let hdc = GetDC(None);
            if hdc.0.is_null() {
                continue;
            }
            let old = SelectObject(hdc, HGDIOBJ(style.font.0));
            widest = widest.max(text_width(hdc, &text));
            if !old.0.is_null() {
                SelectObject(hdc, old);
            }
            ReleaseDC(None, hdc);
        }
    }
    let min = scaled(32, style.dpi);
    if widest == 0 {
        min
    } else {
        min.max(widest + style.shortcut_gap + if has_submenu { style.arrow_w } else { 0 })
    }
}

/// The column already shared by the rows of an existing menu (the 扩展
/// submenu gets script rows appended after it was built).
unsafe fn level_accel_of(menu: HMENU) -> i32 {
    let count = GetMenuItemCount(Some(menu)).max(0);
    for pos in 0..count {
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_DATA,
            ..Default::default()
        };
        if GetMenuItemInfoW(menu, pos as u32, true, &mut mii).is_ok() && mii.dwItemData != 0 {
            let row = &*(mii.dwItemData as *const MenuRow);
            if row.magic == ROW_MAGIC {
                return row.accel;
            }
        }
    }
    0
}

fn menu_type(row: &MenuRow) -> MENU_ITEM_TYPE {
    if row.separator {
        MFT_SEPARATOR | MFT_OWNERDRAW
    } else {
        MFT_STRING | MFT_OWNERDRAW
    }
}

fn menu_state(row: &MenuRow) -> MENU_ITEM_STATE {
    let mut state = if row.enabled { MFS_ENABLED } else { MFS_GRAYED };
    if row.checked {
        state |= MFS_CHECKED;
    }
    state
}

/// Inserts one described row at `pos`, moving its heap block into the menu.
/// The caller keeps no reference: `dwItemData` is the only owner until the
/// menu tree is destroyed and the block is freed by `free_tree`.
unsafe fn insert_row(menu: HMENU, pos: u32, row: &MenuRow, sub: Option<HMENU>) -> bool {
    let ptr = row as *const MenuRow as usize;
    let mut mii = MENUITEMINFOW {
        cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_FTYPE | MIIM_ID | MIIM_STATE | MIIM_DATA,
        fType: menu_type(row),
        fState: menu_state(row),
        wID: row.id,
        dwItemData: ptr,
        ..Default::default()
    };
    if !row.separator {
        mii.fMask |= MIIM_STRING;
        mii.dwTypeData = PWSTR(row.label.as_ptr() as *mut u16);
    }
    if let Some(sub) = sub {
        mii.fMask |= MIIM_SUBMENU;
        mii.hSubMenu = sub;
    }
    InsertMenuItemW(menu, pos, true, &mii).is_ok()
}

/// Destroys a menu tree and the row blocks attached to it. `DestroyMenu` does
/// not destroy submenus, so the walk is explicit; the `dwItemData` block is
/// owned by its menu row
///
/// The blocks are dropped *after* the menus: `dwTypeData` points at the label
/// buffer inside the block until the menu is gone, and freeing first would
/// leave `DestroyMenu` reading freed memory.
unsafe fn free_tree(menu: HMENU) {
    let mut rows: Vec<*mut MenuRow> = Vec::new();
    collect_tree(menu, &mut rows);
    for ptr in rows {
        let row = Box::from_raw(ptr);
        if row.magic != ROW_MAGIC {
            std::mem::forget(row);
        }
    }
}

unsafe fn collect_tree(menu: HMENU, rows: &mut Vec<*mut MenuRow>) {
    let count = GetMenuItemCount(Some(menu)).max(0);
    for pos in 0..count {
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_DATA,
            ..Default::default()
        };
        if GetMenuItemInfoW(menu, pos as u32, true, &mut mii).is_ok() && mii.dwItemData != 0 {
            rows.push(mii.dwItemData as *mut MenuRow);
        }
        let sub = GetSubMenu(menu, pos);
        if !sub.0.is_null() {
            collect_tree(sub, rows);
        }
    }
    let _ = DestroyMenu(menu);
}

/// Builds one popup (possibly with nested popups). `None` tears down whatever
/// was built so a half menu is never tracked.
unsafe fn build_popup(items: &[MenuItem]) -> Option<HMENU> {
    if items.is_empty() {
        return None;
    }
    let menu = CreatePopupMenu().ok()?;
    let accel = level_accel(items);
    for (pos, item) in items.iter().enumerate() {
        let sub = if item.children.is_empty() {
            None
        } else {
            match build_popup(&item.children) {
                Some(sub) => Some(sub),
                None => {
                    free_tree(menu);
                    return None;
                }
            }
        };
        let row = MenuRow::new(item, false, accel);
        if insert_row(menu, pos as u32, &row, sub) {
            let _ = Box::into_raw(row);
        } else {
            if let Some(sub) = sub {
                free_tree(sub);
            }
            drop(row);
            free_tree(menu);
            return None;
        }
    }
    Some(menu)
}

/// The bar itself: a `CreateMenu` owner with one ownerdraw popup row per
/// top-level menu, backed by the same builder the popups use.
unsafe fn build_bar(items: &[MenuItem]) -> Option<HMENU> {
    if items.is_empty() {
        return None;
    }
    let bar = CreateMenu().ok()?;
    for (pos, item) in items.iter().enumerate() {
        let Some(sub) = build_popup(&item.children) else {
            free_tree(bar);
            return None;
        };
        let mut row = MenuRow::new(item, true, 0);
        if pos == 0 {
            row.first = true;
        }
        if insert_row(bar, pos as u32, &row, Some(sub)) {
            let _ = Box::into_raw(row);
        } else {
            free_tree(sub);
            drop(row);
            free_tree(bar);
            return None;
        }
    }
    Some(bar)
}

/// The bar's menu. Row blocks hang off the menu's `dwItemData`, so the
/// structure itself only needs the handle; the frame keeps it for the life of
/// the window so 隐藏主菜单 can put the same `HMENU` back.
pub(super) struct Bar {
    pub(super) hmenu: HMENU,
}

/// A child popup of `menu` at `pos` — the 会话 and 最近的文件 popups sit one
/// level below the bar's top row, where `Bar::submenu` cannot reach.
pub(super) unsafe fn child_popup(menu: HMENU, pos: usize) -> HMENU {
    GetSubMenu(menu, pos as i32)
}

impl Bar {
    /// The popup at `index`, for the WM_INITMENUPOPUP refresh and the script
    /// rebuild.
    pub(super) unsafe fn submenu(&self, index: usize) -> HMENU {
        GetSubMenu(self.hmenu, index as i32)
    }

    /// Whether `popup` sits anywhere in this bar's menu tree. The
    /// `WM_INITMENUPOPUP` refresh re-marks the popups it answers with the bar's
    /// state table; a context menu marks its own rows when it is built, and
    /// asking the bar's table about its ids would wipe those marks. Nested
    /// submenus (查看 → 颜文字, 文档 → 换行符格式) are as much the bar's as the
    /// top-level dropdowns — Windows sends each its own `WM_INITMENUPOPUP`, so
    /// stopping at depth one left every nested mark frozen at build time.
    pub(super) unsafe fn owns_popup(&self, popup: HMENU) -> bool {
        unsafe fn walk(menu: HMENU, want: HMENU) -> bool {
            let count = GetMenuItemCount(Some(menu)).max(0);
            (0..count).any(|i| {
                let sub = GetSubMenu(menu, i);
                !sub.0.is_null() && (sub == want || walk(sub, want))
            })
        }
        walk(self.hmenu, popup)
    }

    /// Removes the script rows from `menu` starting at `base` and frees their
    /// blocks. Used by 刷新脚本菜单.
    pub(super) unsafe fn clear_scripts(&mut self, menu: HMENU, base: usize) {
        let count = GetMenuItemCount(Some(menu)).max(0) as usize;
        for pos in (base..count).rev() {
            let mut mii = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_DATA,
                ..Default::default()
            };
            let ptr = if GetMenuItemInfoW(menu, pos as u32, true, &mut mii).is_ok() {
                mii.dwItemData as *mut MenuRow
            } else {
                std::ptr::null_mut()
            };
            let _ = RemoveMenu(menu, pos as u32, MF_BYPOSITION);
            if !ptr.is_null() {
                let row = Box::from_raw(ptr);
                if row.magic != ROW_MAGIC {
                    std::mem::forget(row);
                }
            }
        }
    }

    /// Appends a separator to `menu` as an ownerdraw row.
    pub(super) unsafe fn push_separator(&mut self, menu: HMENU) {
        let row = MenuRow::from_parts(
            0,
            "",
            false,
            true,
            false,
            false,
            false,
            level_accel_of(menu),
        );
        let pos = GetMenuItemCount(Some(menu)).max(0) as u32;
        if insert_row(menu, pos, &row, None) {
            let _ = Box::into_raw(row);
        }
    }

    /// Appends a command row to `menu` as an ownerdraw row.
    pub(super) unsafe fn push_command(&mut self, menu: HMENU, id: u32, label: &str) {
        let row = MenuRow::from_parts(
            id,
            label,
            true,
            false,
            false,
            false,
            false,
            level_accel_of(menu),
        );
        let pos = GetMenuItemCount(Some(menu)).max(0) as u32;
        if insert_row(menu, pos, &row, None) {
            let _ = Box::into_raw(row);
        }
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        unsafe { free_tree(self.hmenu) };
    }
}

/// Wraps a menu the test built by hand so the frame-state paths that need a
/// `Bar` (the 扩展 script rebuild) can be exercised without a window.
#[cfg(test)]
pub(super) fn test_bar(hmenu: HMENU) -> Bar {
    Bar { hmenu }
}

/// Every ownerdraw command row in `menu` as `(id, 显示文字)`，分隔和子菜单跳过。
/// 给「扩展菜单动态区到底挂了几行」这类断言用 —— 不用截图、不用真的弹菜单。
#[cfg(test)]
pub(super) unsafe fn test_rows(menu: HMENU) -> Vec<(u32, String)> {
    let count = GetMenuItemCount(Some(menu)).max(0);
    let mut out = Vec::new();
    for pos in 0..count {
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_DATA,
            ..Default::default()
        };
        if GetMenuItemInfoW(menu, pos as u32, true, &mut mii).is_err() || mii.dwItemData == 0 {
            continue;
        }
        let row = &*(mii.dwItemData as *const MenuRow);
        if row.magic != ROW_MAGIC || row.separator || row.submenu {
            continue;
        }
        let label = String::from_utf16_lossy(&row.label)
            .trim_end_matches('\0')
            .to_string();
        out.push((row.id, label));
    }
    out
}

/// The mark row `id` carries in `menu`, for the frame tests that drive
/// `WM_INITMENUPOPUP` without opening the popup.
#[cfg(test)]
pub(super) unsafe fn test_row_checked(menu: HMENU, id: u32) -> Option<bool> {
    let count = GetMenuItemCount(Some(menu)).max(0);
    for pos in 0..count {
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_DATA,
            ..Default::default()
        };
        if GetMenuItemInfoW(menu, pos as u32, true, &mut mii).is_err() || mii.dwItemData == 0 {
            continue;
        }
        let row = &*(mii.dwItemData as *const MenuRow);
        if row.magic == ROW_MAGIC && !row.separator && !row.submenu && row.id == id {
            return Some(row.checked);
        }
    }
    None
}

unsafe fn apply_background(menu: HMENU, brush: HBRUSH) {
    let info = MENUINFO {
        cbSize: std::mem::size_of::<MENUINFO>() as u32,
        fMask: MIM_BACKGROUND | MIM_APPLYTOSUBMENUS,
        hbrBack: brush,
        ..Default::default()
    };
    let _ = SetMenuInfo(menu, &info);
}

/// Attaches the frame's menu bar with every row owner-drawn and the band
/// painted from `brush` (the theme's menu background, owned by the frame).
pub(super) unsafe fn install_bar(owner: HWND, items: &[MenuItem], brush: HBRUSH) -> Option<Bar> {
    let bar = build_bar(items)?;
    apply_background(bar, brush);
    if SetMenu(owner, Some(bar)).is_err() {
        free_tree(bar);
        return None;
    }
    Some(Bar { hmenu: bar })
}

/// Re-applies the band brush after a theme change.
pub(super) unsafe fn set_bar_background(bar: &Bar, brush: HBRUSH) {
    apply_background(bar.hmenu, brush);
}

/// Applies the theme background to a menu that was not built here (the
/// toolbar's dropdown skeleton). The brush stays owned by the frame.
pub(super) unsafe fn set_background(menu: HMENU, brush: HBRUSH) {
    apply_background(menu, brush);
}

/// Rebuilds an existing popup (the toolbar's dropdown) as ownerdraw rows.
/// Removes and frees the previous rows first; the heap blocks are owned by
/// the menu, so the caller keeps only the handle.
pub(super) unsafe fn fill_popup(menu: HMENU, items: &[MenuItem]) -> bool {
    let count = GetMenuItemCount(Some(menu)).max(0);
    for pos in (0..count).rev() {
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_DATA,
            ..Default::default()
        };
        let ptr = if GetMenuItemInfoW(menu, pos as u32, true, &mut mii).is_ok() {
            mii.dwItemData as *mut MenuRow
        } else {
            std::ptr::null_mut()
        };
        let _ = RemoveMenu(menu, pos as u32, MF_BYPOSITION);
        if !ptr.is_null() {
            let row = Box::from_raw(ptr);
            if row.magic != ROW_MAGIC {
                std::mem::forget(row);
            }
        }
    }
    let accel = level_accel(items);
    for (pos, item) in items.iter().enumerate() {
        let row = MenuRow::new(item, false, accel);
        if insert_row(menu, pos as u32, &row, None) {
            let _ = Box::into_raw(row);
        } else {
            drop(row);
            return false;
        }
    }
    true
}

/// Re-marks the rows of the popup that is about to open (`WM_INITMENUPOPUP`).
///
/// Rows are ownerdraw, so the mark is a field in our heap block rather than a
/// `CheckMenuItem`: `checked` is asked for the row's command id and the draw
/// handler reads it back a moment later. Popup and separator rows are skipped.
pub(super) unsafe fn refresh_checks(popup: HMENU, mut checked: impl FnMut(u32) -> bool) {
    let count = GetMenuItemCount(Some(popup)).max(0);
    for pos in 0..count {
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_DATA,
            ..Default::default()
        };
        if GetMenuItemInfoW(popup, pos as u32, true, &mut mii).is_err() {
            continue;
        }
        let ptr = mii.dwItemData as *mut MenuRow;
        if ptr.is_null() {
            continue;
        }
        let row = &mut *ptr;
        if row.magic != ROW_MAGIC || row.separator || row.submenu {
            continue;
        }
        row.checked = checked(row.id);
    }
}

/// Re-marks the enabled state of the popup's leaf rows, the same heap-block
/// way [`refresh_checks`] re-marks checks. The 文件 menu's three state-driven
/// rows (1236/1242/1243) read it on `WM_INITMENUPOPUP`; everything else —
/// including the recent-file and session rows appended at runtime — answers
/// `true` from the caller's predicate, which keeps their built-in state.
pub(super) unsafe fn refresh_enabled(popup: HMENU, mut enabled: impl FnMut(u32) -> bool) {
    let count = GetMenuItemCount(Some(popup)).max(0);
    for pos in 0..count {
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_DATA,
            ..Default::default()
        };
        if GetMenuItemInfoW(popup, pos as u32, true, &mut mii).is_err() {
            continue;
        }
        let ptr = mii.dwItemData as *mut MenuRow;
        if ptr.is_null() {
            continue;
        }
        let row = &mut *ptr;
        if row.magic != ROW_MAGIC || row.separator || row.submenu {
            continue;
        }
        row.enabled = enabled(row.id);
    }
}

/// x clamped into the monitor work area that owns the anchor
/// And the reason a menu opened near the right edge of a
/// second monitor does not slide back onto the first one.
fn clamp_x(x: i32, work: &RECT) -> i32 {
    x.max(work.left).min(work.right)
}

fn work_area(pt: POINT) -> Option<RECT> {
    let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    if mon.0.is_null() {
        return None;
    }
    let mut mi = MONITORINFO {
        cbSize: core::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        return None;
    }
    Some(mi.rcWork)
}

/// Tracks `items` at `anchor` (client space) and returns the chosen id.
/// `brush` paints the popup's own margin; rows are painted by `WM_DRAWITEM`.
pub(super) unsafe fn track(
    owner: HWND,
    items: &[MenuItem],
    anchor: POINT,
    brush: HBRUSH,
) -> Option<u32> {
    let menu = build_popup(items)?;
    apply_background(menu, brush);
    let mut pt = anchor;
    let _ = ClientToScreen(owner, &mut pt);
    let x = match work_area(pt) {
        Some(work) => clamp_x(pt.x, &work),
        None => pt.x,
    };
    let _ = SetForegroundWindow(owner);
    let chosen = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        x,
        pt.y,
        None,
        owner,
        None,
    )
    .0;
    free_tree(menu);
    (chosen > 0).then_some(chosen as u32)
}

/// Measures one row for `WM_MEASUREITEM`. The returned height is what the
/// popup honors; the bar ignores it and keeps its band height, but it does
/// honor the width. The system adds a fixed gutter to an ownerdraw row's
/// measured width (probed: 16 px at 96 DPI), so subtract it to end at the
/// designed width.
pub(super) unsafe fn measure(item: &mut windows::Win32::UI::Controls::MEASUREITEMSTRUCT) -> bool {
    let Some(style) = current_style() else {
        return false;
    };
    let ptr = item.itemData as *const MenuRow;
    if ptr.is_null() {
        return false;
    }
    let row = &*ptr;
    if row.magic != ROW_MAGIC {
        return false;
    }
    let (w, h) = measure_row(row, &style);
    item.itemWidth = w.max(0) as u32;
    item.itemHeight = h.max(0) as u32;
    true
}

/// The designed width/height of a row, before the system's own gutter.
unsafe fn measure_row(row: &MenuRow, style: &Style) -> (i32, i32) {
    if row.separator {
        return (0, style.sep_h);
    }
    let hdc = GetDC(None);
    if hdc.0.is_null() {
        return (scaled(80, style.dpi), style.row_h);
    }
    let old = SelectObject(hdc, HGDIOBJ(style.font.0));
    let label_w = text_width(hdc, row.label_slice());
    if !old.0.is_null() {
        SelectObject(hdc, old);
    }
    ReleaseDC(None, hdc);

    let w = if row.bar {
        // The first label's glyph starts at the toolbar's first icon, so its
        // leading inset is the small lead alone, not the inter-item pad.
        let lead = if row.first {
            style.bar_lead
        } else {
            style.bar_pad
        };
        label_w + lead + style.bar_pad
    } else {
        // The accelerator column is part of every row's measure; a shortcut
        // that needs more than the column widens its own row instead of
        // spilling into the label.
        style.margin * 2 + style.gutter + label_w + row.accel + style.right_pad
    };
    // The system adds 16 px to an ownerdraw row's measured width at every
    // DPI (probed at 96; the bar at 120 shows the same 16).
    (w - 16, style.row_h)
}

unsafe fn text_width(hdc: HDC, text: &[u16]) -> i32 {
    // Measure the advances of the glyphs DrawText will actually draw, with the
    // '&' prefix markers removed. `DrawText(DT_CALCRECT)` came back ~4 px short
    // on the CJK labels and clipped the closing parenthesis.
    let plain: Vec<u16> = text
        .iter()
        .copied()
        .filter(|&c| c != 0 && c != u16::from(b'&'))
        .collect();
    let mut size = SIZE::default();
    if GetTextExtentPoint32W(hdc, &plain, &mut size).as_bool() {
        size.cx
    } else {
        0
    }
}

/// Paints one menu row (`WM_DRAWITEM`). Returns true when the item was ours,
/// so the frame can tell the system the message is handled.
pub(super) unsafe fn draw(item: &windows::Win32::UI::Controls::DRAWITEMSTRUCT) -> bool {
    let Some(style) = current_style() else {
        return false;
    };
    let ptr = item.itemData as *const MenuRow;
    if ptr.is_null() {
        return false;
    }
    let row = &*ptr;
    if row.magic != ROW_MAGIC {
        return false;
    }
    let _stats = super::paintstats::begin(super::paintstats::Region::MenuRow);
    let hdc = item.hDC;
    let rc = item.rcItem;
    SetBkMode(hdc, TRANSPARENT);

    if row.separator {
        let pen = super::gdicache::pen(PS_SOLID, 1, style.separator);
        let old = SelectObject(hdc, HGDIOBJ(pen.0));
        let y = (rc.top + rc.bottom) / 2;
        let inset = style.margin + scaled(3, style.dpi);
        let _ = MoveToEx(hdc, rc.left + inset, y, None);
        let _ = LineTo(hdc, rc.right - inset, y);
        if !old.0.is_null() {
            SelectObject(hdc, old);
        }
        return true;
    }

    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    let bg = if selected && row.enabled {
        style.hover
    } else {
        style.bg
    };
    FillRect(hdc, &rc, super::gdicache::brush(bg));
    if selected && row.enabled && !row.bar {
        // the hover mark keeps that inset instead of touching the border.
        let hover = super::gdicache::brush(style.hover);
        let inner = RECT {
            left: rc.left + style.margin,
            right: rc.right - style.margin,
            top: rc.top + scaled(1, style.dpi),
            bottom: rc.bottom - scaled(1, style.dpi),
        };
        FillRect(hdc, &inner, hover);
    }

    let old_font = SelectObject(hdc, HGDIOBJ(style.font.0));
    let fg = if row.enabled {
        style.fg
    } else {
        style.disabled
    };
    SetTextColor(hdc, fg);

    let mut text_rc = rc;
    if row.bar {
        text_rc.left += if row.first {
            style.bar_lead
        } else {
            style.bar_pad
        };
        text_rc.right -= style.bar_pad;
    } else {
        text_rc.left += style.margin + style.gutter;
        text_rc.right -= style.margin + style.right_pad + row.accel;
    }
    let flags = DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_HIDEPREFIX;
    let mut label = row.label.clone();
    let label_len = label.len().saturating_sub(1);
    DrawTextW(hdc, &mut label[..label_len], &mut text_rc, flags);

    if row.shortcut.len() > 1 {
        let mut shortcut_rc = rc;
        shortcut_rc.left += style.margin + style.gutter;
        shortcut_rc.right -= style.margin + style.right_pad;
        if row.submenu && !row.bar {
            shortcut_rc.right -= style.arrow_w;
        }
        let mut shortcut = row.shortcut.clone();
        let shortcut_len = shortcut.len().saturating_sub(1);
        DrawTextW(
            hdc,
            &mut shortcut[..shortcut_len],
            &mut shortcut_rc,
            DT_SINGLELINE | DT_VCENTER | DT_RIGHT | DT_HIDEPREFIX,
        );
    }

    if row.checked {
        draw_check(hdc, &rc, &style, fg);
    }
    // The submenu arrow is not drawn here: the system paints its own solid
    // arrow after WM_DRAWITEM, so the popup subclass below repaints it in the
    // theme's foreground and there is only ever one arrow.

    if !old_font.0.is_null() {
        SelectObject(hdc, old_font);
    }
    true
}

unsafe fn draw_check(hdc: HDC, rc: &RECT, style: &Style, fg: COLORREF) {
    let cx = rc.left + style.margin + style.gutter / 2;
    let cy = (rc.top + rc.bottom) / 2;
    let pen = super::gdicache::pen(PS_SOLID, scaled(2, style.dpi), fg);
    let old = SelectObject(hdc, HGDIOBJ(pen.0));
    let _ = MoveToEx(hdc, cx - scaled(4, style.dpi), cy, None);
    let _ = LineTo(hdc, cx - scaled(1, style.dpi), cy + scaled(3, style.dpi));
    let _ = LineTo(hdc, cx + scaled(5, style.dpi), cy - scaled(4, style.dpi));
    if !old.0.is_null() {
        SelectObject(hdc, old);
    }
}

thread_local! {
    static POPUP_HOOK: Cell<isize> = const { Cell::new(0) };
    static POPUP_PROCS: RefCell<Vec<(isize, WNDPROC)>> = const { RefCell::new(Vec::new()) };
    /// Whether the arrow overpaint is already running on this thread, which is
    /// how it recognizes its own `MN_GETHMENU` ask coming back through the
    /// subclassed procedure.
    static OVERPAINTING: Cell<bool> = const { Cell::new(false) };
}

/// The popup's own cleanup timer, and how often it fires. An id this large
/// cannot collide with the ones USER32 arms on the same window.
const ARROW_TIMER: usize = 0xAE07;
const ARROW_PERIOD_MS: u32 = 15;

/// Subclasses every `#32768` popup created on this thread.
///
/// USER32 paints the submenu arrow itself, after `WM_DRAWITEM`, and it takes
/// the arrow's colour from the system menu theme — black on a light-theme
/// process, which is invisible on the dark band. The subclass repaints those
/// arrows in the menu style's foreground so the popup has one solid arrow and
/// its colour comes from the `Theme`.
pub(super) unsafe fn install_popup_hook() {
    POPUP_HOOK.with(|slot| {
        if slot.get() != 0 {
            return;
        }
        let hook = SetWindowsHookExW(
            WH_CBT,
            Some(popup_hook),
            Some(crate::win32::view::instance()),
            GetCurrentThreadId(),
        );
        if let Ok(hook) = hook {
            slot.set(hook.0 as isize);
        }
    });
}

pub(super) unsafe fn remove_popup_hook() {
    POPUP_HOOK.with(|slot| {
        let raw = slot.get();
        if raw != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(raw as *mut core::ffi::c_void));
            slot.set(0);
        }
    });
}

unsafe extern "system" fn popup_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code as u32 == HCBT_CREATEWND {
        let hwnd = HWND(wparam.0 as *mut core::ffi::c_void);
        let mut class = [0u16; 8];
        let len = GetClassNameW(hwnd, &mut class);
        let tag: [u16; 6] = [35, 51, 50, 55, 54, 56];
        if len == 6 && class[..6] == tag {
            let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, popup_proc as *const () as isize);
            if prev != 0 {
                let orig: WNDPROC = core::mem::transmute(prev);
                POPUP_PROCS.with(|procs| procs.borrow_mut().push((hwnd.0 as isize, orig)));
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe fn popup_original(hwnd: HWND) -> WNDPROC {
    POPUP_PROCS.with(|procs| {
        procs
            .borrow()
            .iter()
            .rev()
            .find(|(h, _)| *h == hwnd.0 as isize)
            .and_then(|(_, p)| *p)
    })
}

unsafe extern "system" fn popup_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let orig = popup_original(hwnd);
    let forward = |orig: WNDPROC| match orig {
        Some(p) => p(hwnd, msg, wparam, lparam),
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    // Our own cleanup pass, which the control's procedure has no business
    // seeing.
    if msg == WM_TIMER && wparam.0 == ARROW_TIMER {
        overpaint_popup(hwnd);
        return LRESULT(0);
    }
    match msg {
        WM_CREATE => {
            let result = forward(orig);
            // USER32 can hilite a row — and repaint its arrow — from the
            // frame's menu code without the popup being messaged at all, which
            // no per-message pass can see. The timer bounds how long that, or
            // anything else missed below, can leave the black glyph on screen
            // while the menu is up.
            let _ = SetTimer(Some(hwnd), ARROW_TIMER, ARROW_PERIOD_MS, None);
            result
        }
        WM_PRINTCLIENT => {
            let result = forward(orig);
            paint_popup_arrows(hwnd, HDC(wparam.0 as *mut core::ffi::c_void));
            result
        }
        WM_PRINT => {
            let result = forward(orig);
            // PRF_CLIENT = 4: only the client pass carries the rows.
            if (lparam.0 as u32) & 4 != 0 {
                paint_popup_arrows(hwnd, HDC(wparam.0 as *mut core::ffi::c_void));
            }
            result
        }
        WM_NCDESTROY => {
            let result = forward(orig);
            POPUP_PROCS.with(|procs| procs.borrow_mut().retain(|(h, _)| *h != hwnd.0 as isize));
            result
        }
        // Everything else, because any message the control handles can carry
        // the row repaint: USER32 hilites synchronously inside the mouse move,
        // inside the internal `MN_*` asks that drive a sweep from one row to
        // the next, and inside the timers that open and close a cascade.
        // Naming only the input messages left the arrow black whenever the
        // repaint rode one that was not on the list.
        _ => {
            let result = forward(orig);
            overpaint_popup(hwnd);
            result
        }
    }
}

unsafe fn fill_colour(hdc: HDC, rc: &RECT, colour: COLORREF) {
    if rc.right <= rc.left || rc.bottom <= rc.top {
        return;
    }
    FillRect(hdc, rc, super::gdicache::brush(colour));
}

/// Lays the theme-coloured triangles over a popup that has just painted
/// itself through the live window DC.
unsafe fn overpaint_popup(hwnd: HWND) {
    // `paint_popup_arrows` asks the popup for its `HMENU`, and that ask comes
    // back through this same procedure — without the guard it recurses.
    if OVERPAINTING.with(|busy| busy.get()) {
        return;
    }
    OVERPAINTING.with(|busy| busy.set(true));
    let dc = GetDC(Some(hwnd));
    if !dc.0.is_null() {
        paint_popup_arrows(hwnd, dc);
        let _ = ReleaseDC(Some(hwnd), dc);
    }
    OVERPAINTING.with(|busy| busy.set(false));
}

/// Overpaints the arrow USER32 drew for each submenu row in the style's
/// foreground. `MN_GETHMENU` is the only way back to the `HMENU` from the
/// popup's window handle.
unsafe fn paint_popup_arrows(hwnd: HWND, hdc: HDC) {
    let Some(style) = current_style() else {
        return;
    };
    let raw = SendMessageW(hwnd, MN_GETHMENU, None, None).0;
    let menu = HMENU(raw as *mut core::ffi::c_void);
    if menu.0.is_null() {
        return;
    }
    for pos in 0..GetMenuItemCount(Some(menu)).max(0) {
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_SUBMENU | MIIM_STATE | MIIM_FTYPE,
            ..Default::default()
        };
        if GetMenuItemInfoW(menu, pos as u32, true, &mut mii).is_err()
            || mii.hSubMenu.0.is_null()
            || (mii.fType.0 & MFT_SEPARATOR.0) != 0
        {
            continue;
        }
        let mut rc = RECT::default();
        if GetMenuItemRect(Some(hwnd), menu, pos as u32, &mut rc).is_err() {
            continue;
        }
        let mut tl = POINT {
            x: rc.left,
            y: rc.top,
        };
        let mut br = POINT {
            x: rc.right,
            y: rc.bottom,
        };
        let _ = ScreenToClient(hwnd, &mut tl);
        let _ = ScreenToClient(hwnd, &mut br);
        let rc = RECT {
            left: tl.x,
            top: tl.y,
            right: br.x,
            bottom: br.y,
        };
        if rc.right <= rc.left || rc.bottom <= rc.top {
            continue;
        }
        let enabled = (mii.fState.0 & MFS_GRAYED.0) == 0;
        let selected = (mii.fState.0 & MFS_HILITE.0) != 0;
        let fg = if enabled { style.fg } else { style.disabled };
        // The system arrow is drawn in the item's right gutter, after our
        // row paint; cover that gutter and draw the triangle again.
        let cover = RECT {
            left: rc.right - style.margin - style.right_pad - scaled(4, style.dpi),
            top: rc.top + scaled(1, style.dpi),
            right: rc.right,
            bottom: rc.bottom - scaled(1, style.dpi),
        };
        // Match USER32's own glyph: a solid triangle `half` px on either side
        // of its centre line and `scaled(3) + 1` px wide, with that centre line
        // `scaled(2)` px above the row's — every one of them a DPI-scaled
        // metric, so the glyph is 5x9 px at 96 and 6x11 px at 120. GDI's
        // penless fill drops the top, bottom and right boundary, so the
        // polygon is one pixel larger than the raster it leaves behind.
        let half = scaled(3, style.dpi) + 1;
        let left = rc.right - scaled(10, style.dpi);
        let apex = left + scaled(3, style.dpi) + 1;
        let cy = (rc.top + rc.bottom) / 2 - scaled(2, style.dpi);
        // Leave an arrow that is already ours alone. Every message and every
        // tick of the cleanup timer runs this loop, and the gap between blanking
        // the gutter and laying the triangle down is a frame the eye catches —
        // that was the flicker. One pixel inside the triangle says which of the
        // two glyphs is on screen, and it is only wrong in the direction of
        // painting again.
        if GetPixel(hdc, left + 1, cy) == fg {
            continue;
        }
        if selected && enabled {
            let inner_right = rc.right - style.margin;
            fill_colour(
                hdc,
                &RECT {
                    right: inner_right.min(cover.right),
                    ..cover
                },
                style.hover,
            );
            if cover.right > inner_right {
                fill_colour(
                    hdc,
                    &RECT {
                        left: inner_right,
                        ..cover
                    },
                    style.bg,
                );
            }
        } else {
            fill_colour(hdc, &cover, style.bg);
        }
        let points = [
            POINT {
                x: left,
                y: cy - half,
            },
            POINT { x: apex, y: cy },
            POINT {
                x: left,
                y: cy + half,
            },
        ];
        let brush = super::gdicache::brush(fg);
        let pen = super::gdicache::pen(PS_NULL, 0, fg);
        let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
        let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
        let _ = Polygon(hdc, &points);
        if !old_brush.0.is_null() {
            SelectObject(hdc, old_brush);
        }
        if !old_pen.0.is_null() {
            SelectObject(hdc, old_pen);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u32, label: &str) -> MenuItem {
        MenuItem::new(id, label)
    }

    #[test]
    fn a_tab_splits_the_shortcut_column() {
        let r = MenuRow::new(&row(57635, "剪切(&T)\tCtrl+X"), false, 66);
        assert_eq!(String::from_utf16_lossy(r.label_slice()), "剪切(&T)");
        assert_eq!(String::from_utf16_lossy(r.shortcut_slice()), "Ctrl+X");
        assert_eq!(r.accel, 66);
        assert!(!r.submenu && !r.separator && r.enabled);
    }

    #[test]
    fn separator_rows_ignore_their_own_state() {
        let mut r = MenuRow::new(&MenuItem::separator(), false, 0);
        assert!(r.separator);
        assert!(!r.enabled);
        r.enabled = true;
        assert!(r.separator);
    }

    #[test]
    fn a_row_with_children_is_marked_as_a_submenu() {
        let parent = MenuItem::popup(91, "更多粘贴(&M)...", vec![row(91, "列模式粘贴(&P)")]);
        let r = MenuRow::new(&parent, true, 0);
        assert!(r.bar && r.submenu);
        assert!(!r.separator);
    }

    #[test]
    fn clamp_x_keeps_the_anchor_on_its_own_monitor() {
        let main = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(clamp_x(-40, &main), 0);
        assert_eq!(clamp_x(2500, &main), 1920);
        assert_eq!(clamp_x(700, &main), 700);
        let left = RECT {
            left: -1920,
            top: 0,
            right: -10,
            bottom: 1040,
        };
        assert_eq!(clamp_x(500, &left), -10);
        assert_eq!(clamp_x(-3000, &left), -1920);
    }

    #[test]
    fn empty_menu_is_never_built_or_tracked() {
        unsafe {
            assert!(build_popup(&[]).is_none());
            assert!(track(HWND::default(), &[], POINT::default(), HBRUSH::default()).is_none());
        }
    }

    /// Four described rows build a menu whose every row carries our block; the
    /// menu is destroyed again so the test does not leak an `HMENU`.
    #[test]
    fn build_returns_a_menu_it_owns_back() {
        let items = vec![
            row(1, "CRLF"),
            MenuItem::separator(),
            row(2, "LF").with_checked(true),
            row(3, "CR").with_enabled(false),
        ];
        unsafe {
            let menu = build_popup(&items).expect("four described rows build");
            assert_eq!(GetMenuItemCount(Some(menu)), 4);
            let mut mii = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_DATA,
                ..Default::default()
            };
            GetMenuItemInfoW(menu, 1, true, &mut mii).expect("separator block");
            let sep = &*(mii.dwItemData as *const MenuRow);
            assert_eq!(sep.magic, ROW_MAGIC);
            assert!(sep.separator);
            free_tree(menu);
        }
    }

    /// One with shortcuts widens it past the minimum.
    #[test]
    fn the_accel_column_has_a_floor_and_grows_for_shortcuts() {
        use windows::Win32::Graphics::Gdi::DeleteObject;
        let theme = Theme::default();
        unsafe {
            let font = crate::win32::uifont::create(120);
            set_style(Style::new(&theme, font, 120));
            let eol = vec![
                row(1, "CRLF (PC)"),
                row(2, "LF (Linux)"),
                row(3, "CR (MacOS)"),
            ];
            assert_eq!(level_accel(&eol), scaled(32, 120));
            let edit = vec![row(4, "全选(&A)\tCtrl+A")];
            let wide = level_accel(&edit);
            assert!(wide > scaled(32, 120));
            clear_style();
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }

    /// The style is DPI-scaled and follows the theme, so a 96-DPI strip and a
    /// 120-DPI one differ by exactly the scale, not by hand-tuned constants.
    #[test]
    fn style_scales_its_metrics() {
        use windows::Win32::Graphics::Gdi::DeleteObject;
        let theme = Theme::default();
        unsafe {
            let font = crate::win32::uifont::create(96);
            let s96 = Style::new(&theme, font, 96);
            let s120 = Style::new(&theme, font, 120);
            assert_eq!(s120.margin, scaled(5, 120));
            assert_eq!(s120.gutter, scaled(30, 120));
            assert!(s120.row_h > s96.row_h);
            assert_eq!(s96.margin, 5);
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }

    /// The mark reaches pixels: a checked row paints the tick in its gutter
    /// and an unchecked one leaves the gutter clean, so the frame's re-mark is
    /// what the user sees when the popup opens.
    #[test]
    fn a_checked_row_paints_the_tick() {
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
        };
        use windows::Win32::UI::Controls::{
            DRAWITEMSTRUCT, ODA_DRAWENTIRE, ODS_FLAGS, ODT_MENU,
        };
        unsafe {
            let theme = Theme::default();
            let font = crate::win32::uifont::create(96);
            set_style(Style::new(&theme, font, 96));
            let style = current_style().expect("style");
            let (fg, row_h) = (style.fg, style.row_h);
            let gutter = style.margin + style.gutter;
            let rc = RECT {
                left: 0,
                top: 0,
                right: 220,
                bottom: row_h,
            };
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, rc.right, row_h);
            let old = SelectObject(mem, HGDIOBJ(bmp.0));
            let mut row = MenuRow::new(&row(1334, "显示颜文字(&S)"), false, 66);
            let item = DRAWITEMSTRUCT {
                CtlType: ODT_MENU,
                CtlID: 0,
                itemID: 0,
                itemAction: ODA_DRAWENTIRE,
                itemState: ODS_FLAGS(0),
                hwndItem: HWND::default(),
                hDC: mem,
                rcItem: rc,
                itemData: &mut *row as *mut MenuRow as usize,
            };
            let gutted_ink = |item: &DRAWITEMSTRUCT| {
                let mut n = 0;
                for y in rc.top..rc.bottom {
                    for x in rc.left..rc.left + gutter {
                        if GetPixel(item.hDC, x, y) == fg {
                            n += 1;
                        }
                    }
                }
                n
            };
            assert!(draw(&item), "the row is ours");
            assert_eq!(gutted_ink(&item), 0, "an unchecked row has no tick");
            row.checked = true;
            assert!(draw(&item), "the row is ours");
            assert!(gutted_ink(&item) > 0, "a checked row paints the tick");
            if !old.0.is_null() {
                SelectObject(mem, old);
            }
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = DeleteDC(mem);
            let _ = ReleaseDC(None, screen);
            clear_style();
        }
    }

    /// A nested submenu is the bar's too: its `WM_INITMENUPOPUP` re-marks the
    /// rows, where a depth-one test left every nested mark frozen. A popup
    /// from outside the tree (a context menu) still answers `false`, so its
    /// own marks survive the bar's table.
    #[test]
    fn nested_popups_belong_to_the_bar_and_get_remarked() {
        use windows::Win32::UI::WindowsAndMessaging::{AppendMenuW, MF_POPUP};
        use windows::core::w;
        unsafe {
            let bar = CreateMenu().expect("bar");
            let view = CreatePopupMenu().expect("查看");
            let emoji = build_popup(&[row(1334, "显示颜文字(&S)")]).expect("颜文字 popup");
            let _ = AppendMenuW(bar, MF_POPUP, view.0 as usize, w!("查看(&V)"));
            let _ = AppendMenuW(view, MF_POPUP, emoji.0 as usize, w!("颜文字"));
            let foreign = CreatePopupMenu().expect("context menu");
            let ours = Bar { hmenu: bar };
            assert!(ours.owns_popup(view));
            assert!(ours.owns_popup(emoji), "a nested popup is the bar's too");
            assert!(!ours.owns_popup(foreign));
            let mut mii = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_DATA,
                ..Default::default()
            };
            GetMenuItemInfoW(emoji, 0, true, &mut mii).expect("the leaf row");
            let leaf = &*(mii.dwItemData as *const MenuRow);
            assert!(!leaf.checked, "built unmarked");
            refresh_checks(emoji, |id| id == 1334);
            assert!(leaf.checked, "opening the popup re-marks the row");
            drop(ours);
            let _ = DestroyMenu(bar);
            let _ = DestroyMenu(foreign);
        }
    }
}
