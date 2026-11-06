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
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, DrawFocusRect, EndPaint, FillRect,
    GetStockObject, GetTextExtentPoint32W, RoundRect, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, TextOutW, HBRUSH, HGDIOBJ, HFONT, HDC, NULL_BRUSH, PAINTSTRUCT, PS_SOLID,
    TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::{
    BST_CHECKED, CloseThemeData, DrawThemeBackground, GetThemePartSize, OpenThemeData,
    SetWindowTheme, BP_RADIOBUTTON, TS_DRAW, RBS_CHECKEDDISABLED, RBS_CHECKEDNORMAL,
    RBS_UNCHECKEDDISABLED, RBS_UNCHECKEDNORMAL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, IsWindowEnabled};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BS_AUTORADIOBUTTON, BS_GROUPBOX, BS_RADIOBUTTON, CallNextHookEx, EnumChildWindows,
    GetClassNameW, GetClientRect, GetWindow, GetWindowLongPtrW, GetWindowTextW, MessageBoxW,
    SendMessageW, SetWindowLongPtrW, SetWindowsHookExW, UnhookWindowsHookEx, GWL_STYLE, GW_OWNER,
    HCBT_ACTIVATE, HHOOK, MESSAGEBOX_STYLE, WH_CBT, WM_CTLCOLORBTN, WM_CTLCOLOREDIT,
    WM_CTLCOLORSTATIC, WM_ERASEBKGND, WM_GETFONT, WM_NCDESTROY, WM_PAINT, WS_CLIPCHILDREN,
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

/// The chrome palette a group box is painted with: the dialog face behind the
/// caption, the caption
/// process-wide table (a process-wide table, filled when the theme is applied)
/// and reads it from the group-box subclass, so this does too — which is why
/// `set_chrome_theme` runs wherever a dialog asks for the dark style.
static CHROME_FACE: AtomicU32 = AtomicU32::new(0x0020_2020);
static CHROME_CAPTION: AtomicU32 = AtomicU32::new(0x00E0_E0E0);
static CHROME_LINE: AtomicU32 = AtomicU32::new(0x0046_4646);

/// Publishes the theme's chrome colours for [`groupbox_proc`].
pub(super) fn set_chrome_theme(theme: &Theme) {
    CHROME_FACE.store(to_colorref(theme.toolbar_bg).0, Ordering::Release);
    CHROME_CAPTION.store(to_colorref(theme.toolbar_fg).0, Ordering::Release);
    CHROME_LINE.store(to_colorref(theme.toolbar_border).0, Ordering::Release);
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
            let face = CreateSolidBrush(COLORREF(CHROME_FACE.load(Ordering::Acquire)));
            let _ = FillRect(hdc, &rc, face);
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
            let _ = DeleteObject(HGDIOBJ(face.0));
            if !text.is_empty() {
                let _ = SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(CHROME_CAPTION.load(Ordering::Acquire)));
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


/// `groupbox_hook`'s other half of the `Button` class: a radio button whose
/// caption the visual style owns. A radio does ask its parent for the colours —
/// the page's `WM_CTLCOLORSTATIC` arm runs for it and sets the theme's ink — and
/// then draws the label with the style's own text colour, which stays black on
/// the dark face. Check boxes take the parent's ink and need nothing, so this
/// only takes the two radio styles.
fn radio_hook(hwnd: HWND) {
    let mut class = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut class) };
    if len <= 0 {
        return;
    }
    if String::from_utf16_lossy(&class[..len as usize]) != "Button" {
        return;
    }
    if !is_radio_style(unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32) {
        return;
    }
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(radio_proc), 5, usize::default());
    }
}

/// The two `Button` type bits whose caption the style owns. A check box honours
/// the parent's `WM_CTLCOLORSTATIC` ink, so taking it here would only replace a
/// working paint with a hand-made one.
fn is_radio_style(style: u32) -> bool {
    let kind = style & 0xF;
    kind == BS_RADIOBUTTON as u32 || kind == BS_AUTORADIOBUTTON as u32
}

