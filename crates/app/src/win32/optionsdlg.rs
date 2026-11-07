//! 选项 — the Options dialog behind 工具 > 设置 > 常规 (`cm_config_options`, 1336).
//!
//! The shell is a 379×240 DLU dialog: category tree at 7,17 106×195, page host
//! at 118,0 258×210, and four buttons at y 219 (确定 165, 取消 218, 应用 270,
//! 帮助 322). Titles come from the language table; every page uses the same
//! template with a hidden static per row.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlgtpl;
use crate::win32::dpi::scaled;
// The template types live in `dlgtpl` now; re-exported so the pages, the
// external-diff dialog and go-to-anything keep their `optionsdlg::*` paths.
pub(super) use crate::win32::dlgtpl::{
    ATOM_BUTTON, ATOM_COMBOBOX, ATOM_EDIT, ATOM_LISTBOX, ATOM_STATIC, Class, Item,
};
use std::path::{Path, PathBuf};

use super::autobackup::{DEFAULT_BUDGET_MB, DEFAULT_KEEP_DAYS, DEFAULT_VERSIONS};
use super::pages;
use dg_render::theme::Theme;
use std::os::windows::ffi::OsStrExt;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    COLORREF, ERROR_SUCCESS, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateSolidBrush, DeleteObject, ExtTextOutW, FillRect, GetDC,
    GetDeviceCaps, GetTextExtentPoint32W, LOGPIXELSY, ScreenToClient, ETO_CLIPPED, HBRUSH, HBITMAP, HDC, HFONT, HGDIOBJ,
    InvalidateRect, MapWindowPoints, ReleaseDC, SelectObject, SetTextColor,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegDeleteKeyW, RegOpenKeyExW, RegSetValueW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, REG_SZ,
};
use windows::Win32::UI::Controls::Dialogs::{
    ChooseColorW, GetOpenFileNameW, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, OFN_FILEMUSTEXIST,
    OFN_HIDEREADONLY, OPENFILENAMEW,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_SELECTED,
    EM_REPLACESEL, EM_SETSEL, LIST_VIEW_ITEM_STATE_FLAGS,
    ImageList_Add, ImageList_Create, ImageList_Destroy, ImageList_Draw, HIMAGELIST, ILC_COLOR32,
    ILD_NORMAL, NMHDR, NM_CLICK,
    BST_CHECKED, BST_UNCHECKED, SetWindowTheme, UDM_SETBUDDY, UDM_SETRANGE32,
    LVIF_PARAM, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS,
    LVM_DELETEITEM, LVM_GETITEMCOUNT, LVM_GETITEMTEXTW, LVM_GETITEMW, LVM_GETNEXTITEM,
    LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVM_SETITEMW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_GETITEMSTATE, LVIS_STATEIMAGEMASK,
    LVS_EX_CHECKBOXES, LVS_EX_FULLROWSELECT, LVN_ITEMCHANGED,
    LVCF_FMT, LVCF_SUBITEM,
    LVCF_TEXT, LVCF_WIDTH, LVCFMT_LEFT, LVCOLUMNW, LVNI_SELECTED,
    TVHITTESTINFO, TVIF_HANDLE, TVIF_IMAGE, TVIF_PARAM, TVIF_SELECTEDIMAGE, TVIF_TEXT, TVINSERTSTRUCTW,
    TVINSERTSTRUCTW_0, TVITEMW, TVI_LAST, TVI_ROOT, TVM_EXPAND, TVM_GETITEMRECT, TVM_GETITEMW,
    TVM_GETNEXTITEM, TVM_HITTEST, TVM_INSERTITEMW, TVM_SELECTITEM, TVM_SETBKCOLOR, TVM_SETIMAGELIST,
    TVM_SETITEMW, TVM_SETTEXTCOLOR, TVE_EXPAND, TVGN_CARET, TVHT_ONITEM, NM_DBLCLK,
    TVN_SELCHANGEDW, TVS_FULLROWSELECT, TVS_NOSCROLL, TVS_SHOWSELALWAYS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, SetFocus, VK_ESCAPE, VK_RETURN, VK_SPACE,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, BIF_RETURNONLYFSDIRS, BROWSEINFOW,
    SHBrowseForFolderW, SHGetPathFromIDListW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateDialogIndirectParamW, CreatePopupMenu, CreateWindowExW, DestroyMenu,
    DestroyWindow, DialogBoxIndirectParamW, EndDialog, GetClientRect, GetDlgItem, GetMessagePos,
    GetParent, GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetClassNameW,
    IsWindow, PostMessageW, SendMessageW, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW,
    ShowWindow, TrackPopupMenu, DLGTEMPLATE, GWL_STYLE, GWLP_USERDATA, GW_CHILD, GW_HWNDNEXT,
    MessageBoxW, MB_ICONINFORMATION, MB_OK,
    HMENU, MF_STRING, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SW_HIDE, SW_SHOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE,
    SWP_NOZORDER, SystemParametersInfoW, TPM_LEFTALIGN,
    TPM_LEFTBUTTON, TPM_RETURNCMD, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_ERASEBKGND,
    WM_DRAWITEM, WM_INITDIALOG, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONUP, WM_MEASUREITEM,
    WM_NCDESTROY, WM_NOTIFY, WM_RBUTTONUP, WM_SETFONT, WS_BORDER,
    WS_CHILD, WS_CLIPCHILDREN, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL, CBS_DROPDOWNLIST, CB_ADDSTRING,
    CB_GETCURSEL, CB_GETDROPPEDSTATE, CB_SETCURSEL, CB_SETDROPPEDWIDTH, CB_SHOWDROPDOWN,
    CBN_KILLFOCUS, CBN_SELCHANGE, CBN_SELENDCANCEL, EN_KILLFOCUS, ES_AUTOHSCROLL, BM_GETCHECK,
    BM_SETCHECK, LB_ADDSTRING, LB_DELETESTRING, LB_GETCOUNT, LB_GETCURSEL,
    LB_GETTEXT, LB_GETTEXTLEN, LB_GETITEMDATA, LB_INSERTSTRING, LB_RESETCONTENT,
    LB_SETCURSEL, LB_SETITEMDATA, LBN_SELCHANGE, CB_GETCOUNT,
    CB_GETITEMDATA, CB_SETITEMDATA, CB_RESETCONTENT, CB_GETLBTEXT,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;

use crate::win32::customdraw;
use crate::win32::chrome;
use crate::win32::uifont;
use crate::win32::view::instance;
use crate::win32::window::to_colorref;

// ---------------------------------------------------------------------------
// The dialog tables
// ---------------------------------------------------------------------------

const SHELL_STYLE: u32 = 0x80c8_00c8;
const SHELL_W: u16 = 379;
const SHELL_H: u16 = 240;
const PAGE_STYLE: u32 = 0x4008_0048;
const PAGE_W: u16 = 263;
const PAGE_H: u16 = 210;

/// The 模板 page, which 配置模板 (1230) opens directly.
pub(super) const TEMPLATE_PAGE: usize = 213;

const ID_TREE: i32 = 1200;
const ID_HOST: i32 = 1201;
const ID_APPLY: i32 = 1202;
const ID_HELP: i32 = 1203;
const ID_CANCEL: i32 = 1204;
const ID_OK: i32 = 1205;
const ID_CATEGORY: i32 = 3000;

const ID_PAGE_TREE: i32 = 2000;
const ID_ROW_RECENT: i32 = 1201;
const ID_ROW_START_RECENT: i32 = 1265;
const ID_ROW_MULINST: i32 = 1205;
const ID_ROW_SHELL: i32 = 1206;
const ID_ROW_SHELL_FIND: i32 = 1222;
const ID_ROW_TEAROFF: i32 = 1214;
const ID_ROW_SYSBG: i32 = 1209;
const ID_ROW_DOCK_THEME: i32 = 1220;
const ID_ROW_HIDE_PATH: i32 = 1224;
const ID_ROW_FONT: i32 = 1221;
const ID_ROW_UPDATE: i32 = 1215;

/// 通知 — the completion notifications of the external-tool runner (`win32::notify`).
/// These are this implementation's own ids, well clear of every id the options dialog
/// and its pages use. They sit at the end of 常规, whose list still has the room for them.
const ID_ROW_FLASH: i32 = 1301;
const ID_ROW_SOUND: i32 = 1302;
const ID_ROW_SOUND_FILE: i32 = 1303;
const ID_ROW_PROGRESS: i32 = 1304;
const ID_ROW_FAIL_SOUND_FILE: i32 = 1305;

/// Switching from `TVN_SELCHANGED` instead builds or shows a page while the tree is still
/// inside its own button-down, and that click never completes — the page only appears on
/// the second click.
const WM_PAGE_READY: u32 = WM_APP + 1;
/// The same deferral for one property row. The row's editor is never created while the tree
/// still owns the click.
const WM_ROW_ACTIVATE: u32 = WM_APP + 2;
/// 应用 landed in the file: the frame re-reads what a page owns and cannot wait
/// for the dialog to close for. The 工具条 page's button table is the case that
/// shows on screen.
pub(super) const WM_SETTINGS_APPLIED: u32 = WM_APP + 0x4C;
/// Subclass ids
const CATEGORY_SUBCLASS: usize = 6;
const PROPTREE_SUBCLASS: usize = 7;

const ID_EDITOR: i32 = 30001;

const IDCANCEL_WORD: u32 = 2;
const BN_CLICKED: u32 = 0;

/// The 常规 page's `zh-CN.lng` captions; the hidden statics in the template
/// carry the same text, and the page reads the labels back from them.
const PAGE_ROWS: &[(i32, &str)] = &[
    (ID_ROW_RECENT, "记忆最近打开的文件个数(0~64):"),
    (ID_ROW_START_RECENT, "开始页显示最近的文件"),
    (ID_ROW_MULINST, "允许多个实例同时运行"),
    (ID_ROW_SHELL, "右键菜单添加:用 DogEdit 打开"),
    (ID_ROW_SHELL_FIND, "右键菜单添加:在文件中查找"),
    (ID_ROW_TEAROFF, "在菜单上显示一个小剪刀(需重启)"),
    (ID_ROW_SYSBG, "主工具条的背景使用系统样式"),
    (ID_ROW_DOCK_THEME, "在停靠窗口上使用编辑区配色"),
    (ID_ROW_HIDE_PATH, "标题栏不显示路径信息, 只显示文件名称"),
    (ID_ROW_FONT, "界面使用的字体(需重启)"),
    (ID_ROW_UPDATE, "检查最新版本"),
    (ID_ROW_FLASH, "外部工具执行完成后闪烁任务栏按钮(颜色由系统决定,成败看图标徽标)"),
    (ID_ROW_PROGRESS, "外部工具执行完成后任务栏按钮显示进度(成功系统色,失败红色)"),
    (ID_ROW_SOUND, "外部工具执行完成后播放提示音"),
    (ID_ROW_SOUND_FILE, "成功提示音文件:"),
    (ID_ROW_FAIL_SOUND_FILE, "失败提示音文件:"),
];

/// `zh-CN.lng`'s auto-update choices.  The stored value is the menu id
/// (1..4); the row keeps the zero-based index.
const UPDATE_ITEMS: &[&str] = &["一天一次", "一周一次", "一月一次", "禁止更新"];
/// Fills the row from it. The captions are the menu's own strings. The size in each caption
/// is tenths of a point, which is what the UI-font helper puts in the LOGFONT.
const FONT_ITEMS: &[&str] = &[
    "系统默认",
    "Tahoma, 8pt",
    "Segoe UI, 8pt",
    "Segoe UI, 9pt",
    "Meiryo, 8pt",
    "Meiryo, 9pt",
    "Verdana, 8pt",
];
/// The UI-font ids behind `FONT_ITEMS`, in the same order.
const FONT_IDS: &[u32] = &[1200, 1, 2, 3, 4, 5, 6];

/// The index the UI-font list gives a stored id. An id the menu does
/// not have.
fn font_index(id: u32) -> usize {
    FONT_IDS
        .iter()
        .position(|&m| m == id)
        .unwrap_or(0)
}


// ---------------------------------------------------------------------------
// The template writer
// ---------------------------------------------------------------------------

/// The `DS_SETFONT` block a template carries. The dialog manager turns it into
/// the font every control inherits — and, because dialog units are derived from
/// that font's metrics, into the pixel size of the whole dialog. Two blocks
/// that look equivalent are not:
///
/// * [`FontBlock::legacy`] — the literal `8 pt "Ms Shell Dlg"` of a dialog
///   template, written with `DEFAULT_CHARSET`. The manager renames the face to
///   "MS Shell Dlg 2" and GDI picks Tahoma *at that literal 8 pt*: at 125 % DPI
///   a `-13` font whose DLU base unit is 7 x 16, so a 285 x 208 dialog comes
///   out 499 x 416.
/// * [`FontBlock::shell`] carry `pointsize + face` and no charset field at all,
///   so GDI resolves "Ms Shell Dlg" to the *shell dialog font*, size included:
///   Microsoft YaHei UI 9 pt on a zh-CN system, `-15`, base unit 8 x 16, dialog
///   568 x 414. Asking `SPI_GETNONCLIENTMETRICS` for `lfMessageFont` is that
///   font, so neither the face nor the locale is hard-coded here.
pub(super) struct FontBlock {
    points: u16,
    weight: u16,
    italic: u8,
    charset: u8,
    face: String,
}

impl FontBlock {
    /// The block every reimplemented dialog has used so far.
    pub(super) fn legacy(weight: u16) -> Self {
        Self {
            points: 8,
            weight,
            italic: 0,
            charset: 1, // DEFAULT_CHARSET — what triggers the "MS Shell Dlg 2" swap
            face: "Ms Shell Dlg".to_string(),
        }
    }

    /// The shell dialog font, measured rather than named.
    pub(super) fn shell() -> Self {
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: core::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let got = unsafe {
            SystemParametersInfoW(
                SPI_GETNONCLIENTMETRICS,
                ncm.cbSize,
                Some(&mut ncm as *mut _ as *mut core::ffi::c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        if got.is_err() {
            return Self::legacy(400);
        }
        let lf = ncm.lfMessageFont;
        // `lfHeight` is pixels; the template wants points, and the manager will
        // turn the points back into pixels with the same nearest rounding.
        let dpi = unsafe {
            let hdc = GetDC(None);
            let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSY);
            let _ = ReleaseDC(None, hdc);
            if dpi > 0 { dpi } else { 96 }
        };
        let pixels = if lf.lfHeight < 0 {
            -lf.lfHeight
        } else {
            lf.lfHeight
        };
        let points = ((pixels * 72) as f32 / dpi as f32).round().max(1.0) as u16;
        let end = lf
            .lfFaceName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(lf.lfFaceName.len());
        Self {
            points,
            weight: lf.lfWeight as u16,
            italic: 0,
            // `DEFAULT_CHARSET`, not the message font's own: a template with no
            // charset field means exactly this. Copying the message font's
            // charset (134 here) keeps Microsoft YaHei UI and its 9 x 20
            // metrics — a dialog 100 px too wide and 100 too tall.
            charset: 1,
            face: String::from_utf16_lossy(&lf.lfFaceName[..end]),
        }
    }

    fn as_font(&self) -> dlgtpl::Font<'_> {
        dlgtpl::Font {
            points: self.points,
            weight: self.weight,
            italic: self.italic,
            charset: self.charset,
            face: &self.face,
        }
    }
}

/// The legacy-font writer every page uses. Kept as a named wrapper so the
/// pages / batch / external-diff / go-to-anything call sites stay unchanged.
pub(super) fn build_template(
    title: &str,
    style: u32,
    w: u16,
    h: u16,
    weight: u16,
    items: &[Item],
) -> Vec<u8> {
    build_template_font(title, style, w, h, &FontBlock::legacy(weight), items)
}

/// [`build_template`] with the font spelled out — see [`FontBlock`] for why the
/// choice is worth a parameter.
pub(super) fn build_template_font(
    title: &str,
    style: u32,
    w: u16,
    h: u16,
    font: &FontBlock,
    items: &[Item],
) -> Vec<u8> {
    dlgtpl::build(title, style, 0, w, h, font.as_font(), items)
}

/// The options dialog's shell, item for item: the buttons, 类别:, the tree, the host,
/// and the etched separator at y 215.
fn build_shell_template() -> Vec<u8> {
    let items = [
        Item {
            id: ID_OK,
            class: Class::Atom(ATOM_BUTTON),
            style: 0x5001_0000,
            x: 165,
            y: 219,
            w: 50,
            h: 14,
            label: "确定",
        },
        Item {
            id: ID_CANCEL,
            class: Class::Atom(ATOM_BUTTON),
            style: 0x5001_0000,
            x: 218,
            y: 219,
            w: 50,
            h: 14,
            label: "取消",
        },
        Item {
            id: ID_APPLY,
            class: Class::Atom(ATOM_BUTTON),
            style: 0x5001_0001,
            x: 270,
            y: 219,
            w: 50,
            h: 14,
            label: "应用",
        },
        Item {
            id: ID_HELP,
            class: Class::Atom(ATOM_BUTTON),
            style: 0x5001_0000,
            x: 322,
            y: 219,
            w: 50,
            h: 14,
            label: "帮助",
        },
        Item {
            id: ID_CATEGORY,
            class: Class::Atom(ATOM_STATIC),
            style: 0x5002_0000,
            x: 7,
            y: 5,
            w: 80,
            h: 10,
            label: "类别:",
        },
        Item {
            id: ID_TREE,
            class: Class::Name("SysTreeView32"),
            style: 0x5081_0000,
            x: 7,
            y: 17,
            w: 106,
            h: 195,
            label: "",
        },
        Item {
            id: ID_HOST,
            class: Class::Atom(ATOM_STATIC),
            style: 0x5002_0000,
            x: 118,
            y: 0,
            w: 258,
            h: 210,
            label: "",
        },
        Item {
            id: -1,
            class: Class::Atom(ATOM_STATIC),
            style: 0x5000_1007,
            x: 1,
            y: 215,
            w: 378,
            h: 1,
            label: "",
        },
    ];
    build_template("选项", SHELL_STYLE, SHELL_W, SHELL_H, 400, &items)
}

/// The options dialog's page body.  The statics are hidden row definitions:
/// their positions are arbitrary, their order is the row order, and their ids
/// name the setting.
fn build_page_template() -> Vec<u8> {
    let mut items = vec![Item {
        id: ID_PAGE_TREE,
        class: Class::Name("SysTreeView32"),
        style: 0x5081_2800,
        x: 0,
        y: 18,
        w: 245,
        h: 190,
        label: "",
    }];
    let statics = [
        (ID_ROW_RECENT, 15, 120),
        (ID_ROW_START_RECENT, 15, 230),
        (ID_ROW_MULINST, 30, 230),
        (ID_ROW_SHELL, 45, 235),
        (ID_ROW_SHELL_FIND, 135, 205),
        (ID_ROW_SYSBG, 75, 240),
        (ID_ROW_TEAROFF, 90, 215),
        (ID_ROW_DOCK_THEME, 135, 205),
        (ID_ROW_UPDATE, 135, 205),
        (ID_ROW_FONT, 135, 205),
        (ID_ROW_HIDE_PATH, 30, 230),
        // 通知 — the completion notifications of the external-tool
        // runner (`win32::notify`), so these are this
        // implementation's own ids, well clear of every id the
        // options dialog and its pages use. They sit at the end of
        // 常规, whose list still has the room for them. These statics
        // only carry labels, so their positions are arbitrary.
        (ID_ROW_FLASH, 60, 240),
        (ID_ROW_PROGRESS, 105, 240),
        (ID_ROW_SOUND, 75, 240),
        (ID_ROW_SOUND_FILE, 90, 240),
        (ID_ROW_FAIL_SOUND_FILE, 90, 240),
    ];
    for (id, y, w) in statics {
        let label = PAGE_ROWS
            .iter()
            .find(|(row, _)| *row == id)
            .map(|(_, text)| *text)
            .unwrap_or("");
        items.push(Item {
            id,
            class: Class::Atom(ATOM_STATIC),
            style: 0x4002_0000,
            x: 0,
            y,
            w,
            h: 12,
            label,
        });
    }
    build_template("", PAGE_STYLE, PAGE_W, PAGE_H, 0, &items)
}

// ---------------------------------------------------------------------------
// setting.ini
// ---------------------------------------------------------------------------

/// `setting.ini` next to the executable
///
/// Shared with the `[Ctags]` reader (`win32::ctags`), which needs the same file
/// but nothing else from this module.
pub(super) fn ini_path() -> PathBuf {
    let mut p = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    p.push("setting.ini");
    p
}

#[derive(Clone)]
pub(super) struct IniText(dg_core::ini::Ini);

impl IniText {
    pub(super) fn load(path: &Path) -> Self {
        Self(dg_core::ini::Ini::load(path).unwrap_or_default())
    }

    /// `[section]` 下的一个值：段名键名不分大小写，值去空白，空值当「没有」。
    pub(super) fn get(&self, section: &str, key: &str) -> Option<String> {
        self.0.get(section, key).map(str::to_string)
    }

    /// 写一个键：改最后一处同名键，找不到就插在段末，段不在就补在文件末尾。
    pub(super) fn set(&mut self, section: &str, key: &str, value: &str) {
        self.0.set(section, key, value);
    }

    /// Replace a section's body with `rows`, keeping the section where it was.
    pub(super) fn replace_section(&mut self, section: &str, rows: &[(String, String)]) {
        self.0.replace_section(section, rows);
    }

    pub(super) fn remove_section(&mut self, section: &str) {
        self.0.remove_section(section);
    }

    /// Every section name starting with `prefix` (for the `Temp%d` blocks).
    pub(super) fn sections_with_prefix(&self, prefix: &str) -> Vec<String> {
        self.0.sections_with_prefix(prefix)
    }

    pub(super) fn save(&self, path: &Path) {
        let _ = self.0.save(path);
    }
}

fn bool_text(b: bool) -> &'static str {
    if b { "Y" } else { "N" }
}

pub(super) fn read_bool(ini: &IniText, section: &str, key: &str, default: bool) -> bool {
    match ini.get(section, key).as_deref() {
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "y" | "yes" | "1" | "true" | "on" => true,
            "n" | "no" | "0" | "false" | "off" => false,
            _ => default,
        },
        None => default,
    }
}

fn read_u32(ini: &IniText, section: &str, key: &str, default: u32) -> u32 {
    ini.get(section, key)
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

/// `[Notify]` of `setting.ini`. The tool runner reads these on every
/// completion, so they are read on their own (`load_notify`) rather than through
/// `Settings::load`, which also probes the shell registry.
const NOTIFY_SECTION: &str = "Notify";
const NOTIFY_FLASH: &str = "FlashTaskbar";
const NOTIFY_PROGRESS: &str = "TaskbarProgress";
const NOTIFY_SOUND: &str = "Sound";
const NOTIFY_SOUND_FILE: &str = "SoundFile";
const NOTIFY_FAIL_SOUND_FILE: &str = "FailSoundFile";

/// The General page's values
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Settings {
    pub recent_doc: u32,
    pub start_page_recent: bool,
    pub mul_inst: bool,
    pub shell_app: bool,
    pub shell_find: bool,
    pub tear_off: bool,
    pub sys_bg: bool,
    pub dock_theme: bool,
    pub hide_file_path: bool,
    pub ui_font: u32,
    pub auto_update: u32,
    /// 通知 — the completion notifications of the external-tool runner and
    /// the sound files they play. See `load_notify` for the reader it uses.
    pub flash_taskbar: bool,
    pub progress: bool,
    pub notify_sound: bool,
    pub sound_file: String,
    pub fail_sound_file: String,
}

impl Settings {
    fn load(ini: &IniText) -> Self {
        // An id the menu does not carry is read back as the system default the locale
        // default it falls back to.
        let raw_font = read_u32(ini, "Setting", "UIFont", FONT_IDS[0]);
        let ui_font = if FONT_IDS.contains(&raw_font) {
            raw_font
        } else {
            FONT_IDS[0]
        };
        Self {
            recent_doc: read_u32(ini, "Setting", "RecentDoc", 16).min(64),
            start_page_recent: read_bool(ini, "Setting", "StartPageRecent", true),
            mul_inst: read_bool(ini, "Setting", "MulInst", false),
            shell_app: shell_registered(false),
            shell_find: shell_registered(true),
            tear_off: read_bool(ini, "Setting", "TearOff", true),
            // Windows 10 and later answer true.
            sys_bg: read_bool(ini, "Setting", "SysBG", true),
            dock_theme: read_bool(ini, "DockWindow", "Theme", false),
            hide_file_path: read_bool(ini, "Setting", "HideFilePath", false),
            ui_font,
            auto_update: read_u32(ini, "Setting", "AutoUpdate", 1).clamp(1, 4),
            flash_taskbar: read_bool(ini, NOTIFY_SECTION, NOTIFY_FLASH, true),
            progress: read_bool(ini, NOTIFY_SECTION, NOTIFY_PROGRESS, true),
            notify_sound: read_bool(ini, NOTIFY_SECTION, NOTIFY_SOUND, true),
            sound_file: ini.get(NOTIFY_SECTION, NOTIFY_SOUND_FILE).unwrap_or_default(),
            fail_sound_file: ini
                .get(NOTIFY_SECTION, NOTIFY_FAIL_SOUND_FILE)
                .unwrap_or_default(),
        }
    }
}

/// Whether `HKCU\SOFTWARE\Classes\...\Shell\DogEdit` exists.  `directory`
/// selects the 在文件中查找 entry.
fn shell_registered(directory: bool) -> bool {
    let path: Vec<u16> = if directory {
        "SOFTWARE\\Classes\\Directory\\shell\\DogEdit\0".encode_utf16().collect()
    } else {
        "SOFTWARE\\Classes\\*\\Shell\\DogEdit\0".encode_utf16().collect()
    };
    unsafe {
        let mut h = HKEY::default();
        let rc = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            KEY_READ,
            &mut h,
        );
        if rc == ERROR_SUCCESS {
            let _ = RegCloseKey(h);
            true
        } else {
            false
        }
    }
}

/// The ini file plus the values read from it, read once when the dialog opens.
pub(super) fn load_settings() -> Settings {
    let ini = IniText::load(&ini_path());
    Settings::load(&ini)
}

/// The `[Notify]` values, as the tool runner wants them when a child exits.
pub(super) struct Notify {
    pub(super) flash: bool,
    pub(super) progress: bool,
    pub(super) sound: bool,
    /// The success sound; empty means the system's OK beep.
    pub(super) file: String,
    /// The failure sound; empty means the system's error beep.
    pub(super) fail_file: String,
}

/// Read on their own rather than through `Settings::load`: that one also
/// probes the shell registry for the two 右键菜单 rows, which have nothing to
/// do with a finished tool, and this runs on every completion.
pub(super) fn load_notify() -> Notify {
    let ini = IniText::load(&ini_path());
    Notify {
        flash: read_bool(&ini, NOTIFY_SECTION, NOTIFY_FLASH, true),
        progress: read_bool(&ini, NOTIFY_SECTION, NOTIFY_PROGRESS, true),
        sound: read_bool(&ini, NOTIFY_SECTION, NOTIFY_SOUND, true),
        file: ini.get(NOTIFY_SECTION, NOTIFY_SOUND_FILE).unwrap_or_default(),
        fail_file: ini
            .get(NOTIFY_SECTION, NOTIFY_FAIL_SOUND_FILE)
            .unwrap_or_default(),
    }
}

/// The chrome's `Setting/UIFont` id, resolved once for the process.
pub(super) fn ui_font_id() -> u32 {
    static ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *ID.get_or_init(|| load_settings().ui_font)
}

// ---------------------------------------------------------------------------
// The property list's row sprites
// ---------------------------------------------------------------------------

