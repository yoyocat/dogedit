//! 文件比较(外部) — the two-list compare box behind 工具 > 文件比较(外部)
//! (`cm_tool_diff_external`, 1616).
//!
//! The box lists every open document twice, 左 and 右; 比较 hands the two
//! selections to the configured external diff tool. A document that is saved
//! and unmodified compares as its file on disk; anything else — untitled, or
//! edited since the last save — first asks the 15215 question and lands in a
//! temp file under the user's temp directory, so the tool always sees the
//! buffer's truth. Two identical files are reported (15234) instead of
//! launching anything; the same document in both lists is rejected (15217);
//! an empty or missing tool is rejected with 15218.
//!
//! 配置 opens the tool box: the executable path, its parameter line and the
//! preset menu. The parameter line carries `%1` and `%2` placeholders for the
//! two files; a line without placeholders gets both files appended, quoted,
//! which is what every common diff tool expects. The choice persists in
//! `setting.ini` as `[Tool] DiffExe` and `DiffArgs`.
//!
//! Geometry follows the 比较 box's own template — the 279×208 units dialog
//! with the two lists side by side under their 左:/右: captions, a read-only
//! command line along the bottom, and 比较/关闭 plus 配置 on the button row —
//! and the 199×91 units 配置 box with its path/参数 edits. Captions come from
//! the `[DLG270]` and `[DLG271]` tables by control id.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::wstr::wide;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, HFONT,
};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OPENFILENAMEW,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, DialogBoxIndirectParamW, DLGTEMPLATE, EndDialog,
    GetClientRect, GetDlgItem, GetDlgItemTextW, GWLP_USERDATA, GetWindowLongPtrW, GetWindowRect,
    LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT, MB_ICONINFORMATION, MB_OK, MF_STRING,
    MessageBoxW, SendMessageW, SetDlgItemTextW, SetWindowLongPtrW, SW_SHOWNORMAL, TPM_NONOTIFY,
    TPM_RETURNCMD, TrackPopupMenu, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT, WM_CTLCOLORBTN,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_INITDIALOG,
    MESSAGEBOX_STYLE,
};

use crate::win32::chrome;
use crate::win32::customdraw;
use crate::win32::lang::{format as lfmt, localize_dialog, string as lstr, trs};
use crate::win32::optionsdlg::{build_template, Class, Item};
use crate::win32::view::instance;
use crate::win32::window::{to_colorref, EditorState};

// ---------------------------------------------------------------------------
// Control ids — the `[DLG270]` / `[DLG271]` tables key captions by these
// ---------------------------------------------------------------------------

const ID_LEFT: i32 = 1200;
const ID_RIGHT: i32 = 1201;
const ID_CMDLINE: i32 = 1202;
const ID_CONFIG: i32 = 1203;
const ID_DIFF: i32 = 1;
const ID_CLOSE: i32 = 2;
const ID_LBL_LEFT: i32 = 3000;
const ID_LBL_RIGHT: i32 = 3001;

const ID_EXE: i32 = 1204;
const ID_BROWSE: i32 = 1205;
const ID_ARGS: i32 = 1206;
const ID_PRESET: i32 = 1208;
const ID_LBL_EXE: i32 = 3000;
const ID_LBL_ARGS: i32 = 3001;

/// The 预配置 menu rows: the common tools with their usual executable names.
/// The parameter line stays empty — the box appends both files quoted, and
/// the user edits from there.
const PRESETS: &[(&str, &str, &str)] = &[
    ("Beyond Compare", "BComp.exe", ""),
    ("Compare It!", "Wincmp3.exe", ""),
    ("ExamDiff", "ExamDiff.exe", ""),
    ("Ultra Compare", "UC.exe", ""),
    ("WinMerge", "WinMergeU.exe", ""),
    ("WinDiff", "WinDiff.exe", ""),
];

const ATOM_BUTTON: u16 = 0x0080;
const ATOM_EDIT: u16 = 0x0081;
const ATOM_STATIC: u16 = 0x0082;
const ATOM_LISTBOX: u16 = 0x0083;

