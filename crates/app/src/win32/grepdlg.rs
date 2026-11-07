//! 在文件中查找和替换 — the find/replace-in-files dialog (`cm_find_infiles`
//! 1408, Ctrl+Shift+F).
//!
//! One singleton dialog is created on first use and re-shown afterwards; the
//! window procedure keeps the corner hit codes so only the side edges resize,
//! and `WM_GETMINMAXINFO` pins the minimum size.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dpi::scale;
use crate::win32::dlgtpl::{self, Class, Item};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LRESULT, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BACKGROUND_MODE, CreateFontIndirectW, CreateSolidBrush, DrawTextW, DT_CENTER, DT_NOCLIP,
    DT_SINGLELINE, DT_VCENTER, DeleteObject, FillRect, FONT_CHARSET, GetDC, HBRUSH, HDC, HFONT,
    HGDIOBJ, InvalidateRect, LOGFONTW, MapWindowPoints, RedrawWindow, ReleaseDC, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT, RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE,
    RDW_UPDATENOW,
};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::Controls::{
    BP_PUSHBUTTON, CloseThemeData, DRAWITEMSTRUCT, EM_GETSEL, EM_REPLACESEL, EM_SCROLLCARET,
    EM_SETSEL, GetThemeColor, OpenThemeData, PBS_NORMAL, PBS_PRESSED, TMT_TEXTCOLOR,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, SetFocus, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::Shell::{
    BFFM_INITIALIZED, BFFM_SETSELECTIONW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW,
    SHBrowseForFolderW, SHGetPathFromIDListW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BM_GETCHECK, BM_SETCHECK, SPI_GETICONTITLELOGFONT,
    SystemParametersInfoW,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, CB_ADDSTRING, CB_GETCOUNT, CB_GETCURSEL, CB_GETITEMDATA,
    CB_INSERTSTRING, CB_LIMITTEXT, CB_RESETCONTENT, CB_SETCURSEL, CB_SETDROPPEDWIDTH,
    CB_SETEDITSEL, CB_SETITEMDATA, CreateDialogIndirectParamW, CreatePopupMenu,
    DefWindowProcW, DestroyMenu, DestroyWindow, DLGTEMPLATE, GA_ROOT, GetAncestor,
    GetClientRect, GetDlgItem, GetParent, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GWLP_USERDATA, GWLP_WNDPROC, GWL_EXSTYLE, GWL_ID,
    GWL_STYLE, HMENU, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTLEFT, HTNOWHERE, HTRIGHT, HTTOP,
    HTTOPLEFT, HTTOPRIGHT, IDOK, IsDialogMessageW, IsWindowVisible,
    MESSAGEBOX_STYLE, MessageBoxW, MB_ICONWARNING, MB_OKCANCEL, MF_POPUP, MF_SEPARATOR, MF_STRING,
    MINMAXINFO, MSG, PostMessageW, SendMessageW, SetDlgItemTextW, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER,
    TPM_NONOTIFY, TPM_RETURNCMD, TrackPopupMenu, WNDPROC, WM_APP,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC,
    WM_DESTROY, WM_DRAWITEM, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_INITDIALOG, WM_KEYDOWN,
    GW_CHILD, GW_HWNDNEXT, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_SETREDRAW, WM_SETFONT,
    WM_SIZE, WM_SYSKEYDOWN, WS_CLIPCHILDREN, WS_EX_TOOLWINDOW,
};
use windows::core::{PCWSTR, w};

use crate::win32::customdraw;
use crate::win32::view::instance;
use crate::win32::window::to_colorref;
use dg_render::theme::Theme;

// ---------------------------------------------------------------------------
// Control ids
// ---------------------------------------------------------------------------

const ID_FIND: i32 = 1200;
const ID_REPL: i32 = 1217;
const ID_TYPE: i32 = 1201;
const ID_EXCL: i32 = 1222;
const ID_DIR: i32 = 1202;
const ID_ENC: i32 = 1208;
const ID_ARROW_FIND: i32 = 1203;
const ID_ARROW_REPL: i32 = 1218;
const ID_ARROW_TYPE: i32 = 1211;
const ID_ARROW_EXCL: i32 = 1223;
const ID_BROWSE: i32 = 1204;
const ID_MONITOR: i32 = 1220;
const ID_UPDIR: i32 = 1221;
const ID_FIND_BTN: i32 = 1;
const ID_REPL_BTN: i32 = 1216;
const ID_CLOSE: i32 = 2;
const ID_CASE: i32 = 1206;
const ID_SUBFOLDER: i32 = 1207;
const ID_WORD: i32 = 1214;
const ID_REGEX: i32 = 1205;
const ID_INVERT: i32 = 1210;
const ID_ONLYNAME: i32 = 1212;
const ID_USEOUTPUT: i32 = 1209;
const ID_BACKUP: i32 = 1219;
const ID_LOCK: i32 = 1213;
const ID_SEP: i32 = 4000;

/// The four combos that carry an MRU, and the section each list lives in.
const MRU_COMBOS: [(i32, &str); 5] = [
    (ID_FIND, "GrepFind"),
    (ID_REPL, "GrepReplace"),
    (ID_TYPE, "GrepFileType"),
    (ID_DIR, "GrepFolder"),
    (ID_EXCL, "GrepExclude"),
];

/// The dialog's own class atoms, the way a template names them.
const ATOM_BUTTON: u16 = 0x0080;
const ATOM_STATIC: u16 = 0x0082;
const ATOM_COMBO: u16 = 0x0085;

/// The template's dialog style and extended style, byte for byte.
const DLG_STYLE: u32 = 0x90cc_08cc;
const DLG_EXSTYLE: u32 = 0x0000_0080;
const DLG_W: u16 = 352;
const DLG_H: u16 = 165;
const DLG_TITLE: &str = "在文件中查找和替换";

// ---------------------------------------------------------------------------
// The 32 template children, in template order
// ---------------------------------------------------------------------------

/// The template's items. Positions and styles come from the dialog template;
/// labels come from `zh-CN.lng`'s 在文件中查找和替换 box. Only 4000 carries its own
/// style (see the module doc); 1220/1221 are the template's own, blank
/// caption included, and draw their glyph themselves.
const ITEMS: &[Item] = &[
    Item { id: 3000, class: Class::Atom(ATOM_STATIC), style: 0x50020000, x: 7, y: 9, w: 40, h: 15, label: "查找(&T):" },
    Item { id: ID_FIND, class: Class::Atom(ATOM_COMBO), style: 0x50010242, x: 47, y: 7, w: 230, h: 30, label: "" },
    Item { id: ID_ARROW_FIND, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 279, y: 6, w: 16, h: 13, label: "4" },
    Item { id: 3004, class: Class::Atom(ATOM_STATIC), style: 0x50020000, x: 7, y: 25, w: 39, h: 13, label: "替换(&A):" },
    Item { id: ID_REPL, class: Class::Atom(ATOM_COMBO), style: 0x50010242, x: 47, y: 23, w: 230, h: 30, label: "" },
    Item { id: ID_ARROW_REPL, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 279, y: 22, w: 15, h: 13, label: "4" },
    Item { id: 3002, class: Class::Atom(ATOM_STATIC), style: 0x50020000, x: 7, y: 41, w: 40, h: 15, label: "类型(&P):" },
    Item { id: ID_TYPE, class: Class::Atom(ATOM_COMBO), style: 0x50010242, x: 47, y: 39, w: 230, h: 30, label: "" },
    Item { id: ID_ARROW_TYPE, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 279, y: 39, w: 16, h: 13, label: "4" },
    Item { id: 3005, class: Class::Atom(ATOM_STATIC), style: 0x50020000, x: 7, y: 57, w: 39, h: 11, label: "排除(&X):" },
    Item { id: ID_EXCL, class: Class::Atom(ATOM_COMBO), style: 0x50010242, x: 47, y: 55, w: 230, h: 30, label: "" },
    Item { id: ID_ARROW_EXCL, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 279, y: 55, w: 16, h: 13, label: "4" },
    Item { id: 3001, class: Class::Atom(ATOM_STATIC), style: 0x50020000, x: 7, y: 73, w: 40, h: 10, label: "文件夹(&R):" },
    Item { id: ID_DIR, class: Class::Atom(ATOM_COMBO), style: 0x50010242, x: 47, y: 71, w: 230, h: 30, label: "" },
    Item { id: ID_BROWSE, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 279, y: 71, w: 16, h: 14, label: "..." },
    Item { id: ID_MONITOR, class: Class::Atom(ATOM_BUTTON), style: 0x50011003, x: 297, y: 71, w: 24, h: 14, label: "" },
    Item { id: ID_UPDIR, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 323, y: 71, w: 24, h: 14, label: "" },
    Item { id: ID_FIND_BTN, class: Class::Atom(ATOM_BUTTON), style: 0x50010001, x: 297, y: 6, w: 50, h: 14, label: "查找(&F)" },
    Item { id: ID_REPL_BTN, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 297, y: 22, w: 50, h: 14, label: "替换" },
    Item { id: ID_CLOSE, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 297, y: 38, w: 50, h: 14, label: "关闭(&C)" },
    Item { id: ID_SUBFOLDER, class: Class::Atom(ATOM_BUTTON), style: 0x50010003, x: 7, y: 102, w: 120, h: 11, label: "包含子文件夹(&I)" },
    Item { id: ID_CASE, class: Class::Atom(ATOM_BUTTON), style: 0x50010003, x: 7, y: 86, w: 125, h: 14, label: "匹配大小写(&S)" },
    Item { id: ID_WORD, class: Class::Atom(ATOM_BUTTON), style: 0x50010003, x: 137, y: 89, w: 110, h: 11, label: "全字匹配(&W)" },
    Item { id: ID_REGEX, class: Class::Atom(ATOM_BUTTON), style: 0x50010003, x: 252, y: 87, w: 95, h: 14, label: "正则表达式(&E)" },
    Item { id: ID_INVERT, class: Class::Atom(ATOM_BUTTON), style: 0x50010003, x: 7, y: 115, w: 128, h: 14, label: "查找不匹配的行(&N)" },
    Item { id: ID_ONLYNAME, class: Class::Atom(ATOM_BUTTON), style: 0x50010003, x: 137, y: 101, w: 110, h: 13, label: "仅显示文件路径(&H)" },
    Item { id: ID_USEOUTPUT, class: Class::Atom(ATOM_BUTTON), style: 0x50010003, x: 252, y: 101, w: 90, h: 13, label: "使用输出窗口(&U)" },
    Item { id: ID_BACKUP, class: Class::Atom(ATOM_BUTTON), style: 0x50010003, x: 137, y: 118, w: 170, h: 8, label: "备份被替换的文件(&B)" },
    Item { id: 3003, class: Class::Atom(ATOM_STATIC), style: 0x50020000, x: 7, y: 133, w: 85, h: 11, label: "编码(&D):" },
    Item { id: ID_ENC, class: Class::Atom(ATOM_COMBO), style: 0x50210843, x: 7, y: 145, w: 95, h: 30, label: "" },
    Item { id: ID_SEP, class: Class::Atom(ATOM_STATIC), style: 0x5000010D, x: 7, y: 131, w: 337, h: 1, label: "" },
    Item { id: ID_LOCK, class: Class::Atom(ATOM_BUTTON), style: 0x50010003, x: 109, y: 144, w: 225, h: 15, label: "锁定主窗口(推荐)(&L)" },
];

/// The resize table, from the dialog template:
/// 16 controls plus the end marker. `SIZE_X` stretches the right edge with the
/// client, `MOVE_X` drags the left edge along with it, `MOVE_Y` the same for
/// the bottom.
const RESIZE_TABLE: &[(i32, u32)] = &[
    (ID_FIND, 0x1),
    (ID_REPL, 0x1),
    (ID_TYPE, 0x1),
    (ID_DIR, 0x1),
    (ID_EXCL, 0x1),
    (ID_ARROW_FIND, 0x4),
    (ID_MONITOR, 0x4),
    (ID_UPDIR, 0x4),
    (ID_ARROW_REPL, 0x4),
    (ID_ARROW_TYPE, 0x4),
    (ID_BROWSE, 0x4),
    (ID_ARROW_EXCL, 0x4),
    (ID_FIND_BTN, 0x4),
    (ID_REPL_BTN, 0x4),
    (ID_CLOSE, 0x4),
    (ID_SEP, 0x9),
];

/// Build the DLGTEMPLATEEX byte buffer the dialog manager is handed.
fn build_template() -> Vec<u8> {
    dlgtpl::build_legacy(DLG_TITLE, DLG_STYLE, DLG_EXSTYLE, DLG_W, DLG_H, 400, ITEMS)
}

// ---------------------------------------------------------------------------
// The four arrow menus — the dropdown row tables, localised
// ---------------------------------------------------------------------------

/// One row of a tracked menu. id 0 is a separator, id 77 opens the 更多...
/// submenu. The caption is the whole `token\tdescription` string; what gets
/// inserted into the edit is the part before the first tab.
struct MenuRow {
    id: i32,
    text: &'static str,
}

const ROW_MORE: MenuRow = MenuRow { id: 77, text: "更多..." };
const ROW_SEP: MenuRow = MenuRow { id: 0, text: "" };

/// The find-arrow menu, in the order its captions are stored. The
/// 更多... submenu comes first, then the token groups.
const REGEX_ROWS: &[MenuRow] = &[
    ROW_MORE,
    ROW_SEP,
    MenuRow { id: 1, text: "\\n\t换行符" },
    MenuRow { id: 2, text: "\\t\t制表符" },
    ROW_SEP,
    MenuRow { id: 4, text: "^\t行头" },
    MenuRow { id: 5, text: "$\t行尾" },
    MenuRow { id: 6, text: ".\t任意字符" },
    MenuRow { id: 7, text: "\\b\t字符边界" },
    MenuRow { id: 8, text: "\\x00\t2字节字符" },
    MenuRow { id: 9, text: "\\u0000\t4字节字符" },
    MenuRow { id: 10, text: "\\w\t单词(A-Z, a-z, 0-9, _)" },
    MenuRow { id: 11, text: "\\W\t非单词" },
    MenuRow { id: 12, text: "\\s\t空白字符" },
    MenuRow { id: 13, text: "\\S\t非空白字符" },
    MenuRow { id: 14, text: "\\d\t数字(0-9)" },
    MenuRow { id: 15, text: "\\D\t非数字" },
    ROW_SEP,
    MenuRow { id: 17, text: "[]\t字符集合" },
    MenuRow { id: 18, text: "[^]\t字符集合之外" },
    MenuRow { id: 19, text: "()\t子表达式分组" },
    MenuRow { id: 20, text: "|\t或" },
    MenuRow { id: 21, text: "{n}\tn次" },
    MenuRow { id: 22, text: "{n,}\t多于n次" },
    MenuRow { id: 23, text: "{n,m}\tn~m次" },
    MenuRow { id: 24, text: "*\t0次或多次" },
    MenuRow { id: 25, text: "+\t1次或多次" },
    MenuRow { id: 26, text: "?\t0次或1次" },
    ROW_SEP,
    MenuRow { id: 28, text: "(?=exp)\t正向预搜索" },
    MenuRow { id: 29, text: "(?!exp)\t正向预搜索否定" },
    MenuRow { id: 30, text: "(?<=exp)\t反向预搜索" },
    MenuRow { id: 31, text: "(?<!exp)\t反向预搜索否定" },
];

/// The 更多... submenu of the find-arrow menu.
const REGEX_MORE: &[MenuRow] = &[
    MenuRow { id: 201, text: "\\c\t汉字" },
    MenuRow { id: 202, text: "\\j\t平假名" },
    MenuRow { id: 203, text: "\\J\t片假名" },
    MenuRow { id: 204, text: "\\P\tCJK符号和标点" },
    ROW_SEP,
    MenuRow { id: 230, text: "[:alnum:]\t任何一个字母或数字(A-Z,a-z,0-9)" },
    MenuRow { id: 231, text: "[:alpha:]\t任何一个字母(A-Z,a-z)" },
    MenuRow { id: 232, text: "[:ascii:]\t任何一个 ASCII 范围内字符(x00-x7F)" },
    MenuRow { id: 233, text: "[:cntrl:]\t任何一个控制字符(x00-x1F,x7F)" },
    MenuRow { id: 234, text: "[:digit:]\t任何一个数字(0–9)" },
    MenuRow { id: 235, text: "[:print:]\t任何一个可显示的 ASCII 字符(x20-x7E)" },
    MenuRow { id: 236, text: "[:space:]\t任何一个空白字符(x09-x0D,x20)" },
    MenuRow { id: 237, text: "[:graph:]\t任何一个可显示的 ASCII 字符,不包含空格(x21-x7E)" },
    MenuRow { id: 238, text: "[:lower:]\t任何一个小写字母(a-z)" },
    MenuRow { id: 239, text: "[:punct:]\t可显示字符[:print:]中除去字母数字[:alnum:]" },
    MenuRow { id: 240, text: "[:upper:]\t任何一个大写字母(A-Z)" },
    MenuRow { id: 241, text: "[:blank:]\t空格或者制表符(x20,x09)" },
    MenuRow { id: 245, text: "[:xdigit:]\t任何一个十六进制数字(0-9,A-F,a-f)" },
];

/// The replace-arrow menu.
const REPLACE_ROWS: &[MenuRow] = &[
    MenuRow { id: 1, text: "$0\t查找到的文本" },
    ROW_SEP,
    MenuRow { id: 2, text: "$1\t反向引用1" },
    MenuRow { id: 3, text: "$2\t反向引用2" },
    MenuRow { id: 4, text: "$3\t反向引用3" },
    MenuRow { id: 5, text: "$4\t反向引用4" },
    MenuRow { id: 6, text: "$5\t反向引用5" },
    MenuRow { id: 7, text: "$6\t反向引用6" },
    MenuRow { id: 8, text: "$7\t反向引用7" },
    MenuRow { id: 9, text: "$8\t反向引用8" },
    MenuRow { id: 10, text: "$9\t反向引用9" },
    ROW_SEP,
    MenuRow { id: 12, text: "\\n\t换行符" },
    MenuRow { id: 13, text: "\\t\t制表符" },
    MenuRow { id: 14, text: "$$\t$字符本身" },
];

/// The exclude-pattern presets — pure glob lists, with no language
/// overrides behind them.
const EXCLUDE_ROWS: &[&str] = &[
    "*.gif;*.bmp;*.jpg;*.jpeg;*.png",
    "*.exe;*.dll;*.cab;*.bin",
    "*.zip;*.rar;*.7z",
    "*.git;*.ignore;*.skip;*.svn;*.idea;node_modules",
];


/// `(codepage, display name)`. The combo shows `name [cp]` when
/// `cp > 10 && cp != 65001`, and rides the codepage in the item data; 自动探测
/// is inserted on top with item data 1.
pub(super) const CODE_PAGES: &[(u16, &str)] = &[
    (0, "Default (ANSI)"),
    (65001, "UTF-8"),
    (1200, "Unicode (UTF-16)"),
    (1201, "Unicode Big Endian (UTF-16)"),
    (936, "Chinese Simplified (GB2312)"),
    (950, "Chinese Traditional (Big5)"),
    (932, "Japanese (Shift-JIS)"),
    (949, "Korean"),
    (864, "Arabic (864)"),
    (708, "Arabic (ASMO 708)"),
    (720, "Arabic (DOS)"),
    (28596, "Arabic (iso-8859-6)"),
    (1004, "Arabic (Mac)"),
    (1256, "Arabic (Windows)"),
    (775, "Baltic (DOS)"),
    (28594, "Baltic (iso-8859-4)"),
    (1257, "Baltic (Windows)"),
    (852, "Central European (DOS)"),
    (28592, "Central European (iso-8859-2)"),
    (10029, "Central European (Mac)"),
    (1250, "Central European (Windows)"),
    (54936, "Chinese Simplified (GB18030)"),
    (20936, "Chinese Simplified (GB2312-80)"),
    (52936, "Chinese Simplified (HZ)"),
    (50227, "Chinese Simplified (ISO-2022)"),
    (1008, "Chinese Simplified (Mac)"),
    (20000, "Chinese Traditional (CNS)"),
    (10002, "Chinese Traditional (Mac)"),
    (50229, "Traditional Chinese (ISO 2022)"),
    (10082, "Croatian (Mac)"),
    (855, "Cyrillic (DOS)"),
    (866, "Cyrillic (DOS)"),
    (28595, "Cyrillic (iso-8859-5)"),
    (20866, "Cyrillic (KOI8-R)"),
    (21866, "Cyrillic (KOI8-U)"),
    (10007, "Cyrillic (Mac)"),
    (1251, "Cyrillic (Windows)"),
    (28603, "Estonian (iso-8859-3)"),
    (863, "French Canadian (DOS)"),
    (737, "Greek (DOS)"),
    (28597, "Greek (iso-8859-7)"),
    (10006, "Greek (Mac)"),
    (1253, "Greek (Windows)"),
    (869, "Greek, Modern (DOS)"),
    (862, "Hebrew (DOS)"),
    (28598, "Hebrew (iso-8859-8)"),
    (38598, "Hebrew (iso-8859-8-i)"),
    (10005, "Hebrew (Mac)"),
    (1255, "Hebrew (Windows)"),
    (875, "IBM EBCDIC (Greek Modern)"),
    (500, "IBM EBCDIC (International)"),
    (20290, "IBM EBCDIC (Japanese Katakana)"),
    (1026, "IBM EBCDIC (Turkish Latin-5)"),
    (37, "IBM EBCDIC (US-Canada)"),
    (861, "Icelandic (DOS)"),
    (10079, "Icelandic (Mac)"),
    (57006, "ISCII Assamese"),
    (57003, "ISCII Bengali"),
    (57002, "ISCII Devanagari"),
    (57010, "ISCII Gujarati"),
    (57008, "ISCII Kannada"),
    (57009, "ISCII Malayalam"),
    (57007, "ISCII Oriya"),
    (57011, "ISCII Punjabi"),
    (57004, "ISCII Tamil"),
    (57005, "ISCII Telugu"),
    (20932, "Japanese (EUC-JP, JIS 0208-1900 and 0212-1900)"),
    (50220, "Japanese (JIS)"),
    (50222, "Japanese (JIS-Allow 1 byte Kana-SO/SI)"),
    (50221, "Japanese (JIS-Allow 1 byte Kana)"),
    (10001, "Japanese (Mac)"),
    (51949, "Korean (EUC)"),
    (50225, "Korean (iso-2022-kr)"),
    (1361, "Korean (Johab)"),
    (10003, "Korean (Mac)"),
    (20949, "Korean Wansung"),
    (28605, "Latin 9 (iso-8859-15)"),
    (865, "Nordic (DOS)"),
    (860, "Portuguese (DOS)"),
    (10010, "Romanian (Mac)"),
    (20261, "T.61"),
    (10021, "Thai (Mac)"),
    (874, "Thai (Windows)"),
    (857, "Turkish (DOS)"),
    (28599, "Turkish (iso-8859-9)"),
    (10081, "Turkish (Mac)"),
    (1254, "Turkish (Windows)"),
    (10017, "Ukrainian (Mac)"),
    (12000, "Unicode 32"),
    (12001, "Unicode 32 Big Endian"),
    (65000, "Unicode (UTF-7)"),
    (437, "United States (DOS)"),
    (20127, "US-ASCII"),
    (1258, "Vietnamese (Windows)"),
    (850, "Western European (DOS)"),
    (28591, "Western European (iso-8859-1)"),
    (10000, "Western European (Mac)"),
    (1252, "Western European (Windows)"),
];

/// The display text for one codepage: the name, plus ` [cp]` unless the
/// codepage is 0 or 65001.
pub(super) fn codepage_label(cp: u16, name: &str) -> String {
    if cp > 10 && cp != 65001 {
        format!("{name} [{cp}]")
    } else {
        name.to_string()
    }
}

// ---------------------------------------------------------------------------
// The file-type presets — the 1211 arrow menu
// ---------------------------------------------------------------------------

/// `所有文件(*.*)`, a separator, `文本文件(*.txt)`, then the 54 package
/// presets. The empty string marks the separator row. Choosing one puts the
/// glob list (the part inside the parentheses) into the type combo.
const FILETYPE_PRESETS: &[&str] = &[
    "所有文件(*.*)",
    "",
    "文本文件(*.txt)",
    "ASM(*.asm)",
    "ASP(*.asp)",
    "AutoHotkey(*.ahk)",
    "AutoIt(*.au3)",
    "AviSynth(*.avs)",
    "Bash(*.sh)",
    "Batch(*.bat;*.cmd)",
    "C++(*.cpp;*.cxx;*.h;*.c;*.cc;*.hpp)",
    "CSS Less(*.less)",
    "CSS(*.css;*.wxss)",
    "CSharp(*.cs)",
    "Cobol(*.cob)",
    "CoffeeScript(*.coffee)",
    "Conf(*.conf)",
    "Erlang(*.erl)",
    "DogEdit Mode File(*.esm)",
    "Fortran(*.for)",
    "Go(*.go)",
    "Groovy(*.groovy)",
    "HTML(*.html;*.hta;*.htm;*.wsf;*.vue)",
    "Haskell(*.hs)",
    "INI(*.ini;*.lng)",
    "JSP(*.jsp)",
    "Java(*.java)",
    "JavaScript(*.js;*.ejs;*.json;*.ts)",
    "Kotlin(*.kt)",
    "Lua(*.lua)",
    "Macro(*.mac;*.evb)",
    "Makefile(*.mak;*.mk)",
    "Markdown(*.md)",
    "Matlab(*.m)",
    "NSIS(*.nsi)",
    "NodeBrain(*.nb)",
    "PHP(*.php;*.volt;*.phtml)",
    "Pascal(*.pas)",
    "Perl(*.pl)",
    "PowerShell(*.ps1)",
    "Python(*.py)",
    "Rebol(*.reb)",
    "Registry(*.reg)",
    "Resource(*.rc)",
    "Ruby(*.rb)",
    "SCSS(*.scss;*.sass)",
    "SQL:MSSQL(*.sql)",
    "TCL(*.tcl)",
    "Tex/Latex(*.tex)",
    "Text(*.txt;*.log)",
    "VB/VBS(*.vbs;*.vb;*.frm;*.bas)",
    "VIM Script(*.vim)",
    "Velocity(*.vm)",
    "Verilog(*.v;*.sv;*.vh;*.svh;*.vt)",
    "XML(*.xml;*.xsd;*.xsl;*.xslt;*.dtd;*.classpath;*.wxml)",
    "YAML(*.yml;*.yaml)",
    "newLISP(*.lsp)",
];

fn preset_to_glob(s: &str) -> String {
    let s = s.strip_suffix(')').unwrap_or(s);
    match s.find('(') {
        Some(at) => s[at + 1..].to_string(),
        None => s.to_string(),
    }
}

// ---------------------------------------------------------------------------
// history.ini — the [Grep] section and the five MRU lists
// ---------------------------------------------------------------------------

/// 解析、编辑、写回都在 [`dg_core::ini::Ini`] 一处；下面的函数只是把
/// 「缺文件当空文档」「读键带默认值」这两个调用习惯留在原地。
pub(super) type IniFile = dg_core::ini::Ini;

pub(super) fn ini_path() -> PathBuf {
    let mut p = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    p.push("history.ini");
    p
}

/// 读一份 INI：文件不存在（或解不出来）就是空文档。
pub(super) fn ini_load(path: &Path) -> IniFile {
    IniFile::load(path).unwrap_or_default()
}

/// One key of one section, or `default`.
pub(super) fn ini_get(ini: &IniFile, section: &str, key: &str, default: &str) -> String {
    ini.get_or(section, key, default)
}

/// One key of one section as a flag.
fn ini_get_bool(ini: &IniFile, section: &str, key: &str, default: bool) -> bool {
    ini.get_bool(section, key, default)
}

/// Set one key, appending the section when it is not there yet.
pub(super) fn ini_set(ini: &mut IniFile, section: &str, key: &str, value: &str) {
    ini.set(section, key, value);
}

/// An empty in-memory file: what a freshly written
/// `[Session]`/`[Item%d]` session document starts from.
pub(super) fn ini_new() -> IniFile {
    IniFile::new()
}

/// Drop a whole section, keys and all — 清除历史记录 (1236) empties
/// `[RecentDoc]` this way, and a rewritten list does before it is re-filled.
pub(super) fn ini_remove_section(ini: &mut IniFile, section: &str) {
    ini.remove_section(section);
}

/// Write the file back.
pub(super) fn ini_save(ini: &IniFile, path: &Path) {
    let _ = ini.save(path);
}

// ---------------------------------------------------------------------------
// MRU list operations
// ---------------------------------------------------------------------------

/// Count the leading blanks (space, tab, ideographic space) — a string that is
/// all blanks is not worth remembering.
fn leading_blanks(s: &str) -> usize {
    s.chars()
        .take_while(|c| *c == ' ' || *c == '\t' || *c == '\u{3000}')
        .count()
}

/// Move `text` to the head, dropping any duplicate, and cap at 20. `false`
/// when the head already is `text`.
fn mru_insert(list: &mut Vec<String>, text: &str) -> bool {
    if list.first().map(|s| s.as_str()) == Some(text) {
        return false;
    }
    list.retain(|s| s != text);
    list.insert(0, text.to_string());
    list.truncate(20);
    true
}

/// Read `Item1..Item20`, stopping at the first missing key, skipping empty
/// values and stripping a leading `#`.
fn mru_load(ini: &IniFile, section: &str) -> Vec<String> {
    let mut out = Vec::new();
    for i in 1..=20 {
        let v = ini_get(ini, section, &format!("Item{i}"), "");
        if v.is_empty() {
            break;
        }
        out.push(v.strip_prefix('#').unwrap_or(&v).to_string());
    }
    out
}

/// Split a `;`-separated filter into globs, dropping `*.*` (everything) and
/// blanks.
pub(super) fn split_globs(s: &str) -> Vec<String> {
    s.split(';')
        .map(str::trim)
        .filter(|g| !g.is_empty() && *g != "*.*")
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// The execution request the dialog hands to the frame
// ---------------------------------------------------------------------------

/// The message the dialog posts to the frame when 查找 / 替换 runs. The
/// `lparam` is a `Box<GrepRequest>` the frame takes ownership of.
pub(super) const WM_GREP: u32 = WM_APP + 0x48;

/// Everything the search needs, in the shape `dg_core::filesearch` speaks.
#[derive(Clone, Debug)]
/// `use_output`, `backup` and `lock_mode` ride along for the frame even when
/// this module itself does not read them back.
#[allow(dead_code)]
pub(super) struct GrepRequest {
    pub find: String,
    pub replace: String,
    pub folder: String,
    pub filetype: String,
    pub exclude: String,
    pub case_sensitive: bool,
    pub regex: bool,
    pub subfolder: bool,
    pub invert: bool,
    pub only_name: bool,
    pub word: bool,
    pub use_output: bool,
    pub backup: bool,
    pub lock_mode: bool,
    /// The combo's codepage; 1 (自动探测) arrives as -1.
    pub encoding: i32,
    pub is_replace: bool,
}


/// What the dialog needs when 1408 asks for it.
pub(super) struct Request<'a> {
    pub frame: HWND,
    /// The editor's selection, already filtered to a single line under 512
    /// characters — the prefill condition
    pub selection: String,
    /// The active document's path, empty when there is none.
    pub doc_path: String,
    /// `-g` / 目录右键菜单指定的文件夹。非空时盖过 `[Grep] Folder` 和
    /// 「跟随当前文件」的预填；普通打开（1408、Ctrl+Shift+F）传空串。
    pub folder: String,
    pub theme: &'a Theme,
}

/// One control of the resize manager: its id, flags and initial client rect.
#[derive(Clone, Copy)]
struct ResizeEntry {
    id: i32,
    flags: u32,
    rect: RECT,
}

struct Dlg {
    frame: HWND,
    theme: Theme,
    font: HFONT,
    marlett: HFONT,
    symbol: HFONT,
    /// the icon-title font's UI font (the icon-title font, the UI-font helper):
    /// every child gets it before the arrows get Marlett.
    sys_font: HFONT,
    /// The dialog face, the control face, the one-pixel edge, the glyph ink.
    bg: HBRUSH,
    face: HBRUSH,
    edge: HBRUSH,
    glyph: HBRUSH,
    /// The dialog's shared tooltip host (the
    /// four control tools live in it).
    tip_host: Option<crate::win32::tiphost::TipHost>,
    /// The five text fields and nine flags, written back to `[Grep]`
    /// so the next open starts where this one left off.
    find: String,
    replace: String,
    filetype: String,
    folder: String,
    exclude: String,
    /// The nine option flags.
    regex: bool,
    case: bool,
    subfolder: bool,
    invert: bool,
    use_output: bool,
    only_name: bool,
    lock_mode: bool,
    word: bool,
    backup: bool,
    /// The encoding combo's item data; 1 is 自动探测.
    encoding: i32,
    /// The 1220 check — kept in the object so 1408 and the toggle agree.
    monitor: bool,
    /// What 1408 prefills from.
    selection: String,
    doc_path: String,
    /// 本次打开带过来的文件夹，用完即清：对话框是单例，下一次 Ctrl+Shift+F
    /// 不该还钉在上一次目录右键点到的那个文件夹上。
    initial_folder: String,
    /// The five MRU lists.
    mru: [RefCell<Vec<String>>; 5],
    /// The resize manager's captured state.
    init_client: (i32, i32),
    init_window: (i32, i32),
    entries: RefCell<Vec<ResizeEntry>>,
}

impl Dlg {
    fn mru_for(&self, combo: i32) -> &RefCell<Vec<String>> {
        let idx = MRU_COMBOS
            .iter()
            .position(|(id, _)| *id == combo)
            .unwrap_or(0);
        &self.mru[idx]
    }
}

/// The dialog's one instance, as the dialog keeps it.
static DLG: AtomicIsize = AtomicIsize::new(0);

fn dlg_hwnd() -> Option<HWND> {
    let v = DLG.load(Ordering::Acquire);
    (v != 0).then_some(HWND(v as *mut core::ffi::c_void))
}

unsafe fn state(hwnd: HWND) -> &'static Dlg {
    &*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Dlg)
}