/// The four markers a property row draws. These four are the ones the list
/// uses, painted on the strip's 16 unit grid so the same shape serves its 16
/// px and 24 px halves.
#[derive(Clone, Copy, Debug)]
enum Sprite {
    Unchecked,
    Checked,
    Edit,
    List,
}

/// The strip's two colours: it is a monochrome glyph sheet, ink on paper.
const INK: [u8; 3] = [0x00, 0x00, 0x00];
const PAPER: [u8; 3] = [0xFF, 0xFF, 0xFF];

/// A square BGRA cell, drawn top-down as `CreateBitmap` wants it.
struct Sheet {
    size: usize,
    px: Vec<u8>,
}

impl Sheet {
    /// Every pixel whose centre lies inside the shape, in unit coordinates.
    fn paint(&mut self, mut inside: impl FnMut(f32, f32) -> bool, rgb: [u8; 3]) {
        let u = self.size as f32 / 16.0;
        for y in 0..self.size {
            for x in 0..self.size {
                if !inside((x as f32 + 0.5) / u, (y as f32 + 0.5) / u) {
                    continue;
                }
                let o = (y * self.size + x) * 4;
                self.px[o] = rgb[2];
                self.px[o + 1] = rgb[1];
                self.px[o + 2] = rgb[0];
                self.px[o + 3] = 0xFF;
            }
        }
    }

    /// The rectangle `x0..x1` by `y0..y1`.
    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, rgb: [u8; 3]) {
        self.paint(|x, y| x >= x0 && x < x1 && y >= y0 && y < y1, rgb);
    }

    /// The segment widened to `w`, which is how a check or a pencil is drawn.
    fn stroke(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, rgb: [u8; 3]) {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = dx * dx + dy * dy;
        self.paint(|x, y| {
            let t = (((x - x0) * dx + (y - y0) * dy) / len2).clamp(0.0, 1.0);
            let (px, py) = (x0 + t * dx - x, y0 + t * dy - y);
            px * px + py * py <= w * w / 4.0
        }, rgb);
    }
}

/// One sprite as 32 bpp BGRA; the unit grid is 16 across whatever the size.
fn sprite(kind: Sprite, size: usize) -> Vec<u8> {
    let mut s = Sheet { size, px: vec![0; size * size * 4] };
    // A check box is a 2-unit frame inset one unit, with the strip's spare
    // columns left empty on the right; a value row fills the cell with a
    // 1-unit frame.
    let ((x0, y0, x1, y1), border) = match kind {
        Sprite::Unchecked | Sprite::Checked => ((1.0, 1.0, 14.0, 14.0), 2.0),
        Sprite::Edit | Sprite::List => ((0.0, 0.0, 16.0, 16.0), 1.0),
    };
    s.rect(x0, y0, x1, y1, INK);
    s.rect(x0 + border, y0 + border, x1 - border, y1 - border, PAPER);
    match kind {
        Sprite::Unchecked => {}
        Sprite::Checked => {
            s.stroke(4.4, 6.4, 6.2, 8.4, 1.9, INK);
            s.stroke(6.2, 8.4, 10.4, 4.2, 1.9, INK);
        }
        Sprite::Edit => {
            s.stroke(8.8, 4.6, 5.4, 8.0, 3.2, INK);
            s.stroke(5.4, 8.0, 4.2, 10.6, 1.4, INK);
        }
        Sprite::List => {
            s.stroke(6.2, 3.2, 3.2, 6.2, 3.0, INK);
            s.stroke(3.2, 6.2, 2.3, 8.4, 1.4, INK);
            s.rect(6.2, 7.3, 14.0, 8.4, INK);
            s.paint(
                |x, y| {
                    let k = (11.4 - y) / (11.4 - 8.6);
                    y >= 8.6 && y < 11.4 && (x - 10.1).abs() <= 2.3 * k
                },
                INK,
            );
        }
    }
    s.px
}

/// The image list the options list builder hangs on the property tree: 16 px below
/// 144 DPI, 24 px above.  A 32 bpp list reads per-pixel alpha, so the untouched
/// cells stay transparent instead of needing the strip's magenta key.
unsafe fn property_images(dpi: i32) -> HIMAGELIST {
    let size: usize = if dpi >= 144 { 24 } else { 16 };
    let size32 = size as i32;
    let list = ImageList_Create(size32, size32, ILC_COLOR32, 4, 1);
    if list.is_invalid() {
        return list;
    }
    for kind in [Sprite::Unchecked, Sprite::Checked, Sprite::Edit, Sprite::List] {
        let px = sprite(kind, size);
        let bmp: HBITMAP = CreateBitmap(
            size32,
            size32,
            1,
            32,
            Some(px.as_ptr() as *const core::ffi::c_void),
        );
        if !bmp.is_invalid() {
            ImageList_Add(list, bmp, Some(HBITMAP::default()));
            let _ = DeleteObject(HGDIOBJ(bmp.0));
        }
    }
    list
}

/// Sprite indices, in the order `property_images` adds them.
const IMG_UNCHECKED: i32 = 0;
const IMG_CHECKED: i32 = 1;
const IMG_EDIT: i32 = 2;
const IMG_LIST: i32 = 3;

/// A page's template, built from the control table in `pages`: the same dialog
/// style
/// with the captions filled in by `localize_page` from the 2052 table.
fn page_template(id: usize) -> Option<Vec<u8>> {
    let def = pages::page_def(id)?;
    Some(build_template("", def.style, def.w, def.h, 0, def.items))
}

/// Captions for a page, from the 5.0 2052 language table.
fn page_texts(id: usize) -> &'static [(i32, &'static str)] {
    match id {
        238 => &[
            (1202, "选区文本保持语法高亮的前景色"),
            (1203, "选区文本的背景色扩展到窗口右侧"),
            (1209, "移动光标时保持选区或者查找高亮"),
            (1211, "按下Tab时跳出刚刚自动输入的配对字符"),
            (1210, "光标列号从1开始计数"),
            (1215, "当光标距边界?行时进行滚动(0~16)"),
            (1216, "禁止出现在行头的字符:"),
            (1217, "禁止出现在行尾的字符:"),
            (1218, "强制剪贴板的换行符格式:"),
            (1219, "中文括号(必须配对输入):"),
            (1220, "选区高亮最大字符数:"),
        ],
        275 => &[
            (1200, "折叠样式:"),
            (1201, "光标样式:"),
            (1202, "当前行样式:"),
            (1203, "制表符样式:"),
            (1204, "空格样式:"),
            (1205, "文档地图活动区域:"),
            (1208, "行编辑状态栏的宽度:"),
            (1210, "缩进线:"),
            (1211, "书签:"),
            (1206, "总是隐藏垂直滚动条"),
            (1207, "总是隐藏水平滚动条"),
            (1209, "垂直滚动条上显示工具条"),
            (1221, "光标效果:"),
        ],
        252 => &[
            (1200, "当文件行数小于?时自动换行:"),
            (1201, "当文件小于?(MB)时直接用内存打开:"),
            (1202, "自动编码探测缓存大小(KB):"),
            (1204, "查找全部时,限制每次输出的文本的大小:"),
            (1205, "大文件语法着色的行数上限:"),
        ],
        217 => &[
            (3000, "多文档标签样式:"),
            (3001, "停靠窗格标签样式:"),
            (3002, "激活Tab的背景色"),
            (1202, "在标签上显示图标"),
            (1203, "当文档被修改时显示星号(*)"),
            (1200, "在主程序下方显示标签栏"),
            (1204, "使各项目为固定宽度"),
            (1216, "固定宽度的大小(50~300像素)"),
            (1205, "当仅有一个标签时隐藏标签栏"),
            (1207, "显示滚动按钮"),
            (1208, "显示文件下拉按钮"),
            (1209, "在右侧显示全局关闭按钮"),
            (1210, "在每一项中显示关闭按钮"),
            (1211, "双击左键时关闭标签"),
            (1212, "用文档的首行文本动态更新tab标题"),
            (1214, "没有文档时,执行关闭文档命令则退出程序"),
            (1215, "在最右侧打开新标签"),
            (1217, "关闭文件的时候优先激活左侧的标签"),
        ],
        224 => &[
            (1207, "监视文件变化"),
            (1212, "启动时创建一个空白文件"),
            (1216, "编码探测"),
            (1221, "文件未被修改时也可以保存"),
            (3000, "新建文件或0字节文件:"),
            (3001, "格式:"),
            (3002, "编码:"),
            (3003, "其它"),
            (3004, "只读文件的修改:"),
            (3005, "默认模式:"),
            (3006, "类型:"),
        ],
        247 => &[
            (1204, "直接使用当前编辑器视图的字体"),
            (3000, "边距(毫米)"),
            (3001, "下"),
            (3002, "左"),
            (3003, "上"),
            (3004, "右"),
            (3005, "页眉:"),
            (3006, "页脚:"),
        ],
        254 => &[
            (1209, "使用数字键1~9选择候选词(默认是ALT+1~9)"),
            (3000, "触发按键:"),
            (3003, "全部:"),
            (3004, "已选择:"),
            (3005, "匹配方法:"),
        ],
        296 => &[(3000, "自定义状态栏的显示:")],
        207 => &[(3000, "图标主题:"), (1209, "打开目录")],
        213 => &[
            (1210, "以snippet形式插入模板文件(限制4KB)"),
            (1211, "模板文件使用自身的编码和换行符"),
            (3000, "标题:"),
            (3001, "类型:"),
            (3002, "模板:"),
        ],
        273 => &[
            (0, "主机/目录映射"),
            (1, "确认"),
            (2, "取消"),
            (3000, "主机:(例如: http://localhost:8080/myproject)"),
            (3001, r"目录:(例如: c:\iisroot\myproject)"),
        ],
        272 => &[
            (1200, "添加"),
            (1201, "修改"),
            (1202, "删除"),
            (3000, "主机/目录 设置:"),
            (3001, "外部浏览器:"),
            (3002, "其它浏览器:"),
        ],
        246 => &[
            (1200, "启动时恢复上次退出前未关闭的文件"),
            (1201, "记忆曾经打开过的文件的光标位置"),
            (1202, "记忆/恢复书签"),
            (1203, "记忆/恢复被修改的文件的内容(热保存)"),
            (3000, "设置临时文件夹用以存储/恢复被修改的文件:"),
            (3001, "※不设置的话,则使用系统默认临时文件夹"),
            (3002, "恢复"),
        ],
        215 => &[
            (3000, "选择您的界面语言(需重启):"),
            (3001, "作者:"),
            (3002, "主页:"),
        ],
        244 => &[(3000, "以HEX模式打开扩展名为下面的文件:")],
        285 => &[
            (1200, "使用自动备份"),
            (1211, "自定义文件夹"),
            (3003, "设置阈值,不对大文件进行自动备份(MB)"),
            (3005, "自动保存时间间隔(分钟,0:禁止)"),
            (3006, "每个文件保留的版本数(1~500)"),
            (3007, "历史保留天数(0:永久保留)"),
            (3008, "历史总大小上限(MB,0:不限制)"),
        ],
        286 => &[
            (3000, "代理服务器类型:"),
            (3001, "主机:"),
            (3002, "端口:"),
            (3003, "用户名:"),
            (3004, "密码:"),
            (1200, "无"),
            (1201, "HTTP/1.1 use CONNECT method"),
            (1202, "SOCKS 5"),
        ],
        _ => &[],
    }
}

// ---------------------------------------------------------------------------
// Categories — the 5.0 dialog's tree
// ---------------------------------------------------------------------------

struct Cat {
    label: &'static str,
    page: usize,
    children: &'static [Cat],
}

/// The category tree this table follows is the options tree in the language file: 会话/语言 are top-level entries, and 文件 is the
/// first page under the 编辑 group.
const EDIT_GROUP_CATS: &[Cat] = &[
    Cat { label: "文件", page: 224, children: &[] },
    Cat { label: "模板", page: TEMPLATE_PAGE, children: &[] },
    Cat { label: "备份", page: 285, children: &[] },
    Cat { label: "编辑", page: 238, children: &[] },
    Cat { label: "样式", page: 275, children: &[] },
    Cat { label: "自动完成", page: 254, children: &[] },
    Cat { label: "性能", page: 252, children: &[] },
    Cat { label: "打印", page: 247, children: &[] },
];
const ADVANCED_CATS: &[Cat] = &[
    Cat { label: "标签", page: 217, children: &[] },
    Cat { label: "状态栏", page: 296, children: &[] },
    Cat { label: "工具条", page: 207, children: &[] },
    Cat { label: "Web预览", page: 272, children: &[] },
];
const OTHER_CATS: &[Cat] = &[
    Cat { label: "二进制", page: 244, children: &[] },
    Cat { label: "代理", page: 286, children: &[] },
];
const CATEGORIES: &[Cat] = &[
    Cat { label: "常规", page: 219, children: &[] },
    Cat { label: "会话", page: 246, children: &[] },
    Cat { label: "语言", page: 215, children: &[] },
    Cat { label: "编辑", page: 0, children: EDIT_GROUP_CATS },
    Cat { label: "高级", page: 0, children: ADVANCED_CATS },
    Cat { label: "其它", page: 0, children: OTHER_CATS },
];

/// The category tree has its own caption section: a leaf is keyed by the page
/// id it opens, a group by its `P` id in the same section.
fn category_label(cat: &Cat) -> String {
    crate::win32::lang::menu_text(326, cat.page as u32, cat.page == 0, cat.label)
}

// ---------------------------------------------------------------------------
// Property-list pages
// ---------------------------------------------------------------------------

/// The 折叠样式 choices — `[Edit] FoldingStyle`.
const FOLDING_ITEMS: &[&str] = &["简洁", "矩形", "箭头", "矩形和线条", "五边形", "圆形图标"];
/// The 光标样式 choices — `[Edit] CaretStyle`.
const CARET_ITEMS: &[&str] = &["默认", "纤细", "字宽", "DOS样式"];
/// `[Edit] CaretEffect` — the 增强层's five presets.
const CARET_EFFECT_ITEMS: &[&str] = &["关", "呼吸", "呼吸+柔光", "呼吸+柔光+脉冲", "全套(+滑动)"];
/// The 活动行样式 choices — `[Edit] ActiveLineStyle`.
const ACTIVE_LINE_ITEMS: &[&str] = &["背景色", "下划实线", "下划虚线", "上下实线", "上下虚线"];
/// The 标签样式 choices — `[Edit] TabStyle`.
const TAB_STYLE_ITEMS: &[&str] = &["箭头", "线条", "箭头线", "字符"];
/// The 空格样式 choices — `[Edit] SpaceStyle`.
const SPACE_STYLE_ITEMS: &[&str] = &["大点", "小点", "线条"];
/// The 缩略图样式 choices — `[Edit] MiniMapStyle`.
const MINIMAP_ITEMS: &[&str] = &["动态隐藏", "背景色", "边框"];
const LES_WIDTH_ITEMS: &[&str] = &["1", "2"];
/// The 缩进线样式 choices — `[Edit] IndentStyle`.
const INDENT_ITEMS: &[&str] = &["实线", "虚线"];
/// The 书签样式 choices — `[Edit] BookmarkStyle`.
const BOOKMARK_ITEMS: &[&str] = &["默认", "箭头"];
/// The docking-window tab style choices — `[Tab] Style`.
const TAB_STYLE_KIND_ITEMS: &[&str] = &["默认", "平坦", "渐变填充", "位图填充"];

#[derive(Clone, Copy)]
struct RowSpec {
    id: i32,
    kind: RowKind,
    items: &'static [&'static str],
}

const fn check(id: i32) -> RowSpec {
    RowSpec { id, kind: RowKind::Check, items: &[] }
}
const fn edit(id: i32) -> RowSpec {
    RowSpec { id, kind: RowKind::Edit, items: &[] }
}
const fn text(id: i32) -> RowSpec {
    RowSpec { id, kind: RowKind::Text, items: &[] }
}
const fn list(id: i32, items: &'static [&'static str]) -> RowSpec {
    RowSpec { id, kind: RowKind::List, items }
}

/// Row order is the order of the hidden statics in the shipped template.
const ROWS_238: &[RowSpec] = &[
    check(1202),
    check(1203),
    check(1209),
    check(1211),
    check(1210),
    edit(1215),
    text(1216),
    text(1217),
    edit(1218),
    text(1219),
    edit(1220),
];
const ROWS_275: &[RowSpec] = &[
    list(1200, FOLDING_ITEMS),
    list(1201, CARET_ITEMS),
    list(1221, CARET_EFFECT_ITEMS),
    list(1202, ACTIVE_LINE_ITEMS),
    list(1203, TAB_STYLE_ITEMS),
    list(1204, SPACE_STYLE_ITEMS),
    list(1205, MINIMAP_ITEMS),
    list(1208, LES_WIDTH_ITEMS),
    list(1210, INDENT_ITEMS),
    list(1211, BOOKMARK_ITEMS),
    check(1206),
    check(1207),
    check(1209),
];
const ROWS_252: &[RowSpec] = &[edit(1200), edit(1201), edit(1202), edit(1204), edit(1205)];
const ROWS_217: &[RowSpec] = &[
    list(3000, TAB_STYLE_KIND_ITEMS),
    list(3001, TAB_STYLE_KIND_ITEMS),
    RowSpec { id: 3002, kind: RowKind::Color, items: &[] },
    check(1202),
    check(1203),
    check(1200),
    check(1204),
    edit(1216),
    check(1205),
    check(1207),
    check(1208),
    check(1209),
    check(1210),
    check(1211),
    check(1212),
    check(1214),
    check(1215),
    check(1217),
];

fn prop_rows(id: usize) -> &'static [RowSpec] {
    match id {
        238 => ROWS_238,
        275 => ROWS_275,
        252 => ROWS_252,
        217 => ROWS_217,
        _ => &[],
    }
}

fn is_prop_page(id: usize) -> bool {
    id == 219 || !prop_rows(id).is_empty()
}

// ---------------------------------------------------------------------------
// The live object
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Check,
    Edit,
    Text,
    Font,
    List,
    Color,
    /// A file name: shown as text, edited through a file dialog.
    File,
}

struct Row {
    id: i32,
    label: String,
    kind: RowKind,
    check: bool,
    value: usize,
    text: String,
    items: &'static [&'static str],
}

struct Page {
    id: usize,
    hwnd: HWND,
    tree: HWND,
    rows: Vec<Row>,
}

struct EditCtl {
    hwnd: HWND,
    row: usize,
    combo: bool,
}

/// A page that uses its shipped native controls instead of the property list.
struct NativePage {
    id: usize,
    hwnd: HWND,
    _ctx: Box<NativeCtx>,
}

struct NativeCtx {
    shell: *mut Shell,
    id: usize,
    langs: Vec<LangItem>,
    sel: usize,
    sel0: usize,
    /// 工具条 (207): the glyph list both lists draw their rows from, and
    /// its square. Owned here because the rows outlive any one message.
    icons: HIMAGELIST,
    icon_cell: i32,
    templates: Vec<TemplateEntry>,
    temp_types: Vec<String>,
    /// Web预览 (272): `[WebHost]` rows.
    web: Vec<WebEntry>,
    /// Whether a page handler changed the page's data, so 应用 rewrites it.
    dirty: bool,
    /// Filling the template page's fields from the selected row; the edit
    /// controls' EN_CHANGE must not write those half-populated values back.
    syncing: bool,
    field: HBRUSH,
}

#[derive(Clone, Default)]
pub(super) struct TemplateEntry {
    pub(super) title: String,
    pub(super) type_: String,
    pub(super) file: String,
    pub(super) snippet: bool,
}

#[derive(Clone, Default)]
struct WebEntry {
    host: String,
    dir: String,
}

impl Drop for NativeCtx {
    fn drop(&mut self) {
        unsafe {
            if !self.field.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.field.0));
            }
            if self.icons.0 != 0 {
                let _ = ImageList_Destroy(Some(self.icons));
            }
        }
    }
}

struct LangItem {
    stem: String,
    name: String,
    author: String,
    home: String,
}

struct Shell {
    hwnd: HWND,
    frame: HWND,
    theme: Theme,
    dark: bool,
    dpi: i32,
    bg: HBRUSH,
    field: HBRUSH,
    font: HFONT,
    himl: HIMAGELIST,
    tree: HWND,
    host: HWND,
    ini: IniText,
    settings: Settings,
    applied: Option<Settings>,
    prop: Vec<Page>,
    native: Vec<NativePage>,
    page_id: usize,
    editor: Option<EditCtl>,
    /// The category the dialog opens on. A plain open lands on 常规; the
    /// toolbar's 自定义… row asks for 工具条.
    initial_page: usize,
}

impl Drop for Shell {
    fn drop(&mut self) {
        unsafe {
            let _ = ImageList_Destroy(Some(self.himl));
            let _ = DeleteObject(HGDIOBJ(self.bg.0));
            let _ = DeleteObject(HGDIOBJ(self.field.0));
            if !self.font.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
        }
    }
}

