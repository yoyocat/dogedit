//! 设置字体 — the `cm_config_setfont` (1321) dialog, `工具 → 设置 → 设置字体`.
//!
//! 布局：左边十个字体槽的 ListBox，右边一个 `SysTabControl32`
//! 和「对中日韩字符使用单独的字体」勾，底部 确定 / 取消 / 应用。页里是
//! 名称、大小、字体平滑、加粗、基线（带上下箭头）和一行预览。
//!
//! 与 `encdlg` 同族：`CreateDialogIndirectParamW` 建 `#32770`，`prompt` 的
//! `run_modal_loop` 收消息，`chrome` + `customdraw` 管深浅两套面。`destroyed`
//! 只走裸指针 —— 模态循环那条规则见 `prompt.rs`。
//!
//! 〔应用〕把改过的槽写回 `[FontN]` 后给主窗投 `WM_FONT_APPLIED`；主窗重读配置、
//! 走 `refresh_metrics_and_fonts`。〔确定〕同样写盘，回到帧由 `show_font_dialog`
//! 刷新；〔取消〕一个字节都不写。

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlgtpl::{self, Class, Item};
use crate::win32::wstr::wide;
use std::collections::BTreeSet;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
    EnumFontFamiliesW, FillRect, GetDC, MapWindowPoints, SelectObject, SetBkMode, SetTextColor,
    HBRUSH, HDC, HFONT, HGDIOBJ, LOGFONTW, ReleaseDC, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ICC_TAB_CLASSES, ICC_UPDOWN_CLASS, INITCOMMONCONTROLSEX,
    InitCommonControlsEx, NMHDR, ODS_SELECTED, ODT_TAB, TCIF_TEXT, TCITEMW, TCM_DELETEITEM,
    TCM_GETCURSEL, TCM_GETITEMCOUNT, TCM_GETITEMRECT, TCM_INSERTITEMW, TCM_SETCURSEL,
    TCM_SETITEMSIZE, TCN_SELCHANGE, TCS_OWNERDRAWFIXED, UDM_SETRANGE32, UDM_SETBUDDY,
    UDM_SETPOS32,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BN_CLICKED, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL,
    CBN_EDITCHANGE, CBN_SELCHANGE, CreateDialogIndirectParamW, DefWindowProcW, DestroyWindow,
    DLGTEMPLATE, EN_CHANGE, GetClientRect, GetDlgItem, GetParent, GetWindow, GetWindowTextW,
    GetWindowLongPtrW, GWL_STYLE, GWLP_USERDATA, GW_CHILD, GW_HWNDNEXT, LB_ADDSTRING, LB_GETCURSEL,
    LB_SETCURSEL, LB_SETITEMDATA, LBN_SELCHANGE, PostMessageW, PostQuitMessage, SendMessageW,
    SetWindowLongPtrW, SetWindowTextW, WM_APP, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND,
    WM_DRAWITEM, WM_INITDIALOG, WM_NCDESTROY, WM_NOTIFY, WM_PRINTCLIENT, WM_SETFONT,
    WS_CLIPCHILDREN, HWND_BOTTOM, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SetWindowPos,
};
use windows::core::{PCWSTR, PWSTR};

use dg_render::theme::Theme;

use super::chrome;
use super::customdraw;
use super::fontcfg::{self, Config, FontSpec, SLOT_IDS, VIEW_SLOTS};
use super::prompt::{run_modal_loop, show_modal};
use super::uifont;
use super::view::instance;
use crate::win32::window::to_colorref;

/// 〔应用〕的通知。`WM_APP + 0x4D` 被主题框占了，往上取一个空号。
pub(super) const WM_FONT_APPLIED: u32 = WM_APP + 0x52;

// ---------------------------------------------------------------------------
// 控件 id / 模板
// ---------------------------------------------------------------------------

const ID_OK: i32 = 1;
const ID_CANCEL: i32 = 2;
const ID_APPLY: i32 = 1208;
const ID_LIST: i32 = 1200;
const ID_TAB: i32 = 1206;
const ID_CJK: i32 = 1207;
// 页里的控件
const ID_BOLD: i32 = 1204;
const ID_SMOOTH: i32 = 1205;
const ID_FACE: i32 = 1202;
const ID_SIZE: i32 = 1201;
const ID_PREVIEW: i32 = 1203;
const ID_BASE: i32 = 1209;
const ID_SPIN: i32 = 1210;
const ID_LABEL_FACE: i32 = 3000;
const ID_LABEL_SIZE: i32 = 3001;
const ID_LABEL_SMOOTH: i32 = 3002;
const ID_LABEL_BASE: i32 = 3003;
/// 页底色：盖住 tab 控件自己画的那块浅色显示区。`SysTabControl32` 的
/// `DarkMode_Explorer` 只把页签条的底色给成深色，它自己那块显示区仍是
/// `#F9F9F9` 加一圈白边，选中的页签也还是白面黑字。显示区放一只空 Static，
/// 由 `WM_CTLCOLORSTATIC` 刷成主题色；页签条则改用 `TCS_OWNERDRAWFIXED`
/// 自己画。
///
/// 三层（页签 < 页底板 < 页控件）的显示关系**不是**模板白给的：对话框按
/// 模板逆序建窗口，先建的反而高 —— 页签压着页底板和页控件，页签一重绘
/// （勾选/取消「对中日韩字符使用单独的字体」加减第二页）就把整页盖掉。
/// `init` 里用 `SetWindowPos` 把三层钉死，这里的 `WS_CLIPSIBLINGS` 再保证
/// 页底板重绘时绕开压在它上面的页控件。
const ID_PAGE_PANEL: i32 = 3004;

/// 页底板样式：`WS_CHILD|WS_VISIBLE|WS_CLIPSIBLINGS|WS_GROUP`。
const PANEL_STYLE: u32 = 0x5402_0000;

