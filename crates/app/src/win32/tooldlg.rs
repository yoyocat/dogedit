//! The external-tool manager (`cm_config_tools`, 1580).
//!
//! A 263x238 DLU dialog: group combo, tool list, add/remove/icon buttons,
//! the title/command/arguments/start-directory fields with their `$(...)`
//! insertion arrows, and the action-mode page. Layout and control ids follow
//! the dialog template; the texts come from the language table.

#![allow(unsafe_op_in_unsafe_fn)]

use std::sync::OnceLock;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, CreateSolidBrush, DeleteObject, FillRect, GetDC,
    GetSysColorBrush, GetTextMetricsW, HBRUSH, HDC, HFONT, HGDIOBJ,
    ReleaseDC, SelectObject, TEXTMETRICW,
};
use windows::Win32::UI::Controls::{
    BST_CHECKED, LIST_VIEW_ITEM_STATE_FLAGS, LVCOLUMNW, LVITEMW,
    LVN_ITEMCHANGED, LVM_SETBKCOLOR, LVM_SETTEXTBKCOLOR,
    LVM_SETTEXTCOLOR, NMLISTVIEW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, CB_ADDSTRING, CB_GETCOUNT, CB_GETCURSEL,
    CB_GETITEMDATA, CB_RESETCONTENT, CB_SETCURSEL, CB_SETDROPPEDWIDTH,
    CB_SETITEMDATA, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
    GetClientRect, GWLP_USERDATA, GW_CHILD, GW_HWNDNEXT,
    GetDlgItem, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, IDCANCEL, IDOK,
    PostQuitMessage, RegisterClassW, SendMessageW,
    SetWindowLongPtrW, SetWindowTextW,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_ERASEBKGND, WNDCLASSW, WM_NOTIFY, WM_SETFONT, WS_CAPTION,
    WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
};
use windows::core::{PCWSTR, w};

use crate::win32::prompt::{
    CLASS_BUTTON, CLASS_COMBO, CLASS_EDIT, CLASS_STATIC, add_child,
    run_modal_loop, show_modal, wide,
};
use crate::win32::customdraw;
use crate::win32::view::instance;
use crate::win32::window::to_colorref;
use dg_render::theme::Theme;

/// The manager's window class, registered once
/// per process like the prompt box's.
const DLG_CLASS: PCWSTR = w!("DogEditToolMgr");

/// The control ids
/// reports them.
const ID_GROUP_COMBO: i32 = 1_226;
const ID_LIST: i32 = 1_214;
const ID_ADD: i32 = 1_200;
const ID_DELETE: i32 = 1_201;
const ID_ICON: i32 = 1_229;
const ID_ADD_GROUP: i32 = 1_231;
const ID_DEL_GROUP: i32 = 1_232;
const ID_TITLE: i32 = 1_206;
const ID_EXE: i32 = 1_207;
const ID_ARGS: i32 = 1_208;
const ID_INITDIR: i32 = 1_209;
const ID_VAR_EXE: i32 = 1_228;
const ID_VAR_ARGS: i32 = 1_211;
const ID_VAR_DIR: i32 = 1_212;
const ID_BROWSE_EXE: i32 = 1_210;
const ID_CAPTURE: i32 = 1_224;
const ID_ADVANCED: i32 = 1_216;
const ID_INPUT: i32 = 1_230;

/// The controls the tool-dialog enable pass enables only while a real tool sits behind the
/// selected row: the four edits, the three `$(...)` arrows, the browse button,
/// 高级, 动作模式 and 图标. 删除, 删除分组 and 接受输入 each have a rule of their
/// own and are not in this list.
const FIELD_CONTROLS: [i32; 11] = [
    ID_TITLE,
    ID_EXE,
    ID_ARGS,
    ID_INITDIR,
    ID_VAR_EXE,
    ID_BROWSE_EXE,
    ID_VAR_ARGS,
    ID_VAR_DIR,
    ID_ADVANCED,
    ID_CAPTURE,
    ID_ICON,
];

/// The template's combo style words, with the `WS_CHILD | WS_VISIBLE` that
/// [`add_child`] adds taken back out. 1226 and 1219-1221 are
/// `CBS_HASSTRINGS | CBS_DROPDOWNLIST` (1226 with `WS_TABSTOP`); 1224 and the
/// 高级 box's 1223/1225 also drop with `WS_VSCROLL | CBS_DISABLENOSCROLL |
/// CBS_AUTOHSCROLL`.
const STYLE_GROUP_COMBO: u32 = 0x0001_0203;
const STYLE_CAPTURE_COMBO: u32 = 0x0021_0843;
const STYLE_ADV_GROUP_COMBO: u32 = 0x0000_0203;
const STYLE_ADV_DROP_COMBO: u32 = 0x0020_0843;

/// The report view's style word, `0x5081C00D` in the template: `WS_BORDER |
/// LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOCOLUMNHEADER |
/// LVS_NOSORTHEADER`. The last two are why the single column below never shows
/// its caption or a sort arrow.
const STYLE_LIST: u32 = 0x0081_C00D;

/// `LVM_SETEXTENDEDLISTVIEWSTYLE`'s `lParam` in `0x20` is
/// `LVS_EX_FULLROWSELECT`, and the zero `wParam` mask makes it the *whole*
/// extended style rather than an addition — no grid lines, no double buffer.
const LIST_EXSTYLE: u32 = 0x20;

/// There is no visible header text even before `LVS_NOCOLUMNHEADER` hides
/// the header outright.
const LIST_CAPTION: &str = "";

/// The three `$(...)` insertion arrows — the template's only buttons whose
/// caption is a Marlett glyph rather than words. They are the ones
/// [`apply_marlett`] re-fonts after the UI-font broadcast.
const ARROWS: [i32; 3] = [ID_VAR_EXE, ID_VAR_ARGS, ID_VAR_DIR];

/// The arrows' template caption, verbatim: `6`, the down arrow in Marlett.
const ARROW_CAPTION: &str = "6";

/// The template's items at 1.6 logical px per
/// dialog unit: (x, y, w, h). The window's own
/// caption and border are the window's, not
/// these numbers'.
const RECT_GROUP: (i32, i32, i32, i32) = (11, 11, 304, 48);
const RECT_LIST: (i32, i32, i32, i32) = (11, 35, 304, 139);
const RECT_ADD: (i32, i32, i32, i32) = (323, 11, 86, 22);
const RECT_DELETE: (i32, i32, i32, i32) = (323, 35, 86, 22);
const RECT_ICON: (i32, i32, i32, i32) = (323, 96, 86, 22);
const RECT_ADD_GROUP: (i32, i32, i32, i32) = (323, 128, 86, 22);
const RECT_DEL_GROUP: (i32, i32, i32, i32) = (323, 152, 86, 22);
const RECT_L_TITLE: (i32, i32, i32, i32) = (11, 186, 96, 19);
const RECT_L_EXE: (i32, i32, i32, i32) = (11, 208, 96, 19);
const RECT_L_ARGS: (i32, i32, i32, i32) = (11, 232, 96, 19);
const RECT_L_DIR: (i32, i32, i32, i32) = (11, 256, 104, 19);
const RECT_TITLE: (i32, i32, i32, i32) = (115, 181, 200, 21);
const RECT_EXE: (i32, i32, i32, i32) = (115, 205, 200, 21);
const RECT_VAR_EXE: (i32, i32, i32, i32) = (323, 205, 24, 21);
const RECT_BROWSE_EXE: (i32, i32, i32, i32) = (352, 205, 24, 21);
const RECT_ARGS: (i32, i32, i32, i32) = (115, 229, 200, 21);
const RECT_VAR_ARGS: (i32, i32, i32, i32) = (323, 229, 24, 21);
const RECT_DIR: (i32, i32, i32, i32) = (115, 253, 200, 21);
const RECT_VAR_DIR: (i32, i32, i32, i32) = (323, 253, 24, 21);
const RECT_L_CAPTURE: (i32, i32, i32, i32) = (11, 280, 96, 16);
const RECT_CAPTURE: (i32, i32, i32, i32) = (115, 277, 202, 48);
const RECT_ADVANCED: (i32, i32, i32, i32) = (323, 275, 88, 22);
const RECT_INPUT: (i32, i32, i32, i32) = (115, 306, 240, 19);
const RECT_RULE: (i32, i32, i32, i32) = (8, 334, 397, 2);
const RECT_OK: (i32, i32, i32, i32) = (216, 347, 96, 22);
const RECT_CANCEL: (i32, i32, i32, i32) = (315, 347, 91, 22);
/// The client size the template's 263x238
/// dialog units come to at 1.6, and the window
/// that client sits in.
const CLIENT_W: i32 = 421;
const CLIENT_H: i32 = 381;
const DLG_W: i32 = CLIENT_W + 15;
const DLG_H: i32 = CLIENT_H + 38;

/// The order is the template's own — 1, 2, 9, 8, 3, 4, 5, 6, 7 —
/// which the 输出模式 captions repeat row for row. The first row is
/// `Capture`'s default.
pub(super) const CAPTURE_ROWS: &[(dg_core::CaptureMode, &str)] = &[
    (dg_core::CaptureMode::None, "无"),
    (dg_core::CaptureMode::CaptureHide, "捕获输出(隐藏窗口)"),
    (dg_core::CaptureMode::CaptureShow, "捕获输出(显示窗口)"),
    (dg_core::CaptureMode::WebPreview, "捕获到Web预览"),
    (dg_core::CaptureMode::Insert, "插入"),
    (dg_core::CaptureMode::InsertNewLine, "从新行插入"),
    (dg_core::CaptureMode::ReplaceLine, "替换当前行"),
    (dg_core::CaptureMode::ReplaceDocument, "替换文档"),
    (dg_core::CaptureMode::RunHidden, "隐藏方式运行"),
];

/// The 保存模式 rows, in the order the 高级 dialog's combo lists them.
pub(super) const SAVE_ROWS: &[(dg_core::SaveMode, &str)] = &[
    (dg_core::SaveMode::None, "无"),
    (dg_core::SaveMode::Current, "保存当前文档"),
    (dg_core::SaveMode::All, "保存全部文档"),
];