impl Shell {
    fn new(frame: HWND, theme: &Theme, dpi: i32) -> Self {
        let ini = IniText::load(&ini_path());
        let settings = Settings::load(&ini);
        let font = unsafe { uifont::create(dpi) };
        chrome::set_chrome_theme(theme);
        Self {
            hwnd: HWND::default(),
            frame,
            theme: theme.clone(),
            dark: chrome::theme_is_dark(theme),
            dpi,
            bg: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
            field: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_field)) },
            font,
            himl: unsafe { property_images(dpi) },
            tree: HWND::default(),
            host: HWND::default(),
            ini,
            settings,
            applied: None,
            prop: Vec::new(),
            native: Vec::new(),
            page_id: 0,
            editor: None,
            initial_page: 219,
        }
    }

    unsafe fn init(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        crate::win32::lang::localize_dialog(hwnd, 250);
        // the template's 8 pt face only decides dialog units.
        if !self.font.is_invalid() {
            if let Ok(first) = GetWindow(hwnd, GW_CHILD) {
                let mut child = first;
                loop {
                    SendMessageW(
                        child,
                        WM_SETFONT,
                        Some(WPARAM(self.font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                    let Ok(next) = GetWindow(child, GW_HWNDNEXT) else {
                        break;
                    };
                    child = next;
                }
            }
        }
        chrome::dark_theme_children(hwnd, self.dark);
        chrome::title_bar(hwnd, self.dark);
        // WS_CLIPCHILDREN keeps the face erase from painting over the page.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, style | WS_CLIPCHILDREN.0 as isize);

        let Ok(tree) = GetDlgItem(Some(hwnd), ID_TREE) else {
            return;
        };
        self.tree = tree;
        if let Ok(host) = GetDlgItem(Some(hwnd), ID_HOST) {
            self.host = host;
        }
        prepare_tree(tree, self.font, self.theme.toolbar_field, self.theme.toolbar_fg);
        // Selection visible without focus — style 0x1835.
        let style = GetWindowLongPtrW(tree, GWL_STYLE) as i32;
        SetWindowLongPtrW(tree, GWL_STYLE, (style | 0x1835) as isize);
        let _ = SetWindowPos(tree, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED);
        let _ = SetWindowSubclass(
            tree,
            Some(category_proc),
            CATEGORY_SUBCLASS,
            hwnd.0 as usize,
        );

        let mut selected = HTREEITEM_DEFAULT;
        for cat in CATEGORIES {
            let parent = insert_item(tree, &category_label(cat), 0, cat.page as isize, None);
            if cat.page == self.initial_page {
                selected = parent;
            }
            let mut any = false;
            for child in cat.children {
                let h = insert_item(
                    tree,
                    &category_label(child),
                    0,
                    child.page as isize,
                    Some(parent),
                );
                if child.page == self.initial_page {
                    selected = h;
                }
                any = true;
            }
            if any {
                SendMessageW(
                    tree,
                    TVM_EXPAND,
                    Some(WPARAM(TVE_EXPAND.0 as usize)),
                    Some(LPARAM(parent.0 as isize)),
                );
            }
        }
        if item_is_set(selected) {
            SendMessageW(
                tree,
                TVM_SELECTITEM,
                Some(WPARAM(TVGN_CARET as usize)),
                Some(LPARAM(selected.0 as isize)),
            );
        }
        self.load_page(self.initial_page);
    }

    /// Show the page named by the selected category.  Pages are created on
    /// first use and cached, so their edits survive a category switch.
    unsafe fn load_page(&mut self, id: usize) {
        // 编辑/高级/其它 are expander rows with no page of their own.
        if !is_prop_page(id) && pages::page_def(id).is_none() {
            return;
        }
        self.close_editor(false);
        for page in &self.prop {
            if page.id != id {
                let _ = ShowWindow(page.hwnd, SW_HIDE);
            }
        }
        for page in &self.native {
            if page.id != id {
                let _ = ShowWindow(page.hwnd, SW_HIDE);
            }
        }
        if is_prop_page(id) {
            if !self.prop.iter().any(|page| page.id == id) {
                self.create_prop_page(id);
            }
            if let Some(page) = self.prop.iter().find(|page| page.id == id) {
                let _ = ShowWindow(page.hwnd, SW_SHOW);
            }
        } else if pages::page_def(id).is_some() {
            if !self.native.iter().any(|page| page.id == id) {
                self.create_native_page(id);
            }
            if let Some(page) = self.native.iter().find(|page| page.id == id) {
                let _ = ShowWindow(page.hwnd, SW_SHOW);
            }
        }
        self.page_id = id;
    }

    unsafe fn create_native_page(&mut self, id: usize) {
        let Some(tmpl) = page_template(id) else {
            return;
        };
        if self.host.is_invalid() {
            return;
        }
        let ctx = Box::new(NativeCtx {
            shell: self as *mut Shell,
            id,
            langs: Vec::new(),
            sel: 0,
            sel0: 0,
            icons: HIMAGELIST(0),
            icon_cell: 0,
            templates: Vec::new(),
            temp_types: Vec::new(),
            web: Vec::new(),
            dirty: false,
            syncing: false,
            field: CreateSolidBrush(to_colorref(self.theme.toolbar_field)),
        });
        let ctx_ptr = &*ctx as *const NativeCtx;
        // The dialog manager reads the template's first fields as DWORDs, so
        // the byte vector is copied into a DWORD-aligned buffer.
        let mut aligned = vec![0u32; (tmpl.len() + 3) / 4];
        std::ptr::copy_nonoverlapping(
            tmpl.as_ptr(),
            aligned.as_mut_ptr() as *mut u8,
            tmpl.len(),
        );
        clip_children(&mut aligned);
        let page = CreateDialogIndirectParamW(
            Some(instance()),
            aligned.as_ptr() as *const DLGTEMPLATE,
            Some(self.host),
            Some(native_proc),
            LPARAM(ctx_ptr as isize),
        );
        match page {
            Ok(page) => {
                self.fit_page(page);
                self.native.push(NativePage { id, hwnd: page, _ctx: ctx });
                let _ = ShowWindow(page, SW_SHOW);
            }
            Err(_) => {}
        }
    }

    /// A page is never larger than the 258x210 host even when its template
    /// is.
    unsafe fn fit_page(&self, page: HWND) {
        let mut rc = RECT::default();
        if GetClientRect(self.host, &mut rc).is_err() {
            return;
        }
        let _ = SetWindowPos(
            page,
            None,
            0,
            0,
            rc.right,
            rc.bottom,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }

    unsafe fn create_prop_page(&mut self, id: usize) {
        if self.host.is_invalid() {
            return;
        }
        let tmpl = if id == 219 {
            build_page_template()
        } else {
            let Some(tmpl) = page_template(id) else {
                return;
            };
            tmpl
        };
        let mut init = PropInit {
            shell: self as *mut Shell,
            id,
            built: None,
        };
        let init_ptr = &mut init as *mut PropInit;
        // The dialog manager reads the template's first fields as DWORDs, so
        // the byte vector is copied into a DWORD-aligned buffer.
        let mut aligned = vec![0u32; (tmpl.len() + 3) / 4];
        std::ptr::copy_nonoverlapping(
            tmpl.as_ptr(),
            aligned.as_mut_ptr() as *mut u8,
            tmpl.len(),
        );
        clip_children(&mut aligned);
        let page = CreateDialogIndirectParamW(
            Some(instance()),
            aligned.as_ptr() as *const DLGTEMPLATE,
            Some(self.host),
            Some(page_proc),
            LPARAM(init_ptr as isize),
        );
        match page {
            Ok(page) => {
                if let Some(built) = init.built.take() {
                    self.fit_page(built.hwnd);
                    self.prop.push(built);
                } else {
                    let _ = DestroyWindow(page);
                }
            }
            Err(_) => {}
        }
    }

    unsafe fn apply(&mut self) {
        self.close_editor(true);
        let mut next = self.settings.clone();
        // They are folded into the same `next` rather than going through `prop_apply`.
        if let Some(page) = self.prop.iter_mut().find(|page| page.id == 219) {
            for (index, row) in page.rows.iter_mut().enumerate() {
                match row.id {
                    ID_ROW_RECENT => {
                        row.value = row.value.min(64);
                        next.recent_doc = row.value as u32;
                        set_item_text(page.tree, index, row);
                    }
                    ID_ROW_START_RECENT => next.start_page_recent = row.check,
                    ID_ROW_MULINST => next.mul_inst = row.check,
                    ID_ROW_SHELL => next.shell_app = row.check,
                    ID_ROW_SHELL_FIND => next.shell_find = row.check,
                    ID_ROW_TEAROFF => next.tear_off = row.check,
                    ID_ROW_SYSBG => next.sys_bg = row.check,
                    ID_ROW_DOCK_THEME => next.dock_theme = row.check,
                    ID_ROW_HIDE_PATH => next.hide_file_path = row.check,
                    ID_ROW_FONT => {
                        // The row keeps the UI-font list's index; the ini keeps
                        // the id.
                        next.ui_font = FONT_IDS[row.value.min(FONT_IDS.len() - 1)]
                    }
                    ID_ROW_UPDATE => next.auto_update = (row.value as u32 + 1).clamp(1, 4),
                    ID_ROW_FLASH => next.flash_taskbar = row.check,
                    ID_ROW_PROGRESS => next.progress = row.check,
                    ID_ROW_SOUND => next.notify_sound = row.check,
                    ID_ROW_SOUND_FILE => next.sound_file = row.text.trim().to_string(),
                    ID_ROW_FAIL_SOUND_FILE => {
                        next.fail_sound_file = row.text.trim().to_string()
                    }
                    _ => {}
                }
            }
        }
        let old = self.settings.clone();
        let mut dirty = false;
        if next.recent_doc != old.recent_doc {
            self.ini
                .set("Setting", "RecentDoc", &next.recent_doc.to_string());
            dirty = true;
        }
        if next.start_page_recent != old.start_page_recent {
            self.ini
                .set("Setting", "StartPageRecent", bool_text(next.start_page_recent));
            dirty = true;
        }
        if next.mul_inst != old.mul_inst {
            self.ini.set("Setting", "MulInst", bool_text(next.mul_inst));
            dirty = true;
        }
        if next.tear_off != old.tear_off {
            self.ini.set("Setting", "TearOff", bool_text(next.tear_off));
            dirty = true;
        }
        if next.auto_update != old.auto_update {
            self.ini
                .set("Setting", "AutoUpdate", &next.auto_update.to_string());
            dirty = true;
        }
        if next.sys_bg != old.sys_bg {
            self.ini.set("Setting", "SysBG", bool_text(next.sys_bg));
            dirty = true;
        }
        if next.hide_file_path != old.hide_file_path {
            self.ini
                .set("Setting", "HideFilePath", bool_text(next.hide_file_path));
            dirty = true;
        }
        if next.ui_font != old.ui_font {
            self.ini
                .set("Setting", "UIFont", &next.ui_font.to_string());
            dirty = true;
        }
        if next.dock_theme != old.dock_theme {
            self.ini
                .set("DockWindow", "Theme", bool_text(next.dock_theme));
            dirty = true;
        }
        if next.flash_taskbar != old.flash_taskbar {
            self.ini
                .set(NOTIFY_SECTION, NOTIFY_FLASH, bool_text(next.flash_taskbar));
            dirty = true;
        }
        if next.progress != old.progress {
            self.ini
                .set(NOTIFY_SECTION, NOTIFY_PROGRESS, bool_text(next.progress));
            dirty = true;
        }
        if next.notify_sound != old.notify_sound {
            self.ini
                .set(NOTIFY_SECTION, NOTIFY_SOUND, bool_text(next.notify_sound));
            dirty = true;
        }
        if next.sound_file != old.sound_file {
            self.ini.set(NOTIFY_SECTION, NOTIFY_SOUND_FILE, &next.sound_file);
            dirty = true;
        }
        if next.fail_sound_file != old.fail_sound_file {
            self.ini.set(
                NOTIFY_SECTION,
                NOTIFY_FAIL_SOUND_FILE,
                &next.fail_sound_file,
            );
            dirty = true;
        }
        let mut prop: Vec<(usize, Vec<RowValue>)> = self
            .prop
            .iter()
            .filter(|page| page.id != 219)
            .map(|page| (page.id, snapshot_rows(page)))
            .collect();
        for (id, rows) in prop.drain(..) {
            if prop_apply(self, id, &rows) {
                dirty = true;
            }
        }
        let pages: Vec<(usize, HWND, *mut NativeCtx)> = self
            .native
            .iter()
            .map(|page| (page.id, page.hwnd, &*page._ctx as *const NativeCtx as *mut NativeCtx))
            .collect();
        for (id, hwnd, ctx) in pages {
            let changed = match id {
                246 => session_apply(self, hwnd),
                215 => language_apply(self, &mut *ctx, hwnd),
                285 => save_apply(self, hwnd),
                286 => proxy_apply(self, hwnd),
                244 => hex_apply(self, hwnd),
                224 => file_apply(self, hwnd),
                247 => print_apply(self, hwnd),
                254 => complete_apply(self, hwnd),
                296 => status_apply(self, hwnd),
                207 => toolbar_apply(self, hwnd),
                213 => {
                    let ctx = &mut *ctx;
                    template_apply(ctx, hwnd)
                }
                272 => {
                    let ctx = &mut *ctx;
                    web_apply(self, ctx, hwnd)
                }
                _ => false,
            };
            if changed {
                dirty = true;
            }
        }
        if dirty {
            self.ini.save(&ini_path());
            unsafe {
                let _ = PostMessageW(Some(self.frame), WM_SETTINGS_APPLIED, WPARAM(0), LPARAM(0));
            }
        }
        if next.shell_app != old.shell_app {
            write_shell_entry(false, next.shell_app);
        }
        if next.shell_find != old.shell_find {
            write_shell_entry(true, next.shell_find);
        }
        self.settings = next.clone();
        self.applied = Some(next);
    }

    unsafe fn close_editor(&mut self, commit: bool) {
        let Some(ed) = self.editor.take() else {
            return;
        };
        if commit {
            let is_text = self
                .prop
                .iter()
                .find(|p| p.id == self.page_id)
                .and_then(|page| page.rows.get(ed.row))
                .is_some_and(|row| row.kind == RowKind::Text);
            let typed = if is_text {
                let len = GetWindowTextLengthW(ed.hwnd);
                let mut buf = vec![0u16; len.max(0) as usize + 1];
                let n = GetWindowTextW(ed.hwnd, &mut buf);
                Some(String::from_utf16_lossy(&buf[..n.max(0) as usize]))
            } else {
                None
            };
            let value = if ed.combo {
                let sel = SendMessageW(ed.hwnd, CB_GETCURSEL, None, None).0 as i32;
                (sel >= 0).then_some(sel as usize)
            } else if is_text {
                None
            } else {
                let len = GetWindowTextLengthW(ed.hwnd);
                let mut buf = vec![0u16; len.max(0) as usize + 1];
                let n = GetWindowTextW(ed.hwnd, &mut buf);
                String::from_utf16_lossy(&buf[..n.max(0) as usize])
                    .trim()
                    .parse::<usize>()
                    .ok()
            };
            if let Some(page) = self.prop.iter_mut().find(|p| p.id == self.page_id) {
                if let Some(row) = page.rows.get_mut(ed.row) {
                    if row.kind == RowKind::Text {
                        if let Some(text) = typed {
                            row.text = text;
                        }
                    } else if let Some(value) = value {
                        row.value = value;
                    }
                    if row.id == ID_ROW_RECENT {
                        row.value = row.value.min(64);
                    }
                    let index = ed.row;
                    set_item_text(page.tree, index, row);
                }
            }
        }
        let tree = self
            .prop
            .iter()
            .find(|p| p.id == self.page_id)
            .map(|p| p.tree)
            .unwrap_or_default();
        let _ = DestroyWindow(ed.hwnd);
        if !tree.is_invalid() {
            let _ = SetFocus(Some(tree));
        }
    }
}

const HTREEITEM_DEFAULT: HTREEITEM = HTREEITEM(0);

use windows::Win32::UI::Controls::HTREEITEM;

fn item_is_set(h: HTREEITEM) -> bool {
    h.0 != 0
}

/// The selected category's page id, from the item's `lParam`.
unsafe fn selected_page_id(tree: HWND) -> Option<usize> {
    let h = SendMessageW(
        tree,
        TVM_GETNEXTITEM,
        Some(WPARAM(TVGN_CARET as usize)),
        None,
    )
    .0;
    if h == 0 {
        return None;
    }
    let mut item = TVITEMW::default();
    item.mask = TVIF_HANDLE | TVIF_PARAM;
    item.hItem = HTREEITEM(h);
    let ok = SendMessageW(
        tree,
        TVM_GETITEMW,
        None,
        Some(LPARAM(&mut item as *mut TVITEMW as isize)),
    );
    if ok.0 == 0 {
        return None;
    }
    Some(item.lParam.0 as usize)
}

unsafe fn prepare_tree(
    tree: HWND,
    font: HFONT,
    bg: dg_render::theme::Color,
    fg: dg_render::theme::Color,
) {
    if !font.is_invalid() {
        SendMessageW(tree, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    }
    SendMessageW(
        tree,
        TVM_SETBKCOLOR,
        None,
        Some(LPARAM(to_colorref(bg).0 as isize)),
    );
    SendMessageW(
        tree,
        TVM_SETTEXTCOLOR,
        None,
        Some(LPARAM(to_colorref(fg).0 as isize)),
    );
}

unsafe fn insert_item(
    tree: HWND,
    label: &str,
    image: i32,
    param: isize,
    parent: Option<HTREEITEM>,
) -> HTREEITEM {
    let mut text: Vec<u16> = label.encode_utf16().chain([0]).collect();
    let mut item = TVITEMW::default();
    item.mask = TVIF_TEXT | TVIF_PARAM | TVIF_IMAGE | TVIF_SELECTEDIMAGE;
    item.pszText = PWSTR(text.as_mut_ptr());
    item.lParam = LPARAM(param);
    item.iImage = image;
    item.iSelectedImage = image;
    let mut ins = TVINSERTSTRUCTW::default();
    ins.hParent = parent.unwrap_or(TVI_ROOT);
    ins.hInsertAfter = TVI_LAST;
    ins.Anonymous = TVINSERTSTRUCTW_0 { item };
    let h = SendMessageW(
        tree,
        TVM_INSERTITEMW,
        Some(WPARAM(0)),
        Some(LPARAM(&mut ins as *mut TVINSERTSTRUCTW as isize)),
    );
    HTREEITEM(h.0)
}

fn row_text(row: &Row) -> String {
    match row.kind {
        RowKind::Check => row.label.clone(),
        RowKind::Edit => format!("{} {}", row.label, row.value),
        RowKind::Text => format!("{} {}", row.label, row.text),
        RowKind::Font | RowKind::List => {
            let item = row.items.get(row.value).copied().unwrap_or_else(|| {
                row.items.first().copied().unwrap_or("")
            });
            format!("{} {}", row.label, crate::win32::lang::trs(item))
        }
        RowKind::Color => format!("{} {}", row.label, color_text(row.value as u32)),
        // An unset sound file is not a missing value: it is the system's own
        // beep, which is what the row says.
        RowKind::File => {
            if row.text.trim().is_empty() {
                format!("{} {}", row.label, crate::win32::lang::trs("系统提示音"))
            } else {
                format!("{} {}", row.label, row.text)
            }
        }
    }
}

fn color_text(value: u32) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        value & 0xFF,
        (value >> 8) & 0xFF,
        (value >> 16) & 0xFF
    )
}

fn row_image(row: &Row) -> i32 {
    match row.kind {
        RowKind::Check => {
            if row.check {
                IMG_CHECKED
            } else {
                IMG_UNCHECKED
            }
        }
        RowKind::Edit | RowKind::Text | RowKind::Color | RowKind::File => IMG_EDIT,
        RowKind::Font | RowKind::List => IMG_LIST,
    }
}

unsafe fn set_item_text(tree: HWND, index: usize, row: &Row) {
    let Some(item) = item_of_row(tree, index) else {
        return;
    };
    let mut text: Vec<u16> = row_text(row).encode_utf16().chain([0]).collect();
    let mut tv = TVITEMW::default();
    tv.mask = TVIF_TEXT;
    tv.hItem = item;
    tv.pszText = PWSTR(text.as_mut_ptr());
    tv.cchTextMax = text.len() as i32;
    SendMessageW(
        tree,
        TVM_SETITEMW,
        None,
        Some(LPARAM(&mut tv as *mut TVITEMW as isize)),
    );
}

unsafe fn set_item_image(tree: HWND, item: HTREEITEM, image: i32) {
    let mut tv = TVITEMW::default();
    tv.mask = TVIF_IMAGE | TVIF_SELECTEDIMAGE;
    tv.hItem = item;
    tv.iImage = image;
    tv.iSelectedImage = image;
    SendMessageW(
        tree,
        TVM_SETITEMW,
        None,
        Some(LPARAM(&mut tv as *mut TVITEMW as isize)),
    );
}

/// The tree item whose `lParam` is the row index.
unsafe fn item_of_row(tree: HWND, index: usize) -> Option<HTREEITEM> {
    let mut h = SendMessageW(tree, TVM_GETNEXTITEM, Some(WPARAM(0)), None).0;
    while h != 0 {
        let item = HTREEITEM(h);
        let mut tv = TVITEMW::default();
        tv.mask = TVIF_HANDLE | TVIF_PARAM;
        tv.hItem = item;
        if SendMessageW(
            tree,
            TVM_GETITEMW,
            None,
            Some(LPARAM(&mut tv as *mut TVITEMW as isize)),
        )
        .0
            != 0
        {
            if tv.lParam.0 == index as isize {
                return Some(item);
            }
        }
        h = SendMessageW(
            tree,
            TVM_GETNEXTITEM,
            Some(WPARAM(1)),
            Some(LPARAM(item.0 as isize)),
        )
        .0;
    }
    None
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

unsafe fn page_this(page: HWND) -> Option<&'static mut Shell> {
    let p = GetWindowLongPtrW(page, GWLP_USERDATA) as *mut Shell;
    if p.is_null() {
        None
    } else {
        Some(&mut *p)
    }
}

/// The value a property row starts with, from `setting.ini`.
struct RowValue {
    id: i32,
    check: bool,
    value: usize,
    text: String,
}

unsafe fn prop_row_state(sh: &Shell, page_id: usize, spec: RowSpec) -> RowValue {
    let ini = &sh.ini;
    let mut check = false;
    let mut value = 0usize;
    let mut text = String::new();
    match page_id {
        238 => match spec.id {
            1202 => check = read_bool(ini, "Edit", "KeepSelFG", false),
            1203 => check = read_bool(ini, "Edit", "SelExtend", false),
            1209 => check = read_bool(ini, "Edit", "KeepHighlight", false),
            1211 => check = read_bool(ini, "Edit", "JumpWithTab", true),
            1210 => check = read_bool(ini, "Edit", "ColOne", false),
            1215 => value = read_u32(ini, "Edit", "ScrollLimit", 0).min(16) as usize,
            1218 => value = read_u32(ini, "Edit", "ClipFormat", 1) as usize,
            1220 => value = read_u32(ini, "Edit", "MaxSelHighlight", 64) as usize,
            1216 => text = ini.get("Edit", "NoHeadChars").unwrap_or_default(),
            1217 => text = ini.get("Edit", "NoTailChars").unwrap_or_default(),
            1219 => text = ini.get("Edit", "FullWidthPair").unwrap_or_default(),
            _ => {}
        },
        275 => match spec.id {
            1206 => check = read_bool(ini, "Edit", "HideVBar", false),
            1207 => check = read_bool(ini, "Edit", "HideHBar", false),
            1209 => check = read_bool(ini, "Edit", "VertTool", false),
            1200 => value = read_u32(ini, "Edit", "FoldingStyle", 4).saturating_sub(1) as usize,
            1201 => value = read_u32(ini, "Edit", "CaretStyle", 1).saturating_sub(1) as usize,
            // `Edit` 的这几行存 1 起的序号，读取时减一；增强层直接存 0..4，
            // 所以只有它不减。
            1221 => value = read_u32(ini, "Edit", "CaretEffect", 0).min(4) as usize,
            1202 => value = read_u32(ini, "Edit", "ActiveLineStyle", 1).saturating_sub(1) as usize,
            1203 => value = read_u32(ini, "Edit", "TabStyle", 4).saturating_sub(1) as usize,
            1204 => value = read_u32(ini, "Edit", "SpaceStyle", 1).saturating_sub(1) as usize,
            1205 => value = read_u32(ini, "Edit", "MiniMapStyle", 1).saturating_sub(1) as usize,
            1208 => value = read_u32(ini, "Edit", "LESWidth", 1).clamp(1, 2) as usize - 1,
            1210 => value = read_u32(ini, "Edit", "IndentStyle", 1).clamp(1, 2) as usize - 1,
            1211 => value = read_u32(ini, "Edit", "BookmarkStyle", 2).saturating_sub(1) as usize,
            _ => {}
        },
        252 => match spec.id {
            1200 => value = read_u32(ini, "Setting", "AutoWrapMax", 50_000) as usize,
            1201 => value = read_u32(ini, "Setting", "BigFileSize", 256) as usize,
            1202 => value = read_u32(ini, "Setting", "AutoDetectBuffer", 4).clamp(4, 10_000) as usize,
            1204 => value = read_u32(ini, "Setting", "FindAllMax", 500).clamp(100, 4000) as usize,
            1205 => value = read_u32(ini, "Edit", "LexMax", 500_000) as usize,
            _ => {}
        },
        217 => match spec.id {
            3000 => value = read_u32(ini, "Tab", "Style", 1).clamp(1, 4) as usize - 1,
            3001 => value = read_u32(ini, "Tab", "DockStyle", 1).clamp(1, 4) as usize - 1,
            3002 => value = read_u32(ini, "Tab", "ActiveBG", 0x00EC_ECD9) as usize,
            1202 => check = read_bool(ini, "Tab", "ShowIcon", true),
            1203 => check = read_bool(ini, "Tab", "ShowMark", true),
            1200 => check = read_bool(ini, "Tab", "Bottom", false),
            1204 => check = read_bool(ini, "Tab", "FixedWidth", false),
            1216 => value = read_u32(ini, "Tab", "FixedWidthVal", 250).clamp(50, 300) as usize,
            1205 => check = read_bool(ini, "Tab", "HideOnlyOne", true),
            1207 => check = read_bool(ini, "Tab", "Scroll", true),
            1208 => check = read_bool(ini, "Tab", "FileList", true),
            1209 => check = read_bool(ini, "Tab", "GlobalClose", false),
            1210 => check = read_bool(ini, "Tab", "ItemClose", true),
            1211 => check = read_bool(ini, "Tab", "DbclkClose", true),
            1212 => check = read_bool(ini, "Tab", "DynamicTitle", false),
            1214 => check = read_bool(ini, "Tab", "Exit", false),
            1215 => check = read_bool(ini, "Tab", "TabRight", false),
            1217 => check = read_bool(ini, "Tab", "LeftFirst", false),
            _ => {}
        },
        219 => match spec.id {
            ID_ROW_START_RECENT => check = sh.settings.start_page_recent,
            ID_ROW_MULINST => check = sh.settings.mul_inst,
            ID_ROW_SHELL => check = sh.settings.shell_app,
            ID_ROW_SHELL_FIND => check = sh.settings.shell_find,
            ID_ROW_TEAROFF => check = sh.settings.tear_off,
            ID_ROW_SYSBG => check = sh.settings.sys_bg,
            ID_ROW_DOCK_THEME => check = sh.settings.dock_theme,
            ID_ROW_HIDE_PATH => check = sh.settings.hide_file_path,
            ID_ROW_RECENT => value = sh.settings.recent_doc as usize,
            ID_ROW_FONT => value = font_index(sh.settings.ui_font),
            ID_ROW_UPDATE => value = (sh.settings.auto_update - 1) as usize,
            ID_ROW_FLASH => check = sh.settings.flash_taskbar,
            ID_ROW_PROGRESS => check = sh.settings.progress,
            ID_ROW_SOUND => check = sh.settings.notify_sound,
            ID_ROW_SOUND_FILE => text = sh.settings.sound_file.clone(),
            ID_ROW_FAIL_SOUND_FILE => text = sh.settings.fail_sound_file.clone(),
            _ => {}
        },
        _ => {}
    }
    if spec.kind == RowKind::List && !spec.items.is_empty() {
        value = value.min(spec.items.len() - 1);
    }
    RowValue { id: spec.id, check, value, text }
}

fn general_row_kind(id: i32) -> RowKind {
    match id {
        ID_ROW_RECENT => RowKind::Edit,
        ID_ROW_FONT => RowKind::Font,
        ID_ROW_UPDATE => RowKind::List,
        ID_ROW_SOUND_FILE | ID_ROW_FAIL_SOUND_FILE => RowKind::File,
        _ => RowKind::Check,
    }
}

unsafe fn build_prop_rows(sh: &mut Shell, page: HWND, page_id: usize) -> Option<Page> {
    let Ok(tree) = GetDlgItem(Some(page), ID_PAGE_TREE) else {
        return None;
    };
    prepare_tree(tree, sh.font, sh.theme.toolbar_field, sh.theme.toolbar_fg);
    // the options list builder's style surgery: clear FULLROWSELECT and the native
    // checkbox style (the sprite strip is the checkbox), keep selection
    // visible while the tree does not have the focus, and stop the horizontal
    // scroll bar.
    let style = GetWindowLongPtrW(tree, GWL_STYLE) as u32;
    let style = (style & !(TVS_FULLROWSELECT | 0x100)) | TVS_SHOWSELALWAYS | TVS_NOSCROLL;
    SetWindowLongPtrW(tree, GWL_STYLE, style as isize);
    let _ = SetWindowPos(tree, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED);
    SendMessageW(
        tree,
        TVM_SETIMAGELIST,
        Some(WPARAM(0)),
        Some(LPARAM(sh.himl.0 as isize)),
    );
    let _ = SetWindowSubclass(
        tree,
        Some(proptree_proc),
        PROPTREE_SUBCLASS,
        sh as *mut Shell as usize,
    );

    let general: Vec<RowSpec>;
    let specs: &[RowSpec] = if page_id == 219 {
        general = PAGE_ROWS
            .iter()
            .map(|(id, _)| RowSpec {
                id: *id,
                kind: general_row_kind(*id),
                items: match *id {
                    ID_ROW_FONT => FONT_ITEMS,
                    ID_ROW_UPDATE => UPDATE_ITEMS,
                    _ => &[],
                },
            })
            .collect::<Vec<_>>();
        &general
    } else {
        prop_rows(page_id)
    };

    let mut rows = Vec::with_capacity(specs.len());
    for spec in specs {
        // `page_texts` and `PAGE_ROWS` stay as the Chinese fallback for
        // controls the table does not name.
        let label = crate::win32::lang::dlg(page_id as u32, spec.id as u32)
            .map(str::to_string)
            .or_else(|| {
                page_texts(page_id)
                    .iter()
                    .find(|(id, _)| *id == spec.id)
                    .map(|(_, s)| (*s).to_string())
            })
            .or_else(|| {
                PAGE_ROWS
                    .iter()
                    .find(|(id, _)| *id == spec.id)
                    .map(|(_, s)| (*s).to_string())
            })
            .unwrap_or_else(|| ctrl_text(page, spec.id));
        let state = prop_row_state(sh, page_id, *spec);
        rows.push(Row {
            id: spec.id,
            label,
            kind: spec.kind,
            check: state.check,
            value: state.value,
            text: state.text,
            items: spec.items,
        });
    }

    for (index, row) in rows.iter().enumerate() {
        let image = row_image(row);
        let mut text: Vec<u16> = row_text(row).encode_utf16().chain([0]).collect();
        let mut item = TVITEMW::default();
        item.mask = TVIF_TEXT | TVIF_PARAM | TVIF_IMAGE | TVIF_SELECTEDIMAGE;
        item.pszText = PWSTR(text.as_mut_ptr());
        item.lParam = LPARAM(index as isize);
        item.iImage = image;
        item.iSelectedImage = image;
        let mut ins = TVINSERTSTRUCTW::default();
        ins.hParent = TVI_ROOT;
        ins.hInsertAfter = TVI_LAST;
        ins.Anonymous = TVINSERTSTRUCTW_0 { item };
        SendMessageW(
            tree,
            TVM_INSERTITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(&mut ins as *mut TVINSERTSTRUCTW as isize)),
        );
    }
    Some(Page { id: page_id, hwnd: page, tree, rows })
}

fn snapshot_rows(page: &Page) -> Vec<RowValue> {
    page.rows
        .iter()
        .map(|row| RowValue {
            id: row.id,
            check: row.check,
            value: row.value,
            text: row.text.clone(),
        })
        .collect()
}

fn set_ini_bool(sh: &mut Shell, dirty: &mut bool, section: &str, key: &str, value: bool, default: bool) {
    if read_bool(&sh.ini, section, key, default) != value {
        sh.ini.set(section, key, bool_text(value));
        *dirty = true;
    }
}

fn set_ini_u32(sh: &mut Shell, dirty: &mut bool, section: &str, key: &str, value: u32, default: u32) {
    if read_u32(&sh.ini, section, key, default) != value {
        sh.ini.set(section, key, &value.to_string());
        *dirty = true;
    }
}

fn set_ini_str(sh: &mut Shell, dirty: &mut bool, section: &str, key: &str, value: &str) {
    if sh.ini.get(section, key).unwrap_or_default() != value {
        sh.ini.set(section, key, value);
        *dirty = true;
    }
}

/// Read the page's rows back into `setting.ini`, writing only changed keys.
unsafe fn prop_apply(sh: &mut Shell, page_id: usize, rows: &[RowValue]) -> bool {
    let mut dirty = false;
    let row = |id: i32| rows.iter().find(|row| row.id == id);
    match page_id {
        238 => {
            for (id, key, default) in [
                (1202, "KeepSelFG", false),
                (1203, "SelExtend", false),
                (1209, "KeepHighlight", false),
                (1211, "JumpWithTab", true),
                (1210, "ColOne", false),
            ] {
                if let Some(row) = row(id) {
                    set_ini_bool(sh, &mut dirty, "Edit", key, row.check, default);
                }
            }
            if let Some(row) = row(1215) {
                set_ini_u32(sh, &mut dirty, "Edit", "ScrollLimit", row.value.min(16) as u32, 0);
            }
            if let Some(row) = row(1218) {
                set_ini_u32(sh, &mut dirty, "Edit", "ClipFormat", row.value as u32, 1);
            }
            if let Some(row) = row(1220) {
                set_ini_u32(sh, &mut dirty, "Edit", "MaxSelHighlight", row.value as u32, 64);
            }
            if let Some(row) = row(1216) {
                set_ini_str(sh, &mut dirty, "Edit", "NoHeadChars", &row.text);
            }
            if let Some(row) = row(1217) {
                set_ini_str(sh, &mut dirty, "Edit", "NoTailChars", &row.text);
            }
            if let Some(row) = row(1219) {
                let pair: String = row.text.chars().filter(|c| *c != ' ').collect();
                set_ini_str(sh, &mut dirty, "Edit", "FullWidthPair", &pair);
            }
        }
        275 => {
            for (id, key, default, max) in [
                (1200, "FoldingStyle", 4u32, FOLDING_ITEMS.len()),
                (1201, "CaretStyle", 1, CARET_ITEMS.len()),
                (1202, "ActiveLineStyle", 1, ACTIVE_LINE_ITEMS.len()),
                (1203, "TabStyle", 1, TAB_STYLE_ITEMS.len()),
                (1204, "SpaceStyle", 1, SPACE_STYLE_ITEMS.len()),
                (1205, "MiniMapStyle", 1, MINIMAP_ITEMS.len()),
                (1208, "LESWidth", 2, 2),
                (1210, "IndentStyle", 1, 2),
                (1211, "BookmarkStyle", 2, BOOKMARK_ITEMS.len()),
            ] {
                if let Some(row) = row(id) {
                    let value = (row.value.min(max - 1) + 1) as u32;
                    set_ini_u32(sh, &mut dirty, "Edit", key, value, default);
                }
            }
            if let Some(row) = row(1221) {
                set_ini_u32(sh, &mut dirty, "Edit", "CaretEffect", row.value as u32, 0);
            }
            for (id, key) in [(1206, "HideVBar"), (1207, "HideHBar"), (1209, "VertTool")] {
                if let Some(row) = row(id) {
                    set_ini_bool(sh, &mut dirty, "Edit", key, row.check, false);
                }
            }
        }
        252 => {
            if let Some(row) = row(1200) {
                set_ini_u32(sh, &mut dirty, "Setting", "AutoWrapMax", row.value as u32, 50_000);
            }
            if let Some(row) = row(1201) {
                set_ini_u32(sh, &mut dirty, "Setting", "BigFileSize", row.value as u32, 256);
            }
            if let Some(row) = row(1202) {
                let value = (row.value as u32).clamp(4, 10_000);
                set_ini_u32(sh, &mut dirty, "Setting", "AutoDetectBuffer", value, 4);
            }
            if let Some(row) = row(1204) {
                let value = (row.value as u32).clamp(100, 4000);
                set_ini_u32(sh, &mut dirty, "Setting", "FindAllMax", value, 500);
            }
            if let Some(row) = row(1205) {
                set_ini_u32(sh, &mut dirty, "Edit", "LexMax", row.value as u32, 500_000);
            }
        }
        217 => {
            if let Some(row) = row(3000) {
                let value = (row.value.min(TAB_STYLE_KIND_ITEMS.len() - 1) + 1) as u32;
                set_ini_u32(sh, &mut dirty, "Tab", "Style", value, 1);
            }
            if let Some(row) = row(3002) {
                set_ini_u32(
                    sh,
                    &mut dirty,
                    "Tab",
                    "ActiveBG",
                    row.value as u32,
                    0x00EC_ECD9,
                );
            }
            for (id, key, default) in [
                (1202, "ShowIcon", true),
                (1203, "ShowMark", true),
                (1200, "Bottom", false),
                (1204, "FixedWidth", false),
                (1205, "HideOnlyOne", true),
                (1207, "Scroll", true),
                (1208, "FileList", true),
                (1209, "GlobalClose", false),
                (1210, "ItemClose", true),
                (1211, "DbclkClose", true),
                (1212, "DynamicTitle", false),
                (1214, "Exit", false),
                (1215, "TabRight", false),
                (1217, "LeftFirst", false),
            ] {
                if let Some(row) = row(id) {
                    set_ini_bool(sh, &mut dirty, "Tab", key, row.check, default);
                }
            }
            if let Some(row) = row(1216) {
                let value = (row.value as u32).clamp(50, 300);
                set_ini_u32(sh, &mut dirty, "Tab", "FixedWidthVal", value, 250);
            }
        }
        _ => {}
    }
    dirty
}

unsafe fn ctrl_text(parent: HWND, id: i32) -> String {
    let Ok(h) = GetDlgItem(Some(parent), id) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(h);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(h, &mut buf);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe fn activate_row(sh: &mut Shell, index: usize) {
    let page_id = sh.page_id;
    let kind = {
        let Some(page) = sh.prop.iter().find(|page| page.id == page_id) else {
            return;
        };
        let Some(row) = page.rows.get(index) else {
            return;
        };
        row.kind
    };
    match kind {
        RowKind::Check => {
            let Some(page) = sh.prop.iter_mut().find(|page| page.id == page_id) else {
                return;
            };
            page.rows[index].check = !page.rows[index].check;
            let image = row_image(&page.rows[index]);
            if let Some(item) = item_of_row(page.tree, index) {
                set_item_image(page.tree, item, image);
            }
        }
        RowKind::Color => {
            let Some(page) = sh.prop.iter_mut().find(|page| page.id == page_id) else {
                return;
            };
            let mut custom = [COLORREF(0); 16];
            let mut cc = CHOOSECOLORW {
                lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
                hwndOwner: page.hwnd,
                rgbResult: COLORREF(page.rows[index].value as u32),
                lpCustColors: custom.as_mut_ptr(),
                Flags: CC_RGBINIT | CC_FULLOPEN,
                ..Default::default()
            };
            if ChooseColorW(&mut cc).as_bool() {
                page.rows[index].value = cc.rgbResult.0 as usize;
                set_item_text(page.tree, index, &page.rows[index]);
            }
        }
        RowKind::File => {
            let Some(page) = sh.prop.iter_mut().find(|page| page.id == page_id) else {
                return;
            };
            let current = page.rows[index].text.clone();
            // `PlaySoundW` reads WAV; the filter leads with it and keeps 所有文件
            // for the containers it also understands.
            if let Some(path) = browse_file(
                page.hwnd,
                None,
                &current,
                "声音文件\0*.wav\0所有文件\0*.*\0\0",
            ) {
                page.rows[index].text = path;
                set_item_text(page.tree, index, &page.rows[index]);
            }
        }
        _ => open_editor(sh, index),
    }
}

unsafe fn selected_row(tree: HWND) -> Option<usize> {
    let h = SendMessageW(
        tree,
        TVM_GETNEXTITEM,
        Some(WPARAM(TVGN_CARET as usize)),
        None,
    )
    .0;
    if h == 0 {
        return None;
    }
    let mut item = TVITEMW::default();
    item.mask = TVIF_HANDLE | TVIF_PARAM;
    item.hItem = HTREEITEM(h);
    if SendMessageW(
        tree,
        TVM_GETITEMW,
        None,
        Some(LPARAM(&mut item as *mut TVITEMW as isize)),
    )
    .0
        == 0
    {
        return None;
    }
    Some(item.lParam.0 as usize)
}

/// the options list builder's row activation: the checkbox toggles in place, a value
/// row opens its editor over the value's pixels.
unsafe fn open_editor(sh: &mut Shell, index: usize) {
    sh.close_editor(false);
    let page_id = sh.page_id;
    let Some((page_hwnd, tree, kind, label, value, initial, items)) =
        sh.prop.iter().find(|p| p.id == page_id).and_then(|page| {
            let row = page.rows.get(index)?;
            if row.kind == RowKind::Check || row.kind == RowKind::Color {
                return None;
            }
            Some((
                page.hwnd,
                page.tree,
                row.kind,
                row.label.clone(),
                row.value,
                row.text.clone(),
                row.items,
            ))
        })
    else {
        return;
    };
    let Some(item) = item_of_row(tree, index) else {
        return;
    };
    // Handing the handle in `wParam` instead — the form the SDK macro writes — is rejected
    // on this control and leaves the rect at zero, which put every editor at the top of the
    // page.
    let mut rc = RECT::default();
    rc.left = (item.0 as u64 & 0xFFFF_FFFF) as i32;
    rc.top = ((item.0 as u64 >> 32) & 0xFFFF_FFFF) as i32;
    SendMessageW(
        tree,
        TVM_GETITEMRECT,
        Some(WPARAM(0)),
        Some(LPARAM(&mut rc as *mut RECT as isize)),
    );
    // The rect is the list's client space and the editor is the page's child.
    let mut corner = [POINT { x: rc.left, y: rc.top }];
    let _ = MapWindowPoints(Some(tree), Some(page_hwnd), &mut corner[..]);
    let icon = if sh.dpi >= 144 { 24 } else { 16 };
    let pad = scaled(4, sh.dpi);
    let label_w = measure_text(tree, &label, sh.font);
    let x = corner[0].x + icon + pad + label_w + scaled(6, sh.dpi);
    let y = corner[0].y;
    let h = (rc.bottom - rc.top).max(scaled(16, sh.dpi));
    let combo = matches!(kind, RowKind::Font | RowKind::List);
    let (w, ch) = if combo {
        (scaled(160, sh.dpi), scaled(220, sh.dpi))
    } else {
        (scaled(90, sh.dpi), h)
    };
    let (class, style) = if combo {
        (
            w!("ComboBox"),
            WINDOW_STYLE(
                (WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL).0 | CBS_DROPDOWNLIST as u32,
            ),
        )
    } else {
        (
            w!("EDIT"),
            WINDOW_STYLE(
                (WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER).0 | ES_AUTOHSCROLL as u32,
            ),
        )
    };
    let Ok(ctl) = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        w!(""),
        style,
        x,
        y,
        w,
        ch,
        Some(page_hwnd),
        Some(HMENU(ID_EDITOR as isize as *mut core::ffi::c_void)),
        Some(instance()),
        None,
    ) else {
        return;
    };
    chrome::dark_theme(ctl, sh.dark);
    if combo {
        let items: &[&str] = match kind {
            RowKind::Font => FONT_ITEMS,
            _ => items,
        };
        for text in items {
            let text = crate::win32::lang::trs(text);
            let mut buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
            SendMessageW(
                ctl,
                CB_ADDSTRING,
                None,
                Some(LPARAM(buf.as_mut_ptr() as isize)),
            );
        }
        SendMessageW(ctl, CB_SETCURSEL, Some(WPARAM(value)), None);
        SendMessageW(ctl, CB_SETDROPPEDWIDTH, Some(WPARAM(w as usize)), None);
    } else {
        let display = if kind == RowKind::Text {
            initial
        } else {
            value.to_string()
        };
        let mut text: Vec<u16> = display.encode_utf16().chain([0]).collect();
        let _ = SetWindowTextW(ctl, PCWSTR(text.as_mut_ptr()));
    }
    if !sh.font.is_invalid() {
        SendMessageW(ctl, WM_SETFONT, Some(WPARAM(sh.font.0 as usize)), Some(LPARAM(1)));
    }
    let _ = SetWindowSubclass(ctl, Some(editor_subclass), 1, combo as usize);
    let _ = SetFocus(Some(ctl));
    if combo {
        SendMessageW(ctl, CB_SHOWDROPDOWN, Some(WPARAM(1)), None);
    }
    sh.editor = Some(EditCtl { hwnd: ctl, row: index, combo });
}

unsafe fn measure_text(tree: HWND, text: &str, font: HFONT) -> i32 {
    let hdc = GetDC(Some(tree));
    let old = if !font.is_invalid() {
        SelectObject(hdc, HGDIOBJ(font.0))
    } else {
        HGDIOBJ::default()
    };
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &wide, &mut size);
    if !old.is_invalid() {
        SelectObject(hdc, old);
    }
    ReleaseDC(Some(tree), hdc);
    size.cx
}

