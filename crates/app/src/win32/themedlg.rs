//! 主题 — a standalone modeless dialog: the installed themes on the left, the
//! selected theme's colour table on the right.
//!
//! Why a dialog of its own rather than a page inside 设置: this table is the
//! *subject*, not one value among twenty. It wants the whole window, its own
//! theme list, and cells that are the thing you click. 设置 keeps the options
//! that are one value each.
//!
//! Three pieces of state:
//!
//! * **which theme** — the left list, one entry per name from
//!   [`super::themeext::names`], the same list the theme menu walks.
//! * **what the table shows** — one row per `[Theme]` key of that theme, in the
//!   file's own spelling. The runtime token names (`editor.bg` …) are a
//!   *different* vocabulary: writing those into the ini would read back as
//!   nothing, so the table must speak the file's.
//! * **where an edit goes** — [`super::themeext::write_key`], the only path that
//!   writes an ini. It copies the bundled text out first if the user has never
//!   edited this theme.
//!
//! The dialog does **not** re-install the theme on its own. The frame watches
//! the file (fingerprint every 700 ms) and reloads on its own, so a key edit
//! only posts [`WM_THEME_DLG`] with [`APPLY`] to skip the wait — and that one
//! reinstalls **only if the selected theme is the live one**: the user may be
//! editing a theme that is not in effect, and then the writes stay in the
//! file. Switching is the 应用 button's job: it posts [`SWITCH`], which
//! installs the selected theme whatever the live one is.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlggeom;
use crate::win32::dpi::scale;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreatePen,
    CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, ExtTextOutW, FillRect,
    FrameRect, GetTextExtentPoint32W, HBRUSH, HDC, HFONT, HGDIOBJ, InvalidateRect,
    OUT_DEFAULT_PRECIS, PS_SOLID, Polyline, SelectObject, SetBkColor, SetBkMode, SetTextColor,
    ETO_CLIPPED, TRANSPARENT,
};
use windows::Win32::Graphics::Gdi::BACKGROUND_MODE;
use windows::Win32::UI::Controls::Dialogs::{
    CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW,
};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_POSTPAINT, CDDS_PREPAINT, CDDS_SUBITEM, CDRF_DODEFAULT, CDRF_NEWFONT,
    CDRF_NOTIFYITEMDRAW, CDRF_NOTIFYPOSTPAINT, CDRF_NOTIFYSUBITEMDRAW, CDRF_SKIPDEFAULT,
    DRAWITEMSTRUCT, HDM_GETITEMCOUNT, HDM_GETITEMRECT, ICC_BAR_CLASSES,
    INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCF_FMT, LVCF_TEXT, LVCF_WIDTH, LVCFMT_LEFT,
    LVCOLUMNW, LVIF_TEXT, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_GETCOLUMNW,
    LVM_GETHEADER, LVM_GETITEMSTATE, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETBKCOLOR,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR,
    LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP, LVS_REPORT, LVS_SHOWSELALWAYS,
    LVS_SINGLESEL, MEASUREITEMSTRUCT, NMCUSTOMDRAW, NMHDR, NMITEMACTIVATE, NM_CLICK, NM_CUSTOMDRAW,
    NM_DBLCLK, NMLVCUSTOMDRAW, ODS_SELECTED,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateDialogIndirectParamW, CreateWindowExW, DestroyIcon, DestroyWindow, GA_ROOT, GetAncestor,
    GetClientRect, GetDlgItem, GetWindowLongPtrW,
    HICON, ICON_BIG, ICON_SMALL, IsDialogMessageW,
    PostMessageW, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, SendMessageW,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, BS_OWNERDRAW,
    DLGTEMPLATE, DWLP_MSGRESULT, GWLP_USERDATA, HMENU, IDYES, LB_ADDSTRING, LB_GETCURSEL,
    LB_RESETCONTENT,
    LB_SETCURSEL, LB_SETITEMHEIGHT, LBN_SELCHANGE, LBS_NOTIFY, LBS_NOINTEGRALHEIGHT,
    LBS_OWNERDRAWFIXED, MB_ICONQUESTION, MB_YESNO, MINMAXINFO, MSG, SW_HIDE, SW_SHOW, WM_APP,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC,
    WM_DESTROY,
    WM_DRAWITEM, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_INITDIALOG, WM_MEASUREITEM, WM_NCDESTROY,
    WM_NOTIFY, WM_PRINTCLIENT, WM_SETICON, WM_SETFONT, WM_SETREDRAW, WS_CAPTION, WS_CHILD,
    WS_CLIPCHILDREN,
    WS_POPUP,
    WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL, WINDOW_EX_STYLE, WINDOW_LONG_PTR_INDEX,
    WINDOW_STYLE,
};
use windows::core::{PCWSTR, w};

use crate::win32::chrome;
use crate::win32::ownerdraw;
use crate::win32::prompt;
use crate::win32::view::instance;
use crate::win32::window::{frame_icon, to_colorref};
use dg_render::theme::{luma_blend, Color, Theme};
use dg_skin::metrics::Clamp;

/// 主题对话框给主窗口的消息。`wparam` 是 [`APPLY`]。
pub(super) const WM_THEME_DLG: u32 = WM_APP + 0x4D;

/// 把当前选中的主题装回去 —— 跳过热重载那 700 ms 的等待。
///
/// 只在「选中的就是生效主题」时重装：改键落盘走的是它，用户改一个没生效的
/// 主题时只写文件，屏幕不动。
pub(super) const APPLY: usize = 0;

/// 把对话框里选中的主题装上去，**哪怕它不是当前生效的那个** —— 那就是切换
/// 主题，应用 按钮发的是它。
pub(super) const SWITCH: usize = 1;

const LIST_CLASS: &str = "ListBox";
const BUTTON_CLASS: &str = "Button";
const STATIC_CLASS: &str = "Static";
const GRID_CLASS: &str = "SysListView32";

const ID_THEMES: i32 = 1200;
const ID_ADD: i32 = 1201;
const ID_DEL: i32 = 1202;
const ID_GRID: i32 = 1203;
const ID_APPLY: i32 = 1204;
const ID_SAVE: i32 = 1205;
const ID_CLOSE: i32 = 1206;
const ID_STATUS: i32 = 1207;
/// 几何页（`[Metrics]`）的表格。
const ID_METRICS: i32 = 1208;
/// 两个页签按钮：颜色 / 几何。
const ID_TAB_COLORS: i32 = 1209;
const ID_TAB_METRICS: i32 = 1210;
/// 字体页（`[Fonts]`）的表格。
const ID_FONTS: i32 = 1211;
const ID_TAB_FONTS: i32 = 1212;
/// 形状页（`[Shape]`）的表格。
const ID_SHAPES: i32 = 1213;
const ID_TAB_SHAPES: i32 = 1214;

/// 页签：右半边这张表显示哪一段。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Colors,
    Metrics,
    Fonts,
    Shapes,
}

/// Esc / 关闭框的命令号。
const ID_CANCEL_CMD: i32 = 2;

/// 客户区，96 dpi 像素。控件坐标都在这个坐标系里，跑起来再乘 dpi。
///
/// 十二列要 548 px，加上左边的主题列表和两条边距就是 780。高度按 20 px 一行
/// 算：颜色表 45 行，336 只看得见 11 行 —— 一屏半列就没了，滚起来不知道自己在
/// 哪儿。400 给到 14 行，配合左边主题列表一起长，两张表底边仍然对齐。
const CLIENT_W: i32 = 780;
const CLIENT_H: i32 = 400;

/// 表格的列：区域 / 背景 / 前景 / 值 + `[Style]` 的八位。宽度是 96 dpi 像素。
///
/// 后八列的顺序是给人看的顺序（粗 斜 矩 下 删 双 点 波），不是位顺序 ——
/// 位顺序见 `dg_syntax::STYLE_BOLD` 上面那张表，两者无关。
const COLUMNS: [(&str, i32); 12] = [
    ("区域", 138),
    ("背景", 50),
    ("前景", 50),
    ("值", 124),
    ("粗", 26),
    ("斜", 26),
    ("矩", 26),
    ("下", 26),
    ("删", 26),
    ("双", 26),
    ("点", 26),
    ("波", 26),
];

// 颜色表的列：0 区域 / 1 背景 / 2 前景 / 3 值 / 4.. 八个字形位（顺序同 `COLUMNS`）。
// 只有 1、2 两个需要名字 —— 别处都按下标一次写完整行。
const COL_BG: usize = 1;
const COL_FG: usize = 2;
/// 第一个字形列。这一列往后每一列对应 `[Style]` 的一位。
const COL_STYLE0: usize = 4;

/// 几何页的列：项目 / 值 / 范围 / 键名。宽度是 96 dpi 像素。
///
/// 键名留在最后一列是给手改 ini 用的：中文那一列是给人找的，写文件要的是
/// `status.height` 这种拼法。
const METRIC_COLUMNS: [(&str, i32); 4] =
    [("项目", 160), ("值", 64), ("范围", 96), ("键名", 280)];

/// 字体页的列：一个 token 一行，四个字段各一列。
///
/// 六个 token 就是 `[Fonts]` 的全部（`FontToken::ALL`），不像颜色那样有几十行 ——
/// 所以这一页是「横着看一行就是一个字体」。
const FONT_COLUMNS: [(&str, i32); 6] = [
    ("字体", 70),
    ("用途", 132),
    ("字面", 186),
    ("字号", 62),
    ("字重", 62),
    ("斜体", 50),
];

/// 形状页的列：区域 / 键 / 状态 / 值 / 键名。
///
/// 只列**接了绘制**的区域（`WIRED_REGIONS` 那三个），一个区域认几个键就几行 ——
/// 二十九个区域全铺出来会是一百多行「写了不生效」，把能用的那十行淹掉。
///
/// 键名留在最后一列的理由和几何页一样：中文那两列是给人找的，写文件要的是
/// `tab.states.active.radius` 这种拼法，`states` 是中缀这一点只能靠它说清楚。
const SHAPE_COLUMNS: [(&str, i32); 5] = [
    ("区域", 100),
    ("键", 74),
    ("状态", 76),
    ("值", 52),
    ("键名", 320),
];

/// 形状页「值」列的下标（双击认「键」和「值」两列，和几何页一样）。
const COL_SHAPE_VALUE: usize = 3;

/// 八列 → 位。下标 `i` 对应 `COL_STYLE0 + i`。
///
/// 和 `dg_syntax` 里的 `STYLE_*` 一一对应，这里重写一遍是因为对话框不
/// 依赖 `dg-syntax` 的常量名（列顺序是界面的事）。
const STYLE_BITS: [u32; 8] = [1, 2, 4, 16, 8, 32, 64, 128];

/// 下划线一族（下 / 双 / 点 / 波）：语义上互斥，点其中一列要清掉其余三位。
const UNDERLINE_FAMILY: u32 = 16 | 32 | 64 | 128;

/// 点一列之后的新值。
///
/// 下划线那一族是**单选**：同一条线不可能又是点又是波浪，同时置好几位只会
/// 让画的时候挑一个、文件里留三个矛盾的值。其余位是各自独立的开关。
fn toggled_style(style: u32, bit: u32) -> u32 {
    if bit & UNDERLINE_FAMILY != 0 {
        if style & bit != 0 {
            style & !UNDERLINE_FAMILY
        } else {
            (style & !UNDERLINE_FAMILY) | bit
        }
    } else {
        style ^ bit
    }
}

/// 主题列表的一行高。
const ROW_H: i32 = 20;

/// 表格里的一行 —— `[Theme]` 的一个键。
#[derive(Clone)]
struct Row {
    key: String,
    /// 中文区域名。`[Theme]` 的键是英文拼写（写文件要用），这一列是给人看的。
    label: &'static str,
    bg: Option<Color>,
    fg: Option<Color>,
    /// 这一行**读进来时**的值（合成本身：文件里有就是文件那个，没有就是内置那个）。
    ///
    /// `保存` 拿它判断「这次会话里动过没有」。改色是**立刻**落盘的，所以这里不是
    /// 「盘上现在是什么」而是「打开对话框时是什么」—— 按一下保存报的是这次改了
    /// 几行，而不是把已经写过的再数一遍。
    base: (Option<Color>, Option<Color>),
    /// `[Style]` 的**原值**：字形位只占低八位，其余位原样带着 —— 只改一位
    /// 不能把别的位抹掉。
    style: u32,
    /// 从文件读到的原值。`保存` 只写变过的那几行，没动过的主题不会因为
    /// 按一下保存就长出 45 行 `[Style]`。
    orig: u32,
}

struct Dlg {
    frame: HWND,
    theme: Theme,
    font: HFONT,
    bg: HBRUSH,
    face: HBRUSH,
    face_hot: HBRUSH,
    edge: HBRUSH,
    dpi: f32,
    /// 左列表的名字。`+` / `×` 会重建它，所以是 `RefCell`。
    names: RefCell<Vec<String>>,
    /// 左列表选中的那个主题。
    cur: Cell<usize>,
    rows: RefCell<Vec<Row>>,
    /// 右半边现在显示哪一段：颜色 / 几何。
    page: Cell<Page>,
    /// 几何页的行，顺序就是 `MetricId::ALL`。
    metrics: RefCell<Vec<MetricRow>>,
    /// 字体页的行，顺序就是 `FontToken::ALL`。
    fonts: RefCell<Vec<FontRow>>,
    /// 形状页的行：已接线区域 × 它认的键，常态一行，写过的状态层各一行。
    shapes: RefCell<Vec<ShapeRow>>,
    /// 标题栏的两张图标（大 / 小）。`LoadImageW` 交给我们的是自己的一份，
    /// 窗口销毁时要还回去。
    icons: Cell<(isize, isize)>,
}

/// 几何页的一行。
#[derive(Clone, Copy)]
struct MetricRow {
    id: dg_skin::MetricId,
    value: f32,
    /// 读进来时的值（夹过的）。`保存` 只写变过的那几行，和颜色那套一个规矩：
    /// 改一项几何是立刻落盘的，所以按一下保存报的是这次改了几项。
    orig: f32,
}