/// The dialog's live state. The pointer
/// travels through `GWLP_USERDATA`, so the
/// state outlives the window it is attached
/// to; the frame owns the groups afterwards.
pub(super) struct ToolDlg<'a> {
    dir: &'a std::path::Path,
    groups: &'a mut Vec<dg_core::ToolGroup>,
    /// The group the combo has selected, an
    /// index into `groups`.
    group: usize,
    /// The tool the list has selected, an
    /// index into the group's tools. `None`
    /// with an empty list, or while the
    /// fields show a brand-new tool.
    tool: Option<usize>,
    /// Whether the list's selection change is
    /// the dialog's own doing — the list
    /// reports a change when it is cleared,
    /// and that must not wipe the fields.
    loading: bool,
    /// Set when 确定 was used, so the frame
    /// knows to reload.
    accepted: bool,
    /// Whether [`setup_list`] has run. The
    /// fill runs after every edit, but the
    /// column may only be inserted once.
    list_ready: bool,
    /// The box proc's own destroy flag, for
    /// the modal loop's `WM_QUIT` bookkeeping.
    destroyed: bool,
    /// The same one [`grepdlg`](super::grepdlg) reads.
    theme: Theme,
    /// the UI-font helper's UI font, sent to
    /// every child the way the icon-title font
    /// does; it is also what gives the combos
    /// their proper height.
    font: HFONT,
    /// The dialog face (`toolbar_bg`).
    bg: HBRUSH,
    /// The edit and combo fields
    /// (`toolbar_field`).
    face: HBRUSH,
    /// the font-size routine's Marlett: the three
    /// `$(...)` arrows are push buttons whose
    /// `6` caption is a down arrow in that face
    /// and a plain digit in any other.
    marlett: HFONT,
}

impl ToolDlg<'_> {
    /// Frees the two fonts and the two brushes.
    fn release(&self) {
        unsafe {
            free_chrome(self.font, self.bg, self.face);
            if !self.marlett.0.is_null() {
                let _ = DeleteObject(self.marlett.into());
            }
        }
    }
}

/// Runs the manager over `groups`. Returns
/// `true` when the user pressed 确定 — the
/// groups have been written to disk and the
/// caller should reload its flat list.
pub(super) fn run(
    owner: HWND,
    theme: &Theme,
    dir: &std::path::Path,
    groups: &mut Vec<dg_core::ToolGroup>,
) -> bool {
    let mut state = ToolDlg {
        dir,
        groups,
        group: 0,
        tool: None,
        loading: false,
        accepted: false,
        list_ready: false,
        destroyed: false,
        theme: theme.clone(),
        font: HFONT::default(),
        bg: HBRUSH::default(),
        face: HBRUSH::default(),
        marlett: HFONT::default(),
    };
    unsafe {
        let dpi = crate::win32::uifont::window_dpi(owner);
        state.font = crate::win32::uifont::create(dpi);
        state.marlett = super::grepdlg::marlett_font(dpi);
        state.bg = CreateSolidBrush(to_colorref(theme.toolbar_bg));
        state.face = CreateSolidBrush(to_colorref(theme.toolbar_field));
    }
    run_dlg(owner, &mut state);
    state.release();
    state.accepted
}

/// Builds the dialog's children at the
/// template's rectangles, then runs the modal
/// loop.
fn run_dlg(owner: HWND, state: &mut ToolDlg<'_>) {
    unsafe {
        register_class();
        let dpi = GetDpiForWindow(owner).max(96) as f32 / 96.0;
        let scale = |v: i32| (v as f32 * dpi).round() as i32;

        let title16 = wide("外部工具");
        let state_ptr = state as *mut ToolDlg<'_> as *const core::ffi::c_void;
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            DLG_CLASS,
            PCWSTR::from_raw(title16.as_ptr()),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            scale(DLG_W),
            scale(DLG_H),
            Some(owner),
            None,
            Some(instance()),
            Some(state_ptr),
        ) else {
            return;
        };

        // The group combo and the tool list.
        add_child(
            hwnd,
            CLASS_COMBO,
            PCWSTR::null(),
            ID_GROUP_COMBO,
            WINDOW_STYLE(STYLE_GROUP_COMBO),
            RECT_GROUP,
            dpi,
        );
        add_child(
            hwnd,
            w!("SysListView32"),
            PCWSTR::null(),
            ID_LIST,
            WINDOW_STYLE(STYLE_LIST),
            RECT_LIST,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(wide("添加").as_ptr()),
            ID_ADD,
            WS_TABSTOP,
            RECT_ADD,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(wide("删除").as_ptr()),
            ID_DELETE,
            WS_TABSTOP,
            RECT_DELETE,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(wide("图标").as_ptr()),
            ID_ICON,
            WS_TABSTOP,
            RECT_ICON,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(wide("添加分组").as_ptr()),
            ID_ADD_GROUP,
            WS_TABSTOP,
            RECT_ADD_GROUP,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(wide("删除分组").as_ptr()),
            ID_DEL_GROUP,
            WS_TABSTOP,
            RECT_DEL_GROUP,
            dpi,
        );

        // The four fields under their statics.
        for (id, text, rect) in [
            (ID_TITLE, "标题:", RECT_L_TITLE),
            (ID_EXE, "命令:", RECT_L_EXE),
            (ID_ARGS, "参数:", RECT_L_ARGS),
            (ID_INITDIR, "初始目录:", RECT_L_DIR),
        ] {
            add_child(
                hwnd,
                CLASS_STATIC,
                PCWSTR::from_raw(wide(text).as_ptr()),
                id + 1_794, // 3000..3003
                WINDOW_STYLE::default(),
                rect,
                dpi,
            );
        }
        let edit_style = WINDOW_STYLE(0x0081_0080); // ES_AUTOHSCROLL | WS_BORDER | ...
        for (id, rect) in [
            (ID_TITLE, RECT_TITLE),
            (ID_EXE, RECT_EXE),
            (ID_ARGS, RECT_ARGS),
            (ID_INITDIR, RECT_DIR),
        ] {
            add_child(
                hwnd,
                CLASS_EDIT,
                PCWSTR::null(),
                id,
                edit_style,
                rect,
                dpi,
            );
        }
        // The `$(...)` insertion arrows and the 命令 browse button. The
        // caption is a single `6` for the arrows: the template spells
        // it that way.
        for (id, rect) in [
            (ID_VAR_EXE, RECT_VAR_EXE),
            (ID_VAR_ARGS, RECT_VAR_ARGS),
            (ID_VAR_DIR, RECT_VAR_DIR),
        ] {
            add_child(
                hwnd,
                CLASS_BUTTON,
                PCWSTR::from_raw(wide(ARROW_CAPTION).as_ptr()),
                id,
                WS_TABSTOP,
                rect,
                dpi,
            );
        }
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(wide("...").as_ptr()),
            ID_BROWSE_EXE,
            WS_TABSTOP,
            RECT_BROWSE_EXE,
            dpi,
        );

        // 动作模式 and its combo, 高级, 接受输入.
        add_child(
            hwnd,
            CLASS_STATIC,
            PCWSTR::from_raw(wide("动作模式:").as_ptr()),
            3_004,
            WINDOW_STYLE::default(),
            RECT_L_CAPTURE,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_COMBO,
            PCWSTR::null(),
            ID_CAPTURE,
            WINDOW_STYLE(STYLE_CAPTURE_COMBO),
            RECT_CAPTURE,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(wide("高级").as_ptr()),
            ID_ADVANCED,
            WS_TABSTOP,
            RECT_ADVANCED,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(wide("接受输入").as_ptr()),
            ID_INPUT,
            WINDOW_STYLE(0x0001_0003), // BS_AUTOCHECKBOX
            RECT_INPUT,
            dpi,
        );
        // The rule above the buttons.
        add_child(
            hwnd,
            CLASS_STATIC,
            PCWSTR::null(),
            0,
            WINDOW_STYLE::default(),
            RECT_RULE,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(wide("确定").as_ptr()),
            IDOK.0 as i32,
            WINDOW_STYLE(0x0001_0001), // BS_DEFPUSHBUTTON
            RECT_OK,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(wide("取消").as_ptr()),
            IDCANCEL.0 as i32,
            WS_TABSTOP,
            RECT_CANCEL,
            dpi,
        );

        // the icon-title font: the UI font goes to every child first, then the
        // chrome asks for the dark visual style on the controls — a dialog that
        // only asked for itself would keep light-mode edits and combos.
        crate::win32::lang::localize_dialog(hwnd, 203);
        apply_font(hwnd, state.font);
        apply_marlett(hwnd, state.marlett);
        let dark = crate::win32::chrome::theme_is_dark(&state.theme);
        crate::win32::chrome::set_chrome_theme(&state.theme);
        crate::win32::chrome::dark_theme_children(hwnd, dark);
        crate::win32::chrome::title_bar(hwnd, dark);
        paint_list(hwnd, &state.theme);

        show_modal(hwnd, owner);
        fill_from_state(hwnd, state);

        run_modal_loop(hwnd, owner, std::ptr::addr_of!(state.destroyed));
    }
}

/// the icon-title font: `WM_SETFONT` with the UI font for every child of `hwnd`,
/// `GW_CHILD` then `GW_HWNDNEXT`. Re-measuring is what makes a combo as tall as
/// its font rather than as tall as the template's `CBS_DROPDOWNLIST` default.
unsafe fn apply_font(hwnd: HWND, font: HFONT) {
    if font.0.is_null() {
        return;
    }
    let mut child = GetWindow(hwnd, GW_CHILD).unwrap_or_default();
    while !child.0.is_null() {
        let _ = SendMessageW(
            child,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(0)),
        );
        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
    }
}