unsafe fn row_from_tree(sh: &Shell, tree: HWND, pt: POINT) -> Option<usize> {
    let page = sh.prop.iter().find(|p| p.tree == tree)?;
    let mut hit = TVHITTESTINFO::default();
    hit.pt = pt;
    SendMessageW(
        page.tree,
        TVM_HITTEST,
        None,
        Some(LPARAM(&mut hit as *mut TVHITTESTINFO as isize)),
    );
    // A click lands on exactly one part of the row — the sprite, the label or
    // the state icon — so the test is whether any `TVHT_ONITEM` bit is set, not
    // whether all of them are (`contains` asks for the whole mask).
    if (hit.flags & TVHT_ONITEM).0 == 0 {
        return None;
    }
    // A click on an unselected row both selects and opens it in one go.
    if SendMessageW(
        page.tree,
        TVM_GETNEXTITEM,
        Some(WPARAM(TVGN_CARET as usize)),
        None,
    )
    .0
        != hit.hItem.0
    {
        SendMessageW(
            page.tree,
            TVM_SELECTITEM,
            Some(WPARAM(TVGN_CARET as usize)),
            Some(LPARAM(hit.hItem.0 as isize)),
        );
    }
    let mut item = TVITEMW::default();
    item.mask = TVIF_HANDLE | TVIF_PARAM;
    item.hItem = hit.hItem;
    if SendMessageW(
        page.tree,
        TVM_GETITEMW,
        None,
        Some(LPARAM(&mut item as *mut TVITEMW as isize)),
    )
    .0
        == 0
    {
        return None;
    }
    Some(item.lParam.0 as usize)
}

/// The row is resolved from the notification's own message position — the tree's
/// `NM_CLICK` carries no point — because the button-up itself never reaches this control:
/// the list is inside a modal dialog, and the tree consumes the up in its own capture
/// handling. That is why activation hangs off the notification and not off
/// `WM_LBUTTONUP`.
unsafe fn activate_clicked_row(sh: &Shell, tree: HWND) -> Option<usize> {
    let pos = GetMessagePos();
    let pt = POINT {
        x: pos as u16 as i16 as i32,
        y: ((pos >> 16) as u16 as i16) as i32,
    };
    let mut client = pt;
    let _ = ScreenToClient(tree, &mut client);
    row_from_tree(sh, tree, client)
}

/// Hand the row under the click's position to the page that owns the list.
unsafe fn post_row(tree: HWND, shell: usize) {
    let Some(sh) = (shell as *mut Shell).as_mut() else {
        return;
    };
    let Some(index) = activate_clicked_row(sh, tree) else {
        return;
    };
    let Some(page) = sh.prop.iter().find(|p| p.tree == tree).map(|p| p.hwnd) else {
        return;
    };
    let _ = PostMessageW(Some(page), WM_ROW_ACTIVATE, WPARAM(index), LPARAM(0));
}

/// the options list builder's subclass of the property list. A key activates the selected
/// row; the mouse is reported by the list's `NM_CLICK` notification, which the
/// page resolves against the message position.
unsafe extern "system" fn proptree_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    refdata: usize,
) -> LRESULT {
    let r = DefSubclassProc(hwnd, msg, wp, lp);
    match msg {
        WM_KEYDOWN => {
            let vk = wp.0 as u32;
            if vk == VK_SPACE.0 as u32 || vk == VK_RETURN.0 as u32 {
                if let Some(index) = selected_row(hwnd) {
                    if let Some(sh) = (refdata as *mut Shell).as_mut() {
                        if let Some(page) =
                            sh.prop.iter().find(|p| p.tree == hwnd).map(|p| p.hwnd)
                        {
                            let _ = PostMessageW(Some(page), WM_ROW_ACTIVATE, WPARAM(index), LPARAM(0));
                        }
                    }
                }
            }
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(proptree_proc), PROPTREE_SUBCLASS);
        }
        _ => {}
    }
    r
}

/// The category tree's half of the page switch: the click is reported after the
/// tree has finished with it, so no page window is created inside it.
unsafe extern "system" fn category_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    refdata: usize,
) -> LRESULT {
    let r = DefSubclassProc(hwnd, msg, wp, lp);
    match msg {
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let shell = HWND(refdata as *mut core::ffi::c_void);
            let _ = PostMessageW(Some(shell), WM_PAGE_READY, WPARAM(0), LPARAM(0));
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(category_proc), CATEGORY_SUBCLASS);
        }
        _ => {}
    }
    r
}

struct PropInit {
    shell: *mut Shell,
    id: usize,
    built: Option<Page>,
}

/// The page's background erase fills its whole client, so it must not paint
/// over its own controls: DS_ templates carry no WS_CLIPCHILDREN, add it to
/// the copy before the dialog manager reads the style.
pub(super) fn clip_children(tmpl: &mut [u32]) {
    let style = if tmpl[0] == 0xffff_0001 { 3 } else { 0 };
    tmpl[style] |= WS_CLIPCHILDREN.0;
}

unsafe extern "system" fn page_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            let init = &mut *(lp.0 as *mut PropInit);
            let sh = &mut *init.shell;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, init.shell as isize);
            // The page template's 8 pt font only decides dialog units; the
            // tree and its sibling statics draw with the UI font like every
            // other page. `prepare_tree` sets the tree again below.
            set_font_deep(hwnd, sh.font);
            if let Some(page) = build_prop_rows(sh, hwnd, init.id) {
                init.built = Some(page);
                // The page's own controls ask for the dark parts; the page
                // dialog itself is never named (that doubles a #32770 frame).
                chrome::dark_theme_children(hwnd, sh.dark);
                1
            } else {
                0
            }
        }
        WM_NOTIFY => {
            // The row's editor is still deferred to `WM_ROW_ACTIVATE` so nothing is
            // created while the list owns the message.
            let nm = &*(lp.0 as *const NMHDR);
            if nm.code == NM_CLICK {
                if let Some(sh) = page_this(hwnd) {
                    post_row(nm.hwndFrom, sh as *mut Shell as usize);
                }
            }
            0
        }
        WM_ROW_ACTIVATE => {
            let Some(sh) = page_this(hwnd) else {
                return 0;
            };
            activate_row(sh, wp.0);
            1
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as i32;
            let code = ((wp.0 >> 16) & 0xFFFF) as u32;
            if id == ID_EDITOR {
                let Some(sh) = page_this(hwnd) else {
                    return 0;
                };
                match code {
                    x if x == CBN_SELCHANGE => sh.close_editor(true),
                    x if x == CBN_SELENDCANCEL => sh.close_editor(false),
                    x if x == CBN_KILLFOCUS || x == EN_KILLFOCUS => sh.close_editor(true),
                    _ => {}
                }
                return 1;
            }
            0
        }
        WM_ERASEBKGND => {
            let Some(sh) = page_this(hwnd) else {
                return 0;
            };
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let _ = FillRect(HDC(wp.0 as *mut core::ffi::c_void), &rc, sh.bg);
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let Some(sh) = page_this(hwnd) else {
                return 0;
            };
            customdraw::ctlcolor(
                HDC(wp.0 as *mut core::ffi::c_void),
                to_colorref(sh.theme.toolbar_fg),
                to_colorref(sh.theme.toolbar_bg),
                sh.bg,
            )
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let Some(sh) = page_this(hwnd) else {
                return 0;
            };
            customdraw::ctlcolor(
                HDC(wp.0 as *mut core::ffi::c_void),
                to_colorref(sh.theme.toolbar_fg),
                to_colorref(sh.theme.toolbar_field),
                sh.field,
            )
        }
        _ => 0,
    }
}

unsafe extern "system" fn editor_subclass(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    refdata: usize,
) -> LRESULT {
    let combo = refdata != 0;
    if msg == WM_KEYDOWN {
        let vk = wp.0 as u32;
        if let Ok(page) = GetParent(hwnd) {
            let Some(sh) = page_this(page) else {
                return DefSubclassProc(hwnd, msg, wp, lp);
            };
            let mine = sh.editor.as_ref().is_some_and(|e| e.hwnd == hwnd);
            if mine && vk == VK_ESCAPE.0 as u32 {
                if combo
                    && SendMessageW(hwnd, CB_GETDROPPEDSTATE, None, None).0 != 0
                {
                    // Let the combo close its own list; CBN_SELENDCANCEL
                    // reaches the page and cancels the editor there.
                } else {
                    sh.close_editor(false);
                    return LRESULT(0);
                }
            } else if mine && !combo && vk == VK_RETURN.0 as u32 {
                sh.close_editor(true);
                return LRESULT(0);
            }
        }
    }
    if msg == WM_KILLFOCUS {
        if let Ok(page) = GetParent(hwnd) {
            if let Some(sh) = page_this(page) {
                if sh.editor.as_ref().is_some_and(|e| e.hwnd == hwnd) {
                    sh.close_editor(true);
                }
            }
        }
    }
    if msg == WM_NCDESTROY {
        let _ = RemoveWindowSubclass(hwnd, Some(editor_subclass), 1);
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

// ---------------------------------------------------------------------------
// Native pages (会话 / 语言 /...)
// ---------------------------------------------------------------------------

unsafe fn set_font_deep(hwnd: HWND, font: HFONT) {    if font.is_invalid() {
        return;
    }
    let Ok(mut child) = GetWindow(hwnd, GW_CHILD) else {
        return;
    };
    loop {
        SendMessageW(
            child,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
        set_font_deep(child, font);
        let Ok(next) = GetWindow(child, GW_HWNDNEXT) else {
            break;
        };
        child = next;
    }
}

/// A themed `Edit` paints its own light field and ignores the `WM_CTLCOLOR`
/// brush, so it is un-named and handed back to the dark brush. A stock
/// `ListBox` needs the same row colours but must keep its theme name —
/// `DarkMode_Explorer` is what gives it a dark scrollbar — so its client rows
/// are taken over by `chrome::paint_stock_listbox` instead. An ownerdraw list
/// already paints its own rows and keeps its theme untouched.
unsafe fn listbox_is_ownerdraw(hwnd: HWND) -> bool {
    (GetWindowLongPtrW(hwnd, GWL_STYLE) as u32) & (0x0010 | 0x0020) != 0
}

unsafe fn plain_field(hwnd: HWND) {
    let Ok(mut child) = GetWindow(hwnd, GW_CHILD) else {
        return;
    };
    loop {
        let mut cls = [0u16; 32];
        let n = GetClassNameW(child, &mut cls);
        let name = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
        if name == "Edit" {
            let _ = SetWindowTheme(child, w!(" "), w!(" "));
            let _ = InvalidateRect(Some(child), None, true);
        } else if name == "ListBox" && !listbox_is_ownerdraw(child) {
            chrome::paint_stock_listbox(child);
        }
        plain_field(child);
        let Ok(next) = GetWindow(child, GW_HWNDNEXT) else {
            break;
        };
        child = next;
    }
}

/// Writes the 2052 captions onto a page's controls.  Entry 0 of a `[DLGn]`
/// section is the dialog's own title, which `GetDlgItem` cannot reach, so it
/// goes straight to the window.
unsafe fn localize_page(hwnd: HWND, id: usize) {
    for &(cid, text) in page_texts(id) {
        let text = crate::win32::lang::dlg(id as u32, cid as u32)
            .unwrap_or(text)
            .to_string();
        let ctl = if cid == 0 {
            hwnd
        } else {
            match GetDlgItem(Some(hwnd), cid) {
                Ok(ctl) => ctl,
                Err(_) => continue,
            }
        };
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let _ = SetWindowTextW(ctl, PCWSTR(wide.as_ptr()));
    }
}

unsafe fn ctl_text(hwnd: HWND, id: i32) -> String {
    let Ok(ctl) = GetDlgItem(Some(hwnd), id) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(ctl);
    let mut buf = vec![0u16; len.max(0) as usize + 1];
    let n = GetWindowTextW(ctl, &mut buf);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe fn set_ctl_text(hwnd: HWND, id: i32, text: &str) {
    if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let _ = SetWindowTextW(ctl, PCWSTR(wide.as_ptr()));
    }
}

unsafe fn set_check(hwnd: HWND, id: i32, on: bool) {
    if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
        let state = if on { BST_CHECKED } else { BST_UNCHECKED };
        SendMessageW(ctl, BM_SETCHECK, Some(WPARAM(state.0 as usize)), None);
    }
}

unsafe fn get_check(hwnd: HWND, id: i32) -> bool {
    let Ok(ctl) = GetDlgItem(Some(hwnd), id) else {
        return false;
    };
    SendMessageW(ctl, BM_GETCHECK, None, None).0 != 0
}

unsafe fn browse_folder(hwnd: HWND) -> Option<String> {
    let mut display = [0u16; 260];
    let title: Vec<u16> = "选择文件夹\0".encode_utf16().collect();
    let bi = BROWSEINFOW {
        hwndOwner: hwnd,
        pidlRoot: std::ptr::null_mut(),
        pszDisplayName: PWSTR(display.as_mut_ptr()),
        lpszTitle: PCWSTR(title.as_ptr()),
        ulFlags: BIF_RETURNONLYFSDIRS,
        ..Default::default()
    };
    let pidl = SHBrowseForFolderW(&bi);
    if pidl.is_null() {
        return None;
    }
    let mut path = [0u16; 260];
    let ok = SHGetPathFromIDListW(pidl, &mut path).as_bool();
    CoTaskMemFree(Some(pidl as *const core::ffi::c_void));
    if !ok {
        return None;
    }
    let len = path.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&path[..len]))
}

unsafe fn session_load(sh: &Shell, hwnd: HWND) {
    set_check(hwnd, 1200, read_bool(&sh.ini, "Session", "File", false));
    set_check(hwnd, 1201, read_bool(&sh.ini, "Session", "Cursor", true));
    set_check(hwnd, 1202, read_bool(&sh.ini, "Session", "Marker", false));
    set_check(hwnd, 1203, read_bool(&sh.ini, "Session", "SaveModified", false));
    let temp = sh.ini.get("Session", "TempFolder").unwrap_or_default();
    set_ctl_text(hwnd, 1204, &temp);
}

unsafe fn session_apply(sh: &mut Shell, hwnd: HWND) -> bool {
    let mut dirty = false;
    for (id, key, default) in [
        (1200, "File", false),
        (1201, "Cursor", true),
        (1202, "Marker", false),
        (1203, "SaveModified", false),
    ] {
        let value = get_check(hwnd, id);
        if read_bool(&sh.ini, "Session", key, default) != value {
            sh.ini.set("Session", key, bool_text(value));
            dirty = true;
        }
    }
    let temp = ctl_text(hwnd, 1204).trim().to_string();
    if sh.ini.get("Session", "TempFolder").unwrap_or_default() != temp {
        sh.ini.set("Session", "TempFolder", &temp);
        dirty = true;
    }
    dirty
}

unsafe fn save_enable(hwnd: HWND) {
    let on = get_check(hwnd, 1200);
    for id in [
        1206, 1207, 1208, 1209, 1210, 1211, 1212, 1213, 1214, 1215, 1216, 1217, 1218,
    ] {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            let _ = EnableWindow(ctl, on);
        }
    }
    if on {
        let special = get_check(hwnd, 1211);
        for id in [1206, 1212] {
            if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
                let _ = EnableWindow(ctl, special);
            }
        }
    }
}

unsafe fn link_updown(hwnd: HWND, up_id: i32, edit_id: i32, min: u32, max: u32) {
    if let (Ok(up), Ok(edit)) = (
        GetDlgItem(Some(hwnd), up_id),
        GetDlgItem(Some(hwnd), edit_id),
    ) {
        SendMessageW(up, UDM_SETBUDDY, Some(WPARAM(edit.0 as usize)), None);
        SendMessageW(
            up,
            UDM_SETRANGE32,
            Some(WPARAM(min as usize)),
            Some(LPARAM(max as isize)),
        );
    }
}