// ---------------------------------------------------------------------------
// The templates
// ---------------------------------------------------------------------------

const DLG_STYLE: u32 = 0x90c8_08cc;

const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_GROUP: u32 = 0x0002_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_BORDER: u32 = 0x0080_0000;
const ES_AUTOHSCROLL: u32 = 0x0000_0080;
const ES_READONLY: u32 = 0x0000_0800;
const LBS_NOTIFY: u32 = 0x0000_0001;
const LBS_NOINTEGRALHEIGHT: u32 = 0x0000_0100;

const STYLE_BUTTON: u32 = WS_CHILD | WS_VISIBLE | WS_TABSTOP;
const STYLE_DEFBUTTON: u32 = STYLE_BUTTON | 0x0000_0001;
const STYLE_STATIC: u32 = WS_CHILD | WS_VISIBLE | WS_GROUP;
const STYLE_EDIT: u32 = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL;
const STYLE_RO_EDIT: u32 = STYLE_EDIT | ES_READONLY;
const STYLE_LIST: u32 = WS_CHILD
    | WS_VISIBLE
    | WS_TABSTOP
    | WS_BORDER
    | WS_VSCROLL
    | LBS_NOTIFY
    | LBS_NOINTEGRALHEIGHT;

const DLG_TITLE: &str = "比较";
const DLG_W: u16 = 279;
const DLG_H: u16 = 208;

/// The 比较 box: two lists under their captions, the read-only command line,
/// and the button row.
const ITEMS: &[Item] = &[
    Item { id: ID_LBL_LEFT, class: Class::Atom(ATOM_STATIC), style: STYLE_STATIC, x: 7, y: 6, w: 100, h: 10, label: "" },
    Item { id: ID_LBL_RIGHT, class: Class::Atom(ATOM_STATIC), style: STYLE_STATIC, x: 142, y: 6, w: 105, h: 10, label: "" },
    Item { id: ID_LEFT, class: Class::Atom(ATOM_LISTBOX), style: STYLE_LIST, x: 7, y: 17, w: 130, h: 150, label: "" },
    Item { id: ID_RIGHT, class: Class::Atom(ATOM_LISTBOX), style: STYLE_LIST, x: 142, y: 17, w: 130, h: 150, label: "" },
    Item { id: ID_CMDLINE, class: Class::Atom(ATOM_EDIT), style: STYLE_RO_EDIT, x: 7, y: 170, w: 265, h: 14, label: "" },
    Item { id: ID_CONFIG, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 7, y: 187, w: 55, h: 14, label: "" },
    Item { id: ID_DIFF, class: Class::Atom(ATOM_BUTTON), style: STYLE_DEFBUTTON, x: 167, y: 187, w: 50, h: 14, label: "" },
    Item { id: ID_CLOSE, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 222, y: 187, w: 50, h: 14, label: "" },
];

const CFG_TITLE: &str = "配置";
const CFG_W: u16 = 199;
const CFG_H: u16 = 91;

/// The 配置 box: the executable path with its browse button, the parameter
/// line with the preset menu beside it, 确认/取消.
const CFG_ITEMS: &[Item] = &[
    Item { id: ID_LBL_EXE, class: Class::Atom(ATOM_STATIC), style: STYLE_STATIC, x: 7, y: 7, w: 140, h: 11, label: "" },
    Item { id: ID_EXE, class: Class::Atom(ATOM_EDIT), style: STYLE_EDIT, x: 7, y: 20, w: 160, h: 13, label: "" },
    Item { id: ID_BROWSE, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 172, y: 19, w: 19, h: 14, label: "..." },
    Item { id: ID_LBL_ARGS, class: Class::Atom(ATOM_STATIC), style: STYLE_STATIC, x: 7, y: 38, w: 88, h: 10, label: "" },
    Item { id: ID_PRESET, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 100, y: 37, w: 67, h: 14, label: "" },
    Item { id: ID_ARGS, class: Class::Atom(ATOM_EDIT), style: STYLE_EDIT, x: 7, y: 51, w: 160, h: 14, label: "" },
    Item { id: 1, class: Class::Atom(ATOM_BUTTON), style: STYLE_DEFBUTTON, x: 42, y: 70, w: 50, h: 14, label: "" },
    Item { id: 2, class: Class::Atom(ATOM_BUTTON), style: STYLE_BUTTON, x: 97, y: 70, w: 50, h: 14, label: "" },
];