/// the font-size routine's Marlett for the three `$(...)` arrows (1228/1211/1212).
/// The template gives them the caption `6`, which is the down arrow in Marlett
/// and the digit in every other face — and since the UI font above reaches
/// *every* child, this has to run after it, the same order `grepdlg` uses for
/// its four arrows.
unsafe fn apply_marlett(hwnd: HWND, marlett: HFONT) {
    if marlett.0.is_null() {
        return;
    }
    for id in ARROWS {
        if let Ok(child) = GetDlgItem(Some(hwnd), id) {
            let _ = SendMessageW(
                child,
                WM_SETFONT,
                Some(WPARAM(marlett.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    }
}

/// The report list's own three colours: the themed controls paint their frame
/// and header, but a list view still fills its rows from `COLOR_WINDOW` unless
/// it is told otherwise.
unsafe fn paint_list(hwnd: HWND, theme: &Theme) {
    let Ok(list) = GetDlgItem(Some(hwnd), ID_LIST) else {
        return;
    };
    for (msg, color) in [
        (LVM_SETBKCOLOR, theme.toolbar_field),
        (LVM_SETTEXTBKCOLOR, theme.toolbar_field),
        (LVM_SETTEXTCOLOR, theme.toolbar_fg),
    ] {
        SendMessageW(
            list,
            msg,
            Some(WPARAM(0)),
            Some(LPARAM(to_colorref(color).0 as isize)),
        );
    }
}

unsafe fn register_class() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| unsafe {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(tool_proc),
            hInstance: instance(),
            lpszClassName: DLG_CLASS,
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            eprintln!("tool manager class registration failed");
        }
    });
}

// ------------------------------------------------------------------
// list-view plumbing
// ------------------------------------------------------------------

/// Replaces the list's rows with the
/// group's tools. One row per tool, the
/// title in the report view's single
/// `LVIF_TEXT` with `iSubItem = 0` and
/// nothing else, so the command is not a
/// second column.
unsafe fn fill_list(hwnd: HWND, tools: &[dg_core::Tool]) {
    let Ok(list) = GetDlgItem(Some(hwnd), ID_LIST) else {
        return;
    };
    SendMessageW(
        list,
        windows::Win32::UI::Controls::LVM_DELETEALLITEMS,
        Some(WPARAM(0)),
        Some(LPARAM(0)),
    );
    for (i, tool) in tools.iter().enumerate() {
        // The selection handler reads the row's text back out of the list rather than off
        // the object.
        let mut item = LVITEMW {
            mask: windows::Win32::UI::Controls::LVIF_TEXT
                | windows::Win32::UI::Controls::LVIF_PARAM,
            iItem: i as i32,
            iSubItem: 0,
            lParam: LPARAM(i as isize),
            ..Default::default()
        };
        let title16 = wide(&tool.title);
        item.pszText = windows::core::PWSTR(
            title16.as_ptr() as *mut u16,
        );
        SendMessageW(
            list,
            windows::Win32::UI::Controls::LVM_INSERTITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(&item as *const _ as isize)),
        );
    }
}

/// The list's selected row, if any.
unsafe fn list_selection(hwnd: HWND) -> Option<usize> {
    let Ok(list) = GetDlgItem(Some(hwnd), ID_LIST) else {
        return None;
    };
    // LVM_GETNEXTITEM with LVNI_SELECTED
    let r = SendMessageW(
        list,
        windows::Win32::UI::Controls::LVM_GETNEXTITEM,
        Some(WPARAM(usize::MAX)), // -1
        Some(LPARAM(0x0002)),     // LVNI_SELECTED
    );
    if r.0 < 0 { None } else { Some(r.0 as usize) }
}

/// Selects one row of the list.
unsafe fn list_select(hwnd: HWND, index: usize) {
    let Ok(list) = GetDlgItem(Some(hwnd), ID_LIST) else {
        return;
    };
    let mut item = LVITEMW {
        mask: windows::Win32::UI::Controls::LVIF_STATE,
        state: LIST_VIEW_ITEM_STATE_FLAGS(
            windows::Win32::UI::Controls::LVIS_SELECTED.0
                | windows::Win32::UI::Controls::LVIS_FOCUSED.0,
        ),
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(
            windows::Win32::UI::Controls::LVIS_SELECTED.0
                | windows::Win32::UI::Controls::LVIS_FOCUSED.0,
        ),
        iItem: index as i32,
        ..Default::default()
    };
    SendMessageW(
        list,
        windows::Win32::UI::Controls::LVM_SETITEMSTATE,
        Some(WPARAM(index)),
        Some(LPARAM(&mut item as *mut _ as isize)),
    );
}

/// `EnableWindow` by control id, for the pass below.
unsafe fn enable(hwnd: HWND, id: i32, on: bool) {
    if let Ok(control) = GetDlgItem(Some(hwnd), id) {
        let _ = EnableWindow(control, on);
    }
}

/// the tool-dialog enable pass, the dialog's whole enable pass. It reads the
/// selected row's *text* back out of the list: a title starting with `-` is
/// a separator, which selects (so 删除 stays live) but carries no tool, so the
/// eleven field controls go disabled. 删除分组 needs a group other than the
/// first.
unsafe fn update_enables(hwnd: HWND, state: &ToolDlg<'_>) {
    let tool = state
        .groups
        .get(state.group)
        .and_then(|g| state.tool.and_then(|i| g.tools.get(i)));
    let separator = tool.is_some_and(|t| t.title.starts_with('-'));
    let editing = tool.is_some() && !separator;
    for id in FIELD_CONTROLS {
        enable(hwnd, id, editing);
    }
    // Either flag is enough: a separator row keeps 删除 live even though the fields go
    // dark.
    enable(hwnd, ID_DELETE, editing || separator);
    enable(
        hwnd,
        ID_DEL_GROUP,
        combo_index(hwnd, ID_GROUP_COMBO).unwrap_or(0) != 0,
    );
    enable(
        hwnd,
        ID_INPUT,
        tool.is_some_and(|t| {
            matches!(
                t.action_pattern,
                dg_core::CaptureMode::CaptureHide
                    | dg_core::CaptureMode::CaptureShow
            )
        }),
    );
}

/// The extended style goes in first: `LVM_SETEXTENDEDLISTVIEWSTYLE` with a zero
/// mask, which *replaces* the whole set — `LVS_EX_FULLROWSELECT` and nothing
/// else. Immediately overwrites the width with the list's own width less `4 *
/// tmAveCharWidth` (`LVM_SETCOLUMNWIDTH`). The caption is the empty global
/// `psz`, so with `LVS_NOCOLUMNHEADER` the header never shows at all — there is
/// no 标题/命令 pair the way a two-column implementation would have.
unsafe fn setup_list(hwnd: HWND, font: HFONT) {
    let Ok(list) = GetDlgItem(Some(hwnd), ID_LIST) else {
        return;
    };
    SendMessageW(
        list,
        windows::Win32::UI::Controls::LVM_SETEXTENDEDLISTVIEWSTYLE,
        Some(WPARAM(0)),
        Some(LPARAM(LIST_EXSTYLE as isize)),
    );
    let mut rect = RECT::default();
    let _ = GetWindowRect(list, &mut rect);
    let width = (rect.right - rect.left) - 4 * ave_char_width(font);
    let caption = wide(LIST_CAPTION);
    let col = LVCOLUMNW {
        mask: windows::Win32::UI::Controls::LVCF_FMT
            | windows::Win32::UI::Controls::LVCF_WIDTH
            | windows::Win32::UI::Controls::LVCF_TEXT
            | windows::Win32::UI::Controls::LVCF_SUBITEM,
        cx: width.max(0),
        pszText: windows::core::PWSTR(caption.as_ptr() as *mut u16),
        ..Default::default()
    };
    // The column struct borrows the caption for the duration of the call.
    SendMessageW(
        list,
        windows::Win32::UI::Controls::LVM_INSERTCOLUMNW,
        Some(WPARAM(0)),
        Some(LPARAM(&col as *const _ as isize)),
    );
}

// ------------------------------------------------------------------
// field plumbing
// ------------------------------------------------------------------

/// Reads an edit's text.
unsafe fn edit_text(hwnd: HWND, id: i32) -> String {
    let Ok(e) = GetDlgItem(Some(hwnd), id) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(e);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(e, &mut buf);
    buf.truncate(n.max(0) as usize);
    String::from_utf16_lossy(&buf)
}

/// Writes an edit's text.
unsafe fn set_edit_text(hwnd: HWND, id: i32, text: &str) {
    if let Ok(e) = GetDlgItem(Some(hwnd), id) {
        let t = wide(text);
        let _ = SetWindowTextW(e, PCWSTR::from_raw(t.as_ptr()));
    }
}

/// The combo's current selection.
unsafe fn combo_index(hwnd: HWND, id: i32) -> Option<usize> {
    let Ok(c) = GetDlgItem(Some(hwnd), id) else {
        return None;
    };
    let r = SendMessageW(
        c,
        CB_GETCURSEL,
        Some(WPARAM(0)),
        Some(LPARAM(0)),
    );
    if r.0 < 0 { None } else { Some(r.0 as usize) }
}

/// Fills the whole dialog from `state`:
/// the group combo, the list, the fields.
unsafe fn fill_from_state(hwnd: HWND, state: &mut ToolDlg<'_>) {
    state.loading = true;
    if let Ok(combo) = GetDlgItem(Some(hwnd), ID_GROUP_COMBO) {
        SendMessageW(
            combo,
            CB_RESETCONTENT,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        );
        for g in state.groups.iter() {
            let t = wide(&g.name);
            SendMessageW(
                combo,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(t.as_ptr() as isize)),
            );
        }
        if !state.groups.is_empty() {
            SendMessageW(
                combo,
                CB_SETCURSEL,
                Some(WPARAM(state.group)),
                Some(LPARAM(0)),
            );
        }
    }
    // this fill runs after every edit, and `LVM_INSERTCOLUMNW` has no "if
    // not present" form — a second call would stack a second column on the
    // report view.
    if !state.list_ready {
        setup_list(hwnd, state.font);
        state.list_ready = true;
    }
    let tools = state
        .groups
        .get(state.group)
        .map(|g| g.tools.as_slice())
        .unwrap_or(&[]);
    fill_list(hwnd, tools);
    if let Some(i) = state.tool {
        list_select(hwnd, i);
    }
    fill_fields(hwnd, state);
    state.loading = false;
    update_enables(hwnd, state);
}