/// 字体页的一行 —— 一个 token 的四项。
#[derive(Clone)]
struct FontRow {
    token: dg_skin::FontToken,
    spec: dg_skin::FontSpec,
    orig: dg_skin::FontSpec,
    /// 文件里本来有没有这个 token。
    ///
    /// 没有的时候四个格子是**空的**，不是「9 pt / 400」：`FontSpec::default()`
    /// 只是「字段缺省时填什么」，而没写这个 token 时真正生效的是内置字体
    /// （由系统字面和 `setting.ini` 决定）。把 9 pt 摆出来会让人以为编辑器现在
    /// 就是 9 pt。
    had: bool,
}

/// 形状页的一行 —— 一个区域的**一个键**在**一层**上的值。
///
/// 常态那一层每个键都有一行（`state` 是 `None`）；状态层只有文件里真写了才有
/// 行，所以这一页的行数会随文件长（`tab.states.active.radius=0` 就多一行）。
/// 状态层只能在文件里新建 —— 表里没有「加一层」的地方，硬塞一个空行进去
/// 等于让人手打状态名。
#[derive(Clone, Copy)]
struct ShapeRow {
    region: &'static str,
    key: dg_skin::ShapeKey,
    /// `None` = 常态层；`Some("active")` = `tab.states.active.*`。
    state: Option<&'static str>,
    /// 这一层的值（配方解析时已经按 `ShapeKey::range` 夹过）。
    ///
    /// 常态行显示的是**这一层写了多少**，没写过就是 0 —— 和几何页不同，形状键
    /// 的出厂值全是 0（「不圆角、不留缝、不描边」），摆一个空格子反而要多解释
    /// 一遍「空等于 0」。
    value: f32,
    /// 读进来时的值。`保存` 只写变过的那几行。
    orig: f32,
}

static DLG: AtomicIsize = AtomicIsize::new(0);

fn dlg_hwnd() -> Option<HWND> {
    let v = DLG.load(Ordering::Acquire);
    (v != 0).then_some(HWND(v as *mut core::ffi::c_void))
}

/// Whether the frame's loop should feed `msg` to this dialog.
pub(super) fn translate(msg: &MSG) -> bool {
    let Some(dlg) = dlg_hwnd() else {
        return false;
    };
    if unsafe { GetAncestor(msg.hwnd, GA_ROOT) } != dlg {
        return false;
    }
    unsafe { IsDialogMessageW(dlg, msg).as_bool() }
}

/// 当前选中的主题名 —— 主窗口收到 [`WM_THEME_DLG`] 后用它决定装哪个。
pub(super) fn current_name() -> Option<String> {
    let dlg = dlg_hwnd()?;
    let st = unsafe { state(dlg) };
    st.names.borrow().get(st.cur.get()).cloned()
}

/// 生效主题刚被换掉（应用 装上了一个新的）：对话框自己也跟上。
///
/// 表里每一格、左列表每一行的颜色每轮自绘都读 [`Dlg`] 里那份快照，把它换掉
/// 就够；要动手的是建窗时造的那四把刷子和建表时设过一次的表底/表字色 ——
/// 不换的话编辑器已经换了衣裳，对话框还穿着上一件。子控件的深浅（滚动条、
/// 表头）也得重新声明一遍。框没开着就是无事可做。
pub(super) fn retheme(theme: &Theme) {
    let Some(dlg) = dlg_hwnd() else {
        return;
    };
    unsafe {
        let st = &mut *(GetWindowLongPtrW(dlg, GWLP_USERDATA) as *mut Dlg);
        st.theme = theme.clone();
        let dark = chrome::theme_is_dark(&st.theme);
        // 四把刷子按建窗时同一个配方重造：先还旧的，再要新的。
        for (slot, color) in [
            (&mut st.bg, st.theme.background),
            (&mut st.face, st.theme.panel_bg),
            (
                &mut st.face_hot,
                luma_blend(st.theme.foreground, st.theme.panel_bg, 0.08),
            ),
            (&mut st.edge, st.theme.indent_line),
        ] {
            let _ = DeleteObject(HGDIOBJ(slot.0));
            *slot = CreateSolidBrush(to_colorref(color));
        }
        for id in [ID_GRID, ID_METRICS, ID_FONTS, ID_SHAPES] {
            if let Some(grid) = item(dlg, id) {
                let bg = to_colorref(st.theme.background).0 as isize;
                let fg = to_colorref(st.theme.foreground).0 as isize;
                let _ = SendMessageW(grid, LVM_SETBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg)));
                let _ = SendMessageW(grid, LVM_SETTEXTBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg)));
                let _ = SendMessageW(grid, LVM_SETTEXTCOLOR, Some(WPARAM(0)), Some(LPARAM(fg)));
            }
        }
        chrome::dark_theme_children(dlg, dark);
        let _ = InvalidateRect(Some(dlg), None, true);
    }
}

pub(super) struct Request<'a> {
    pub frame: HWND,
    pub theme: &'a Theme,
    pub name: &'a str,
}

/// Show the dialog, creating it the first time.
pub(super) fn show(req: Request<'_>) {
    unsafe {
        if let Some(existing) = dlg_hwnd() {
            let _ = ShowWindow(existing, SW_SHOW);
            select_theme(existing, req.name);
            return;
        }
        let dpi = GetDpiForWindow(req.frame).max(96) as f32 / 96.0;
        create(req, dpi);
    }
}

unsafe fn state(hwnd: HWND) -> &'static Dlg {
    &*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Dlg)
}

unsafe fn item(dlg: HWND, id: i32) -> Option<HWND> {
    GetDlgItem(Some(dlg), id).ok()
}

#[repr(C, align(4))]
struct Template {
    head: DLGTEMPLATE,
    tail: [u16; 3],
}

const DLG_STYLE: u32 = WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0;

unsafe fn create(req: Request<'_>, dpi: f32) {
    let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
        dwICC: ICC_BAR_CLASSES,
        ..Default::default()
    });
    let tmpl = Template {
        head: DLGTEMPLATE {
            style: DLG_STYLE,
            dwExtendedStyle: 0,
            cdit: 0,
            x: 0,
            y: 0,
            cx: 64,
            cy: 64,
        },
        tail: [0, 0, 0],
    };
    let theme = req.theme.clone();
    // 主题在下面被移进 `Dlg`，深浅得先问出来 —— 子控件的视觉样式、表头、
    // 滚动条都按它选。
    let dark = chrome::theme_is_dark(&theme);
    let boxed = Box::new(Dlg {
        frame: req.frame,
        font: ui_font(dpi),
        bg: CreateSolidBrush(to_colorref(theme.background)),
        face: CreateSolidBrush(to_colorref(theme.panel_bg)),
        face_hot: CreateSolidBrush(to_colorref(luma_blend(
            theme.foreground,
            theme.panel_bg,
            0.08,
        ))),
        edge: CreateSolidBrush(to_colorref(theme.indent_line)),
        theme,
        dpi,
        names: RefCell::new(super::themeext::names()),
        cur: Cell::new(0),
        rows: RefCell::new(Vec::new()),
        page: Cell::new(Page::Colors),
        metrics: RefCell::new(Vec::new()),
        fonts: RefCell::new(Vec::new()),
        shapes: RefCell::new(Vec::new()),
        icons: Cell::new((0, 0)),
    });
    let param = LPARAM(Box::into_raw(boxed) as isize);
    let dlg = match CreateDialogIndirectParamW(
        Some(instance()),
        &tmpl.head,
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

    let title: Vec<u16> = "主题".encode_utf16().chain([0]).collect();
    let _ = SetWindowTextW(dlg, PCWSTR::from_raw(title.as_ptr()));
    // 标题栏图标：空模板建出来的 `#32770` 一个图标也没有，系统就把它那个
    // 通用对话框图标摆上去 —— 和主窗口并排时一眼就看得出不是一家人。资源组
    // 和主窗口是同一个，`frame_icon` 按标题栏 / 任务栏两个尺寸各取一张。
    let big = frame_icon(instance(), SM_CXICON, SM_CYICON);
    let small = frame_icon(instance(), SM_CXSMICON, SM_CYSMICON);
    for (which, icon) in [(ICON_BIG, big), (ICON_SMALL, small)] {
        if !icon.is_invalid() {
            let _ = SendMessageW(
                dlg,
                WM_SETICON,
                Some(WPARAM(which as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
        }
    }
    state(dlg).icons.set((big.0 as isize, small.0 as isize));

    build_children(dlg);
    // 子控件建完才能套：`dark_theme_children` 遍历的是**现有**子窗口，早一步
    // 调用等于什么都没做。少了这一句，列表的滚动条和表头会按系统浅色画 ——
    // 对话框自己是深色，那两处是白的。
    chrome::dark_theme_children(dlg, dark);
    size_and_place(dlg);
    init_grid(dlg);
    fill_list(dlg);
    select_theme(dlg, req.name);
    let _ = ShowWindow(dlg, SW_SHOW);
    // Escape needs a cancel command to post; this dialog has no id-2 control.
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("Button"),
        PCWSTR::null(),
        WINDOW_STYLE(WS_CHILD.0),
        0,
        0,
        0,
        0,
        Some(dlg),
        Some(HMENU(ID_CANCEL_CMD as usize as *mut core::ffi::c_void)),
        Some(instance()),
        None,
    );
}

/// 一个控件的位置与种类。
struct Ctl {
    id: i32,
    class: &'static str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    /// 1 = owner-draw 按钮，2 = 列表，3 = 表格，4 = 静态文字。
    kind: u32,
    label: &'static str,
}

/// 页签按钮：顶上那一排，62×22，自绘，两个只差位置和文字。
///
/// 单独一个构造函数是为了让 `CTLS` 那张表读起来还是「一行一个控件」—— 整行
/// 写全的话两行都过 100 列。
const fn tab(id: i32, x: i32, label: &'static str) -> Ctl {
    Ctl { id, class: BUTTON_CLASS, x, y: 10, w: 62, h: 22, kind: 1, label }
}

const CTLS: &[Ctl] = &[
    Ctl { id: ID_THEMES, class: LIST_CLASS, x: 10, y: 10, w: 130, h: 326, kind: 2, label: "" },
    Ctl { id: ID_ADD, class: BUTTON_CLASS, x: 10, y: 340, w: 26, h: 22, kind: 1, label: "+" },
    Ctl { id: ID_DEL, class: BUTTON_CLASS, x: 40, y: 340, w: 26, h: 22, kind: 1, label: "×" },
    // 页签按钮：自绘的那个「按下」= 当前页（见 `set_page`）。
    tab(ID_TAB_COLORS, 148, "颜色"),
    tab(ID_TAB_METRICS, 214, "几何"),
    tab(ID_TAB_FONTS, 280, "字体"),
    tab(ID_TAB_SHAPES, 346, "形状"),
    Ctl { id: ID_GRID, class: GRID_CLASS, x: 148, y: 38, w: 622, h: 298, kind: 3, label: "" },
    Ctl { id: ID_METRICS, class: GRID_CLASS, x: 148, y: 38, w: 622, h: 298, kind: 5, label: "" },
    Ctl { id: ID_FONTS, class: GRID_CLASS, x: 148, y: 38, w: 622, h: 298, kind: 5, label: "" },
    Ctl { id: ID_SHAPES, class: GRID_CLASS, x: 148, y: 38, w: 622, h: 298, kind: 5, label: "" },
    Ctl { id: ID_STATUS, class: STATIC_CLASS, x: 10, y: 372, w: 560, h: 18, kind: 4, label: "" },
    Ctl { id: ID_APPLY, class: BUTTON_CLASS, x: 580, y: 370, w: 62, h: 22, kind: 1, label: "应用(&A)" },
    Ctl { id: ID_SAVE, class: BUTTON_CLASS, x: 648, y: 370, w: 62, h: 22, kind: 1, label: "保存(&S)" },
    Ctl { id: ID_CLOSE, class: BUTTON_CLASS, x: 716, y: 370, w: 54, h: 22, kind: 1, label: "关闭(&C)" },
];

unsafe fn build_children(dlg: HWND) {
    let dpi = state(dlg).dpi;
    for ctl in CTLS {
        let style = match ctl.kind {
            1 => WINDOW_STYLE((WS_CHILD | WS_VISIBLE).0 | BS_OWNERDRAW as u32),
            2 => WINDOW_STYLE(
                (WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL).0
                    | LBS_OWNERDRAWFIXED as u32
                    | LBS_NOTIFY as u32
                    | LBS_NOINTEGRALHEIGHT as u32,
            ),
            3 => WINDOW_STYLE(
                (WS_CHILD | WS_VISIBLE | WS_TABSTOP).0
                    | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS),
            ),
            // 5 = 和 3 一样，但**建好先藏着**：几何页要等切过去才露面。
            5 => WINDOW_STYLE(
                (WS_CHILD | WS_TABSTOP).0 | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS),
            ),
            _ => WINDOW_STYLE((WS_CHILD | WS_VISIBLE).0),
        };
        let class: Vec<u16> = ctl.class.encode_utf16().chain([0]).collect();
        let label: Vec<u16> = ctl.label.encode_utf16().chain([0]).collect();
        let Ok(h) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR::from_raw(class.as_ptr()),
            PCWSTR::from_raw(label.as_ptr()),
            style,
            scale(ctl.x, dpi),
            scale(ctl.y, dpi),
            scale(ctl.w, dpi),
            scale(ctl.h, dpi),
            Some(dlg),
            Some(HMENU(ctl.id as usize as *mut core::ffi::c_void)),
            Some(instance()),
            None,
        ) else {
            continue;
        };
        let _ = SendMessageW(
            h,
            WM_SETFONT,
            Some(WPARAM(state(dlg).font.0 as usize)),
            Some(LPARAM(1)),
        );
        if ctl.kind == 1 {
            ownerdraw::install(dlg, &[ctl.id]);
        }
        // 表头是**列表**的子窗口，它的 `NM_CUSTOMDRAW` 只到列表这里，到不了
        // 对话框 —— 不接这一道，深色对话框顶上就是一条白带。
        if ctl.kind == 3 || ctl.kind == 5 {
            let _ = SetWindowSubclass(h, Some(grid_proc), GRID_SUBCLASS, dlg.0 as usize);
        }
    }
}