// ---------------------------------------------------------------------------
// Shared bits
// ---------------------------------------------------------------------------

/// A `%d`/`%s` row from the string table, or the fallback text.
fn msg(id: u32, args: &[&str], fallback: &str) -> String {
    lfmt(id, args).unwrap_or_else(|| fallback.to_string())
}

/// The box's own caption, from the language table.
fn box_title() -> String {
    trs(DLG_TITLE)
}

/// The chrome block every dogedit dialog paints itself with.
struct Face {
    bg: HBRUSH,
    field: HBRUSH,
    ink: COLORREF,
    face: COLORREF,
    field_face: COLORREF,
}

impl Face {
    fn new(theme: &dg_render::Theme) -> Self {
        Self {
            bg: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
            field: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_field)) },
            ink: to_colorref(theme.toolbar_fg),
            face: to_colorref(theme.toolbar_bg),
            field_face: to_colorref(theme.toolbar_field),
        }
    }
}

impl Drop for Face {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.bg.into());
            let _ = DeleteObject(self.field.into());
        }
    }
}

unsafe fn show_box(hwnd: HWND, text: &str, flags: MESSAGEBOX_STYLE) {
    let text = wide(text);
    let title = wide(&box_title());
    MessageBoxW(Some(hwnd), PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), flags);
}

/// Builds the tool's parameter line: `%1`/`%2` placeholders take the two
/// quoted files; without placeholders both files are appended, quoted, which
/// is what the common diff tools want.
fn build_params(args: &str, left: &str, right: &str) -> String {
    let l = format!("\"{left}\"");
    let r = format!("\"{right}\"");
    if args.contains("%1") || args.contains("%2") {
        args.replace("%1", &l).replace("%2", &r)
    } else if args.trim().is_empty() {
        format!("{l} {r}")
    } else {
        format!("{} {l} {r}", args.trim())
    }
}

// ---------------------------------------------------------------------------
// The 比较 box
// ---------------------------------------------------------------------------

struct Ctx {
    state: *mut EditorState,
    rows: Vec<String>,
    face: Face,
    /// The UI font for the children; the template's 8 pt `DS_SETFONT` block
    /// stays untouched because it is also the dialog-unit ruler.
    font: HFONT,
}

/// Opens the box over the frame. The document list is read once at open; a
/// document opened afterwards appears the next time the command runs.
pub(super) fn open(state: &mut EditorState, hwnd: HWND) {
    let rows: Vec<String> = state.diff_doc_rows();
    let theme = state.docs.act().theme.clone();
    let mut ctx = Box::new(Ctx {
        state: state as *mut EditorState,
        rows,
        face: Face::new(&theme),
        font: unsafe { crate::win32::uifont::create(crate::win32::uifont::window_dpi(hwnd)) },
    });
    let ptr = &mut *ctx as *mut Ctx;
    let tmpl = build_template(DLG_TITLE, DLG_STYLE, DLG_W, DLG_H, 0, ITEMS);
    let mut aligned = vec![0u32; (tmpl.len() + 3) / 4];
    unsafe {
        std::ptr::copy_nonoverlapping(tmpl.as_ptr(), aligned.as_mut_ptr() as *mut u8, tmpl.len());
    }
    unsafe {
        let _ = DialogBoxIndirectParamW(
            Some(instance()),
            aligned.as_ptr() as *const DLGTEMPLATE,
            Some(hwnd),
            Some(dialog_proc),
            LPARAM(ptr as isize),
        );
        let _ = DeleteObject(ctx.font.into());
    }
}


/// The list/text messages this box sends, through the `Option`-wrapped
/// signature the bindings give `SendMessageW`.
unsafe fn send(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    SendMessageW(hwnd, msg, Some(wparam), Some(lparam)).0
}