/// Fills the four fields, the capture combo and the 接受输入 box from the
/// selected tool. The dialog opens empty rather than showing a
/// made-up tool.
unsafe fn fill_fields(hwnd: HWND, state: &ToolDlg<'_>) {
    let Some(tool) = state
        .groups
        .get(state.group)
        .and_then(|g| state.tool.and_then(|i| g.tools.get(i)))
    else {
        return;
    };
    set_edit_text(hwnd, ID_TITLE, &tool.title);
    set_edit_text(hwnd, ID_EXE, &tool.exe_path);
    set_edit_text(hwnd, ID_ARGS, &tool.args);
    set_edit_text(hwnd, ID_INITDIR, &tool.init_dir);
    if let Ok(combo) = GetDlgItem(Some(hwnd), ID_CAPTURE) {
        SendMessageW(
            combo,
            CB_RESETCONTENT,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        );
        for (_, caption) in CAPTURE_ROWS {
            let t = wide(caption);
            SendMessageW(
                combo,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(t.as_ptr() as isize)),
            );
        }
        let row = CAPTURE_ROWS
            .iter()
            .position(|(m, _)| *m == tool.action_pattern)
            .unwrap_or(0);
        SendMessageW(
            combo,
            CB_SETCURSEL,
            Some(WPARAM(row)),
            Some(LPARAM(0)),
        );
    }
    if let Ok(chk) = GetDlgItem(Some(hwnd), ID_INPUT) {
        SendMessageW(
            chk,
            BM_SETCHECK,
            Some(WPARAM(if tool.input {
                BST_CHECKED.0 as usize
            } else {
                0
            })),
            Some(LPARAM(0)),
        );
    }
}

/// Writes the four fields, the capture
/// combo and the 接受输入 box back into the
/// selected tool. A tool that is all
/// blanks is not stored — 添加 puts a
/// blank row up and the user may walk
/// away without filling it.
unsafe fn save_fields(hwnd: HWND, state: &mut ToolDlg<'_>) {
    let Some(group) = state.groups.get_mut(state.group) else {
        return;
    };
    let Some(index) = state.tool else {
        return;
    };
    let Some(tool) = group.tools.get_mut(index) else {
        return;
    };
    tool.title = edit_text(hwnd, ID_TITLE);
    tool.exe_path = edit_text(hwnd, ID_EXE);
    tool.args = edit_text(hwnd, ID_ARGS);
    tool.init_dir = edit_text(hwnd, ID_INITDIR);
    if let Some(row) = combo_index(hwnd, ID_CAPTURE) {
        if let Some((mode, _)) = CAPTURE_ROWS.get(row) {
            tool.action_pattern = *mode;
        }
    }
    if let Ok(chk) = GetDlgItem(Some(hwnd), ID_INPUT) {
        let r = SendMessageW(
            chk,
            BM_GETCHECK,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        );
        tool.input = r.0 as u32 == BST_CHECKED.0;
    }
    // The list's row shows the new title.
    let _ = group;
    let title16 = wide(&tool.title);
    let mut item = LVITEMW {
        mask: windows::Win32::UI::Controls::LVIF_TEXT,
        iItem: index as i32,
        iSubItem: 0,
        ..Default::default()
    };
    item.pszText = windows::core::PWSTR(title16.as_ptr() as *mut u16);
    if let Ok(list) = GetDlgItem(Some(hwnd), ID_LIST) {
        SendMessageW(
            list,
            windows::Win32::UI::Controls::LVM_SETITEMTEXTW,
            Some(WPARAM(index)),
            Some(LPARAM(&item as *const _ as isize)),
        );
    }
}

// ------------------------------------------------------------------
// group and tool commands
// ------------------------------------------------------------------

/// Selects a group, carrying the edited
/// fields over to the tool they belong to
/// first.
unsafe fn select_group(hwnd: HWND, state: &mut ToolDlg<'_>, index: usize) {
    if index >= state.groups.len() || index == state.group {
        return;
    }
    save_fields(hwnd, state);
    state.group = index;
    state.tool = None;
    fill_from_state(hwnd, state);
}

/// Selects a tool row, saving the one the fields hold first. A
/// separator row is selected like any other but carries no tool.
unsafe fn select_tool(hwnd: HWND, state: &mut ToolDlg<'_>, index: usize) {
    save_fields(hwnd, state);
    state.tool = Some(index);
    let separator = state
        .groups
        .get(state.group)
        .and_then(|g| g.tools.get(index))
        .is_some_and(|t| t.title.starts_with('-'));
    if !separator {
        fill_fields(hwnd, state);
    }
}

/// The group is created if the manager was opened with none.
unsafe fn add_tool(hwnd: HWND, state: &mut ToolDlg<'_>) {
    save_fields(hwnd, state);
    if state.groups.is_empty() {
        state.groups.push(dg_core::ToolGroup {
            name: String::from("工具"),
            tools: Vec::new(),
        });
        state.group = 0;
    }
    let group = &mut state.groups[state.group];
    let uuid = dg_core::next_uuid(group.tools.iter());
    let mut tool = dg_core::Tool::blank();
    tool.uuid = uuid;
    group.tools.push(tool);
    state.tool = Some(group.tools.len() - 1);
    fill_from_state(hwnd, state);
}

/// 删除 (1201): the selected tool leaves
/// its group.
unsafe fn delete_tool(hwnd: HWND, state: &mut ToolDlg<'_>) {
    let Some(index) = state.tool else {
        return;
    };
    save_fields(hwnd, state);
    let Some(group) = state.groups.get_mut(state.group) else {
        return;
    };
    if index < group.tools.len() {
        group.tools.remove(index);
    }
    state.tool = None;
    fill_from_state(hwnd, state);
}

/// 添加分组 (1231): a new empty group, named
/// by the user, selected at once.
unsafe fn add_group(hwnd: HWND, state: &mut ToolDlg<'_>) {
    save_fields(hwnd, state);
    let Some(name) = crate::win32::prompt::prompt(
        hwnd,
        "外部工具",
        "请输入分组名称:",
        "",
    ) else {
        return;
    };
    if name.trim().is_empty() {
        return;
    }
    if let Some(existing) =
        state.groups.iter().position(|g| g.name == name)
    {
        state.group = existing;
        state.tool = None;
        fill_from_state(hwnd, state);
        return;
    }
    state.groups.push(dg_core::ToolGroup {
        name,
        tools: Vec::new(),
    });
    state.group = state.groups.len() - 1;
    state.tool = None;
    fill_from_state(hwnd, state);
}

/// 删除分组 (1232): the group's file goes
/// away and the group with it.
unsafe fn delete_group(hwnd: HWND, state: &mut ToolDlg<'_>) {
    if state.groups.is_empty() {
        return;
    }
    save_fields(hwnd, state);
    let name = state.groups[state.group].name.clone();
    let path = dg_core::group_path(state.dir, &name);
    let _ = std::fs::remove_file(&path);
    state.groups.remove(state.group);
    if state.group >= state.groups.len() {
        state.group = state.groups.len().saturating_sub(1);
    }
    state.tool = None;
    fill_from_state(hwnd, state);
}

/// 图标 (1229): the icon path, stored in
/// the tool's `Icon` key.
unsafe fn pick_icon(hwnd: HWND, state: &mut ToolDlg<'_>) {
    let Some(path) = crate::win32::prompt::prompt(
        hwnd,
        "外部工具",
        "请输入图标路径:",
        "",
    ) else {
        return;
    };
    if let Some(group) = state.groups.get_mut(state.group) {
        if let Some(tool) =
            state.tool.and_then(|i| group.tools.get_mut(i))
        {
            tool.icon = path;
        }
    }
}

/// The `$(...)` variables the variable list offers, in that list's
/// own order, with `None` for the separator rows between the
/// families. The row order is the list's own — the first sixteen
/// variables read 1, 15, 2, 3, 4, 16, then 10, then 5, 9, 6, 7, 8,
/// then 11-14 — and the language table maps each id to its Chinese
/// caption. (The template's first row is a stray `Args` string the
/// lng section does not list; it is not one of the sixteen.)
const VARIABLE_ROWS: &[Option<&str>] = &[
    Some("$(FilePath)"),
    Some("$(FilePathNoExt)"),
    Some("$(FileDir)"),
    Some("$(FileName)"),
    Some("$(FileNameNoExt)"),
    Some("$(FileExt)"),
    None,
    Some("$(AppPath)"),
    None,
    Some("$(CurLine)"),
    Some("$(CurLineText)"),
    Some("$(CurCol)"),
    Some("$(CurSel)"),
    Some("$(CurWord)"),
    None,
    Some("$(UserArg1)"),
    Some("$(UserArg2)"),
    Some("$(UserArg3)"),
    Some("$(UserArg4)"),
];

/// The `6` arrow beside a field: pops the
/// variable menu at the button's bottom
/// left and inserts the chosen token into
/// the field at the caret — the same move
/// the dropdown click makes for the date/time
/// dialog's arrow.
unsafe fn insert_variable(hwnd: HWND, id: i32, button: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, MF_SEPARATOR,
        MF_STRING, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
        TrackPopupMenu,
    };
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    // The command id is the row index plus one, so
    // a separator row keeps its slot and the
    // chosen id still addresses `VARIABLE_ROWS`.
    for (i, row) in VARIABLE_ROWS.iter().enumerate() {
        match row {
            Some(token) => {
                let t = wide(token);
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    i + 1,
                    PCWSTR::from_raw(t.as_ptr()),
                );
            }
            None => {
                let _ = AppendMenuW(menu, MF_SEPARATOR, i + 1, None);
            }
        }
    }
    let mut rect = windows::Win32::Foundation::RECT::default();
    let _ = GetWindowRect(button, &mut rect);
    let chosen = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_LEFTALIGN,
        rect.left,
        rect.bottom,
        None,
        hwnd,
        None,
    );
    let _ = DestroyMenu(menu);
    let chosen = chosen.0 as usize;
    if chosen == 0 {
        return;
    }
    if let Some(Some(token)) = VARIABLE_ROWS.get(chosen - 1) {
        insert_at_caret(hwnd, id, token);
    }
}

