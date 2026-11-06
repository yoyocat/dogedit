//! The chrome this app does not draw: menus, the title bar, the toolbar's own visual
//! style. Surfaces come from the system rather than from the draw list — the menu bar,
//! every tracked popup, the non-client title bar, and the glyph comctl32 draws inside a
//! `ToolbarWindow32` dropdown button — and all of them take their palette from the
//! process's immersive colour policy. The same capture of this implementation came back
//! `#FFFFFF` / `#F3F3F3` with a black arrow until this file existed. The ask has to
//! land before any window, menu or control opens its visual style. The request is
//! driven by the loaded theme, never hard-coded: a light theme must get the light
//! chrome back, which is why `theme_is_dark` reads the theme's own background instead
//! of a constant.

#![allow(unsafe_op_in_unsafe_fn)]

use dg_render::theme::Theme;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use windows::core::{w, BOOL, PCSTR, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, DrawFocusRect, DrawTextW, EndPaint,
    FillRect, GetDC, GetStockObject, GetTextColor, GetTextExtentPoint32W, InvalidateRect,
    ReleaseDC, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, TextOutW, HBRUSH,
    HGDIOBJ, HFONT, HDC, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, NULL_BRUSH, PAINTSTRUCT,
    PS_SOLID, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::{
    BST_HOT, CloseThemeData, DrawThemeBackground, GetThemePartSize, OpenThemeData, SetWindowTheme,
    BP_CHECKBOX, BP_RADIOBUTTON, TS_DRAW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, IsWindowEnabled};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETSTATE, BS_3STATE, BS_AUTO3STATE, BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_CHECKBOX,
    BS_GROUPBOX, BS_LEFTTEXT, BS_PUSHLIKE, BS_RADIOBUTTON, BST_PUSHED, CallNextHookEx,
    EnumChildWindows, GetClassNameW, GetClientRect, GetDlgItem, GetParent, GetWindow,
    GetWindowLongPtrW, GetWindowTextW, LB_ERR, LB_GETCOUNT, LB_GETCURSEL, LB_GETITEMRECT,
    LB_GETSEL, LB_GETTEXT, LB_GETTEXTLEN, LB_GETTOPINDEX, MessageBoxW, SendMessageW,
    SetWindowLongPtrW, SetWindowsHookExW, UnhookWindowsHookEx, GWL_STYLE, GW_OWNER, HCBT_ACTIVATE,
    HHOOK, MESSAGEBOX_STYLE, WH_CBT, WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_ERASEBKGND, WM_GETFONT, WM_NCDESTROY, WM_PAINT,
    WM_PRINTCLIENT, WS_CLIPCHILDREN,
};

use crate::win32::prompt;
use crate::win32::window::to_colorref;

/// `uxtheme.dll`'s `SetPreferredAppMode` argument for the dark half of the
/// 深色主题 switch: 2 puts the whole process on the dark palette. The light half
/// is not a call — see [`set_app_dark_mode`].
const PREFERRED_FORCE_DARK: i32 = 2;
/// `MAKEINTRESOURCEA(135)` — `SetPreferredAppMode`, also ordinal-only.
const SET_PREFERRED_APP_MODE: usize = 135;
/// `MAKEINTRESOURCEA(133)` — `AllowDarkModeForWindow`, also ordinal-only.
const ALLOW_DARK_MODE_FOR_WINDOW: usize = 133;
/// `MAKEINTRESOURCEA(104)` — `RefreshImmersiveColorPolicyState`. The policy is
/// cached when uxtheme first loads, so the mode set above is not seen by any
/// palette read until this is called.
const REFRESH_IMMERSIVE_POLICY: usize = 104;
/// `MAKEINTRESOURCEA(136)` — `FlushMenuThemes`, which drops the visual-style
/// handles already opened against the old palette.
const FLUSH_MENU_THEMES: usize = 136;

/// The address of one ordinal-only `uxtheme.dll` export, or nothing when this
/// build does not have it.
fn uxtheme_export(ordinal: usize) -> Option<isize> {
    unsafe {
        const UXTHEME: &[u8] = b"uxtheme.dll\0";
        let Ok(lib) = LoadLibraryA(PCSTR(UXTHEME.as_ptr())) else {
            return None;
        };
        GetProcAddress(lib, PCSTR(ordinal as *const u8)).map(|addr| addr as isize)
    }
}

/// 进程这一格到底要不要深色：开机由 `[Setting] DarkTheme` 定一次，记在这儿。
/// 每个窗口的点名都过这道闸 —— 开关说浅色时，没有任何一个窗口可以自作主张要
/// 深色（`[Setting] LightEditTheme` 里写了一套深色配色就是这么个情况：正文照那
/// 套画，原生控件仍然归开关管）。
static APP_DARK: AtomicBool = AtomicBool::new(true);

/// What the process decided at boot: dark, or classic light.
pub(super) fn dark_mode() -> bool {
    APP_DARK.load(Ordering::Acquire)
}

/// All three, in that order — the mode alone leaves every common control
/// reading the palette uxtheme cached when it loaded.
///
/// 浅色那一头**一个调用都不发**：经典程序不 opt-in 就是系统给的浅色，这正是这一
/// 副面孔要的。发 `SetPreferredAppMode(ForceLight)` 是另一种状态 —— 程序明确告
/// 诉 uxtheme「我要浅色」，调色板照发不动，可已经按系统深色开起来的窗口风格并不
/// 因此回头；而开机读的那个开关说浅色，意思就是「别掺和」。
pub(super) fn set_app_dark_mode(dark: bool) {
    APP_DARK.store(dark, Ordering::Release);
    unsafe {
        if !dark {
            return;
        }
        if let Some(addr) = uxtheme_export(SET_PREFERRED_APP_MODE) {
            let set: unsafe extern "system" fn(i32) -> i32 = core::mem::transmute(addr);
            set(PREFERRED_FORCE_DARK);
        }
        if let Some(addr) = uxtheme_export(REFRESH_IMMERSIVE_POLICY) {
            let refresh: unsafe extern "system" fn() = core::mem::transmute(addr);
            refresh();
        }
        if let Some(addr) = uxtheme_export(FLUSH_MENU_THEMES) {
            let flush: unsafe extern "system" fn() = core::mem::transmute(addr);
            flush();
        }
    }
}

/// Whether this theme wants dark system chrome. Rec. 601 luma, the same
/// weighting the rest of this implementation uses to pick a readable foreground.
pub(super) fn theme_is_dark(theme: &Theme) -> bool {
    let c = theme.background;
    (u32::from(c.r) * 299 + u32::from(c.g) * 587 + u32::from(c.b) * 114) / 1000 < 128
}