unsafe fn ctx(hwnd: HWND) -> &'static mut Ctx {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ctx)
}

unsafe fn fill_list(hwnd: HWND, id: i32) {
    let Ok(ctl) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    let ctx = ctx(hwnd);
    send(ctl, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for name in &ctx.rows {
        let text = wide(name);
        send(ctl, LB_ADDSTRING, WPARAM(0), LPARAM(text.as_ptr() as isize));
    }
}

unsafe fn list_sel(hwnd: HWND, id: i32) -> Option<usize> {
    let ctl = GetDlgItem(Some(hwnd), id).ok()?;
    let idx = send(ctl, LB_GETCURSEL, WPARAM(0), LPARAM(0));
    if idx < 0 {
        None
    } else {
        Some(idx as usize)
    }
}

/// The one 比较 press: validate the two selections, resolve both sides to
/// files, then hand them to the tool. Refusals speak through their message
/// boxes; the read-only edit carries the command line that did run.
unsafe fn run_diff(hwnd: HWND) {
    let Some(l) = list_sel(hwnd, ID_LEFT) else {
        // 15216: 请先选择2个文件!
        show_box(hwnd, &msg(15216, &[], "请先选择2个文件!"), MB_OK | MB_ICONINFORMATION);
        return;
    };
    let Some(r) = list_sel(hwnd, ID_RIGHT) else {
        show_box(hwnd, &msg(15216, &[], "请先选择2个文件!"), MB_OK | MB_ICONINFORMATION);
        return;
    };
    if l == r {
        // 15217: 被比较的文件是同一个文件!
        show_box(hwnd, &msg(15217, &[], "被比较的文件是同一个文件!"), MB_OK | MB_ICONINFORMATION);
        return;
    }
    let state = ctx(hwnd).state;
    let (Some(left), Some(right)) = (
        (*state).diff_doc_file(l, hwnd),
        (*state).diff_doc_file(r, hwnd),
    ) else {
        return; // the confirm was answered no, or the temp write failed loudly
    };
    // 这两个文件内容是一致的! — nothing to show a diff of.
    let same = match (std::fs::read(&left), std::fs::read(&right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same {
        show_box(hwnd, &msg(15234, &[], "这两个文件内容是一致的!"), MB_OK | MB_ICONINFORMATION);
        return;
    }
    let (exe, args) = (*state).diff_tool_config();
    let exe = exe.trim().to_string();
    // 15218 wants the tool path; an absolute path is checked here, a bare
    // name falls to the shell's own search and is reported by its result.
    if exe.is_empty()
        || (exe.contains('\\') || exe.contains('/')) && !std::path::Path::new(&exe).is_file()
    {
        let text = msg(15218, &[&exe], &format!("外部比较工具不存在!\n{exe}"));
        show_box(hwnd, &text, MB_OK | MB_ICONINFORMATION);
        return;
    }
    let params = build_params(&args, &left, &right);
    // The command line lands in the read-only edit: what ran, and what a user
    // can copy into a shortcut.
    let cmdline = format!("{exe} {params}");
    let _ = SetDlgItemTextW(hwnd, ID_CMDLINE, PCWSTR(wide(&cmdline).as_ptr()));
    let exe16 = wide(&exe);
    let params16 = wide(&params);
    let h = ShellExecuteW(
        Some(hwnd),
        None,
        PCWSTR(exe16.as_ptr()),
        PCWSTR(params16.as_ptr()),
        None,
        SW_SHOWNORMAL,
    );
    if h.0 as usize <= 32 {
        let text = msg(15218, &[&exe], &format!("外部比较工具不存在!\n{exe}"));
        show_box(hwnd, &text, MB_OK | MB_ICONINFORMATION);
    }
}

unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0 as isize);
            let ctx = ctx(hwnd);
            // The template's 8 pt font is the dialog-unit ruler, so only the
            // children's text font is replaced.
            crate::win32::uifont::set_children(hwnd, ctx.font);
            let theme = (*ctx.state).docs.act().theme.clone();
            localize_dialog(hwnd, 270);
            let dark = chrome::theme_is_dark(&theme);
            chrome::set_chrome_theme(&theme);
            chrome::dark_theme_children(hwnd, dark);
            if dark {
                // Both lists keep their dark themed scrollbars; the subclass
                // paints the rows with this dialog's field face and ink.
                chrome::themed_listbox(hwnd, ID_LEFT);
                chrome::themed_listbox(hwnd, ID_RIGHT);
            }
            chrome::title_bar(hwnd, dark);
            fill_list(hwnd, ID_LEFT);
            fill_list(hwnd, ID_RIGHT);
            1
        }
        WM_ERASEBKGND => {
            let st = ctx(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.face.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = ctx(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.face.ink, st.face.face, st.face.bg)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let st = ctx(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.face.ink, st.face.field_face, st.face.field)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            match id {
                ID_DIFF => run_diff(hwnd),
                // ID_CLOSE == 2 == IDCANCEL: the click and `Esc` land here
                // alike.
                ID_CLOSE => {
                    let _ = EndDialog(hwnd, 0);
                }
                ID_CONFIG => {
                    let ctx = ctx(hwnd);
                    let theme = (*ctx.state).docs.act().theme.clone();
                    config_dialog(hwnd, &theme);
                }
                _ => {}
            }
            1
        }
        WM_CLOSE => {
            let _ = EndDialog(hwnd, 0);
            1
        }
        WM_DESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            0
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// The 配置 box
// ---------------------------------------------------------------------------

struct CfgCtx {
    state: *mut EditorState,
    face: Face,
    /// The UI font for the children of the 配置 box.
    font: HFONT,
}

/// Opens 配置 over the 比较 box. 确认 writes `[Tool] DiffExe`/`DiffArgs`
/// through the frame state; 取消 leaves the stored values alone.
fn config_dialog(owner: HWND, theme: &dg_render::Theme) {
    let mut ctx = Box::new(CfgCtx {
        state: unsafe { cfg_state(owner) },
        // Fresh brushes: the 比较 box owns the ones its own face holds.
        face: Face::new(theme),
        font: unsafe { crate::win32::uifont::create(crate::win32::uifont::window_dpi(owner)) },
    });
    let ptr = &mut *ctx as *mut CfgCtx;
    let tmpl = build_template(CFG_TITLE, DLG_STYLE, CFG_W, CFG_H, 0, CFG_ITEMS);
    let mut aligned = vec![0u32; (tmpl.len() + 3) / 4];
    unsafe {
        std::ptr::copy_nonoverlapping(tmpl.as_ptr(), aligned.as_mut_ptr() as *mut u8, tmpl.len());
    }
    unsafe {
        let _ = DialogBoxIndirectParamW(
            Some(instance()),
            aligned.as_ptr() as *const DLGTEMPLATE,
            Some(owner),
            Some(config_proc),
            LPARAM(ptr as isize),
        );
        let _ = DeleteObject(ctx.font.into());
    }
}

/// The frame state behind the 比较 box, fished out of its window data: the
/// config box needs it for the stored tool and the persistence write.
unsafe fn cfg_state(owner: HWND) -> *mut EditorState {
    ctx(owner).state
}

unsafe fn cfg_ctx(hwnd: HWND) -> &'static mut CfgCtx {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CfgCtx)
}

unsafe fn set_text(hwnd: HWND, id: i32, text: &str) {
    let _ = SetDlgItemTextW(hwnd, id, PCWSTR(wide(text).as_ptr()));
}

unsafe fn read_text(hwnd: HWND, id: i32) -> String {
    let mut buf = [0u16; 1024];
    let _ = GetDlgItemTextW(hwnd, id, &mut buf);
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// The 预配置 menu: the common tools at the button's bottom-left corner.
/// Picking one fills both edits, ready for further edits before 确认.
unsafe fn run_presets(hwnd: HWND) {
    let Ok(btn) = GetDlgItem(Some(hwnd), ID_PRESET) else {
        return;
    };
    let mut rc = RECT::default();
    if GetWindowRect(btn, &mut rc).is_err() {
        return;
    }
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    for (i, (name, _, _)) in PRESETS.iter().enumerate() {
        let label = wide(name);
        if AppendMenuW(menu, MF_STRING, 1300 + i, PCWSTR::from_raw(label.as_ptr())).is_err() {
            let _ = DestroyMenu(menu);
            return;
        }
    }
    let chosen = TrackPopupMenu(
        menu,
        TPM_NONOTIFY | TPM_RETURNCMD,
        rc.left,
        rc.bottom,
        None,
        hwnd,
        None,
    )
    .0;
    let _ = DestroyMenu(menu);
    if chosen == 0 {
        return;
    }
    let (_, exe, args) = &PRESETS[(chosen - 1300) as usize];
    set_text(hwnd, ID_EXE, exe);
    set_text(hwnd, ID_ARGS, args);
}

/// The executable browse: an .exe filter, the box's caption as the title.
unsafe fn browse_exe(hwnd: HWND) -> Option<String> {
    let mut file = [0u16; 520];
    let filter: Vec<u16> = lstr(40144)
        .unwrap_or("可执行文件 (*.exe)|*.exe|所有文件 (*.*)|*.*")
        .replace('|', "\0")
        .encode_utf16()
        .chain([0, 0])
        .collect();
    let title: Vec<u16> = wide(&trs(CFG_TITLE));
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_HIDEREADONLY,
        ..Default::default()
    };
    if !GetOpenFileNameW(&mut ofn).as_bool() {
        return None;
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(0);
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&file[..len]))
}