unsafe fn size_and_place(dlg: HWND) {
    let st = state(dlg);
    let dpi = st.dpi;
    dlggeom::size_and_place(
        dlg,
        st.frame,
        scale(CLIENT_W, dpi),
        scale(CLIENT_H, dpi),
        (dpi * 96.0) as u32,
        DLG_STYLE,
    );
}

/// 表格的骨架：扩展风格、主题色、列。两张表共用，只有列不一样。
unsafe fn init_table(dlg: HWND, id: i32, columns: &[(&str, i32)]) {
    let Some(grid) = item(dlg, id) else {
        return;
    };
    let st = state(dlg);
    // 双缓冲：滚动、改选中这些单帧重绘也走内存位图，色块多的颜色表不再拉丝。
    let ex = (LVS_EX_FULLROWSELECT | LVS_EX_LABELTIP | LVS_EX_DOUBLEBUFFER) as usize;
    let _ = SendMessageW(
        grid,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        Some(WPARAM(ex)),
        Some(LPARAM(ex as isize)),
    );
    let bg = to_colorref(st.theme.background).0 as isize;
    let fg = to_colorref(st.theme.foreground).0 as isize;
    let _ = SendMessageW(grid, LVM_SETBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg)));
    let _ = SendMessageW(grid, LVM_SETTEXTBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg)));
    let _ = SendMessageW(grid, LVM_SETTEXTCOLOR, Some(WPARAM(0)), Some(LPARAM(fg)));
    for (i, (title, width)) in columns.iter().enumerate() {
        let mut buf: Vec<u16> = title.encode_utf16().chain([0]).collect();
        let mut col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_FMT,
            fmt: LVCFMT_LEFT,
            cx: scale(*width, st.dpi),
            pszText: windows::core::PWSTR(buf.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            grid,
            LVM_INSERTCOLUMNW,
            Some(WPARAM(i)),
            Some(LPARAM(&raw mut col as isize)),
        );
    }
    // 表头自带一套系统字体（9 pt），和表格里那几行不是同一个字号 —— 列标题
    // 会明显小一号。给它同一份 `HFONT`，表头高度也跟着重算。
    let header = SendMessageW(grid, LVM_GETHEADER, None, None);
    if header.0 != 0 {
        let _ = SendMessageW(
            HWND(header.0 as *mut core::ffi::c_void),
            WM_SETFONT,
            Some(WPARAM(st.font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
}

/// 表格的列只在建表时给一次，之后由用户拖。
unsafe fn init_grid(dlg: HWND) {
    init_table(dlg, ID_GRID, &COLUMNS);
    init_table(dlg, ID_METRICS, &METRIC_COLUMNS);
    init_table(dlg, ID_FONTS, &FONT_COLUMNS);
    init_table(dlg, ID_SHAPES, &SHAPE_COLUMNS);
    let st = state(dlg);
    if let Some(list) = item(dlg, ID_THEMES) {
        let _ = SendMessageW(
            list,
            LB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(scale(ROW_H, st.dpi) as isize)),
        );
    }
}

unsafe fn fill_list(dlg: HWND) {
    let Some(list) = item(dlg, ID_THEMES) else {
        return;
    };
    let st = state(dlg);
    // 同一张表的理由：清空加一条重画一帧，几十个主题就是几十帧。
    with_redraw(list, || {
        SendMessageW(list, LB_RESETCONTENT, None, None);
        for name in st.names.borrow().iter() {
            let text: Vec<u16> = name.encode_utf16().chain([0]).collect();
            SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
        }
    });
}

/// 选一个主题：左列表跟过去，右表格按它重读。
unsafe fn select_theme(dlg: HWND, name: &str) {
    let st = state(dlg);
    let idx = st
        .names
        .borrow()
        .iter()
        .position(|n| n.eq_ignore_ascii_case(name))
        .unwrap_or(0);
    st.cur.set(idx);
    if let Some(list) = item(dlg, ID_THEMES) {
        let _ = SendMessageW(list, LB_SETCURSEL, Some(WPARAM(idx)), None);
        let _ = InvalidateRect(Some(list), None, true);
    }
    reload_rows(dlg);
    // 三张表读的是同一份文件的三个段，一起刷新：切主题时别的页要是旧的，
    // 用户切过去会看见上一个主题的颜色 / 几何 / 字体。
    reload_metrics(dlg);
    reload_fonts(dlg);
    reload_shapes(dlg);
}

/// 从文件重读这个主题的 `[Theme]`。
///
/// 读的是**合成结果**（随包那份叠上外部那份）：表格要显示的是「现在生效的值」，
/// 只列外部文件写过的那几行会让人以为主题只有十几个键。
unsafe fn reload_rows(dlg: HWND) {
    let st = state(dlg);
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    let external = super::themeext::existing(&name).and_then(|p| std::fs::read_to_string(p).ok());
    let rows: Vec<Row> = match super::window::compose_theme(&name, external.as_deref()) {
        Some((_, file)) => file
            .colors
            .iter()
            .map(|(key, c)| {
                let style = file.style(key).unwrap_or(0);
                Row {
                    key: key.clone(),
                    label: region_label(key),
                    bg: c.bg,
                    fg: c.fg,
                    base: (c.bg, c.fg),
                    style,
                    orig: style,
                }
            })
            .collect(),
        None => Vec::new(),
    };
    let count = rows.len();
    *st.rows.borrow_mut() = rows;
    rebuild_grid(dlg);
    set_status(
        dlg,
        &format!("{name} · {count} 个颜色键 · 双击色块改色，点「粗/斜/矩/下/删/双/点/波」改字形"),
    );
}

/// 往表格里塞一行：第一列跟着插入走，其余各列再补。
///
/// 三张表（颜色 / 几何 / 字体）共用这一段。分开写而不是拼一个 `LVITEMW` 是
/// 因为**插入时只认第一列** —— 把别的列塞进 `LVM_INSERTITEMW` 不会报错，只是
/// 不生效，看着像「后面几列一直是空的」。
unsafe fn insert_row(grid: HWND, index: usize, cells: &[String]) {
    for (sub, text) in cells.iter().enumerate() {
        let mut buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
        if sub == 0 {
            let it = LVITEMW {
                mask: LVIF_TEXT,
                iItem: index as i32,
                iSubItem: 0,
                pszText: windows::core::PWSTR(buf.as_mut_ptr()),
                ..Default::default()
            };
            let _ = SendMessageW(
                grid,
                LVM_INSERTITEMW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const it as isize)),
            );
        } else {
            let cell = LVITEMW {
                iSubItem: sub as i32,
                pszText: windows::core::PWSTR(buf.as_mut_ptr()),
                ..Default::default()
            };
            let _ = SendMessageW(
                grid,
                LVM_SETITEMTEXTW,
                Some(WPARAM(index)),
                Some(LPARAM(&raw const cell as isize)),
            );
        }
    }
}

/// 批量改写一个控件的内容时把重绘关掉，改完一次铺出来。
///
/// `LVM_DELETEALLITEMS` 和逐行插入每一步都会让控件自己重画：颜色表几十行，
/// 换一次主题就是「清空 → 半满 → 全量」好几帧，看着在闪。`WM_SETREDRAW` 压住
/// 中间这些帧，收尾开回去并整体失效 —— 全程只剩一次擦除加一次绘制。
unsafe fn with_redraw<T>(h: HWND, f: impl FnOnce() -> T) -> T {
    let _ = SendMessageW(h, WM_SETREDRAW, Some(WPARAM(0)), None);
    let r = f();
    let _ = SendMessageW(h, WM_SETREDRAW, Some(WPARAM(1)), None);
    let _ = InvalidateRect(Some(h), None, true);
    r
}

unsafe fn rebuild_grid(dlg: HWND) {
    let Some(grid) = item(dlg, ID_GRID) else {
        return;
    };
    let st = state(dlg);
    with_redraw(grid, || {
        SendMessageW(grid, LVM_DELETEALLITEMS, None, None);
        let rows = st.rows.borrow();
        for (i, row) in rows.iter().enumerate() {
            // 背景 / 前景那两格没有文字：色块由 `NM_CUSTOMDRAW` 自己铺，这里留空。
            // 值那一列写 `#RRGGBB` / `#RRGGBB,#RRGGBB` —— 色块看着直观，可要手抄一个
            // 色号还是得看字。
            let mut cells = vec![
                row.label.to_string(),
                String::new(),
                String::new(),
                hex_pair(row.bg, row.fg),
            ];
            // 字形列：一个勾，空格子 = 没开。勾由控件自己画（`clrText` /
            // `clrTextBk` 在自绘那一轮已经按主题设好），不用另开一段绘制。
            for bit in STYLE_BITS {
                cells.push(if row.style & bit != 0 { "✓".to_string() } else { String::new() });
            }
            insert_row(grid, i, &cells);
        }
    });
}

// ---------------------------------------------------------------------------
// 几何页（`[Metrics]`）
// ---------------------------------------------------------------------------

/// 度量 → 中文名。键名那一列另有 `MetricId::as_str`，这一列是给人找的。
fn metric_label(id: dg_skin::MetricId) -> &'static str {
    match id {
        dg_skin::MetricId::StatusHeight => "状态栏高度",
        dg_skin::MetricId::StatusPad => "状态格文字左边距",
        dg_skin::MetricId::StatusIconPad => "状态格图标预留宽",
        dg_skin::MetricId::StatusMinWidth => "状态格最小宽度",
        dg_skin::MetricId::SplitBandH => "上下分栏分隔带",
        dg_skin::MetricId::SplitBandV => "左右分栏分隔带",
        dg_skin::MetricId::TabHeight => "文档页签高度",
        dg_skin::MetricId::TabPadX => "页签左右内边距",
        dg_skin::MetricId::TabClose => "页签关闭钮格宽",
        dg_skin::MetricId::DockTabHeight => "停靠页签高度",
        dg_skin::MetricId::MenuRowHeight => "菜单行高",
        dg_skin::MetricId::MenuPad => "菜单左右内边距",
        dg_skin::MetricId::ToolbarIcon => "工具栏图标格边长",
    }
}

/// 夹取规则写成一行话。
///
/// `ZeroOr` 的 0 要明说：那一档的意思是「跟字体/图标格走」，不是「零像素高」。
/// 用户把 `status.height` 填 0 之前得先知道这一点。
fn range_text(id: dg_skin::MetricId) -> String {
    match id.clamp() {
        Clamp::Range(lo, hi) => format!("{}..{}", fmt_num(lo), fmt_num(hi)),
        Clamp::ZeroOr(lo, hi) => format!("0 或 {}..{}", fmt_num(lo), fmt_num(hi)),
        Clamp::OneOf(allowed) => allowed
            .iter()
            .map(|v| fmt_num(*v))
            .collect::<Vec<_>>()
            .join("/"),
    }
}

/// 数字到文本。整数值不带 `.0` —— 这一列是给人抄的。
fn fmt_num(v: f32) -> String {
    format!("{v}")
}

/// 当前页对应的那张表重读一遍。
///
/// 切页（[`set_page`]）和左列表换主题都走这里：换主题换的是数据，四张表读的
/// 是同一份文件的四个段，停在字体页上换主题，字体表就得当场跟着换。
unsafe fn reload_current_page(dlg: HWND) {
    match state(dlg).page.get() {
        Page::Metrics => reload_metrics(dlg),
        Page::Fonts => reload_fonts(dlg),
        Page::Shapes => reload_shapes(dlg),
        Page::Colors => reload_rows(dlg),
    }
}

/// 切页：四张表一显三藏，页签按钮重画（当前那页画成按下的样子）。
unsafe fn set_page(dlg: HWND, page: Page) {
    let st = state(dlg);
    if st.page.get() == page {
        return;
    }
    st.page.set(page);
    for (id, own) in [
        (ID_GRID, Page::Colors),
        (ID_METRICS, Page::Metrics),
        (ID_FONTS, Page::Fonts),
        (ID_SHAPES, Page::Shapes),
    ] {
        if let Some(h) = item(dlg, id) {
            let _ = ShowWindow(h, if own == page { SW_SHOW } else { SW_HIDE });
        }
    }
    for id in [ID_TAB_COLORS, ID_TAB_METRICS, ID_TAB_FONTS, ID_TAB_SHAPES] {
        if let Some(h) = item(dlg, id) {
            let _ = InvalidateRect(Some(h), None, true);
        }
    }
    reload_current_page(dlg);
}

/// 从文件重读这个主题的 `[Metrics]`。
///
/// 显示的是**生效值**（夹过的），不是文件里那个数：这一页的用处是边改边看，
/// 屏幕上多高就该写多高。文件里写了 500、真正生效 48 时，行里是 48 —— 范围那
/// 一列已经写着 16..48，用户看得出来被夹过。
///
/// 十三项永远都在（`MetricId::ALL` 是固定的），所以和颜色那张表不同：这里没有
/// 「文件里没有这一行」的情况，`orig` 就是「文件里那个值夹完之后的样子」。
unsafe fn reload_metrics(dlg: HWND) {
    let st = state(dlg);
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    let external = super::themeext::existing(&name).and_then(|p| std::fs::read_to_string(p).ok());
    let file = super::window::compose_theme(&name, external.as_deref()).map(|(_, f)| f);
    let rows: Vec<MetricRow> = dg_skin::MetricId::ALL
        .iter()
        .map(|&id| {
            let value = file
                .as_ref()
                .and_then(|f| f.metrics.get(id.as_str()).copied())
                .map(|v| id.clamp().apply(v))
                .unwrap_or_else(|| id.default_value());
            MetricRow { id, value, orig: value }
        })
        .collect();
    let count = rows.len();
    let written = file.as_ref().map(|f| f.metrics.len()).unwrap_or(0);
    *st.metrics.borrow_mut() = rows;
    rebuild_metrics(dlg);
    // 颜色页也在读同一份文件；切页之后状态行说的是**这一页**的事。
    if st.page.get() == Page::Metrics {
        set_status(
            dlg,
            &format!("{name} · {count} 项几何 · 文件里写了 {written} 项 · 双击「值」改数字"),
        );
    }
}