/// The chrome table, filled when the theme is applied. A `Button` subclass asks
/// its own parent for the face and the enabled ink (see [`fill_parent_face`]), so
/// `FACE`/`CAPTION` here are only the fallback for a parent that never answers.
/// `LINE` is the group box's frame, `DIM` the disabled caption — neither has a
/// per-dialog equivalent, which is why `set_chrome_theme` still runs wherever a
/// dialog asks for the dark style.
static CHROME_FACE: AtomicU32 = AtomicU32::new(0x0020_2020);
static CHROME_CAPTION: AtomicU32 = AtomicU32::new(0x00E0_E0E0);
static CHROME_LINE: AtomicU32 = AtomicU32::new(0x0046_4646);
static CHROME_DIM: AtomicU32 = AtomicU32::new(0x008C_8C8C);
/// A stock list box's selected row. The visual style would supply a system
/// highlight that has nothing to do with the loaded theme, so the row painter
/// takes this instead.
static CHROME_SELECT: AtomicU32 = AtomicU32::new(0x0078_4F26);

/// Publishes the theme's chrome colours for the button and list-box painters.
pub(super) fn set_chrome_theme(theme: &Theme) {
    CHROME_FACE.store(to_colorref(theme.toolbar_bg).0, Ordering::Release);
    CHROME_CAPTION.store(to_colorref(theme.toolbar_fg).0, Ordering::Release);
    CHROME_LINE.store(to_colorref(theme.toolbar_border).0, Ordering::Release);
    CHROME_DIM.store(to_colorref(theme.toolbar_disabled).0, Ordering::Release);
    CHROME_SELECT.store(to_colorref(theme.selection_bg).0, Ordering::Release);
}

/// Asks a control's parent for the brush and the ink it publishes on one of the
/// `WM_CTLCOLOR*` questions, without drawing anything. The brush belongs to the
/// parent and outlives the paint.
///
/// `DefWindowProc` answers with the system ink — black on a light system — and
/// a stock brush. A dark dialog never paints a caption in black, so seeding the
/// DC with 0 lets the same value mean "the parent did not publish its own
/// palette" afterwards. (Not CLR_INVALID: `SetTextColor` would reject
/// 0xFFFFFFFF and leave the DC untouched.) The chrome table is the fallback.
unsafe fn parent_colours(hwnd: HWND, hdc: HDC, msg: u32) -> (Option<HBRUSH>, COLORREF) {
    SetTextColor(hdc, COLORREF(0));
    let mut brush = None;
    if let Ok(parent) = GetParent(hwnd) {
        if !parent.0.is_null() {
            let answer = SendMessageW(
                parent,
                msg,
                Some(WPARAM(hdc.0 as usize)),
                Some(LPARAM(hwnd.0 as isize)),
            );
            if answer.0 != 0 {
                brush = Some(HBRUSH(answer.0 as *mut core::ffi::c_void));
            }
        }
    }
    let answered = GetTextColor(hdc).0;
    // Zero ink means the default dialog procedure answered, and the brush it
    // handed back is its light stock one — neither belongs on a dark dialog.
    if answered == 0 {
        return (None, COLORREF(CHROME_CAPTION.load(Ordering::Acquire)));
    }
    (brush, COLORREF(answered))
}

/// The face brush and the caption ink a `Button` subclass paints with.
///
/// A themed button stopped asking its parent, but the parent's `WM_CTLCOLORBTN`
/// answer is still the one place that knows this dialog's own face — some pages
/// fill from `Theme::background`, others from `toolbar_bg`, and a process-wide
/// chrome table cannot be right for both. Asking it explicitly is what keeps the
/// rectangle behind a check or a group box the same colour as the dialog.
///
/// Returns the ink the parent chose; a disabled caption still takes [`CHROME_DIM`].
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn fill_parent_face(hwnd: HWND, hdc: HDC, rc: &RECT) -> COLORREF {
    let (brush, ink) = parent_colours(hwnd, hdc, WM_CTLCOLORBTN);
    if let Some(brush) = brush {
        let _ = FillRect(hdc, rc, brush);
    } else {
        let own = CreateSolidBrush(COLORREF(CHROME_FACE.load(Ordering::Acquire)));
        let _ = FillRect(hdc, rc, own);
        let _ = DeleteObject(HGDIOBJ(own.0));
    }
    ink
}

/// A stock `ListBox` named `DarkMode_Explorer` gets the style's dark scrollbar,
/// but its rows are painted with the style's own palette — on a light system
/// palette that is black ink on a light field, and the parent's
/// `WM_CTLCOLORLISTBOX` answer is ignored. Un-naming the control used to be the
/// workaround and cost the scrollbar. Instead the subclass below paints only
/// the client rows: the non-client frame, and with it the dark scrollbar, stays
/// with the visual style.
pub(super) fn themed_listbox(dlg: HWND, id: i32) {
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(dlg), id) {
            paint_stock_listbox(ctl);
        }
    }
}

/// The subclass id for a stock list box's rows.
const LISTBOX_SUBCLASS: usize = 8;

/// `LBS_OWNERDRAWFIXED | LBS_OWNERDRAWVARIABLE`. An ownerdraw list already
/// paints its own rows from the theme, and its scrollbar is already dark.
fn is_stock_listbox(style: u32) -> bool {
    style & 0x0030 == 0
}

/// Takes over a stock list's client paint without touching its theme name.
pub(super) fn paint_stock_listbox(hwnd: HWND) {
    unsafe {
        let mut class = [0u16; 32];
        let len = GetClassNameW(hwnd, &mut class);
        if len <= 0 || String::from_utf16_lossy(&class[..len as usize]) != "ListBox" {
            return;
        }
        if !is_stock_listbox(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32) {
            return;
        }
        let _ = SetWindowSubclass(hwnd, Some(listbox_proc), LISTBOX_SUBCLASS, 0);
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

/// Only the client half of a themed list is ours. `WM_PAINT` still goes through
/// the default procedure first, so the style keeps drawing its frame and dark
/// scrollbar; the rows are then repainted over whatever ink it chose.
#[allow(unsafe_op_in_unsafe_fn)]
unsafe extern "system" fn listbox_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _refdata: usize,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            let hdc = HDC(wp.0 as *mut core::ffi::c_void);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let (brush, _) = parent_colours(hwnd, hdc, WM_CTLCOLORLISTBOX);
            if let Some(brush) = brush {
                let _ = FillRect(hdc, &rc, brush);
            } else {
                let own = CreateSolidBrush(COLORREF(CHROME_FACE.load(Ordering::Acquire)));
                let _ = FillRect(hdc, &rc, own);
                let _ = DeleteObject(HGDIOBJ(own.0));
            }
            LRESULT(1)
        }
        WM_PAINT => {
            let base = DefSubclassProc(hwnd, msg, wp, lp);
            let hdc = GetDC(Some(hwnd));
            if !hdc.0.is_null() {
                paint_list_rows(hwnd, hdc);
                let _ = ReleaseDC(Some(hwnd), hdc);
            }
            base
        }
        WM_PRINTCLIENT => {
            let hdc = HDC(wp.0 as *mut core::ffi::c_void);
            if !hdc.0.is_null() {
                paint_list_rows(hwnd, hdc);
            }
            LRESULT(1)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(listbox_proc), LISTBOX_SUBCLASS);
            DefSubclassProc(hwnd, msg, wp, lp)
        }
        _ => DefSubclassProc(hwnd, msg, wp, lp),
    }
}