unsafe fn save_load(sh: &Shell, hwnd: HWND) {
    set_check(hwnd, 1200, read_bool(&sh.ini, "Save", "AutoBackup", false));
    set_check(hwnd, 1211, read_bool(&sh.ini, "Save", "SpecialDir", false));
    for (id, key, default) in [
        (1207, "BigFile", 4),
        (1209, "AutoSaveTime", 0),
        (1213, "BackupVersions", DEFAULT_VERSIONS),
        (1215, "KeepDays", DEFAULT_KEEP_DAYS),
        (1217, "BudgetMB", DEFAULT_BUDGET_MB),
    ] {
        set_ctl_text(hwnd, id, &read_u32(&sh.ini, "Save", key, default).to_string());
    }
    set_ctl_text(hwnd, 1212, &sh.ini.get("Save", "Dir").unwrap_or_default());
    link_updown(hwnd, 1208, 1207, 1, 100_000);
    link_updown(hwnd, 1210, 1209, 0, 100_000);
    link_updown(hwnd, 1214, 1213, 1, 500);
    link_updown(hwnd, 1216, 1215, 0, 3650);
    link_updown(hwnd, 1218, 1217, 0, 65536);
    save_enable(hwnd);
}

unsafe fn save_apply(sh: &mut Shell, hwnd: HWND) -> bool {
    let mut dirty = false;
    for (id, key, default) in [(1200, "AutoBackup", false), (1211, "SpecialDir", false)] {
        let value = get_check(hwnd, id);
        if read_bool(&sh.ini, "Save", key, default) != value {
            sh.ini.set("Save", key, bool_text(value));
            dirty = true;
        }
    }
    // 数值项：输入框里可能是空的或者一堆乱七八糟，按默认值兜底再夹进范围。
    for (id, key, default, min, max) in [
        (1207, "BigFile", 4, 1, 100_000),
        (1209, "AutoSaveTime", 0, 0, 100_000),
        (1213, "BackupVersions", DEFAULT_VERSIONS, 1, 500),
        (1215, "KeepDays", DEFAULT_KEEP_DAYS, 0, 3650),
        (1217, "BudgetMB", DEFAULT_BUDGET_MB, 0, 65536),
    ] {
        let value = ctl_text(hwnd, id)
            .trim()
            .parse::<u32>()
            .unwrap_or(default)
            .clamp(min, max);
        if read_u32(&sh.ini, "Save", key, default) != value {
            sh.ini.set("Save", key, &value.to_string());
            dirty = true;
        }
    }
    let dir = ctl_text(hwnd, 1212).trim().to_string();
    if sh.ini.get("Save", "Dir").unwrap_or_default() != dir {
        sh.ini.set("Save", "Dir", &dir);
        dirty = true;
    }
    dirty
}

unsafe fn proxy_type(hwnd: HWND) -> u32 {
    if get_check(hwnd, 1201) {
        1
    } else if get_check(hwnd, 1202) {
        2
    } else {
        0
    }
}

unsafe fn proxy_enable(hwnd: HWND) {
    let on = proxy_type(hwnd) != 0;
    for id in [1203, 1204, 1205, 1206] {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            let _ = EnableWindow(ctl, on);
        }
    }
}

unsafe fn proxy_load(sh: &Shell, hwnd: HWND) {
    let kind = read_u32(&sh.ini, "Proxy", "Type", 0).min(2);
    set_check(hwnd, 1200, kind == 0);
    set_check(hwnd, 1201, kind == 1);
    set_check(hwnd, 1202, kind == 2);
    set_ctl_text(hwnd, 1203, &sh.ini.get("Proxy", "Host").unwrap_or_default());
    set_ctl_text(
        hwnd,
        1204,
        &read_u32(&sh.ini, "Proxy", "Port", 0).to_string(),
    );
    set_ctl_text(hwnd, 1205, &sh.ini.get("Proxy", "User").unwrap_or_default());
    set_ctl_text(hwnd, 1206, &sh.ini.get("Proxy", "Pwd").unwrap_or_default());
    proxy_enable(hwnd);
}

unsafe fn proxy_apply(sh: &mut Shell, hwnd: HWND) -> bool {
    let mut dirty = false;
    let kind = proxy_type(hwnd);
    if read_u32(&sh.ini, "Proxy", "Type", 0) != kind {
        sh.ini.set("Proxy", "Type", &kind.to_string());
        dirty = true;
    }
    let host = ctl_text(hwnd, 1203).trim().to_string();
    if sh.ini.get("Proxy", "Host").unwrap_or_default() != host {
        sh.ini.set("Proxy", "Host", &host);
        dirty = true;
    }
    let port = ctl_text(hwnd, 1204)
        .trim()
        .parse::<u32>()
        .unwrap_or(0)
        .min(65535);
    if read_u32(&sh.ini, "Proxy", "Port", 0) != port {
        sh.ini.set("Proxy", "Port", &port.to_string());
        dirty = true;
    }
    let user = ctl_text(hwnd, 1205).trim().to_string();
    if sh.ini.get("Proxy", "User").unwrap_or_default() != user {
        sh.ini.set("Proxy", "User", &user);
        dirty = true;
    }
    let pwd = ctl_text(hwnd, 1206);
    if sh.ini.get("Proxy", "Pwd").unwrap_or_default() != pwd {
        sh.ini.set("Proxy", "Pwd", &pwd);
        dirty = true;
    }
    dirty
}

const HEX_DEFAULT_EXT: &str = "dat;exe;bin;dll;hex;zip;rar";

unsafe fn hex_load(sh: &Shell, hwnd: HWND) {
    let ext = sh
        .ini
        .get("HEX", "Ext")
        .unwrap_or_else(|| HEX_DEFAULT_EXT.to_string());
    set_ctl_text(hwnd, 1200, &ext);
}

unsafe fn hex_apply(sh: &mut Shell, hwnd: HWND) -> bool {
    let ext = ctl_text(hwnd, 1200).trim().to_string();
    if sh.ini.get("HEX", "Ext").unwrap_or_default() != ext {
        sh.ini.set("HEX", "Ext", &ext);
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Native page plumbing
// ---------------------------------------------------------------------------

const EOL_ITEMS: &[(&str, u32)] = &[("CRLF (PC)", 1), ("LF (Linux)", 2), ("CR (MacOS)", 3)];
const ENCODING_ITEMS: &[(&str, u32)] = &[
    ("ANSI", 0),
    ("UTF-8", 65001),
    ("UTF-8+BOM", 65002),
    ("UTF-16 LE", 1200),
    ("UTF-16 BE", 1201),
    ("GBK", 936),
    ("Shift-JIS", 932),
];
const RELOAD_ITEMS: &[(&str, u32)] = &[
    ("提示变更", 0),
    ("自动读取", 1),
    ("读取并滚动到最后", 2),
];
const READONLY_ITEMS: &[(&str, u32)] = &[
    ("蜂鸣", 1),
    ("修改时提示", 2),
    ("保存时提示", 3),
    ("不提示", 4),
];
const FIXED_ITEMS: &[(&str, u32)] = &[("默认", 1)];
const TRIGGER_ITEMS: &[(&str, u32)] = &[
    ("回车", 13),
    ("空格", 32),
    ("制表符", 9),
    ("Esc键", 27),
];
const MATCH_ITEMS: &[(&str, u32)] = &[("默认", 1), ("部分匹配", 2), ("模糊匹配", 3)];
const COMPLETE_ITEMS: &[(&str, u32)] = &[
    ("@ 代码片段", 1),
    ("- 上下文单词", 2),
    ("* 关键字", 3),
    ("# 工程符号", 4),
    ("~ 文件名称", 5),
];
const BROWSER_ITEMS: &[(&str, u32)] = &[
    ("系统默认浏览器", 1),
    ("Internet Explorer", 2),
    ("Firefox", 3),
    ("Chrome", 4),
    ("Opera", 5),
    ("其它浏览器", 100),
];
const PANE_NAMES: &[(&str, u32)] = &[
    ("光标行号", 1),
    ("光标列号", 2),
    ("光标处字符值", 3),
    ("总行数", 4),
    ("文件编码", 5),
    ("编辑模式", 6),
    ("换行符类型", 7),
    ("制表符", 8),
    ("语法着色类型", 9),
    ("选区信息", 10),
    ("同步滚动", 11),
    ("文件大小", 12),
    ("外部工具", 13),
];

unsafe fn combo_fill(hwnd: HWND, id: i32, items: &[(&str, u32)], want: u32) {
    let Ok(ctl) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    SendMessageW(ctl, CB_RESETCONTENT, None, None);
    for (label, data) in items {
        let mut buf: Vec<u16> = label.encode_utf16().chain([0]).collect();
        let index = SendMessageW(
            ctl,
            CB_ADDSTRING,
            None,
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        )
        .0;
        if index >= 0 {
            SendMessageW(
                ctl,
                CB_SETITEMDATA,
                Some(WPARAM(index as usize)),
                Some(LPARAM(*data as isize)),
            );
        }
    }
    let count = SendMessageW(ctl, CB_GETCOUNT, None, None).0;
    let mut selected = 0usize;
    for i in 0..count.max(0) as usize {
        if SendMessageW(ctl, CB_GETITEMDATA, Some(WPARAM(i)), None).0 == want as isize {
            selected = i;
            break;
        }
    }
    SendMessageW(ctl, CB_SETCURSEL, Some(WPARAM(selected)), None);
}

unsafe fn combo_data(hwnd: HWND, id: i32) -> u32 {
    let Ok(ctl) = GetDlgItem(Some(hwnd), id) else {
        return 0;
    };
    let sel = SendMessageW(ctl, CB_GETCURSEL, None, None).0;
    if sel < 0 {
        return 0;
    }
    SendMessageW(ctl, CB_GETITEMDATA, Some(WPARAM(sel as usize)), None).0 as u32
}

unsafe fn lb_add(ctl: HWND, text: &str, data: usize) -> isize {
    let text = crate::win32::lang::trs(text);
    let mut buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let index = SendMessageW(
        ctl,
        LB_ADDSTRING,
        None,
        Some(LPARAM(buf.as_mut_ptr() as isize)),
    )
    .0;
    if index >= 0 {
        SendMessageW(
            ctl,
            LB_SETITEMDATA,
            Some(WPARAM(index as usize)),
            Some(LPARAM(data as isize)),
        );
    }
    index
}

unsafe fn lb_text(hwnd: HWND, id: i32, index: usize) -> String {
    let Ok(ctl) = GetDlgItem(Some(hwnd), id) else {
        return String::new();
    };
    let len = SendMessageW(ctl, LB_GETTEXTLEN, Some(WPARAM(index)), None).0;
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    SendMessageW(
        ctl,
        LB_GETTEXT,
        Some(WPARAM(index)),
        Some(LPARAM(buf.as_mut_ptr() as isize)),
    );
    String::from_utf16_lossy(&buf[..len as usize])
}

unsafe fn lb_data(hwnd: HWND, id: i32, index: usize) -> usize {
    let Ok(ctl) = GetDlgItem(Some(hwnd), id) else {
        return 0;
    };
    SendMessageW(ctl, LB_GETITEMDATA, Some(WPARAM(index)), None).0 as usize
}

unsafe fn lb_sel(hwnd: HWND, id: i32) -> Option<usize> {
    let ctl = GetDlgItem(Some(hwnd), id).ok()?;
    let sel = SendMessageW(ctl, LB_GETCURSEL, None, None).0;
    (sel >= 0).then_some(sel as usize)
}

unsafe fn lv_add(hwnd: HWND, id: i32, text: &str, data: usize) -> usize {
    let Ok(lv) = GetDlgItem(Some(hwnd), id) else {
        return 0;
    };
    let index = SendMessageW(lv, LVM_GETITEMCOUNT, None, None).0.max(0) as usize;
    let mut buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let mut item = LVITEMW {
        mask: LVIF_TEXT | LVIF_PARAM,
        iItem: index as i32,
        pszText: PWSTR(buf.as_mut_ptr()),
        lParam: LPARAM(data as isize),
        ..Default::default()
    };
    SendMessageW(
        lv,
        LVM_INSERTITEMW,
        Some(WPARAM(0)),
        Some(LPARAM(&mut item as *mut LVITEMW as isize)),
    );
    index
}

unsafe fn lv_clear(hwnd: HWND, id: i32) {
    if let Ok(lv) = GetDlgItem(Some(hwnd), id) {
        SendMessageW(lv, LVM_DELETEALLITEMS, None, None);
    }
}

unsafe fn lv_count(hwnd: HWND, id: i32) -> usize {
    let Ok(lv) = GetDlgItem(Some(hwnd), id) else {
        return 0;
    };
    SendMessageW(lv, LVM_GETITEMCOUNT, None, None).0.max(0) as usize
}

unsafe fn lv_selected(hwnd: HWND, id: i32) -> Option<usize> {
    let lv = GetDlgItem(Some(hwnd), id).ok()?;
    let index = SendMessageW(
        lv,
        LVM_GETNEXTITEM,
        Some(WPARAM(usize::MAX)),
        Some(LPARAM(LVNI_SELECTED as isize)),
    )
    .0;
    (index >= 0).then_some(index as usize)
}

unsafe fn lv_select(hwnd: HWND, id: i32, index: usize) {
    let Ok(lv) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    let mut item = LVITEMW {
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
        state: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
        ..Default::default()
    };
    SendMessageW(
        lv,
        LVM_SETITEMSTATE,
        Some(WPARAM(index)),
        Some(LPARAM(&mut item as *mut LVITEMW as isize)),
    );
}

unsafe fn lv_text(hwnd: HWND, id: i32, index: usize) -> String {
    let Ok(lv) = GetDlgItem(Some(hwnd), id) else {
        return String::new();
    };
    let mut buf = [0u16; 256];
    let mut item = LVITEMW {
        iSubItem: 0,
        pszText: PWSTR(buf.as_mut_ptr()),
        cchTextMax: buf.len() as i32,
        ..Default::default()
    };
    let n = SendMessageW(
        lv,
        LVM_GETITEMTEXTW,
        Some(WPARAM(index)),
        Some(LPARAM(&mut item as *mut LVITEMW as isize)),
    )
    .0;
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe fn lv_data(hwnd: HWND, id: i32, index: usize) -> usize {
    let Ok(lv) = GetDlgItem(Some(hwnd), id) else {
        return 0;
    };
    let mut item = LVITEMW {
        mask: LVIF_PARAM,
        iItem: index as i32,
        ..Default::default()
    };
    if SendMessageW(
        lv,
        LVM_GETITEMW,
        Some(WPARAM(index)),
        Some(LPARAM(&mut item as *mut LVITEMW as isize)),
    )
    .0
        == 0
    {
        return 0;
    }
    item.lParam.0 as usize
}

unsafe fn lv_delete(hwnd: HWND, id: i32, index: usize) {
    if let Ok(lv) = GetDlgItem(Some(hwnd), id) {
        SendMessageW(lv, LVM_DELETEITEM, Some(WPARAM(index)), None);
    }
}

fn browse_file_in(hwnd: HWND, dir: Option<&Path>) -> Option<String> {
    browse_file(hwnd, dir, "", "所有文件\0*.*\0\0")
}

/// `browse_file_in` with the dialog's other two knobs: a file name to start
/// from, and the filter list. `filter` is the doubled-`\0` form
/// `OPENFILENAMEW` wants — label, pattern, empty string.
fn browse_file(hwnd: HWND, dir: Option<&Path>, name: &str, filter: &str) -> Option<String> {
    let mut file = [0u16; 520];
    for (slot, unit) in file.iter_mut().zip(name.encode_utf16()) {
        *slot = unit;
    }
    let filter: Vec<u16> = filter.encode_utf16().collect();
    let initial: Vec<u16> = match dir {
        Some(d) => d.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect(),
        None => Vec::new(),
    };
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrInitialDir: if initial.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(initial.as_ptr())
        },
        Flags: OFN_FILEMUSTEXIST | OFN_HIDEREADONLY,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        return None;
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&file[..len]))
}

unsafe fn insert_at_caret(edit: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    SendMessageW(edit, EM_REPLACESEL, Some(WPARAM(1)), Some(LPARAM(wide.as_ptr() as isize)));
}

// ---------------------------------------------------------------------------
// 文件 (224)
// ---------------------------------------------------------------------------

/// The Type combo: 默认 plus every installed package name, selected
/// from `[Syntax] Default`.
unsafe fn fill_syntax_combo(hwnd: HWND, sh: &Shell) {
    let mut items = vec!["默认".to_string()];
    items.extend(load_template_types(&[]));
    let want = sh
        .ini
        .get("Syntax", "Default")
        .unwrap_or_default()
        .trim()
        .to_string();
    let index = if want.is_empty() {
        0
    } else {
        items
            .iter()
            .position(|name| name.eq_ignore_ascii_case(&want))
            .unwrap_or(0)
    };
    combo_fill_owned(hwnd, 1217, &items, index);
}

unsafe fn file_load(sh: &Shell, hwnd: HWND) {
    combo_fill(
        hwnd,
        1200,
        EOL_ITEMS,
        read_u32(&sh.ini, "Setting", "DefaultFileFormat", 1),
    );
    combo_fill(
        hwnd,
        1201,
        ENCODING_ITEMS,
        read_u32(&sh.ini, "Setting", "DefaultEncoding", 0),
    );
    fill_syntax_combo(hwnd, sh);
    combo_fill(
        hwnd,
        1213,
        RELOAD_ITEMS,
        read_u32(&sh.ini, "Setting", "ReloadMethod", 0),
    );
    combo_fill(
        hwnd,
        1215,
        READONLY_ITEMS,
        read_u32(&sh.ini, "Edit", "ReadOnly", 2),
    );
    combo_fill(hwnd, 1218, FIXED_ITEMS, 1);
    set_check(hwnd, 1212, read_bool(&sh.ini, "Setting", "NewFileOnStartup", false));
    set_check(hwnd, 1220, read_bool(&sh.ini, "Setting", "ANSI2UTF8", false));
    set_check(hwnd, 1221, read_bool(&sh.ini, "Edit", "SaveUnmodified", false));
    set_check(hwnd, 1207, read_bool(&sh.ini, "Setting", "MonitorFile", false));
}

unsafe fn file_apply(sh: &mut Shell, hwnd: HWND) -> bool {
    let mut dirty = false;
    set_ini_u32(
        sh,
        &mut dirty,
        "Setting",
        "DefaultFileFormat",
        combo_data(hwnd, 1200),
        1,
    );
    set_ini_u32(
        sh,
        &mut dirty,
        "Setting",
        "DefaultEncoding",
        combo_data(hwnd, 1201),
        0,
    );
    set_ini_u32(
        sh,
        &mut dirty,
        "Setting",
        "ReloadMethod",
        combo_data(hwnd, 1213),
        0,
    );
    set_ini_u32(
        sh,
        &mut dirty,
        "Edit",
        "ReadOnly",
        combo_data(hwnd, 1215),
        2,
    );
    set_ini_bool(
        sh,
        &mut dirty,
        "Setting",
        "NewFileOnStartup",
        get_check(hwnd, 1212),
        false,
    );
    set_ini_bool(
        sh,
        &mut dirty,
        "Setting",
        "ANSI2UTF8",
        get_check(hwnd, 1220),
        false,
    );
    set_ini_bool(
        sh,
        &mut dirty,
        "Edit",
        "SaveUnmodified",
        get_check(hwnd, 1221),
        false,
    );
    set_ini_bool(
        sh,
        &mut dirty,
        "Setting",
        "MonitorFile",
        get_check(hwnd, 1207),
        false,
    );
    let syntax = if combo_index(hwnd, 1217) == Some(0) {
        String::new()
    } else {
        let Ok(combo) = GetDlgItem(Some(hwnd), 1217) else {
            return dirty;
        };
        // Read the selected string rather than its index: the combo's data
        // is the item index, but the stored setting is the package name.
        let sel = SendMessageW(combo, CB_GETCURSEL, None, None).0;
        let mut buf = vec![0u16; 256];
        if sel >= 0 {
            SendMessageW(
                combo,
                CB_GETLBTEXT,
                Some(WPARAM(sel as usize)),
                Some(LPARAM(buf.as_mut_ptr() as isize)),
            );
            let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
            String::from_utf16_lossy(&buf[..n])
        } else {
            String::new()
        }
    };
    set_ini_str(sh, &mut dirty, "Syntax", "Default", &syntax);
    dirty
}

// ---------------------------------------------------------------------------
// 打印 (247)
// ---------------------------------------------------------------------------

unsafe fn print_load(sh: &Shell, hwnd: HWND) {
    let margin = super::printing::read_margin(sh.ini.get("Print", "Margin").as_deref());
    set_ctl_text(hwnd, 1200, &margin[1].to_string());
    set_ctl_text(hwnd, 1201, &margin[0].to_string());
    set_ctl_text(hwnd, 1202, &margin[2].to_string());
    set_ctl_text(hwnd, 1203, &margin[3].to_string());
    set_check(hwnd, 1204, read_bool(&sh.ini, "Print", "MirrorFont", false));
    set_ctl_text(
        hwnd,
        2405,
        &sh.ini.get("Print", "Header").unwrap_or_default(),
    );
    set_ctl_text(
        hwnd,
        2406,
        &sh.ini.get("Print", "Footer").unwrap_or_default(),
    );
}

unsafe fn print_apply(sh: &mut Shell, hwnd: HWND) -> bool {
    let mut dirty = false;
    let int_at = |id: i32| {
        ctl_text(hwnd, id)
            .trim()
            .parse::<i32>()
            .unwrap_or(16)
            .clamp(0, 200)
    };
    let margin = [int_at(1201), int_at(1200), int_at(1202), int_at(1203)];
    if super::printing::read_margin(sh.ini.get("Print", "Margin").as_deref()) != margin {
        sh.ini.set("Print", "Margin", &super::printing::margin_hex(margin));
        dirty = true;
    }
    set_ini_bool(
        sh,
        &mut dirty,
        "Print",
        "MirrorFont",
        get_check(hwnd, 1204),
        false,
    );
    set_ini_str(
        sh,
        &mut dirty,
        "Print",
        "Header",
        ctl_text(hwnd, 2405).trim(),
    );
    set_ini_str(
        sh,
        &mut dirty,
        "Print",
        "Footer",
        ctl_text(hwnd, 2406).trim(),
    );
    dirty
}

/// The header/footer `...` buttons insert the format tokens the print engine
/// expands (the format strings `%Y-%m-%d`, `%H:%M:%S`).
unsafe fn print_token_menu(hwnd: HWND, edit_id: i32) {
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    for (index, label) in ["日期 (%Y-%m-%d)", "时间 (%H:%M:%S)", "日期时间 (%Y-%m-%d %H:%M:%S)"]
        .iter()
        .enumerate()
    {
        let mut text: Vec<u16> = label.encode_utf16().chain([0]).collect();
        let _ = AppendMenuW(menu, MF_STRING, index + 1, PCWSTR(text.as_mut_ptr()));
    }
    let pos = GetMessagePos();
    let x = (pos as u16 as i16) as i32;
    let y = ((pos >> 16) as u16 as i16) as i32;
    let cmd = TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_LEFTBUTTON | TPM_RETURNCMD,
        x,
        y,
        None,
        hwnd,
        None,
    );
    let token = match cmd.0 {
        1 => "%Y-%m-%d",
        2 => "%H:%M:%S",
        3 => "%Y-%m-%d %H:%M:%S",
        _ => "",
    };
    if !token.is_empty() {
        if let Ok(edit) = GetDlgItem(Some(hwnd), edit_id) {
            let _ = SetFocus(Some(edit));
            insert_at_caret(edit, token);
        }
    }
    let _ = DestroyMenu(menu);
}

// ---------------------------------------------------------------------------
// 自动完成 (254)
// ---------------------------------------------------------------------------

unsafe fn complete_load(sh: &Shell, hwnd: HWND) {
    combo_fill(
        hwnd,
        1200,
        TRIGGER_ITEMS,
        read_u32(&sh.ini, "WordComplete", "WordTrigger", 13),
    );
    combo_fill(
        hwnd,
        1202,
        MATCH_ITEMS,
        read_u32(&sh.ini, "WordComplete", "FuzzyMatch", 1),
    );
    let selected = sh
        .ini
        .get("WordComplete", "Selected")
        .unwrap_or_else(|| "123".to_string());
    let chosen: Vec<u32> = selected
        .chars()
        .filter_map(|c| c.to_digit(10))
        .filter(|n| (1..=5).contains(n))
        .collect();
    if let Ok(all) = GetDlgItem(Some(hwnd), 1203) {
        SendMessageW(all, LB_RESETCONTENT, None, None);
        for (label, id) in COMPLETE_ITEMS {
            if !chosen.contains(id) {
                lb_add(all, label, *id as usize);
            }
        }
    }
    if let Ok(picked) = GetDlgItem(Some(hwnd), 1208) {
        SendMessageW(picked, LB_RESETCONTENT, None, None);
        for (label, id) in COMPLETE_ITEMS {
            if chosen.contains(id) {
                lb_add(picked, label, *id as usize);
            }
        }
    }
    set_check(
        hwnd,
        1209,
        read_bool(&sh.ini, "WordComplete", "UseNumKey", false),
    );
}

unsafe fn complete_selected_string(hwnd: HWND) -> String {
    let count = GetDlgItem(Some(hwnd), 1208)
        .map(|ctl| SendMessageW(ctl, LB_GETCOUNT, None, None).0.max(0) as usize)
        .unwrap_or(0);
    let mut out = String::new();
    for i in 0..count {
        let data = lb_data(hwnd, 1208, i);
        if data <= 9 {
            out.push(char::from_digit(data as u32, 10).unwrap_or('0'));
        }
    }
    out
}

unsafe fn complete_apply(sh: &mut Shell, hwnd: HWND) -> bool {
    let mut dirty = false;
    set_ini_u32(
        sh,
        &mut dirty,
        "WordComplete",
        "WordTrigger",
        combo_data(hwnd, 1200),
        13,
    );
    set_ini_u32(
        sh,
        &mut dirty,
        "WordComplete",
        "FuzzyMatch",
        combo_data(hwnd, 1202),
        1,
    );
    set_ini_bool(
        sh,
        &mut dirty,
        "WordComplete",
        "UseNumKey",
        get_check(hwnd, 1209),
        false,
    );
    set_ini_str(
        sh,
        &mut dirty,
        "WordComplete",
        "Selected",
        &complete_selected_string(hwnd),
    );
    dirty
}

/// Move the selected row of `from` to `to` (the `>>`/`<<` buttons).
unsafe fn lb_move(hwnd: HWND, from: i32, to: i32) {
    let Some(index) = lb_sel(hwnd, from) else {
        return;
    };
    let text = lb_text(hwnd, from, index);
    let data = lb_data(hwnd, from, index);
    if let Ok(ctl) = GetDlgItem(Some(hwnd), from) {
        SendMessageW(ctl, LB_DELETESTRING, Some(WPARAM(index)), None);
    }
    if let Ok(ctl) = GetDlgItem(Some(hwnd), to) {
        let at = lb_add(ctl, &text, data);
        if at >= 0 {
            SendMessageW(ctl, LB_SETCURSEL, Some(WPARAM(at as usize)), None);
        }
    }
}