unsafe fn rebuild_metrics(dlg: HWND) {
    let Some(grid) = item(dlg, ID_METRICS) else {
        return;
    };
    let st = state(dlg);
    with_redraw(grid, || {
        SendMessageW(grid, LVM_DELETEALLITEMS, None, None);
        let rows = st.metrics.borrow();
        for (i, row) in rows.iter().enumerate() {
            let cells = vec![
                metric_label(row.id).to_string(),
                fmt_num(row.value),
                range_text(row.id),
                row.id.as_str().to_string(),
            ];
            insert_row(grid, i, &cells);
        }
    });
}

/// 写一项几何：夹取 → 落盘 → 让主窗口当场重装。
///
/// 和改颜色一样是**立刻**落盘的（不是等「保存」）：这一页的用处就是边改边看，
/// 写一半留在内存里、屏幕上还是旧尺寸，等于没反应。
unsafe fn apply_metric(dlg: HWND, index: usize, raw: f32) {
    let st = state(dlg);
    let Some(entry) = st.metrics.borrow().get(index).copied() else {
        return;
    };
    let value = entry.id.clamp().apply(raw);
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    match super::themeext::write_key(&name, "Metrics", entry.id.as_str(), &fmt_num(value)) {
        Ok(path) => {
            if let Some(slot) = st.metrics.borrow_mut().get_mut(index) {
                slot.value = value;
            }
            rebuild_metrics(dlg);
            let note = if value != raw {
                format!("（{} 夹到 {}）", fmt_num(raw), fmt_num(value))
            } else {
                String::new()
            };
            set_status(
                dlg,
                &format!(
                    "已写入 [Metrics] {}={}{note} · {}",
                    entry.id.as_str(),
                    fmt_num(value),
                    path.display()
                ),
            );
            let _ = PostMessageW(Some(st.frame), WM_THEME_DLG, WPARAM(APPLY), LPARAM(0));
        }
        Err(e) => set_status(dlg, &format!("写不进去：{e}")),
    }
}

/// 双击几何页的一行：问一个数字，夹完写回去。
///
/// 「值」和「项目」两列都认：一行只有一个数，点名字改数和点数字改数是一个意思，
/// 而「值」那一列只有几十像素宽，逼着人往窄格里点不友好。
unsafe fn on_metrics_dbl_click(dlg: HWND, hit: &NMITEMACTIVATE) -> isize {
    let row = hit.iItem;
    if row < 0 {
        return 0;
    }
    let sub = hit.iSubItem as usize;
    if sub > 1 {
        return 0;
    }
    let st = state(dlg);
    let Some(entry) = st.metrics.borrow().get(row as usize).copied() else {
        return 0;
    };
    let question = format!(
        "{}（{}）\n范围 {}",
        metric_label(entry.id),
        entry.id.as_str(),
        range_text(entry.id)
    );
    let Some(text) = prompt::prompt(dlg, "几何", &question, &fmt_num(entry.value)) else {
        return 0;
    };
    match text.trim().parse::<f32>() {
        Ok(v) => apply_metric(dlg, row as usize, v),
        Err(_) => set_status(dlg, &format!("「{}」不是数字", text.trim())),
    }
    0
}

// ---------------------------------------------------------------------------
// 字体页（`[Fonts]`）
// ---------------------------------------------------------------------------

/// 字体 token → 中文用途。写文件用的是 `mono` 这种拼法（「字体」那一列）。
fn font_label(token: dg_skin::FontToken) -> &'static str {
    match token {
        dg_skin::FontToken::Ui => "界面正文",
        dg_skin::FontToken::Mono => "编辑器字面",
        dg_skin::FontToken::Tab => "文档页签",
        dg_skin::FontToken::Status => "状态栏",
        dg_skin::FontToken::Menu => "菜单",
        dg_skin::FontToken::Pane => "停靠窗格",
    }
}

/// 四个字段的拼法，顺序就是表格里那四列。`[Fonts]` 的键是 `token.字段`。
const FONT_FIELDS: [&str; 4] = ["face", "size", "weight", "italic"];

/// 「斜体」那一列的下标。前两列是名字（字体 / 用途），中间三列是打得字的字段，
/// 斜体只走单击 —— 一列一种改法。
const COL_ITALIC: usize = 2 + 3;

/// 表格列 → `[Fonts]` 的字段号。名字那两列和斜体列都不是「双击改值」。
fn font_field_at(sub: usize) -> Option<usize> {
    if (2..COL_ITALIC).contains(&sub) {
        Some(sub - 2)
    } else {
        None
    }
}

/// 一格里该显示什么。
///
/// `had == false`（文件里没这个 token）时四格全空：真正生效的是内置字体，那是
/// 系统字面 + `setting.ini` 算出来的，`FontSpec::default()` 里那个 9 pt 只是
/// 「字段缺省填什么」。摆一个 9 出来等于告诉用户「你的编辑器是 9 pt」。
fn font_field_text(row: &FontRow, field: usize) -> String {
    if !row.had {
        return String::new();
    }
    match field {
        0 => row.spec.face.clone(),
        1 => fmt_num(row.spec.size),
        2 => row.spec.weight.to_string(),
        _ => {
            if row.spec.italic {
                "✓".to_string()
            } else {
                String::new()
            }
        }
    }
}

/// 从文件重读这个主题的 `[Fonts]`。
///
/// 用的是应用自己那个解析器（`Fonts::from_theme_file`）：写了一半的 token
/// （只有 `mono.size` 没有 `mono.face`）该怎么补默认值，两边必须是同一套算法，
/// 否则对话框显示的和真正生效的会不一样。
unsafe fn reload_fonts(dlg: HWND) {
    let st = state(dlg);
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    let external = super::themeext::existing(&name).and_then(|p| std::fs::read_to_string(p).ok());
    let file = super::window::compose_theme(&name, external.as_deref()).map(|(_, f)| f);
    let fonts = file.as_ref().map(|f| dg_skin::Fonts::from_theme_file(f).0);
    let rows: Vec<FontRow> = dg_skin::FontToken::ALL
        .iter()
        .map(|&token| {
            let spec = fonts.as_ref().and_then(|f| f.get(token)).cloned();
            FontRow {
                token,
                spec: spec.clone().unwrap_or_default(),
                orig: spec.clone().unwrap_or_default(),
                had: spec.is_some(),
            }
        })
        .collect();
    let count = rows.len();
    let written = rows.iter().filter(|r| r.had).count();
    *st.fonts.borrow_mut() = rows;
    rebuild_fonts(dlg);
    if st.page.get() == Page::Fonts {
        set_status(
            dlg,
            &format!("{name} · {count} 个字体 · 文件里写了 {written} 个 · 双击格子改，点「斜体」翻"),
        );
    }
}

unsafe fn rebuild_fonts(dlg: HWND) {
    let Some(grid) = item(dlg, ID_FONTS) else {
        return;
    };
    let st = state(dlg);
    with_redraw(grid, || {
        SendMessageW(grid, LVM_DELETEALLITEMS, None, None);
        let rows = st.fonts.borrow();
        for (i, row) in rows.iter().enumerate() {
            let mut cells = vec![row.token.name().to_string(), font_label(row.token).to_string()];
            for field in 0..FONT_FIELDS.len() {
                cells.push(font_field_text(row, field));
            }
            insert_row(grid, i, &cells);
        }
    });
}

/// 改一格字体：解析 → 夹取 → 写 `[Fonts]` 的那一行 → 让主窗口重装。
///
/// 只写**这一格**对应的那个键。写 `mono.size=12` 一个键就够了 ——
/// `Fonts::from_theme_file` 会把同一个 token 缺的字段按 `FontSpec::default()`
/// 补上，所以「写一半」不会留下一个坏 token，只是其余三项回到默认。
unsafe fn apply_font(dlg: HWND, index: usize, field: usize, raw: &str) {
    let st = state(dlg);
    let Some(mut row) = st.fonts.borrow().get(index).cloned() else {
        return;
    };
    let raw = raw.trim();
    match field {
        0 => row.spec.face = raw.to_string(),
        1 => match raw.parse::<f32>() {
            Ok(v) => row.spec.size = v,
            Err(_) => {
                set_status(dlg, &format!("「{raw}」不是数字（字号是 pt，6–48）"));
                return;
            }
        },
        2 => match raw.parse::<u16>() {
            Ok(v) => row.spec.weight = v,
            Err(_) => {
                set_status(dlg, &format!("「{raw}」不是数字（字重 100–900）"));
                return;
            }
        },
        _ => row.spec.italic = raw.eq_ignore_ascii_case("true") || raw == "1",
    }
    // 夹取规则在 `dg_skin`（6–48 pt、100–900、字面去空白），这里不另写一份。
    row.spec.clamp();
    row.had = true;
    let text = match field {
        0 => row.spec.face.clone(),
        1 => fmt_num(row.spec.size),
        2 => row.spec.weight.to_string(),
        _ => if row.spec.italic { "true" } else { "false" }.to_string(),
    };
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    let key = format!("{}.{}", row.token.name(), FONT_FIELDS[field]);
    match super::themeext::write_key(&name, "Fonts", &key, &text) {
        Ok(path) => {
            if let Some(slot) = st.fonts.borrow_mut().get_mut(index) {
                slot.spec = row.spec.clone();
                slot.had = true;
            }
            rebuild_fonts(dlg);
            let note = if text == raw {
                String::new()
            } else {
                format!("（{} 记成 {}）", raw, text)
            };
            set_status(
                dlg,
                &format!("已写入 [Fonts] {key}={text}{note} · {}", path.display()),
            );
            let _ = PostMessageW(Some(st.frame), WM_THEME_DLG, WPARAM(APPLY), LPARAM(0));
        }
        Err(e) => set_status(dlg, &format!("写不进去：{e}")),
    }
}

/// 点「斜体」那一格：翻一位。其余三列要打字，走双击。
unsafe fn on_font_click(dlg: HWND, hit: &NMITEMACTIVATE) -> isize {
    if hit.iItem < 0 || hit.iSubItem as usize != COL_ITALIC {
        return 0;
    }
    let st = state(dlg);
    let Some(row) = st.fonts.borrow().get(hit.iItem as usize).cloned() else {
        return 0;
    };
    let want = if row.spec.italic { "false" } else { "true" };
    apply_font(dlg, hit.iItem as usize, 3, want);
    0
}

/// 双击字体页的一格：问一个值，写回去。
///
/// 「斜体」那一列不在这儿 —— 它单击就翻（和颜色页的字形列一个手感）。
unsafe fn on_font_dbl_click(dlg: HWND, hit: &NMITEMACTIVATE) -> isize {
    if hit.iItem < 0 {
        return 0;
    }
    let Some(field) = font_field_at(hit.iSubItem as usize) else {
        return 0;
    };
    let st = state(dlg);
    let Some(row) = st.fonts.borrow().get(hit.iItem as usize).cloned() else {
        return 0;
    };
    let (title, hint) = match field {
        0 => ("字面", "字体名，比如 Consolas / Segoe UI；留空 = 系统默认".to_string()),
        1 => ("字号", "字号，pt（6–48）".to_string()),
        2 => ("字重", "字重 100–900（400 常规 / 700 粗）".to_string()),
        _ => ("斜体", "true 或 false".to_string()),
    };
    let question = format!("{}（{}）\n{hint}", font_label(row.token), row.token.name());
    let initial = if field == 0 {
        row.spec.face.clone()
    } else if field == 1 {
        fmt_num(row.spec.size)
    } else if field == 2 {
        row.spec.weight.to_string()
    } else {
        row.spec.italic.to_string()
    };
    let Some(text) = prompt::prompt(dlg, title, &question, &initial) else {
        return 0;
    };
    apply_font(dlg, hit.iItem as usize, field, &text);
    0
}

// ---------------------------------------------------------------------------
// 形状页（`[Shape]`）
// ---------------------------------------------------------------------------

/// 形状键 → 中文。两个键在状态栏上意思不一样，所以带区域判一下。
///
/// `[Shape]` 的 `gap` 在页签上是「相邻两格留的空隙」，在状态栏上却是「文字
/// 左右各留这么多」—— 同一个词在两处指的是不同的边，笼统写「空隙」会让人把
/// 状态格调宽之后发现文字没动。
fn shape_label(region: &str, key: dg_skin::ShapeKey) -> &'static str {
    let status = region.starts_with("status");
    match key {
        dg_skin::ShapeKey::Radius => "圆角",
        dg_skin::ShapeKey::Gap if status => "文字留白",
        dg_skin::ShapeKey::Gap => "空隙",
        dg_skin::ShapeKey::Border if status => "格间线",
        dg_skin::ShapeKey::Border => "描边",
        dg_skin::ShapeKey::PadY => "上下留白",
        dg_skin::ShapeKey::Font => "字体",
    }
}

/// 这一行在文件里的拼法：`tab.radius` / `tab.states.active.radius`。
///
/// `states` 是**中缀**不是一层 —— 区域名自己可以带点（`dock.tab`），所以状态
/// 只能插在最后那个键名前面。写文件、双击提示框、状态行都用这一个拼法。
fn shape_key_text(row: &ShapeRow) -> String {
    match row.state {
        Some(state) => format!("{}.states.{}.{}", row.region, state, row.key.name()),
        None => format!("{}.{}", row.region, row.key.name()),
    }
}

/// 从文件重读这个主题的 `[Shape]`。
///
/// 行 = **接了绘制**的区域（`WIRED_REGIONS`）× 它认的键，常态一行；文件里写了
/// 状态层的，那一层再各占一行。二十六个没接线的区域不铺进来：写了也看不见，
/// 摆进表里等于把能用的那十行淹掉。它们的数量写在状态行里。
///
/// 解析走 `Recipe::from_theme_file`，不是自己拆键 —— 区域族展开（`status` →
/// `status.pane`）、`states` 中缀、越界夹取这三件事只有那一份实现，自己再拆一遍
/// 迟早和它对不上。
unsafe fn reload_shapes(dlg: HWND) {
    let st = state(dlg);
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    let external = super::themeext::existing(&name).and_then(|p| std::fs::read_to_string(p).ok());
    let file = super::window::compose_theme(&name, external.as_deref()).map(|(_, f)| f);
    let rows = shape_rows(file.as_ref());
    let count = rows.len();
    let wired = dg_skin::WIRED_REGIONS.len();
    let unwired = dg_skin::Region::ALL.len() - wired;
    *st.shapes.borrow_mut() = rows;
    rebuild_shapes(dlg);
    if st.page.get() == Page::Shapes {
        set_status(
            dlg,
            &format!(
                "{name} · {count} 行 · {wired} 个区域接线（另 {unwired} 个写了不生效）· 双击「键」或「值」"
            ),
        );
    }
}