unsafe fn state_mut(hwnd: HWND) -> &'static mut Dlg {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg)
}

/// Whether the frame's loop should feed `msg` to this dialog. The accelerator
/// keys are answered here, before `IsDialogMessageW`.
pub(super) fn translate(msg: &MSG) -> bool {
    let Some(dlg) = dlg_hwnd() else {
        return false;
    };
    let root = unsafe { GetAncestor(msg.hwnd, GA_ROOT) };
    if root != dlg {
        return false;
    }
    unsafe {
        if let Some(cmd) = accel_of(msg) {
            match cmd {
                1280 => {
                    // Ctrl+F: the frame's own 查找, then this dialog goes away.
                    let _ = PostMessageW(Some(state(dlg).frame), WM_COMMAND, WPARAM(57636), LPARAM(0));
                    let _ = DestroyWindow(dlg);
                }
                1281 => {
                    // Ctrl+H / Ctrl+R: 替换.
                    let _ = PostMessageW(Some(state(dlg).frame), WM_COMMAND, WPARAM(57641), LPARAM(0));
                    let _ = DestroyWindow(dlg);
                }
                1282 => {
                    // Shift+Ctrl+Alt+F: back to the find combo.
                    if let Ok(c) = GetDlgItem(Some(dlg), ID_FIND) {
                        let _ = SetFocus(Some(c));
                    }
                }
                _ => {}
            }
            return true;
        }
        IsDialogMessageW(dlg, msg).as_bool()
    }
}