/// `WS_CHILD|WS_VISIBLE|WS_CLIPSIBLINGS|WS_TABSTOP|TCS_OWNERDRAWFIXED`。
/// 自画页签是唯一的办法：主题给不了选中页签一个深色的面。
const TAB_STYLE: u32 = 0x5401_0000 | TCS_OWNERDRAWFIXED;

const DLG_STYLE: u32 = 0x80c8_08cc;
const DLG_W: u16 = 309;
const DLG_H: u16 = 173;

const ATOM_BUTTON: u16 = 0x0080;
const ATOM_EDIT: u16 = 0x0081;
const ATOM_STATIC: u16 = 0x0082;
const ATOM_LISTBOX: u16 = 0x0083;
const ATOM_COMBOBOX: u16 = 0x0085;

/// 页里的平滑方式：数值就是 `Font=` 的第 4 格，标签复用 `[MENU357]`
/// （255=默认 / 3=不抗锯齿 / 4=抗锯齿 / 5=ClearType —— 后三个正是 GDI 的
/// `NONANTIALIASED_QUALITY` / `ANTIALIASED_QUALITY` / `CLEARTYPE_QUALITY`）。
/// 出厂那行是 1（默认），本实现的编辑器走 DirectWrite，抗锯齿由渲染后端决定，
/// 这一格只保留用户的选择，不悄悄改掉它。
const SMOOTH: [(u8, u32); 4] = [(1, 255), (3, 3), (4, 4), (5, 5)];

/// 大小下拉的常用档。框是可编辑的，用户仍可敲任意 1..200。
const SIZES: &[i32] = &[6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 36, 48, 72];

/// `DLGTEMPLATEEX`，页控件是主对话框的直接子窗（与页对话框显示/隐藏那套
/// 等价但少一层窗口）。模板序给出的 Z 是「先建的高」—— 页签反而压着页底板
/// 和页控件；三层真正的上下关系由 `init` 里的 `SetWindowPos` 钉死，不靠模板
/// 顺序。tab 与页底板都带 `WS_CLIPSIBLINGS`，重绘时各管各的矩形。
fn build_template() -> Vec<u8> {
    let items = [
        Item { id: ID_OK, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0001, x: 197, y: 152, w: 50, h: 14, label: "确定" },
        Item { id: ID_CANCEL, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 252, y: 152, w: 50, h: 14, label: "取消" },
        Item { id: ID_APPLY, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 142, y: 152, w: 50, h: 14, label: "应用" },
        Item { id: ID_LIST, class: Class::Atom(ATOM_LISTBOX), style: 0x50a1_0101, x: 7, y: 7, w: 85, h: 140, label: "" },
        Item { id: ID_CJK, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0003, x: 95, y: 10, w: 205, h: 11, label: "对中日韩字符使用单独的字体" },
        Item { id: ID_TAB, class: Class::Name("SysTabControl32"), style: TAB_STYLE, x: 95, y: 27, w: 205, h: 120, label: "" },
        // 盖住 tab 的浅色显示区。初始矩形只是占位，`init` 里按控件自己报的
        // 页签条高度用 `SetWindowPos` 重算；空格文本保证 Static 会走一遍
        // `WM_CTLCOLORSTATIC` 把整块擦成主题色（纯空文本的 Static 不一定擦）。
        Item { id: ID_PAGE_PANEL, class: Class::Atom(ATOM_STATIC), style: PANEL_STYLE, x: 96, y: 42, w: 203, h: 104, label: " " },
        // 页：控件排在页面板 (96,42) 内侧
        Item { id: ID_LABEL_FACE, class: Class::Atom(ATOM_STATIC), style: 0x5002_0000, x: 100, y: 48, w: 131, h: 10, label: "名称:" },
        Item { id: ID_LABEL_SIZE, class: Class::Atom(ATOM_STATIC), style: 0x5002_0000, x: 242, y: 48, w: 56, h: 10, label: "大小:" },
        // 不要 CBS_SORT：字面表按 BTreeSet 的顺序加进去，行号要与 `faces`
        // 的索引对齐；让公共控件按区域规则重排会把 `CB_SETCURSEL` 指错字体。
        Item { id: ID_FACE, class: Class::Atom(ATOM_COMBOBOX), style: 0x5021_0203, x: 100, y: 60, w: 138, h: 30, label: "" },
        Item { id: ID_SIZE, class: Class::Atom(ATOM_COMBOBOX), style: 0x5021_0202, x: 241, y: 60, w: 53, h: 30, label: "" },
        Item { id: ID_LABEL_SMOOTH, class: Class::Atom(ATOM_STATIC), style: 0x5002_0000, x: 100, y: 77, w: 75, h: 10, label: "字体平滑:" },
        Item { id: ID_LABEL_BASE, class: Class::Atom(ATOM_STATIC), style: 0x5002_0000, x: 243, y: 77, w: 57, h: 10, label: "基线:" },
        Item { id: ID_SMOOTH, class: Class::Atom(ATOM_COMBOBOX), style: 0x5001_0203, x: 100, y: 89, w: 90, h: 30, label: "" },
        Item { id: ID_BOLD, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0003, x: 193, y: 88, w: 45, h: 14, label: "加粗" },
        // 不带 ES_NUMBER：基线可以是 -3，带 ES_NUMBER 的编辑框会把减号吃掉。
        Item { id: ID_BASE, class: Class::Atom(ATOM_EDIT), style: 0x5081_0080, x: 243, y: 89, w: 40, h: 14, label: "" },
        Item { id: ID_SPIN, class: Class::Name("msctls_updown32"), style: 0x5000_00b6, x: 283, y: 88, w: 11, h: 14, label: "" },
        Item { id: ID_PREVIEW, class: Class::Atom(ATOM_STATIC), style: 0x5002_0000, x: 100, y: 107, w: 195, h: 36, label: "AaBbYyZz 测试 サンプル" },
    ];
    dlgtpl::build_legacy("字体设置", DLG_STYLE, 0, DLG_W, DLG_H, 400, &items)
}