/// 表格的行：已接线区域 × 它认的键。
///
/// 单独一个纯函数是因为「哪些行该有、值是多少」是这一页唯一容易错的地方 ——
/// 状态层的顺序、区域族的展开、没写过的键算 0，全在这一段，能单测就不用靠截图。
fn shape_rows(file: Option<&dg_render::theme::ThemeFile>) -> Vec<ShapeRow> {
    let recipe = file
        .map(|f| dg_skin::Recipe::from_theme_file(f).0)
        .unwrap_or_default();
    let mut rows = Vec::new();
    for wired in dg_skin::WIRED_REGIONS {
        let reg = recipe.region(wired.name);
        for &key in wired.keys {
            let normal = reg
                .map(|r| r.resolve(dg_skin::State::normal()).number(key))
                .unwrap_or_default();
            rows.push(ShapeRow {
                region: wired.name,
                key,
                state: None,
                value: normal,
                orig: normal,
            });
            // 状态层：只列文件里真写了这个键的那几层，顺序按 `State::NAMES`
            // （配方自己也是按这个顺序叠的，表里跟着来才对得上屏幕）。
            for state in dg_skin::State::NAMES {
                let layer = reg.and_then(|r| {
                    r.states()
                        .iter()
                        .find(|(n, _)| n == state)
                        .map(|(_, patch)| patch)
                });
                let Some(v) = layer.and_then(|patch| patch.number(key)) else {
                    continue;
                };
                rows.push(ShapeRow {
                    region: wired.name,
                    key,
                    state: Some(state),
                    value: v,
                    orig: v,
                });
            }
        }
    }
    rows
}

unsafe fn rebuild_shapes(dlg: HWND) {
    let Some(grid) = item(dlg, ID_SHAPES) else {
        return;
    };
    let st = state(dlg);
    with_redraw(grid, || {
        SendMessageW(grid, LVM_DELETEALLITEMS, None, None);
        let rows = st.shapes.borrow();
        for (i, row) in rows.iter().enumerate() {
            let cells = vec![
                row.region.to_string(),
                shape_label(row.region, row.key).to_string(),
                row.state.unwrap_or("常态").to_string(),
                fmt_num(row.value),
                shape_key_text(row),
            ];
            insert_row(grid, i, &cells);
        }
    });
}

/// 写一层形状：夹取 → 落盘 → 让主窗口当场重装。
///
/// 和改颜色、改几何一样是**立刻**落盘的。写的是**这一行**那一层：常态行写
/// `tab.radius`，状态行写 `tab.states.active.radius` —— 只改常态不能顺手把状态
/// 层抹掉，`patch_theme_ini` 是按键改的，正好。
unsafe fn apply_shape(dlg: HWND, index: usize, raw: f32) {
    let st = state(dlg);
    let Some(entry) = st.shapes.borrow().get(index).copied() else {
        return;
    };
    let Some((lo, hi)) = entry.key.range() else {
        return;
    };
    let value = if raw.is_nan() { lo } else { raw.clamp(lo, hi) };
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    let key = shape_key_text(&entry);
    match super::themeext::write_key(&name, "Shape", &key, &fmt_num(value)) {
        Ok(path) => {
            if let Some(slot) = st.shapes.borrow_mut().get_mut(index) {
                slot.value = value;
                slot.orig = value;
            }
            rebuild_shapes(dlg);
            let note = if value != raw {
                format!("（{} 夹到 {}）", fmt_num(raw), fmt_num(value))
            } else {
                String::new()
            };
            set_status(
                dlg,
                &format!(
                    "已写入 [Shape] {key}={}{note} · {}",
                    fmt_num(value),
                    path.display()
                ),
            );
            let _ = PostMessageW(Some(st.frame), WM_THEME_DLG, WPARAM(APPLY), LPARAM(0));
        }
        Err(e) => set_status(dlg, &format!("写不进去：{e}")),
    }
}

/// 双击形状页的一行：问一个数字，夹完写回去。
///
/// 「键」和「值」两列都认（和几何页一样：一行只有一个数）。状态列和键名列不认 ——
/// 那两格是「这一行是什么」，点它改数字没有道理。
unsafe fn on_shapes_dbl_click(dlg: HWND, hit: &NMITEMACTIVATE) -> isize {
    let row = hit.iItem;
    if row < 0 {
        return 0;
    }
    let sub = hit.iSubItem as usize;
    if sub != 1 && sub != COL_SHAPE_VALUE {
        return 0;
    }
    let st = state(dlg);
    let Some(entry) = st.shapes.borrow().get(row as usize).copied() else {
        return 0;
    };
    let Some((lo, hi)) = entry.key.range() else {
        return 0;
    };
    let question = format!(
        "{}（{}）\n范围 {}..{}，0 = 不画",
        shape_label(entry.region, entry.key),
        shape_key_text(&entry),
        fmt_num(lo),
        fmt_num(hi)
    );
    let Some(text) = prompt::prompt(dlg, "形状", &question, &fmt_num(entry.value)) else {
        return 0;
    };
    match text.trim().parse::<f32>() {
        Ok(v) => apply_shape(dlg, row as usize, v),
        Err(_) => set_status(dlg, &format!("「{}」不是数字", text.trim())),
    }
    0
}

fn hex_of(c: Color) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

fn hex_pair(bg: Option<Color>, fg: Option<Color>) -> String {
    match (bg, fg) {
        (Some(b), Some(f)) => format!("{},{}", hex_of(b), hex_of(f)),
        (Some(b), None) => hex_of(b),
        (None, Some(f)) => hex_of(f),
        (None, None) => String::new(),
    }
}

unsafe fn set_status(dlg: HWND, text: &str) {
    if let Some(ctl) = item(dlg, ID_STATUS) {
        let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let _ = SetWindowTextW(ctl, PCWSTR::from_raw(buf.as_ptr()));
    }
}

/// `[Theme]` 的键 → 中文区域名。
///
/// 文件里是英文拼写（写回去要用它），这一列是给人读的。表里没有的键回落到键名
/// 本身：主题文件是手写的，多一个键不该变成一行空白。
fn region_label(key: &str) -> &'static str {
    match key {
        "Default" => "默认文本",
        "Selection" => "选区",
        "WhiteSpace" => "空白/换行符/文档结束符",
        "ConChar" => "控制字符",
        "LineNumber" => "行号",
        "Folding" => "折叠区域",
        "IconArea" => "图标区域(调试信息等...)",
        "BookMark" => "书签",
        "Pair" => "配对符号",
        "Ruler" => "标尺",
        "Found" => "查找结果",
        "Comment1" => "注释1(单行注释)",
        "Comment2" => "注释2(多行注释)",
        "String1" => "字符串1",
        "String2" => "字符串2",
        "Tag" => "标记",
        "Macro" => "宏",
        "Url" => "URL",
        "Email" => "邮件",
        "Number" => "数字",
        "Word1" => "关键字1",
        "Word2" => "关键字2",
        "Word3" => "关键字3",
        "Word4" => "关键字4",
        "Function" => "函数",
        "Var" => "变量",
        "SubLan" => "子语言",
        "Operator" => "运算符",
        "Highlight1" => "高亮1",
        "Highlight2" => "高亮2",
        "Highlight3" => "高亮3",
        "Highlight4" => "高亮4",
        "Highlight5" => "高亮5",
        "Highlight6" => "高亮6",
        "Highlight7" => "高亮7",
        "Highlight8" => "高亮8",
        "WordComplete" => "自动完成",
        "CallTip" => "函数提示窗口",
        "IndentLine" => "缩进线",
        "SelIndicator" => "选区边界指示符",
        "WaveLine" => "波浪线/拼写检查错误等",
        "ActiveLine" => "当前行",
        "BoundaryLine" => "边界线",
        "DefaultBgNull" => "默认背景(空)",
        "Caret" => "光标",
        _ => "",
    }
}

/// 色块在格子里留多少边。
const SWATCH_INSET_X: i32 = 6;
const SWATCH_INSET_Y: i32 = 5;

/// 勾选框的边长上限与上下留白，96 dpi 像素。行高不够时先缩边长 —— 方块
/// 顶到行线上比小一点难看得多。
const CHECK_SIDE: i32 = 12;
const CHECK_INSET: i32 = 3;