/// Types `text` into edit `id` at the
/// caret.
unsafe fn insert_at_caret(hwnd: HWND, id: i32, text: &str) {
    use windows::Win32::UI::Controls::{
        EM_GETSEL, EM_REPLACESEL, EM_SCROLLCARET,
    };
    let Ok(e) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    let mut start = 0u32;
    let mut end = 0u32;
    SendMessageW(
        e,
        EM_GETSEL,
        Some(WPARAM(&mut start as *mut u32 as usize)),
        Some(LPARAM(&mut end as *mut u32 as isize)),
    );
    let t = wide(text);
    let _ = SendMessageW(
        e,
        EM_REPLACESEL,
        Some(WPARAM(1)),
        Some(LPARAM(t.as_ptr() as isize)),
    );
    let _ = SendMessageW(
        e,
        EM_SCROLLCARET,
        Some(WPARAM(0)),
        Some(LPARAM(0)),
    );
}

/// The 高级 button opens its box — 使用输出定位 over the 正则表达式 edit, the 捕获
/// group with its three capture-group combos, 保存模式 and 输出编码. Every
/// item of the template is here.
unsafe fn advanced(hwnd: HWND, state: &mut ToolDlg<'_>) {
    save_fields(hwnd, state);
    let mut edited = {
        let Some(group) = state.groups.get(state.group) else {
            return;
        };
        let Some(tool) = state.tool.and_then(|i| group.tools.get(i))
        else {
            return;
        };
        tool.clone()
    };
    let theme = state.theme.clone();
    if !advanced_dlg(hwnd, &theme, &mut edited) {
        return;
    }
    if let Some(group) = state.groups.get_mut(state.group) {
        if let Some(slot) = state.tool.and_then(|i| group.tools.get_mut(i))
        {
            *slot = edited;
        }
    }
}

// ------------------------------------------------------------------
// the 高级 box
// ------------------------------------------------------------------

const ADV_CLASS: PCWSTR = w!("DogEditToolAdv");

/// The template's control ids as the template numbers them: 1217-1225
/// for the interactive items, 3000-3005 for the six labels. The 动作模式
/// combo on the main tool page is not one of them, so the two dialogs
/// share only the 3000-series labels.
const ADV_USE: i32 = 1_217;
const ADV_PATTERN: i32 = 1_218;
const ADV_GROUP_PATH: i32 = 1_219;
const ADV_GROUP_LINE: i32 = 1_220;
const ADV_GROUP_COL: i32 = 1_221;
const ADV_GROUPBOX: i32 = 1_222;
const ADV_SAVE: i32 = 1_223;
const ADV_ENCODING: i32 = 1_225;
const ADV_L_PATTERN: i32 = 3_000;
const ADV_L_PATH: i32 = 3_001;
const ADV_L_LINE: i32 = 3_002;
const ADV_L_COL: i32 = 3_003;
const ADV_L_SAVE: i32 = 3_004;
const ADV_L_ENCODING: i32 = 3_005;

/// A 30-unit combo is the 48 px its dropped list opens to and the 捕获
/// group box is 96 px tall.
const ADV_RECT_USE: (i32, i32, i32, i32) = (11, 8, 192, 16);
const ADV_RECT_L_PATTERN: (i32, i32, i32, i32) = (11, 30, 155, 16);
const ADV_RECT_PATTERN: (i32, i32, i32, i32) = (11, 51, 272, 22);
const ADV_RECT_GROUP: (i32, i32, i32, i32) = (11, 80, 272, 96);
const ADV_RECT_L_PATH: (i32, i32, i32, i32) = (19, 106, 72, 16);
const ADV_RECT_PATH: (i32, i32, i32, i32) = (91, 99, 88, 48);
const ADV_RECT_L_LINE: (i32, i32, i32, i32) = (19, 130, 64, 16);
const ADV_RECT_LINE: (i32, i32, i32, i32) = (91, 123, 88, 48);
const ADV_RECT_L_COL: (i32, i32, i32, i32) = (19, 152, 64, 16);
const ADV_RECT_COL: (i32, i32, i32, i32) = (91, 147, 88, 48);
const ADV_RECT_L_SAVE: (i32, i32, i32, i32) = (11, 186, 120, 16);
const ADV_RECT_SAVE: (i32, i32, i32, i32) = (11, 203, 272, 48);
const ADV_RECT_L_ENCODING: (i32, i32, i32, i32) = (11, 229, 96, 16);
const ADV_RECT_ENCODING: (i32, i32, i32, i32) = (11, 246, 272, 48);
const ADV_RECT_OK: (i32, i32, i32, i32) = (115, 278, 80, 22);
const ADV_RECT_CANCEL: (i32, i32, i32, i32) = (203, 278, 80, 22);
/// The client size the template's 184x195 dialog
/// units come to at 1.6, and the window that
/// client sits in — the same 15x38 frame the
/// manager's own window adds.
const ADV_CLIENT_W: i32 = 294;
const ADV_CLIENT_H: i32 = 312;
const ADV_W: i32 = ADV_CLIENT_W + 15;
const ADV_H: i32 = ADV_CLIENT_H + 38;

/// The capture-group rows each of the three
/// combos offers: the tool-dialog handler counts 1 to 9.
const ADV_GROUP_ROWS: i32 = 9;

/// The 输出编码 list drops these four rows
/// (the tool-dialog handler's `(id - 1200) <= 1 ||
/// (id - 12000) <= 1` test): an external tool
/// hands back bytes, and the two UTF-16 and two
/// UTF-32 pages are not a byte encoding.
const ADV_NO_ENCODINGS: [u16; 4] = [1_200, 1_201, 12_000, 12_001];

struct AdvState<'a> {
    tool: &'a mut dg_core::Tool,
    theme: Theme,
    font: HFONT,
    bg: HBRUSH,
    face: HBRUSH,
    accepted: bool,
    destroyed: bool,
}

/// Runs the 高级 box over `tool`. `true` when
/// 确定 was used.
unsafe fn advanced_dlg(
    owner: HWND,
    theme: &Theme,
    tool: &mut dg_core::Tool,
) -> bool {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| unsafe {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(adv_proc),
            hInstance: instance(),
            lpszClassName: ADV_CLASS,
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            eprintln!("tool advanced class registration failed");
        }
    });
    let dpi_i = crate::win32::uifont::window_dpi(owner);
    let dpi = dpi_i as f32 / 96.0;
    let scale = |v: i32| (v as f32 * dpi).round() as i32;
    let mut state = AdvState {
        tool,
        theme: theme.clone(),
        font: crate::win32::uifont::create(dpi_i),
        bg: CreateSolidBrush(to_colorref(theme.toolbar_bg)),
        face: CreateSolidBrush(to_colorref(theme.toolbar_field)),
        accepted: false,
        destroyed: false,
    };
    let title16 = wide("高级");
    let state_ptr =
        &mut state as *mut AdvState<'_> as *const core::ffi::c_void;
    let Ok(hwnd) = CreateWindowExW(
        WS_EX_DLGMODALFRAME,
        ADV_CLASS,
        PCWSTR::from_raw(title16.as_ptr()),
        WS_POPUP | WS_CAPTION | WS_SYSMENU,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        scale(ADV_W),
        scale(ADV_H),
        Some(owner),
        None,
        Some(instance()),
        Some(state_ptr),
    ) else {
        free_chrome(state.font, state.bg, state.face);
        return false;
    };

    // 使用输出定位 (1217) — `BS_AUTOCHECKBOX`,
    // the gate the other four controls hang off.
    add_child(
        hwnd,
        CLASS_BUTTON,
        PCWSTR::from_raw(wide("使用输出定位").as_ptr()),
        ADV_USE,
        WINDOW_STYLE(0x0001_0003),
        ADV_RECT_USE,
        dpi,
    );
    // The 正则表达式 label and its edit.
    add_child(
        hwnd,
        CLASS_STATIC,
        PCWSTR::from_raw(wide("正则表达式:").as_ptr()),
        ADV_L_PATTERN,
        WINDOW_STYLE::default(),
        ADV_RECT_L_PATTERN,
        dpi,
    );
    add_child(
        hwnd,
        CLASS_EDIT,
        PCWSTR::null(),
        ADV_PATTERN,
        WINDOW_STYLE(0x0081_0080),
        ADV_RECT_PATTERN,
        dpi,
    );
    // 捕获 (1222) and the three group combos it
    // frames. The combos are 1219/1220/1221 —
    // 文件路径, 行号, 列号 in that order, the
    // order `Capture` stores its three ints in.
    add_child(
        hwnd,
        CLASS_BUTTON,
        PCWSTR::from_raw(wide("捕获").as_ptr()),
        ADV_GROUPBOX,
        WINDOW_STYLE(0x0000_0007), // BS_GROUPBOX
        ADV_RECT_GROUP,
        dpi,
    );
    for (i, (label_id, label_rect, combo_id, combo_rect)) in [
        (ADV_L_PATH, ADV_RECT_L_PATH, ADV_GROUP_PATH, ADV_RECT_PATH),
        (ADV_L_LINE, ADV_RECT_L_LINE, ADV_GROUP_LINE, ADV_RECT_LINE),
        (ADV_L_COL, ADV_RECT_L_COL, ADV_GROUP_COL, ADV_RECT_COL),
    ]
    .into_iter()
    .enumerate()
    {
        let label = format!("{}:", dg_core::CAPTURE_GROUPS[i]);
        add_child(
            hwnd,
            CLASS_STATIC,
            PCWSTR::from_raw(wide(&label).as_ptr()),
            label_id,
            WINDOW_STYLE::default(),
            label_rect,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_COMBO,
            PCWSTR::null(),
            combo_id,
            WINDOW_STYLE(STYLE_ADV_GROUP_COMBO),
            combo_rect,
            dpi,
        );
    }
    // 保存模式 (1223) and 输出编码 (1225). The
    // template gives both `CBS_DISABLENOSCROLL |
    // CBS_AUTOHSCROLL | CBS_DROPDOWNLIST` — no
    // `CBS_SORT`, so the rows keep their menu
    // order.
    add_child(
        hwnd,
        CLASS_STATIC,
        PCWSTR::from_raw(wide("保存模式:").as_ptr()),
        ADV_L_SAVE,
        WINDOW_STYLE::default(),
        ADV_RECT_L_SAVE,
        dpi,
    );
    add_child(
        hwnd,
        CLASS_COMBO,
        PCWSTR::null(),
        ADV_SAVE,
        WINDOW_STYLE(STYLE_ADV_DROP_COMBO),
        ADV_RECT_SAVE,
        dpi,
    );
    add_child(
        hwnd,
        CLASS_STATIC,
        PCWSTR::from_raw(wide("输出编码:").as_ptr()),
        ADV_L_ENCODING,
        WINDOW_STYLE::default(),
        ADV_RECT_L_ENCODING,
        dpi,
    );
    add_child(
        hwnd,
        CLASS_COMBO,
        PCWSTR::null(),
        ADV_ENCODING,
        WINDOW_STYLE(STYLE_ADV_DROP_COMBO),
        ADV_RECT_ENCODING,
        dpi,
    );
    // 确定 is the plain button and 取消 the
    // default one — the template's own pair.
    add_child(
        hwnd,
        CLASS_BUTTON,
        PCWSTR::from_raw(wide("确定").as_ptr()),
        IDOK.0 as i32,
        WINDOW_STYLE::default(),
        ADV_RECT_OK,
        dpi,
    );
    add_child(
        hwnd,
        CLASS_BUTTON,
        PCWSTR::from_raw(wide("取消").as_ptr()),
        IDCANCEL.0 as i32,
        WINDOW_STYLE(0x0001_0001), // BS_DEFPUSHBUTTON
        ADV_RECT_CANCEL,
        dpi,
    );

    apply_font(hwnd, state.font);
    let dark = crate::win32::chrome::theme_is_dark(theme);
    crate::win32::chrome::set_chrome_theme(theme);
    crate::win32::chrome::dark_theme_children(hwnd, dark);
    crate::win32::chrome::title_bar(hwnd, dark);

    show_modal(hwnd, owner);
    fill_advanced(hwnd, &state);

    run_modal_loop(hwnd, owner, std::ptr::addr_of!(state.destroyed));
    free_chrome(state.font, state.bg, state.face);
    state.accepted
}