/// Swap the selected listbox row with its neighbour.
unsafe fn lb_step(hwnd: HWND, id: i32, delta: i32) {
    let Some(index) = lb_sel(hwnd, id) else {
        return;
    };
    let target = index as i32 + delta;
    let count = GetDlgItem(Some(hwnd), id)
        .map(|ctl| SendMessageW(ctl, LB_GETCOUNT, None, None).0.max(0) as i32)
        .unwrap_or(0);
    if target < 0 || target >= count {
        return;
    }
    let text = lb_text(hwnd, id, index);
    let data = lb_data(hwnd, id, index);
    let other_text = lb_text(hwnd, id, target as usize);
    let other_data = lb_data(hwnd, id, target as usize);
    if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
        SendMessageW(ctl, LB_DELETESTRING, Some(WPARAM(index)), None);
        let mut buf: Vec<u16> = other_text.encode_utf16().chain([0]).collect();
        let at = SendMessageW(
            ctl,
            LB_INSERTSTRING,
            Some(WPARAM(index)),
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        )
        .0;
        if at >= 0 {
            SendMessageW(
                ctl,
                LB_SETITEMDATA,
                Some(WPARAM(at as usize)),
                Some(LPARAM(other_data as isize)),
            );
        }
        SendMessageW(ctl, LB_DELETESTRING, Some(WPARAM(target as usize)), None);
        let mut buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let at = SendMessageW(
            ctl,
            LB_INSERTSTRING,
            Some(WPARAM(target as usize)),
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        )
        .0;
        if at >= 0 {
            SendMessageW(
                ctl,
                LB_SETITEMDATA,
                Some(WPARAM(at as usize)),
                Some(LPARAM(data as isize)),
            );
        }
        SendMessageW(ctl, LB_SETCURSEL, Some(WPARAM(target as usize)), None);
    }
}

/// Turns on a list view's extended styles (check boxes / full-row select).
unsafe fn lv_ext(hwnd: HWND, id: i32, ex: u32) {
    if let Ok(lv) = GetDlgItem(Some(hwnd), id) {
        SendMessageW(
            lv,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            Some(WPARAM(ex as usize)),
            Some(LPARAM(ex as isize)),
        );
    }
}

/// Adds report columns to a list view. `width` is in pixels; 0 asks for the
/// control's client width.
unsafe fn lv_columns(hwnd: HWND, id: i32, cols: &[(&str, i32)]) {
    let Ok(lv) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    let mut rc = RECT::default();
    let _ = GetClientRect(lv, &mut rc);
    let client = (rc.right - rc.left).max(80);
    for (index, (label, width)) in cols.iter().enumerate() {
        let mut text: Vec<u16> = label.encode_utf16().chain([0]).collect();
        let mut col = LVCOLUMNW {
            mask: LVCF_FMT | LVCF_WIDTH | LVCF_TEXT | LVCF_SUBITEM,
            fmt: LVCFMT_LEFT,
            cx: if *width > 0 {
                *width
            } else {
                (client / cols.len().max(1) as i32).max(40)
            },
            pszText: PWSTR(text.as_mut_ptr()),
            iSubItem: index as i32,
            ..Default::default()
        };
        SendMessageW(
            lv,
            LVM_INSERTCOLUMNW,
            Some(WPARAM(index)),
            Some(LPARAM(&mut col as *mut LVCOLUMNW as isize)),
        );
    }
}

unsafe fn lv_set_subtext(hwnd: HWND, id: i32, index: usize, sub: i32, text: &str) {
    let Ok(lv) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    let mut buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: index as i32,
        iSubItem: sub,
        pszText: PWSTR(buf.as_mut_ptr()),
        ..Default::default()
    };
    SendMessageW(
        lv,
        LVM_SETITEMTEXTW,
        Some(WPARAM(index)),
        Some(LPARAM(&mut item as *mut LVITEMW as isize)),
    );
}

/// Inserts a two-column row (the Web预览 mapping table).
unsafe fn lv_add2(hwnd: HWND, id: i32, first: &str, second: &str, data: usize) -> usize {
    let index = lv_add(hwnd, id, first, data);
    lv_set_subtext(hwnd, id, index, 1, second);
    index
}

unsafe fn lv_checked(hwnd: HWND, id: i32, index: usize) -> bool {
    let Ok(lv) = GetDlgItem(Some(hwnd), id) else {
        return false;
    };
    let state = SendMessageW(
        lv,
        LVM_GETITEMSTATE,
        Some(WPARAM(index)),
        Some(LPARAM(LVIS_STATEIMAGEMASK.0 as isize)),
    )
    .0 as u32;
    (state & 0xF000) == 0x2000
}

unsafe fn lv_set_checked(hwnd: HWND, id: i32, index: usize, on: bool) {
    let Ok(lv) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    let image = if on { 0x2000 } else { 0x1000 };
    let mut item = LVITEMW {
        stateMask: LVIS_STATEIMAGEMASK,
        state: LIST_VIEW_ITEM_STATE_FLAGS(image),
        ..Default::default()
    };
    SendMessageW(
        lv,
        LVM_SETITEMSTATE,
        Some(WPARAM(index)),
        Some(LPARAM(&mut item as *mut LVITEMW as isize)),
    );
}

unsafe fn lv_set_text(hwnd: HWND, id: i32, index: usize, text: &str) {
    lv_set_subtext(hwnd, id, index, 0, text);
}

unsafe fn lv_set_data(hwnd: HWND, id: i32, index: usize, data: usize) {
    let Ok(lv) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    let mut item = LVITEMW {
        mask: LVIF_PARAM,
        iItem: index as i32,
        lParam: LPARAM(data as isize),
        ..Default::default()
    };
    SendMessageW(
        lv,
        LVM_SETITEMW,
        Some(WPARAM(index)),
        Some(LPARAM(&mut item as *mut LVITEMW as isize)),
    );
}

/// Swap a checked list view row with its neighbour (状态栏's 上移/下移).
unsafe fn lb_step_lv(hwnd: HWND, id: i32, delta: i32) {
    let Some(index) = lv_selected(hwnd, id) else {
        return;
    };
    let target = index as i32 + delta;
    if target < 0 || target as usize >= lv_count(hwnd, id) {
        return;
    }
    let target = target as usize;
    let a_text = lv_text(hwnd, id, index);
    let b_text = lv_text(hwnd, id, target);
    let a_data = lv_data(hwnd, id, index);
    let b_data = lv_data(hwnd, id, target);
    let a_check = lv_checked(hwnd, id, index);
    let b_check = lv_checked(hwnd, id, target);
    lv_set_text(hwnd, id, index, &b_text);
    lv_set_data(hwnd, id, index, b_data);
    lv_set_text(hwnd, id, target, &a_text);
    lv_set_data(hwnd, id, target, a_data);
    lv_set_checked(hwnd, id, index, b_check);
    lv_set_checked(hwnd, id, target, a_check);
    lv_select(hwnd, id, target);
}

/// A combo box whose item data is the item's own index (the template Type
/// list is built from the install's packages, so it is not a static table).
unsafe fn combo_fill_owned(hwnd: HWND, id: i32, items: &[String], want: usize) {
    let Ok(ctl) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    SendMessageW(ctl, CB_RESETCONTENT, None, None);
    for (index, label) in items.iter().enumerate() {
        let mut buf: Vec<u16> = label.encode_utf16().chain([0]).collect();
        let at = SendMessageW(
            ctl,
            CB_ADDSTRING,
            None,
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        )
        .0;
        if at >= 0 {
            SendMessageW(
                ctl,
                CB_SETITEMDATA,
                Some(WPARAM(at as usize)),
                Some(LPARAM(index as isize)),
            );
        }
    }
    if !items.is_empty() {
        SendMessageW(ctl, CB_SETCURSEL, Some(WPARAM(want.min(items.len() - 1))), None);
    }
}

unsafe fn combo_index(hwnd: HWND, id: i32) -> Option<usize> {
    let ctl = GetDlgItem(Some(hwnd), id).ok()?;
    let sel = SendMessageW(ctl, CB_GETCURSEL, None, None).0;
    (sel >= 0).then_some(sel as usize)
}

fn toolbar_label(id: u32) -> String {
    if id == 0 {
        return "--------".to_string();
    }
    if id == 1320 {
        // The palette's FTP button; the bar's own caption for 1320 is not a
        // menu row (the dropdown carries 1239 instead).
        return "FTP/SFTP".to_string();
    }
    // 文件菜单已经撤掉 FTP/SFTP 那一行，但工具条上那颗（置灰的）按钮还在：
    // 没了菜单行就查不到标题，名字得从命令说明表里取，否则选项页会显示成
    // 「命令 1239」。
    if id == 1239 {
        return crate::win32::cmdtext::text_for(id).0.to_string();
    }
    crate::win32::lang::menu(128, id)
        .or_else(|| crate::win32::menubar::command_caption(id))
        .map(|caption| caption.replace('&', ""))
        .unwrap_or_else(|| format!("命令 {id}"))
}

/// `[ToolBar]` as the IDs the layout editor shows: `Count` entries of
/// `ToolBar`, 8 hex digits each, little-endian.
fn read_toolbar_ids(sh: &Shell) -> Vec<u32> {
    let count = sh
        .ini
        .get("ToolBar", "Count")
        .and_then(|v| v.trim().parse::<usize>().ok());
    let hex = sh.ini.get("ToolBar", "ToolBar");
    if let (Some(count), Some(hex)) = (count, hex) {
        let ids = crate::win32::toolbar::ids_from_hex(&hex, count);
        if !ids.is_empty() {
            return ids;
        }
    }
    crate::win32::toolbar::default_layout()
        .iter()
        .map(|row| row.id)
        .collect()
}

/// The `[ToolBar]` value the page writes — the shared codec, so the frame's
/// reader and this writer cannot drift apart.
fn toolbar_hex(ids: &[u32]) -> String {
    crate::win32::toolbar::ids_to_hex(ids)
}

fn app_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `template.ini` beside the executable, or in the working directory —
/// the same two candidates `setting.ini` uses, so a checkout run from its
/// own root and an installed copy both find their templates.
fn template_ini_path() -> PathBuf {
    let beside = app_dir().join("template.ini");
    if beside.exists() {
        return beside;
    }
    let cwd = PathBuf::from("template.ini");
    if cwd.exists() {
        return cwd;
    }
    beside
}

/// A template's file. An absolute `File` is itself; a relative one is under
/// the `template` folder next to the ini that named it.
pub(super) fn template_file_path(file: &str) -> PathBuf {
    let p = PathBuf::from(file);
    if p.is_absolute() {
        return p;
    }
    let base = template_ini_path()
        .parent()
        .map(|d| d.to_path_buf())
        .unwrap_or_else(app_dir);
    base.join("template").join(p)
}

/// The `[Temp]` rows the 新建 menu shows and the create-from-template path
/// reads. Absent file → no rows.
pub(super) fn load_templates() -> Vec<TemplateEntry> {
    load_template_entries(&IniText::load(&template_ini_path()))
}

/// `[Temp] UseTempFormat`: a template document takes the template file's own
/// encoding and line ending instead of the new-document defaults.
pub(super) fn template_use_file_format() -> bool {
    read_bool(
        &IniText::load(&template_ini_path()),
        "Temp",
        "UseTempFormat",
        false,
    )
}

fn load_template_entries(ini: &IniText) -> Vec<TemplateEntry> {
    let count = ini
        .get("Temp", "Count")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let section = format!("Temp{i}");
        out.push(TemplateEntry {
            title: ini.get(&section, "Title").unwrap_or_default(),
            type_: ini.get(&section, "Type").unwrap_or_default(),
            file: ini.get(&section, "File").unwrap_or_default(),
            snippet: read_bool(ini, &section, "Snippet", false),
        });
    }
    out
}

/// The Type combo's entries: every `package\*.pkg` stem.
fn load_template_types(entries: &[TemplateEntry]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(app_dir().join("package")) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pkg"))
            {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    out.push(stem.to_string());
                }
            }
        }
        out.sort_by_key(|name| name.to_lowercase());
        out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    }
    if out.is_empty() {
        for name in [
            "Text", "C++", "HTML", "Java", "JavaScript", "Markdown", "PHP", "Python", "Rust",
            "XML", "CSS", "JSON",
        ] {
            out.push(name.to_string());
        }
    }
    for entry in entries {
        if !entry.type_.is_empty() && !out.iter().any(|t| t.eq_ignore_ascii_case(&entry.type_)) {
            out.push(entry.type_.clone());
        }
    }
    out
}

/// The status strip's configurable pane ids, in `[Setting] SBPane` order. A
/// present-but-empty key is a user who unchecked every pane.
fn sbpane_from(ini: &IniText) -> Vec<u32> {
    let Some(raw) = ini.get("Setting", "SBPane") else {
        return vec![12, 11, 10, 9, 8, 1, 2, 3, 4, 7, 6, 5];
    };
    let mut out = Vec::new();
    for part in raw.split(',') {
        if let Ok(id) = part.trim().parse::<u32>() {
            if (1..=13).contains(&id) && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

fn read_sbpane(sh: &Shell) -> Vec<u32> {
    sbpane_from(&sh.ini)
}

/// `[Setting] SBPane` as the frame's strip needs it, read once at startup and
/// again whenever 选项 closes with 确定/应用.
pub(super) fn load_status_order() -> Vec<u32> {
    sbpane_from(&IniText::load(&ini_path()))
}

fn sbpane_text(ids: &[u32]) -> String {
    let mut out = String::new();
    for id in ids {
        out.push_str(&format!("{id},"));
    }
    out
}

// ---------------------------------------------------------------------------
// Small modal input dialogs
// ---------------------------------------------------------------------------

struct AskCtx {
    shell: *mut Shell,
    initial: String,
    result: Option<String>,
    field: HBRUSH,
}

unsafe extern "system" fn ask_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp.0);
            let ctx = &mut *(lp.0 as *mut AskCtx);
            let sh = &*ctx.shell;
            set_font_deep(hwnd, sh.font);
            chrome::dark_theme_children(hwnd, sh.dark);
            if sh.dark {
                plain_field(hwnd);
            }
            set_ctl_text(hwnd, 1001, &ctx.initial);
            if let Ok(edit) = GetDlgItem(Some(hwnd), 1001) {
                let _ = SetFocus(Some(edit));
                SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
            1
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as i32;
            match id {
                1002 => {
                    if let Some(ctx) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AskCtx).as_mut()
                    {
                        ctx.result = Some(ctl_text(hwnd, 1001));
                    }
                    let _ = EndDialog(hwnd, 1);
                    1
                }
                1003 => {
                    let _ = EndDialog(hwnd, 0);
                    1
                }
                _ => 0,
            }
        }
        WM_CLOSE => {
            let _ = EndDialog(hwnd, 0);
            1
        }
        WM_ERASEBKGND => {
            let Some(p) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AskCtx).as_ref() else {
                return 0;
            };
            let sh = &*p.shell;
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let _ = FillRect(HDC(wp.0 as *mut core::ffi::c_void), &rc, sh.bg);
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let Some(p) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AskCtx).as_ref() else {
                return 0;
            };
            let sh = &*p.shell;
            customdraw::ctlcolor(
                HDC(wp.0 as *mut core::ffi::c_void),
                to_colorref(sh.theme.toolbar_fg),
                to_colorref(sh.theme.toolbar_bg),
                sh.bg,
            )
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let Some(p) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AskCtx).as_ref() else {
                return 0;
            };
            let sh = &*p.shell;
            customdraw::ctlcolor(
                HDC(wp.0 as *mut core::ffi::c_void),
                to_colorref(sh.theme.toolbar_fg),
                to_colorref(sh.theme.toolbar_field),
                p.field,
            )
        }
        _ => 0,
    }
}

unsafe fn ask_text(shell: *mut Shell, owner: HWND, caption: &str, initial: &str) -> Option<String> {
    let items = [
        Item {
            id: 1000,
            class: Class::Atom(ATOM_STATIC),
            style: 0x5002_0000,
            x: 7,
            y: 7,
            w: 200,
            h: 10,
            label: "请输入标题:",
        },
        Item {
            id: 1001,
            class: Class::Name("EDIT"),
            style: 0x5081_0080,
            x: 7,
            y: 20,
            w: 200,
            h: 14,
            label: "",
        },
        Item {
            id: 1002,
            class: Class::Atom(ATOM_BUTTON),
            style: 0x5001_0001,
            x: 100,
            y: 42,
            w: 50,
            h: 14,
            label: crate::win32::lang::tr("确认").unwrap_or("确认"),
        },
        Item {
            id: 1003,
            class: Class::Atom(ATOM_BUTTON),
            style: 0x5001_0000,
            x: 157,
            y: 42,
            w: 50,
            h: 14,
            label: crate::win32::lang::tr("取消").unwrap_or("取消"),
        },
    ];
    let tmpl = build_template(caption, 0x90C8_00C8, 214, 62, 0, &items);
    let mut aligned = vec![0u32; (tmpl.len() + 3) / 4];
    std::ptr::copy_nonoverlapping(tmpl.as_ptr(), aligned.as_mut_ptr() as *mut u8, tmpl.len());
    let field = CreateSolidBrush(to_colorref((*shell).theme.toolbar_field));
    let mut ctx = AskCtx {
        shell,
        initial: initial.to_string(),
        result: None,
        field,
    };
    let ctx_ptr = &mut ctx as *mut AskCtx;
    let _ = DialogBoxIndirectParamW(
        Some(instance()),
        aligned.as_ptr() as *const DLGTEMPLATE,
        Some(owner),
        Some(ask_proc),
        LPARAM(ctx_ptr as isize),
    );
    let _ = DeleteObject(HGDIOBJ(field.0));
    ctx.result.map(|text| text.trim().to_string())
}

/// The Host/Directory Mapping dialog. The page's Add and
/// Modify buttons both run it; only the initial text differs.
struct HostCtx {
    shell: *mut Shell,
    host: String,
    dir: String,
    result: Option<(String, String)>,
    field: HBRUSH,
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp.0);
            let ctx = &mut *(lp.0 as *mut HostCtx);
            let sh = &*ctx.shell;
            set_font_deep(hwnd, sh.font);
            chrome::dark_theme_children(hwnd, sh.dark);
            if sh.dark {
                plain_field(hwnd);
            }
            localize_page(hwnd, 273);
            set_ctl_text(hwnd, 1200, &ctx.host);
            set_ctl_text(hwnd, 1201, &ctx.dir);
            if let Ok(edit) = GetDlgItem(Some(hwnd), 1200) {
                let _ = SetFocus(Some(edit));
            }
            1
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as i32;
            match id {
                1202 => {
                    if let Some(folder) = browse_folder(hwnd) {
                        set_ctl_text(hwnd, 1201, &folder);
                    }
                    1
                }
                1 => {
                    let host = ctl_text(hwnd, 1200).trim().to_string();
                    let dir = ctl_text(hwnd, 1201).trim().to_string();
                    if host.is_empty() || dir.is_empty() {
                        return 1;
                    }
                    if let Some(ctx) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut HostCtx).as_mut()
                    {
                        ctx.result = Some((host, dir));
                    }
                    let _ = EndDialog(hwnd, 1);
                    1
                }
                2 => {
                    let _ = EndDialog(hwnd, 0);
                    1
                }
                _ => 0,
            }
        }
        WM_CLOSE => {
            let _ = EndDialog(hwnd, 0);
            1
        }
        WM_ERASEBKGND => {
            let Some(p) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut HostCtx).as_ref() else {
                return 0;
            };
            let sh = &*p.shell;
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let _ = FillRect(HDC(wp.0 as *mut core::ffi::c_void), &rc, sh.bg);
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let Some(p) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut HostCtx).as_ref() else {
                return 0;
            };
            let sh = &*p.shell;
            customdraw::ctlcolor(
                HDC(wp.0 as *mut core::ffi::c_void),
                to_colorref(sh.theme.toolbar_fg),
                to_colorref(sh.theme.toolbar_bg),
                sh.bg,
            )
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let Some(p) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut HostCtx).as_ref() else {
                return 0;
            };
            let sh = &*p.shell;
            customdraw::ctlcolor(
                HDC(wp.0 as *mut core::ffi::c_void),
                to_colorref(sh.theme.toolbar_fg),
                to_colorref(sh.theme.toolbar_field),
                p.field,
            )
        }
        _ => 0,
    }
}

unsafe fn host_dialog(shell: *mut Shell, owner: HWND, host: &str, dir: &str) -> Option<(String, String)> {
    let tmpl = pages::host_template();
    let mut aligned = vec![0u32; (tmpl.len() + 3) / 4];
    std::ptr::copy_nonoverlapping(tmpl.as_ptr(), aligned.as_mut_ptr() as *mut u8, tmpl.len());
    let field = CreateSolidBrush(to_colorref((*shell).theme.toolbar_field));
    let mut ctx = HostCtx {
        shell,
        host: host.to_string(),
        dir: dir.to_string(),
        result: None,
        field,
    };
    let ctx_ptr = &mut ctx as *mut HostCtx;
    let _ = DialogBoxIndirectParamW(
        Some(instance()),
        aligned.as_ptr() as *const DLGTEMPLATE,
        Some(owner),
        Some(host_proc),
        LPARAM(ctx_ptr as isize),
    );
    let _ = DeleteObject(HGDIOBJ(field.0));
    ctx.result
}

// ---------------------------------------------------------------------------
// 状态栏 (296)
// ---------------------------------------------------------------------------

unsafe fn status_load(sh: &Shell, hwnd: HWND) {
    let configured = read_sbpane(sh);
    let header = crate::win32::lang::trs("状态栏");
    lv_columns(hwnd, 1200, &[(&header, 0)]);
    lv_ext(hwnd, 1200, LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT);
    let mut order = configured.clone();
    for id in 1..=13u32 {
        if !order.contains(&id) {
            order.push(id);
        }
    }
    for id in &order {
        let label = PANE_NAMES
            .iter()
            .find(|(_, pane)| pane == id)
            .map(|(name, pane)| {
                crate::win32::lang::menu(355, *pane)
                    .map(str::to_string)
                    .unwrap_or_else(|| crate::win32::lang::trs(name))
            })
            .unwrap_or_else(|| "?".to_string());
        let index = lv_add(hwnd, 1200, &label, *id as usize);
        if configured.contains(id) {
            lv_set_checked(hwnd, 1200, index, true);
        }
    }
    if !order.is_empty() {
        lv_select(hwnd, 1200, 0);
    }
}

unsafe fn status_apply(sh: &mut Shell, hwnd: HWND) -> bool {
    let mut ids = Vec::new();
    for index in 0..lv_count(hwnd, 1200) {
        if lv_checked(hwnd, 1200, index) {
            ids.push(lv_data(hwnd, 1200, index) as u32);
        }
    }
    let text = sbpane_text(&ids);
    if sh.ini.get("Setting", "SBPane").unwrap_or_default().trim() == text {
        return false;
    }
    sh.ini.set("Setting", "SBPane", &text);
    true
}

// ---------------------------------------------------------------------------
// 工具条 (207)
// ---------------------------------------------------------------------------

unsafe fn lb_insert(ctl: HWND, at: usize, text: &str, data: usize) -> isize {
    let mut buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let index = SendMessageW(
        ctl,
        LB_INSERTSTRING,
        Some(WPARAM(at)),
        Some(LPARAM(buf.as_mut_ptr() as isize)),
    )
    .0;
    if index >= 0 {
        SendMessageW(
            ctl,
            LB_SETITEMDATA,
            Some(WPARAM(index as usize)),
            Some(LPARAM(data as isize)),
        );
    }
    index
}

unsafe fn toolbar_add(hwnd: HWND) {
    let Some(sel) = lb_sel(hwnd, 1201) else {
        return;
    };
    let mut id = lb_data(hwnd, 1201, sel) as u32;
    // 1601..1610 are the per-encoding commands the palette folds into the
    // 编码 dropdown (1580)
    if (1601..=1610).contains(&id) {
        id = 1580;
    }
    let Ok(left) = GetDlgItem(Some(hwnd), 1201) else {
        return;
    };
    let Ok(right) = GetDlgItem(Some(hwnd), 1202) else {
        return;
    };
    let count = SendMessageW(right, LB_GETCOUNT, None, None).0.max(0) as usize;
    for index in 0..count {
        if SendMessageW(right, LB_GETITEMDATA, Some(WPARAM(index)), None).0 as u32 == id {
            SendMessageW(right, LB_SETCURSEL, Some(WPARAM(index)), None);
            return;
        }
    }
    let at = lb_sel(hwnd, 1202).map(|i| i + 1).unwrap_or(count);
    let index = lb_insert(right, at, &toolbar_label(id), id as usize);
    if index >= 0 {
        SendMessageW(right, LB_SETCURSEL, Some(WPARAM(index as usize)), None);
    }
    let left_count = SendMessageW(left, LB_GETCOUNT, None, None).0.max(0) as usize;
    if left_count > 0 {
        SendMessageW(left, LB_SETCURSEL, Some(WPARAM((sel + 1).min(left_count - 1))), None);
    }
}

unsafe fn toolbar_add_separator(hwnd: HWND) {
    let Ok(right) = GetDlgItem(Some(hwnd), 1202) else {
        return;
    };
    let count = SendMessageW(right, LB_GETCOUNT, None, None).0.max(0) as usize;
    let at = lb_sel(hwnd, 1202).map(|i| i + 1).unwrap_or(count);
    let index = lb_insert(right, at, &toolbar_label(0), 0);
    if index >= 0 {
        SendMessageW(right, LB_SETCURSEL, Some(WPARAM(index as usize)), None);
    }
}

unsafe fn toolbar_remove(hwnd: HWND) {
    let Some(index) = lb_sel(hwnd, 1202) else {
        return;
    };
    let Ok(right) = GetDlgItem(Some(hwnd), 1202) else {
        return;
    };
    SendMessageW(right, LB_DELETESTRING, Some(WPARAM(index)), None);
    let count = SendMessageW(right, LB_GETCOUNT, None, None).0.max(0) as usize;
    if count > 0 {
        let select = index.min(count - 1);
        SendMessageW(right, LB_SETCURSEL, Some(WPARAM(select)), None);
    }
}

unsafe fn toolbar_load(sh: &Shell, hwnd: HWND) {
    if let Ok(left) = GetDlgItem(Some(hwnd), 1201) {
        SendMessageW(left, LB_RESETCONTENT, None, None);
        // The menu bar names the commands in the order a user reads them;
        // the rest of the palette (commands no menu row carries) follows in
        // the table's own order.
        let palette = crate::win32::toolbar::palette_ids();
        let ordered = crate::win32::menubar::command_order()
            .into_iter()
            .filter(|id| palette.contains(id));
        let mut listed = Vec::new();
        for id in ordered {
            if !listed.contains(&id) {
                lb_add(left, &toolbar_label(id), id as usize);
                listed.push(id);
            }
        }
        for id in palette {
            if !listed.contains(&id) {
                lb_add(left, &toolbar_label(id), id as usize);
            }
        }
        SendMessageW(left, LB_SETCURSEL, Some(WPARAM(0)), None);
    }
    if let Ok(right) = GetDlgItem(Some(hwnd), 1202) {
        SendMessageW(right, LB_RESETCONTENT, None, None);
        for id in read_toolbar_ids(sh) {
            lb_add(right, &toolbar_label(id), id as usize);
        }
        SendMessageW(right, LB_SETCURSEL, Some(WPARAM(0)), None);
    }
    toolbar_theme_load(sh, hwnd);
}

unsafe fn toolbar_current_ids(hwnd: HWND) -> Vec<u32> {
    let mut ids = Vec::new();
    for index in 0..GetDlgItem(Some(hwnd), 1202)
        .map(|ctl| SendMessageW(ctl, LB_GETCOUNT, None, None).0.max(0) as usize)
        .unwrap_or(0)
    {
        ids.push(lb_data(hwnd, 1202, index) as u32);
    }
    ids
}

unsafe fn toolbar_apply(sh: &mut Shell, hwnd: HWND) -> bool {
    let mut changed = false;
    let ids = toolbar_current_ids(hwnd);
    if ids != read_toolbar_ids(sh) {
        sh.ini.set("ToolBar", "Count", &ids.len().to_string());
        sh.ini.set("ToolBar", "ToolBar", &toolbar_hex(&ids));
        changed = true;
    }
    if let Some(theme) = toolbar_theme_pick(hwnd) {
        let current = sh
            .ini
            .get("Setting", "IconTheme")
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
        if theme != current {
            sh.ini.set("Setting", "IconTheme", &theme);
            changed = true;
        }
    }
    changed
}