/// 自绘一个色块：填色 + 一圈描边。
///
/// 描边不是为了好看 —— `#1E2126` 这种底色和表格背景几乎一样，不描边就是一个看
/// 不出边界的黑洞，等于没画。
unsafe fn paint_swatch(hdc: HDC, rc: &RECT, c: Color, edge: Color) {
    let fill = CreateSolidBrush(to_colorref(c));
    let _ = FillRect(hdc, rc, fill);
    let _ = DeleteObject(HGDIOBJ(fill.0));
    let pen = CreateSolidBrush(to_colorref(edge));
    let _ = FrameRect(hdc, rc, pen);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

/// 一格勾选框：外框常显，开着的填实并画一道勾。
///
/// 常显是关键 —— 没开的格子也得看得出「这里能点」。方框边长按行高收，宽度
/// 用同一个数：字形列只有 26 px，铺满会顶到列边界。
///
/// 开着的那种用**前景色填实、勾用格子的底色画**：主题里前景和底色按定义是
/// 一对高对比，深浅两套主题都不用另挑颜色；挑 `selection_bg` 反而会和选中行
/// 撞成一片。
unsafe fn paint_check(hdc: HDC, cell: &RECT, on: bool, face: Color, st: &Dlg) {
    let back = CreateSolidBrush(to_colorref(face));
    let _ = FillRect(hdc, cell, back);
    let _ = DeleteObject(HGDIOBJ(back.0));
    let side = (cell.bottom - cell.top - 2 * scale(CHECK_INSET, st.dpi)).min(scale(CHECK_SIDE, st.dpi));
    if side <= 2 {
        return;
    }
    let box_rc = RECT {
        left: cell.left + (cell.right - cell.left - side) / 2,
        top: cell.top + (cell.bottom - cell.top - side) / 2,
        right: cell.left + (cell.right - cell.left + side) / 2,
        bottom: cell.top + (cell.bottom - cell.top + side) / 2,
    };
    let fill = CreateSolidBrush(to_colorref(if on {
        st.theme.foreground
    } else {
        face
    }));
    let _ = FillRect(hdc, &box_rc, fill);
    let _ = DeleteObject(HGDIOBJ(fill.0));
    let frame = CreateSolidBrush(to_colorref(if on {
        st.theme.foreground
    } else {
        st.theme.indent_line
    }));
    let _ = FrameRect(hdc, &box_rc, frame);
    let _ = DeleteObject(HGDIOBJ(frame.0));
    if !on {
        return;
    }
    // 勾是两段线，用 `Polyline` 而不是拿小方块拼：斜边拼出来的锯齿在这个
    // 尺寸（10 px 上下）比线粗一点还显眼。线色是格子的底色，正好压在实心块上。
    let pen = CreatePen(PS_SOLID, (side / 6).max(1), to_colorref(face));
    let old = SelectObject(hdc, HGDIOBJ(pen.0));
    let (l, t, s) = (box_rc.left, box_rc.top, side);
    let pts = [
        POINT { x: l + s * 25 / 100, y: t + s * 52 / 100 },
        POINT { x: l + s * 43 / 100, y: t + s * 70 / 100 },
        POINT { x: l + s * 76 / 100, y: t + s * 32 / 100 },
    ];
    let _ = Polyline(hdc, &pts);
    if !old.is_invalid() {
        SelectObject(hdc, old);
    }
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

unsafe fn on_custom_draw(dlg: HWND, cd: &mut NMLVCUSTOMDRAW) -> isize {
    let stage = cd.nmcd.dwDrawStage.0;
    if stage == CDDS_PREPAINT.0 {
        return CDRF_NOTIFYITEMDRAW as isize;
    }
    if stage == CDDS_ITEMPREPAINT.0 {
        return CDRF_NOTIFYSUBITEMDRAW as isize;
    }
    if stage != (CDDS_ITEMPREPAINT.0 | CDDS_SUBITEM.0) {
        return 0;
    }
    let st = state(dlg);
    // 选中状态问控件要，不看 `uItemState`：逐格这一轮里它带的是**这一格**的状态，
    // 选中位只在整项那一轮出现 —— 照它判断，前两列（色块）永远被当成没选中，
    // 而名字那一列被当成选中，一行里两半颜色对不上。
    let selected = (SendMessageW(
        cd.nmcd.hdr.hwndFrom,
        LVM_GETITEMSTATE,
        Some(WPARAM(cd.nmcd.dwItemSpec)),
        Some(LPARAM(LVIS_SELECTED.0 as isize)),
    )
    .0 as u32
        & LVIS_SELECTED.0)
        != 0;
    let (ink, face) = if selected {
        (st.theme.foreground, st.theme.selection_bg)
    } else {
        (st.theme.foreground, st.theme.background)
    };
    cd.clrText = to_colorref(ink);
    cd.clrTextBk = to_colorref(face);

    let sub = cd.iSubItem as usize;
    let is_colors = item(dlg, ID_GRID) == Some(cd.nmcd.hdr.hwndFrom);
    let is_fonts = item(dlg, ID_FONTS) == Some(cd.nmcd.hdr.hwndFrom);
    // 两处「开关」：颜色表的八个字形列、字体表的「斜体」列。两格都画成方框 ——
    // 原来写的是一个 `✓` 字符，那是**字**，字体里不一定有那个码位，屏幕上一片
    // 空白，看着像「这一列坏了」；而且没开的时候格子是空的，也看不出能点。
    let boxed = if is_colors && sub >= COL_STYLE0 && sub < COL_STYLE0 + STYLE_BITS.len() {
        st.rows
            .borrow()
            .get(cd.nmcd.dwItemSpec)
            .map(|row| row.style & STYLE_BITS[sub - COL_STYLE0] != 0)
    } else if is_fonts && sub == COL_ITALIC {
        st.fonts
            .borrow()
            .get(cd.nmcd.dwItemSpec)
            .map(|row| row.spec.italic)
    } else {
        None
    };
    if let Some(on) = boxed {
        paint_check(cd.nmcd.hdc, &cd.nmcd.rc, on, face, st);
        return CDRF_SKIPDEFAULT as isize;
    }
    // 几何页那张表没有色块：它只要上面那两行（主题的前景/背景）。再往下走会拿
    // **颜色**的第 `sub` 行去填几何的格子 —— 行号对不上，第 1、2 列会画出别的
    // 区域的色块。
    if !is_colors || (sub != COL_BG && sub != COL_FG) {
        return CDRF_NEWFONT as isize;
    }
    // 色块那两列自己画：这两格里没有文字，而底色得是一块实色。先自己把格子铺
    // 上底色 —— `CDRF_SKIPDEFAULT` 之后控件什么都不画，不铺就是一片残留。
    let want = match st.rows.borrow().get(cd.nmcd.dwItemSpec) {
        Some(row) if sub == COL_BG => row.bg,
        Some(row) => row.fg,
        None => return CDRF_NEWFONT as isize,
    };
    // 逐项自绘这一轮里 `nmcd.rc` 就是**这一格**的矩形，不用再问
    // `LVM_GETSUBITEMRECT`：那条消息的输入输出约定（左边界填子项号、上边界填
    // `LVIR_*`）很容易写反，写反了拿回来的是整行，一填就把别的列盖掉。
    let cell = cd.nmcd.rc;
    let back = CreateSolidBrush(to_colorref(face));
    let _ = FillRect(cd.nmcd.hdc, &cell, back);
    let _ = DeleteObject(HGDIOBJ(back.0));
    let Some(color) = want else {
        return CDRF_SKIPDEFAULT as isize;
    };
    let swatch = RECT {
        left: cell.left + SWATCH_INSET_X,
        top: cell.top + SWATCH_INSET_Y,
        right: cell.right - SWATCH_INSET_X,
        bottom: cell.bottom - SWATCH_INSET_Y,
    };
    if swatch.right > swatch.left && swatch.bottom > swatch.top {
        paint_swatch(cd.nmcd.hdc, &swatch, color, st.theme.indent_line);
    }
    CDRF_SKIPDEFAULT as isize
}

/// 点一下某一个字形列：翻一位 → 写回 `themes/<name>.ini` 的 `[Style]`。
///
/// 单点就改，不是双击：这些列没有"选中"可言，勾上就是勾上。下划线那一族是
/// 单选（见 [`toggled_style`]），其余位各自独立。
unsafe fn on_click(dlg: HWND, hit: &NMITEMACTIVATE) -> isize {
    let (row, sub) = (hit.iItem, hit.iSubItem as usize);
    if row < 0 || sub < COL_STYLE0 || sub >= COL_STYLE0 + STYLE_BITS.len() {
        return 0;
    }
    let st = state(dlg);
    let Some((key, style)) = st
        .rows
        .borrow()
        .get(row as usize)
        .map(|r| (r.key.clone(), r.style))
    else {
        return 0;
    };
    let new = toggled_style(style, STYLE_BITS[sub - COL_STYLE0]);
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    match super::themeext::write_key(&name, "Style", &key, &new.to_string()) {
        Ok(path) => {
            if let Some(slot) = st.rows.borrow_mut().get_mut(row as usize) {
                slot.style = new;
            }
            rebuild_grid(dlg);
            set_status(dlg, &format!("已写入 [Style] {key}={new} · {}", path.display()));
            // 文件变了，让主窗口立刻重装，而不是等热重载那 700 ms。
            let _ = PostMessageW(Some(st.frame), WM_THEME_DLG, WPARAM(APPLY), LPARAM(0));
        }
        Err(e) => set_status(dlg, &format!("写不进去：{e}")),
    }
    0
}

/// 双击一个色块：取色 → 写回 `themes/<name>.ini`。
///
/// `[Theme]` 的一个键是 `#背景,#前景` 两段。改哪一段都得把另一段原样带着 ——
/// 只写 `#背景` 会把前景抹掉。
unsafe fn on_dbl_click(dlg: HWND, hit: &NMITEMACTIVATE) -> isize {
    // 命中的行、列都在 `NMITEMACTIVATE` 里：不用再问光标在哪 —— 键盘触发的
    // 激活根本没有光标位置。
    let (row, sub) = (hit.iItem, hit.iSubItem as usize);
    if row < 0 || (sub != COL_BG && sub != COL_FG) {
        return 0;
    }
    let st = state(dlg);
    let Some((key, bg, fg)) = st
        .rows
        .borrow()
        .get(row as usize)
        .map(|r| (r.key.clone(), r.bg, r.fg))
    else {
        return 0;
    };
    let start = if sub == COL_BG {
        bg.unwrap_or_else(|| st.theme.background)
    } else {
        fg.unwrap_or_else(|| st.theme.foreground)
    };
    let Some(picked) = pick_color(dlg, start) else {
        return 0;
    };
    let value = if sub == COL_BG {
        hex_pair(Some(picked), fg)
    } else {
        // 前景单独写没有意义：解析时前半段就是背景，所以背景必须带着。
        hex_pair(Some(bg.unwrap_or(st.theme.background)), Some(picked))
    };
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    match super::themeext::write_key(&name, "Theme", &key, &value) {
        Ok(path) => {
            if let Some(slot) = st.rows.borrow_mut().get_mut(row as usize) {
                if sub == COL_BG {
                    slot.bg = Some(picked);
                } else {
                    slot.fg = Some(picked);
                }
            }
            rebuild_grid(dlg);
            set_status(dlg, &format!("已写入 {}={value} · {}", key, path.display()));
            // 文件变了，让主窗口立刻重装，而不是等热重载那 700 ms。
            let _ = PostMessageW(Some(st.frame), WM_THEME_DLG, WPARAM(APPLY), LPARAM(0));
        }
        Err(e) => set_status(dlg, &format!("写不进去：{e}")),
    }
    0
}

unsafe fn pick_color(dlg: HWND, start: Color) -> Option<Color> {
    let mut custom = [windows::Win32::Foundation::COLORREF(0); 16];
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: dlg,
        rgbResult: to_colorref(start),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    if !ChooseColorW(&mut cc).as_bool() {
        return None;
    }
    let c = cc.rgbResult.0;
    Some(Color::rgb(
        (c & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        ((c >> 16) & 0xFF) as u8,
    ))
}

unsafe fn on_command(dlg: HWND, wparam: WPARAM, lparam: LPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as u16 as i32;
    let code = ((wparam.0 >> 16) & 0xFFFF) as u16 as i32;
    let st = state(dlg);
    match id {
        // 2 是 Esc / 关闭框发的那条命令。`IDCANCEL` 在这个绑定里是
        // `MESSAGEBOX_RESULT`，不是对话框命令号，所以自己写一个。
        ID_CANCEL_CMD | ID_CLOSE => {
            let _ = DestroyWindow(dlg);
            return 1;
        }
        ID_APPLY => {
            // 应用 = 切换主题：装的是**选中的**那个，不是生效的那个。
            let _ = PostMessageW(Some(st.frame), WM_THEME_DLG, WPARAM(SWITCH), LPARAM(0));
            set_status(dlg, "已应用");
            return 1;
        }
        ID_SAVE => {
            save_all(dlg);
            return 1;
        }
        // 页签按钮：切换右半边显示哪一段。不做 BN_CLICKED 判断 ——
        // 自绘按钮的 `BN_CLICKED` 也是 `BN_CLICKED`，而这里只可能是点击。
        ID_TAB_COLORS => {
            set_page(dlg, Page::Colors);
            return 1;
        }
        ID_TAB_METRICS => {
            set_page(dlg, Page::Metrics);
            return 1;
        }
        ID_TAB_FONTS => {
            set_page(dlg, Page::Fonts);
            return 1;
        }
        ID_TAB_SHAPES => {
            set_page(dlg, Page::Shapes);
            return 1;
        }
        ID_THEMES if code == LBN_SELCHANGE as i32 => {
            let list = HWND(lparam.0 as *mut core::ffi::c_void);
            let sel = SendMessageW(list, LB_GETCURSEL, None, None).0;
            if sel >= 0 {
                st.cur.set(sel as usize);
                reload_current_page(dlg);
            }
            return 1;
        }
        ID_ADD => {
            add_theme(dlg);
            return 1;
        }
        ID_DEL => {
            remove_theme(dlg);
            return 1;
        }
        _ => {}
    }
    0
}

/// 保存时这一行该不该写进外部文件。
///
/// 只写**和读进来时不一样**的行。改色/改字形是立刻落盘的，所以这里通常一行都
/// 不写 —— 它存在的意义是「改完一屏再一起存」这个习惯，以及把这次会话动过几行
/// 报出来。
///
/// 没颜色的行（`value` 空）不写：写一个空值等于把键删掉。
fn should_write_color(value: &str, base: &str) -> bool {
    !value.is_empty() && value != base
}

/// 保存：把**改过的**行写回去。
///
/// 每次改色/改几何其实已经落盘了（双击就写），所以这一下通常一个字节都不变 ——
/// `write_key` 见值没变就不碰文件。留着它是因为「改完一屏再一起存」是人的习惯。
///
/// 只写改过的行，不是"把整张表倒出来"：45 个键全写一遍的话，用户改一个颜色
/// 就会凭空拿到一份 45 行的外部副本，把内置默认值**冻结**在那一刻 —— 以后随
/// 包主题改了默认值，他的文件还压着旧值。几何那十三项同理。
unsafe fn save_all(dlg: HWND) {
    let st = state(dlg);
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    let rows: Vec<Row> = st.rows.borrow().clone();
    let mut written = 0usize;
    let mut styled = 0usize;
    for row in &rows {
        let value = hex_pair(row.bg, row.fg);
        let base = hex_pair(row.base.0, row.base.1);
        if should_write_color(&value, &base) {
            if let Err(e) = super::themeext::write_key(&name, "Theme", &row.key, &value) {
                set_status(dlg, &format!("写不进去：{e}"));
                return;
            }
            // 落盘之后它就是「读进来的样子」了：不更新的话再按一次保存会重写一遍、
            // 再报一次「改了 N 行」。
            if let Some(slot) = st.rows.borrow_mut().iter_mut().find(|r| r.key == row.key) {
                slot.base = (row.bg, row.fg);
            }
            written += 1;
        }
        // 字形：只写**动过**的那几行。45 个键全写一遍会让一个从没调过字形的
        // 主题凭空长出几十行 `[Style]`。
        if row.style != row.orig {
            if let Err(e) =
                super::themeext::write_key(&name, "Style", &row.key, &row.style.to_string())
            {
                set_status(dlg, &format!("写不进去：{e}"));
                return;
            }
            if let Some(slot) = st.rows.borrow_mut().iter_mut().find(|r| r.key == row.key) {
                slot.orig = row.style;
            }
            styled += 1;
        }
    }
    // 几何：同一套规矩。十三项全写一遍会让一个从没调过尺寸的主题凭空长出
    // 13 行 `[Metrics]`，把出厂几何冻在用户的文件里。
    let metrics: Vec<MetricRow> = st.metrics.borrow().clone();
    let mut geom = 0usize;
    for row in &metrics {
        if row.value == row.orig {
            continue;
        }
        if let Err(e) =
            super::themeext::write_key(&name, "Metrics", row.id.as_str(), &fmt_num(row.value))
        {
            set_status(dlg, &format!("写不进去：{e}"));
            return;
        }
        if let Some(slot) = st.metrics.borrow_mut().iter_mut().find(|r| r.id == row.id) {
            slot.orig = row.value;
        }
        geom += 1;
    }
    // 字体：一个 token 一行，四个字段各自比。写一个 `mono.size=12` 就够了 ——
    // 同一个 token 缺的字段由 `Fonts::from_theme_file` 按默认值补齐，不会留下
    // 一个坏 token。
    let fonts: Vec<FontRow> = st.fonts.borrow().clone();
    let mut faces = 0usize;
    for row in &fonts {
        for field in 0..FONT_FIELDS.len() {
            let now = match field {
                0 => row.spec.face.clone(),
                1 => fmt_num(row.spec.size),
                2 => row.spec.weight.to_string(),
                _ => if row.spec.italic { "true" } else { "false" }.to_string(),
            };
            let was = match field {
                0 => row.orig.face.clone(),
                1 => fmt_num(row.orig.size),
                2 => row.orig.weight.to_string(),
                _ => if row.orig.italic { "true" } else { "false" }.to_string(),
            };
            if now == was {
                continue;
            }
            let key = format!("{}.{}", row.token.name(), FONT_FIELDS[field]);
            if let Err(e) = super::themeext::write_key(&name, "Fonts", &key, &now) {
                set_status(dlg, &format!("写不进去：{e}"));
                return;
            }
            if let Some(slot) = st.fonts.borrow_mut().iter_mut().find(|r| r.token == row.token) {
                slot.orig = row.spec.clone();
                slot.had = true;
            }
            faces += 1;
        }
    }
    // 形状：一行一层，值没动就不写。常态行写 `tab.radius`、状态行写
    // `tab.states.active.radius` —— 两种拼法都由 `shape_key_text` 给出，和
    // 双击改值时写的是同一个键。
    let shapes: Vec<ShapeRow> = st.shapes.borrow().clone();
    let mut shaped = 0usize;
    for row in &shapes {
        if row.value == row.orig {
            continue;
        }
        let key = shape_key_text(row);
        if let Err(e) =
            super::themeext::write_key(&name, "Shape", &key, &fmt_num(row.value))
        {
            set_status(dlg, &format!("写不进去：{e}"));
            return;
        }
        if let Some(slot) = st
            .shapes
            .borrow_mut()
            .iter_mut()
            .find(|r| r.region == row.region && r.key == row.key && r.state == row.state)
        {
            slot.orig = row.value;
        }
        shaped += 1;
    }
    set_status(
        dlg,
        &format!(
            "已保存 {name} · {written} 个颜色键 · {styled} 个字形 · {geom} 项几何 · {faces} 项字体 · {shaped} 项形状"
        ),
    );
    let _ = PostMessageW(Some(st.frame), WM_THEME_DLG, WPARAM(APPLY), LPARAM(0));
}

/// `+`：把当前主题复制成一个新名字。
///
/// 不动现有主题，也不问名字 —— 名字是 `<原名> 副本`，重名往后加序号。要改名就去
/// 资源管理器里改，比在这里造一个输入框便宜。
unsafe fn add_theme(dlg: HWND) {
    let st = state(dlg);
    let base = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    // 复制的是**合成结果**：外部副本可能只有改过的那几行，原样复制出来的新主题
    // 没有内置底子可叠，会缺一大半颜色。
    let Some(text) = super::themeext::merged_text(&base) else {
        set_status(dlg, "这个主题没有原文可复制");
        return;
    };
    if text.is_empty() {
        set_status(dlg, "这个主题没有原文可复制");
        return;
    }
    let mut n = 1;
    let name = loop {
        let candidate = if n == 1 {
            format!("{base} 副本")
        } else {
            format!("{base} 副本 {n}")
        };
        let taken = st.names.borrow().iter().any(|x| x.eq_ignore_ascii_case(&candidate));
        if !taken && super::themeext::existing(&candidate).is_none() {
            break candidate;
        }
        n += 1;
    };
    let Some(path) = super::themeext::user_path(&name, false) else {
        set_status(dlg, "找不到可写的 themes 目录");
        return;
    };
    if let Err(e) = std::fs::write(&path, text.as_bytes()) {
        set_status(dlg, &format!("{}: {e}", path.display()));
        return;
    }
    *st.names.borrow_mut() = super::themeext::names();
    fill_list(dlg);
    select_theme(dlg, &name);
    set_status(dlg, &format!("已新建 {}", path.display()));
}

/// `×`：删掉这个主题的外部文件。
///
/// 内置主题删的是「用户那份覆盖」，删完回到随包的样子 —— 这不算删主题，所以文案
/// 分开写。自建主题删了就真没了，先问一句。
unsafe fn remove_theme(dlg: HWND) {
    let st = state(dlg);
    let name = st.names.borrow().get(st.cur.get()).cloned().unwrap_or_default();
    let Some(path) = super::themeext::existing(&name) else {
        set_status(dlg, &format!("{name} 没有外部文件可删"));
        return;
    };
    let bundled = crate::themes::BUNDLED_THEMES
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case(&name));
    let msg = if bundled {
        format!(
            "还原本内置主题「{name}」？\n\n会删掉 {}，回到随包的样子。",
            path.display()
        )
    } else {
        format!(
            "删除主题「{name}」？\n\n会删掉 {}，删了就没了。",
            path.display()
        )
    };
    let wide: Vec<u16> = msg.encode_utf16().chain([0]).collect();
    let answer = windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
        Some(dlg),
        PCWSTR::from_raw(wide.as_ptr()),
        w!("主题"),
        MB_YESNO | MB_ICONQUESTION,
    );
    if answer != IDYES {
        return;
    }
    if let Err(e) = std::fs::remove_file(&path) {
        set_status(dlg, &format!("{}: {e}", path.display()));
        return;
    }
    *st.names.borrow_mut() = super::themeext::names();
    fill_list(dlg);
    select_theme(dlg, &name);
    set_status(dlg, &format!("已删除 {}", path.display()));
    let _ = PostMessageW(Some(st.frame), WM_THEME_DLG, WPARAM(APPLY), LPARAM(0));
}

unsafe fn on_measure_item(dlg: HWND, mis: &mut MEASUREITEMSTRUCT) -> isize {
    if mis.CtlID == ID_THEMES as u32 {
        mis.itemHeight = scale(ROW_H, state(dlg).dpi) as u32;
        return 1;
    }
    0
}

/// 左列表的一格：底色 / 选中色 + 主题名。
unsafe fn on_draw_item(dlg: HWND, dis: &DRAWITEMSTRUCT) -> isize {
    if dis.CtlID != ID_THEMES as u32 {
        return 0;
    }
    let st = state(dlg);
    let idx = dis.itemID as usize;
    let Some(name) = st.names.borrow().get(idx).cloned() else {
        return 1;
    };
    let selected = (dis.itemState.0 & ODS_SELECTED.0) != 0;
    let (ink, face) = if selected {
        (st.theme.foreground, st.theme.selection_bg)
    } else {
        (st.theme.foreground, st.theme.background)
    };
    let brush = CreateSolidBrush(to_colorref(face));
    let _ = FillRect(dis.hDC, &dis.rcItem, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));

    let old = SelectObject(dis.hDC, HGDIOBJ(st.font.0));
    let text: Vec<u16> = name.encode_utf16().collect();
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(dis.hDC, &text, &mut size);
    let h = dis.rcItem.bottom - dis.rcItem.top;
    SetTextColor(dis.hDC, to_colorref(ink));
    // 这个 DC 是**控件的**，状态跨帧留着：谁在上面画过一段不透明文字，后面每一
    // 格都会在字后面填一块底色。探针里见过一次 —— 关掉提示框之后选中行变成「白
    // 底浅字」，而这一行在第一次画的时候是好的。所以自己设、自己收。
    let old_mode = SetBkMode(dis.hDC, TRANSPARENT);
    let _ = ExtTextOutW(
        dis.hDC,
        dis.rcItem.left + scale(6, st.dpi),
        dis.rcItem.top + (h - size.cy).max(0) / 2,
        ETO_CLIPPED,
        Some(&dis.rcItem),
        PCWSTR(text.as_ptr()),
        text.len() as u32,
        None,
    );
    if old_mode != 0 {
        let _ = SetBkMode(dis.hDC, BACKGROUND_MODE(old_mode as u32));
    }
    if !old.is_invalid() {
        SelectObject(dis.hDC, old);
    }
    1
}