/// The three keys accelerator table 164 maps into the dialog.
fn accel_of(msg: &MSG) -> Option<i32> {
    if msg.message != WM_KEYDOWN && msg.message != WM_SYSKEYDOWN {
        return None;
    }
    let vk = msg.wParam.0 as u16;
    let down = |k: u16| (unsafe { GetKeyState(k as i32) } as u16 & 0x8000) != 0;
    let (ctrl, shift, alt) = (
        down(VK_CONTROL.0),
        down(VK_SHIFT.0),
        down(VK_MENU.0),
    );
    if ctrl && !alt && !shift && vk == 0x46 {
        Some(1280)
    } else if ctrl && !alt && !shift && (vk == 0x48 || vk == 0x52) {
        Some(1281)
    } else if ctrl && shift && alt && vk == 0x46 {
        Some(1282)
    } else {
        None
    }
}

/// Show the dialog, creating it the first time.
pub(super) fn show(req: Request<'_>) {
    unsafe {
        if let Some(existing) = dlg_hwnd() {
            // Update what the re-show needs, then run the initial fill.
            {
                let st = state_mut(existing);
                st.selection = req.selection.clone();
                st.doc_path = req.doc_path.clone();
                st.initial_folder = req.folder.clone();
            }
            let _ = ShowWindow(existing, SW_SHOW);
            let _ = SetFocus(Some(existing));
            let _ = PostMessageW(Some(existing), WM_COMMAND, WPARAM(1408), LPARAM(0));
            return;
        }
        let dpi = GetDpiForWindow(req.frame).max(96) as f32 / 96.0;
        let dpi_i = GetDpiForWindow(req.frame).max(96) as i32;
        let ini = ini_load(&ini_path());
        let boxed = Box::new(Dlg {
            frame: req.frame,
            theme: req.theme.clone(),
            font: ui_font(dpi),
            marlett: marlett_font(dpi_i),
            symbol: symbol_font(dpi_i),
            sys_font: sys_ui_font(),
            // It is fixed regardless of the theme.
            bg: CreateSolidBrush(to_colorref(req.theme.toolbar_bg)),
            face: CreateSolidBrush(to_colorref(req.theme.toolbar_field)),
            edge: CreateSolidBrush(to_colorref(req.theme.toolbar_border)),
            glyph: CreateSolidBrush(to_colorref(req.theme.toolbar_fg)),
            tip_host: None,
            find: ini_get(&ini, "Grep", "Find", ""),
            replace: ini_get(&ini, "Grep", "Replace", ""),
            filetype: ini_get(&ini, "Grep", "FileType", ""),
            folder: ini_get(&ini, "Grep", "Folder", ""),
            exclude: ini_get(&ini, "Grep", "Exclude", ""),
            regex: ini_get_bool(&ini, "Grep", "Regex", false),
            case: ini_get_bool(&ini, "Grep", "Case", false),
            subfolder: ini_get_bool(&ini, "Grep", "SubFolder", false),
            invert: ini_get_bool(&ini, "Grep", "Invert", false),
            use_output: ini_get_bool(&ini, "Grep", "UseOutput", false),
            only_name: ini_get_bool(&ini, "Grep", "OnlyFileName", false),
            lock_mode: ini_get_bool(&ini, "Grep", "LockMode", true),
            word: ini_get_bool(&ini, "Grep", "Word", false),
            backup: ini_get_bool(&ini, "Grep", "Backup", true),
            encoding: ini_get(&ini, "Grep", "Encoding", "1").parse().unwrap_or(1),
            monitor: ini_get_bool(&ini, "Grep", "Monitor", true),
            selection: req.selection.clone(),
            doc_path: req.doc_path.clone(),
            initial_folder: req.folder.clone(),
            mru: [
                RefCell::new(mru_load(&ini, "GrepFind")),
                RefCell::new(mru_load(&ini, "GrepReplace")),
                RefCell::new(mru_load(&ini, "GrepFileType")),
                RefCell::new(mru_load(&ini, "GrepFolder")),
                RefCell::new(mru_load(&ini, "GrepExclude")),
            ],
            init_client: (-1, -1),
            init_window: (-1, -1),
            entries: RefCell::new(Vec::new()),
        });
        let param = LPARAM(Box::into_raw(boxed) as isize);
        let tmpl = build_template();
        let dlg = match CreateDialogIndirectParamW(
            Some(instance()),
            &*(tmpl.as_ptr() as *const DLGTEMPLATE),
            Some(req.frame),
            Some(dialog_proc),
            param,
        ) {
            Ok(h) => h,
            Err(_) => {
                drop(Box::from_raw(param.0 as *mut Dlg));
                return;
            }
        };
        DLG.store(dlg.0 as isize, Ordering::Release);
        // Without this the edits, combos, checkboxes and push buttons keep
        // light-mode parts no matter what the dialog's own face is painted
        // with.
        crate::win32::chrome::set_chrome_theme(req.theme);
        crate::win32::chrome::dark_theme_window(dlg, crate::win32::chrome::theme_is_dark(req.theme));
        // The fill the creator sends right after creation.
        let _ = PostMessageW(Some(dlg), WM_COMMAND, WPARAM(1408), LPARAM(0));
    }
}