/// Every visible row, drawn with the parent's field brush and ink and the
/// theme's selection colour. The window's own visual style is left in place for
/// the non-client scrollbar, and nothing here depends on what palette it picked
/// for its rows.
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn paint_list_rows(hwnd: HWND, hdc: HDC) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (brush, ink) = parent_colours(hwnd, hdc, WM_CTLCOLORLISTBOX);
    let mut owned = None;
    let field = match brush {
        Some(b) => b,
        None => {
            let b = CreateSolidBrush(COLORREF(CHROME_FACE.load(Ordering::Acquire)));
            owned = Some(b);
            b
        }
    };
    let selected = CreateSolidBrush(COLORREF(CHROME_SELECT.load(Ordering::Acquire)));
    let font = HFONT(SendMessageW(hwnd, WM_GETFONT, None, None).0 as *mut core::ffi::c_void);
    let has_font = !font.0.is_null();
    let old = if has_font {
        SelectObject(hdc, HGDIOBJ(font.0))
    } else {
        HGDIOBJ::default()
    };
    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = FillRect(hdc, &rc, field);

    let enabled = IsWindowEnabled(hwnd).as_bool();
    let row_ink = if enabled {
        ink
    } else {
        COLORREF(CHROME_DIM.load(Ordering::Acquire))
    };
    let focused = GetFocus() == hwnd;
    let cur = SendMessageW(hwnd, LB_GETCURSEL, None, None).0;
    let count = SendMessageW(hwnd, LB_GETCOUNT, None, None).0;
    let first = SendMessageW(hwnd, LB_GETTOPINDEX, None, None).0.max(0);
    let multicol = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & 0x0200 != 0;
    let mut i = first;
    while i < count {
        let mut row = RECT::default();
        let got = SendMessageW(
            hwnd,
            LB_GETITEMRECT,
            Some(WPARAM(i as usize)),
            Some(LPARAM(&mut row as *mut RECT as isize)),
        );
        if got.0 as i32 == LB_ERR {
            break;
        }
        if row.top > rc.bottom {
            // A multi-column list wraps back to the top for the next column.
            if !multicol {
                break;
            }
            i += 1;
            continue;
        }
        let is_sel = SendMessageW(hwnd, LB_GETSEL, Some(WPARAM(i as usize)), None).0 > 0;
        let _ = FillRect(hdc, &row, if is_sel { selected } else { field });
        let len = SendMessageW(hwnd, LB_GETTEXTLEN, Some(WPARAM(i as usize)), None).0;
        if len > 0 {
            let mut buf = vec![0u16; len as usize + 1];
            let copied = SendMessageW(
                hwnd,
                LB_GETTEXT,
                Some(WPARAM(i as usize)),
                Some(LPARAM(buf.as_mut_ptr() as isize)),
            )
            .0;
            if copied > 0 {
                let n = (copied as usize).min(buf.len());
                let mut caption = row;
                caption.left += 2;
                caption.right = (caption.right - 2).max(caption.left);
                SetTextColor(hdc, row_ink);
                // DrawText clips to the row rect, which is what keeps a long
                // item out of the next column of a multi-column list.
                let _ = DrawTextW(
                    hdc,
                    &mut buf[..n],
                    &mut caption,
                    DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER,
                );
            }
        }
        if focused && i == cur {
            let mut frame = row;
            let _ = DrawFocusRect(hdc, &mut frame);
        }
        i += 1;
    }
    if has_font {
        let _ = SelectObject(hdc, old);
    }
    let _ = DeleteObject(HGDIOBJ(selected.0));
    if let Some(b) = owned {
        let _ = DeleteObject(HGDIOBJ(b.0));
    }
}

/// the window-theme helper's class-specific half for a group box: after the window has
/// been named, a `Button` whose type bits are `BS_GROUPBOX` is subclassed with id 4.
fn groupbox_hook(hwnd: HWND) {
    let mut class = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut class) };
    if len <= 0 {
        return;
    }
    if String::from_utf16_lossy(&class[..len as usize]) != "Button" {
        return;
    }
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    if style & 0xF != BS_GROUPBOX as u32 {
        return;
    }
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(groupbox_proc), 4, usize::default());
    }
}

/// A themed group box draws its caption in the visual style's own text colour, which the
/// dark palette keeps grey the erase and draws the face, the frame and the caption itself.
/// The frame starts at half the caption's height, which is what leaves the gap in the line
/// that a group box is recognised by.
#[allow(unsafe_op_in_unsafe_fn)]
unsafe extern "system" fn groupbox_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _refdata: usize,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let ink = fill_parent_face(hwnd, hdc, &rc);
            let mut text = vec![0u16; 256];
            let len = GetWindowTextW(hwnd, &mut text).max(0) as usize;
            let text = &text[..len.min(text.len())];
            let font = windows::Win32::Graphics::Gdi::HFONT(
                SendMessageW(hwnd, WM_GETFONT, None, None).0 as *mut core::ffi::c_void,
            );
            let old = SelectObject(hdc, HGDIOBJ(font.0));
            let mut size = SIZE::default();
            if !text.is_empty() {
                let _ = GetTextExtentPoint32W(hdc, text, &mut size);
            }
            let pen = CreatePen(PS_SOLID, 1, COLORREF(CHROME_LINE.load(Ordering::Acquire)));
            let oldpen = SelectObject(hdc, HGDIOBJ(pen.0));
            let oldbrush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
            let top = size.cy / 2;
            let _ = RoundRect(hdc, rc.left, top, rc.right, rc.bottom, 8, 8);
            let _ = SelectObject(hdc, oldbrush);
            let _ = SelectObject(hdc, oldpen);
            let _ = DeleteObject(HGDIOBJ(pen.0));
            if !text.is_empty() {
                let _ = SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, ink);
                let _ = TextOutW(hdc, rc.left + 7, rc.top + 1, text);
            }
            let _ = SelectObject(hdc, old);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(1)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(groupbox_proc), 4);
            DefSubclassProc(hwnd, msg, wp, lp)
        }
        _ => DefSubclassProc(hwnd, msg, wp, lp),
    }
}