/// the tool-dialog handler: the nine capture rows under
/// a `无` row in each of the three combos, the
/// two menu-backed combos, then the tool's own
/// values, then the 1217 gate.
unsafe fn fill_advanced(hwnd: HWND, state: &AdvState<'_>) {
    // The nine rows are the group box's own caption plus the group's
    // number.
    let caption = edit_text(hwnd, ADV_GROUPBOX);
    for id in [ADV_GROUP_PATH, ADV_GROUP_LINE, ADV_GROUP_COL] {
        let Ok(combo) = GetDlgItem(Some(hwnd), id) else {
            continue;
        };
        combo_add(combo, "无", 0);
        for n in 1..=ADV_GROUP_ROWS {
            combo_add(combo, &format!("{caption} {n}"), n);
        }
    }
    if let Ok(combo) = GetDlgItem(Some(hwnd), ADV_SAVE) {
        for (mode, caption) in SAVE_ROWS {
            combo_add(combo, caption, mode.id());
        }
    }
    if let Ok(combo) = GetDlgItem(Some(hwnd), ADV_ENCODING) {
        for (cp, name) in super::grepdlg::CODE_PAGES {
            if ADV_NO_ENCODINGS.contains(cp) {
                continue;
            }
            combo_add(
                combo,
                &super::grepdlg::codepage_label(*cp, name),
                i32::from(*cp),
            );
        }
        let ave = ave_char_width(state.font);
        if ave > 0 {
            SendMessageW(
                combo,
                CB_SETDROPPEDWIDTH,
                Some(WPARAM((60 * ave) as usize)),
                Some(LPARAM(0)),
            );
        }
    }
    if let Ok(chk) = GetDlgItem(Some(hwnd), ADV_USE) {
        SendMessageW(
            chk,
            BM_SETCHECK,
            Some(WPARAM(if state.tool.use_pattern {
                BST_CHECKED.0 as usize
            } else {
                0
            })),
            Some(LPARAM(0)),
        );
    }
    set_edit_text(hwnd, ADV_PATTERN, &state.tool.pattern);
    for (id, slot) in [
        (ADV_GROUP_PATH, dg_core::CAPTURE_FILE_PATH),
        (ADV_GROUP_LINE, dg_core::CAPTURE_LINE),
        (ADV_GROUP_COL, dg_core::CAPTURE_COLUMN),
    ] {
        combo_select_data(hwnd, id, state.tool.capture[slot]);
    }
    combo_select_data(hwnd, ADV_SAVE, state.tool.save_pattern.id());
    combo_select_data(hwnd, ADV_ENCODING, state.tool.encoding);
    enable_advanced_by_use(hwnd);
}

/// 1217 alone gates the 正则表达式 edit and the three capture combos. 保存模式
/// and 输出编码 stay live.
unsafe fn enable_advanced_by_use(hwnd: HWND) {
    let on = checked(hwnd, ADV_USE);
    for id in [ADV_PATTERN, ADV_GROUP_PATH, ADV_GROUP_LINE, ADV_GROUP_COL]
    {
        if let Ok(c) = GetDlgItem(Some(hwnd), id) {
            let _ = EnableWindow(c, on);
        }
    }
}

unsafe extern "system" fn adv_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return LRESULT(0);
    }
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA)
        as *mut AdvState<'_>;
    if msg == WM_COMMAND {
        let id = (wparam.0 & 0xFFFF) as i32;
        if id == IDOK.0 {
            if !state.is_null() {
                let st = &mut *state;
                st.tool.use_pattern = checked(hwnd, ADV_USE);
                st.tool.pattern = edit_text(hwnd, ADV_PATTERN);
                for (combo, slot) in [
                    (ADV_GROUP_PATH, dg_core::CAPTURE_FILE_PATH),
                    (ADV_GROUP_LINE, dg_core::CAPTURE_LINE),
                    (ADV_GROUP_COL, dg_core::CAPTURE_COLUMN),
                ] {
                    if let Some(v) = combo_data(hwnd, combo) {
                        st.tool.capture[slot] = v;
                    }
                }
                if let Some(v) = combo_data(hwnd, ADV_SAVE) {
                    st.tool.save_pattern = dg_core::SaveMode::from_id(v);
                }
                if let Some(v) = combo_data(hwnd, ADV_ENCODING) {
                    st.tool.encoding = v;
                }
                st.accepted = true;
            }
            DestroyWindow(hwnd).ok();
            return LRESULT(0);
        }
        if id == IDCANCEL.0 {
            DestroyWindow(hwnd).ok();
            return LRESULT(0);
        }
        if id == ADV_USE {
            enable_advanced_by_use(hwnd);
            return LRESULT(0);
        }
        return LRESULT(0);
    }
    if !state.is_null() {
        let st = &*state;
        if let Some(r) =
            colour(msg, hwnd, wparam, &st.theme, st.bg, st.face)
        {
            return r;
        }
    }
    if msg == WM_CLOSE {
        DestroyWindow(hwnd).ok();
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        if !state.is_null() {
            (*state).destroyed = true;
        }
        PostQuitMessage(0);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// A check box's state.
unsafe fn checked(hwnd: HWND, id: i32) -> bool {
    let Ok(chk) = GetDlgItem(Some(hwnd), id) else {
        return false;
    };
    let r = SendMessageW(
        chk,
        BM_GETCHECK,
        Some(WPARAM(0)),
        Some(LPARAM(0)),
    );
    r.0 as u32 == BST_CHECKED.0
}

/// `CB_ADDSTRING` plus `CB_SETITEMDATA` — the
/// pair the tool-dialog handler uses for every row it
/// builds, so the selection can be found again by
/// value rather than by position.
unsafe fn combo_add(combo: HWND, text: &str, data: i32) {
    let t = wide(text);
    let i = SendMessageW(
        combo,
        CB_ADDSTRING,
        Some(WPARAM(0)),
        Some(LPARAM(t.as_ptr() as isize)),
    );
    if i.0 >= 0 {
        SendMessageW(
            combo,
            CB_SETITEMDATA,
            Some(WPARAM(i.0 as usize)),
            Some(LPARAM(data as isize)),
        );
    }
}

/// The item data of the combo's current row.
unsafe fn combo_data(hwnd: HWND, id: i32) -> Option<i32> {
    let cur = combo_index(hwnd, id)?;
    let c = GetDlgItem(Some(hwnd), id).ok()?;
    let r = SendMessageW(
        c,
        CB_GETITEMDATA,
        Some(WPARAM(cur)),
        Some(LPARAM(0)),
    );
    Some(r.0 as i32)
}

/// Selects the row whose item data is `data` —
/// the loop the tool-dialog handler runs over each combo
/// before it shows the dialog.
unsafe fn combo_select_data(hwnd: HWND, id: i32, data: i32) {
    let Ok(c) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    let count = SendMessageW(
        c,
        CB_GETCOUNT,
        Some(WPARAM(0)),
        Some(LPARAM(0)),
    )
    .0;
    for i in 0..count.max(0) {
        let d = SendMessageW(
            c,
            CB_GETITEMDATA,
            Some(WPARAM(i as usize)),
            Some(LPARAM(0)),
        )
        .0 as i32;
        if d == data {
            SendMessageW(
                c,
                CB_SETCURSEL,
                Some(WPARAM(i as usize)),
                Some(LPARAM(0)),
            );
            return;
        }
    }
}

/// the UI font's average character
/// width, which the 输出编码 combo's dropped width
/// is sixty of.
unsafe fn ave_char_width(font: HFONT) -> i32 {
    if font.0.is_null() {
        return 0;
    }
    let dc = GetDC(None);
    if dc.0.is_null() {
        return 0;
    }
    let old = SelectObject(dc, HGDIOBJ(font.0));
    let mut tm = TEXTMETRICW::default();
    let ok = GetTextMetricsW(dc, &mut tm).as_bool();
    SelectObject(dc, old);
    ReleaseDC(None, dc);
    if ok { tm.tmAveCharWidth } else { 0 }
}

/// Frees the font and the two brushes a dialog
/// created. The class's `COLOR_3DFACE` brush is the
/// system's and stays.
unsafe fn free_chrome(font: HFONT, bg: HBRUSH, face: HBRUSH) {
    for b in [bg, face] {
        if !b.0.is_null() {
            let _ = DeleteObject(b.into());
        }
    }
    if !font.0.is_null() {
        let _ = DeleteObject(font.into());
    }
}

/// The paint arms both dialogs answer: the face
/// behind the controls, the labels' ink, and the
/// fields' own face. `None` for every other message,
/// so the caller can fall through to `DefWindowProcW`.
unsafe fn colour(
    msg: u32,
    hwnd: HWND,
    wparam: WPARAM,
    theme: &Theme,
    bg: HBRUSH,
    face: HBRUSH,
) -> Option<LRESULT> {
    match msg {
        WM_ERASEBKGND => {
            // The class ships a `COLOR_3DFACE` brush; the space between and
            // behind the controls has to come from Theme.
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, bg);
            }
            Some(LRESULT(1))
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            // Labels, the rule, the group box and the check boxes sit on the
            // dialog face.
            Some(LRESULT(customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                to_colorref(theme.toolbar_fg),
                to_colorref(theme.toolbar_bg),
                bg,
            )))
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            // Left to the system the field would be `COLOR_WINDOW`, a different grey.
            Some(LRESULT(customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                to_colorref(theme.toolbar_fg),
                to_colorref(theme.toolbar_field),
                face,
            )))
        }
        _ => None,
    }
}