/// The face, the dark glyph and the caption, drawn in the order the style would
/// have drawn them: the ring comes from the button's own visual style so the
/// selected dot keeps the palette the sub-app name chose, and only the ink of the
/// caption is the chrome table's.
#[allow(unsafe_op_in_unsafe_fn)]
unsafe extern "system" fn radio_proc(
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
            let face = CreateSolidBrush(COLORREF(CHROME_FACE.load(Ordering::Acquire)));
            let _ = FillRect(hdc, &rc, face);
            let _ = DeleteObject(HGDIOBJ(face.0));
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let face = CreateSolidBrush(COLORREF(CHROME_FACE.load(Ordering::Acquire)));
            let _ = FillRect(hdc, &rc, face);
            let enabled = IsWindowEnabled(hwnd).as_bool();
            let checked = SendMessageW(hwnd, BM_GETCHECK, None, None).0 as u32 == BST_CHECKED.0;
            let state = match (enabled, checked) {
                (false, false) => RBS_UNCHECKEDDISABLED,
                (false, true) => RBS_CHECKEDDISABLED,
                (true, false) => RBS_UNCHECKEDNORMAL,
                (true, true) => RBS_CHECKEDNORMAL,
            };
            let theme = OpenThemeData(Some(hwnd), w!("BUTTON"));
            let mut ring = SIZE { cx: 16, cy: 16 };
            if theme.0 != 0 {
                if let Ok(size) =
                    GetThemePartSize(theme, Some(hdc), BP_RADIOBUTTON.0, state.0, None, TS_DRAW)
                {
                    if size.cx > 0 && size.cy > 0 {
                        ring = size;
                    }
                }
            }
            let top = (rc.bottom - ring.cy) / 2;
            let glyph = RECT {
                left: rc.left,
                top,
                right: rc.left + ring.cx,
                bottom: top + ring.cy,
            };
            if theme.0 != 0 {
                let _ = DrawThemeBackground(theme, hdc, BP_RADIOBUTTON.0, state.0, &glyph, None);
            }
            let mut text = vec![0u16; 256];
            let len = GetWindowTextW(hwnd, &mut text).max(0) as usize;
            let text = &text[..len.min(text.len())];
            let font = HFONT(SendMessageW(hwnd, WM_GETFONT, None, None).0 as *mut core::ffi::c_void);
            let old = SelectObject(hdc, HGDIOBJ(font.0));
            let mut size = SIZE::default();
            if !text.is_empty() {
                let _ = GetTextExtentPoint32W(hdc, text, &mut size);
                let _ = SetBkMode(hdc, TRANSPARENT);
                let ink = if enabled {
                    CHROME_CAPTION.load(Ordering::Acquire)
                } else {
                    CHROME_LINE.load(Ordering::Acquire)
                };
                SetTextColor(hdc, COLORREF(ink));
                let baseline = (rc.bottom - size.cy) / 2;
                let _ = TextOutW(hdc, glyph.right + 4, baseline, text);
                if GetFocus() == hwnd {
                    let mut frame = RECT {
                        left: glyph.right + 2,
                        top: baseline - 1,
                        right: glyph.right + 6 + size.cx,
                        bottom: baseline + size.cy + 1,
                    };
                    let _ = DrawFocusRect(hdc, &mut frame);
                }
            }
            let _ = SelectObject(hdc, old);
            if theme.0 != 0 {
                let _ = CloseThemeData(theme);
            }
            let _ = DeleteObject(HGDIOBJ(face.0));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(1)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(radio_proc), 5);
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
/// group boxes and radios, framed/icon `Static`, `SysTabControl32`, list views).
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
        radio_hook(hwnd);
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
    unsafe {
        let _ = EnumChildWindows(Some(hwnd), Some(dark_theme_child), LPARAM(dark as isize));
    }
}

/// The same ask for the controls only. A `#32770` that gets `SetWindowTheme` on
/// *itself* has its non-client frame recalculated with doubled metrics: the
/// window keeps its size and the client loses a whole frame, which drops the
/// template's bottom-most control below the client edge and clips it away. The
/// dialog's own face is painted by its window proc, so only the controls need
/// the palette.
pub(super) fn dark_theme_children(hwnd: HWND, dark: bool) {
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

    /// The styles the option pages actually carry (verified template
    /// words): only the two radios of 代理 take the hand-drawn caption, while the
    /// checkbox, push button and group-box styles keep their own paint.
    #[test]
    fn only_the_radio_styles_are_taken_over() {
        assert!(is_radio_style(0x5002_0009));
        assert!(is_radio_style(0x5000_0009));
        assert!(is_radio_style(BS_RADIOBUTTON as u32));
        assert!(!is_radio_style(0x5001_0003));
        assert!(!is_radio_style(0x5001_0000));
        assert!(!is_radio_style(0x5000_0007));
        assert!(!is_radio_style(0x5001_0001));
    }
}