/// 网格的子类化 id。表头是**列表**的子窗口，它发的 `NM_CUSTOMDRAW` 只到列表
/// 这里，到不了对话框 —— 不接这一道，深色对话框顶上就是一条白带。
const GRID_SUBCLASS: usize = 4;

unsafe extern "system" fn grid_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    dlg: usize,
) -> LRESULT {
    let dlg = HWND(dlg as *mut core::ffi::c_void);
    if msg == WM_NOTIFY {
        let hdr = &*(lparam.0 as *const NMHDR);
        if hdr.code == NM_CUSTOMDRAW && is_header(hwnd, hdr.hwndFrom) {
            return LRESULT(on_header_draw(dlg, hwnd, lparam));
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

unsafe fn is_header(list: HWND, hwnd: HWND) -> bool {
    let header = SendMessageW(list, LVM_GETHEADER, None, None);
    header.0 as isize == hwnd.0 as isize
}

/// 表头的自绘：列表控件顺手建的那个 `SysHeader32` 按系统主题画，深色对话框上
/// 就是一条白带。
///
/// 列标题从列表自己身上问（`LVM_GETCOLUMNW`），四张表共用这一段 —— 表各有一份
/// 列名，再抄一份到这儿就是第四处要同步的地方。
unsafe fn on_header_draw(dlg: HWND, list: HWND, lparam: LPARAM) -> isize {
    let cd = &mut *(lparam.0 as *mut NMCUSTOMDRAW);
    let st = state(dlg);
    let hdc = cd.hdc;
    let stage = cd.dwDrawStage.0;
    if stage == CDDS_PREPAINT.0 {
        let mut rc = RECT::default();
        if GetClientRect(cd.hdr.hwndFrom, &mut rc).is_ok() {
            let _ = FillRect(hdc, &rc, st.face);
        }
        // `POSTPAINT` 那一条是给**最后一列右边**留的：那一段没有列，控件按
        // 默认背景刷一遍，深色表头上就是一块白。
        return (CDRF_NOTIFYITEMDRAW | CDRF_NOTIFYPOSTPAINT) as isize;
    }
    if stage == CDDS_POSTPAINT.0 {
        header_tail(cd.hdr.hwndFrom, hdc, st.face);
        return CDRF_DODEFAULT as isize;
    }
    if stage != CDDS_ITEMPREPAINT.0 {
        return CDRF_DODEFAULT as isize;
    }
    let mut rc = cd.rc;
    let _ = FillRect(hdc, &rc, st.face);
    let divider = RECT {
        left: rc.right - 1,
        top: rc.top + scale(3, st.dpi),
        right: rc.right,
        bottom: rc.bottom - scale(3, st.dpi),
    };
    let _ = FillRect(hdc, &divider, st.edge);
    rc.left += scale(6, st.dpi);
    rc.right -= scale(4, st.dpi);
    let title = column_title(list, cd.dwItemSpec as i32);
    ownerdraw::draw_text(
        hdc,
        rc,
        st.font,
        to_colorref(st.theme.panel_fg),
        &title,
        ownerdraw::Align::Left(0),
    );
    CDRF_SKIPDEFAULT as isize
}

/// 表头最后一列右边那一段：没有列，控件按默认背景刷，深色表头上就是一块白。
unsafe fn header_tail(header: HWND, hdc: HDC, face: HBRUSH) {
    let count = SendMessageW(header, HDM_GETITEMCOUNT, None, None).0 as i32;
    if count <= 0 {
        return;
    }
    let mut last = RECT {
        left: count - 1,
        ..Default::default()
    };
    let _ = SendMessageW(
        header,
        HDM_GETITEMRECT,
        Some(WPARAM(count as usize - 1)),
        Some(LPARAM(&raw mut last as isize)),
    );
    let mut client = RECT::default();
    if GetClientRect(header, &mut client).is_err() {
        return;
    }
    let tail = RECT {
        left: last.right.max(client.left),
        top: client.top,
        right: client.right,
        bottom: client.bottom,
    };
    if tail.right > tail.left {
        let _ = FillRect(hdc, &tail, face);
    }
}

/// 列表某一列的标题。缓冲区按列标题的量给 64 个 `u16` —— 最长那一列是
/// 「文字留白」这种四五个字，写满也不会截。
unsafe fn column_title(list: HWND, i: i32) -> String {
    let mut buf = [0u16; 64];
    let mut col = LVCOLUMNW {
        mask: LVCF_TEXT,
        pszText: windows::core::PWSTR(buf.as_mut_ptr()),
        cchTextMax: buf.len() as i32,
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_GETCOLUMNW,
        Some(WPARAM(i as usize)),
        Some(LPARAM(&raw mut col as isize)),
    );
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    String::from_utf16_lossy(&buf[..len])
}

unsafe fn on_print(hwnd: HWND, hdc: HDC) {
    let mut rc = RECT::default();
    if GetClientRect(hwnd, &mut rc).is_ok() {
        let _ = FillRect(hdc, &rc, state(hwnd).bg);
    }
}

unsafe extern "system" fn dialog_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0 as isize);
            1
        }
        // 对话框的底色：`#32770` 自带的是 `COLOR_3DFACE`，深色主题下就是一圈浅灰。
        // `WM_PRINTCLIENT` 那一条是自绘按钮问的 —— 它要的正是它背后的那块。
        WM_ERASEBKGND | WM_PRINTCLIENT => {
            on_print(hwnd, HDC(wparam.0 as *mut core::ffi::c_void));
            1
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
            let st = state(hwnd);
            mmi.ptMinTrackSize.x = scale(CLIENT_W - 120, st.dpi);
            mmi.ptMinTrackSize.y = scale(CLIENT_H, st.dpi);
            0
        }
        // 静态文字（状态行）问颜色。自绘按钮不会问 —— 它自己画。
        //
        // 左边那个列表也要问，而且**只有它这一条会露出来**：自绘列表只画它交给我们
        // 的那几格，剩下的那条（客户区 408 px ÷ 一格 25 px = 16 格，多出 8 px）由
        // 控件自己拿「背景刷」刷一遍。不答这一条，那个刷子是 `COLOR_WINDOW` ——
        // 滚到最后一格时底下就横着一条纯白。别的对话框早就答了（`shortcutdlg`、
        // `optionsdlg`、`encdlg` 各有一支），这里漏了。
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX => {
            let st = state(hwnd);
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            SetTextColor(hdc, to_colorref(st.theme.foreground));
            let _ = SetBkColor(hdc, to_colorref(st.theme.background));
            st.bg.0 as isize
        }
        WM_MEASUREITEM => {
            let mis = &mut *(lparam.0 as *mut MEASUREITEMSTRUCT);
            on_measure_item(hwnd, mis)
        }
        WM_DRAWITEM => {
            let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if dis.CtlID == ID_THEMES as u32 {
                return on_draw_item(hwnd, dis);
            }
            let st = state(hwnd);
            // 页签按钮：当前那一页画成**按下**的样子。`BS_OWNERDRAW` 没有
            // 「按下就锁住」的形态（那是 `BS_AUTORADIOBUTTON` 的事），自绘按钮的
            // 「按下」就是 `ODS_SELECTED`，`frame`/`label` 都按它取色 —— 所以给
            // 当前页那一份结构体补上这一位，别处的画法一个字都不用改。
            let tab = match dis.CtlID as i32 {
                ID_TAB_COLORS => Some(Page::Colors),
                ID_TAB_METRICS => Some(Page::Metrics),
                _ => None,
            };
            let mut di = *dis;
            if tab == Some(st.page.get()) {
                di.itemState.0 |= ODS_SELECTED.0;
            }
            let look = ownerdraw::Look {
                face: st.face,
                hot: st.face_hot,
                // 页签的「按下」表示当前页，用热色；别的按钮照旧（它们的按下是
                // 手指按住，和这里不是一件事）。
                press: if tab.is_some() { st.face_hot } else { st.face },
                edge: ownerdraw::Edge::Square(st.edge),
                ink: ownerdraw::Ink {
                    normal: to_colorref(st.theme.panel_fg),
                    pressed: to_colorref(st.theme.foreground),
                    disabled: to_colorref(st.theme.menu_disabled),
                },
                font: st.font,
                under: None,
                inset: 0,
                press_offset: 0,
                live_rect: false,
            };
            let rc = ownerdraw::frame(&di, &look);
            let label = CTLS
                .iter()
                .find(|c| c.id == dis.CtlID as i32)
                .map(|c| c.label)
                .unwrap_or("");
            let mut shown = label.to_string();
            if let Some(at) = shown.find('&') {
                shown.remove(at);
            }
            ownerdraw::label(&di, rc, &look, &shown, ownerdraw::Align::Center);
            1
        }
        WM_COMMAND => on_command(hwnd, wparam, lparam),
        // `#32770` 的窗口过程**不把对话框过程的返回值当消息结果**：`NM_CUSTOMDRAW`
        // 在 `CDDS_PREPAINT` 上返回的 `CDRF_NOTIFYITEMDRAW` 会被丢掉，于是永远收不到
        // 逐项那一轮，自绘的色块一格也画不出来。对话框要交结果就得写进
        // `DWLP_MSGRESULT` 再返回 TRUE。
        WM_NOTIFY => {
            let hdr = &*(lparam.0 as *const NMHDR);
            // 四张表都往这里发通知，按窗口分辨：颜色表点格子翻字形位、双击改色，
            // 几何表双击改数字，字体表点「斜体」翻、双击改值，形状表双击改数字。
            // 分不清的话双击一个色块会去改一项几何。
            let metrics = item(hwnd, ID_METRICS) == Some(hdr.hwndFrom);
            let fonts = item(hwnd, ID_FONTS) == Some(hdr.hwndFrom);
            let shapes = item(hwnd, ID_SHAPES) == Some(hdr.hwndFrom);
            // `NM_CLICK` / `NM_DBLCLK` 带的都是 `NMITEMACTIVATE`：命中的行和列
            // 在里面，不用再问光标在哪 —— 键盘触发的激活根本没有光标位置。
            let hit = &*(lparam.0 as *const NMITEMACTIVATE);
            let result = match hdr.code {
                NM_CUSTOMDRAW => on_custom_draw(hwnd, &mut *(lparam.0 as *mut NMLVCUSTOMDRAW)),
                NM_CLICK if fonts => on_font_click(hwnd, hit),
                NM_CLICK if metrics || shapes => 0,
                NM_CLICK => on_click(hwnd, hit),
                NM_DBLCLK if metrics => on_metrics_dbl_click(hwnd, hit),
                NM_DBLCLK if fonts => on_font_dbl_click(hwnd, hit),
                NM_DBLCLK if shapes => on_shapes_dbl_click(hwnd, hit),
                NM_DBLCLK => on_dbl_click(hwnd, hit),
                _ => 0,
            };
            if result == 0 {
                return 0;
            }
            SetWindowLongPtrW(hwnd, WINDOW_LONG_PTR_INDEX(DWLP_MSGRESULT as i32), result);
            1
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            1
        }
        WM_DESTROY => {
            DLG.store(0, Ordering::Release);
            1
        }
        WM_NCDESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !ptr.is_null() {
                let boxed = Box::from_raw(ptr);
                let _ = DeleteObject(HGDIOBJ(boxed.font.0));
                for b in [boxed.bg, boxed.face, boxed.face_hot, boxed.edge] {
                    let _ = DeleteObject(HGDIOBJ(b.0));
                }
                // 标题栏那两张是 `LoadImageW` 给的一份，窗口没了要还。
                let (big, small) = boxed.icons.get();
                for icon in [big, small] {
                    if icon != 0 {
                        let _ = DestroyIcon(HICON(icon as *mut core::ffi::c_void));
                    }
                }
            }
            0
        }
        _ => 0,
    }
}