/// `groupbox_hook`'s other half of the `Button` class: the two-choice buttons,
/// check boxes and radios. A visual-style button paints its caption with the
/// style's own text colour, not the ink its parent set on `WM_CTLCOLORBTN` —
/// when the system palette is light that caption is black and disappears on the
/// dark face, while the glyph beside it is still dark. Taking over the whole
/// paint fixes the caption on every machine: the face and the ink come from the
/// parent's `WM_CTLCOLORBTN` answer, the glyph still comes from the button's own
/// visual style. Push-like toggles, owner-drawn buttons, group boxes and push
/// buttons keep their own paint.
fn choice_hook(hwnd: HWND) {
    let mut class = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut class) };
    if len <= 0 {
        return;
    }
    if String::from_utf16_lossy(&class[..len as usize]) != "Button" {
        return;
    }
    if choice_style(unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32).is_none() {
        return;
    }
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(choice_proc), 5, usize::default());
    }
}

/// Which visual-style part a two-choice button draws, or nothing for a
/// `Button` that must keep its own paint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Choice {
    Check,
    Radio,
}

fn choice_style(style: u32) -> Option<Choice> {
    if style & BS_PUSHLIKE as u32 != 0 {
        return None;
    }
    let kind = style & 0xF;
    if kind == BS_CHECKBOX as u32
        || kind == BS_AUTOCHECKBOX as u32
        || kind == BS_3STATE as u32
        || kind == BS_AUTO3STATE as u32
    {
        Some(Choice::Check)
    } else if kind == BS_RADIOBUTTON as u32 || kind == BS_AUTORADIOBUTTON as u32 {
        Some(Choice::Radio)
    } else {
        None
    }
}

/// A caption's `&` is a mnemonic prefix, not ink. `&&` is the one form that
/// does stand for a literal ampersand; everything else drops the marker and
/// keeps the character it marked, which is what `DT_HIDEPREFIX` does.
fn hide_prefix(text: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'&' as u16 {
            if i + 1 < text.len() && text[i + 1] == b'&' as u16 {
                out.push(b'&' as u16);
                i += 2;
                continue;
            }
        } else {
            out.push(text[i]);
        }
        i += 1;
    }
    out
}

/// The visual-style state for one two-choice button. `BM_GETSTATE`'s low bits
/// are 未选 / 选中 / 不确定 (radios have no third one), and each of those owns
/// four consecutive states: 普通、指向、按下、禁用.
fn choice_state_id(kind: Choice, enabled: bool, button_state: u32) -> i32 {
    let check = match kind {
        Choice::Check => (button_state & 0x3).min(2),
        Choice::Radio => button_state & 0x1,
    };
    let phase = if !enabled {
        3
    } else if button_state & BST_PUSHED != 0 {
        2
    } else if button_state & BST_HOT != 0 {
        1
    } else {
        0
    };
    1 + 4 * check as i32 + phase
}

/// The face, the glyph and the caption, drawn in the order the style would have
/// drawn them: the square or the ring comes from the button's own visual style,
/// so the selected tick keeps the palette the sub-app name chose, while the face
/// and the caption's ink come from the parent's `WM_CTLCOLORBTN` answer — the
/// same brush the rest of that dialog is filled with. Disabled captions take
/// `ToolbarDisabled` instead of dimming that ink, which would otherwise land on
/// the border colour and vanish into the face.
#[allow(unsafe_op_in_unsafe_fn)]
unsafe extern "system" fn choice_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _refdata: usize,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            let hdc = HDC(wp.0 as *mut core::ffi::c_void);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let _ = fill_parent_face(hwnd, hdc, &rc);
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let parent_ink = fill_parent_face(hwnd, hdc, &rc);
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let kind = choice_style(style).unwrap_or(Choice::Radio);
            let part = if kind == Choice::Check {
                BP_CHECKBOX.0
            } else {
                BP_RADIOBUTTON.0
            };
            let enabled = IsWindowEnabled(hwnd).as_bool();
            let button_state = SendMessageW(hwnd, BM_GETSTATE, None, None).0 as u32;
            let state = choice_state_id(kind, enabled, button_state);
            let theme = OpenThemeData(Some(hwnd), w!("BUTTON"));
            let mut ring = SIZE { cx: 16, cy: 16 };
            if theme.0 != 0 {
                if let Ok(size) = GetThemePartSize(theme, Some(hdc), part, state, None, TS_DRAW) {
                    if size.cx > 0 && size.cy > 0 {
                        ring = size;
                    }
                }
            }
            // `BS_LEFTTEXT` puts the square on the right and right-aligns the
            // caption against it.
            let left_text = style & BS_LEFTTEXT as u32 != 0;
            let glyph_left = if left_text {
                rc.right - ring.cx
            } else {
                rc.left
            };
            let top = (rc.bottom - ring.cy) / 2;
            let glyph = RECT {
                left: glyph_left,
                top,
                right: glyph_left + ring.cx,
                bottom: top + ring.cy,
            };
            if theme.0 != 0 {
                let _ = DrawThemeBackground(theme, hdc, part, state, &glyph, None);
            }
            let mut raw = vec![0u16; 256];
            let len = GetWindowTextW(hwnd, &mut raw).max(0) as usize;
            let text = hide_prefix(&raw[..len.min(raw.len())]);
            let font = HFONT(SendMessageW(hwnd, WM_GETFONT, None, None).0 as *mut core::ffi::c_void);
            let old = SelectObject(hdc, HGDIOBJ(font.0));
            let mut size = SIZE::default();
            if !text.is_empty() {
                let _ = GetTextExtentPoint32W(hdc, &text, &mut size);
                let _ = SetBkMode(hdc, TRANSPARENT);
                let ink = if enabled {
                    parent_ink
                } else {
                    COLORREF(CHROME_DIM.load(Ordering::Acquire))
                };
                SetTextColor(hdc, ink);
                let baseline = (rc.bottom - size.cy) / 2;
                let text_left = if left_text {
                    glyph.left - 4 - size.cx
                } else {
                    glyph.right + 4
                };
                let _ = TextOutW(hdc, text_left, baseline, &text);
                if GetFocus() == hwnd {
                    let mut frame = RECT {
                        left: text_left - 2,
                        top: baseline - 1,
                        right: text_left + size.cx + 2,
                        bottom: baseline + size.cy + 1,
                    };
                    let _ = DrawFocusRect(hdc, &mut frame);
                }
            }
            let _ = SelectObject(hdc, old);
            if theme.0 != 0 {
                let _ = CloseThemeData(theme);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(1)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(choice_proc), 5);
            DefSubclassProc(hwnd, msg, wp, lp)
        }
        _ => DefSubclassProc(hwnd, msg, wp, lp),
    }
}