/// The dialog face, as the template's `Ms Shell Dlg 8` — used by the
/// owner-drawn rows only; template children get the dialog manager's font.
fn ui_font(dpi: f32) -> HFONT {
    unsafe {
        let face: Vec<u16> = "Ms Shell Dlg".encode_utf16().chain([0]).collect();
        CreateFontIndirectW(&LOGFONTW {
            lfHeight: -scale(13, dpi),
            lfFaceName: face_name(&face),
            ..Default::default()
        })
    }
}

/// the font-size routine's Marlett, nine point at the dialog's DPI — the four arrow
/// buttons' `4` caption is a down arrow in it. the 外部工具 box's three `6` captions
/// are the same font, which is why this is shared.
pub(super) fn marlett_font(dpi: i32) -> HFONT {
    unsafe {
        let face: Vec<u16> = "Marlett".encode_utf16().chain([0]).collect();
        // ANSI_CHARSET makes GDI pick a substitute face and the arrow
        // caption renders as the digit 4.
        CreateFontIndirectW(&LOGFONTW {
            lfHeight: -((90 * dpi + 360) / 720),
            lfCharSet: FONT_CHARSET(1),
            lfFaceName: face_name(&face),
            ..Default::default()
        })
    }
}

/// The two icon buttons' caption face: `Segoe UI Symbol`, which is what carries
/// U+2BAA. The dialog's own `Ms Shell Dlg` has no glyph for it and GDI draws the
/// missing-glyph rectangle; U+1F441 followed by U+FE0E asks for the text
/// (monochrome) presentation of the eye, which this face also has.
fn symbol_font(dpi: i32) -> HFONT {
    unsafe {
        let face: Vec<u16> = "Segoe UI Symbol".encode_utf16().chain([0]).collect();
        CreateFontIndirectW(&LOGFONTW {
            lfHeight: -((160 * dpi + 360) / 720),
            lfCharSet: FONT_CHARSET(1),
            lfFaceName: face_name(&face),
            ..Default::default()
        })
    }
}