/// Fills the icon theme row: 默认 first, then the folders the theme roots
/// carry. The saved name selects itself when the list still holds it.
unsafe fn toolbar_theme_load(sh: &Shell, hwnd: HWND) {
    let Ok(combo) = GetDlgItem(Some(hwnd), 1208) else {
        return;
    };
    SendMessageW(combo, CB_RESETCONTENT, None, None);
    let roots = crate::win32::toolbar::theme_roots();
    let mut names = vec![String::new()];
    names.extend(crate::win32::toolbar::theme_names(&roots));
    for name in &names {
        let label = if name.is_empty() { "默认" } else { name.as_str() };
        let mut buf: Vec<u16> = label.encode_utf16().chain([0]).collect();
        SendMessageW(
            combo,
            CB_ADDSTRING,
            None,
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        );
    }
    let current = sh
        .ini
        .get("Setting", "IconTheme")
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    let sel = names.iter().position(|n| *n == current).unwrap_or(0);
    SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(sel)), None);
}

/// The icon theme the picker holds: `None` when the combo is not there,
/// `Some("")` for the built-in sheet set.
unsafe fn toolbar_theme_pick(hwnd: HWND) -> Option<String> {
    let combo = GetDlgItem(Some(hwnd), 1208).ok()?;
    let sel = SendMessageW(combo, CB_GETCURSEL, None, None).0;
    if sel < 0 {
        return None;
    }
    if sel == 0 {
        return Some(String::new());
    }
    crate::win32::toolbar::theme_names(&crate::win32::toolbar::theme_roots())
        .get(sel as usize - 1)
        .cloned()
}

/// Opens the theme folders' home: the executable-side root when it already
/// exists (a portable install keeps its themes there), else the user's
/// `%APPDATA%` root, created on first use.
unsafe fn open_theme_dir(hwnd: HWND) {
    let roots = crate::win32::toolbar::theme_roots();
    let Some(target) = roots
        .iter()
        .find(|root| root.is_dir())
        .cloned()
        .or_else(|| roots.last().cloned())
        .filter(|root| !root.as_os_str().is_empty())
    else {
        return;
    };
    let _ = std::fs::create_dir_all(&target);
    let wide: Vec<u16> = target.as_os_str().encode_wide().chain([0]).collect();
    let _ = ShellExecuteW(Some(hwnd), None, PCWSTR(wide.as_ptr()), None, None, SW_SHOW);
}
// ---------------------------------------------------------------------------
// 模板 (213)
// ---------------------------------------------------------------------------

unsafe fn template_show(ctx: &mut NativeCtx, hwnd: HWND) {
    let Some(index) = lv_selected(hwnd, 1201) else {
        return;
    };
    let Some(entry) = ctx.templates.get(index).cloned() else {
        return;
    };
    ctx.syncing = true;
    set_ctl_text(hwnd, 1206, &entry.title);
    set_ctl_text(hwnd, 1208, &entry.file);
    set_check(hwnd, 1210, entry.snippet);
    let type_index = ctx
        .temp_types
        .iter()
        .position(|name| name.eq_ignore_ascii_case(&entry.type_))
        .unwrap_or(0);
    if !ctx.temp_types.is_empty() {
        if let Ok(combo) = GetDlgItem(Some(hwnd), 1200) {
            SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(type_index)), None);
        }
    }
    ctx.syncing = false;
}

unsafe fn template_sync(ctx: &mut NativeCtx, hwnd: HWND) {
    if ctx.syncing {
        return;
    }
    let Some(index) = lv_selected(hwnd, 1201) else {
        return;
    };
    if index >= ctx.templates.len() {
        return;
    }
    let title = ctl_text(hwnd, 1206).trim().to_string();
    let file = ctl_text(hwnd, 1208).trim().to_string();
    let type_ = combo_index(hwnd, 1200)
        .and_then(|i| ctx.temp_types.get(i).cloned())
        .unwrap_or_default();
    let snippet = get_check(hwnd, 1210);
    let entry = &mut ctx.templates[index];
    entry.title = title.clone();
    entry.type_ = type_;
    entry.file = file;
    entry.snippet = snippet;
    lv_set_text(hwnd, 1201, index, &title);
    ctx.dirty = true;
}

unsafe fn template_refresh(ctx: &NativeCtx, hwnd: HWND, select: Option<usize>) {
    lv_clear(hwnd, 1201);
    for entry in &ctx.templates {
        lv_add(hwnd, 1201, &entry.title, 0);
    }
    if let Some(index) = select {
        if index < ctx.templates.len() {
            lv_select(hwnd, 1201, index);
        }
    }
}

unsafe fn template_edit(ctx: &mut NativeCtx, hwnd: HWND, index: usize) {
    ctx.templates[index].title = ctl_text(hwnd, 1206).trim().to_string();
    ctx.templates[index].file = ctl_text(hwnd, 1208).trim().to_string();
    ctx.templates[index].type_ = combo_index(hwnd, 1200)
        .and_then(|i| ctx.temp_types.get(i).cloned())
        .unwrap_or_default();
    ctx.templates[index].snippet = get_check(hwnd, 1210);
}

unsafe fn template_add(ctx: &mut NativeCtx, hwnd: HWND) {
    let Some(title) = ask_text(ctx.shell, hwnd, "新建模板", "") else {
        return;
    };
    let title = if title.is_empty() { "新模板".to_string() } else { title };
    let type_ = ctx.temp_types.first().cloned().unwrap_or_default();
    ctx.templates.push(TemplateEntry {
        title,
        type_,
        file: String::new(),
        snippet: false,
    });
    ctx.dirty = true;
    let index = ctx.templates.len() - 1;
    template_refresh(ctx, hwnd, Some(index));
    template_show(ctx, hwnd);
    ctx.sel = index;
    if let Ok(edit) = GetDlgItem(Some(hwnd), 1206) {
        let _ = SetFocus(Some(edit));
    }
}

unsafe fn template_remove(ctx: &mut NativeCtx, hwnd: HWND) {
    let Some(index) = lv_selected(hwnd, 1201) else {
        return;
    };
    if index >= ctx.templates.len() {
        return;
    }
    ctx.templates.remove(index);
    ctx.dirty = true;
    let select = if ctx.templates.is_empty() {
        None
    } else {
        Some(index.min(ctx.templates.len() - 1))
    };
    template_refresh(ctx, hwnd, select);
    if let Some(index) = select {
        template_show(ctx, hwnd);
        ctx.sel = index;
    } else {
        set_ctl_text(hwnd, 1206, "");
        set_ctl_text(hwnd, 1208, "");
        set_check(hwnd, 1210, false);
    }
}

unsafe fn template_move(ctx: &mut NativeCtx, hwnd: HWND, delta: i32) {
    let Some(index) = lv_selected(hwnd, 1201) else {
        return;
    };
    let target = index as i32 + delta;
    if target < 0 || target as usize >= ctx.templates.len() {
        return;
    }
    // The edits on screen belong to the row before it moves.
    template_edit(ctx, hwnd, index);
    ctx.templates.swap(index, target as usize);
    ctx.dirty = true;
    template_refresh(ctx, hwnd, Some(target as usize));
    template_show(ctx, hwnd);
    ctx.sel = target as usize;
}

unsafe fn template_load(ctx: &mut NativeCtx, hwnd: HWND) {
    let ini = IniText::load(&template_ini_path());
    ctx.templates = load_template_entries(&ini);
    ctx.temp_types = load_template_types(&ctx.templates);
    ctx.dirty = false;
    combo_fill_owned(hwnd, 1200, &ctx.temp_types, 0);
    lv_columns(hwnd, 1201, &[("标题", 0)]);
    lv_ext(hwnd, 1201, LVS_EX_FULLROWSELECT);
    for entry in &ctx.templates {
        lv_add(hwnd, 1201, &entry.title, 0);
    }
    set_check(
        hwnd,
        1211,
        read_bool(&ini, "Temp", "UseTempFormat", false),
    );
    ctx.sel = usize::MAX;
    if !ctx.templates.is_empty() {
        lv_select(hwnd, 1201, 0);
        template_show(ctx, hwnd);
        ctx.sel = 0;
    }
}

unsafe fn template_apply(ctx: &mut NativeCtx, hwnd: HWND) -> bool {
    if !ctx.dirty {
        return false;
    }
    // The row being edited may not have seen EN_KILLFOCUS yet.
    if let Some(index) = lv_selected(hwnd, 1201) {
        if index < ctx.templates.len() {
            template_edit(ctx, hwnd, index);
        }
    }
    let path = template_ini_path();
    let mut ini = IniText::load(&path);
    let count = ctx.templates.len();
    let use_format = get_check(hwnd, 1211);
    ini.replace_section(
        "Temp",
        &[
            ("Count".to_string(), count.to_string()),
            ("UseTempFormat".to_string(), bool_text(use_format).to_string()),
        ],
    );
    for (index, entry) in ctx.templates.iter().enumerate() {
        ini.replace_section(
            &format!("Temp{index}"),
            &[
                ("Title".to_string(), entry.title.clone()),
                ("Type".to_string(), entry.type_.clone()),
                ("File".to_string(), entry.file.clone()),
                ("Snippet".to_string(), bool_text(entry.snippet).to_string()),
            ],
        );
    }
    for name in ini.sections_with_prefix("Temp") {
        let suffix = &name[4..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            if suffix.parse::<usize>().map(|n| n >= count).unwrap_or(false) {
                ini.remove_section(&name);
            }
        }
    }
    ini.save(&path);
    ctx.dirty = false;
    true
}

// ---------------------------------------------------------------------------
// Web预览 (272)
// ---------------------------------------------------------------------------

unsafe fn web_enable(hwnd: HWND) {
    let other = combo_data(hwnd, 1204) == 100;
    if let Ok(edit) = GetDlgItem(Some(hwnd), 1205) {
        let _ = EnableWindow(edit, other);
    }
    if let Ok(button) = GetDlgItem(Some(hwnd), 1206) {
        let _ = EnableWindow(button, other);
    }
}

fn web_entries(ini: &IniText) -> Vec<WebEntry> {
    let count = read_u32(ini, "WebHost", "Count", 0) as usize;
    let mut out = Vec::new();
    for index in 0..count {
        let host = ini
            .get("WebHost", &format!("Host{index}"))
            .unwrap_or_default();
        let dir = ini
            .get("WebHost", &format!("Dir{index}"))
            .unwrap_or_default();
        if !host.trim().is_empty() && !dir.trim().is_empty() {
            out.push(WebEntry { host, dir });
        }
    }
    out
}

unsafe fn web_load(ctx: &mut NativeCtx, hwnd: HWND) {
    let sh = &*ctx.shell;
    ctx.web = web_entries(&sh.ini);
    lv_columns(hwnd, 1203, &[("主机", 0), ("目录", 0)]);
    lv_ext(hwnd, 1203, LVS_EX_FULLROWSELECT);
    for entry in &ctx.web {
        lv_add2(hwnd, 1203, &entry.host, &entry.dir, 0);
    }
    if !ctx.web.is_empty() {
        lv_select(hwnd, 1203, 0);
    }
    combo_fill(
        hwnd,
        1204,
        BROWSER_ITEMS,
        read_u32(&sh.ini, "Browser", "Current", 1),
    );
    set_ctl_text(
        hwnd,
        1205,
        &sh.ini.get("Browser", "Other").unwrap_or_default(),
    );
    web_enable(hwnd);
    ctx.dirty = false;
}

unsafe fn web_edit(ctx: &mut NativeCtx, hwnd: HWND, index: Option<usize>) {
    let (host, dir) = match index {
        Some(i) if i < ctx.web.len() => (ctx.web[i].host.clone(), ctx.web[i].dir.clone()),
        _ => (String::new(), String::new()),
    };
    let Some((host, dir)) = host_dialog(ctx.shell, hwnd, &host, &dir) else {
        return;
    };
    match index {
        Some(i) if i < ctx.web.len() => {
            ctx.web[i] = WebEntry {
                host: host.clone(),
                dir: dir.clone(),
            };
            lv_set_subtext(hwnd, 1203, i, 0, &host);
            lv_set_subtext(hwnd, 1203, i, 1, &dir);
            lv_select(hwnd, 1203, i);
        }
        _ => {
            ctx.web.push(WebEntry { host, dir });
            let row = ctx.web.len() - 1;
            lv_add2(hwnd, 1203, &ctx.web[row].host, &ctx.web[row].dir, 0);
            lv_select(hwnd, 1203, row);
        }
    }
    ctx.dirty = true;
}

unsafe fn web_remove(ctx: &mut NativeCtx, hwnd: HWND) {
    let Some(index) = lv_selected(hwnd, 1203) else {
        return;
    };
    if index >= ctx.web.len() {
        return;
    }
    ctx.web.remove(index);
    lv_delete(hwnd, 1203, index);
    let count = lv_count(hwnd, 1203);
    if count > 0 {
        lv_select(hwnd, 1203, index.min(count - 1));
    }
    ctx.dirty = true;
}

unsafe fn web_apply(sh: &mut Shell, ctx: &mut NativeCtx, hwnd: HWND) -> bool {
    let current = combo_data(hwnd, 1204);
    let other = ctl_text(hwnd, 1205).trim().to_string();
    let rows = web_entries(&sh.ini);
    let same_rows = rows.len() == ctx.web.len()
        && rows
            .iter()
            .zip(ctx.web.iter())
            .all(|(a, b)| a.host == b.host && a.dir == b.dir);
    let same_browser = read_u32(&sh.ini, "Browser", "Current", 1) == current
        && sh.ini.get("Browser", "Other").unwrap_or_default().trim() == other;
    if same_rows && same_browser && !ctx.dirty {
        return false;
    }
    let mut section = vec![("Count".to_string(), ctx.web.len().to_string())];
    for (index, entry) in ctx.web.iter().enumerate() {
        section.push((format!("Host{index}"), entry.host.clone()));
        section.push((format!("Dir{index}"), entry.dir.clone()));
    }
    sh.ini.replace_section("WebHost", &section);
    sh.ini.set("Browser", "Current", &current.to_string());
    sh.ini.set("Browser", "Other", &other);
    ctx.dirty = false;
    true
}

/// The language list sorts its own rows, so a row number is not an index into
/// `ctx.langs`. Every row carries `index + 1` as item data; 0 (or an error)
/// means a row without a language behind it.
fn language_index_from_item_data(data: isize, len: usize) -> Option<usize> {
    usize::try_from(data)
        .ok()?
        .checked_sub(1)
        .filter(|&index| index < len)
}

/// The row that shows `wanted`, searched by item data rather than by number.
fn language_row_for_item(rows: &[Option<usize>], wanted: usize) -> usize {
    rows.iter()
        .position(|&index| index == Some(wanted))
        .unwrap_or(0)
}

/// Which `ctx.langs` entry `[Setting] Language` names. `None` — an empty or
/// missing key — is the follow-system row at index 0; every real language
/// starts at 1 and carries its LCID in `stem`. A written value whose table
/// has since disappeared has no row, so the page highlights the language
/// actually in force instead of claiming a table the program did not load.
fn language_index_of(items: &[LangItem], code: u32) -> Option<usize> {
    items
        .iter()
        .position(|item| item.stem.parse::<u32>().ok() == Some(code))
}

fn language_index_for(items: &[LangItem], configured: Option<u32>, loaded: Option<u32>) -> usize {
    match configured {
        None => 0,
        Some(code) => language_index_of(items, code)
            .or_else(|| loaded.and_then(|code| language_index_of(items, code)))
            .unwrap_or(0),
    }
}

unsafe fn language_item_at(ctx: &NativeCtx, hwnd: HWND, row: usize) -> Option<usize> {
    let Ok(list) = GetDlgItem(Some(hwnd), 1200) else {
        return None;
    };
    let data = SendMessageW(list, LB_GETITEMDATA, Some(WPARAM(row)), None).0;
    language_index_from_item_data(data, ctx.langs.len())
}

unsafe fn language_show(ctx: &NativeCtx, hwnd: HWND) {
    if let Some(item) = language_item_at(ctx, hwnd, ctx.sel).and_then(|i| ctx.langs.get(i)) {
        set_ctl_text(hwnd, 1202, &item.author);
        set_ctl_text(hwnd, 1203, &item.home);
    }
}

unsafe fn language_load(ctx: &mut NativeCtx, hwnd: HWND) {
    // Row 0 is not a language file: its empty `stem` clears `[Setting]
    // Language`, which is how startup goes back to following the system UI
    // language. It rides the sorted listbox as an ordinary item with item
    // data, so it has no fixed position among the real languages.
    let follow = crate::win32::lang::follow_system_caption();
    ctx.langs = std::iter::once(LangItem {
        stem: String::new(),
        name: follow,
        author: String::new(),
        home: String::new(),
    })
    .chain(crate::win32::lang::available().into_iter().map(|item| LangItem {
        stem: item.code.to_string(),
        name: item.name,
        author: item.author,
        home: item.home,
    }))
    .collect();
    let configured = ctx
        .shell
        .as_ref()
        .and_then(|sh| (*sh).ini.get("Setting", "Language"))
        .unwrap_or_default();
    // A written value may be a name (`zh-CN`), so the same parser the loader
    // uses decides which row to highlight. Empty/missing is the follow-system
    // row, not the table the system happened to resolve to this time.
    let wanted_index = language_index_for(
        &ctx.langs,
        crate::win32::lang::parse_language_value(&configured),
        crate::win32::lang::current_code(),
    );
    let Ok(list) = GetDlgItem(Some(hwnd), 1200) else {
        return;
    };
    let _ = SendMessageW(list, LB_RESETCONTENT, None, None);
    for (index, item) in ctx.langs.iter().enumerate() {
        let wide: Vec<u16> = item.name.encode_utf16().chain([0]).collect();
        let row = SendMessageW(
            list,
            LB_ADDSTRING,
            None,
            Some(LPARAM(wide.as_ptr() as isize)),
        )
        .0;
        if row >= 0 {
            let _ = SendMessageW(
                list,
                LB_SETITEMDATA,
                Some(WPARAM(row as usize)),
                Some(LPARAM(index as isize + 1)),
            );
        }
    }
    let count = SendMessageW(list, LB_GETCOUNT, None, None).0.max(0) as usize;
    let rows: Vec<Option<usize>> = (0..count)
        .map(|row| language_item_at(ctx, hwnd, row))
        .collect();
    ctx.sel = language_row_for_item(&rows, wanted_index);
    ctx.sel0 = ctx.sel;
    SendMessageW(list, LB_SETCURSEL, Some(WPARAM(ctx.sel)), None);
    language_show(ctx, hwnd);
}

unsafe fn language_apply(sh: &mut Shell, ctx: &mut NativeCtx, hwnd: HWND) -> bool {
    let sel = GetDlgItem(Some(hwnd), 1200)
        .map(|list| SendMessageW(list, LB_GETCURSEL, None, None).0 as isize)
        .unwrap_or(-1);
    if sel < 0 || sel as usize == ctx.sel0 {
        return false;
    }
    let Some(item) =
        language_item_at(ctx, hwnd, sel as usize).and_then(|index| ctx.langs.get(index))
    else {
        return false;
    };
    // The follow-system row has an empty stem: `Language=` reads back as
    // absent, which is exactly the "follow the system" setting.
    sh.ini.set("Setting", "Language", &item.stem);
    // The applied row is the new baseline: switching back before 确定 has to
    // write the old value again, not count as "nothing changed".
    ctx.sel0 = sel as usize;
    // The 需重启 caption: the language table cannot be swapped under
    // live windows.
    let message = crate::win32::lang::string(15087)
        .unwrap_or("您需要重启 DogEdit 以应用该设置!");
    let text: Vec<u16> = message.encode_utf16().chain([0]).collect();
    let title: Vec<u16> = "DogEdit".encode_utf16().chain([0]).collect();
    MessageBoxW(
        Some(hwnd),
        PCWSTR(text.as_ptr()),
        PCWSTR(title.as_ptr()),
        MB_OK | MB_ICONINFORMATION,
    );
    true
}

unsafe fn native_init(ctx: &mut NativeCtx, hwnd: HWND) {
    let sh = &mut *ctx.shell;
    set_font_deep(hwnd, sh.font);
    chrome::dark_theme_children(hwnd, sh.dark);
    if sh.dark {
        plain_field(hwnd);
    }
    localize_page(hwnd, ctx.id);
    // This implementation has no strip, so the same glyphs ride as captions.
    let glyphs: &[(i32, &str)] = match ctx.id {
        207 => &[(1203, "←"), (1204, "→"), (1205, "≡"), (1206, "▲"), (1207, "▼")],
        296 => &[(1201, "▲"), (1202, "▼")],
        213 => &[(1204, "▲"), (1205, "▼"), (1209, "✎")],
        _ => &[],
    };
    for &(id, text) in glyphs {
        set_ctl_text(hwnd, id, text);
    }
    match ctx.id {
        246 => session_load(sh, hwnd),
        215 => language_load(ctx, hwnd),
        285 => save_load(sh, hwnd),
        286 => proxy_load(sh, hwnd),
        244 => hex_load(sh, hwnd),
        224 => file_load(sh, hwnd),
        247 => print_load(sh, hwnd),
        254 => complete_load(sh, hwnd),
        296 => status_load(sh, hwnd),
        207 => {
            if let Some((himl, cell)) =
                crate::win32::toolbar::dialog_icon_list(sh.dpi.max(96) as u32)
            {
                ctx.icons = himl;
                ctx.icon_cell = cell;
            }
            toolbar_load(sh, hwnd);
        }
        213 => template_load(ctx, hwnd),
        272 => web_load(ctx, hwnd),
        _ => {}
    }
}

unsafe fn native_ctx(hwnd: HWND) -> Option<&'static mut NativeCtx> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut NativeCtx;
    if p.is_null() {
        None
    } else {
        Some(&mut *p)
    }
}