/// Marks one window as dark, which is what decides the palette a common control
/// resolves when it opens its visual style. 进程那一格说浅色，这里就不发深色：
/// 开关之外的窗口可以要浅色，也可以不要深色，但不能反过来。
pub(super) fn allow_dark_mode(hwnd: HWND, dark: bool) {
    let dark = dark && dark_mode();
    unsafe {
        let Some(addr) = uxtheme_export(ALLOW_DARK_MODE_FOR_WINDOW) else {
            return;
        };
        let allow: unsafe extern "system" fn(HWND, BOOL) -> i32 = core::mem::transmute(addr);
        allow(hwnd, BOOL::from(dark));
    }
}

/// The per-window dark visual style, which is the half `set_app_dark_mode`
/// cannot do on its own: the mode picks the palette the process reads, this
/// picks the style each control opens. The window-theme helper is the per-window
/// half: it always allows the mode and always names a sub-app — `DarkMode_CFD`
/// for `ComboBox` and for `Edit` without scrollbars, `DarkMode_Explorer` for
/// every other window, including `Button`, `Static`, `SysListView32` and the
/// dialog itself. The class-specific halves that follow only subclass (`Button`
/// group boxes and the two-choice buttons, framed/icon `Static`,
/// `SysTabControl32`, list views). A stock `ListBox` gets the separate
/// [`paint_stock_listbox`] subclass, which paints only its client rows and so
/// never clears the name that supplies the dark scrollbar.
/// The gating flags are `RtlGetNtVersionNumbers` build ≥ 17763.
pub(super) fn dark_theme(hwnd: HWND, dark: bool) {
    // 深浅色归进程那一格管，单个窗口只能要求浅色：见 [`APP_DARK`]。
    let dark = dark && dark_mode();
    allow_dark_mode(hwnd, dark);
    // The name is never skipped: a window left unnamed falls back to the
    // process app mode, which on this build still opens the light style, so a
    // push button draws a `#FDFDFD` face and a checkbox caption black.
    let name: &str = if dark {
        dark_sub_app(hwnd)
    } else {
        // NULL hands the window back the palette the process app-mode chooses,
        // which is what a light theme needs after a dark one was applied and
        // then switched.
        ""
    };
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let psz = PCWSTR(if name.is_empty() {
        core::ptr::null()
    } else {
        wide.as_ptr()
    });
    unsafe {
        let _ = SetWindowTheme(hwnd, psz, PCWSTR::null());
    }
    if dark {
        groupbox_hook(hwnd);
        choice_hook(hwnd);
    }
}

/// The sub-app the window-theme helper names for this window. `DarkMode_CFD` is chosen
/// by the `ComboBox` and `Edit` class compares,
/// which return 0 on a match — plus the `WS_HSCROLL |
/// WS_VSCROLL` bit, read only for `Edit`; anything else takes the fall-through
/// `DarkMode_Explorer`.
///
/// `SysListView32` takes that fall-through as well, deliberately. Naming it
/// `DarkMode_ItemsView` — the sub-app Explorer's own file list uses — paints the
/// rows and the header dark but leaves the **scrollbar** light: measured on this
/// machine, the same control draws a `#171717` track and a `#959595` thumb named
/// `DarkMode_Explorer`, and `#F0F0F0` / `#858585` named `DarkMode_ItemsView`.
/// Explorer's item view has no classic scrollbar of its own to recolour, so that
/// sub-app has no dark one to hand out.
fn dark_sub_app(hwnd: HWND) -> &'static str {
    let mut class = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut class) };
    if len <= 0 {
        return "DarkMode_Explorer";
    }
    let name = String::from_utf16_lossy(&class[..len as usize]);
    match name.as_str() {
        "ComboBox" => "DarkMode_CFD",
        "Edit" => {
            let scroll = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } & 0x0030_0000;
            if scroll == 0 {
                "DarkMode_CFD"
            } else {
                "DarkMode_Explorer"
            }
        }
        _ => "DarkMode_Explorer",
    }
}

/// the same ask for a window *and* every child it owns, which
/// is how a dialog's edits, combos, checkboxes and scroll arrows all land on the
/// dark style. A dialog that only asked for itself keeps light-mode controls.
pub(super) fn dark_theme_tree(hwnd: HWND, dark: bool) {
    dark_theme(hwnd, dark);
    if dark {
        hook_dialog_colours(hwnd);
    }
    unsafe {
        let _ = EnumChildWindows(Some(hwnd), Some(dark_theme_child), LPARAM(dark as isize));
    }
}

/// The subclass id for the dialog's own colour question.
const DLG_COLOUR_SUBCLASS: usize = 7;

/// `WM_CTLCOLORDLG` is the one colour question no dialog in this tree answered:
/// they all fill their own face in `WM_ERASEBKGND`, but the system still asks for
/// a brush, and the default `#32770` answer is `COLOR_3DFACE`. Routing the
/// question to the very same window's `WM_CTLCOLORBTN` gets the *dialog's own*
/// face brush and ink — every dialog already publishes those two there — without
/// growing one more arm in twenty window procedures. `SendMessageW`, not
/// `DefSubclassProc`, so windows that own their paint (the system message box)
/// answer through their place in the subclass chain too.
pub(super) fn hook_dialog_colours(hwnd: HWND) {
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(dlg_colour_proc), DLG_COLOUR_SUBCLASS, 0);
    }
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe extern "system" fn dlg_colour_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _refdata: usize,
) -> LRESULT {
    if msg == WM_CTLCOLORDLG {
        let self_hwnd = LPARAM(hwnd.0 as isize);
        let hdc = HDC(wp.0 as *mut core::ffi::c_void);
        // A window without a WM_CTLCOLORBTN arm (the find bar) leaves the ink
        // at the default black; its face is published on WM_CTLCOLORSTATIC.
        SetTextColor(hdc, COLORREF(0));
        let btn = SendMessageW(hwnd, WM_CTLCOLORBTN, Some(wp), Some(self_hwnd));
        if GetTextColor(hdc).0 != 0 {
            return btn;
        }
        return SendMessageW(hwnd, WM_CTLCOLORSTATIC, Some(wp), Some(self_hwnd));
    }
    if msg == WM_NCDESTROY {
        let _ = RemoveWindowSubclass(hwnd, Some(dlg_colour_proc), DLG_COLOUR_SUBCLASS);
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

/// The same ask for the controls only. A `#32770` that gets `SetWindowTheme` on
/// *itself* has its non-client frame recalculated with doubled metrics: the
/// window keeps its size and the client loses a whole frame, which drops the
/// template's bottom-most control below the client edge and clips it away. The
/// dialog's own face is painted by its window proc, so only the controls need
/// the palette.
pub(super) fn dark_theme_children(hwnd: HWND, dark: bool) {
    if dark {
        hook_dialog_colours(hwnd);
    }
    unsafe {
        let _ = EnumChildWindows(Some(hwnd), Some(dark_theme_child), LPARAM(dark as isize));
    }
}

/// Naming a top-level window without the attribute costs the caption: this implementation
/// measured a `#141414` bar with no title text and no close glyph `#4C4A48` bar with
/// both.
pub(super) fn dark_theme_window(hwnd: HWND, dark: bool) {
    dark_theme_tree(hwnd, dark);
    title_bar(hwnd, dark);
}

unsafe extern "system" fn dark_theme_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
    dark_theme(hwnd, lparam.0 != 0);
    BOOL(1)
}

/// The title bar's dark mode, which is per-window and therefore has to be set
/// again for every top-level window the app opens. 同样只认进程那一格：开关说
/// 浅色，标题栏就不跟着某一套深色配色走。
pub(super) fn title_bar(hwnd: HWND, dark: bool) {
    let on = BOOL::from(dark && dark_mode());
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &on as *const BOOL as *const core::ffi::c_void,
            core::mem::size_of::<BOOL>() as u32,
        );
    }
}