// ------------------------------------------------------------------
// the manager's window procedure
// ------------------------------------------------------------------

unsafe extern "system" fn tool_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return LRESULT(0);
    }
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA)
        as *mut ToolDlg<'_>;
    if msg == WM_COMMAND {
        let id = (wparam.0 & 0xFFFF) as i32;
        let code = (wparam.0 >> 16) as u32;
        if state.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let st = &mut *state;
        match id {
            x if x == IDOK.0 => {
                save_fields(hwnd, st);
                write_groups(st);
                st.accepted = true;
                DestroyWindow(hwnd).ok();
                return LRESULT(0);
            }
            x if x == IDCANCEL.0 => {
                DestroyWindow(hwnd).ok();
                return LRESULT(0);
            }
            x if x == ID_ADD => add_tool(hwnd, st),
            x if x == ID_DELETE => delete_tool(hwnd, st),
            x if x == ID_ADD_GROUP => add_group(hwnd, st),
            x if x == ID_DEL_GROUP => delete_group(hwnd, st),
            x if x == ID_ICON => pick_icon(hwnd, st),
            x if x == ID_ADVANCED => advanced(hwnd, st),
            x if x == ID_BROWSE_EXE => {
                if let Some(path) = browse_file(hwnd) {
                    set_edit_text(hwnd, ID_EXE, &path);
                }
            }
            // The `6` arrows: pop the variable menu
            // over the button that was pressed.
            x if x == ID_VAR_EXE
                || x == ID_VAR_ARGS
                || x == ID_VAR_DIR =>
            {
                let field = match id {
                    ID_VAR_EXE => ID_EXE,
                    ID_VAR_ARGS => ID_ARGS,
                    _ => ID_INITDIR,
                };
                let button = HWND(lparam.0 as *mut _);
                insert_variable(hwnd, field, button);
            }
            // The group combo's selection.
            x if x == ID_GROUP_COMBO => {
                if code == 1 {
                    // CBN_SELCHANGE
                    if let Some(i) = combo_index(hwnd, ID_GROUP_COMBO) {
                        select_group(hwnd, st, i);
                    }
                }
            }
            // The capture combo's selection lands
            // with the fields on 确定.
            _ => {}
        }
        return LRESULT(0);
    }
    if msg == WM_NOTIFY {
        // The list's selection change. Which is what greys the whole form out
        // when the last row is deselected.
        let hdr = &*(lparam.0 as *const NMLISTVIEW);
        if hdr.hdr.idFrom as i32 == ID_LIST
            && hdr.hdr.code == LVN_ITEMCHANGED as u32
            && !state.is_null()
            && !(*state).loading
        {
            let st = &mut *state;
            match list_selection(hwnd) {
                Some(i) if st.tool != Some(i) => select_tool(hwnd, st, i),
                None if st.tool.is_some() => st.tool = None,
                _ => {}
            }
            update_enables(hwnd, st);
        }
        return LRESULT(0);
    }
    if !state.is_null() {
        let st = &*state;
        if let Some(r) =
            colour(msg, hwnd, wparam, &st.theme, st.bg, st.face)
        {
            return r;
        }
    }
    if msg == WM_CLOSE {
        DestroyWindow(hwnd).ok();
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        if !state.is_null() {
            (*state).destroyed = true;
        }
        PostQuitMessage(0);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Writes every group back to its file.
/// A group that lost all its tools keeps
/// its file — an empty group is a group
/// the user made on purpose.
unsafe fn write_groups(state: &ToolDlg<'_>) {
    // A fresh install has no `tool` folder, and writing a file into a
    // missing directory fails — the first tool a user adds would be
    // dropped on 确定 without a word.
    let _ = std::fs::create_dir_all(state.dir);
    for group in state.groups.iter() {
        let path = dg_core::group_path(state.dir, &group.name);
        let _ = dg_core::save_file(&path, &group.tools);
    }
}

/// The 命令 row's `...` button: a file
/// picker for the program to run.
unsafe fn browse_file(hwnd: HWND) -> Option<String> {
    crate::win32::window::pick_executable(hwnd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The template's items, converted at the 1.6 logical px per
    /// dialog unit these dialogs measure.
    #[test]
    fn template_geometry_is_kept() {
        // 263x238 dialog units
        assert_eq!((CLIENT_W, CLIENT_H), (421, 381));
        assert_eq!(
            (DLG_W - CLIENT_W, DLG_H - CLIENT_H),
            (15, 38)
        );
        // the list sits under the group combo
        assert!(RECT_LIST.1 > RECT_GROUP.1);
        // the field labels sit three dialog
        // units (five px) below their edits'
        // tops and left of them — the
        // template's own offsets
        assert_eq!(RECT_L_TITLE.1 - RECT_TITLE.1, 5);
        assert_eq!(RECT_L_EXE.1 - RECT_EXE.1, 3);
        assert!(RECT_L_TITLE.0 + RECT_L_TITLE.2 <= RECT_TITLE.0);
        assert!(RECT_L_EXE.0 + RECT_L_EXE.2 <= RECT_EXE.0);
        // the arrows sit right of the edits
        assert_eq!(RECT_VAR_EXE.0, RECT_EXE.0 + RECT_EXE.2 + 8);
        // the buttons close the form
        assert_eq!(RECT_OK.1, RECT_CANCEL.1);
    }

    /// The four combo style words are the template's own, with the
    /// `WS_CHILD | WS_VISIBLE` the child helper adds taken back out — the dump
    /// reads 0x50010203 (1226), 0x50210843 (1224), 0x50200843 (1223/1225) and
    /// 0x50000203 (1219-1221).
    #[test]
    fn the_combo_styles_are_the_templates() {
        const CHILD_VISIBLE: u32 = 0x5000_0000;
        assert_eq!(STYLE_GROUP_COMBO | CHILD_VISIBLE, 0x5001_0203);
        assert_eq!(STYLE_CAPTURE_COMBO | CHILD_VISIBLE, 0x5021_0843);
        assert_eq!(STYLE_ADV_DROP_COMBO | CHILD_VISIBLE, 0x5020_0843);
        assert_eq!(STYLE_ADV_GROUP_COMBO | CHILD_VISIBLE, 0x5000_0203);
        // the drop-downs carry `CBS_DROPDOWNLIST` and not `CBS_SORT`: the rows
        // have to keep the order the 输出模式, 保存模式 and variable lists give them
        for style in [
            STYLE_GROUP_COMBO,
            STYLE_CAPTURE_COMBO,
            STYLE_ADV_DROP_COMBO,
            STYLE_ADV_GROUP_COMBO,
        ] {
            assert_eq!(style & 0x3, 3, "0x{style:08X} is not a drop-down list");
            assert_eq!(style & 0x100, 0, "0x{style:08X} would sort its rows");
        }
    }

    /// the tool-dialog enable pass's three rules: eleven controls follow the selection,
    /// 删除 also stays live for a separator, 删除分组 follows the group index,
    /// and 接受输入 is gated by the two 捕获输出 modes.
    #[test]
    fn the_enable_pass_covers_the_templates_controls() {
        assert_eq!(
            FIELD_CONTROLS,
            [1_206, 1_207, 1_208, 1_209, 1_228, 1_210, 1_211, 1_212, 1_216, 1_224, 1_229]
        );
        // the three exceptions must not be in the list
        for id in [ID_DELETE, ID_DEL_GROUP, ID_INPUT] {
            assert!(!FIELD_CONTROLS.contains(&id), "{id} has its own rule");
        }
        // 接受输入 tests the mode against `2` and `9` — the two 捕获输出 rows
        assert_eq!(dg_core::CaptureMode::CaptureHide.id(), 2);
        assert_eq!(dg_core::CaptureMode::CaptureShow.id(), 9);
    }

    /// The list is the template's report view: `0x5081C00D`, one empty-headed
    /// column, `LVS_EX_FULLROWSELECT` as the *whole* extended style. A second
    /// column would put a 标题/命令 header over the rows
    /// never shows.
    #[test]
    fn the_list_is_the_templates_report_view() {
        const CHILD_VISIBLE: u32 = 0x5000_0000;
        const LVS_REPORT: u32 = 0x0001;
        const WS_BORDER: u32 = 0x0080_0000;
        const LVS_NOCOLUMNHEADER: u32 = 0x4000;
        assert_eq!(STYLE_LIST | CHILD_VISIBLE, 0x5081_C00D);
        assert_eq!(STYLE_LIST & 0x3, LVS_REPORT);
        assert_eq!(STYLE_LIST & WS_BORDER, WS_BORDER, "the list has a border");
        assert_ne!(
            STYLE_LIST & LVS_NOCOLUMNHEADER,
            0,
            "the header would show the empty caption"
        );
        assert_eq!(LIST_EXSTYLE, 0x20);
        assert_eq!(LIST_EXSTYLE, windows::Win32::UI::Controls::LVS_EX_FULLROWSELECT);
        assert_eq!(LIST_CAPTION, "");
    }

    /// The three `$(...)` arrows are the template's only buttons captioned
    /// with a glyph: a single `6`, which Marlett draws as the down arrow. The
    /// browse button beside them is `...` and keeps the UI font, so the two
    /// sets must not be confused.
    #[test]
    fn the_arrows_are_marlett_glyphs() {
        // 1228 (命令), 1211 (参数), 1212 (初始目录), in template order
        assert_eq!(ARROWS, [1_228, 1_211, 1_212]);
        assert_eq!(ARROW_CAPTION, "6");
        assert_eq!(
            ARROW_CAPTION.chars().count(),
            1,
            "a word caption would not be a glyph"
        );
        assert!(!ARROWS.contains(&ID_BROWSE_EXE));
        // 15x13 dialog units: the arrows are square-ish next to their edits
        assert_eq!(RECT_VAR_EXE, (323, 205, 24, 21));
        assert_eq!(RECT_BROWSE_EXE.2, RECT_VAR_EXE.2);
    }

    /// The 输出模式 list's nine rows, in the order the combo lists them —
    /// the ids the loader reads are the row's mode, not the row
    /// number, so the order has to stay that list's: 1, 2, 9, 8, 3,
    /// 4, 5, 6, 7.
    #[test]
    fn capture_rows_cover_menu317() {
        assert_eq!(CAPTURE_ROWS.len(), 9);
        assert_eq!(CAPTURE_ROWS[0].0, dg_core::CaptureMode::None);
        assert_eq!(
            CAPTURE_ROWS[1].0,
            dg_core::CaptureMode::CaptureHide
        );
        let ids: Vec<i32> =
            CAPTURE_ROWS.iter().map(|(m, _)| m.id()).collect();
        // the combo's row order, which is the order the language
        // table lists the 输出模式 captions
        assert_eq!(ids, vec![1, 2, 9, 8, 3, 4, 5, 6, 7]);
    }

    /// The 保存模式 list's three rows, in that list's order.
    #[test]
    fn save_rows_cover_menu318() {
        assert_eq!(SAVE_ROWS.len(), 3);
        assert_eq!(SAVE_ROWS[0].0, dg_core::SaveMode::None);
        assert_eq!(SAVE_ROWS[2].0, dg_core::SaveMode::All);
        let ids: Vec<i32> = SAVE_ROWS.iter().map(|(m, _)| m.id()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    /// The sixteen `$(...)` tokens in the variable list's row order,
    /// with the three separator rows between the families.
    #[test]
    fn variable_rows_match_the_name_table() {
        let tokens: Vec<&str> = VARIABLE_ROWS
            .iter()
            .filter_map(|r| *r)
            .collect();
        assert_eq!(
            tokens,
            vec![
                "$(FilePath)",
                "$(FilePathNoExt)",
                "$(FileDir)",
                "$(FileName)",
                "$(FileNameNoExt)",
                "$(FileExt)",
                "$(AppPath)",
                "$(CurLine)",
                "$(CurLineText)",
                "$(CurCol)",
                "$(CurSel)",
                "$(CurWord)",
                "$(UserArg1)",
                "$(UserArg2)",
                "$(UserArg3)",
                "$(UserArg4)",
            ]
        );
        // three separators: after the file family,
        // around AppPath, and before the user args
        assert_eq!(
            VARIABLE_ROWS
                .iter()
                .filter(|r| r.is_none())
                .count(),
            3
        );
        assert_eq!(VARIABLE_ROWS.len(), 19);
    }

    /// The 高级 box's client is 184x195 dialog units.
    #[test]
    fn advanced_geometry_is_kept() {
        assert_eq!((ADV_CLIENT_W, ADV_CLIENT_H), (294, 312));
        // 184 x 1.6 and 195 x 1.6
        assert_eq!(ADV_CLIENT_W, (184f32 * 1.6) as i32);
        assert_eq!(ADV_CLIENT_H, (195f32 * 1.6) as i32);
        // the 正则表达式 edit sits under its label, same left edge
        assert_eq!(ADV_RECT_PATTERN.0, ADV_RECT_L_PATTERN.0);
        assert!(ADV_RECT_PATTERN.1 > ADV_RECT_L_PATTERN.1);
        // the group box holds all three label/combo pairs
        for (label, combo) in [
            (ADV_RECT_L_PATH, ADV_RECT_PATH),
            (ADV_RECT_L_LINE, ADV_RECT_LINE),
            (ADV_RECT_L_COL, ADV_RECT_COL),
        ] {
            // a label is drawn where the template puts it, so
            // the whole rectangle has to be inside the frame
            assert!(label.0 >= ADV_RECT_GROUP.0, "{label:?} left of the box");
            assert!(label.1 >= ADV_RECT_GROUP.1, "{label:?} above the box");
            assert!(
                label.0 + label.2 <= ADV_RECT_GROUP.0 + ADV_RECT_GROUP.2,
                "{label:?} past the box's right edge"
            );
            assert!(
                label.1 + label.3 <= ADV_RECT_GROUP.1 + ADV_RECT_GROUP.3,
                "{label:?} below the box"
            );
            // a `CBS_DROPDOWNLIST`'s template height is the
            // height its list *drops* to, so only its top
            // edge and its width have to fit
            assert!(combo.0 >= ADV_RECT_GROUP.0);
            assert!(combo.1 >= ADV_RECT_GROUP.1, "{combo:?} above the box");
            assert!(
                combo.0 + combo.2 <= ADV_RECT_GROUP.0 + ADV_RECT_GROUP.2,
                "{combo:?} past the box's right edge"
            );
            // each combo sits right of its own label
            assert!(label.0 + label.2 <= combo.0);
        }
        // 文件路径, 行号, 列号 stack in the order
        // `Capture` stores its three ints
        assert!(ADV_RECT_PATH.1 < ADV_RECT_LINE.1);
        assert!(ADV_RECT_LINE.1 < ADV_RECT_COL.1);
        // 保存模式 and 输出编码 follow the group box
        assert!(ADV_RECT_L_SAVE.1 > ADV_RECT_GROUP.1 + ADV_RECT_GROUP.3);
        assert!(ADV_RECT_SAVE.1 > ADV_RECT_L_SAVE.1);
        assert!(ADV_RECT_ENCODING.1 > ADV_RECT_SAVE.1);
        // the buttons are the last row and share it
        assert_eq!(ADV_RECT_OK.1, ADV_RECT_CANCEL.1);
        assert!(ADV_RECT_OK.1 > ADV_RECT_ENCODING.1);
        assert!(ADV_RECT_OK.0 + ADV_RECT_OK.2 <= ADV_RECT_CANCEL.0);
        // nothing hangs outside the client
        for r in [
            ADV_RECT_USE,
            ADV_RECT_PATTERN,
            ADV_RECT_GROUP,
            ADV_RECT_ENCODING,
            ADV_RECT_CANCEL,
        ] {
            assert!(r.0 + r.2 <= ADV_CLIENT_W, "{r:?} too wide");
            assert!(r.1 + r.3 <= ADV_CLIENT_H, "{r:?} too tall");
        }
    }

    /// The 高级 box's rows and ids: `无` plus nine capture groups per combo,
    /// the four UTF-16/UTF-32 pages dropped from the encoding list, and no id
    /// shared with the manager's own interactive controls.
    #[test]
    fn advanced_rows_cover_the_template() {
        assert_eq!(ADV_GROUP_ROWS, 9);
        assert_eq!(ADV_NO_ENCODINGS, [1_200, 1_201, 12_000, 12_001]);
        // 1219/1220/1221 are 文件路径/行号/列号, in that order
        assert_eq!(
            [ADV_GROUP_PATH, ADV_GROUP_LINE, ADV_GROUP_COL],
            [1_219, 1_220, 1_221]
        );
        //... which is the order `Capture`'s three ints are in
        assert_eq!(
            [
                dg_core::CAPTURE_FILE_PATH,
                dg_core::CAPTURE_LINE,
                dg_core::CAPTURE_COLUMN
            ],
            [0, 1, 2]
        );
        // the two dialogs do not collide
        let manager = [
            ID_GROUP_COMBO, ID_LIST, ID_ADD, ID_DELETE, ID_ICON,
            ID_ADD_GROUP, ID_DEL_GROUP, ID_TITLE, ID_EXE, ID_ARGS,
            ID_INITDIR, ID_VAR_EXE, ID_VAR_ARGS, ID_VAR_DIR,
            ID_BROWSE_EXE, ID_CAPTURE, ID_ADVANCED, ID_INPUT,
        ];
        for id in [
            ADV_USE, ADV_PATTERN, ADV_GROUP_PATH, ADV_GROUP_LINE,
            ADV_GROUP_COL, ADV_GROUPBOX, ADV_SAVE, ADV_ENCODING,
        ] {
            assert!(!manager.contains(&id), "id {id} belongs to the main tool dialog");
        }
        // 1224 is the manager's 动作模式 combo, not the
        // advanced box's
        assert_eq!(ID_CAPTURE, 1_224);
        // the encoding rows are the code-page list minus
        // the four above
        assert!(super::super::grepdlg::CODE_PAGES.len() > 90);
        for cp in ADV_NO_ENCODINGS {
            assert!(
                super::super::grepdlg::CODE_PAGES
                    .iter()
                    .any(|(c, _)| *c == cp),
                "codepage {cp} is not in the list to begin with"
            );
        }
    }
}