// ---------------------------------------------------------------------------
// 字面表
// ---------------------------------------------------------------------------

unsafe extern "system" fn enum_face(
    lf: *const LOGFONTW,
    _tm: *const TEXTMETRICW,
    _kind: u32,
    data: LPARAM,
) -> i32 {
    let set = &mut *(data.0 as *mut BTreeSet<String>);
    if !lf.is_null() {
        let name = &(*lf).lfFaceName;
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        let face = String::from_utf16_lossy(&name[..end]);
        // '@' 开头的是竖排字面，不是用户能选的字体。
        if !face.is_empty() && !face.starts_with('@') {
            set.insert(face);
        }
    }
    1
}

/// 系统字面 + 出厂字面 + 配置里已经写下的字面。后者很重要：一台机器换了配置
/// 文件后，没装的那只字体仍要出现在下拉里，否则一进对话框就被悄悄改成别的。
fn installed_faces(cfg: &Config) -> Vec<String> {
    let mut set = BTreeSet::new();
    unsafe {
        let dc = GetDC(None);
        if !dc.0.is_null() {
            EnumFontFamiliesW(
                dc,
                PCWSTR::null(),
                Some(enum_face),
                LPARAM(&mut set as *mut BTreeSet<String> as isize),
            );
            let _ = ReleaseDC(None, dc);
        }
    }
    set.insert(FontSpec::builtin().face);
    for slot in &cfg.slots {
        if let Some(f) = &slot.font {
            set.insert(f.face.clone());
        }
        if let Some(f) = &slot.cjk {
            set.insert(f.face.clone());
        }
    }
    set.into_iter().collect()
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

struct Dlg {
    frame: HWND,
    cfg: Config,
    /// 当前选中的槽（ListBox 行号）。
    sel: usize,
    /// 0 = 本页字体，1 = 中日韩第二字体。
    tab: usize,
    faces: Vec<String>,
    dpi: i32,
    applied: bool,
    destroyed: bool,
    bg: HBRUSH,
    field: HBRUSH,
    ink: COLORREF,
    face: COLORREF,
    field_face: COLORREF,
    font: HFONT,
    preview: HFONT,
}

fn state_mut(dlg: HWND) -> &'static mut Dlg {
    unsafe {
        let p = GetWindowLongPtrW(dlg, GWLP_USERDATA) as *mut Dlg;
        &mut *p
    }
}

/// 跑一遍模态对话框。返回 `Some(())` 只代表〔确定〕/〔应用〕落过盘；真正的刷新
/// 由帧那边统一做（〔应用〕走 `WM_FONT_APPLIED`，〔确定〕由调用方做）。
pub(super) fn show(frame: HWND, theme: &Theme) -> Option<()> {
    // `SysTabControl32` / `msctls_updown32` 是全工程头一回用：公共控件库要到
    // 注册了这两类之后才认得模板里的类名，否则 `CreateDialogIndirectParamW`
    // 只建出空壳，tab 和上下箭头都是 NULL HWND。
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TAB_CLASSES | ICC_UPDOWN_CLASS,
        };
        let _ = InitCommonControlsEx(&icc);
    }
    let dpi = unsafe { GetDpiForWindow(frame).max(96) as i32 };
    let cfg = Config::load();
    let faces = installed_faces(&cfg);
    let mut st = Box::new(Dlg {
        frame,
        cfg,
        sel: 0,
        tab: 0,
        faces,
        dpi,
        applied: false,
        destroyed: false,
        bg: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
        field: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_field)) },
        ink: to_colorref(theme.toolbar_fg),
        face: to_colorref(theme.toolbar_bg),
        field_face: to_colorref(theme.toolbar_field),
        font: unsafe { uifont::create(dpi) },
        preview: HFONT(std::ptr::null_mut()),
    });
    // 裸指针：模态循环里的 `read_volatile` 只认这个（见 prompt.rs）。Box 在
    // 整段循环里都活着，指针不会悬。
    let destroyed = std::ptr::addr_of!(st.destroyed);
    let param = LPARAM(&mut *st as *mut Dlg as isize);
    let tmpl = build_template();
    let Ok(dlg) = (unsafe {
        CreateDialogIndirectParamW(
            Some(instance()),
            &*(tmpl.as_ptr() as *const DLGTEMPLATE),
            Some(frame),
            Some(dlg_proc),
            param,
        )
    }) else {
        unsafe { st.release_gdi() };
        return None;
    };
    unsafe {
        let dark = chrome::theme_is_dark(theme);
        chrome::set_chrome_theme(theme);
        // 对话框自身不命名给 SetWindowTheme：#32770 会按双倍边框重算矩形。
        chrome::dark_theme_children(dlg, dark);
        chrome::title_bar(dlg, dark);
        let style = GetWindowLongPtrW(dlg, GWL_STYLE) | (WS_CLIPCHILDREN.0 as isize);
        SetWindowLongPtrW(dlg, GWL_STYLE, style);
        show_modal(dlg, frame);
        run_modal_loop(dlg, frame, destroyed);
        st.release_gdi();
    }
    if st.applied { Some(()) } else { None }
}

impl Dlg {
    unsafe fn release_gdi(&mut self) {
        for h in [self.bg, self.field] {
            if !h.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(h.0));
            }
        }
        for h in [self.font, self.preview] {
            if !h.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(h.0));
            }
        }
        self.preview = HFONT(std::ptr::null_mut());
    }
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn ctl(dlg: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(Some(dlg), id).unwrap_or_default() }
}