fn ui_font(dpi: f32) -> HFONT {
    unsafe {
        let face: Vec<u16> = "Segoe UI".encode_utf16().chain([0]).collect();
        CreateFontW(
            -scale(12, dpi),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            PCWSTR::from_raw(face.as_ptr()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fmt_num, font_field_at, font_field_text, font_label, metric_label, range_text,
        shape_key_text, shape_label, shape_rows, should_write_color, toggled_style,
        FontRow, COL_ITALIC, FONT_COLUMNS, FONT_FIELDS, STYLE_BITS, UNDERLINE_FAMILY,
    };

    /// 八列 → 八位，顺序是界面的顺序（粗 斜 矩 下 删 双 点 波）。
    #[test]
    fn style_bits_follow_the_column_order() {
        assert_eq!(STYLE_BITS, [1, 2, 4, 16, 8, 32, 64, 128]);
        assert_eq!(
            UNDERLINE_FAMILY,
            STYLE_BITS[3] | STYLE_BITS[5] | STYLE_BITS[6] | STYLE_BITS[7],
            "下/双/点/波 就是那一族"
        );
    }

    /// 下划线一族单选：点其中一列换掉整族，别的位一个不动。
    #[test]
    fn underline_family_is_a_radio_group() {
        // 下 → 点：下被换掉，不是叠上去。
        assert_eq!(toggled_style(16, 64), 64);
        // 下 + 删，再点波：删留着。
        assert_eq!(toggled_style(16 | 8, 128), 8 | 128);
        // 再点同一列：整族清空，删仍然留着。
        assert_eq!(toggled_style(8 | 128, 128), 8);
        // 一格里挤了三位（文件是手写的，什么都可能有）：点已亮的那一列是
        // "关掉这一族"，三位一起走。
        assert_eq!(toggled_style(16 | 32 | 64, 32), 0);
    }

    /// 其余位各自独立，互不影响。
    #[test]
    fn other_bits_toggle_on_their_own() {
        assert_eq!(toggled_style(0, 1), 1, "粗");
        assert_eq!(toggled_style(1, 2), 3, "斜叠在粗上");
        assert_eq!(toggled_style(3, 1), 2, "粗再点一次就掉");
        assert_eq!(toggled_style(16, 8), 16 | 8, "删不动下划线");
        assert_eq!(toggled_style(16, 4), 16 | 4, "矩不动下划线");
        // 本布局没定义的位（如 256）原样留着：这个函数只碰它认识的那几位。
        assert_eq!(toggled_style(256, 1), 256 | 1);
        assert_eq!(toggled_style(256 | 16, 16), 256);
    }

    /// `保存` 只写改过的行：没动过的键不进外部文件，否则用户改一个颜色就拿到
    /// 一份 45 行的副本，把内置默认值冻结在当时那一版。
    #[test]
    fn save_writes_only_what_changed() {
        // 没动过 → 不写。文件里有没有这个键都一样：`base` 就是读进来的那个值。
        assert!(!should_write_color("#1E2126,#D9DFE8", "#1E2126,#D9DFE8"));
        // 改过 → 写。
        assert!(should_write_color("#1E2126,#FF0000", "#1E2126,#D9DFE8"));
        // 文件里本来是别的值、用户改回内置那个 → 也要写，否则文件压着旧值。
        assert!(should_write_color("#1E2126,#D9DFE8", "#1E2126,#010203"));
        // 读进来时没有这个键（`base` 空）→ 现在有了就写。
        assert!(should_write_color("#1E2126", ""));
        // 没有颜色的一行不写：写空值等于把键删掉。
        assert!(!should_write_color("", ""));
    }

    /// 几何那十三项：每项都要有中文名和范围说法，名字不许撞车。
    ///
    /// 撞了名是复制粘贴的典型后果（两项都写「页签高度」），屏幕上看着像重复行，
    /// 而点哪一行改的都是同一个键 —— 编译器看不出来。
    #[test]
    fn every_metric_has_its_own_label_and_a_range() {
        let mut seen = std::collections::BTreeSet::new();
        for id in dg_skin::MetricId::ALL {
            assert!(seen.insert(metric_label(id)), "{id} 和别的项同名");
            assert!(!range_text(id).is_empty(), "{id} 没有范围");
        }
        // 三种夹取形状各来一条：0 那一档要写出来，离散值列全。
        assert_eq!(range_text(dg_skin::MetricId::StatusHeight), "0 或 16..48");
        assert_eq!(range_text(dg_skin::MetricId::StatusPad), "0..24");
        assert_eq!(range_text(dg_skin::MetricId::ToolbarIcon), "16/24/32");
        // 整数不带 `.0`：这一列是给人抄进 ini 的。
        assert_eq!(fmt_num(16.0), "16");
        assert_eq!(fmt_num(2.5), "2.5");
    }

    /// 字体页那四列就是 `[Fonts]` 认的四个字段。少一个 = 那一列改不了，多一个 =
    /// 写了没反应（解析器会说「不认识的字段」）。
    #[test]
    fn the_font_columns_are_the_four_fields_the_parser_knows() {
        assert_eq!(FONT_FIELDS, ["face", "size", "weight", "italic"]);
        let mut seen = std::collections::BTreeSet::new();
        for token in dg_skin::FontToken::ALL {
            assert!(seen.insert(font_label(token)), "{token} 和别的 token 同名");
            assert!(!token.name().is_empty());
        }
    }

    /// 列号 → 字段号。这一格错一位就是「点字号翻了斜体」—— 编译期看不出来，
    /// 屏幕上也只是多写了一个键。
    #[test]
    fn the_font_table_maps_columns_to_fields() {
        assert_eq!(font_field_at(2), Some(0), "字面");
        assert_eq!(font_field_at(3), Some(1), "字号");
        assert_eq!(font_field_at(4), Some(2), "字重");
        assert_eq!(font_field_at(COL_ITALIC), None, "斜体只走单击");
        assert_eq!(font_field_at(0), None, "字体那一列是名字");
        assert_eq!(font_field_at(1), None, "用途那一列是名字");
        // 2 列名字 + 4 个字段 = 6 列。多一列就有一个字段没地方放。
        assert_eq!(FONT_COLUMNS.len(), 2 + FONT_FIELDS.len());
    }

    /// 文件里没写这个 token 时四格全空 —— 摆一个 9 pt 出来等于告诉用户「你的编辑器
    /// 就是 9 pt」，而真正生效的是内置字体（系统字面 + `setting.ini`）。
    #[test]
    fn an_unwritten_token_shows_empty_cells() {
        let row = FontRow {
            token: dg_skin::FontToken::Mono,
            spec: Default::default(),
            orig: Default::default(),
            had: false,
        };
        for field in 0..FONT_FIELDS.len() {
            assert_eq!(font_field_text(&row, field), "", "field {field}");
        }
        // 写过一次之后，四格显示的就是真正生效的那份 spec。
        let written = FontRow { had: true, ..row };
        assert_eq!(font_field_text(&written, 1), "9");
        assert_eq!(font_field_text(&written, 2), "400");
        assert_eq!(font_field_text(&written, 3), "", "没开斜体就是空格子");
    }

    /// 形状页的行 = 已接线区域 × 它认的键，一个不多一个不少。
    ///
    /// 空文件时每层都是出厂值 0，行数就是 `WIRED_REGIONS` 那三个区域认的键数
    /// 加起来 —— 十行。哪天真把一个区域接上绘制，忘了把键加进表里，这条会红。
    #[test]
    fn the_shape_rows_are_the_wired_keys() {
        let rows = shape_rows(None);
        let want: usize = dg_skin::WIRED_REGIONS.iter().map(|w| w.keys.len()).sum();
        assert_eq!(rows.len(), want, "空文件时每个键只有常态一行");
        assert_eq!(rows.len(), 10);
        assert!(rows.iter().all(|r| r.state.is_none() && r.value == 0.0));
        for row in &rows {
            assert!(!shape_label(row.region, row.key).is_empty());
            assert_eq!(shape_key_text(row), format!("{}.{}", row.region, row.key.name()));
        }
    }

    /// 文件里的状态层各占一行，顺序按 `State::NAMES`，值就是写进去那个。
    ///
    /// 常态行显示的是**常态**那一层的值，不是叠加状态之后的结果：`tab.radius=4`
    /// 加 `tab.states.active.radius=0`，两行分别是 4 和 0 —— 一格里要是显示 4，
    /// 用户会以为活动页签也是圆角。
    #[test]
    fn a_state_layer_gets_its_own_row() {
        let file = dg_render::parse_theme_ini(
            "[Shape]\ntab.radius=4\ntab.states.active.radius=0\ntab.states.hot.gap=2\n",
        );
        let rows = shape_rows(Some(&file));
        let base = rows
            .iter()
            .find(|r| r.region == "tab" && r.key == dg_skin::ShapeKey::Radius && r.state.is_none())
            .expect("常态那一行");
        assert_eq!(base.value, 4.0);
        let active = rows
            .iter()
            .find(|r| r.state == Some("active") && r.key == dg_skin::ShapeKey::Radius)
            .expect("活动层");
        assert_eq!(active.value, 0.0);
        assert_eq!(shape_key_text(active), "tab.states.active.radius");
        assert!(rows.iter().any(|r| shape_key_text(r) == "tab.states.hot.gap"));
    }

    /// 区域族写得进：`status.gap` 落到 `status.pane` 上（`[Shape]` 认
    /// `status` / `scroll` / `menu` 三个族，展开规则在 `Recipe` 里）。
    ///
    /// 这一条守的是「表里的区域名」和「文件里的区域名」不是一回事：用户写
    /// `status.gap=2`，表里必须显示在 `status.pane` 那一行，否则改完看不见。
    #[test]
    fn a_region_family_reaches_its_pane() {
        let file = dg_render::parse_theme_ini("[Shape]\nstatus.gap=2\n");
        let rows = shape_rows(Some(&file));
        let row = rows
            .iter()
            .find(|r| r.region == "status.pane" && r.key == dg_skin::ShapeKey::Gap)
            .expect("状态栏那一行");
        assert_eq!(row.value, 2.0);
        assert_eq!(shape_label(row.region, row.key), "文字留白", "状态栏的 gap 是文字留白");
    }

    /// 越界的值在解析时就夹住，表里显示的是夹完的 —— 和几何页同一个态度。
    #[test]
    fn an_out_of_range_shape_value_shows_clamped() {
        let file = dg_render::parse_theme_ini("[Shape]\ntab.radius=99\ntab.border=-3\n");
        let rows = shape_rows(Some(&file));
        let radius = rows
            .iter()
            .find(|r| r.key == dg_skin::ShapeKey::Radius && r.state.is_none())
            .expect("圆角");
        assert_eq!(radius.value, 24.0, "radius 的上界");
        let border = rows
            .iter()
            .find(|r| r.key == dg_skin::ShapeKey::Border && r.state.is_none())
            .expect("描边");
        assert_eq!(border.value, 0.0, "负数夹到 0");
    }
}