unsafe extern "system" fn config_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0 as isize);
            let ctx = cfg_ctx(hwnd);
            crate::win32::uifont::set_children(hwnd, ctx.font);
            let theme = (*ctx.state).docs.act().theme.clone();
            localize_dialog(hwnd, 271);
            let dark = chrome::theme_is_dark(&theme);
            chrome::set_chrome_theme(&theme);
            chrome::dark_theme_children(hwnd, dark);
            chrome::title_bar(hwnd, dark);
            let (exe, args) = (*ctx.state).diff_tool_config();
            set_text(hwnd, ID_EXE, &exe);
            set_text(hwnd, ID_ARGS, &args);
            1
        }
        WM_ERASEBKGND => {
            let st = cfg_ctx(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.face.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = cfg_ctx(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.face.ink, st.face.face, st.face.bg)
        }
        WM_CTLCOLOREDIT => {
            let st = cfg_ctx(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.face.ink, st.face.field_face, st.face.field)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            match id {
                1 => {
                    let ctx = cfg_ctx(hwnd);
                    let exe = read_text(hwnd, ID_EXE);
                    let args = read_text(hwnd, ID_ARGS);
                    (*ctx.state).diff_set_tool(exe.trim(), &args);
                    let _ = EndDialog(hwnd, 1);
                }
                // 取消, and `Esc` (IDCANCEL is 2 as well).
                2 => {
                    let _ = EndDialog(hwnd, 0);
                }
                ID_BROWSE => {
                    if let Some(path) = browse_exe(hwnd) {
                        set_text(hwnd, ID_EXE, &path);
                    }
                }
                ID_PRESET => run_presets(hwnd),
                _ => {}
            }
            1
        }
        WM_CLOSE => {
            let _ = EndDialog(hwnd, 0);
            1
        }
        WM_DESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            0
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::build_params;

    /// Without placeholders both files go on the end, quoted; with `%1`/`%2`
    /// the line is a template the files are substituted into.
    #[test]
    fn params_append_quoted_files_without_placeholders() {
        assert_eq!(build_params("", "a.txt", "b.txt"), "\"a.txt\" \"b.txt\"");
        assert_eq!(
            build_params("/e /u", "a.txt", "b.txt"),
            "/e /u \"a.txt\" \"b.txt\""
        );
    }

    #[test]
    fn params_substitute_numbered_placeholders() {
        assert_eq!(
            build_params("-left %1 -right %2", "a.txt", "b.txt"),
            "-left \"a.txt\" -right \"b.txt\""
        );
        assert_eq!(build_params("%2 %1", "a.txt", "b.txt"), "\"b.txt\" \"a.txt\"");
    }
}