fn send(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> isize {
    unsafe { SendMessageW(hwnd, msg, Some(w), Some(l)).0 }
}

fn set_text(hwnd: HWND, s: &str) {
    let w = wide(s);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR::from_raw(w.as_ptr()));
    }
}

fn get_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

fn add_string(hwnd: HWND, msg: u32, s: &str) -> isize {
    let mut w = wide(s);
    send(hwnd, msg, WPARAM(0), LPARAM(w.as_mut_ptr() as isize))
}

fn checked(hwnd: HWND) -> bool {
    send(hwnd, BM_GETCHECK, WPARAM(0), LPARAM(0)) != 0
}

fn set_check(hwnd: HWND, on: bool) {
    let _ = send(hwnd, BM_SETCHECK, WPARAM(usize::from(on)), LPARAM(0));
}

/// 下拉里的索引；越界或没选就给 `default`。
fn combo_index(hwnd: HWND, default: usize) -> usize {
    let i = send(hwnd, CB_GETCURSEL, WPARAM(0), LPARAM(0));
    if i < 0 { default } else { i as usize }
}

/// `.lng` 里的「说明\n名称」用的是**字面的两个字符** `\` + `n`，不是换行；
/// 菜单行还带 `&` 助记符（`默认字体(&D)` 去掉 `&` 会留下 `(D)`）。两种残渣
/// 都在这里摘掉，列表和页签用的是同一个口径。
fn clean_label(raw: &str) -> String {
    let tail = raw.rsplit("\\n").next().unwrap_or(raw);
    let tail = tail.rsplit('\n').next().unwrap_or(tail);
    let s = tail.replace('&', "");
    if let Some(open) = s.rfind('(')
        && s.ends_with(')')
        && s[open + 1..s.len() - 1].chars().count() == 1
    {
        return s[..open].trim().to_string();
    }
    s.trim().to_string()
}

/// 槽名：`[MENU128] 1430..1435` 是菜单行，`[STRING] 1436..1439` 是提示行。
fn slot_label(index: usize) -> String {
    let id = SLOT_IDS[index];
    let raw = super::lang::menu(128, id)
        .or_else(|| super::lang::string(id))
        .unwrap_or_else(|| FALLBACK_LABELS[index]);
    clean_label(raw)
}

const FALLBACK_LABELS: [&str; 10] = [
    "默认字体", "自定义字体1", "自定义字体2", "自定义字体3", "自定义字体4",
    "自定义字体5", "输出窗口", "命令窗口", "打印字体", "HEX字体",
];

fn dlg_text(dialog: u32, id: u32, fallback: &str) -> String {
    super::lang::dlg(dialog, id).unwrap_or(fallback).to_string()
}

/// 两页的页签文字，都取自语言文件里现成的行：第一页是 `[STRING] 1430` 的
/// 「说明\n名称」后半（默认字体 / Default font），第二页是 `15276`
/// （中文字体 / CJK）。省掉新造的 `[DLG206]` 键，深浅/中英都不用再加一行。
fn tab_label(index: usize) -> String {
    let id = if index == 0 { 1430 } else { 15276 };
    let raw = super::lang::string(id).unwrap_or(if index == 0 { "默认字体" } else { "中文字体" });
    clean_label(raw)
}

fn smooth_index(value: u8) -> usize {
    SMOOTH.iter().position(|(v, _)| *v == value).unwrap_or(0)
}

fn smooth_value(index: usize) -> u8 {
    SMOOTH.get(index).map(|(v, _)| *v).unwrap_or(1)
}

/// tab 子类只管擦背景。自画模式下控件拿 `COLOR_3DFACE` 的 `#F0F0F0` 擦整条
/// 客户区 —— 页签条上除了 `WM_DRAWITEM` 画出来的那一格全是白的（实测
/// `#F0F0F0` 从 x=175 一直铺到 x=533）。把它换成对话框底色，页签随后照画。
const TAB_SUBCLASS_ID: usize = 0x46;

unsafe extern "system" fn tab_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        let _ = RemoveWindowSubclass(hwnd, Some(tab_proc), TAB_SUBCLASS_ID);
        return DefSubclassProc(hwnd, msg, wparam, lparam);
    }
    if msg == WM_ERASEBKGND || msg == WM_PRINTCLIENT {
        let parent = GetParent(hwnd).unwrap_or_default();
        // 建窗期 GWLP_USERDATA 还是 0，这时别碰 `state_mut`（会解空指针）。
        if !parent.0.is_null() && GetWindowLongPtrW(parent, GWLP_USERDATA) != 0 {
            let st = state_mut(parent);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.bg);
            }
            return LRESULT(1);
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

/// 自画页签要自己定尺寸：`TCS_OWNERDRAWFIXED` 的格子一边大，控件不再按文字
/// 量算宽。宽高跟着 DPI 走，条高随后由 `TCM_GETITEMRECT` 读回给页底板用。
unsafe fn size_tabs(dlg: HWND) {
    let dpi = state_mut(dlg).dpi;
    let w = dpi.clamp(64, 200);
    let h = (22 * dpi / 96).clamp(18, 40);
    // TCM_SETITEMSIZE：LOWORD = 宽，HIWORD = 高。
    let pack = (((h as u32) << 16) | (w as u32 & 0xFFFF)) as isize;
    let _ = send(ctl(dlg, ID_TAB), TCM_SETITEMSIZE, WPARAM(0), LPARAM(pack));
}