fn sys_ui_font() -> HFONT {
    unsafe {
        let mut lf = LOGFONTW::default();
        let _ = SystemParametersInfoW(
            SPI_GETICONTITLELOGFONT,
            std::mem::size_of::<LOGFONTW>() as u32,
            Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        CreateFontIndirectW(&lf)
    }
}

fn face_name(face: &[u16]) -> [u16; 32] {
    let mut out = [0u16; 32];
    for (c, slot) in face.iter().zip(out.iter_mut()) {
        *slot = *c;
    }
    out
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

unsafe fn init(dlg: HWND) -> isize {
    let ex = GetWindowLongPtrW(dlg, GWL_EXSTYLE);
    if ex & WS_EX_TOOLWINDOW.0 as isize != 0 {
        let _ = SetWindowLongPtrW(dlg, GWL_EXSTYLE, ex & !(WS_EX_TOOLWINDOW.0 as isize));
    }
    let st = state_mut(dlg);
    resize_init(st, dlg);
    init_tooltips(st, dlg);
    // the icon-title font: every child gets the UI font first — this is also what
    // gives the combos their 22px height (WM_SETFONT re-measures them).
    {
        let mut child = GetWindow(dlg, GW_CHILD).unwrap_or_default();
        while !child.0.is_null() {
            let _ = SendMessageW(
                child,
                WM_SETFONT,
                Some(WPARAM(st.sys_font.0 as usize)),
                Some(LPARAM(0)),
            );
            child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
        }
    }
    // the font-size routine's Marlett goes to the four arrow buttons.
    for id in [ID_ARROW_FIND, ID_ARROW_REPL, ID_ARROW_TYPE, ID_ARROW_EXCL] {
        if let Ok(c) = GetDlgItem(Some(dlg), id) {
            let _ = SendMessageW(
                c,
                WM_SETFONT,
                Some(WPARAM(st.marlett.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    }
    // The two icon buttons draw their own glyph over the themed face the native
    // button paints; see [`glyph_proc`].
    for id in [ID_MONITOR, ID_UPDIR] {
        if let Ok(c) = GetDlgItem(Some(dlg), id) {
            let prev = SetWindowLongPtrW(c, GWLP_WNDPROC, glyph_proc as *const () as isize);
            if prev != 0 {
                let orig: WNDPROC = core::mem::transmute(prev);
                GLYPH_PROCS.with(|p| p.borrow_mut().push((c.0 as isize, orig)));
            }
        }
    }
    // The five MRU combos: reset, fill, 4096-character limit.
    for (id, _) in MRU_COMBOS {
        let list = st.mru_for(id).borrow().clone();
        fill_mru(dlg, id, &list);
        if let Ok(c) = GetDlgItem(Some(dlg), id) {
            let _ = SendMessageW(c, CB_LIMITTEXT, Some(WPARAM(4096)), None);
        }
    }
    fill_encoding(dlg);
    sync(st, dlg, false, -1);
    // No folder history yet: `-g` 指定的文件夹优先，否则用当前文档的目录
    // 当第一行（编辑框里仍保留 `[Grep] Folder` 的值）。
    if let Ok(c) = GetDlgItem(Some(dlg), ID_DIR) {
        let count = SendMessageW(c, CB_GETCOUNT, None, None).0 as i32;
        let seed = if !st.initial_folder.is_empty() {
            st.initial_folder.clone()
        } else {
            parent_of(&st.doc_path)
        };
        if count <= 0 && !seed.is_empty() {
            let buf: Vec<u16> = seed.encode_utf16().chain([0]).collect();
            let _ = SendMessageW(
                c,
                CB_INSERTSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(buf.as_ptr() as isize)),
            );
        }
    }
    if let Ok(c) = GetDlgItem(Some(dlg), ID_FIND) {
        let _ = SetFocus(Some(c));
    }
    let regex = st.regex;
    if let Ok(c) = GetDlgItem(Some(dlg), ID_WORD) {
        let _ = EnableWindow(c, !regex);
    }
    if let Ok(c) = GetDlgItem(Some(dlg), ID_ARROW_FIND) {
        let _ = EnableWindow(c, regex);
    }
    if let Ok(c) = GetDlgItem(Some(dlg), ID_ARROW_REPL) {
        let _ = EnableWindow(c, regex);
    }
    let monitor = st.monitor;
    if let Ok(c) = GetDlgItem(Some(dlg), ID_MONITOR) {
        let _ = SendMessageW(c, BM_SETCHECK, Some(WPARAM(monitor as usize)), None);
    }
    1
}

/// reset the combo and replay the list.
unsafe fn fill_mru(dlg: HWND, id: i32, list: &[String]) {
    let Ok(c) = GetDlgItem(Some(dlg), id) else {
        return;
    };
    let _ = SendMessageW(c, CB_RESETCONTENT, None, None);
    for text in list {
        let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let _ = SendMessageW(
            c,
            CB_INSERTSTRING,
            Some(WPARAM(0)),
            Some(LPARAM(buf.as_ptr() as isize)),
        );
    }
}

/// The encoding combo: the 98 codepages, then 自动探测 on top with item data 1.
unsafe fn fill_encoding(dlg: HWND) {
    let Ok(c) = GetDlgItem(Some(dlg), ID_ENC) else {
        return;
    };
    let _ = SendMessageW(c, CB_SETDROPPEDWIDTH, Some(WPARAM(250)), None);
    let _ = SendMessageW(c, CB_RESETCONTENT, None, None);
    for (cp, name) in CODE_PAGES {
        let label = codepage_label(*cp, name);
        let buf: Vec<u16> = label.encode_utf16().chain([0]).collect();
        let idx = SendMessageW(
            c,
            CB_ADDSTRING,
            Some(WPARAM(0)),
            Some(LPARAM(buf.as_ptr() as isize)),
        )
        .0 as i32;
        if idx >= 0 {
            let _ = SendMessageW(
                c,
                CB_SETITEMDATA,
                Some(WPARAM(idx as usize)),
                Some(LPARAM(*cp as isize)),
            );
        }
    }
    let auto = crate::win32::lang::string(15019).unwrap_or("自动探测");
    let buf: Vec<u16> = auto.encode_utf16().chain([0]).collect();
    let idx = SendMessageW(
        c,
        CB_INSERTSTRING,
        Some(WPARAM(0)),
        Some(LPARAM(buf.as_ptr() as isize)),
    )
    .0 as i32;
    if idx >= 0 {
        let _ = SendMessageW(
            c,
            CB_SETITEMDATA,
            Some(WPARAM(idx as usize)),
            Some(LPARAM(1)),
        );
    }
}

// ---------------------------------------------------------------------------
// The control/model sync
// ---------------------------------------------------------------------------

unsafe fn get_check(dlg: HWND, id: i32) -> bool {
    GetDlgItem(Some(dlg), id)
        .map(|c| SendMessageW(c, BM_GETCHECK, None, None).0 as u32 & 1 == 1)
        .unwrap_or(false)
}

unsafe fn set_check(dlg: HWND, id: i32, on: bool) {
    if let Ok(c) = GetDlgItem(Some(dlg), id) {
        let _ = SendMessageW(c, BM_SETCHECK, Some(WPARAM(on as usize)), None);
    }
}

unsafe fn text_of(dlg: HWND, id: i32) -> String {
    let Ok(ctl) = GetDlgItem(Some(dlg), id) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(ctl);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(ctl, &mut buf);
    buf.truncate(n.max(0) as usize);
    String::from_utf16_lossy(&buf)
}

unsafe fn set_text(dlg: HWND, id: i32, text: &str) -> bool {
    let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    SetDlgItemTextW(dlg, id, PCWSTR::from_raw(buf.as_ptr())).is_ok()
}

/// A control refused the exchange: beep and focus it
unsafe fn refuse(dlg: HWND, id: i32) -> bool {
    let _ = MessageBeep(MESSAGEBOX_STYLE(0xFFFF_FFFF));
    if let Ok(c) = GetDlgItem(Some(dlg), id) {
        let _ = SetFocus(Some(c));
    }
    false
}

/// `from` reads the controls into the model; otherwise the model is written
/// into them. `only` narrows it to one control (-1 is everything), and the
/// return says whether the exchange held.
unsafe fn sync(st: &mut Dlg, dlg: HWND, from: bool, only: i32) -> bool {
    let want = |id: i32| only == -1 || only == id;
    // The nine option checks
    macro_rules! flag {
        ($id:expr, $field:ident) => {
            if want($id) {
                if from {
                    st.$field = get_check(dlg, $id);
                } else {
                    set_check(dlg, $id, st.$field);
                }
            }
        };
    }
    flag!(ID_REGEX, regex);
    flag!(ID_CASE, case);
    flag!(ID_SUBFOLDER, subfolder);
    flag!(ID_USEOUTPUT, use_output);
    flag!(ID_INVERT, invert);
    flag!(ID_ONLYNAME, only_name);
    flag!(ID_WORD, word);
    flag!(ID_LOCK, lock_mode);
    flag!(ID_BACKUP, backup);
    // The five text fields.
    macro_rules! text {
        ($id:expr, $field:ident) => {
            if want($id) {
                if from {
                    st.$field = text_of(dlg, $id);
                } else if !set_text(dlg, $id, &st.$field) {
                    return refuse(dlg, $id);
                }
            }
        };
    }
    text!(ID_REPL, replace);
    text!(ID_FIND, find);
    text!(ID_TYPE, filetype);
    text!(ID_DIR, folder);
    text!(ID_EXCL, exclude);
    // The encoding combo rides the codepage in its item data.
    if want(ID_ENC) {
        if let Ok(c) = GetDlgItem(Some(dlg), ID_ENC) {
            if from {
                let sel = SendMessageW(c, CB_GETCURSEL, None, None).0 as i32;
                st.encoding =
                    SendMessageW(c, CB_GETITEMDATA, Some(WPARAM(sel.max(0) as usize)), None).0 as i32;
            } else {
                let count = SendMessageW(c, CB_GETCOUNT, None, None).0 as i32;
                for i in 0..count {
                    let data =
                        SendMessageW(c, CB_GETITEMDATA, Some(WPARAM(i as usize)), None).0 as i32;
                    if data == st.encoding {
                        let _ = SendMessageW(c, CB_SETCURSEL, Some(WPARAM(i as usize)), None);
                        break;
                    }
                }
            }
        }
    }
    true
}

/// The text is set back.
unsafe fn mru_commit(st: &Dlg, dlg: HWND, id: i32) {
    let text = text_of(dlg, id);
    if text.is_empty() || leading_blanks(&text) >= text.chars().count() {
        return;
    }
    let list = st.mru_for(id);
    let changed = mru_insert(&mut list.borrow_mut(), &text);
    if changed {
        fill_mru(dlg, id, &list.borrow());
    }
    let _ = set_text(dlg, id, &text);
}

// ---------------------------------------------------------------------------
// The tooltip host
// ---------------------------------------------------------------------------

unsafe fn init_tooltips(st: &mut Dlg, dlg: HWND) {
    let Some(host) = crate::win32::tiphost::TipHost::create(dlg) else {
        return;
    };
    // The four windows that name a tool in zh-CN.lng: the two icon buttons,
    // the exclude combo, and the exclude edit's own child.
    let excl_tip = "输入想要排除的文件名或者文件夹名,多个模式之间用分号分割，比如:*.tmp;*.bak";
    let mut targets: Vec<HWND> = Vec::new();
    if let Ok(c) = GetDlgItem(Some(dlg), ID_MONITOR) {
        targets.push(c);
    }
    if let Ok(c) = GetDlgItem(Some(dlg), ID_UPDIR) {
        targets.push(c);
    }
    if let Ok(c) = GetDlgItem(Some(dlg), ID_EXCL) {
        targets.push(c);
        if let Some(edit) = get_child_edit(c) {
            targets.push(edit);
        }
    }
    for (i, target) in targets.iter().enumerate() {
        let text = match i {
            0 => "使用当前文件的路径",
            1 => "移动到上一级目录",
            _ => excl_tip,
        };
        host.add_window_tool(*target, text);
    }
    st.tip_host = Some(host);
}

/// The combo's edit child (GW_CHILD), when it has one.
unsafe fn get_child_edit(combo: HWND) -> Option<HWND> {
    let child = GetWindow(combo, GW_CHILD).unwrap_or_default();
    (!child.0.is_null()).then_some(child)
}

// ---------------------------------------------------------------------------
// The arrow menus
// one per direction
// ---------------------------------------------------------------------------

/// Track a menu at `button`'s bottom-left and return the chosen row id (0 when
/// dismissed).
unsafe fn track_at(menu: HMENU, dlg: HWND, button: i32) -> i32 {
    let Ok(btn) = GetDlgItem(Some(dlg), button) else {
        let _ = DestroyMenu(menu);
        return 0;
    };
    let mut rect = RECT::default();
    if GetWindowRect(btn, &mut rect).is_err() {
        let _ = DestroyMenu(menu);
        return 0;
    }
    let chosen = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY,
        rect.left,
        rect.bottom,
        None,
        dlg,
        None,
    )
    .0 as i32;
    let _ = DestroyMenu(menu);
    chosen
}

/// Build a menu from rows; id 0 separates, id 77 opens the 更多... submenu.
unsafe fn build_menu(rows: &[MenuRow]) -> HMENU {
    let Ok(menu) = CreatePopupMenu() else {
        return HMENU::default();
    };
    for row in rows {
        if row.id == 0 {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        } else if row.id == 77 {
            let sub = build_menu(REGEX_MORE);
            let text: Vec<u16> = row.text.encode_utf16().chain([0]).collect();
            let _ = AppendMenuW(
                menu,
                MF_POPUP,
                sub.0 as usize,
                PCWSTR::from_raw(text.as_ptr()),
            );
        } else {
            let text: Vec<u16> = row.text.encode_utf16().chain([0]).collect();
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                row.id as usize,
                PCWSTR::from_raw(text.as_ptr()),
            );
        }
    }
    menu
}

/// What a row inserts: the caption up to its first tab.
fn row_token(row: &MenuRow) -> &str {
    row.text.split('\t').next().unwrap_or("")
}

/// replace the combo edit's saved selection with `text` and park the caret
/// after it. Reading it at click time is the same value.
unsafe fn insert_at_selection(edit: HWND, text: &str) {
    let mut start = 0i32;
    let mut end = 0i32;
    SendMessageW(
        edit,
        EM_GETSEL,
        Some(WPARAM(&raw mut start as usize)),
        Some(LPARAM(&raw mut end as isize)),
    );
    let _ = SendMessageW(
        edit,
        EM_SETSEL,
        Some(WPARAM(start as usize)),
        Some(LPARAM(start as isize)),
    );
    let _ = SendMessageW(edit, EM_SCROLLCARET, None, None);
    let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let _ = SendMessageW(
        edit,
        EM_REPLACESEL,
        Some(WPARAM(1)),
        Some(LPARAM(buf.as_ptr() as isize)),
    );
    let _ = SetFocus(Some(edit));
    let at = start + text.encode_utf16().count() as i32;
    let _ = SendMessageW(
        edit,
        EM_SETSEL,
        Some(WPARAM(at as usize)),
        Some(LPARAM(at as isize)),
    );
    let _ = SendMessageW(edit, EM_SCROLLCARET, None, None);
}

/// The combo's edit child, the insert target.
unsafe fn combo_edit(dlg: HWND, combo: i32) -> Option<HWND> {
    let c = GetDlgItem(Some(dlg), combo).ok()?;
    get_child_edit(c)
}

/// 1203: the regex helper menu, inserting into the find combo.
unsafe fn on_regex_menu(st: &mut Dlg, dlg: HWND) {
    let menu = build_menu(REGEX_ROWS);
    let chosen = track_at(menu, dlg, ID_ARROW_FIND);
    if chosen == 0 {
        return;
    }
    if !sync(st, dlg, true, ID_FIND) {
        return;
    }
    let row = REGEX_ROWS
        .iter()
        .chain(REGEX_MORE.iter())
        .find(|r| r.id == chosen);
    if let Some(row) = row {
        if let Some(edit) = combo_edit(dlg, ID_FIND) {
            insert_at_selection(edit, row_token(row));
        }
    }
}

/// 1218: the replace helper menu, inserting into the replace combo.
unsafe fn on_replace_menu(st: &mut Dlg, dlg: HWND) {
    let menu = build_menu(REPLACE_ROWS);
    let chosen = track_at(menu, dlg, ID_ARROW_REPL);
    if chosen == 0 {
        return;
    }
    if !sync(st, dlg, true, ID_REPL) {
        return;
    }
    if let Some(row) = REPLACE_ROWS.iter().find(|r| r.id == chosen) {
        if let Some(edit) = combo_edit(dlg, ID_REPL) {
            insert_at_selection(edit, row_token(row));
        }
    }
}

/// 1211: the file-type presets. The chosen caption loses its `Title(...)`
/// wrapper and what is left becomes the type field.
unsafe fn on_filetype_menu(st: &mut Dlg, dlg: HWND) {
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    let mut chosen_preset: Option<&str> = None;
    for (i, preset) in FILETYPE_PRESETS.iter().enumerate() {
        if preset.is_empty() {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        } else {
            let text: Vec<u16> = preset.encode_utf16().chain([0]).collect();
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                (i + 1) as usize,
                PCWSTR::from_raw(text.as_ptr()),
            );
        }
    }
    let chosen = track_at(menu, dlg, ID_ARROW_TYPE);
    if chosen > 0 {
        let idx = (chosen - 1) as usize;
        if let Some(preset) = FILETYPE_PRESETS.get(idx) {
            chosen_preset = Some(preset);
        }
    }
    if let Some(preset) = chosen_preset {
        st.filetype = preset_to_glob(preset);
        let _ = sync(st, dlg, false, ID_TYPE);
    }
}

/// 1223: the exclude presets, appended to the exclude field after a `;`.
unsafe fn on_exclude_menu(st: &mut Dlg, dlg: HWND) {
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    for (i, text) in EXCLUDE_ROWS.iter().enumerate() {
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            (i + 1) as usize,
            PCWSTR::from_raw(wide.as_ptr()),
        );
    }
    let chosen = track_at(menu, dlg, ID_ARROW_EXCL);
    if chosen <= 0 {
        return;
    }
    let Some(glob) = EXCLUDE_ROWS.get((chosen - 1) as usize) else {
        return;
    };
    if !sync(st, dlg, true, ID_EXCL) {
        return;
    }
    if !st.exclude.is_empty() {
        st.exclude.push(';');
    }
    st.exclude.push_str(glob);
    let _ = sync(st, dlg, false, ID_EXCL);
}

// ---------------------------------------------------------------------------
// Browse and the folder buttons
// ---------------------------------------------------------------------------

/// The browse callback: `BFFM_INITIALIZED` selects the current folder.
unsafe extern "system" fn browse_cb(_hwnd: HWND, msg: u32, _lparam: LPARAM, data: LPARAM) -> i32 {
    if msg == BFFM_INITIALIZED && data.0 != 0 {
        let _ = SendMessageW(_hwnd, BFFM_SETSELECTIONW, None, Some(data));
    }
    0
}

/// 1204: the shell folder picker; the choice replaces the folder field.
unsafe fn on_browse(st: &mut Dlg, dlg: HWND) {
    if !sync(st, dlg, true, ID_DIR) {
        return;
    }
    let current: Vec<u16> = st.folder.encode_utf16().chain([0]).collect();
    let bi = BROWSEINFOW {
        hwndOwner: dlg,
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        lpfn: Some(browse_cb),
        lParam: LPARAM(current.as_ptr() as isize),
        ..Default::default()
    };
    let pidl = SHBrowseForFolderW(&raw const bi);
    if !pidl.is_null() {
        let mut buf = [0u16; 260];
        if SHGetPathFromIDListW(pidl, &mut buf).as_bool() {
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            st.folder = String::from_utf16_lossy(&buf[..end]);
            let _ = sync(st, dlg, false, ID_DIR);
        }
        let _ = windows::Win32::System::Com::CoTaskMemFree(Some(pidl as *const _));
    }
}

/// 1220: the monitor toggle — persists `Grep/Monitor` at once, and when turned
/// on adopts the active document's directory.
unsafe fn on_monitor(st: &mut Dlg, dlg: HWND) {
    // A native `BS_AUTOCHECKBOX | BS_PUSHLIKE` has already flipped its own state
    // by the time `BN_CLICKED` arrives, so the model follows the control.
    st.monitor = get_check(dlg, ID_MONITOR);
    let on = st.monitor;
    let mut ini = ini_load(&ini_path());
    ini_set(&mut ini, "Grep", "Monitor", if on { "Y" } else { "N" });
    ini_save(&ini, &ini_path());
    if on && !st.doc_path.is_empty() {
        let parent = parent_of(&st.doc_path);
        if !parent.is_empty() {
            st.folder = parent;
            let _ = sync(st, dlg, false, ID_DIR);
        }
    }
}

/// 1221: the folder field's parent directory.
unsafe fn on_updir(st: &mut Dlg, dlg: HWND) {
    st.folder = text_of(dlg, ID_DIR);
    st.folder = parent_of(&st.folder);
    let _ = set_text(dlg, ID_DIR, &st.folder);
}

// ---------------------------------------------------------------------------
// 查找 / 替换
// ---------------------------------------------------------------------------

/// The five text fields and nine flags, written back to `[Grep]` so the next
/// open starts where this one left off. This is where this implementation
/// puts them.
fn save_grep(st: &Dlg) {
    let mut ini = ini_load(&ini_path());
    ini_set(&mut ini, "Grep", "Find", &st.find);
    ini_set(&mut ini, "Grep", "Replace", &st.replace);
    ini_set(&mut ini, "Grep", "Folder", &st.folder);
    ini_set(&mut ini, "Grep", "Exclude", &st.exclude);
    ini_set(&mut ini, "Grep", "FileType", &st.filetype);
    for (key, on) in [
        ("Case", st.case),
        ("Regex", st.regex),
        ("SubFolder", st.subfolder),
        ("UseOutput", st.use_output),
        ("Invert", st.invert),
        ("Word", st.word),
        ("OnlyFileName", st.only_name),
        ("LockMode", st.lock_mode),
        ("Backup", st.backup),
    ] {
        ini_set(&mut ini, "Grep", key, if on { "Y" } else { "N" });
    }
    ini_set(&mut ini, "Grep", "Encoding", &st.encoding.to_string());
    ini_save(&ini, &ini_path());
}

/// The request both buttons build, then post and destroy over.
unsafe fn run(st: &Dlg, is_replace: bool) {
    let req = Box::new(GrepRequest {
        find: st.find.clone(),
        replace: st.replace.clone(),
        folder: st.folder.clone(),
        filetype: st.filetype.clone(),
        exclude: st.exclude.clone(),
        case_sensitive: st.case,
        regex: st.regex,
        subfolder: st.subfolder,
        invert: st.invert,
        only_name: st.only_name,
        word: st.word,
        use_output: st.use_output,
        backup: st.backup,
        lock_mode: st.lock_mode,
        encoding: if st.encoding == 1 { -1 } else { st.encoding },
        is_replace,
    });
    let _ = PostMessageW(
        Some(st.frame),
        WM_GREP,
        WPARAM(is_replace as usize),
        LPARAM(Box::into_raw(req) as isize),
    );
}

unsafe fn on_find(st: &mut Dlg, dlg: HWND) {
    if !sync(st, dlg, true, -1) {
        return;
    }
    if st.filetype.is_empty() {
        st.filetype = "*.*".to_string();
        if !sync(st, dlg, false, ID_TYPE) {
            return;
        }
    }
    for (id, _) in MRU_COMBOS {
        mru_commit(st, dlg, id);
    }
    if st.find.is_empty() {
        let _ = MessageBeep(MESSAGEBOX_STYLE(0x30));
        if let Ok(c) = GetDlgItem(Some(dlg), ID_FIND) {
            let _ = SetFocus(Some(c));
        }
        return;
    }
    if st.folder.is_empty() {
        let _ = MessageBeep(MESSAGEBOX_STYLE(0x30));
        if let Ok(c) = GetDlgItem(Some(dlg), ID_DIR) {
            let _ = SetFocus(Some(c));
        }
        return;
    }
    save_grep(st);
    run(st, false);
    let _ = DestroyWindow(dlg);
}

unsafe fn on_replace(st: &mut Dlg, dlg: HWND) {
    if !sync(st, dlg, true, -1) {
        return;
    }
    if st.filetype.is_empty() {
        st.filetype = "*.*".to_string();
        if !sync(st, dlg, false, ID_TYPE) {
            return;
        }
    }
    for (id, _) in MRU_COMBOS {
        mru_commit(st, dlg, id);
    }
    if st.find.is_empty() || st.folder.is_empty() {
        return;
    }
    let prompt = if st.backup {
        "您正试图在文件夹中进行替换,继续吗?"
    } else {
        "您未选择备份被替换的文件,继续吗?"
    };
    let text: Vec<u16> = prompt.encode_utf16().chain([0]).collect();
    let title: Vec<u16> = "DogEdit".encode_utf16().chain([0]).collect();
    let yes = MessageBoxW(
        Some(dlg),
        PCWSTR::from_raw(text.as_ptr()),
        PCWSTR::from_raw(title.as_ptr()),
        MB_OKCANCEL | MB_ICONWARNING,
    )
    .0
        == IDOK.0;
    if !yes {
        return;
    }
    save_grep(st);
    run(st, true);
    let _ = DestroyWindow(dlg);
}

/// 1408: the initial fill. A one-line selection under 512 characters prefills
/// the find combo and selects all; monitor on adopts the document's directory.
unsafe fn on_fill(st: &mut Dlg, dlg: HWND) {
    let sel = st.selection.clone();
    if !sel.is_empty() && !sel.contains('\n') && sel.chars().count() < 512 {
        st.find = sel;
        if !sync(st, dlg, false, ID_FIND) {
            return;
        }
        if let Ok(c) = GetDlgItem(Some(dlg), ID_FIND) {
            let _ = SendMessageW(c, CB_SETEDITSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
    }
    // `-g` 带来的文件夹赢过「跟随当前文件」：目录右键点开的那一刻，用户
    // 要搜的就是那个目录，不是当前文档所在的目录。它只作用这一次。
    let explicit = std::mem::take(&mut st.initial_folder);
    if !explicit.is_empty() {
        st.folder = explicit;
        if !sync(st, dlg, false, ID_DIR) {
            return;
        }
    } else if st.monitor && !st.doc_path.is_empty() && Path::new(&st.doc_path).exists() {
        let parent = parent_of(&st.doc_path);
        if !parent.is_empty() {
            st.folder = parent;
            if !sync(st, dlg, false, ID_DIR) {
                return;
            }
        }
    }
    let _ = sync(st, dlg, true, ID_FIND);
}

// ---------------------------------------------------------------------------
// The resize manager
// ---------------------------------------------------------------------------

/// Arm the manager: make sure the dialog clips its children, then capture the
/// initial client size, the initial window size and every table control's
/// initial client rect.
unsafe fn resize_init(st: &mut Dlg, dlg: HWND) {
    let style = GetWindowLongPtrW(dlg, GWL_STYLE);
    if style & WS_CLIPCHILDREN.0 as isize == 0 {
        let _ = SetWindowLongPtrW(dlg, GWL_STYLE, style | WS_CLIPCHILDREN.0 as isize);
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(dlg, &mut rc);
    st.init_client = (rc.right - rc.left, rc.bottom - rc.top);
    let _ = GetWindowRect(dlg, &mut rc);
    st.init_window = (rc.right - rc.left, rc.bottom - rc.top);
    let mut entries = st.entries.borrow_mut();
    entries.clear();
    for (id, flags) in RESIZE_TABLE {
        if let Ok(ctl) = GetDlgItem(Some(dlg), *id) {
            let mut r = RECT::default();
            if GetWindowRect(ctl, &mut r).is_ok() {
                let _ = MapWindowPoints(
                    None,
                    Some(dlg),
                    &mut [
                        POINT { x: r.left, y: r.top },
                        POINT { x: r.right, y: r.bottom },
                    ],
                );
                entries.push(ResizeEntry {
                    id: *id,
                    flags: *flags,
                    rect: r,
                });
            }
        }
    }
}

/// Move every table control by the client-size delta, each by its own flags.
/// The arithmetic works from the captured initial rects: a control that only
/// sizes keeps its left edge, one that only moves keeps its width.
unsafe fn resize(st: &Dlg, dlg: HWND, cx: i32, cy: i32) {
    let visible = IsWindowVisible(dlg).as_bool();
    if visible {
        let _ = SendMessageW(dlg, WM_SETREDRAW, Some(WPARAM(0)), None);
    }
    let entries = st.entries.borrow();
    for e in entries.iter() {
        let Ok(ctl) = GetDlgItem(Some(dlg), e.id) else {
            continue;
        };
        let mut rect = RECT::default();
        if GetWindowRect(ctl, &mut rect).is_err() {
            continue;
        }
        let _ = MapWindowPoints(
            None,
            Some(dlg),
            &mut [
                POINT { x: rect.left, y: rect.top },
                POINT { x: rect.right, y: rect.bottom },
            ],
        );
        let f = e.flags;
        if f & 0x20 != 0 {
            let w = rect.right - rect.left;
            rect.left = (cx - w) / 2;
            rect.right = rect.left + w;
        } else if f & 5 != 0 {
            let new_right = e.rect.right + (cx - st.init_client.0);
            rect.right = new_right;
            if f & 4 != 0 {
                rect.left = new_right + e.rect.left - e.rect.right;
            }
        }
        if f & 0x40 != 0 {
            let h = rect.bottom - rect.top;
            rect.top = (cy - h) / 2;
            rect.bottom = rect.top + h;
        } else if f & 0xA != 0 {
            let new_bottom = e.rect.bottom + (cy - st.init_client.1);
            rect.bottom = new_bottom;
            if f & 8 != 0 {
                rect.top = new_bottom + e.rect.top - e.rect.bottom;
            }
        }
        if f & 0x10 != 0 {
            let _ = InvalidateRect(Some(ctl), None, true);
        }
        if f & 0x7F != 0 {
            let _ = SetWindowPos(
                ctl,
                None,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
    if visible {
        let _ = SendMessageW(dlg, WM_SETREDRAW, Some(WPARAM(1)), None);
    }
    let _ = RedrawWindow(
        Some(dlg),
        None,
        None,
        RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
    );
}

// ---------------------------------------------------------------------------
// The two icon buttons' glyph — 1220, 1221
// ---------------------------------------------------------------------------

/// The glyph each icon button carries. So do we. U+FE0E asks for the text
/// (monochrome) presentation of the eye.
const GLYPH_MONITOR: &str = "\u{1F441}\u{FE0E}";
const GLYPH_UPDIR: &str = "\u{2BAA}";

thread_local! {
    /// The procs [`glyph_proc`] replaced, so the button can be given its own
    /// back on `WM_NCDESTROY`.
    static GLYPH_PROCS: RefCell<Vec<(isize, WNDPROC)>> = const { RefCell::new(Vec::new()) };
}

/// The button proc: let the native button draw its themed face, its checked
/// state and its press feedback, then put the glyph on top of the result.
unsafe extern "system" fn glyph_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let orig = GLYPH_PROCS.with(|p| {
        p.borrow()
            .iter()
            .rev()
            .find(|(h, _)| *h == hwnd.0 as isize)
            .and_then(|(_, f)| *f)
    });
    let result = match orig {
        Some(f) => f(hwnd, msg, wp, lp),
        None => DefWindowProcW(hwnd, msg, wp, lp),
    };
    if msg == WM_PAINT {
        paint_glyph(hwnd);
    } else if msg == WM_NCDESTROY {
        if let Some(f) = orig {
            let _ = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, f as *const () as isize);
            GLYPH_PROCS.with(|p| p.borrow_mut().retain(|(h, _)| *h != hwnd.0 as isize));
        }
    }
    result
}

/// Draw the glyph centred in its button. Drawing it here rather than handing it
/// to the caption is what makes centring possible: USER32 places a caption by the
/// font's line box, and `Segoe UI Symbol`'s ink for these two codepoints sits
/// below that box's centre by a tenth of its em — 2.5 px at 14 pt, 3 px at 16 pt
/// and 120 DPI. `BS_VCENTER`, `BS_MULTILINE` and both signs of `lfHeight` all
/// leave it there on the live control, so the rect is lifted by
/// `scale(2)` instead, which lands the ink within half a pixel of the centre.
unsafe fn paint_glyph(hwnd: HWND) {
    let Ok(dlg) = GetParent(hwnd) else {
        return;
    };
    if GetWindowLongPtrW(dlg, GWLP_USERDATA) == 0 {
        return;
    }
    let st = state(dlg);
    let mut text: Vec<u16> = if GetWindowLongPtrW(hwnd, GWL_ID) as i32 == ID_MONITOR {
        GLYPH_MONITOR
    } else {
        GLYPH_UPDIR
    }
    .encode_utf16()
    .collect();
    let mut rc = RECT::default();
    if GetClientRect(hwnd, &mut rc).is_err() {
        return;
    }
    // The lift the native placement is short by, in the control's own DPI.
    let dpi = GetDpiForWindow(hwnd) as f32 / 96.0;
    let lift = scale(2, dpi);
    rc.top -= lift;
    rc.bottom -= lift;
    let hdc = GetDC(Some(hwnd));
    if hdc.0.is_null() {
        return;
    }
    let old_font = SelectObject(hdc, HGDIOBJ(st.symbol.0));
    let old_bk = SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, glyph_ink(hwnd, st));
    let _ = DrawTextW(
        hdc,
        &mut text,
        &mut rc as *mut RECT,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP,
    );
    let _ = SetBkMode(hdc, BACKGROUND_MODE(old_bk as u32));
    SelectObject(hdc, old_font);
    let _ = ReleaseDC(Some(hwnd), hdc);
}

/// The ink colour the native caption would have used, so the checked face keeps
/// its dark glyph and the normal face its light one.
unsafe fn glyph_ink(hwnd: HWND, st: &Dlg) -> COLORREF {
    let theme = OpenThemeData(Some(hwnd), w!("BUTTON"));
    let fallback = to_colorref(st.theme.toolbar_fg);
    if theme.0 == 0 {
        return fallback;
    }
    let checked = SendMessageW(hwnd, BM_GETCHECK, None, None).0 as i32 == 1;
    let state = if checked { PBS_PRESSED } else { PBS_NORMAL };
    let got = GetThemeColor(theme, BP_PUSHBUTTON.0, state.0, TMT_TEXTCOLOR);
    let _ = CloseThemeData(theme);
    match got {
        Ok(c) if c.0 != 0 => c,
        _ => fallback,
    }
}

// ---------------------------------------------------------------------------
// The owner-drawn row — 4000
// ---------------------------------------------------------------------------

unsafe fn on_draw_item(dlg: HWND, lparam: LPARAM) -> isize {
    let di = &*(lparam.0 as *const DRAWITEMSTRUCT);
    if di.CtlID as i32 != ID_SEP {
        return 0;
    }
    // The one-pixel separator the template carries as a sunken frame.
    let rc = di.rcItem;
    let st = state(dlg);
    FillRect(di.hDC, &rc, st.edge);
    1
}

// ---------------------------------------------------------------------------
// The window proc
// ---------------------------------------------------------------------------

unsafe fn on_command(dlg: HWND, wparam: WPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as u16 as i32;
    let st = state_mut(dlg);
    match id {
        // IDCANCEL and 关闭 are the same id: 关闭 hides the singleton, which
        // the next 1408 re-shows.
        ID_CLOSE => {
            let _ = ShowWindow(dlg, SW_HIDE);
            1
        }
        1280 => {
            // Ctrl+F, from accelerator table 164: the frame's own 查找.
            let _ = PostMessageW(Some(st.frame), WM_COMMAND, WPARAM(57636), LPARAM(0));
            let _ = DestroyWindow(dlg);
            1
        }
        1281 => {
            let _ = PostMessageW(Some(st.frame), WM_COMMAND, WPARAM(57641), LPARAM(0));
            let _ = DestroyWindow(dlg);
            1
        }
        1282 => {
            if let Ok(c) = GetDlgItem(Some(dlg), ID_FIND) {
                let _ = SetFocus(Some(c));
            }
            1
        }
        ID_REGEX if wparam.0 as u32 == ID_REGEX as u32 => {
            // The one toggle with immediate consequences: the arrows follow it
            // and 全字匹配 is meaningless against a pattern.
            if !sync(st, dlg, true, ID_REGEX) {
                return 1;
            }
            let regex = st.regex;
            if let Ok(c) = GetDlgItem(Some(dlg), ID_WORD) {
                let _ = EnableWindow(c, !regex);
            }
            if let Ok(c) = GetDlgItem(Some(dlg), ID_ARROW_FIND) {
                let _ = EnableWindow(c, regex);
            }
            if let Ok(c) = GetDlgItem(Some(dlg), ID_ARROW_REPL) {
                let _ = EnableWindow(c, regex);
            }
            1
        }
        ID_CASE | ID_SUBFOLDER | ID_WORD | ID_INVERT | ID_ONLYNAME | ID_USEOUTPUT | ID_BACKUP
        | ID_LOCK
            if wparam.0 >> 16 == 0 =>
        {
            // BN_CLICKED: the button has already flipped its own check and
            // repainted, so this only carries the new state into the model.
            let _ = sync(st, dlg, true, id);
            1
        }
        ID_ARROW_FIND => {
            on_regex_menu(st, dlg);
            1
        }
        ID_ARROW_REPL => {
            on_replace_menu(st, dlg);
            1
        }
        ID_BROWSE => {
            on_browse(st, dlg);
            1
        }
        ID_ARROW_TYPE => {
            on_filetype_menu(st, dlg);
            1
        }
        ID_ARROW_EXCL => {
            on_exclude_menu(st, dlg);
            1
        }
        ID_FIND_BTN => {
            on_find(st, dlg);
            1
        }
        ID_REPL_BTN => {
            on_replace(st, dlg);
            1
        }
        ID_MONITOR => {
            on_monitor(st, dlg);
            1
        }
        ID_UPDIR => {
            on_updir(st, dlg);
            1
        }
        1408 => {
            on_fill(st, dlg);
            1
        }
        _ => 0,
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
            crate::win32::lang::localize_dialog(hwnd, 220);
            // TRUE: the manager sets the initial focus itself.
            init(hwnd)
        }
        WM_COMMAND => on_command(hwnd, wparam),
        WM_DRAWITEM => on_draw_item(hwnd, lparam),
        WM_ERASEBKGND => {
            // The `#32770` class ships a `COLOR_3DFACE` brush; the space
            // between and behind the controls has to come from Theme.
            let st = state(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            // Labels and checkboxes sit on the dialog face.
            let st = state(hwnd);
            customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                to_colorref(st.theme.toolbar_fg),
                to_colorref(st.theme.toolbar_bg),
                st.bg,
            )
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            // Left to the system the face would be `COLOR_WINDOW` instead, which is a
            // different grey.
            let st = state(hwnd);
            customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                to_colorref(st.theme.toolbar_fg),
                to_colorref(st.theme.toolbar_field),
                st.face,
            )
        }
        WM_NCHITTEST => {
            // Only the left and right edges resize: the corners report those
            // edges and the top and bottom report nowhere.
            let r = DefWindowProcW(hwnd, WM_NCHITTEST, wparam, lparam);
            let hit = r.0 as u32;
            let mapped = match hit {
                HTBOTTOMLEFT | HTTOPLEFT => HTLEFT,
                HTBOTTOMRIGHT | HTTOPRIGHT => HTRIGHT,
                HTTOP | HTBOTTOM => HTNOWHERE,
                other => other,
            };
            mapped as isize
        }
        WM_SIZE => {
            // A minimised window has nothing to lay out.
            if wparam.0 != 1 {
                let cx = (lparam.0 & 0xFFFF) as i32;
                let cy = ((lparam.0 >> 16) & 0xFFFF) as i32;
                resize(state(hwnd), hwnd, cx, cy);
            }
            1
        }
        WM_GETMINMAXINFO => {
            // The window cannot shrink below the size it opened at.
            let st = state(hwnd);
            if st.init_window.0 != -1 && st.init_window.1 != -1 {
                let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
                mmi.ptMinTrackSize = POINT {
                    x: st.init_window.0,
                    y: st.init_window.1,
                };
            }
            1
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            1
        }
        WM_DESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DLG.store(0, Ordering::Release);
            if !ptr.is_null() {
                let mut b = Box::from_raw(ptr);
                for obj in [
                    b.font.0,
                    b.marlett.0,
                    b.symbol.0,
                    b.sys_font.0,
                    b.bg.0,
                    b.face.0,
                    b.edge.0,
                    b.glyph.0,
                ] {
                    let _ = DeleteObject(HGDIOBJ(obj));
                }
                // The tooltip control dies with its own
                // `TipHost::drop`.
                b.tip_host = None;
            }
            0
        }
        _ => 0,
    }
}

/// `PathRemoveFileSpecW`: the path without its last component.
fn parent_of(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid is the template: a duplicated id would make a click
    /// ambiguous, and every resize-table control has to exist.
    #[test]
    fn the_measured_grid_is_kept() {
        let mut ids: Vec<i32> = ITEMS.iter().map(|i| i.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "control ids must be unique");
        assert_eq!(n, 32);
        for (id, _) in RESIZE_TABLE {
            assert!(ITEMS.iter().any(|i| i.id == *id), "resize {id} missing");
        }
    }

    /// The template is the EX form, and every item lands DWORD-aligned.
    #[test]
    fn template_bytes_are_the_ex_form() {
        let b = build_template();
        assert_eq!(&b[0..4], &[1, 0, 0xFF, 0xFF]);
        assert_eq!(u16::from_le_bytes([b[16], b[17]]), 32);
        // Every item starts DWORD-aligned
        // on a 2-byte creation-data count and the manager does not pad it.
        let mut p = 26usize + 2 + 2;
        p += 2;
        while b[p] != 0 || b[p + 1] != 0 {
            p += 2;
        }
        p += 2; // title
        p += 2; // ptsz
        p += 2; // weight
        p += 1; // italic
        p += 1; // charset
        while b[p] != 0 || b[p + 1] != 0 {
            p += 2;
        }
        p += 2; // face
        for i in 0..32 {
            while p % 4 != 0 {
                p += 1;
            }
            assert_eq!(b.len() > p, true, "item {i} runs off the template");
            p += 12; // help, ex, style
            p += 8; // xywh
            p += 4; // id
            p += 4; // class atom
            while b[p] != 0 || b[p + 1] != 0 {
                p += 2;
            }
            p += 2; // title
            p += 2; // creation-data count
        }
    }

    #[test]
    fn preset_surgery_keeps_the_globs() {
        assert_eq!(preset_to_glob("所有文件(*.*)"), "*.*");
        assert_eq!(preset_to_glob("文本文件(*.txt)"), "*.txt");
        assert_eq!(preset_to_glob("C++(*.cpp;*.h)"), "*.cpp;*.h");
    }

    /// Every leaf row inserts its token, which is the caption up to its tab.
    #[test]
    fn regex_rows_insert_their_token() {
        for row in REGEX_ROWS.iter().chain(REGEX_MORE.iter()) {
            if row.id == 0 || row.id == 77 {
                continue;
            }
            let token = row_token(row);
            assert!(!token.is_empty(), "row {} has no token", row.id);
            assert!(row.text.starts_with(token));
        }
        assert_eq!(row_token(&REGEX_ROWS[2]), "\\n");
        assert_eq!(row_token(&REGEX_ROWS[3]), "\\t");
    }

    #[test]
    fn mru_moves_to_head_and_caps() {
        let mut list: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert!(mru_insert(&mut list, "d"));
        assert_eq!(list, ["d", "a", "b", "c"]);
        assert!(!mru_insert(&mut list, "d"));
        assert!(mru_insert(&mut list, "b"));
        assert_eq!(list, ["b", "d", "a", "c"]);
        let mut big: Vec<String> = (0..25).map(|i| format!("s{i}")).collect();
        mru_insert(&mut big, "new");
        assert_eq!(big.len(), 20);
        assert_eq!(big[0], "new");
    }

    /// A missing Item key stops the read, an empty one is skipped, a `#`
    /// prefix is stripped.
    #[test]
    fn mru_load_stops_at_the_first_gap() {
        let mut ini = ini_new();
        ini_set(&mut ini, "GrepFind", "Item1", "foo");
        ini_set(&mut ini, "GrepFind", "Item2", "#bar");
        ini_set(&mut ini, "GrepFind", "Item4", "baz");
        assert_eq!(mru_load(&ini, "GrepFind"), ["foo", "bar"]);
    }

    #[test]
    fn ini_round_trips_and_keeps_other_sections() {
        let dir = std::env::temp_dir().join("grepdlg_test_ini");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("history.ini");
        let _ = std::fs::remove_file(&path);
        let mut ini = ini_new();
        ini_set(&mut ini, "Other", "Keep", "1");
        ini_set(&mut ini, "Grep", "Find", "alpha");
        ini_set(&mut ini, "Grep", "Regex", "Y");
        ini_save(&ini, &path);
        let back = ini_load(&path);
        assert_eq!(ini_get(&back, "Other", "Keep", ""), "1");
        assert_eq!(ini_get(&back, "Grep", "Find", ""), "alpha");
        assert!(ini_get_bool(&back, "Grep", "Regex", false));
        // A missing key falls back to the default, whichever way it points.
        assert!(!ini_get_bool(&back, "Grep", "Case", false));
        assert!(ini_get_bool(&back, "Grep", "Case", true));
        assert_eq!(ini_get(&back, "Grep", "Missing", "d"), "d");
        let _ = std::fs::remove_file(&path);
    }

    /// 清除历史记录 (1236) and the list rewrites drop a whole section while
    /// the rest of the file — the find histories, the window position —
    /// stays byte-for-byte in place.
    #[test]
    fn ini_new_starts_empty_and_remove_section_spares_neighbors() {
        let mut ini = ini_new();
        assert!(ini.to_text().is_empty());
        ini_set(&mut ini, "RecentDoc", "Count", "2");
        ini_set(&mut ini, "RecentDoc", "Item0", "a.txt");
        ini_set(&mut ini, "Session", "Count", "1");
        ini_remove_section(&mut ini, "RecentDoc");
        assert_eq!(ini_get(&ini, "RecentDoc", "Count", "?"), "?");
        assert_eq!(ini_get(&ini, "Session", "Count", "?"), "1");
        // Removing an absent section is a no-op.
        ini_remove_section(&mut ini, "Grep");
        assert_eq!(ini_get(&ini, "Session", "Count", "?"), "1");
    }

    #[test]
    fn codepage_labels_carry_the_number() {
        assert_eq!(codepage_label(0, "Default (ANSI)"), "Default (ANSI)");
        assert_eq!(codepage_label(65001, "UTF-8"), "UTF-8");
        assert_eq!(
            codepage_label(1252, "Western European (Windows)"),
            "Western European (Windows) [1252]"
        );
    }

    #[test]
    fn split_globs_drops_everything() {
        assert!(split_globs("").is_empty());
        assert!(split_globs("*.*").is_empty());
        assert_eq!(split_globs("*.rs; *.txt"), ["*.rs", "*.txt"]);
    }

    #[test]
    fn blank_text_is_not_remembered() {
        assert_eq!(leading_blanks("  \t"), 3);
        assert_eq!(leading_blanks("\u{3000}x"), 1);
        assert_eq!(leading_blanks("x"), 0);
    }

    #[test]
    fn parent_of_strips_the_last_component() {
        assert_eq!(parent_of("C:\\a\\b.txt"), "C:\\a");
        assert_eq!(parent_of("folder"), "");
    }
}