unsafe extern "system" fn native_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp.0);
            let ctx = &mut *(lp.0 as *mut NativeCtx);
            native_init(ctx, hwnd);
            1
        }
        WM_MEASUREITEM => {
            let Some(ctx) = native_ctx(hwnd) else {
                return 0;
            };
            let mis = &mut *(lp.0 as *mut MEASUREITEMSTRUCT);
            // 工具条 (207) 的两只列表是 owner-draw 控件
            // (`LBS_OWNERDRAWVARIABLE`); without an item height the
            // control paints zero-height rows.
            if ctx.id != 207 || (mis.CtlID != 1201 && mis.CtlID != 1202) {
                return 0;
            }
            let sh = &*ctx.shell;
            let dc = GetDC(Some(hwnd));
            let mut size = SIZE::default();
            let sample: Vec<u16> = "Xg".encode_utf16().collect();
            let old = SelectObject(dc, HGDIOBJ(sh.font.0));
            let _ = GetTextExtentPoint32W(dc, &sample, &mut size);
            if !old.is_invalid() {
                SelectObject(dc, old);
            }
            ReleaseDC(Some(hwnd), dc);
            mis.itemHeight = ((size.cy + 4).max(16).max(ctx.icon_cell + 4)) as u32;
            1
        }
        WM_DRAWITEM => {
            let Some(ctx) = native_ctx(hwnd) else {
                return 0;
            };
            let dis = &*(lp.0 as *const DRAWITEMSTRUCT);
            if ctx.id != 207 || (dis.CtlID != 1201 && dis.CtlID != 1202) || dis.itemID == u32::MAX
            {
                return 0;
            }
            let sh = &*ctx.shell;
            let rc = dis.rcItem;
            let bg = if (dis.itemState.0 & ODS_SELECTED.0) != 0 {
                sh.theme.selection_bg
            } else {
                sh.theme.toolbar_field
            };
            let brush = CreateSolidBrush(to_colorref(bg));
            let _ = FillRect(dis.hDC, &rc, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            let id = SendMessageW(
                dis.hwndItem,
                LB_GETITEMDATA,
                Some(WPARAM(dis.itemID as usize)),
                None,
            )
            .0 as u32;
            let old = SelectObject(dis.hDC, HGDIOBJ(sh.font.0));
            let _ = SetTextColor(dis.hDC, to_colorref(sh.theme.toolbar_fg));
            if id == 0 {
                // A separator row: a rule through the middle, not the
                // placeholder caption the list stores.
                let y = (rc.top + rc.bottom) / 2;
                let rule = RECT {
                    left: rc.left + 6,
                    top: y,
                    right: rc.right - 6,
                    bottom: y + 1,
                };
                let line = CreateSolidBrush(to_colorref(sh.theme.toolbar_fg));
                let _ = FillRect(dis.hDC, &rule, line);
                let _ = DeleteObject(HGDIOBJ(line.0));
                if !old.is_invalid() {
                    SelectObject(dis.hDC, old);
                }
                return 1;
            }
            // The command's own glyph, from the sheet the bar draws with, so
            // a list row shows the button the user would get.
            let mut text_x = rc.left + 4;
            if ctx.icons.0 != 0 {
                if let Some(image) = crate::win32::toolbar::image_of(id) {
                    let y = rc.top + ((rc.bottom - rc.top) - ctx.icon_cell) / 2;
                    let _ = ImageList_Draw(ctx.icons, image, dis.hDC, text_x, y, ILD_NORMAL);
                }
                text_x += ctx.icon_cell + 6;
            }
            let text_len = SendMessageW(
                dis.hwndItem,
                LB_GETTEXTLEN,
                Some(WPARAM(dis.itemID as usize)),
                None,
            )
            .0
            .max(0) as usize;
            let mut buf = vec![0u16; text_len + 1];
            SendMessageW(
                dis.hwndItem,
                LB_GETTEXT,
                Some(WPARAM(dis.itemID as usize)),
                Some(LPARAM(buf.as_mut_ptr() as isize)),
            );
            let mut size = SIZE::default();
            let _ = GetTextExtentPoint32W(dis.hDC, &buf[..text_len], &mut size);
            let text_y = rc.top + ((rc.bottom - rc.top) - size.cy).max(0) / 2;
            let _ = ExtTextOutW(
                dis.hDC,
                text_x,
                text_y,
                ETO_CLIPPED,
                Some(&rc),
                PCWSTR(buf.as_ptr()),
                text_len as u32,
                None,
            );
            if !old.is_invalid() {
                SelectObject(dis.hDC, old);
            }
            1
        }
        WM_COMMAND => {
            let Some(ctx) = native_ctx(hwnd) else {
                return 0;
            };
            let id = (wp.0 & 0xFFFF) as i32;
            let code = ((wp.0 >> 16) & 0xFFFF) as u32;
            match ctx.id {
                246 if id == 1205 => {
                    if let Some(folder) = browse_folder(hwnd) {
                        set_ctl_text(hwnd, 1204, &folder);
                    }
                    1
                }
                215 if id == 1200 && code == LBN_SELCHANGE => {
                    let list = GetDlgItem(Some(hwnd), 1200);
                    if let Ok(list) = list {
                        let sel = SendMessageW(list, LB_GETCURSEL, None, None).0 as isize;
                        if sel >= 0 {
                            ctx.sel = sel as usize;
                            language_show(ctx, hwnd);
                        }
                    }
                    1
                }
                285 if code == 0 && (id == 1200 || id == 1211) => {
                    save_enable(hwnd);
                    1
                }
                285 if id == 1206 => {
                    if let Some(folder) = browse_folder(hwnd) {
                        set_ctl_text(hwnd, 1212, &folder);
                    }
                    1
                }
                286 if code == 0 && (1200..=1202).contains(&id) => {
                    proxy_enable(hwnd);
                    1
                }
                213 if id == 1202 => {
                    template_add(ctx, hwnd);
                    1
                }
                213 if id == 1203 => {
                    template_remove(ctx, hwnd);
                    1
                }
                213 if id == 1204 => {
                    let ctx = unsafe { &mut *(ctx as *mut NativeCtx) };
                    template_move(ctx, hwnd, -1);
                    1
                }
                213 if id == 1205 => {
                    let ctx = unsafe { &mut *(ctx as *mut NativeCtx) };
                    template_move(ctx, hwnd, 1);
                    1
                }
                213 if id == 1206 && code == 0x300 => {
                    template_sync(ctx, hwnd);
                    1
                }
                213 if id == 1208 && code == 0x300 => {
                    template_sync(ctx, hwnd);
                    1
                }
                213 if id == 1210 => {
                    template_sync(ctx, hwnd);
                    1
                }
                213 if id == 1200 && code == 1 => {
                    template_sync(ctx, hwnd);
                    1
                }
                213 if id == 1207 => {
                    let dir = app_dir().join("template");
                    if let Some(path) = browse_file_in(hwnd, Some(&dir)) {
                        let file = Path::new(&path)
                            .strip_prefix(&dir)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or(path);
                        set_ctl_text(hwnd, 1208, &file);
                        template_sync(ctx, hwnd);
                    }
                    1
                }
                213 if id == 1209 => {
                    let file = ctl_text(hwnd, 1208);
                    if !file.trim().is_empty() {
                        let path = template_file_path(file.trim());
                        let frame = (*ctx.shell).frame;
                        crate::win32::window::open_path_from_dialog(
                            frame,
                            &path.to_string_lossy(),
                        );
                    }
                    1
                }
                272 if id == 1200 => {
                    let ctx = unsafe { &mut *(ctx as *mut NativeCtx) };
                    web_edit(ctx, hwnd, None);
                    1
                }
                272 if id == 1201 => {
                    let ctx = unsafe { &mut *(ctx as *mut NativeCtx) };
                    let index = lv_selected(hwnd, 1203);
                    web_edit(ctx, hwnd, index);
                    1
                }
                272 if id == 1202 => {
                    let ctx = unsafe { &mut *(ctx as *mut NativeCtx) };
                    web_remove(ctx, hwnd);
                    1
                }
                272 if id == 1206 => {
                    if let Some(path) = browse_file_in(hwnd, None) {
                        set_ctl_text(hwnd, 1205, &path);
                        ctx.dirty = true;
                    }
                    1
                }
                272 if id == 1204 && code == 1 => {
                    web_enable(hwnd);
                    ctx.dirty = true;
                    1
                }
                247 if id == 1207 => {
                    print_token_menu(hwnd, 2405);
                    1
                }
                247 if id == 1208 => {
                    print_token_menu(hwnd, 2406);
                    1
                }
                254 if id == 1206 => {
                    lb_move(hwnd, 1203, 1208);
                    1
                }
                254 if id == 1207 => {
                    lb_move(hwnd, 1208, 1203);
                    1
                }
                254 if id == 1204 => {
                    lb_step(hwnd, 1208, -1);
                    1
                }
                254 if id == 1205 => {
                    lb_step(hwnd, 1208, 1);
                    1
                }
                296 if id == 1201 => {
                    lb_step_lv(hwnd, 1200, -1);
                    1
                }
                296 if id == 1202 => {
                    lb_step_lv(hwnd, 1200, 1);
                    1
                }
                207 if id == 1203 => {
                    toolbar_remove(hwnd);
                    1
                }
                207 if id == 1204 => {
                    toolbar_add(hwnd);
                    1
                }
                207 if id == 1205 => {
                    toolbar_add_separator(hwnd);
                    1
                }
                207 if id == 1206 => {
                    lb_step(hwnd, 1202, -1);
                    1
                }
                207 if id == 1207 => {
                    lb_step(hwnd, 1202, 1);
                    1
                }
                207 if id == 1209 => {
                    open_theme_dir(hwnd);
                    1
                }
                _ => 0,
            }
        }
        WM_NOTIFY => {
            let Some(ctx) = native_ctx(hwnd) else {
                return 0;
            };
            let hdr = unsafe { &*(lp.0 as *const NMHDR) };
            if ctx.id == 213
                && hdr.code == LVN_ITEMCHANGED
                && GetDlgItem(Some(hwnd), 1201).map(|lv| lv == hdr.hwndFrom).unwrap_or(false)
            {
                if let Some(index) = lv_selected(hwnd, 1201) {
                    if index != ctx.sel {
                        template_show(ctx, hwnd);
                        ctx.sel = index;
                    }
                }
                return 1;
            }
            if ctx.id == 272
                && hdr.code == NM_DBLCLK
                && GetDlgItem(Some(hwnd), 1203).map(|lv| lv == hdr.hwndFrom).unwrap_or(false)
            {
                let index = lv_selected(hwnd, 1203);
                let ctx = unsafe { &mut *(ctx as *mut NativeCtx) };
                web_edit(ctx, hwnd, index);
                return 1;
            }
            0
        }
        WM_ERASEBKGND => {
            let Some(ctx) = native_ctx(hwnd) else {
                return 0;
            };
            let sh = &*ctx.shell;
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let _ = FillRect(HDC(wp.0 as *mut core::ffi::c_void), &rc, sh.bg);
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let Some(ctx) = native_ctx(hwnd) else {
                return 0;
            };
            let sh = &*ctx.shell;
            customdraw::ctlcolor(
                HDC(wp.0 as *mut core::ffi::c_void),
                to_colorref(sh.theme.toolbar_fg),
                to_colorref(sh.theme.toolbar_bg),
                sh.bg,
            )
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let Some(ctx) = native_ctx(hwnd) else {
                return 0;
            };
            let sh = &*ctx.shell;
            customdraw::ctlcolor(
                HDC(wp.0 as *mut core::ffi::c_void),
                to_colorref(sh.theme.toolbar_fg),
                to_colorref(sh.theme.toolbar_field),
                ctx.field,
            )
        }
        WM_CLOSE => 1,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// The shell
// ---------------------------------------------------------------------------

unsafe fn shell_this(hwnd: HWND) -> Option<&'static mut Shell> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Shell;
    if p.is_null() {
        None
    } else {
        Some(&mut *p)
    }
}

unsafe extern "system" fn shell_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp.0);
            if let Some(sh) = shell_this(hwnd) {
                sh.init(hwnd);
                1
            } else {
                0
            }
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as u32;
            let code = ((wp.0 >> 16) & 0xFFFF) as u32;
            match id {
                IDCANCEL_WORD => {
                    let _ = EndDialog(hwnd, 2);
                    1
                }
                x if x == ID_OK as u32 => {
                    if code == BN_CLICKED {
                        if let Some(sh) = shell_this(hwnd) {
                            sh.apply();
                        }
                        let _ = EndDialog(hwnd, 1);
                    }
                    1
                }
                x if x == ID_APPLY as u32 => {
                    if code == BN_CLICKED {
                        if let Some(sh) = shell_this(hwnd) {
                            sh.apply();
                        }
                    }
                    1
                }
                x if x == ID_CANCEL as u32 => {
                    if code == BN_CLICKED {
                        let _ = EndDialog(hwnd, 2);
                    }
                    1
                }
                x if x == ID_HELP as u32 => {
                    if code == BN_CLICKED {
                        if let Some(sh) = shell_this(hwnd) {
                            SendMessageW(
                                sh.frame,
                                WM_COMMAND,
                                Some(WPARAM(2010)),
                                Some(LPARAM(0)),
                            );
                        }
                    }
                    1
                }
                _ => 0,
            }
        }
        WM_NOTIFY => {
            let nm = &*(lp.0 as *const NMHDR);
            let Some(sh) = shell_this(hwnd) else {
                return 0;
            };
            if nm.hwndFrom == sh.tree && nm.code == TVN_SELCHANGEDW {
                let _ = PostMessageW(Some(hwnd), WM_PAGE_READY, WPARAM(0), LPARAM(0));
                return 1;
            }
            0
        }
        WM_PAGE_READY => {
            let Some(sh) = shell_this(hwnd) else {
                return 0;
            };
            if let Some(id) = selected_page_id(sh.tree) {
                sh.load_page(id);
            }
            1
        }
        WM_ERASEBKGND => {
            let Some(sh) = shell_this(hwnd) else {
                return 0;
            };
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let _ = FillRect(HDC(wp.0 as *mut core::ffi::c_void), &rc, sh.bg);
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let Some(sh) = shell_this(hwnd) else {
                return 0;
            };
            customdraw::ctlcolor(
                HDC(wp.0 as *mut core::ffi::c_void),
                to_colorref(sh.theme.toolbar_fg),
                to_colorref(sh.theme.toolbar_bg),
                sh.bg,
            )
        }
        WM_CLOSE => {
            let _ = EndDialog(hwnd, 2);
            1
        }
        _ => 0,
    }
}

/// Open 选项 and return the applied settings when 确定 or 应用 was pressed.
/// The category the next `show` opens on. 0 stands for the dialog's own
/// default (常规); the toolbar's 自定义… row asks for 工具条.
static INITIAL_PAGE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Opens the dialog on category `page` instead of 常规. A one-shot: the
/// pick is consumed by the next `show`, so a later plain open is unaffected.
pub(super) fn show_at(frame: HWND, theme: &Theme, page: usize) -> Option<Settings> {
    INITIAL_PAGE.store(page, std::sync::atomic::Ordering::SeqCst);
    show(frame, theme)
}

pub(super) fn show(frame: HWND, theme: &Theme) -> Option<Settings> {
    unsafe {
        if !IsWindow(Some(frame)).as_bool() {
            return None;
        }
        let dpi = GetDpiForWindow(frame).max(96) as i32;
        let mut shell = Box::new(Shell::new(frame, theme, dpi));
        let page = INITIAL_PAGE.swap(0, std::sync::atomic::Ordering::SeqCst);
        if page != 0 {
            shell.initial_page = page;
        }
        let ptr = (&mut *shell) as *mut Shell;
        let tmpl = build_shell_template();
        let result = DialogBoxIndirectParamW(
            Some(instance()),
            tmpl.as_ptr() as *const DLGTEMPLATE,
            Some(frame),
            Some(shell_proc),
            LPARAM(ptr as isize),
        );
        if result == -1 {
            return None;
        }
        shell.applied.take()
    }
}

// ---------------------------------------------------------------------------
// Registry — the two right-mouse-button rows
// ---------------------------------------------------------------------------

/// Enable/disable `HKCU\SOFTWARE\Classes\*\Shell\DogEdit` (or the Directory
/// entry)
/// `"<exe>" "%1"` and `"<exe>" -g "%1"`.
unsafe fn write_shell_entry(directory: bool, enable: bool) {
    if !enable {
        // it never touches the container itself.
        let (parent, command) = if directory {
            (
                "SOFTWARE\\Classes\\Directory\\shell\0",
                "DogEdit\\Command\0",
            )
        } else {
            ("SOFTWARE\\Classes\\*\\Shell\0", "DogEdit\\Command\0")
        };
        let parent: Vec<u16> = parent.encode_utf16().collect();
        let command: Vec<u16> = command.encode_utf16().collect();
        let dogedit: Vec<u16> = "DogEdit\0".encode_utf16().collect();
        let mut h = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(parent.as_ptr()),
            None,
            windows::Win32::System::Registry::KEY_ALL_ACCESS,
            &mut h,
        ) == ERROR_SUCCESS
        {
            let _ = RegDeleteKeyW(h, PCWSTR(command.as_ptr()));
            let _ = RegDeleteKeyW(h, PCWSTR(dogedit.as_ptr()));
            let _ = RegCloseKey(h);
        }
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let exe = exe.display().to_string();
    let command = if directory {
        format!("\"{exe}\" -g \"%1\"")
    } else {
        format!("\"{exe}\" \"%1\"")
    };
    let label = if directory {
        "用 DogEdit 在文件中查找"
    } else {
        "用 DogEdit 打开"
    };
    let (command_key, entry_key) = if directory {
        (
            "SOFTWARE\\Classes\\Directory\\shell\\DogEdit\\command\0",
            "SOFTWARE\\Classes\\Directory\\shell\\DogEdit\0",
        )
    } else {
        (
            "SOFTWARE\\Classes\\*\\Shell\\DogEdit\\Command\0",
            "SOFTWARE\\Classes\\*\\Shell\\DogEdit\0",
        )
    };
    let command_key: Vec<u16> = command_key.encode_utf16().collect();
    let entry_key: Vec<u16> = entry_key.encode_utf16().collect();
    let command_w: Vec<u16> = command.encode_utf16().chain([0]).collect();
    let label_w: Vec<u16> = label.encode_utf16().chain([0]).collect();
    let mut h = HKEY::default();
    if RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(command_key.as_ptr()), &mut h) == ERROR_SUCCESS {
        let _ = RegSetValueW(
            h,
            PCWSTR::null(),
            REG_SZ,
            PCWSTR(command_w.as_ptr()),
            (command_w.len() * 2) as u32,
        );
        let _ = RegCloseKey(h);
    }
    let mut h = HKEY::default();
    if RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(entry_key.as_ptr()), &mut h) == ERROR_SUCCESS {
        let _ = RegSetValueW(
            h,
            PCWSTR::null(),
            REG_SZ,
            PCWSTR(label_w.as_ptr()),
            (label_w.len() * 2) as u32,
        );
        let _ = RegCloseKey(h);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ini(lines: &[&str]) -> IniText {
        let mut text = String::new();
        for l in lines {
            text.push_str(l);
            text.push('\n');
        }
        IniText(dg_core::ini::Ini::parse(&text))
    }

    fn at(px: &[u8], size: usize, x: usize, y: usize) -> [u8; 4] {
        let o = (y * size + x) * 4;
        px[o..o + 4].try_into().unwrap()
    }

    fn ink(px: &[u8], size: usize) -> usize {
        (0..size * size).filter(|i| px[i * 4 + 3] != 0 && px[i * 4] == 0).count()
    }

    #[test]
    fn sprites_draw_the_box_and_the_value_glyphs() {
        let unchecked = sprite(Sprite::Unchecked, 16);
        let checked = sprite(Sprite::Checked, 16);
        let edit = sprite(Sprite::Edit, 16);
        let list = sprite(Sprite::List, 16);
        let key = [0, 0, 0, 0];
        let paper = [PAPER[2], PAPER[1], PAPER[0], 0xFF];
        let ink_ = [INK[2], INK[1], INK[0], 0xFF];
        // A check box is inset and leaves the strip's spare columns open.
        for px in [&unchecked, &checked] {
            assert_eq!(at(px, 16, 0, 0), key, "outside the box");
            assert_eq!(at(px, 16, 14, 14), key, "spare column");
            assert_eq!(at(px, 16, 1, 1), ink_, "frame");
            assert_eq!(at(px, 16, 3, 3), paper, "interior");
        }
        assert!(ink(&checked, 16) > ink(&unchecked, 16), "the check is drawn");
        // A value row fills the cell and draws its glyph inside the frame.
        for px in [&edit, &list] {
            assert_eq!(at(px, 16, 0, 0), ink_, "cell frame");
            assert_eq!(at(px, 16, 1, 1), paper, "cell face");
            assert_eq!(at(px, 16, 13, 13), paper, "below the glyph");
        }
        assert_eq!(at(&edit, 16, 8, 3), ink_, "the pencil shaft");
        assert_eq!(at(&edit, 16, 4, 10), ink_, "the pencil tip");
        // The list marker adds a rule with a drop below it on the right.
        assert_eq!(at(&list, 16, 12, 7), ink_, "the rule");
        assert_eq!(at(&list, 16, 10, 9), ink_, "the drop");
        assert_eq!(at(&list, 16, 12, 11), paper, "under the drop");
    }

    #[test]
    fn sprites_are_the_same_shape_at_both_sizes() {
        for kind in [Sprite::Unchecked, Sprite::Checked, Sprite::Edit, Sprite::List] {
            let small = sprite(kind, 16);
            let large = sprite(kind, 24);
            assert_eq!(at(&small, 16, 0, 0), at(&large, 24, 0, 0), "corner {kind:?}");
            assert!(
                ink(&large, 24) * 256 >= ink(&small, 16) * 576 / 2,
                "the glyph scales, {kind:?}"
            );
        }
    }

    #[test]
    fn ini_reads_case_insensitively_and_inserts_into_the_section() {
        let mut f = ini(&["[Edit]", "CaretStyle=1", "", "[Setting]", "RecentDoc=16"]);
        assert_eq!(f.get("setting", "recentdoc").as_deref(), Some("16"));
        f.set("Setting", "StartPageRecent", "N");
        f.set("Setting", "MulInst", "Y");
        assert_eq!(f.get("Setting", "StartPageRecent").as_deref(), Some("N"));
        assert_eq!(f.get("Setting", "MulInst").as_deref(), Some("Y"));
        // Other sections and keys keep their order and text.
        assert!(f.0.to_text().starts_with("[Edit]\nCaretStyle=1\n"));
        assert_eq!(f.get("Edit", "CaretStyle").as_deref(), Some("1"));
    }

    #[test]
    fn ini_creates_a_missing_section() {
        let mut f = ini(&["[Edit]", "CaretStyle=1"]);
        f.set("DockWindow", "Theme", "Y");
        assert_eq!(f.get("DockWindow", "Theme").as_deref(), Some("Y"));
        assert!(f.0.to_text().contains("[DockWindow]"));
    }

    #[test]
    fn booleans_follow_the_ini_spelling() {
        let f = ini(&["[Setting]", "TearOff=Y", "SysBG=n", "MulInst=1"]);
        assert!(read_bool(&f, "Setting", "TearOff", false));
        assert!(!read_bool(&f, "Setting", "SysBG", true));
        assert!(read_bool(&f, "Setting", "MulInst", false));
        assert!(read_bool(&f, "Setting", "Missing", true));
    }

    #[test]
    fn general_rows_are_the_five_pages_order_from_the_screenshot() {
        let ids: Vec<i32> = PAGE_ROWS.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            vec![
                ID_ROW_RECENT,
                ID_ROW_START_RECENT,
                ID_ROW_MULINST,
                ID_ROW_SHELL,
                ID_ROW_SHELL_FIND,
                ID_ROW_TEAROFF,
                ID_ROW_SYSBG,
                ID_ROW_DOCK_THEME,
                ID_ROW_HIDE_PATH,
                ID_ROW_FONT,
                ID_ROW_UPDATE,
                ID_ROW_FLASH,
                ID_ROW_PROGRESS,
                ID_ROW_SOUND,
                ID_ROW_SOUND_FILE,
                ID_ROW_FAIL_SOUND_FILE
            ]
        );
    }

    #[test]
    fn value_rows_append_their_value_to_the_label() {
        let row = Row {
            id: ID_ROW_UPDATE,
            label: "检查最新版本".into(),
            kind: RowKind::List,
            check: false,
            value: 0,
            text: String::new(),
            items: UPDATE_ITEMS,
        };
        assert_eq!(row_text(&row), "检查最新版本 一天一次");
        let row = Row {
            id: ID_ROW_FONT,
            label: "界面使用的字体(需重启)".into(),
            kind: RowKind::Font,
            check: false,
            value: 0,
            text: String::new(),
            items: FONT_ITEMS,
        };
        assert_eq!(row_text(&row), "界面使用的字体(需重启) 系统默认");
    }

    #[test]
    fn every_check_row_starts_from_its_setting() {
        let settings = Settings {
            recent_doc: 16,
            start_page_recent: true,
            mul_inst: false,
            shell_app: true,
            shell_find: false,
            tear_off: true,
            sys_bg: true,
            dock_theme: false,
            hide_file_path: false,
            ui_font: FONT_IDS[3],
            auto_update: 1,
            flash_taskbar: true,
            progress: true,
            notify_sound: true,
            sound_file: String::new(),
            fail_sound_file: String::new(),
        };
        assert!(settings.start_page_recent);
        assert!(!settings.mul_inst);
        assert_eq!(UPDATE_ITEMS[(settings.auto_update - 1) as usize], "一天一次");
        // The row shows the caption of the stored menu id.
        assert_eq!(FONT_ITEMS[font_index(settings.ui_font)], "Segoe UI, 9pt");
        assert_eq!(font_index(0), 0);
        assert_eq!(font_index(99), 0);
    }

    #[test]
    fn the_notify_rows_are_the_ports_own_five_rows() {
        // They are the tail of 常规: three switches, then the two sound files.
        let tail: Vec<i32> = PAGE_ROWS
            .iter()
            .map(|(id, _)| *id)
            .skip(PAGE_ROWS.len() - 5)
            .collect();
        assert_eq!(
            tail,
            vec![
                ID_ROW_FLASH,
                ID_ROW_PROGRESS,
                ID_ROW_SOUND,
                ID_ROW_SOUND_FILE,
                ID_ROW_FAIL_SOUND_FILE
            ]
        );
        for id in tail {
            assert!(
                PAGE_ROWS
                    .iter()
                    .any(|(row, text)| *row == id && !text.is_empty()),
                "row {id} has no caption"
            );
        }
        // The sound files are paths, so those rows open a file dialog rather
        // than an inline editor.
        assert!(general_row_kind(ID_ROW_SOUND_FILE) == RowKind::File);
        assert!(general_row_kind(ID_ROW_FAIL_SOUND_FILE) == RowKind::File);
        assert!(general_row_kind(ID_ROW_FLASH) == RowKind::Check);
        assert!(general_row_kind(ID_ROW_PROGRESS) == RowKind::Check);
        assert!(general_row_kind(ID_ROW_SOUND) == RowKind::Check);
    }

    #[test]
    fn the_notify_settings_default_to_on_and_follow_the_ini() {
        let defaults = Settings::load(&ini(&[]));
        assert!(defaults.flash_taskbar);
        assert!(defaults.progress);
        assert!(defaults.notify_sound);
        assert_eq!(defaults.sound_file, "");
        assert_eq!(defaults.fail_sound_file, "");

        let read = Settings::load(&ini(&[
            "[Notify]",
            "FlashTaskbar=N",
            "TaskbarProgress=N",
            "Sound=n",
            "SoundFile=D:\\sound\\done.wav",
            "FailSoundFile=D:\\sound\\failed.wav",
        ]));
        assert!(!read.flash_taskbar);
        assert!(!read.progress);
        assert!(!read.notify_sound);
        assert_eq!(read.sound_file, "D:\\sound\\done.wav");
        assert_eq!(read.fail_sound_file, "D:\\sound\\failed.wav");
        assert_eq!(read.recent_doc, 16);
        assert!(read.start_page_recent);
    }

    #[test]
    fn the_sound_row_says_when_it_is_the_system_beep() {
        let mut row = Row {
            id: ID_ROW_SOUND_FILE,
            label: "提示音文件:".into(),
            kind: RowKind::File,
            check: false,
            value: 0,
            text: String::new(),
            items: &[],
        };
        // An empty value is not a gap in the row: it names what plays instead.
        assert!(row_text(&row).ends_with("系统提示音"));
        assert!(row_image(&row) == IMG_EDIT);
        row.text = "C:\\done.wav".into();
        assert!(row_text(&row).ends_with("C:\\done.wav"));
    }

    /// Empty `[Setting] Language` is a setting of its own, not "whatever
    /// table the system happened to load". The follow-system row has no LCID
    /// and writes an empty value, which `Ini::get` reads back as absent.
    #[test]
    fn a_follow_system_row_is_a_setting_not_a_language() {
        let item = |stem: &str, name: &str| LangItem {
            stem: stem.to_string(),
            name: name.to_string(),
            author: String::new(),
            home: String::new(),
        };
        let items = vec![
            item("", "Follow system"),
            item("1033", "English"),
            item("2052", "中文(简体)"),
        ];
        assert_eq!(
            language_index_for(&items, None, Some(1033)),
            0,
            "an empty key must not highlight the table the system resolved to"
        );
        assert_eq!(language_index_for(&items, Some(1033), None), 1);
        assert_eq!(language_index_for(&items, Some(2052), None), 2);
        // zh-TW has no table: the loader fell back to English, so the page
        // highlights what is actually on screen.
        assert_eq!(language_index_for(&items, Some(1028), Some(1033)), 1);
        assert_eq!(language_index_for(&items, Some(1028), None), 0);
        let mut ini = dg_core::ini::Ini::parse("[Setting]\nLanguage=2052\n");
        ini.set("Setting", "Language", &items[0].stem);
        assert_eq!(ini.get("Setting", "Language"), None);
    }

    #[test]
    fn language_rows_are_looked_up_by_item_data_not_by_number() {
        assert_eq!(language_index_from_item_data(1, 2), Some(0));
        assert_eq!(language_index_from_item_data(2, 2), Some(1));
        assert_eq!(language_index_from_item_data(0, 2), None, "no data");
        assert_eq!(language_index_from_item_data(-1, 2), None, "LB_ERR");
        assert_eq!(language_index_from_item_data(3, 2), None, "out of range");
        // An en-US system sorts the captions the other way round from the
        // code order 1033, 2052: Chinese is row 0, English is row 1.
        let rows = [Some(1), Some(0)];
        assert_eq!(language_row_for_item(&rows, 0), 1);
        assert_eq!(language_row_for_item(&rows, 1), 0);
        assert_eq!(language_row_for_item(&rows, 7), 0, "unknown falls back");
    }

    /// The 语言 page's listbox carries LBS_SORT: LB_ADDSTRING puts a caption
    /// where the control's sort says it goes, so a row number is not the
    /// language's index. Item data is the link that survives the reordering.
    #[test]
    fn a_sorted_language_list_keeps_each_language_with_its_row() {
        unsafe {
            let parent = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                PCWSTR::null(),
                WINDOW_STYLE(0x8000_0000), // WS_POPUP
                0,
                0,
                200,
                200,
                None,
                None,
                Some(instance()),
                None,
            )
            .expect("test parent");
            let list = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("LISTBOX"),
                PCWSTR::null(),
                WINDOW_STYLE(0x50a1_0103), // the 语言 page's listbox style
                0,
                0,
                150,
                150,
                Some(parent),
                Some(HMENU(1200 as *mut core::ffi::c_void)),
                Some(instance()),
                None,
            )
            .expect("test list");
            for (index, name) in ["Follow system", "English", "Chinese (Simplified)"]
                .iter()
                .enumerate()
            {
                let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
                let row = SendMessageW(
                    list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(wide.as_ptr() as isize)),
                )
                .0;
                assert!(row >= 0);
                let _ = SendMessageW(
                    list,
                    LB_SETITEMDATA,
                    Some(WPARAM(row as usize)),
                    Some(LPARAM(index as isize + 1)),
                );
            }
            let count = SendMessageW(list, LB_GETCOUNT, None, None).0.max(0) as usize;
            let (mut rows, mut names) = (Vec::new(), Vec::new());
            for row in 0..count {
                let data = SendMessageW(list, LB_GETITEMDATA, Some(WPARAM(row)), None).0;
                rows.push(language_index_from_item_data(data, 3));
                let mut buf = [0u16; 64];
                let _ = SendMessageW(
                    list,
                    LB_GETTEXT,
                    Some(WPARAM(row)),
                    Some(LPARAM(buf.as_mut_ptr() as isize)),
                );
                let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                names.push(String::from_utf16_lossy(&buf[..end]));
            }
            assert_eq!(names, ["Chinese (Simplified)", "English", "Follow system"]);
            assert_eq!(rows, [Some(2), Some(1), Some(0)]);
            assert_eq!(language_row_for_item(&rows, 0), 2, "Follow system is row 2");
            assert_eq!(language_row_for_item(&rows, 1), 1, "English is row 1");
            let _ = DestroyWindow(parent);
        }
    }
}