thread_local! {
    /// The armed box hook, while one `MessageBoxW` call is running on this
    /// thread. The box does not exist before that call, so it cannot be themed
    /// where the other dialogs are; the hook catches it the moment it
    /// activates.
    static BOX_HOOK: Cell<isize> = const { Cell::new(0) };
    /// The face and the ink, published for the subclass that paints the box.
    static BOX_COLOURS: Cell<(u32, u32)> = const { Cell::new((0, 0)) };
    /// Whether the running call wants the dark skin at all.
    static BOX_DARK: Cell<bool> = const { Cell::new(false) };
    /// One brush for the box's face, reused across its paint messages.
    static BOX_BRUSH: Cell<isize> = const { Cell::new(0) };
    static BOX_BRUSH_COLOUR: Cell<u32> = const { Cell::new(0) };
}

/// The subclass id for the box's own window procedure.
const BOX_SUBCLASS: usize = 6;

/// A system message box in the app's chrome.
///
/// The system still lays the box out — the button row, the icon at the icon
/// metric, the text padding are its own — but the face, the ink and the
/// controls are repainted from the `Theme`. This is the one window the dark
/// palette cannot reach from outside: `MessageBoxW` creates it after the call
/// has started, so a CBT hook catches it at activation, moves it over its
/// owner, and — for a dark theme only — themes its children and subclasses it
/// for the dialog face. A light theme keeps the system look.
pub(super) fn message_box(
    owner: Option<HWND>,
    text: &str,
    caption: &str,
    flags: MESSAGEBOX_STYLE,
    theme: &Theme,
) -> i32 {
    let text16: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let caption16: Vec<u16> = caption.encode_utf16().chain([0]).collect();
    // 这个框整个都是原生的：要不要按深色重刷，问进程那一格，不问手上这套底色 ——
    // 否则会出现「框是深色的、按钮是浅色的」那种半截。
    let dark = dark_mode();
    unsafe {
        if dark {
            set_chrome_theme(theme);
            BOX_COLOURS.with(|c| {
                c.set((
                    to_colorref(theme.toolbar_bg).0,
                    to_colorref(theme.toolbar_fg).0,
                ))
            });
        }
        BOX_DARK.with(|c| c.set(dark));
        install_box_hook();
        let answer = MessageBoxW(
            owner,
            PCWSTR(text16.as_ptr()),
            PCWSTR(caption16.as_ptr()),
            flags,
        )
        .0;
        remove_box_hook();
        answer
    }
}

unsafe fn install_box_hook() {
    BOX_HOOK.with(|slot| {
        if slot.get() != 0 {
            return;
        }
        let hook = SetWindowsHookExW(
            WH_CBT,
            Some(box_hook),
            Some(crate::win32::view::instance()),
            GetCurrentThreadId(),
        );
        if let Ok(hook) = hook {
            slot.set(hook.0 as isize);
        }
    });
}

unsafe fn remove_box_hook() {
    BOX_HOOK.with(|slot| {
        let raw = slot.get();
        if raw != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(raw as *mut core::ffi::c_void));
            slot.set(0);
        }
    });
}

unsafe extern "system" fn box_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code as u32 == HCBT_ACTIVATE {
        let hwnd = HWND(wparam.0 as *mut core::ffi::c_void);
        let mut class = [0u16; 8];
        let len = GetClassNameW(hwnd, &mut class);
        // `#32770` is the only top-level this call can activate.
        let dialog: [u16; 6] = [35, 51, 50, 55, 55, 48];
        if len == 6 && class[..6] == dialog {
            if BOX_DARK.with(|c| c.get()) {
                allow_dark_mode(hwnd, true);
                dark_theme_children(hwnd, true);
                title_bar(hwnd, true);
                // The box paints its own button strip in `WM_PAINT` with the
                // system face; the subclass fills the client itself, and the
                // children keep their own paint out of it.
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE) | (WS_CLIPCHILDREN.0 as isize);
                SetWindowLongPtrW(hwnd, GWL_STYLE, style);
                let _ = SetWindowSubclass(hwnd, Some(box_proc), BOX_SUBCLASS, 0);
            }
            // The system drops an owned message box at the centre of the screen;
            // it belongs over the window that asked.
            if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
                if !owner.0.is_null() {
                    prompt::center_on_owner(hwnd, owner);
                }
            }
            remove_box_hook();
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// The face and the ink a message box's own controls ask the dialog for.
unsafe extern "system" fn box_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut core::ffi::c_void), &rc, box_brush());
            }
            LRESULT(1)
        }
        // The box's own paint lays the button strip down in the system face.
        // Filling the client here replaces it; `WS_CLIPCHILDREN` keeps the
        // buttons and the message out of the fill.
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(hdc, &rc, box_brush());
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT => {
            let (face, ink) = BOX_COLOURS.with(|c| c.get());
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            let _ = SetTextColor(hdc, COLORREF(ink));
            let _ = SetBkColor(hdc, COLORREF(face));
            LRESULT(box_brush().0 as isize)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(box_proc), BOX_SUBCLASS);
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, msg, wparam, lparam),
    }
}