/// 把页底板摆到页签条下面。位置不写死模板单位：条高随 DPI / UI 字体变，问
/// 控件自己最准。`MapWindowPoints` 把 tab 客户区坐标换成对话框客户区坐标；
/// 左右和底边用 tab 的整宽整高，连那圈浅色边框一起盖掉。
unsafe fn layout_panel(dlg: HWND) {
    let tab = ctl(dlg, ID_TAB);
    let panel = ctl(dlg, ID_PAGE_PANEL);
    if tab.0.is_null() || panel.0.is_null() {
        return;
    }
    let mut full = RECT::default();
    if GetClientRect(tab, &mut full).is_err() {
        return;
    }
    let mut item = RECT::default();
    let got = send(
        tab,
        TCM_GETITEMRECT,
        WPARAM(0),
        LPARAM(&mut item as *mut RECT as isize),
    );
    let top = if got != 0 && item.bottom > 0 {
        item.bottom
    } else {
        // 一只页签都还没插进来时的兜底，别把整条页签盖掉。
        full.bottom / 5
    };
    let mut origin = [POINT { x: 0, y: 0 }];
    MapWindowPoints(Some(tab), Some(dlg), &mut origin);
    let _ = SetWindowPos(
        panel,
        None,
        origin[0].x,
        origin[0].y + top,
        full.right,
        (full.bottom - top).max(0),
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
}

// ---------------------------------------------------------------------------
// 初始化 / 页切换
// ---------------------------------------------------------------------------

/// `WM_INITDIALOG`：本地化 → UI 字体 → 填表 → 选中出厂槽。
unsafe fn init(dlg: HWND) -> isize {
    let st = state_mut(dlg);
    let font = st.font;
    let mut child = GetWindow(dlg, GW_CHILD).unwrap_or_default();
    while !child.0.is_null() {
        let _ = SendMessageW(child, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
    }
    // 外壳的标题/按钮/勾走 [DLG206]，页里的标签走 [DLG249]。
    super::lang::localize_dialog(dlg, 206);
    for (id, fallback) in [
        (ID_LABEL_FACE, "名称:"),
        (ID_LABEL_SIZE, "大小:"),
        (ID_LABEL_SMOOTH, "字体平滑:"),
        (ID_LABEL_BASE, "基线:"),
        (ID_BOLD, "加粗"),
    ] {
        set_text(ctl(dlg, id), &dlg_text(249, id as u32, fallback));
    }

    // 槽表：行号就是槽号，item data 存命令 id。
    let list = ctl(dlg, ID_LIST);
    for (i, id) in SLOT_IDS.iter().enumerate() {
        let label = slot_label(i);
        let idx = add_string(list, LB_ADDSTRING, &label);
        if idx >= 0 {
            let _ = send(list, LB_SETITEMDATA, WPARAM(idx as usize), LPARAM(*id as isize));
        }
    }

    // 字面 / 大小 / 平滑三只下拉。
    let face_combo = ctl(dlg, ID_FACE);
    let st = state_mut(dlg);
    for face in &st.faces {
        add_string(face_combo, CB_ADDSTRING, face);
    }
    let size_combo = ctl(dlg, ID_SIZE);
    for size in SIZES {
        add_string(size_combo, CB_ADDSTRING, &size.to_string());
    }
    let smooth_combo = ctl(dlg, ID_SMOOTH);
    for (_, key) in SMOOTH {
        let label = super::lang::menu(357, key).unwrap_or("ClearType").to_string();
        add_string(smooth_combo, CB_ADDSTRING, &label);
    }

    // 基线的上下箭头：buddy 是那只 ES_NUMBER 编辑框，范围 -20..20。
    let base = ctl(dlg, ID_BASE);
    let spin = ctl(dlg, ID_SPIN);
    let _ = send(spin, UDM_SETBUDDY, WPARAM(base.0 as usize), LPARAM(0));
    // 与 optionsdlg::link_updown 同一个口径：wParam = 下限，lParam = 上限。
    let _ = send(spin, UDM_SETRANGE32, WPARAM((-20i32) as usize), LPARAM(20));

    // 当前槽是配置里的 `[Font] CurrentFont`。
    let sel = state_mut(dlg).cfg.active;
    state_mut(dlg).sel = sel;
    let _ = send(list, LB_SETCURSEL, WPARAM(sel), LPARAM(0));
    pin_page_zorder(dlg);
    let _ = SetWindowSubclass(ctl(dlg, ID_TAB), Some(tab_proc), TAB_SUBCLASS_ID, 0);
    size_tabs(dlg);
    sync_tabs(dlg);
    load_page(dlg);
    1
}

/// 页控件们（Z 序钉制用）：标签、三只下拉、加粗、基线加箭头、预览。
const PAGE_CONTROLS: [i32; 11] = [
    ID_LABEL_FACE, ID_LABEL_SIZE, ID_FACE, ID_SIZE,
    ID_LABEL_SMOOTH, ID_LABEL_BASE, ID_SMOOTH, ID_BOLD,
    ID_BASE, ID_SPIN, ID_PREVIEW,
];

/// 把「页签 < 页底板 < 页控件」三层 Z 序钉死。
///
/// 对话框按模板**逆序**建窗口：先建的高。模板里页签在页底板、页控件之前，
/// 于是页签反而压着它们 —— 开窗第一帧各控件的绘制顺序碰巧把页画对，但页签
/// 一重绘（勾选/取消「对中日韩字符使用单独的字体」会插/删第二页，插删会让
/// 页签整块失效重画），它的擦除就盖住页底板，页底板下面的页控件跟着被盖：
/// 取消勾选后整页只剩页签，再勾上时只有内容变过的下拉框自己画回来，标签
/// 全部消失。`WS_CLIPSIBLINGS`（页签、页底板都带着）让重绘各管各的矩形，
/// 这里的顺序让「谁盖谁」从此不再靠第一帧的运气。导航顺序是模板序，与 Z
/// 无关，Tab 键的落点不变。
unsafe fn pin_page_zorder(dlg: HWND) {
    let fix = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    let tab = ctl(dlg, ID_TAB);
    let panel = ctl(dlg, ID_PAGE_PANEL);
    // `hWndInsertAfter` 的语义是「放到那只窗口**下面**」：先让页底板垫底，
    // 再把页签插到页底板下面，页控件全部提到最上。
    let _ = SetWindowPos(panel, Some(HWND_BOTTOM), 0, 0, 0, 0, fix);
    let _ = SetWindowPos(tab, Some(panel), 0, 0, 0, 0, fix);
    for id in PAGE_CONTROLS {
        let _ = SetWindowPos(ctl(dlg, id), Some(HWND_TOP), 0, 0, 0, 0, fix);
    }
}

/// tab 数跟着 `UseCJK` 走：关着只有一页，开着补第二页。HEX 槽（1439）不认第二字体，勾也置灰。
unsafe fn sync_tabs(dlg: HWND) {
    let tab = ctl(dlg, ID_TAB);
    let st = state_mut(dlg);
    // HEX 字体槽（1439）不认第二字体。
    let hex = st.sel + 1 == SLOT_IDS.len();
    let want = if hex {
        1
    } else {
        usize::from(st.cfg.slots[st.sel].use_cjk) + 1
    };
    let have = send(tab, TCM_GETITEMCOUNT, WPARAM(0), LPARAM(0)).max(0) as usize;
    if want > have {
        for i in have..want {
            let mut text = wide(&tab_label(i));
            let item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: PWSTR::from_raw(text.as_mut_ptr()),
                ..Default::default()
            };
            let _ = send(
                tab,
                TCM_INSERTITEMW,
                WPARAM(i),
                LPARAM(&item as *const TCITEMW as isize),
            );
        }
    } else if want < have {
        for _ in want..have {
            let _ = send(tab, TCM_DELETEITEM, WPARAM(want), LPARAM(0));
        }
    }
    let st = state_mut(dlg);
    if st.tab >= want {
        st.tab = 0;
    }
    let tab_idx = st.tab;
    let _ = send(tab, TCM_SETCURSEL, WPARAM(tab_idx), LPARAM(0));
    let use_cjk = st.cfg.slots[st.sel].use_cjk;
    let cjk_box = ctl(dlg, ID_CJK);
    let _ = EnableWindow(cjk_box, !hex);
    set_check(cjk_box, use_cjk && !hex);
    // 页签数变了条高不变，但每次都重摆一遍省得建窗期 / DPI 变化漏掉。
    layout_panel(dlg);
}

/// 把当前槽 / 当前 tab 的值写进页控件。
unsafe fn load_page(dlg: HWND) {
    let st = state_mut(dlg);
    let slot = &st.cfg.slots[st.sel];
    let spec = if st.tab == 0 {
        slot.font_or_default()
    } else {
        slot.cjk_or_default()
    };
    let base = if st.tab == 0 { slot.base0 } else { slot.base1 };
    let faces = st.faces.clone();
    let dpi = st.dpi;

    let face_i = faces
        .iter()
        .position(|f| f.eq_ignore_ascii_case(&spec.face))
        .unwrap_or(0);
    let _ = send(ctl(dlg, ID_FACE), CB_SETCURSEL, WPARAM(face_i), LPARAM(0));
    set_text(ctl(dlg, ID_SIZE), &spec.points.to_string());
    let _ = send(
        ctl(dlg, ID_SMOOTH),
        CB_SETCURSEL,
        WPARAM(smooth_index(spec.smooth)),
        LPARAM(0),
    );
    set_check(ctl(dlg, ID_BOLD), spec.bold);
    set_text(ctl(dlg, ID_BASE), &base.to_string());
    let _ = send(
        ctl(dlg, ID_SPIN),
        UDM_SETPOS32,
        WPARAM(0),
        LPARAM(base as isize),
    );
    update_preview(dlg, &spec, dpi as u32);
}

/// 页控件 → 当前槽。每次控件变更都收一遍，切槽/切页前也会先收，不会丢改动。
unsafe fn store_page(dlg: HWND) {
    let st = state_mut(dlg);
    let sel = st.sel;
    let tab = st.tab;
    let faces = st.faces.clone();
    let old = if tab == 0 {
        st.cfg.slots[sel].font_or_default()
    } else {
        st.cfg.slots[sel].cjk_or_default()
    };
    let face = faces
        .get(combo_index(ctl(dlg, ID_FACE), usize::MAX))
        .cloned()
        .unwrap_or(old.face.clone());
    let points = get_text(ctl(dlg, ID_SIZE))
        .trim()
        .parse::<i32>()
        .ok()
        .filter(|p| (1..=200).contains(p))
        .unwrap_or(old.points);
    let base = get_text(ctl(dlg, ID_BASE))
        .trim()
        .parse::<i32>()
        .ok()
        .unwrap_or(0)
        .clamp(-20, 20);
    let spec = FontSpec {
        face,
        points,
        bold: checked(ctl(dlg, ID_BOLD)),
        smooth: smooth_value(combo_index(ctl(dlg, ID_SMOOTH), 0)),
        // 页里没有斜体勾，出厂值原样带过去。
        italic: old.italic,
    };
    let slot = &mut state_mut(dlg).cfg.slots[sel];
    // 拿「展示值」比，不拿 `Option` 比：一个从没配过的槽显示的就是出厂行，
    // 光切一次槽不该把它标成改过、更不该凭空往用户的 ini 里写一整段。
    if tab == 0 {
        if old != spec {
            slot.font = Some(spec.clone());
            slot.dirty = true;
        }
        if slot.base0 != base {
            slot.base0 = base;
            slot.dirty = true;
        }
    } else {
        if old != spec {
            slot.cjk = Some(spec.clone());
            slot.dirty = true;
        }
        if slot.base1 != base {
            slot.base1 = base;
            slot.dirty = true;
        }
    }
    let dpi = state_mut(dlg).dpi as u32;
    update_preview(dlg, &spec, dpi);
}

/// 预览用选中那只字面按页里的字号重建 HFONT。旧句柄在替换前删掉。
unsafe fn update_preview(dlg: HWND, spec: &FontSpec, dpi: u32) {
    let st = state_mut(dlg);
    let Some(font) = super::gdi::create_editor_font(dpi, &spec.into_face(spec.points as f32))
    else {
        return;
    };
    let old = st.preview;
    st.preview = font;
    let _ = SendMessageW(
        ctl(dlg, ID_PREVIEW),
        WM_SETFONT,
        Some(WPARAM(font.0 as usize)),
        Some(LPARAM(1)),
    );
    if !old.0.is_null() {
        let _ = DeleteObject(HGDIOBJ(old.0));
    }
}

// ---------------------------------------------------------------------------
// 按钮 / 消息
// ---------------------------------------------------------------------------

/// 〔确定〕和〔应用〕共用：收页、写盘、让内存里的配置立刻变成刚写的那份。
unsafe fn apply(dlg: HWND, notify: bool) {
    store_page(dlg);
    let st = state_mut(dlg);
    // 选中 0..5 号槽时，它就是当前编辑视图用的字体；6 号往后是输出/命令/打印/
    // HEX，这些槽不切换视图字体，`CurrentFont` 保持原值。
    if st.sel < VIEW_SLOTS {
        st.cfg.active = st.sel;
    }
    fontcfg::commit(&st.cfg);
    for slot in &mut st.cfg.slots {
        slot.dirty = false;
    }
    st.applied = true;
    if notify {
        let _ = PostMessageW(Some(st.frame), WM_FONT_APPLIED, WPARAM(0), LPARAM(0));
    }
}

unsafe fn end(dlg: HWND) {
    let _ = DestroyWindow(dlg);
}

/// `TCS_OWNERDRAWFIXED`：页签条由对话框画。选中的格子用「输入框面」当抬起的
/// 面，未选中的直接融进对话框底色；两种主题都只用 `chrome` 那套配色，不碰
/// 系统色。`itemState` 里只认 `ODS_SELECTED`，焦点框交给控件自己。
unsafe fn on_draw_item(dlg: HWND, lparam: LPARAM) -> isize {
    if lparam.0 == 0 {
        return 0;
    }
    let di = &*(lparam.0 as *const DRAWITEMSTRUCT);
    if di.CtlType != ODT_TAB || di.CtlID != ID_TAB as u32 {
        return 0;
    }
    let st = state_mut(dlg);
    let selected = di.itemState.0 & ODS_SELECTED.0 != 0;
    let brush = if selected { st.field } else { st.bg };
    let ink = st.ink;
    let font = st.font;
    let _ = FillRect(di.hDC, &di.rcItem, brush);
    let old = SelectObject(di.hDC, HGDIOBJ(font.0));
    let _ = SetBkMode(di.hDC, TRANSPARENT);
    SetTextColor(di.hDC, ink);
    // wide() 带结尾 0，DrawTextW 按切片长度取字，不能把 NUL 也画进去。
    let mut text = wide(&tab_label(di.itemID as usize));
    let n = text.len().saturating_sub(1);
    let mut rc = di.rcItem;
    let _ = DrawTextW(
        di.hDC,
        &mut text[..n],
        &mut rc,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
    let _ = SelectObject(di.hDC, old);
    1
}

unsafe fn on_command(dlg: HWND, wparam: WPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as i32;
    let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
    match id {
        ID_OK => {
            apply(dlg, false);
            end(dlg);
            1
        }
        ID_CANCEL => {
            end(dlg);
            1
        }
        ID_APPLY if code == BN_CLICKED => {
            apply(dlg, true);
            1
        }
        // 槽切换：先把当前页收进旧槽，再装新槽。
        ID_LIST if code == LBN_SELCHANGE => {
            store_page(dlg);
            let i = send(ctl(dlg, ID_LIST), LB_GETCURSEL, WPARAM(0), LPARAM(0));
            if i >= 0 {
                state_mut(dlg).sel = (i as usize).min(SLOT_IDS.len() - 1);
                state_mut(dlg).tab = 0;
                sync_tabs(dlg);
                load_page(dlg);
            }
            1
        }
        // CJK 勾：加/删第二页。
        ID_CJK if code == BN_CLICKED => {
            store_page(dlg);
            let st = state_mut(dlg);
            let on = st.cfg.slots[st.sel].use_cjk;
            st.cfg.slots[st.sel].use_cjk = !on;
            st.cfg.slots[st.sel].dirty = true;
            if on {
                st.tab = 0;
            }
            sync_tabs(dlg);
            load_page(dlg);
            1
        }
        // 页里的编辑：收一遍并刷新预览。
        ID_FACE | ID_SIZE if code == CBN_SELCHANGE || code == CBN_EDITCHANGE => {
            store_page(dlg);
            1
        }
        ID_SMOOTH if code == CBN_SELCHANGE => {
            store_page(dlg);
            1
        }
        ID_BOLD if code == BN_CLICKED => {
            store_page(dlg);
            1
        }
        ID_BASE if code == EN_CHANGE => {
            store_page(dlg);
            1
        }
        _ => 0,
    }
}

unsafe fn on_notify(dlg: HWND, lparam: LPARAM) -> isize {
    if lparam.0 == 0 {
        return 0;
    }
    let nm = &*(lparam.0 as *const NMHDR);
    if nm.code == TCN_SELCHANGE && nm.idFrom == ID_TAB as usize {
        store_page(dlg);
        let tab = send(ctl(dlg, ID_TAB), TCM_GETCURSEL, WPARAM(0), LPARAM(0));
        state_mut(dlg).tab = if tab > 0 { 1 } else { 0 };
        load_page(dlg);
        return 1;
    }
    0
}

unsafe extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    // 组合框的内部编辑框、tab、上下箭头在建窗期间就会给父窗送 WM_CTLCOLOR* /
    // WM_COMMAND（UDS_SETBUDDYINT 填 buddy 还会带一条 EN_CHANGE）。那发生在
    // WM_INITDIALOG 之前，GWLP_USERDATA 还是 0 —— 直接解引用会 abort。这些
    // 早期消息一律交给默认处理；`init()` 稍后会把每个控件的值重填一遍。
    if msg == WM_DESTROY && GetWindowLongPtrW(hwnd, GWLP_USERDATA) == 0 {
        PostQuitMessage(0);
        return 0;
    }
    if msg != WM_INITDIALOG && GetWindowLongPtrW(hwnd, GWLP_USERDATA) == 0 {
        return DefWindowProcW(hwnd, msg, wparam, lparam).0;
    }
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            init(hwnd)
        }
        WM_COMMAND => on_command(hwnd, wparam),
        WM_DRAWITEM => on_draw_item(hwnd, lparam),
        WM_NOTIFY => on_notify(hwnd, lparam),
        WM_ERASEBKGND | WM_PRINTCLIENT => {
            // tab 控件和原生勾会用 `WM_PRINTCLIENT` 问父窗要「自己那一块的底色」，
            // 而不是 `WM_CTLCOLORBTN`；`#32770` 的默认回答是 COLOR_3DFACE
            // `#F0F0F0`，整页就会白成一块（markdlg 里踩过同一个坑）。两种消息
            // 都用主题刷子填，tab 条和它底下的页面才跟对话框一个颜色。
            let st = state_mut(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = state_mut(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.face, st.bg)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let st = state_mut(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.field_face, st.field)
        }
        WM_CLOSE => {
            end(hwnd);
            1
        }
        WM_DESTROY => {
            state_mut(hwnd).destroyed = true;
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam).0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_a_dlgtemplateex_with_every_control() {
        let b = build_template();
        assert_eq!(u16::from_le_bytes([b[0], b[1]]), 1, "dlgVer");
        assert_eq!(u16::from_le_bytes([b[2], b[3]]), 0xFFFF, "EX signature");
        // dlgVer(0) + signature(2) + helpID(4) + exStyle(8) + style(12) +
        // cItems(16) + x(18) + y(20) + cx(22) + cy(24) + menu(26) + class(28)。
        let count = u16::from_le_bytes([b[16], b[17]]);
        assert_eq!(count, 18);
        assert_eq!(u16::from_le_bytes([b[22], b[23]]), DLG_W);
        assert_eq!(u16::from_le_bytes([b[24], b[25]]), DLG_H);
    }

    #[test]
    fn the_tab_is_owner_drawn_and_the_panel_is_a_dialog_child() {
        // 自画位没钉住的话，Windows 会把选中的页签画成白面黑字。
        assert_ne!(TAB_STYLE & TCS_OWNERDRAWFIXED, 0);
        // 页底板必须和页控件同属对话框，三层 Z 序在 init 里用 SetWindowPos
        // 钉死（模板序给出的是反的，见 pin_page_zorder）。
        assert_eq!(ID_PAGE_PANEL, 3004);
        assert_ne!(ID_PAGE_PANEL, ID_TAB);
        // 页底板绕开页控件重绘的那一位：没有它，页底板一重绘就盖掉整页。
        assert_ne!(PANEL_STYLE & 0x0400_0000, 0, "WS_CLIPSIBLINGS");
    }

    /// Z 序钉制覆盖页里每一只控件：漏掉哪只，哪只就会在页签/页底板重绘时
    /// 被盖掉。
    #[test]
    fn the_zorder_pin_covers_every_page_control() {
        let expected = [
            ID_LABEL_FACE, ID_LABEL_SIZE, ID_FACE, ID_SIZE,
            ID_LABEL_SMOOTH, ID_LABEL_BASE, ID_SMOOTH, ID_BOLD,
            ID_BASE, ID_SPIN, ID_PREVIEW,
        ];
        assert_eq!(PAGE_CONTROLS, expected);
        for id in PAGE_CONTROLS {
            assert_ne!(id, ID_PAGE_PANEL);
            assert_ne!(id, ID_TAB);
        }
    }

    #[test]
    fn the_smooth_values_round_trip_through_the_combo() {
        for (i, (value, _)) in SMOOTH.iter().enumerate() {
            assert_eq!(smooth_index(*value), i);
            assert_eq!(smooth_value(i), *value);
        }
        // 出厂值 1 必须是第一项，没见过的字节落到出厂值。
        assert_eq!(SMOOTH[0].0, 1);
        assert_eq!(smooth_value(99), 1);
    }

    #[test]
    fn the_size_list_contains_the_factory_size() {
        assert!(SIZES.contains(&(super::super::gdi::EDITOR_POINTS as i32)));
        assert!(SIZES.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn every_slot_has_a_label() {
        for i in 0..SLOT_IDS.len() {
            assert!(!slot_label(i).is_empty());
        }
        assert!(!FALLBACK_LABELS.iter().any(|s| s.is_empty()));
    }

    #[test]
    fn lng_literal_newlines_and_mnemonics_are_stripped() {
        // `.lng` 里存的是字面的 `\n`，不是换行符。
        assert_eq!(clean_label("说明\\n默认字体"), "默认字体");
        assert_eq!(clean_label("默认字体(&D)"), "默认字体");
        assert_eq!(clean_label("&Default font"), "Default font");
        assert_eq!(clean_label("Custom font 1"), "Custom font 1");
        // 真正的括号说明不是助记符，不能吞。
        assert_eq!(clean_label("字体 (等宽)"), "字体 (等宽)");
        assert_eq!(tab_label(0), clean_label(crate::win32::lang::string(1430).unwrap_or("默认字体")));
    }
}