/// The face brush, rebuilt only when the published face changed.
fn box_brush() -> HBRUSH {
    let (face, _) = BOX_COLOURS.with(|c| c.get());
    let current = BOX_BRUSH.with(|b| b.get());
    if current != 0 && BOX_BRUSH_COLOUR.with(|c| c.get()) == face {
        return HBRUSH(current as *mut core::ffi::c_void);
    }
    unsafe {
        let brush = CreateSolidBrush(COLORREF(face));
        if current != 0 {
            let _ = DeleteObject(HBRUSH(current as *mut core::ffi::c_void).into());
        }
        BOX_BRUSH.with(|b| b.set(brush.0 as isize));
        BOX_BRUSH_COLOUR.with(|c| c.set(face));
        brush
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Controls::{
        CBS_CHECKEDDISABLED, CBS_CHECKEDNORMAL, CBS_CHECKEDPRESSED, CBS_MIXEDNORMAL,
        CBS_UNCHECKEDHOT, CBS_UNCHECKEDNORMAL, RBS_CHECKEDNORMAL, RBS_UNCHECKEDDISABLED,
    };

    /// Every check box and radio the templates carry is taken over; push
    /// buttons, group boxes, owner-drawn buttons and push-like toggles are not.
    #[test]
    fn choice_styles_are_taken_over_and_the_rest_are_left_alone() {
        assert_eq!(choice_style(0x5001_0003), Some(Choice::Check));
        assert_eq!(choice_style(0x5001_0002), Some(Choice::Check));
        assert_eq!(choice_style(BS_3STATE as u32), Some(Choice::Check));
        assert_eq!(choice_style(BS_AUTO3STATE as u32), Some(Choice::Check));
        assert_eq!(
            choice_style(BS_LEFTTEXT as u32 | 0x5001_0003),
            Some(Choice::Check)
        );
        assert_eq!(choice_style(0x5002_0009), Some(Choice::Radio));
        assert_eq!(choice_style(0x5000_0009), Some(Choice::Radio));
        assert_eq!(choice_style(BS_RADIOBUTTON as u32), Some(Choice::Radio));
        assert_eq!(choice_style(0x5001_0000), None);
        assert_eq!(choice_style(0x5001_0001), None);
        assert_eq!(choice_style(0x5000_0007), None);
        assert_eq!(choice_style(0x5001_000B), None);
        assert_eq!(choice_style(0x5001_0003 | BS_PUSHLIKE as u32), None);
    }

    #[test]
    fn captions_hide_mnemonic_prefixes() {
        let wide = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(hide_prefix(&wide("包含子文件夹(&I)")), wide("包含子文件夹(I)"));
        assert_eq!(hide_prefix(&wide("A && B")), wide("A & B"));
        assert_eq!(hide_prefix(&wide("plain")), wide("plain"));
    }

    /// A two-choice button's states are four consecutive ids per check state.
    #[test]
    fn choice_state_ids_follow_the_visual_style() {
        assert_eq!(
            choice_state_id(Choice::Check, true, 0),
            CBS_UNCHECKEDNORMAL.0
        );
        assert_eq!(
            choice_state_id(Choice::Check, true, BST_HOT),
            CBS_UNCHECKEDHOT.0
        );
        assert_eq!(
            choice_state_id(Choice::Check, true, 1),
            CBS_CHECKEDNORMAL.0
        );
        assert_eq!(
            choice_state_id(Choice::Check, true, BST_PUSHED | 1),
            CBS_CHECKEDPRESSED.0
        );
        assert_eq!(
            choice_state_id(Choice::Check, true, 2),
            CBS_MIXEDNORMAL.0
        );
        assert_eq!(
            choice_state_id(Choice::Check, false, 1),
            CBS_CHECKEDDISABLED.0
        );
        assert_eq!(
            choice_state_id(Choice::Radio, true, 1),
            RBS_CHECKEDNORMAL.0
        );
        assert_eq!(
            choice_state_id(Choice::Radio, false, 0),
            RBS_UNCHECKEDDISABLED.0
        );
    }

    // Values no real theme uses, so a pixel or a DC colour saying either of
    // them is unambiguous proof of which window supplied the palette.
    const PARENT_INK: u32 = 0x0012_3456;
    const PARENT_FACE: u32 = 0x0077_8899;

    thread_local! {
        static PARENT_BRUSH: Cell<isize> = const { Cell::new(0) };
    }

    unsafe extern "system" fn ctl_parent_proc(
        hwnd: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
    ) -> LRESULT {
        if msg == WM_CTLCOLORBTN || msg == WM_CTLCOLORLISTBOX {
            SetTextColor(HDC(wp.0 as *mut core::ffi::c_void), COLORREF(PARENT_INK));
            return LRESULT(PARENT_BRUSH.with(|b| b.get()));
        }
        windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wp, lp)
    }

    /// A `Button` subclass must not paint with the process-wide chrome table:
    /// the face and the ink have to be the ones its own parent publishes on
    /// `WM_CTLCOLORBTN`. The same answer is what `WM_CTLCOLORDLG` is routed to.
    #[test]
    fn the_button_face_and_ink_come_from_its_parent() {
        use windows::core::{PCWSTR, w};
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
            GetDC, GetPixel, GetTextColor, ReleaseDC, SelectObject,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, GWLP_WNDPROC, HMENU, SetWindowLongPtrW,
            WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD, WS_POPUP,
        };

        unsafe {
            let brush = CreateSolidBrush(COLORREF(PARENT_FACE));
            PARENT_BRUSH.with(|b| b.set(brush.0 as isize));

            let make = |class, parent, style| {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class,
                    PCWSTR::null(),
                    style,
                    0,
                    0,
                    40,
                    20,
                    parent,
                    Some(HMENU(0 as *mut core::ffi::c_void)),
                    Some(super::super::view::instance()),
                    None,
                )
                .expect("test window")
            };
            let parent = make(w!("STATIC"), None, WS_POPUP);
            let _ = SetWindowLongPtrW(
                parent,
                GWLP_WNDPROC,
                ctl_parent_proc as *const () as isize,
            );
            hook_dialog_colours(parent);
            let child = make(w!("BUTTON"), Some(parent), WINDOW_STYLE(WS_CHILD.0));

            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, 40, 20);
            let old_bmp = SelectObject(dc, HGDIOBJ(bmp.0));
            let rc = RECT {
                left: 0,
                top: 0,
                right: 40,
                bottom: 20,
            };

            // The subclass's own paint: the parent's brush fills the rect and
            // the parent's ink is what the caption will be drawn with.
            let ink = fill_parent_face(child, dc, &rc);
            assert_eq!(ink.0, PARENT_INK, "caption ink is the parent's");
            assert_eq!(
                GetPixel(dc, 5, 5).0,
                PARENT_FACE,
                "the rect is filled with the parent's brush"
            );

            // The dialog's own colour question goes to the very same answer.
            SetTextColor(dc, COLORREF(0));
            let routed = SendMessageW(
                parent,
                WM_CTLCOLORDLG,
                Some(WPARAM(dc.0 as usize)),
                Some(LPARAM(0)),
            );
            assert_eq!(routed.0, brush.0 as isize, "routed through the subclass");
            assert_eq!(GetTextColor(dc).0, PARENT_INK, "and sets the ink again");

            // A parent that leaves the question to DefWindowProc must not be
            // mistaken for an answer: the chrome table is the fallback then.
            let silent = make(w!("STATIC"), None, WS_POPUP);
            let quiet_child = make(w!("BUTTON"), Some(silent), WINDOW_STYLE(WS_CHILD.0));
            let saved = CHROME_CAPTION.load(Ordering::Acquire);
            let saved_face = CHROME_FACE.load(Ordering::Acquire);
            CHROME_CAPTION.store(0x00AB_CD_EF, Ordering::Release);
            CHROME_FACE.store(0x0011_2233, Ordering::Release);
            let fallback = fill_parent_face(quiet_child, dc, &rc);
            CHROME_CAPTION.store(saved, Ordering::Release);
            CHROME_FACE.store(saved_face, Ordering::Release);
            assert_eq!(fallback.0, 0x00AB_CD_EF, "silent parent uses the table's ink");
            assert_eq!(
                GetPixel(dc, 5, 5).0,
                0x0011_2233,
                "and not the default proc's stock face brush"
            );
            let _ = DestroyWindow(quiet_child);
            let _ = DestroyWindow(silent);

            let _ = SelectObject(dc, old_bmp);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(dc);
            let _ = ReleaseDC(None, screen);
            let _ = DestroyWindow(child);
            let _ = DestroyWindow(parent);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            PARENT_BRUSH.with(|b| b.set(0));
        }
    }

    /// Only a stock list is taken over: an ownerdraw one paints its own rows
    /// and must keep the theme that gives it a dark scrollbar.
    #[test]
    fn only_stock_list_styles_are_taken_over() {
        assert!(is_stock_listbox(0x50A1_0101));
        assert!(is_stock_listbox(0x50A1_0101 | 0x0200));
        assert!(!is_stock_listbox(0x50A1_0101 | 0x0010));
        assert!(!is_stock_listbox(0x50A1_0101 | 0x0020));
    }

    /// A stock list's rows are painted with the parent's field and ink, while
    /// the window keeps the sub-app name that supplies its dark scrollbar —
    /// the one thing un-naming it used to cost.
    #[test]
    fn a_themed_list_keeps_its_name_and_uses_the_parent_palette() {
        use windows::core::{PCWSTR, w};
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
            GetDC, GetPixel, GetTextColor, ReleaseDC, SelectObject,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, GWLP_WNDPROC, HMENU, LB_ADDSTRING, SetWindowLongPtrW,
            WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD, WS_POPUP,
        };

        unsafe {
            let brush = CreateSolidBrush(COLORREF(PARENT_FACE));
            PARENT_BRUSH.with(|b| b.set(brush.0 as isize));
            let parent = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                200,
                90,
                None,
                Some(HMENU(0 as *mut core::ffi::c_void)),
                Some(super::super::view::instance()),
                None,
            )
            .expect("test parent");
            let _ = SetWindowLongPtrW(
                parent,
                GWLP_WNDPROC,
                ctl_parent_proc as *const () as isize,
            );
            let list = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("LISTBOX"),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0 | 0x0020_0000), // WS_VSCROLL
                0,
                0,
                120,
                60,
                Some(parent),
                Some(HMENU(1200 as *mut core::ffi::c_void)), // control id
                Some(super::super::view::instance()),
                None,
            )
            .expect("test list");
            let text: Vec<u16> = "alpha\0".encode_utf16().collect();
            let _ = SendMessageW(
                list,
                LB_ADDSTRING,
                None,
                Some(LPARAM(text.as_ptr() as isize)),
            );
            let _ = SetWindowTheme(list, w!("DarkMode_Explorer"), PCWSTR::null());
            themed_listbox(parent, 1200);

            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, 120, 60);
            let old_bmp = SelectObject(dc, HGDIOBJ(bmp.0));
            // PrintWindow reaches a control through WM_PRINTCLIENT.
            let _ = SendMessageW(
                list,
                WM_PRINTCLIENT,
                Some(WPARAM(dc.0 as usize)),
                Some(LPARAM(2)), // PRF_CLIENT
            );
            assert_eq!(
                GetPixel(dc, 5, 5).0,
                PARENT_FACE,
                "the first row is filled with the parent's field brush"
            );
            assert_eq!(
                GetPixel(dc, 90, 40).0,
                PARENT_FACE,
                "and so is the empty space below the last row"
            );
            assert_eq!(
                GetTextColor(dc).0,
                PARENT_INK,
                "and the caption ink is the parent's"
            );

            // Only meaningful when visual styles are on: a window whose theme
            // name was cleared cannot open theme data at all.
            let reference = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0),
                0,
                0,
                10,
                10,
                Some(parent),
                Some(HMENU(0 as *mut core::ffi::c_void)),
                Some(super::super::view::instance()),
                None,
            )
            .expect("reference window");
            let _ = SetWindowTheme(reference, w!("DarkMode_Explorer"), PCWSTR::null());
            let baseline = OpenThemeData(Some(reference), w!("BUTTON"));
            if baseline.0 != 0 {
                let list_theme = OpenThemeData(Some(list), w!("BUTTON"));
                assert!(
                    list_theme.0 != 0,
                    "the list still carries its dark sub-app name"
                );
                let _ = CloseThemeData(list_theme);
                let _ = CloseThemeData(baseline);
                // Calibration: an un-named window really cannot open theme
                // data, so the check above would have caught the old fix.
                let _ = SetWindowTheme(list, w!(" "), w!(" "));
                let cleared = OpenThemeData(Some(list), w!("BUTTON"));
                assert_eq!(
                    cleared.0, 0,
                    "clearing the name is what costs the dark scrollbar"
                );
                if cleared.0 != 0 {
                    let _ = CloseThemeData(cleared);
                }
            }
            let _ = DestroyWindow(reference);

            let _ = SelectObject(dc, old_bmp);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(dc);
            let _ = ReleaseDC(None, screen);
            let _ = DestroyWindow(list);
            let _ = DestroyWindow(parent);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            PARENT_BRUSH.with(|b| b.set(0));
        }
    }
}
