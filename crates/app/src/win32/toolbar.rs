//! The main toolbar (`ToolbarWindow32`).
//!
//! A common control carries the 42-button default layout and an image list
//! built from `assets/sprites/toolbar{16,24,32}.bmp` (soft ink and crisp
//! edges for a light bar) or from `assets/sprites/dark/` (light ink for a
//! dark bar); dropdown arrows mark the buttons whose style carries the
//! dropdown bit. Geometry, hit-testing and the hot/pressed states stay the
//! control's.
//!
//! Colour is ours: the frame answers `NM_CUSTOMDRAW` — the band at
//! `CDDS_PREPAINT`, then per button the state fill, the 1 px frame, the ink,
//! and for dropdowns the arrow repainted in the band's ink at
//! `CDDS_ITEMPOSTPAINT`.

#![allow(unsafe_op_in_unsafe_fn)]

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DC_BRUSH, DEFAULT_GUI_FONT,
    DIB_RGB_COLORS, DeleteObject, FillRect, FrameRect, GetDC, GetObjectW,
    GetStockObject, HBRUSH, HDC, HFONT, HGDIOBJ, LOGFONTW, MapWindowPoints, InvalidateRect,
    IntersectClipRect, ReleaseDC, RestoreDC, SaveDC, ScreenToClient, SetDCBrushColor, SetViewportOrgEx,
};
use windows::Win32::UI::Controls::{
    BTNS_DROPDOWN, BTNS_SEP, CDDS_ITEMPOSTPAINT, CDDS_ITEMPREPAINT, CDDS_POSTPAINT, CDDS_PREPAINT,
    CDIS_CHECKED,
    CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW,
    CDRF_NOTIFYPOSTPAINT, CDRF_SKIPDEFAULT, HIMAGELIST, ICC_BAR_CLASSES, ILC_COLOR32,
    ILD_BLEND50, IMAGELISTDRAWPARAMS, INITCOMMONCONTROLSEX, ImageList_Add, ImageList_Create,
    ImageList_DrawIndirect, ImageList_Destroy, ImageList_GetImageCount, InitCommonControlsEx,
    NMCUSTOMDRAW_DRAW_STATE_FLAGS, NMTBCUSTOMDRAW, TBCDRF_NOBACKGROUND,
    TBCDRF_NOEDGES, TBCDRF_USECDCOLORS, TBBUTTON, TBBUTTONINFOW, TBIF_STYLE, TBSTYLE_AUTOSIZE,
    TBSTYLE_EX_DRAWDDARROWS, TBSTYLE_FLAT, TBSTYLE_TRANSPARENT, TBSTYLE_WRAPABLE,
    TB_HITTEST, WM_MOUSELEAVE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateMenu, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, GWL_STYLE,
    GWLP_WNDPROC, GetClientRect, GetParent, GetSubMenu, GetWindowLongPtrW, GetWindowRect,
    HTCLIENT, HMENU, IsWindowVisible, MF_POPUP, SW_HIDE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TPM_LEFTALIGN, TPM_LEFTBUTTON, TPM_RETURNCMD,
    TrackPopupMenu, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CONTEXTMENU, WM_ERASEBKGND, WM_GETFONT,
    WM_MOUSEMOVE, WM_SETCURSOR,
    WNDPROC, WS_EX_COMPOSITED,
};
use windows::core::{PCWSTR, w};

use crate::win32::chrome;
use crate::win32::view::instance;
use crate::win32::window::{EditorState, high_word, low_word, state_mut, to_colorref};
use dg_render::Rect;
use dg_render::theme::{Color, Theme};
use dg_ui::menu::MenuItem;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) const CTRL_ID: u32 = 0xE800;

/// 皮肤按钮（`s:toolbar`）的 id 起点：第 n 个按钮是 `TB_SCRIPT_BASE + n`。
///
/// **必须高过所有内置命令号**。命令号是 `WM_COMMAND` 的低字（16 位），内置那套
/// 一直用到 59393（`PALETTE` 最后两条），所以起点取 0xF000 —— 留出一千多个位置，
/// 又还在 16 位以内。第一次取的是 0x9000（36864），看着离状态栏的
/// `SCRIPT_BASE(0x8000)` 很远，其实比 `57600`（文件-新建）还低，于是
/// 「把皮肤那几行摘出来」的 `retain` 顺手把文件、编辑、查看那几组全摘掉了 ——
/// 工具栏只剩三十格，而且看不出来是掉了。
///
/// 和状态栏的 `0x8000` 错开是因为两条 id 都从 `WM_COMMAND` 低字进来，撞上会互相
/// 截胡。
pub(super) const TB_SCRIPT_BASE: u32 = 0xF000;

/// The class name of the common control; comctl32 owns it.
const TOOLBAR_CLASS: PCWSTR = w!("ToolbarWindow32");

/// The messages this implementation sends the control. Comctl32's `TB_` range is
/// `WM_USER`-based and the crate does not export the send-codes.
const TB_BUTTONSTRUCTSIZE: u32 = 0x041E;
const TB_SETEXTENDEDSTYLE: u32 = 0x0454;
const TB_SETPADDING: u32 = 0x0457;
/// `TB_SETBUTTONSIZE` = `WM_USER + 31`, `TB_SETBITMAPSIZE` = `WM_USER + 32`.
const TB_SETBUTTONSIZE: u32 = 0x041F;
const TB_SETBITMAPSIZE: u32 = 0x0420;
/// `TB_GETBUTTONSIZE`, whose high word is the button height the band measure
/// takes its row from
/// the toolbar: the control's own answer, plus the 2 px it adds for the border.
const TB_GETBUTTONSIZE: u32 = 0x043A;
const TB_SETIMAGELIST: u32 = 0x0430;
const TB_ADDBUTTONSW: u32 = 0x0444;
const TB_GETRECT: u32 = 0x0433;
/// `toolbar.button` 这个区域名：绘制和悬停声明用的是同一个词。
const SKIN_REGION: &str = "toolbar.button";
const TB_ENABLEBUTTON: u32 = 0x0401;
/// `TB_CHECKBUTTON` = `WM_USER + 2`, the pressed face a toggle wears.
const TB_CHECKBUTTON: u32 = 0x0402;
/// `TB_GETBUTTONINFOW`, sent by the toolbar custom-draw handler to ask one button whether it
/// is a dropdown before it agrees to paint the arrow.
const TB_GETBUTTONINFOW: u32 = 0x043F;
/// `TB_GETEXTENDEDSTYLE`, sent first: with no
/// `TBSTYLE_EX_DRAWDDARROWS` no button has an arrow to repaint.
const TB_GETEXTENDEDSTYLE: u32 = 0x0455;

/// The width of the strip the control's own dropdown arrow occupies. At 96 DPI they
/// are 14 and 3, i.e. a 7 px wide, 4 px tall ▼.
const ARROW_GUTTER: i32 = 14;
const ARROW_HALF: i32 = 3;

const TBSTATE_ENABLED: u8 = 0x04;
const TBSTATE_CHECKED: u8 = 0x01;

/// One row of the button array, as the control stores it.
#[derive(Clone, Copy)]
pub(super) struct TbRow {
    /// Image-list index (`8` is the blank the separators carry).
    pub bmp: i32,
    /// Command id, `0` on a separator.
    pub id: u32,
    pub state: u8,
    /// `BTNS_SEP` (1) or `BTNS_DROPDOWN` (8).
    pub style: u8,
}

/// The 42-button default layout: `(image, command, state, style)`. Separators carry image
/// 8, command 0 and `BTNS_SEP`. The three dropdown rows (57600, 57637, 1320) carry
/// `BTNS_DROPDOWN` (8).
const DEFAULT_LAYOUT: &[(i32, u32, u8, u8)] = &[
    (0, 57600, 4, 8),
    (1, 57601, 4, 0),
    (5, 57603, 4, 0),
    (7, 57604, 4, 0),
    (8, 0, 4, 1),
    (13, 57643, 0, 0),
    (14, 57644, 0, 0),
    (8, 0, 4, 1),
    (15, 57635, 0, 0),
    (16, 57634, 0, 0),
    (18, 57637, 4, 8),
    (8, 0, 4, 1),
    (30, 1320, 5, 8),
    (25, 1413, 4, 0),
    (8, 0, 4, 1),
    (69, 57636, 4, 0),
    (72, 1403, 4, 0),
    (73, 57641, 4, 0),
    (75, 1408, 4, 0),
    (74, 1420, 4, 0),
    (8, 0, 4, 1),
    (95, 1412, 4, 0),
    (8, 0, 4, 1),
    (84, 1570, 4, 0),
    (86, 1571, 0, 0),
    (87, 1573, 0, 0),
    (8, 0, 4, 1),
    (110, 1569, 4, 0),
    (66, 1328, 4, 0),
    (8, 0, 4, 1),
    (43, 1312, 4, 0),
    (44, 1292, 4, 0),
    (45, 1291, 4, 0),
    (46, 1325, 4, 0),
    (47, 1329, 4, 0),
    (8, 0, 4, 1),
    (52, 1239, 4, 0),
    (51, 1489, 4, 0),
    (8, 0, 4, 1),
    (48, 1290, 4, 0),
    (8, 0, 4, 1),
    (111, 57664, 4, 0),
];

/// Command id → image index. The strip carries one image per toolbar command,
/// and this table is what maps a command to its cell: the bar's rows, a stored
/// `[ToolBar]` layout and the 工具条 options page's list all resolve through it.
/// A stored id outside the table is skipped, which cannot happen with the
/// shipped `setting.ini` (it carries no `[ToolBar]` section). Commands this
/// build cannot run stay in the table so their buttons grey out rather than
/// vanish from the list.
const PALETTE: &[(u32, i32)] = &[
    (30, 40),
    (36, 29),
    (47, 31),
    (48, 32),
    (52, 90),
    (54, 33),
    (57, 34),
    (58, 35),
    (62, 89),
    (72, 38),
    (81, 39),
    (82, 37),
    (85, 41),
    (102, 36),
    (1202, 8),
    (1203, 10),
    (1206, 9),
    (1212, 23),
    (1213, 24),
    (1221, 26),
    (1222, 79),
    (1223, 27),
    (1234, 12),
    (1236, 3),
    (1237, 4),
    (1239, 52),
    (1240, 6),
    (1244, 21),
    (1245, 22),
    (1276, 91),
    (1277, 92),
    (1278, 93),
    (1290, 48),
    (1291, 45),
    (1292, 44),
    (1300, 68),
    (1301, 70),
    (1312, 43),
    (1313, 76),
    (1314, 77),
    (1320, 30),
    (1324, 78),
    (1325, 46),
    (1327, 71),
    (1328, 66),
    (1329, 47),
    (1336, 105),
    (1338, 82),
    (1339, 85),
    (1340, 88),
    (1401, 63),
    (1402, 64),
    (1403, 72),
    (1408, 75),
    (1412, 95),
    (1413, 25),
    (1414, 50),
    (1415, 49),
    (1416, 53),
    (1417, 59),
    (1418, 60),
    (1420, 74),
    (1424, 109),
    (1428, 42),
    (1429, 58),
    (1459, 106),
    (1460, 107),
    (1469, 83),
    (1480, 54),
    (1481, 55),
    (1482, 56),
    (1489, 51),
    (1569, 110),
    (1570, 84),
    (1571, 86),
    (1573, 87),
    (1578, 98),
    (1580, 108),
    (1614, 99),
    (1617, 61),
    (1618, 62),
    (1648, 19),
    (1649, 80),
    (1650, 17),
    (1655, 20),
    (1657, 81),
    (1659, 96),
    (1662, 2),
    (1665, 97),
    (1667, 94),
    (1681, 100),
    (1682, 101),
    (1695, 57),
    (1760, 65),
    (1762, 67),
    (36007, 102),
    (57600, 0),
    (57601, 1),
    (57603, 5),
    (57604, 7),
    (57634, 16),
    (57635, 15),
    (57636, 69),
    (57637, 18),
    (57641, 73),
    (57642, 28),
    (57643, 13),
    (57644, 14),
    (57664, 111),
    (57665, 11),
    (59392, 103),
    (59393, 104),
];

/// Commands whose button carries the dropdown bit. The full set in the
/// binary is `{1320, 57600, 1600, 57637, 1284, 1756, 1246, 1247}`; the
/// five the default layout does not name stay in the set so a `[ToolBar]`
/// restore reproduces them.
const WHOLE_DROPDOWN: &[u32] = &[1320, 57600, 1600, 57637, 1284, 1756, 1246, 1247];

/// The band the bar occupies when visible: the bitmap square plus the
/// control's own padding and borders, which the probe measured as
/// `icon_size + 8` (24 px at the default 16 on a 96 DPI display), scaled
/// by the same factor the glyphs are loaded at — 30 px at 120 DPI.
pub(super) fn band_h(icon_size: i32, dpi: u32) -> f32 {
    (icon_size + 8) as f32 * dpi_scale(dpi)
}

/// Any other value falls back to 16. DPI does not enter here — it
/// scales the bitmap instead.
pub(super) fn icon_size(setting: Option<&str>) -> i32 {
    match setting.and_then(|v| v.trim().parse::<i32>().ok()) {
        Some(v @ (16 | 24 | 32)) => v,
        _ => 16,
    }
}

/// One row of a button's dropdown menu.
pub(super) struct DropdownRow {
    /// The command id the row dispatches — or, for the clipboard
    /// history, the history position the frame resolves itself.
    pub id: u32,
    pub label: String,
    pub enabled: bool,
    /// A rule between two groups. `id`, `label` and `enabled` go unread.
    pub separator: bool,
}

impl DropdownRow {
    /// A command row.
    pub(super) fn row(id: u32, label: String, enabled: bool) -> Self {
        Self {
            id,
            label,
            enabled,
            separator: false,
        }
    }

    /// The hairline the 自动换行 menu puts under 取消换行 and under the
    /// width rows, the same two the 文档 menu draws.
    pub(super) fn separator() -> Self {
        Self {
            id: 0,
            label: String::new(),
            enabled: false,
            separator: true,
        }
    }
}

/// The bar window, plus what the control cannot hold for itself: the
/// layout table it was built from and the dropdown menu skeleton.
pub(super) struct ToolbarWin {
    hwnd: HWND,
    /// The control's own window procedure, saved at subclass time.
    orig: WNDPROC,
    /// Last rect handed to `SetWindowPos`, so a layout that changes
    /// nothing costs no window message.
    frame: Rect,
    icon_size: i32,
    /// The DPI the image list was built at, which is what the band
    /// height scales from.
    dpi: u32,
    /// The button array as it was handed to `TB_ADDBUTTONSW`, kept
    /// so an index names a command without another message.
    buttons: Vec<TbRow>,
    /// The dropdown menu: one empty popup per button position, so
    /// `GetSubMenu(menu, button_index)`
    /// structure — names the button's own menu. `popup_at` fills the
    /// rows in when an arrow is clicked.
    drop_menu: HMENU,
    /// Enable state of undo/redo/cut/copy, in that order, so
    /// `update_states` sends `TB_ENABLEBUTTON` only on a change. Every row the
    /// control is built with answers enabled, so this starts as the control's
    /// own state rather than the `0` the table carries.
    states: [bool; 4],
    /// The palette the control's visual style was last asked for, so a
    /// layout that changes nothing costs no `SetWindowTheme`.
    dark: bool,
    /// The image list the buttons draw from. The toolbar control does not own
    /// it, so a pane that is closed has to free it explicitly.
    himl: HIMAGELIST,
    /// Whether this bar carries the output pane's nine-glyph sheet, which is
    /// what a theme swap rebuilds instead of the 112-glyph strip.
    output_sheet: bool,
    /// 多行显示 (`[Setting] WrapToolBar`): the bar wraps its buttons into
    /// further rows when the width runs out, so the band height follows the
    /// control's own row count instead of the fixed square.
    wrap: bool,
    /// 皮肤：`toolbar.button` 有回调时，每一格的底色和边框交给它画。
    script: Option<std::rc::Rc<dg_script::ScriptHost>>,
    /// 指针停在哪一格（控件自己的索引，`-1` = 没有）。comctl32 的热跟踪在这条
    /// 自定义主题的工具栏上不启用，所以这一位是我们自己跟的 —— 见 `tb_proc` 的
    /// `WM_MOUSEMOVE`。
    hot: i32,
    /// 皮肤要的指针形状（`s:hint("toolbar.button", {cursor=…})`）。提示不在这里
    /// —— 这条工具栏的提示是控件按 `TTN_GETDISPINFOW` 问 `cmdtext` 要的。
    hint: crate::win32::skinhint::SkinHint,
    /// `s:toolbar` 声明的按钮，`skin.lua` 重跑时刷新一次。
    ///
    /// 缓存一份而不是每次问 `ScriptHost`：绘制、提示、点击三条路都要按 id 找
    /// 按钮，而 `toolbar_buttons()` 每次都会把整张表从注册表读出来复制一遍 ——
    /// 每格每帧一次就是 O(n²)。
    script_rows: Vec<dg_script::ToolbarButton>,
}

impl ToolbarWin {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            orig: None,
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            icon_size: 16,
            dpi: 96,
            buttons: Vec::new(),
            drop_menu: HMENU::default(),
            states: [true; 4],
            dark: false,
            himl: HIMAGELIST(0),
            output_sheet: false,
            wrap: false,
            script: None,
            hot: -1,
            hint: crate::win32::skinhint::SkinHint::new(),
            script_rows: Vec::new(),
        }
    }

    /// 换上（或撤掉）皮肤。句柄没变就什么都不做：布局每次都会调一遍。
    pub(super) fn set_script(&mut self, host: Option<std::rc::Rc<dg_script::ScriptHost>>) {
        if self.script.as_ref().map(std::rc::Rc::as_ptr) == host.as_ref().map(std::rc::Rc::as_ptr) {
            return;
        }
        self.script = host;
        // 换皮要重画整条：按钮的底色是皮肤给的，控件自己不觉得有什么变了。
        if self.alive() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, true);
            }
        }
    }

    pub(super) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// The band height: the configured size plus the control's own
    /// border allowance, at the DPI the bar was created for.
    pub(super) fn band_h(&self) -> f32 {
        band_h(self.icon_size, self.dpi)
    }

    /// The band height at `width`: the fixed square normally, and for a
    /// wrapping bar the height the control settles on once its buttons
    /// have reflowed at that width — it fits itself, `CCS_NORESIZE` being
    /// off while wrapping.
    pub(super) unsafe fn band_h_at_width(&mut self, width: i32) -> f32 {
        let fixed = self.band_h();
        if !self.wrap || !self.alive() {
            return fixed;
        }
        let _ = SetWindowPos(
            self.hwnd,
            None,
            0,
            0,
            width,
            0,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let mut rc = RECT::default();
        if GetWindowRect(self.hwnd, &mut rc).is_err() {
            return fixed;
        }
        ((rc.bottom - rc.top) as f32).max(fixed)
    }

    /// The glyph square the bar is built at.
    pub(super) fn icon_size(&self) -> i32 {
        self.icon_size
    }

    /// Swaps the bar to a different glyph square. The control is rebuilt rather
    /// than re-told, because the cells a row settles on are worked out from the
    /// image list only while it has never painted: a control that has painted
    /// keeps the wider cells the previous square needed, so 小图标 after 大图标
    /// would leave every glyph swimming in the row it just left.
    pub(super) unsafe fn set_icon_size(&mut self, parent: HWND, dark: bool, icon_size: i32) {
        if self.icon_size == icon_size {
            return;
        }
        self.icon_size = icon_size;
        self.rebuild(parent, dark);
    }

    /// Hands the bar a new button table — the 工具条 page's 应用 — so the row the
    /// user just edited is on screen before the dialog closes. The control is
    /// rebuilt with the new table: emptying a row that has painted leaves the
    /// common control reading a freed button array.
    pub(super) unsafe fn set_layout(&mut self, layout: Vec<TbRow>, parent: HWND, dark: bool) {
        self.install(self.with_script(layout), parent, dark);
    }

    /// 皮肤那几行换了（`skin.lua` 热重载）：底表不动，只重算插入结果。
    ///
    /// 返回 `true` = 按钮表真的变了，调用方得重排一次 —— 多一个按钮就是多一格
    /// 宽，工具栏占的那条带子跟着变。
    pub(super) unsafe fn reload_script(&mut self, parent: HWND, dark: bool) -> bool {
        self.refresh_script_rows();
        let next = self.with_script(self.base_layout());
        let before = self.buttons.len();
        let changed = next.len() != before
            || next
                .iter()
                .zip(self.buttons.iter())
                .any(|(a, b)| (a.bmp, a.id, a.state, a.style) != (b.bmp, b.id, b.state, b.style));
        self.install(next, parent, dark);
        changed
    }

    /// 现在这张按钮表去掉皮肤那几行之后的样子 —— 也就是用户的底表。
    ///
    /// 不另存一份底表：`TB_SCRIPT_BASE` 以上的 id 只可能来自 `s:toolbar`，一条
    /// `retain` 就还原了。两份表分开存的话，`rebuild` 那条「拿 `self.buttons`
    /// 重建」的路会和它分叉。
    fn base_layout(&self) -> Vec<TbRow> {
        let mut base = self.buttons.clone();
        base.retain(|row| row.id < TB_SCRIPT_BASE);
        base
    }

    /// 底表 + `s:toolbar` 的按钮，按声明顺序插进去。
    ///
    /// 每一条都插在它 `after` 那一格后面；`after` 找不到（或写 0）就落到末尾。
    /// 两条声明同一个 `after` 时后者排在前者后面 —— 声明顺序即最终顺序。
    fn with_script(&self, mut layout: Vec<TbRow>) -> Vec<TbRow> {
        // 传进来的可能已经是带皮肤的表（`rebuild` 那条路），先去干净。
        layout.retain(|row| row.id < TB_SCRIPT_BASE);
        for (i, button) in self.script_rows.iter().enumerate() {
            let row = TbRow {
                bmp: button.icon,
                id: TB_SCRIPT_BASE + i as u32,
                // 皮肤按钮恒可用：命令跑不跑得起来是点击那一刻的事，画成灰的
                // 等于提前替脚本撒谎。
                state: 4,
                style: 0,
            };
            let at = if button.after == 0 {
                None
            } else {
                layout.iter().rposition(|r| r.id == button.after)
            };
            match at {
                Some(i) => layout.insert(i + 1, row),
                None => layout.push(row),
            }
        }
        layout
    }

    /// 重新读一遍 `s:toolbar` 的声明。`skin.lua` 跑完之后调，别的时候不必。
    fn refresh_script_rows(&mut self) {
        self.script_rows = self
            .script
            .as_ref()
            .map(|host| host.toolbar_buttons())
            .unwrap_or_default();
    }

    /// 第 `index` 个皮肤按钮。
    pub(super) fn script_button(&self, index: u32) -> Option<&dg_script::ToolbarButton> {
        self.script_rows.get(index as usize)
    }

    /// 装上一张按钮表：真的不一样才重建。
    unsafe fn install(&mut self, layout: Vec<TbRow>, parent: HWND, dark: bool) {
        let same = layout.len() == self.buttons.len()
            && layout
                .iter()
                .zip(self.buttons.iter())
                .all(|(a, b)| (a.bmp, a.id, a.state, a.style) == (b.bmp, b.id, b.state, b.style));
        if same || !self.alive() {
            return;
        }
        self.buttons = layout;
        self.rebuild(parent, dark);
    }

    /// Destroys the control and builds it again at the size and table the struct
    /// already holds.
    unsafe fn rebuild(&mut self, parent: HWND, dark: bool) {
        if !self.alive() {
            return;
        }
        let (hwnd, frame, dpi, wrap, icon_size) =
            (self.hwnd, self.frame, self.dpi, self.wrap, self.icon_size);
        let visible = IsWindowVisible(hwnd).as_bool();
        let layout = std::mem::take(&mut self.buttons);
        if !self.drop_menu.0.is_null() {
            let _ = DestroyMenu(self.drop_menu);
        }
        self.release();
        let _ = DestroyWindow(hwnd);
        self.hwnd = HWND::default();
        self.drop_menu = HMENU::default();
        // 重建后按钮表换了，旧索引可能指到别的格上。
        self.hot = -1;
        self.create(parent, icon_size, dpi, visible, dark, layout, wrap);
        self.reapply_states();
        // The new window sits where nothing has put it yet, and `place` skips a
        // frame it believes it already has.
        self.frame = Rect::new(-1.0, -1.0, -1.0, -1.0);
        self.place(frame);
    }

    /// 多行显示 (`[Setting] WrapToolBar`): on, the control wraps its buttons
    /// into further rows instead of clipping them when the width runs out.
    /// The band height is the frame's to re-measure.
    pub(super) unsafe fn set_wrap(&mut self, on: bool) {
        self.wrap = on;
        if !self.alive() {
            return;
        }
        // 自动尺寸 pins the bar to one row; a wrapping bar lets the control
        // own its height instead, which is what the row count needs.
        let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE) as u32;
        let style = if on {
            (style | TBSTYLE_WRAPABLE) & !(TBSTYLE_AUTOSIZE | 0x0004)
        } else {
            (style & !TBSTYLE_WRAPABLE) | TBSTYLE_AUTOSIZE | 0x0004
        };
        SetWindowLongPtrW(self.hwnd, GWL_STYLE, style as isize);
        let _ = InvalidateRect(Some(self.hwnd), None, true);
    }

    /// Creates the control and subclasses it. The bar is a sibling of
    /// the editor window, and creation order is stacking order, so the
    /// frame creates it before the view. `dark` is the loaded theme's own
    /// answer and only decides the visual style the control resolves for
    /// the parts it still draws itself; every colour the frame owns comes
    /// from `Theme` in `custom_draw`.
    pub(super) unsafe fn create(
        &mut self,
        parent: HWND,
        icon_size: i32,
        dpi: u32,
        visible: bool,
        dark: bool,
        layout: Vec<TbRow>,
        wrap: bool,
    ) {
        // comctl32 registers the toolbar class only once the app has
        // asked for it; without this the window below silently fails
        // to exist.
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        });
        if self.alive() {
            return;
        }
        self.icon_size = icon_size;
        self.dpi = dpi;
        let Ok(hwnd) = CreateWindowExW(
            // `WS_EX_COMPOSITED`: the control paints its band, its separators
            // and every hovered cell straight to the window surface, and a
            // hover repaints the whole strip. Without a composed pass the
            // band fill and the buttons reach the screen as separate paints,
            // which is the strip's hover flash. The flag stages the pass
            // off-screen and shows one frame.
            WINDOW_EX_STYLE(WS_EX_COMPOSITED.0),
            TOOLBAR_CLASS,
            w!("Toolbar"),
            // The strip's creation flags, including `CCS_NODIVIDER` (0x40): the
            // control otherwise reserves a 2 px raised divider above its
            // client, which the default non-client pass paints in the
            // system's 3-D colours, and the band must start flush under the menu.
            // A wrapping bar drops `CCS_NORESIZE`: that bit pins the control
            // to the one-row size it was given and a wrapped reflow needs to
            // own its height.
            WINDOW_STYLE(if wrap { 0x50000140 } else { 0x50000144 }),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(CTRL_ID as usize as *mut core::ffi::c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.hwnd = hwnd;
        // The visual style the control resolves for itself still decides how
        // it hot-tracks and how it draws a label. The band's own colours.
        chrome::dark_theme(hwnd, dark);
        self.dark = dark;
        let layout = layout;
        // `TBSTYLE_EX_DRAWDDARROWS` is what gives a `BTNS_DROPDOWN` button an arrow at all,
        // and the toolbar custom-draw handler reads it back before it agrees to paint one.
        let _ = SendMessageW(hwnd, TB_BUTTONSTRUCTSIZE, Some(WPARAM(32)), None);
        let _ = SendMessageW(
            hwnd,
            TB_SETEXTENDEDSTYLE,
            Some(WPARAM(0)),
            Some(LPARAM(TBSTYLE_EX_DRAWDDARROWS as isize)),
        );
        // `TBSTYLE_TRANSPARENT | TBSTYLE_FLAT`, plus `TBSTYLE_AUTOSIZE` for
        // the one-row bar and `TBSTYLE_WRAPABLE` for 多行显示 — the control
        // settles its wrap behavior when the bits land here, so a live
        // toggle rests on the same pair of writes.
        self.wrap = wrap;
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32
            | TBSTYLE_TRANSPARENT
            | TBSTYLE_FLAT
            | if wrap {
                TBSTYLE_WRAPABLE
            } else {
                TBSTYLE_AUTOSIZE
            };
        SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
        let _ = SendMessageW(hwnd, TB_SETPADDING, Some(WPARAM(0)), Some(LPARAM(0x20002)));
        // The scaled glyph square.
        let px = bitmap_px(icon_size, dpi).max(1) as usize;
        let _ = SendMessageW(
            hwnd,
            TB_SETBITMAPSIZE,
            Some(WPARAM(0)),
            Some(LPARAM((px | (px << 16)) as isize)),
        );
        let bm = px + 7;
        let _ = SendMessageW(
            hwnd,
            TB_SETBUTTONSIZE,
            Some(WPARAM(0)),
            Some(LPARAM((bm | (bm << 16)) as isize)),
        );
        // The image list, then the buttons: `TB_ADDBUTTONSW` reads the
        // array once, so the rows are built first.
        if let Some(himl) = build_image_list(icon_size, dpi) {
            let _ = SendMessageW(
                hwnd,
                TB_SETIMAGELIST,
                Some(WPARAM(0)),
                Some(LPARAM(himl.0 as isize)),
            );
            self.himl = himl;
        }
        // 皮肤那几行在这里合进来：`create` 是唯一一处「表进来、按钮出去」的地方，
        // 合在这里，启动、重建、改设置三条路就都一样了。
        self.refresh_script_rows();
        self.buttons = self.with_script(layout);
        self.apply_buttons();
        self.build_drop_menu();
        if !visible {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        self.orig = to_wndproc(SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            tb_proc as *const () as isize,
        ));
    }

    /// The bar the 输出窗口 pane carries, built as a child of the pane window itself. Same
    /// common control as the frame's bar, different contract: style `0x50000944`
    /// (`TBSTYLE_FLAT | TBSTYLE_TOOLTIPS | CCS_NODIVIDER | CCS_NORESIZE`), id `59392`, the
    /// nine-glyph strip of the pane's sprite, and twelve rows with no checked and no
    /// dropdown button — so no `TB_SETPADDING`, no `TBSTYLE_EX_DRAWDDARROWS` and no
    /// dropdown menu either. The creator's own order is kept: image list, buttons, then the
    /// sizes; `TBSTYLE_TRANSPARENT | TBSTYLE_AUTOSIZE` is ORed in after the buttons have
    /// landed, which is where the output pane creator puts it.
    pub(super) unsafe fn create_output(
        &mut self,
        parent: HWND,
        dpi: u32,
        visible: bool,
        dark: bool,
    ) {
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        });
        if self.alive() {
            return;
        }
        self.icon_size = OUT_CX;
        self.dpi = dpi;
        // The creator's call: no extended style, the handed-in style, a 100 x 100
        // window at the origin, and the control id passed as the menu handle —
        // the 100 x 100 is the creator's own seed size, which
        // `TBSTYLE_AUTOSIZE` and the pane's `WM_SIZE` pass replace.
        let Ok(hwnd) = CreateWindowExW(
            // `WS_EX_COMPOSITED`: the control paints its band, its separators
            // and every hovered cell straight to the window surface, and a
            // hover repaints the whole strip. Without a composed pass the
            // band fill and the buttons reach the screen as separate paints,
            // which is the strip's hover flash. The flag stages the pass
            // off-screen and shows one frame.
            WINDOW_EX_STYLE(WS_EX_COMPOSITED.0),
            TOOLBAR_CLASS,
            w!("Toolbar"),
            WINDOW_STYLE(0x50000944),
            0,
            0,
            100,
            100,
            Some(parent),
            Some(HMENU(OUT_CTRL_ID as usize as *mut core::ffi::c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.hwnd = hwnd;
        chrome::dark_theme(hwnd, dark);
        self.dark = dark;
        self.output_sheet = true;
        let _ = SendMessageW(hwnd, TB_BUTTONSTRUCTSIZE, Some(WPARAM(32)), None);
        if let Some(himl) = build_out_image_list(dpi) {
            let _ = SendMessageW(
                hwnd,
                TB_SETIMAGELIST,
                Some(WPARAM(0)),
                Some(LPARAM(himl.0 as isize)),
            );
            self.himl = himl;
        }
        self.buttons = output_rows();
        self.apply_buttons();
        // The glyph square is the header's `cx`; the height is its `cy` with the
        // control's font height as the floor, and the button that holds it is 7
        // px larger either way — the output bar builder's three last messages exactly.
        let cx = bitmap_px(OUT_CX, dpi).max(1);
        let cy = bitmap_px(OUT_CY, dpi).max(font_px(hwnd));
        let _ = SendMessageW(
            hwnd,
            TB_SETBITMAPSIZE,
            Some(WPARAM(0)),
            Some(LPARAM((cx | (cy << 16)) as isize)),
        );
        let _ = SendMessageW(
            hwnd,
            TB_SETBUTTONSIZE,
            Some(WPARAM(0)),
            Some(LPARAM(((cx + 7) | ((cy + 7) << 16)) as isize)),
        );
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32
            | TBSTYLE_TRANSPARENT
            | TBSTYLE_AUTOSIZE
            | 0x0100;
        let _ = SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
        if !visible {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        self.orig = to_wndproc(SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            tb_proc as *const () as isize,
        ));
    }

    /// The band the band measure reserves for the pane's bar: the control's own
    /// `TB_GETBUTTONSIZE` answer (its high word is the button height), plus
    /// the 2 px the band adds for the border. The
    /// main bar's `icon_size + 8` is the fallback for a control that will not
    /// say — the same number, 24 px at 96 DPI, so a failed ask costs no layout.
    pub(super) unsafe fn ideal_band_h(&self) -> f32 {
        if !self.alive() {
            return 0.0;
        }
        let v = SendMessageW(self.hwnd, TB_GETBUTTONSIZE, Some(WPARAM(0)), None).0 as u32;
        let h = (v >> 16) as i32;
        if h > 0 {
            (h + 2) as f32
        } else {
            band_h(self.icon_size, self.dpi)
        }
    }

    /// Releases what the control does not own when its pane is closed. The
    /// window itself is a child of the pane and goes with its parent; the image
    /// list is a separate GDI object that would outlive it.
    pub(super) unsafe fn release(&mut self) {
        if !self.himl.is_invalid() {
            let _ = ImageList_Destroy(Some(self.himl));
            self.himl = HIMAGELIST(0);
        }
        self.hwnd = HWND::default();
        self.orig = None;
        self.frame = Rect::new(0.0, 0.0, 0.0, 0.0);
        self.buttons.clear();
    }

    /// 只重画，不换图标表。热重载换了皮肤之后由
    /// [`super::window::EditorState::repaint_all_chrome`] 逐条点名。
    pub(super) unsafe fn invalidate(&self) {
        if self.alive() {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Swaps the bar's glyphs for the icon theme the frame just loaded: the
    /// image list is rebuilt from the sheet the theme resolves to and handed
    /// to the control. The old list is the bar's own — the control does not
    /// own it — so it is freed here, after the control has dropped it.
    pub(super) unsafe fn retheme(&mut self) {
        if !self.alive() {
            return;
        }
        let built = if self.output_sheet {
            build_out_image_list(self.dpi)
        } else {
            build_image_list(self.icon_size, self.dpi)
        };
        let Some(himl) = built else {
            return;
        };
        let _ = SendMessageW(
            self.hwnd,
            TB_SETIMAGELIST,
            Some(WPARAM(0)),
            Some(LPARAM(himl.0 as isize)),
        );
        let old = std::mem::replace(&mut self.himl, himl);
        if old.0 != 0 {
            let _ = ImageList_Destroy(Some(old));
        }
        let _ = InvalidateRect(Some(self.hwnd), None, true);
    }

    /// Moves the window to `frame` (frame-client coordinates).
    pub(super) unsafe fn place(&mut self, frame: Rect) {
        if !self.alive() || self.frame == frame {
            return;
        }
        self.frame = frame;
        let _ = SetWindowPos(
            self.hwnd,
            None,
            frame.x.round() as i32,
            frame.y.round() as i32,
            frame.w.round() as i32,
            frame.h.round() as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }

    /// Re-asks the control for the visual style its own label and hot-tracking
    /// use: that style is per-window state. Nothing in `custom_draw` depends
    /// on the answer — the band, the frame, the ink and the arrow are read
    /// from the theme on every paint.
    pub(super) unsafe fn set_dark(&mut self, dark: bool) {
        if !self.alive() || self.dark == dark {
            return;
        }
        self.dark = dark;
        chrome::dark_theme(self.hwnd, dark);
    }

    /// Hands the button table to the control.
    unsafe fn apply_buttons(&self) {
        let mut tb: Vec<TBBUTTON> = self
            .buttons
            .iter()
            .map(|row| TBBUTTON {
                iBitmap: row.bmp,
                idCommand: row.id as i32,
                fsState: row.state,
                fsStyle: row.style,
                bReserved: [0; 6],
                dwData: 0,
                iString: 0,
            })
            .collect();
        let _ = SendMessageW(
            self.hwnd,
            TB_ADDBUTTONSW,
            Some(WPARAM(tb.len())),
            Some(LPARAM(tb.as_mut_ptr() as isize)),
        );
    }

    /// The dropdown menu skeleton: one empty popup per button
    /// position, appended in order so the position *is* the submenu
    /// index.
    unsafe fn build_drop_menu(&mut self) {
        let Ok(menu) = CreateMenu() else {
            return;
        };
        for _ in 0..self.buttons.len() {
            let Ok(sub) = CreatePopupMenu() else {
                continue;
            };
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, PCWSTR::null());
        }
        self.drop_menu = menu;
    }

    /// Refills button `index`'s popup with `rows` and returns it. The
    /// handle is owned by `drop_menu`, not the caller.
    pub(super) unsafe fn popup_at(&mut self, index: i32, rows: &[DropdownRow]) -> Option<HMENU> {
        let sub = GetSubMenu(self.drop_menu, index);
        if sub.0.is_null() {
            return None;
        }
        // The same ownerdraw rows the frame's menus use: the toolbar's
        // dropdown is a popup like any other, and a native `MF_STRING` row
        // here would fall back to the system's own font and palette.
        let items: Vec<MenuItem> = rows
            .iter()
            .map(|row| {
                if row.separator {
                    MenuItem::separator()
                } else {
                    MenuItem::new(row.id, &row.label).with_enabled(row.enabled)
                }
            })
            .collect();
        if !crate::win32::menu::fill_popup(sub, &items) {
            return None;
        }
        Some(sub)
    }

    /// The position of the button carrying `command`. A `TBN_DROPDOWN` names
    /// the command, while the popup skeleton is addressed by position, so
    /// this is the step between the two.
    pub(super) fn command_index(&self, command: u32) -> Option<i32> {
        self.buttons
            .iter()
            .position(|row| row.id == command)
            .map(|i| i as i32)
    }

    /// The screen-space anchor for a dropdown button: the bottom-left corner of
    /// the button carrying `command`. `TB_GETRECT` names a button by command id,
    /// the same id the arrow painter hands it.
    pub(super) unsafe fn dropdown_point(&self, command: u32) -> Option<POINT> {
        let mut rc = RECT::default();
        let ok = SendMessageW(
            self.hwnd,
            TB_GETRECT,
            Some(WPARAM(command as usize)),
            Some(LPARAM(&mut rc as *mut RECT as isize)),
        );
        if ok.0 == 0 {
            return None;
        }
        let mut pts = [POINT {
            x: rc.left,
            y: rc.bottom,
        }];
        let _ = MapWindowPoints(Some(self.hwnd), None, &mut pts);
        Some(pts[0])
    }

    /// Hands the four tracked states back to a button table that was rebuilt
    /// from the array's own flags — the grayed 撤销 has to go gray again.
    unsafe fn reapply_states(&self) {
        for (cmd, value) in [57643u32, 57644, 57635, 57634].iter().zip(self.states.iter()) {
            self.set_button_enabled(*cmd, *value);
        }
    }

    /// `TB_ENABLEBUTTON` for one command. The control names the row by its
    /// command id, so a table the user reordered needs no index lookup.
    unsafe fn set_button_enabled(&self, cmd: u32, on: bool) {
        let _ = SendMessageW(
            self.hwnd,
            TB_ENABLEBUTTON,
            Some(WPARAM(cmd as usize)),
            Some(LPARAM(on as isize)),
        );
    }

    /// `TB_CHECKBUTTON` for one command: the pressed face a button wears
    /// while its toggle is on. The 自动换行 button is the one this build
    /// drives — its mark follows the document's wrap mode.
    pub(super) unsafe fn set_button_checked(&self, cmd: u32, on: bool) {
        let _ = SendMessageW(
            self.hwnd,
            TB_CHECKBUTTON,
            Some(WPARAM(cmd as usize)),
            Some(LPARAM(on as isize)),
        );
    }

    /// `TB_ENABLEBUTTON` is sent only when a state actually changed,
    /// so a caret move costs no message.
    pub(super) unsafe fn update_states(&mut self, can_undo: bool, can_redo: bool, has_sel: bool) {
        let values = [can_undo, can_redo, has_sel, has_sel];
        let commands = [57643u32, 57644, 57635, 57634];
        for (i, (cmd, value)) in commands.iter().zip(values.iter()).enumerate() {
            if *value == self.states[i] {
                continue;
            }
            self.states[i] = *value;
            self.set_button_enabled(*cmd, *value);
        }
    }

    /// The `WM_ERASEBKGND` answer: the band's theme fill and its 1 px
    /// bottom rule. Returning 1 tells the control the background is
    /// done — with `TBSTYLE_TRANSPARENT` set it would otherwise paint
    /// none at all
    unsafe fn erase(&self, hdc: HDC, theme: &Theme) {
        let mut rc = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut rc);
        let _ = FillRect(hdc, &rc, super::gdicache::brush(to_colorref(theme.toolbar_bg)));
        self.bottom_rule(hdc, theme);
    }

    /// The 1 px rule along the bottom of the band, `toolbar_border` on the
    /// client's last row. It is laid down with the band and again after the
    /// buttons, because a button that paints its whole cell — a disabled one,
    /// which takes the band colour for the face — covers this row.
    unsafe fn bottom_rule(&self, hdc: HDC, theme: &Theme) {
        let mut rc = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut rc);
        if rc.bottom <= rc.top + 1 {
            return;
        }
        let rule = RECT {
            top: rc.bottom - 1,
            ..rc
        };
        let border = super::gdicache::brush(to_colorref(theme.toolbar_border));
        let _ = FillRect(hdc, &rule, border);
    }

    /// One of the bar's metrics at the DPI the image list was built for —
    /// `MulDiv` by the DPI, which is what the dropdown arrow painter and the toolbar custom-draw handler
    /// call their scaling helper with (the field, falling back to 96).
    fn scale(&self, value: i32) -> i32 {
        mul_div(value, if self.dpi > 0 { self.dpi as i32 } else { 96 }, 96)
    }

    /// The row a custom-draw `dwItemSpec` names. The control puts the button's
    /// **command id** there — the same reason [`Self::is_dropdown`] can hand it to
    /// `TB_GETBUTTONINFOW`, which reads an id unless `TBIF_BYINDEX` says otherwise —
    /// so the lookup is by id, with the index kept as a fallback.
    fn row_for_spec(&self, spec: usize) -> Option<&TbRow> {
        self.buttons
            .iter()
            .find(|row| row.id as usize == spec)
            .or_else(|| self.buttons.get(spec))
    }

    /// 第 `index` 个按钮的悬停上下文和落点。
    ///
    /// `s:hint` 只读 `text`/`label`/`value` 三样，所以状态位给 0 —— 悬停时按没按
    /// 下、勾没勾是绘制档的事，提示不关心。`rect` 照样换成 DIP，和绘制档一个口径。
    unsafe fn hint_at(&self, index: usize) -> Option<(dg_script::PaintInput, POINT)> {
        let mut rc = RECT::default();
        if SendMessageW(
            self.hwnd,
            TB_GETRECT,
            Some(WPARAM(index)),
            Some(LPARAM(&mut rc as *mut RECT as isize)),
        )
        .0 == 0
        {
            return None;
        }
        let dpi = if self.dpi > 0 { self.dpi as i32 } else { 96 };
        let scale = dpi as f32 / 96.0;
        // 这里给的是**控件索引**（`TB_HITTEST` 的答案），所以直接按下标取行 ——
        // `row_for_spec` 先按 id 匹配，索引和某个 id 撞上就会拿错行。
        let id = self.buttons.get(index).map_or(index as u32, |row| row.id);
        let (short, long) = super::cmdtext::text_for(id);
        let size = (
            (rc.right - rc.left) as f32 / scale,
            (rc.bottom - rc.top) as f32 / scale,
        );
        let input = dg_script::PaintInput {
            rect: (0.0, 0.0, size.0, size.1),
            thumb: (0.0, 0.0, size.0, size.1),
            bar: (0.0, 0.0, 0.0, 0.0),
            scroll: 0.0,
            state: 0,
            part: crate::cmdids::name_for_id(id).unwrap_or("").to_string(),
            text: short.to_string(),
            label: long.to_string(),
            value: id.to_string(),
            dpi,
            index: (id & 0xFFFF) as u16,
        };
        Some((input, crate::win32::skinhint::under(rc.left, rc.bottom)))
    }

    /// Whether button `index` is the kind that gets an arrow: the extended
    /// style first, then the button's own `BTNS_DROPDOWN` bit — the two tests
    /// the toolbar custom-draw handler makes before it asks for the post-paint stage.
    unsafe fn is_dropdown(&self, index: usize) -> bool {
        let ex = SendMessageW(
            self.hwnd,
            TB_GETEXTENDEDSTYLE,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        )
        .0 as u32;
        if ex & TBSTYLE_EX_DRAWDDARROWS == 0 {
            return false;
        }
        let mut info = TBBUTTONINFOW {
            cbSize: std::mem::size_of::<TBBUTTONINFOW>() as u32,
            dwMask: TBIF_STYLE,
            ..Default::default()
        };
        let ok = SendMessageW(
            self.hwnd,
            TB_GETBUTTONINFOW,
            Some(WPARAM(index)),
            Some(LPARAM(&mut info as *mut TBBUTTONINFOW as isize)),
        );
        ok.0 >= 0 && info.fsStyle & BTNS_DROPDOWN as u8 != 0
    }

    /// the dropdown arrow painter: the arrow the control drew is painted out over the
    /// strip it occupies, and a solid ▼ of the band's own ink is laid down in
    /// its place — one 1 px-tall row per step, each a pixel narrower at both
    /// ends than the row above.
    unsafe fn paint_arrow(
        &self,
        hdc: HDC,
        index: usize,
        state: NMCUSTOMDRAW_DRAW_STATE_FLAGS,
        theme: &Theme,
    ) {
        let mut rect = RECT::default();
        if SendMessageW(
            self.hwnd,
            TB_GETRECT,
            Some(WPARAM(index)),
            Some(LPARAM(&mut rect as *mut RECT as isize)),
        )
        .0 == 0
        {
            return;
        }
        let gutter = arrow_gutter(rect, self.scale(ARROW_GUTTER));
        // The blank-out takes the face the button is actually sitting on, so it
        // is invisible in the hot and the checked state as well as the plain.
        dc_fill(
            hdc,
            &gutter,
            item_face(state, theme).unwrap_or(theme.toolbar_bg),
        );
        // 皮肤接管了这一格时，箭头格得由它自己再画一遍：控件在
        // `CDDS_ITEMPREPAINT` 之后把这块刷成自己的底色，只让出箭头那一小块
        // —— 不补这一笔，格子正中就露出一条内置配色的灰，边框也断在这里。
        // 先铺内置底色再让皮肤盖上去，是「皮肤没画这里就还是原样」的写法。
        let _ = self.paint_by_script_clipped(hdc, &rect, state, index, Some(&gutter));
        let half = self.scale(ARROW_HALF);
        let ink = item_ink(state, theme);
        for i in 0..=half {
            dc_fill(hdc, &arrow_row(gutter, half, i), ink);
        }
    }

    /// 让皮肤画这一格。`true` 表示底色和 1 px 边框它画了，内置那两笔让位。
    ///
    /// **只让出底色和边框，图标还是控件的**。那套精灵图在 image list 里（`self.himl`），
    /// `UiOp::Image` 要的是 `HBITMAP`，脚本拿不到 —— 硬要它画等于让它把图标也重画
    /// 一遍。所以这一区的语义是「每格的底子归你，glyph 归控件」，和 `CDRF_SKIPDEFAULT`
    /// 那条路（禁用态的灰图标）刻意不同。
    ///
    /// `ctx.part` 是命令名（`cm_file_save`），按它分支比按序号稳；`ctx.text` /
    /// `ctx.label` 是命令的短名和说明（`cmdtext` 那张表），想画文字标签的皮肤直接
    /// 用。分隔线的 `part`/`text` 都是空串，命令 id 是 0。
    unsafe fn paint_by_script(
        &self,
        hdc: HDC,
        rc: &RECT,
        state: NMCUSTOMDRAW_DRAW_STATE_FLAGS,
        spec: usize,
    ) -> bool {
        self.paint_by_script_clipped(hdc, rc, state, spec, None)
    }

    /// 同上，但只让 `clip`（客户区坐标）那一块留在画面上。下拉按钮的箭头格要
    /// 用这个：控件在 `CDDS_ITEMPREPAINT` 之后自己把那一块重刷一遍，只有在
    /// `CDDS_ITEMPOSTPAINT` 里再回放一次皮肤，格子才不会被截成两色。
    unsafe fn paint_by_script_clipped(
        &self,
        hdc: HDC,
        rc: &RECT,
        state: NMCUSTOMDRAW_DRAW_STATE_FLAGS,
        spec: usize,
        clip: Option<&RECT>,
    ) -> bool {
        const REGION: &str = SKIN_REGION;
        let Some(host) = &self.script else {
            return false;
        };
        if !host.has_paint(REGION) {
            return false;
        }
        let dpi = if self.dpi > 0 { self.dpi as i32 } else { 96 };
        let scale = dpi as f32 / 96.0;
        let id = self.row_for_spec(spec).map_or(spec as u32, |row| row.id);
        let (short, long) = super::cmdtext::text_for(id);
        // 皮肤按钮没有内置名字，`cmdids` 里查不到它：`ctx.part` 给声明里那个
        // 命令名，`ctx.text` / `ctx.label` 给提示 —— 皮肤想单独打扮自己那一格
        // 才有东西可分支。
        let scripted = id
            .checked_sub(TB_SCRIPT_BASE)
            .and_then(|index| self.script_button(index));
        let (part, short, long) = match scripted {
            Some(btn) => (btn.command.clone(), btn.tip.as_str(), btn.tip.as_str()),
            None => (
                crate::cmdids::name_for_id(id).unwrap_or("").to_string(),
                short,
                long,
            ),
        };
        let mut bits = 0u16;
        // comctl32 没有 `CDIS_PRESSED`：**按下就是 `CDIS_SELECTED`**，勾选是
        // `CDIS_CHECKED`。两者是不同位，一个按钮可以同时有（按下时保持勾选）。
        for (flag, bit) in [
            (CDIS_HOT, dg_skin::State::HOT),
            (CDIS_SELECTED, dg_skin::State::PRESSED),
            (CDIS_DISABLED, dg_skin::State::DISABLED),
            (CDIS_CHECKED, dg_skin::State::CHECKED),
            (CDIS_FOCUS, dg_skin::State::FOCUS),
        ] {
            if state.contains(flag) {
                bits |= bit.bits();
            }
        }
        // 带下拉箭头的按钮用 `alt` 标出来 —— 状态位里没有「有菜单」这一位。
        if self.is_dropdown(spec) {
            bits |= dg_skin::State::ALT.bits();
        }
        let input = dg_script::PaintInput {
            rect: (
                0.0,
                0.0,
                (rc.right - rc.left) as f32 / scale,
                (rc.bottom - rc.top) as f32 / scale,
            ),
            // 一格就是一个按钮，没有「里面会动的东西」。
            thumb: (
                0.0,
                0.0,
                (rc.right - rc.left) as f32 / scale,
                (rc.bottom - rc.top) as f32 / scale,
            ),
            bar: (0.0, 0.0, 0.0, 0.0),
            scroll: 0.0,
            state: bits,
            part,
            text: short.to_string(),
            label: long.to_string(),
            value: id.to_string(),
            dpi,
            // 缓存键的区分位：命令 id。42 个命令名哈希进 8 位会撞（实测撞出
            // 3 对），id 是唯一的；用 id 而不是按钮位置，是因为用户改工具栏
            // 顺序时 id 不变、位置会变。
            index: (id & 0xFFFF) as u16,
        };
        let Some(out) = host.paint(REGION, &input) else {
            return false;
        };
        if !out.handled {
            return false;
        }
        let ops = out.compacted();
        // 字体就是控件自己那只（`WM_GETFONT`），不用另建：这条工具栏是纯图标的，
        // 但皮肤想画文字标签时得有东西可选。
        let fonts = [self.font_obj()];
        let ctx = super::skinpaint::ReplayCtx {
            fonts: &fonts,
            images: &[],
            strings: &out.strings,
            points: &out.points,
            text: &input.text,
            label: &input.label,
            value: &input.value,
            dpi,
        };
        // op 的坐标是**格内**的 DIP：把视口原点挪到格子左上角，回放完再弹回来。
        // `SaveDC`/`RestoreDC` 连裁剪和视口一起还原，比手动设回原点稳。
        let saved = SaveDC(hdc);
        let _ = SetViewportOrgEx(hdc, rc.left, rc.top, None);
        if let Some(clip) = clip {
            // 裁剪矩形也是逻辑坐标，所以跟着视口一起挪进格内。
            let _ = IntersectClipRect(
                hdc,
                clip.left - rc.left,
                clip.top - rc.top,
                clip.right - rc.left,
                clip.bottom - rc.top,
            );
        }
        let _ = super::skinpaint::replay(hdc, ops.as_slice(), &ctx);
        let _ = RestoreDC(hdc, saved);
        true
    }

    /// 控件自己那只字体，没有就用系统 UI 字体。
    fn font_obj(&self) -> HFONT {
        let hf = unsafe { SendMessageW(self.hwnd, WM_GETFONT, Some(WPARAM(0)), None) };
        if hf.0 == 0 {
            HFONT(unsafe { GetStockObject(DEFAULT_GUI_FONT) }.0)
        } else {
            HFONT(hf.0 as *mut core::ffi::c_void)
        }
    }

    /// the toolbar custom-draw handler: the frame's answer to the bar's `NM_CUSTOMDRAW`. The
    /// control keeps the geometry, the hit-testing and the glyphs; the frame keeps every
    /// colour, which is the only route the band's ink has to the dropdown arrow — comctl32
    /// takes that glyph from the control's own visual style, and no per-window theming ask
    /// moves it into the dark palette.
    pub(super) unsafe fn custom_draw(&self, lparam: LPARAM, theme: &Theme) -> Option<LRESULT> {
        let _stats = super::paintstats::begin(super::paintstats::Region::Toolbar);
        let cd = lparam.0 as *mut NMTBCUSTOMDRAW;
        if cd.is_null() {
            return None;
        }
        let nmcd = &(*cd).nmcd;
        match nmcd.dwDrawStage {
            CDDS_PREPAINT => {
                self.erase(nmcd.hdc, theme);
                // The item pass is asked for per button, and once more for the
                // whole row: a button that fills its own cell can reach the
                // rule under the row, which is put back after the control.
                Some(LRESULT((CDRF_NOTIFYITEMDRAW | CDRF_NOTIFYPOSTPAINT) as isize))
            }
            CDDS_ITEMPREPAINT => {
                let (hdc, rc, state, spec) = (nmcd.hdc, nmcd.rc, nmcd.uItemState, nmcd.dwItemSpec);
                // 悬停位由我们自己跟（`tb_proc` 的 `WM_MOUSEMOVE`）：comctl32 在这条
                // 自定义主题的工具栏上不发 `CDIS_HOT`。分隔线的 `spec` 是 0，永远
                // 不算热 —— 多根分隔线共用一个 id，按 id 比会全亮。
                let state = if spec != 0 && self.command_index(spec as u32) == Some(self.hot) {
                    state | CDIS_HOT
                } else {
                    state
                };
                // 皮肤接管：底色和 1 px 边框归它。图标仍旧由控件 blit —— 见
                // `paint_by_script` 的说明。
                let scripted = self.paint_by_script(hdc, &rc, state, spec);
                // The button's face and its 1 px edge, both only where a state
                // has one: the toolbar custom-draw handler jumps past the pair for a plain
                // button, which is already covered by the band fill above and
                // gets no outline at all.
                let face = item_face(state, theme);
                if let Some(face) = face {
                    if !scripted {
                        dc_fill(hdc, &rc, face);
                        dc_frame(hdc, &rc, theme.toolbar_frame);
                    }
                }
                // A disabled button has to *look* disabled. The control blits its
                // glyph at full strength once the edges and background are declined
                // below, so the row nothing answers would read exactly like one that
                // does: gray the cell's own image through the same `DrawState` pass
                // the common controls use for a disabled icon.
                if state.contains(CDIS_DISABLED) {
                    if let Some(row) = self.row_for_spec(spec) {
                        let band = face.unwrap_or(theme.toolbar_bg);
                        // 皮肤画过就别再铺一遍 —— 这一笔会把它的底色擦掉；
                        // `band` 还留着当 `ILD_BLEND50` 的混合色。
                        if !scripted {
                            dc_fill(hdc, &rc, band);
                        }
                        let cell = self.scale(self.icon_size);
                        let x = rc.left + ((rc.right - rc.left - cell) / 2).max(0);
                        let y = rc.top + ((rc.bottom - rc.top - cell) / 2).max(0);
                        // `ILD_BLEND50` against the band's own colour is how the common
                        // controls gray a glyph: half the ink, half the face.
                        let _ = ImageList_DrawIndirect(&IMAGELISTDRAWPARAMS {
                            cbSize: std::mem::size_of::<IMAGELISTDRAWPARAMS>() as u32,
                            himl: self.himl,
                            i: row.bmp,
                            hdcDst: hdc,
                            x,
                            y,
                            rgbBk: to_colorref(band),
                            rgbFg: to_colorref(theme.toolbar_fg),
                            fStyle: ILD_BLEND50.0,
                            ..Default::default()
                        });
                    }
                    return Some(LRESULT(CDRF_SKIPDEFAULT as isize));
                }
                (*cd).clrText = to_colorref(item_ink(state, theme));
                // `TBCDRF_NOEDGES | TBCDRF_NOBACKGROUND | TBCDRF_USECDCOLORS`,
                // which is `0xC1000 << 4` — the value the toolbar custom-draw handler returns —
                // with `CDRF_NOTIFYPOSTPAINT` (that same `1` bit, shifted into
                // place by the same `<< 4`) for the buttons that have an arrow
                // to repaint.
                let mut flags =
                    (TBCDRF_NOEDGES | TBCDRF_NOBACKGROUND | TBCDRF_USECDCOLORS) as isize;
                if self.is_dropdown(spec) {
                    flags |= CDRF_NOTIFYPOSTPAINT as isize;
                }
                Some(LRESULT(flags))
            }
            CDDS_ITEMPOSTPAINT => {
                let (hdc, state, spec) = (nmcd.hdc, nmcd.uItemState, nmcd.dwItemSpec);
                self.paint_arrow(hdc, spec, state, theme);
                Some(LRESULT(CDRF_DODEFAULT as isize))
            }
            CDDS_POSTPAINT => {
                self.bottom_rule(nmcd.hdc, theme);
                Some(LRESULT(CDRF_DODEFAULT as isize))
            }
            _ => None,
        }
    }
}

impl Default for ToolbarWin {
    fn default() -> Self {
        Self::new()
    }
}

/// `MulDiv` as gdi32 computes it — the product, the half-denominator rounding
/// term, then the quotient. This is the helper for the metrics hard-coded in
/// the paint code; `windows` 0.61 only exports the API behind a feature this
/// backend does not enable, and [`crate::win32::gdi`] already hand-rolls the
/// same arithmetic.
fn mul_div(value: i32, numerator: i32, denominator: i32) -> i32 {
    let product = value as i64 * numerator as i64 + (denominator / 2) as i64;
    (product / denominator as i64) as i32
}

/// The strip the control's own arrow lives in: the button inset by a pixel on
/// every side, its left edge pulled in to `width` from the right, and never
/// further left than the uninset button. Both the blank-out and the triangle's
/// centre are taken from this.
fn arrow_gutter(rect: RECT, width: i32) -> RECT {
    let inset = RECT {
        left: rect.left + 1,
        top: rect.top + 1,
        right: rect.right - 1,
        bottom: rect.bottom - 1,
    };
    RECT {
        left: (inset.right - width).max(rect.left),
        ..inset
    }
}

/// Row `i` of the ▼ drawn in `gutter` with half-width `half`: one pixel tall,
/// `2 * half + 1` wide at the top and one pixel narrower at each end as it
/// descends.
fn arrow_row(gutter: RECT, half: i32, i: i32) -> RECT {
    let mid_x = (gutter.left + gutter.right) / 2;
    let mid_y = (gutter.top + gutter.bottom) / 2;
    let top = mid_y - half / 2;
    RECT {
        left: mid_x - half + i,
        top: top + i,
        right: mid_x + half + 1 - i,
        bottom: top + i + 1,
    }
}

/// The button's face in this state. The band fill already covered it.
fn item_face(state: NMCUSTOMDRAW_DRAW_STATE_FLAGS, theme: &Theme) -> Option<Color> {
    // `state & 9`, either bit counting: the checked button and the pressed
    // one both take the selected fill
    if (state & (CDIS_SELECTED | CDIS_CHECKED)).0 != 0 {
        Some(theme.toolbar_selected)
    } else if state.contains(CDIS_HOT) {
        Some(theme.toolbar_hot)
    } else {
        None
    }
}

/// The ink, for the control's own text and for the arrow: the disabled colour
/// only when the button is disabled.
fn item_ink(state: NMCUSTOMDRAW_DRAW_STATE_FLAGS, theme: &Theme) -> Color {
    if state.contains(CDIS_DISABLED) {
        theme.toolbar_disabled
    } else {
        theme.toolbar_fg
    }
}

/// `FillRect` with a theme colour, through the DC brush — the set, use, restore
/// trio the dropdown arrow painter performs around every one of its fills.
unsafe fn dc_fill(hdc: HDC, rc: &RECT, color: Color) {
    let prev = SetDCBrushColor(hdc, to_colorref(color));
    let _ = FillRect(hdc, rc, HBRUSH(GetStockObject(DC_BRUSH).0));
    let _ = SetDCBrushColor(hdc, prev);
}

/// The same for `FrameRect`, which is how the 1 px button edge is drawn.
unsafe fn dc_frame(hdc: HDC, rc: &RECT, color: Color) {
    let prev = SetDCBrushColor(hdc, to_colorref(color));
    let _ = FrameRect(hdc, rc, HBRUSH(GetStockObject(DC_BRUSH).0));
    let _ = SetDCBrushColor(hdc, prev);
}

/// Hands a message to the control's own procedure. `orig` is empty
/// only if the subclass was never installed, in which case the
/// default procedure is the closest thing to it.
unsafe fn call_orig(
    orig: WNDPROC,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match orig {
        Some(p) => p(hwnd, msg, wparam, lparam),
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn to_wndproc(v: isize) -> WNDPROC {
    if v == 0 {
        None
    } else {
        // A window procedure is a code address: the same width as
        // `isize` on every platform this backend builds for.
        Some(unsafe { std::mem::transmute_copy(&v) })
    }
}

/// Tracks `menu` at the screen point and returns the chosen row id.
/// `TPM_RETURNCMD` makes the return value the id, so no `WM_COMMAND`
/// round-trip is needed; the caller dispatches it through the frame's
/// existing command path.
pub(super) unsafe fn track(menu: HMENU, at: POINT, owner: HWND) -> Option<u32> {
    // The owner is not necessarily foreground when the notification
    // arrives, and `TrackPopupMenu` documents odd dismissal behaviour
    // for a non-foreground owner — the same workaround the frame's own
    // menu tracking applies.
    let _ = SetForegroundWindow(owner);
    let chosen = TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_LEFTBUTTON | TPM_RETURNCMD,
        at.x,
        at.y,
        None,
        owner,
        None,
    )
    .0;
    (chosen > 0).then_some(chosen as u32)
}

/// The subclass. The erase is the one message answered here. Two bars share it:
/// the frame's, whose parent is the frame, and the 输出窗口's, whose parent is the
/// pane window one level below it.
unsafe extern "system" fn tb_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // The control's `GWLP_USERDATA` belongs to comctl32, so the
    // state is reached through its parent — the frame window, or the
    // frame's child's parent for the pane's own bar.
    let parent = match GetParent(hwnd) {
        Ok(p) if !p.0.is_null() => p,
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    let frame = if state_mut(parent).is_some() {
        parent
    } else {
        match GetParent(parent) {
            Ok(f) if !f.0.is_null() => f,
            _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    };
    let Some(st) = state_mut(frame) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    // Copied out before any re-entrant dispatch: the dropdown
    // path re-enters this procedure while the borrow it made is
    // still live.
    let pane = st.panels.outputs.iter().position(|w| w.tb.hwnd() == hwnd);
    let orig = if let Some(i) = pane {
        st.panels.outputs[i].tb.orig
    } else {
        st.panels.toolbar_win.orig
    };
    match msg {
        WM_CONTEXTMENU => {
            // Only the frame's own bar carries the bar menu; a pane bar
            // keeps the control's default handling.
            if pane.is_some() {
                return call_orig(orig, hwnd, msg, wparam, lparam);
            }
            let screen = if lparam.0 as isize == -1 {
                // A keyboard invocation has no cursor to point at; the menu
                // opens at the bar's lower-left corner instead.
                let mut rc = RECT::default();
                let _ = GetWindowRect(hwnd, &mut rc);
                POINT {
                    x: rc.left,
                    y: rc.bottom,
                }
            } else {
                POINT {
                    x: low_word(lparam) as i16 as i32,
                    y: high_word(lparam) as i16 as i32,
                }
            };
            show_context_menu(st, frame, hwnd, screen);
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            if let Some(i) = pane {
                st.panels.outputs[i].tb.erase(hdc, &st.docs.act().theme);
            } else {
                st.panels.toolbar_win.erase(hdc, &st.docs.act().theme);
            }
            LRESULT(1)
        }
        WM_MOUSEMOVE => {
            // 这条工具栏**不会自己发 `CDIS_HOT`**：`chrome::dark_theme` 给它起的是
            // 自定义主题名，comctl32 的热跟踪于是整条不启用 —— 内置画法里
            // `item_face` 的 `toolbar_hot` 那一支因此一直是死代码（实测：真实光标
            // 停在按钮上，整窗 0 像素差；`TB_GETHOTITEM` 已经是 0，控件自己也不画）。
            // 热跟踪自己来：命中测试记下索引，`CDDS_ITEMPREPAINT` 里补一位。
            let mut pt = POINT {
                x: low_word(lparam) as i16 as i32,
                y: high_word(lparam) as i16 as i32,
            };
            let hit = SendMessageW(
                hwnd,
                TB_HITTEST,
                Some(WPARAM(0)),
                Some(LPARAM(&mut pt as *mut POINT as isize)),
            )
            .0 as i32;
            let changed = {
                // 借只在这一段里：`call_orig` 会重入窗口过程，那时 `st` 不能还借着。
                let bar = match pane {
                    Some(i) => &mut st.panels.outputs[i].tb,
                    None => &mut st.panels.toolbar_win,
                };
                let changed = bar.hot != hit;
                if changed {
                    bar.hot = hit;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                changed
            };
            if changed {
                // 换格了才问皮肤：同一格里每秒几十条移动什么都不做。
                let host = st.lua.clone();
                match pane {
                    Some(i) => {
                        let bar = &mut st.panels.outputs[i].tb;
                        match (hit >= 0).then(|| bar.hint_at(hit as usize)).flatten() {
                            Some((input, anchor)) => bar.hint.retarget(
                                hwnd,
                                frame,
                                host.as_deref(),
                                SKIN_REGION,
                                &input,
                                anchor,
                                &mut st.tip_host,
                            ),
                            None => bar.hint.leave(&mut st.tip_host),
                        }
                    }
                    None => {
                        let bar = &mut st.panels.toolbar_win;
                        match (hit >= 0).then(|| bar.hint_at(hit as usize)).flatten() {
                            Some((input, anchor)) => bar.hint.retarget(
                                hwnd,
                                frame,
                                host.as_deref(),
                                SKIN_REGION,
                                &input,
                                anchor,
                                &mut st.tip_host,
                            ),
                            None => bar.hint.leave(&mut st.tip_host),
                        }
                    }
                }
            }
            // 离开要清掉：控件自己的热跟踪既然没开，`WM_MOUSELEAVE` 也不会自己来。
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);
            call_orig(orig, hwnd, msg, wparam, lparam)
        }
        WM_MOUSELEAVE => {
            {
                let bar = match pane {
                    Some(i) => &mut st.panels.outputs[i].tb,
                    None => &mut st.panels.toolbar_win,
                };
                if bar.hot != -1 {
                    bar.hot = -1;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                bar.hint.leave(&mut st.tip_host);
            }
            call_orig(orig, hwnd, msg, wparam, lparam)
        }
        // 皮肤声明了 `cursor` 就由它说了算。没声明时照旧交给控件。
        WM_SETCURSOR if low_word(lparam) == HTCLIENT as i32 => {
            let applied = match pane {
                Some(i) => st.panels.outputs[i].tb.hint.apply(),
                None => st.panels.toolbar_win.hint.apply(),
            };
            if applied {
                LRESULT(1)
            } else {
                call_orig(orig, hwnd, msg, wparam, lparam)
            }
        }
        _ => call_orig(orig, hwnd, msg, wparam, lparam),
    }
}

/// The default 42-button table as rows.
pub(super) fn default_layout() -> Vec<TbRow> {
    DEFAULT_LAYOUT
        .iter()
        .map(|&(bmp, id, state, style)| TbRow {
            bmp,
            id,
            state: gated(id, state),
            style,
        })
        .collect()
}

/// Whether a row may look clickable: the id names a command this build answers, or it
/// is a button that opens a menu instead. Nothing else in the bar has an answer, and a
/// button is not a menu row — the control swallows a click on a disabled one, so
/// greying here is what stops a dead command from looking like a broken click.
pub(super) fn row_is_live(id: u32) -> bool {
    crate::cmdids::is_answerable(id) || super::window::MENU_BUTTON_IDS.contains(&id)
}

/// `state` with the enabled bit taken back from a row nothing answers. The other bits
/// — the check mark a dropdown row carries — stay as the table has them.
fn gated(id: u32, state: u8) -> u8 {
    if row_is_live(id) || id == 0 {
        state
    } else {
        state & !TBSTATE_ENABLED
    }
}

/// The row a [ToolBar] command id names: a separator for id 0, otherwise the palette
/// image and the checked state that command's own row carries. An id this implementation
/// cannot name is None, which the caller skips. The available rows of the 工具条 options
/// page.
pub(super) fn palette_ids() -> Vec<u32> {
    PALETTE.iter().map(|(id, _)| *id).collect()
}

/// The strip cell a command's button draws — what the 工具条 page needs to
/// show the same glyph in its lists.
pub(super) fn image_of(id: u32) -> Option<i32> {
    PALETTE
        .iter()
        .find(|(cmd, _)| *cmd == id)
        .map(|&(_, cell)| cell)
}

/// The `[ToolBar]` value the 工具条 page writes: each command id as the
/// little-endian dword the file stores, spelled out in byte order, so `57600`
/// becomes `00E10000`.
pub(super) fn ids_to_hex(ids: &[u32]) -> String {
    let mut out = String::with_capacity(ids.len() * 8);
    for id in ids {
        out.push_str(&format!("{:08X}", id.swap_bytes()));
    }
    out
}

/// The command ids a `[ToolBar]` value names, read back through
/// [`ids_to_hex`]'s rule. An incomplete tail block is dropped rather than
/// fatal, and an id nothing in the palette answers is skipped by the caller.
pub(super) fn ids_from_hex(hex: &str, count: usize) -> Vec<u32> {
    let digits: Vec<char> = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let mut ids = Vec::new();
    for i in 0..count {
        let start = i * 8;
        if start + 8 > digits.len() {
            break;
        }
        let word: String = digits[start..start + 8].iter().collect();
        if let Ok(word) = u32::from_str_radix(&word, 16) {
            ids.push(word.swap_bytes());
        }
    }
    ids
}

pub(super) fn row_for_command(id: u32) -> Option<TbRow> {
    if id == 0 {
        return Some(TbRow {
            bmp: 8,
            id: 0,
            state: TBSTATE_ENABLED,
            style: BTNS_SEP as u8,
        });
    }
    let bmp = PALETTE
        .iter()
        .find(|(cmd, _)| *cmd == id)
        .map(|(_, bmp)| *bmp)?;
    let state = if id == 1320 {
        TBSTATE_ENABLED | TBSTATE_CHECKED
    } else {
        TBSTATE_ENABLED
    };
    let style = if WHOLE_DROPDOWN.contains(&id) {
        BTNS_DROPDOWN as u8
    } else {
        0
    };
    Some(TbRow {
        bmp,
        id,
        state: gated(id, state),
        style,
    })
}
/// The strip is loaded at `icon_size * scale`, so the 16 px glyphs become a
/// 20 px image list on a 120 DPI display and the band grows with them.
pub(super) fn dpi_scale(dpi: u32) -> f32 {
    if dpi >= 288 {
        3.0
    } else if dpi >= 240 {
        2.5
    } else if dpi >= 216 {
        2.25
    } else if dpi >= 192 {
        2.0
    } else if dpi >= 168 {
        1.75
    } else if dpi >= 144 {
        1.5
    } else if dpi >= 120 {
        1.25
    } else {
        1.0
    }
}

/// The bitmap square the image list is built at — `(int)(icon_size *
/// scale)`
pub(super) fn bitmap_px(icon_size: i32, dpi: u32) -> i32 {
    (icon_size as f32 * dpi_scale(dpi)) as i32
}

/// Glyphs per strip: the 112 image indices the button table names.
const STRIP_ICONS: i32 = 112;

/// The sheet files an icon theme folder may carry, named as the built-in
/// assets are. A theme is a folder holding any of these — a complete set
/// replaces every bar's glyphs at once.
pub(super) const THEME_SHEETS: &[&str] = &[
    "toolbar16.bmp",
    "toolbar24.bmp",
    "toolbar32.bmp",
    "tabstrip16.bmp",
    "output16.bmp",
];

static ICON_THEME: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Records the icon theme folder the frame loaded (`[Setting] IconTheme`).
/// Answers whether the name changed, so a swap can be skipped when the
/// options page closed without a new pick.
pub(super) fn set_icon_theme(name: Option<&str>) -> bool {
    let mut slot = ICON_THEME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let next = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    if *slot == next {
        return false;
    }
    *slot = next;
    true
}

fn icon_theme() -> Option<String> {
    ICON_THEME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Which of the two built-in ink palettes the bars draw from. Both sheet sets
/// carry the same glyphs in the same order; only the ink differs. A loaded
/// icon theme still takes precedence sheet by sheet.
static DARK_ICONS: AtomicBool = AtomicBool::new(true);

/// The built-in palette the strips resolve to right now.
pub(super) fn dark_icons() -> bool {
    DARK_ICONS.load(Ordering::Acquire)
}

/// Whether the bar's own face is dark. The editor background is not a proxy:
/// a theme can give the text area one palette and the chrome another, and the
/// ink has to contrast with the face it is actually drawn on.
fn palette_is_dark(theme: &Theme) -> bool {
    let c = theme.toolbar_bg;
    let luma = (u32::from(c.r) * 299 + u32::from(c.g) * 587 + u32::from(c.b) * 114) / 1000;
    luma < 128
}

/// Picks the ink that suits the face the bar is about to paint and answers
/// whether the choice changed. The caller rebuilds the image lists only when
/// it did; at boot the bars have not been created yet and read the flag on
/// their own.
pub(super) fn sync_palette(theme: &Theme) -> bool {
    let dark = palette_is_dark(theme);
    DARK_ICONS.swap(dark, Ordering::AcqRel) != dark
}

/// The folders an icon theme can live in, in lookup order: `iconthemes\`
/// beside the executable (a portable install keeps its add-ons next to it),
/// then `%APPDATA%\dogedit\iconthemes`, the writable location when the
/// install sits under Program Files.
pub(super) fn theme_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.join("iconthemes"));
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push(
            std::path::PathBuf::from(appdata)
                .join("dogedit")
                .join("iconthemes"),
        );
    }
    roots
}

/// The loaded theme's copy of `file`, when the name resolves to a folder
/// carrying a sheet of the right geometry — `cells` cells of `cell` px,
/// 32 bpp. A missing folder or file and a wrong-shaped sheet all fall
/// through, so a half-finished theme keeps the built-in glyphs sheet by
/// sheet.
pub(super) fn theme_sheet(file: &str, cell: i32, cells: i32) -> Option<Vec<u8>> {
    theme_sheet_in(&theme_roots(), icon_theme().as_deref(), file, cell, cells)
}

fn theme_sheet_in(
    roots: &[std::path::PathBuf],
    theme: Option<&str>,
    file: &str,
    cell: i32,
    cells: i32,
) -> Option<Vec<u8>> {
    let theme = theme?;
    // A name that walks out of the theme folder is not a theme name.
    if theme.contains('/') || theme.contains('\\') || theme == ".." {
        return None;
    }
    for root in roots {
        let Ok(bytes) = std::fs::read(root.join(theme).join(file)) else {
            continue;
        };
        let Some(strip) = decode_strip(&bytes) else {
            continue;
        };
        if strip.w == cell * cells && strip.h == cell {
            return Some(bytes);
        }
    }
    None
}

/// The theme folder names the roots carry that look usable: a directory
/// holding at least one sheet file. Sorted, so the options list is stable
/// across scans.
pub(super) fn theme_names(roots: &[std::path::PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for root in roots {
        let Ok(read) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !THEME_SHEETS.iter().any(|file| path.join(file).is_file()) {
                continue;
            }
            if !names.iter().any(|seen| seen.eq_ignore_ascii_case(&name)) {
                names.push(name);
            }
        }
    }
    names.sort_by_key(|name| name.to_lowercase());
    names
}

/// The built-in toolbar sheets. Taking `dark` as an argument rather than
/// reading the global keeps the pair testable without disturbing the flag
/// other bars are reading at the same time.
fn builtin_strip(icon_size: i32, dark: bool) -> Option<&'static [u8]> {
    Some(match (dark, icon_size) {
        (false, 16) => include_bytes!("../../assets/sprites/toolbar16.bmp"),
        (false, 24) => include_bytes!("../../assets/sprites/toolbar24.bmp"),
        (false, 32) => include_bytes!("../../assets/sprites/toolbar32.bmp"),
        (true, 16) => include_bytes!("../../assets/sprites/dark/toolbar16.bmp"),
        (true, 24) => include_bytes!("../../assets/sprites/dark/toolbar24.bmp"),
        (true, 32) => include_bytes!("../../assets/sprites/dark/toolbar32.bmp"),
        _ => return None,
    })
}

/// The bar's sprite strip. The toolbar image load reads a strip from the
/// install directory; this implementation ships its own, composed from the
/// committed icon atlas so the bar looks the same wherever it runs. The ink
/// follows the toolbar face, and an icon theme's own `toolbar<size>.bmp`
/// takes its place when one is loaded.
fn strip_bytes(icon_size: i32) -> Option<Vec<u8>> {
    if let Some(bytes) = theme_sheet(
        &format!("toolbar{}.bmp", icon_size),
        icon_size,
        STRIP_ICONS,
    ) {
        return Some(bytes);
    }
    builtin_strip(icon_size, dark_icons()).map(<[u8]>::to_vec)
}

/// A decoded strip: one `w x h` block of BGRA, `h` rows of
/// [`STRIP_ICONS`] glyphs side by side.
pub(super) struct Strip {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

/// The alpha channel is the strip's own — these bitmaps carry no colour
/// key.
pub(super) fn decode_strip(bmp: &[u8]) -> Option<Strip> {
    let head = bmp.get(0..54)?;
    if &head[0..2] != b"BM" || u32::from_le_bytes(head[14..18].try_into().ok()?) != 40 {
        return None;
    }
    let off = i32::from_le_bytes(head[10..14].try_into().ok()?) as usize;
    let w = i32::from_le_bytes(head[18..22].try_into().ok()?);
    let h = i32::from_le_bytes(head[22..26].try_into().ok()?);
    if u16::from_le_bytes(head[28..30].try_into().ok()?) != 32
        || i32::from_le_bytes(head[30..34].try_into().ok()?) != 0
        || w <= 0
        || h == 0
    {
        return None;
    }
    let rows = h.unsigned_abs() as usize;
    let stride = w as usize * 4;
    let len = rows.checked_mul(stride)?;
    if off.checked_add(len).is_none() || off + len > bmp.len() {
        return None;
    }
    let mut px = Vec::with_capacity(len);
    for y in 0..rows {
        let src = if h < 0 { y } else { rows - 1 - y };
        px.extend_from_slice(&bmp[off + src * stride..off + (src + 1) * stride]);
    }
    Some(Strip {
        w,
        h: rows as i32,
        px,
    })
}

/// The 32 bpp path of the strip scaler: WIC's bitmap scaler with linear
/// interpolation, which is what turns the 16 px strip into the 20 px one
/// a 120 DPI bar shows.
pub(super) unsafe fn scale_strip(px: &[u8], w: i32, h: i32, dw: i32, dh: i32) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory,
        WICBitmapInterpolationModeLinear,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    let factory: IWICImagingFactory =
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
    let src = factory
        .CreateBitmapFromMemory(
            w as u32,
            h as u32,
            &GUID_WICPixelFormat32bppBGRA,
            (w * 4) as u32,
            px,
        )
        .ok()?;
    let scaler = factory.CreateBitmapScaler().ok()?;
    scaler
        .Initialize(&src, dw as u32, dh as u32, WICBitmapInterpolationModeLinear)
        .ok()?;
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    scaler
        .CopyPixels(std::ptr::null(), (dw * 4) as u32, &mut out)
        .ok()?;
    Some(out)
}

/// The strip at the size this DPI asks for: the file's own pixels at 96, a WIC
/// linear scale of the whole strip above it — the two branches of
/// the image-list fill.
pub(super) fn strip_at_dpi(strip: Strip, dpi: u32) -> Option<(i32, i32, Vec<u8>)> {
    let scale = dpi_scale(dpi);
    if scale == 1.0 {
        return Some((strip.w, strip.h, strip.px));
    }
    let dw = (strip.w as f32 * scale) as i32;
    let dh = (strip.h as f32 * scale) as i32;
    Some((dw, dh, unsafe {
        scale_strip(&strip.px, strip.w, strip.h, dw, dh)?
    }))
}

/// `cell` square cells, `count` of them, filled from one `w x h` BGRA strip in
/// a single `ImageList_Add` — the image-list fill's `ImageList_Create` +
/// `ImageList_Add` pair, with no mask because an `ILC_COLOR32` list takes its
/// transparency from the alpha channel.
pub(super) unsafe fn image_list(
    px: &[u8],
    w: i32,
    h: i32,
    cell: i32,
    count: i32,
) -> Option<HIMAGELIST> {
    let himl = ImageList_Create(cell, cell, ILC_COLOR32, count, 8);
    if himl.0 == 0 {
        return None;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative: the buffer is already top-down.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let screen = GetDC(None);
    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let bmp = CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0);
    let _ = ReleaseDC(None, screen);
    let Ok(bmp) = bmp else {
        let _ = ImageList_Destroy(Some(himl));
        return None;
    };
    if bits.is_null() {
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = ImageList_Destroy(Some(himl));
        return None;
    }
    std::ptr::copy_nonoverlapping(
        px.as_ptr(),
        bits as *mut u8,
        px.len().min(w as usize * h as usize * 4),
    );
    // No mask: the list is 32 bpp, so the alpha channel is the
    // transparency and no colour key is consulted.
    let added = ImageList_Add(himl, bmp, None);
    let _ = DeleteObject(HGDIOBJ(bmp.0));
    // `ImageList_Add` answers with the index of the first image it took,
    // not the number it took, so the count has to be asked for
    // separately - the whole strip is only worth keeping if all
    // `count` cells landed in it.
    if added < 0 || ImageList_GetImageCount(himl) < count {
        let _ = ImageList_Destroy(Some(himl));
        return None;
    }
    Some(himl)
}

/// The glyph list the 工具条 options page fills its rows from: the same
/// sheet the bar reads, at the 16 px square, so a row shows exactly the
/// button the user would get — the loaded icon theme included.
pub(super) unsafe fn dialog_icon_list(dpi: u32) -> Option<(HIMAGELIST, i32)> {
    let cell = bitmap_px(16, dpi).max(1);
    Some((build_image_list(16, dpi)?, cell))
}

/// The image list: one `side x side` cell per glyph, all
/// [`STRIP_ICONS`] of them added in a single `ImageList_Add` of the
/// whole strip
/// table names are therefore the strip's own.
unsafe fn build_image_list(icon_size: i32, dpi: u32) -> Option<HIMAGELIST> {
    let (w, h, px) = strip_at_dpi(decode_strip(&strip_bytes(icon_size)?)?, dpi)?;
    image_list(&px, w, h, bitmap_px(icon_size, dpi).max(1), STRIP_ICONS)
}

/// The bar's right-click menu row ids. They are private to this popup: the
/// bar acts on them itself instead of routing them through the command
/// table.
const CM_HIDE: u32 = 1;
const CM_SIZE_16: u32 = 2;
const CM_SIZE_24: u32 = 3;
const CM_SIZE_32: u32 = 4;
const CM_TOP: u32 = 5;
const CM_BOTTOM: u32 = 6;
const CM_MERGE: u32 = 7;
const CM_WRAP: u32 = 8;
const CM_CUSTOMIZE: u32 = 9;

/// The rows the bar's context menu carries: hide, the three glyph squares,
/// the band's side of the tab row, merging and wrapping, and the 工具条
/// options page. Merging needs a second bar to merge with and this build
/// has only the main one, so that row stays greyed.
fn context_menu_items(size: i32, top: bool, wrap: bool) -> Vec<MenuItem> {
    vec![
        MenuItem::new(CM_HIDE, "隐藏"),
        MenuItem::separator(),
        MenuItem::new(CM_SIZE_16, "小图标(16*16)").with_checked(size == 16),
        MenuItem::new(CM_SIZE_24, "中图标(24*24)").with_checked(size == 24),
        MenuItem::new(CM_SIZE_32, "大图标(32*32)").with_checked(size == 32),
        MenuItem::separator(),
        MenuItem::new(CM_TOP, "上").with_checked(top),
        MenuItem::new(CM_BOTTOM, "下").with_checked(!top),
        MenuItem::separator(),
        MenuItem::new(CM_MERGE, "合并工具条").with_enabled(false),
        MenuItem::new(CM_WRAP, "多行显示").with_checked(wrap),
        MenuItem::separator(),
        MenuItem::new(CM_CUSTOMIZE, "自定义..."),
    ]
}

/// Shows the bar's context menu at `screen` and runs the chosen row. The
/// popup is the frame's own themed menu builder, so it paints like every
/// other menu instead of falling back to the system palette.
unsafe fn show_context_menu(st: &mut EditorState, frame: HWND, hwnd: HWND, screen: POINT) {
    let items = context_menu_items(
        st.panels.toolbar_win.icon_size(),
        st.toolbar_menu_top(),
        st.toolbar_menu_wrap(),
    );
    let mut anchor = screen;
    let _ = ScreenToClient(hwnd, &mut anchor);
    let Some(choice) = crate::win32::menu::track(hwnd, &items, anchor, st.menu_brush()) else {
        return;
    };
    match choice {
        CM_HIDE => st.toggle_toolbar(frame),
        CM_SIZE_16 => st.set_toolbar_icon_size(frame, 16),
        CM_SIZE_24 => st.set_toolbar_icon_size(frame, 24),
        CM_SIZE_32 => st.set_toolbar_icon_size(frame, 32),
        CM_TOP => st.set_toolbar_top(frame, true),
        CM_BOTTOM => st.set_toolbar_top(frame, false),
        CM_WRAP => st.set_toolbar_wrap(frame, !st.toolbar_menu_wrap()),
        CM_CUSTOMIZE => st.show_options(frame, Some(207)),
        _ => {}
    }
}

/// The 输出窗口 bar's control id — `59392`, the value the output pane creator hands
/// the output bar builder as the window id, which is the same number the frame's own
/// bar carries.
pub(super) const OUT_CTRL_ID: u32 = 59392;
/// The pane strip header's words 1 and 2: the glyph square and the floor for the
/// button height the control's own font can raise.
const OUT_CX: i32 = 16;
const OUT_CY: i32 = 15;
/// The width an entry of `0` gives its separator: the output bar builder writes `8`
/// into `iBitmap` for a `BTNS_SEP` row, and a separator reads that as pixels.
const OUT_SEP_W: i32 = 8;
/// The nine glyphs the pane's strip carries — `144 / 16`, and the count
/// the output bar builder ends its walk of the table with.
const OUT_STRIP_ICONS: i32 = 9;

/// The pane's sprite, the image that shares its number with the pane's button
/// table (144 x 16, 32 bpp). This implementation ships its own sheet in
/// `assets/sprites/output16.bmp` and a second ink in `sprites/dark/`; an
/// icon theme's `output16.bmp` takes their place when one is loaded.
fn out_strip_bytes() -> Option<Vec<u8>> {
    if let Some(bytes) = theme_sheet("output16.bmp", OUT_CX, OUT_STRIP_ICONS) {
        return Some(bytes);
    }
    let sheet: &'static [u8] = if dark_icons() {
        include_bytes!("../../assets/sprites/dark/output16.bmp")
    } else {
        include_bytes!("../../assets/sprites/output16.bmp")
    };
    Some(sheet.to_vec())
}

/// The pane bar's image list, built the way the main bar's is: the whole strip in
/// one `ImageList_Add` into an `ILC_COLOR32` list whose cell is the strip's own
/// width scaled by the DPI.
unsafe fn build_out_image_list(dpi: u32) -> Option<HIMAGELIST> {
    let (w, h, px) = strip_at_dpi(decode_strip(&out_strip_bytes()?)?, dpi)?;
    image_list(
        &px,
        w,
        h,
        bitmap_px(OUT_CX, dpi).max(1),
        OUT_STRIP_ICONS,
    )
}

/// The ids the 输出窗口 bar's own switch answers. That bar dispatches by id inside the
/// pane rather than through the command table, so this list — not
/// [`row_is_live`] — is its liveness rule, and the switch asserts against it so the
/// two cannot drift.
pub(super) const OUTPUT_ANSWERED: &[u32] =
    &[1280, 1282, 1284, 1286, 1287, 1288, 1315, 1316, 1320];

/// The 输出窗口 bar's twelve rows, which is what the pane's table ships: four header words
/// — an unused `1`, `cx = 16`, `cy = 15`, the entry count `12` — and one command id per
/// entry, `0` naming a separator. The output bar builder walks that list and gives
/// every real button the running image index `0..8`, `TBSTATE_ENABLED` and style 0, so
/// this bar has no checked button and no dropdown arrow — unlike the frame's bar, where
/// the same 1320 id is both.
pub(super) fn output_rows() -> Vec<TbRow> {
    const IDS: [u32; 12] = [1316, 1315, 0, 1282, 1280, 0, 1288, 1287, 0, 1320, 1286, 1284];
    let mut img = 0;
    IDS.iter()
        .map(|&id| {
            if id == 0 {
                return TbRow {
                    bmp: OUT_SEP_W,
                    id: 0,
                    state: 0,
                    style: BTNS_SEP as u8,
                };
            }
            let row = TbRow {
                bmp: img,
                id,
                state: if OUTPUT_ANSWERED.contains(&id) {
                    TBSTATE_ENABLED
                } else {
                    0
                },
                style: 0,
            };
            img += 1;
            row
        })
        .collect()
}

/// The height the output bar builder reads off the control's own font: `WM_GETFONT`,
/// the stock GUI font when the control was never given one, and the absolute
/// `lfHeight` of whatever that turns out to be.
unsafe fn font_px(hwnd: HWND) -> i32 {
    let hf = SendMessageW(hwnd, WM_GETFONT, Some(WPARAM(0)), None);
    let font = if hf.0 == 0 {
        GetStockObject(DEFAULT_GUI_FONT)
    } else {
        HGDIOBJ(hf.0 as *mut core::ffi::c_void)
    };
    let mut lf = LOGFONTW::default();
    let ok = GetObjectW(
        font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
    );
    if ok == 0 {
        return 0;
    }
    lf.lfHeight.abs()
}

#[cfg(test)]
mod arrow_tests {
    use super::*;

    /// The two metrics the dropdown arrow painter scales DPI global can
    /// hold. The helper is checked against the arithmetic gdi32 documents
    /// rather than against a picture.
    #[test]
    fn the_arrow_metrics_are_muldiv_of_the_hardcoded_pair() {
        for (dpi, gutter, half) in [(96, 14, 3), (120, 18, 4), (144, 21, 5), (192, 28, 6)] {
            assert_eq!(mul_div(14, dpi, 96), gutter, "gutter at {dpi}");
            assert_eq!(mul_div(3, dpi, 96), half, "half-width at {dpi}");
        }
        // 168 DPI is where the rounding rule shows: the true products are 24.5
        // and 5.25, and `MulDiv`'s half-up term makes those 25 and 5.
        assert_eq!(mul_div(14, 168, 96), 25);
        assert_eq!(mul_div(3, 168, 96), 5);
    }

    /// The strip is the right-anchored 14 px of the inset button, clipped to
    /// the button's own edge.
    #[test]
    fn the_gutter_is_the_right_hand_strip() {
        let rect = RECT {
            left: 100,
            top: 0,
            right: 124,
            bottom: 24,
        };
        let g = arrow_gutter(rect, 14);
        assert_eq!(g.left, 109);
        assert_eq!(g.right, 123);
        assert_eq!(g.top, 1);
        assert_eq!(g.bottom, 23);
        // A button narrower than the strip keeps its own left edge.
        let thin = arrow_gutter(
            RECT {
                left: 100,
                top: 0,
                right: 110,
                bottom: 24,
            },
            14,
        );
        assert_eq!(thin.left, 100);
    }

    /// 96 DPI: four 1 px rows, 7 px across at the top and 1 px at the tip,
    /// centred in the strip. The size the white arrow was measured at.
    #[test]
    fn the_triangle_is_four_rows_seven_across() {
        let g = arrow_gutter(
            RECT {
                left: 100,
                top: 0,
                right: 124,
                bottom: 24,
            },
            14,
        );
        let rows: Vec<RECT> = (0..=3).map(|i| arrow_row(g, 3, i)).collect();
        let want = [
            (113, 11, 120, 12),
            (114, 12, 119, 13),
            (115, 13, 118, 14),
            (116, 14, 117, 15),
        ];
        for (row, (l, t, r, b)) in rows.iter().zip(want) {
            assert_eq!((row.left, row.top, row.right, row.bottom), (l, t, r, b));
            assert_eq!(row.bottom - row.top, 1, "one pixel tall");
        }
        assert_eq!(rows[0].right - rows[0].left, 7);
        assert_eq!(rows[3].right - rows[3].left, 1);
        // The band starts at the strip's vertical centre less half the
        // half-width.
        assert_eq!(rows[0].top, (g.top + g.bottom) / 2 - 3 / 2);
    }

    /// 120 DPI scales the glyph the same way the strip does: 9 px across, 5
    /// rows.
    #[test]
    fn the_triangle_scales_with_the_band() {
        let half = mul_div(3, 120, 96);
        let g = arrow_gutter(
            RECT {
                left: 125,
                top: 0,
                right: 155,
                bottom: 30,
            },
            mul_div(14, 120, 96),
        );
        let top = arrow_row(g, half, 0);
        let last = arrow_row(g, half, half);
        assert_eq!(top.right - top.left, 2 * half + 1);
        assert_eq!(last.right - last.left, 1);
        assert_eq!(last.bottom - top.top, half + 1);
    }

    /// The state → colour table, straight out of the toolbar custom-draw handler's mask of
    /// `uItemState`: selected/checked wins over hot, the plain button gets no
    /// fill at all, and only `CDIS_DISABLED` moves the ink.
    #[test]
    fn the_state_fills_and_ink_come_from_the_theme() {
        let theme = Theme::default();
        assert_eq!(
            item_face(CDIS_SELECTED, &theme),
            Some(theme.toolbar_selected),
            "0x484848"
        );
        assert_eq!(
            item_face(CDIS_CHECKED, &theme),
            Some(theme.toolbar_selected)
        );
        assert_eq!(item_face(CDIS_HOT, &theme), Some(theme.toolbar_hot), "0x3A3A3A");
        assert_eq!(
            item_face(CDIS_SELECTED | CDIS_HOT, &theme),
            Some(theme.toolbar_selected),
            "selected outranks hot"
        );
        assert_eq!(item_face(NMCUSTOMDRAW_DRAW_STATE_FLAGS(0), &theme), None);
        assert_eq!(item_ink(CDIS_DISABLED, &theme), theme.toolbar_disabled);
        assert_eq!(
            item_ink(CDIS_SELECTED | CDIS_HOT, &theme),
            theme.toolbar_fg
        );
        assert_eq!(
            item_ink(NMCUSTOMDRAW_DRAW_STATE_FLAGS(0), &theme),
            theme.toolbar_fg
        );
        assert_eq!(theme.toolbar_fg, Color::rgb(0xE0, 0xE0, 0xE0));
        assert_eq!(theme.toolbar_disabled, Color::rgb(0x8C, 0x8C, 0x8C));
        assert_eq!(theme.toolbar_hot, Color::rgb(0x3A, 0x3A, 0x3A));
        assert_eq!(theme.toolbar_selected, Color::rgb(0x48, 0x48, 0x48));
        assert_eq!(theme.toolbar_frame, Color::rgb(0x5C, 0x5C, 0x5C));
        assert_eq!(theme.toolbar_bg, Color::rgb(0x20, 0x20, 0x20));
    }

    /// The return value the toolbar custom-draw handler builds by `(0xC1000 | dropdown) << 4`,
    /// spelled out here as the flags it names.
    #[test]
    fn the_item_flags_are_the_draw_return_bits_shifted() {
        let plain = (TBCDRF_NOEDGES | TBCDRF_NOBACKGROUND | TBCDRF_USECDCOLORS) as isize;
        assert_eq!(plain, 0xC1000 << 4);
        assert_eq!(plain | CDRF_NOTIFYPOSTPAINT as isize, 0xC1001 << 4);
    }
}

#[cfg(test)]
mod strip_tests {
    use super::*;

    /// The scale table
    #[test]
    fn the_scale_table_maps_dpi_to_a_factor() {
        let table = [
            (96, 1.0),
            (120, 1.25),
            (144, 1.5),
            (168, 1.75),
            (192, 2.0),
            (240, 2.5),
            (288, 3.0),
        ];
        for (dpi, want) in table {
            assert_eq!(dpi_scale(dpi), want, "dpi {}", dpi);
        }
        // The table steps at the boundaries, not below 120.
        assert_eq!(dpi_scale(119), 1.0);
        assert_eq!(bitmap_px(16, 120), 20);
        assert_eq!(bitmap_px(24, 120), 30);
        assert_eq!(bitmap_px(16, 96), 16);
    }

    /// Each embedded strip is what the button table's indices index
    /// into: 112 cells of `icon_size`, 32 bpp, with real alpha.
    #[test]
    fn every_strip_holds_112_alpha_glyphs() {
        for size in [16, 24, 32] {
            let bmp = strip_bytes(size).expect("strip shipped");
            let strip = decode_strip(&bmp).expect("decodable");
            assert_eq!(strip.h, size);
            assert_eq!(strip.w, size * STRIP_ICONS);
            assert_eq!(strip.px.len(), (strip.w * strip.h * 4) as usize);
            // The alpha byte is live — empty cells at 0, ink at 255 —
            // which is what lets `ImageList_Add` skip a colour key.
            let mut clear = 0;
            let mut solid = 0;
            for p in strip.px.chunks_exact(4) {
                match p[3] {
                    0 => clear += 1,
                    255 => solid += 1,
                    _ => {}
                }
            }
            assert!(
                clear > 0 && solid > 0,
                "strip {} alpha: {clear} clear",
                size
            );
        }
        assert!(strip_bytes(20).is_none());
    }

    /// The choice follows the bar's own face, not the editor's background.
    #[test]
    fn the_palette_follows_the_toolbar_face() {
        let mut theme = Theme::default();
        theme.toolbar_bg = Color::rgb(0xE7, 0xE9, 0xEC);
        theme.background = Color::rgb(0x10, 0x10, 0x10);
        assert!(!palette_is_dark(&theme), "a light bar takes deep ink");
        theme.toolbar_bg = Color::rgb(0x20, 0x20, 0x20);
        theme.background = Color::rgb(0xFF, 0xFF, 0xFF);
        assert!(palette_is_dark(&theme), "a dark bar takes light ink");
    }

    /// `DARK_ICONS` starts at `true` because the factory theme (and the
    /// bundled `DEFAULT_THEME`) is dark. If the factory ever turns light,
    /// both that initialiser and this assertion must be flipped together,
    /// or the first frame before any theme is applied shows the wrong ink.
    #[test]
    fn the_boot_palette_matches_the_factory_theme() {
        assert!(
            palette_is_dark(&Theme::default()),
            "the factory bar is dark; DARK_ICONS must start true"
        );
    }

    /// Two built-in inks, one artwork. The light sheets harden their edges
    /// because the atlas' faint coverage fringe reads as blur on a pale bar,
    /// so their alpha bytes no longer match the dark sheets'. Hardening may
    /// only clear or firm up coverage: never invent ink, never drop a glyph,
    /// and leave the dark sheet's own coverage untouched.
    #[test]
    fn light_and_dark_sheets_share_the_same_glyphs() {
        for size in [16, 24, 32] {
            let light = decode_strip(builtin_strip(size, false).expect("light sheet"))
                .expect("light decodes");
            let dark = decode_strip(builtin_strip(size, true).expect("dark sheet"))
                .expect("dark decodes");
            assert_eq!((light.w, light.h), (dark.w, dark.h));
            assert_eq!(light.px.len(), dark.px.len());

            // A crisp edge cannot put ink where the atlas had none.
            assert!(
                light
                    .px
                    .chunks_exact(4)
                    .zip(dark.px.chunks_exact(4))
                    .all(|(a, b)| a[3] == 0 || b[3] != 0),
                "sheet {size} grew ink outside the glyph"
            );

            let cells = (light.w / size) as usize;
            let mut light_core = vec![0i32; cells];
            let mut dark_core = vec![0i32; cells];
            let mut light_fog = 0i32;
            let mut dark_fog = 0i32;
            for p in 0..light.px.len() / 4 {
                let at = p * 4;
                let cell = (((p as i32) % light.w) / size) as usize;
                let la = light.px[at + 3];
                let da = dark.px[at + 3];
                light_core[cell] += i32::from(la >= 128);
                dark_core[cell] += i32::from(da >= 128);
                light_fog += i32::from(la > 0 && la < 64);
                dark_fog += i32::from(da > 0 && da < 64);
            }
            assert!(
                light_core
                    .iter()
                    .zip(&dark_core)
                    .all(|(a, b)| *a > 0 && *b > 0),
                "sheet {size} lost a glyph"
            );
            assert!(
                light_fog * 3 < dark_fog,
                "sheet {size} kept the atlas fringe: {light_fog} vs {dark_fog}"
            );
            assert!(
                light
                    .px
                    .chunks_exact(4)
                    .zip(dark.px.chunks_exact(4))
                    .any(|(a, b)| a[..3] != b[..3]),
                "sheet {size} has no second palette"
            );
        }
        assert!(builtin_strip(20, false).is_none());
    }

    /// A truncated or foreign bitmap is refused rather than painted.
    #[test]
    fn a_short_or_foreign_bitmap_is_not_a_strip() {
        assert!(decode_strip(b"not a bitmap at all").is_none());
        let mut bmp = strip_bytes(16).unwrap().to_vec();
        bmp.truncate(bmp.len() - 4);
        assert!(decode_strip(&bmp).is_none());
        let mut bmp = strip_bytes(16).unwrap().to_vec();
        bmp[30] = 3;
        assert!(decode_strip(&bmp).is_none());
    }

    /// The strip becomes a list of one cell per glyph at both the plain
    /// and the scaled size. `ImageList_Add` answers with the index of the
    /// first image it took, so the count is the only honest check.
    #[test]
    fn a_strip_becomes_a_112_cell_list() {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            for dpi in [96, 120] {
                let himl = build_image_list(16, dpi).expect("image list");
                assert_eq!(ImageList_GetImageCount(himl), STRIP_ICONS, "dpi {}", dpi);
                let _ = ImageList_Destroy(Some(himl));
            }
        }
    }

    /// The band the frame reserves: `icon_size + 8` at 96 DPI, which is
    /// what the probe measured, scaled by the same factor as the glyphs.
    #[test]
    fn the_band_grows_with_the_glyphs() {
        assert_eq!(band_h(16, 96), 24.0);
        assert_eq!(band_h(16, 120), 30.0);
        assert_eq!(band_h(24, 120), 40.0);
    }

    /// The pane strip's bitmap: nine 16 px glyphs on one 144 x 16 row, the alpha
    /// channel the `ILC_COLOR32` list reads as its transparency.
    #[test]
    fn the_output_strip_holds_nine_glyphs() {
        let strip = decode_strip(&out_strip_bytes().unwrap()).expect("output strip");
        assert_eq!(strip.w, OUT_CX * OUT_STRIP_ICONS);
        assert_eq!(strip.h, OUT_CX);
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            for dpi in [96, 120] {
                let himl = build_out_image_list(dpi).expect("output image list");
                assert_eq!(ImageList_GetImageCount(himl), OUT_STRIP_ICONS, "dpi {}", dpi);
                let _ = ImageList_Destroy(Some(himl));
            }
        }
    }

    /// The twelve rows exactly as the output bar builder lays them out from the strip:
    /// the ids in the file's order, separators only where an entry is `0`, the image
    /// index running over the real buttons alone (so it ends at nine), and style 0 on
    /// every button — this bar has neither a checked nor a dropdown row, whatever the
    /// frame's bar does with the same 1320.
    #[test]
    fn the_output_bar_rows_are_the_resource_table() {
        let rows = output_rows();
        assert_eq!(rows.len(), 12);
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![1316, 1315, 0, 1282, 1280, 0, 1288, 1287, 0, 1320, 1286, 1284]
        );
        let seps: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.style == BTNS_SEP as u8)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(seps, vec![2, 5, 8]);
        for (i, r) in rows.iter().enumerate() {
            if seps.contains(&i) {
                assert_eq!((r.bmp, r.state), (OUT_SEP_W, 0), "separator {i}");
            } else {
                assert_ne!(r.id, 0);
                assert_eq!(r.state, TBSTATE_ENABLED, "row {i}");
                assert_eq!(r.style, 0, "row {i}");
            }
        }
        let glyphs: Vec<i32> = rows
            .iter()
            .filter(|r| r.style != BTNS_SEP as u8)
            .map(|r| r.bmp)
            .collect();
        assert_eq!(glyphs, (0..OUT_STRIP_ICONS).collect::<Vec<_>>());
    }
}

/// A button must never look clickable when nothing answers it. The bar has no
/// feedback of its own — a click on a disabled button is swallowed by the control —
/// so the only honest signal that a command is missing is the greyed glyph.
#[cfg(test)]
mod liveness_tests {
    use super::*;

    /// The 工具条 page and the frame read and write the same `[ToolBar]` value:
    /// a stored layout round-trips through the writer unchanged, so a 应用 cannot
    /// hand the bar a table of ids nothing in the palette answers.
    #[test]
    fn the_toolbar_table_round_trips_through_its_file_value() {
        assert_eq!(ids_to_hex(&[57600, 0, 57643]), "00E10000000000002BE10000");
        assert_eq!(
            ids_from_hex("00E10000000000002BE10000", 3),
            vec![57600u32, 0, 57643]
        );
        let ids = default_layout().iter().map(|row| row.id).collect::<Vec<_>>();
        assert_eq!(ids_from_hex(&ids_to_hex(&ids), ids.len()), ids);
        // A stray trailing fragment is dropped rather than fatal.
        assert_eq!(ids_from_hex("00E1000000E1", 2), vec![57600u32]);
    }

    /// Every row the default table ships is lit only when something answers its id.
    /// A live row may still start dark — 撤销/重做/剪切/复制 are enabled from the
    /// editor on every refresh — so this is an implication, not an equality.
    #[test]
    fn a_row_is_lit_only_when_something_answers_it() {
        for row in default_layout() {
            if row.id == 0 {
                continue;
            }
            assert!(
                row.state & TBSTATE_ENABLED == 0 || row_is_live(row.id),
                "button id {} is lit but nothing answers it",
                row.id
            );
        }
    }

    /// The regression itself: the rows whose commands this build does not implement
    /// are greyed, while a real command and the dropdown button stay lit. The
    /// undo/redo/cut/copy rows are excluded because the frame enables those from the
    /// editor on every refresh.
    #[test]
    fn unimplemented_commands_arrive_grayed() {
        // 批量替换 (1420) used to sit on this list; the 批量替换 box answers
        // it now, so the bar's button lights up with the command.
        for id in [1569, 1239, 1489, 1290, 57664] {
            assert!(!row_is_live(id), "id {id} has no implementation yet");
        }
        assert!(row_is_live(57603), "保存 is implemented");
        assert!(row_is_live(1320), "the dropdown button opens a menu");
        assert!(row_is_live(1420), "批量替换 is implemented");
        for row in default_layout() {
            if [1569, 1239, 1489, 1290, 57664].contains(&row.id) {
                assert_eq!(row.state & TBSTATE_ENABLED, 0, "id {}", row.id);
            }
        }
    }

    /// A `[ToolBar]` layout restored from the file goes through the same gate, so the
    /// customization cannot resurrect a dead button.
    #[test]
    fn a_restored_layout_is_gated_too() {
        assert_eq!(row_for_command(1489).unwrap().state & TBSTATE_ENABLED, 0);
        assert_ne!(row_for_command(57603).unwrap().state & TBSTATE_ENABLED, 0);
        assert_ne!(row_for_command(1320).unwrap().state & TBSTATE_ENABLED, 0);
    }

    /// The 输出窗口 bar dispatches by id inside the pane; every row it ships has to be
    /// one the pane's switch answers.
    #[test]
    fn the_output_bar_only_offers_answered_rows() {
        for row in output_rows() {
            if row.id == 0 {
                continue;
            }
            assert!(
                OUTPUT_ANSWERED.contains(&row.id),
                "the output bar offers id {} that its switch ignores",
                row.id
            );
            assert_ne!(row.state & TBSTATE_ENABLED, 0, "id {}", row.id);
        }
    }

    /// One command per cell and one cell per command: the strip has no unnamed
    /// artwork and no two commands draw the same button from one image.
    /// The bar menu's rows: one check on the live square and one on the
    /// live side, merging greyed while only the main bar exists.
    #[test]
    fn the_bar_menu_marks_the_live_size_and_side() {
        let rows = context_menu_items(24, false, true);
        let row = |label: &str| rows.iter().find(|r| r.label == label).cloned();
        assert!(row("中图标(24*24)").unwrap().checked);
        assert!(!row("小图标(16*16)").unwrap().checked);
        assert!(!row("大图标(32*32)").unwrap().checked);
        assert!(row("下").unwrap().checked);
        assert!(!row("上").unwrap().checked);
        assert!(row("多行显示").unwrap().checked);
        assert!(!row("合并工具条").unwrap().enabled);
        let ids: Vec<u32> = rows.iter().filter(|r| !r.separator).map(|r| r.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn the_palette_names_one_cell_per_command() {
        let mut cells = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for &(id, cell) in PALETTE {
            assert!(ids.insert(id), "command {id} appears twice");
            assert!(
                (0..STRIP_ICONS).contains(&cell),
                "command {id} points at cell {cell}, off the {STRIP_ICONS}-cell strip"
            );
            assert!(cells.insert(cell), "cell {cell} is claimed twice");
        }
        assert_eq!(
            cells.len(),
            STRIP_ICONS as usize,
            "the strip has a cell no command names"
        );
    }

    /// A minimal 32 bpp top-down BMP `decode_strip` accepts.
    fn bmp_bytes(w: i32, h: i32) -> Vec<u8> {
        let (aw, ah) = (w.unsigned_abs() as usize, h.unsigned_abs() as usize);
        let stride = aw * 4;
        let mut b = Vec::new();
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&((54 + stride * ah) as u32).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&54u32.to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&32u16.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&((stride * ah) as u32).to_le_bytes());
        b.extend_from_slice(&[0u8; 16]);
        b.resize(54, 0);
        b.extend(std::iter::repeat(0xABu8).take(stride * ah));
        b
    }

    /// A throwaway theme root, unique per call so parallel tests cannot
    /// see each other's folders.
    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dg-theme-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn theme_sheet_takes_a_sheet_of_the_right_geometry() {
        let root = temp_root("ok");
        let theme = root.join("Bold");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(theme.join("toolbar16.bmp"), bmp_bytes(16 * 112, 16)).unwrap();
        let picked = theme_sheet_in(&[root.clone()], Some("Bold"), "toolbar16.bmp", 16, 112);
        assert_eq!(picked.map(|b| b.len()), Some(54 + 16 * 112 * 16 * 4));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn theme_sheet_rejects_a_wrong_shaped_sheet() {
        let root = temp_root("shape");
        let theme = root.join("Bold");
        std::fs::create_dir_all(&theme).unwrap();
        // Right bit depth, wrong width: not 112 cells of 16 px.
        std::fs::write(theme.join("toolbar16.bmp"), bmp_bytes(16 * 75, 16)).unwrap();
        let picked = theme_sheet_in(&[root.clone()], Some("Bold"), "toolbar16.bmp", 16, 112);
        assert!(picked.is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn theme_sheet_answers_none_without_a_theme() {
        let root = temp_root("none");
        let theme = root.join("Bold");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(theme.join("output16.bmp"), bmp_bytes(16 * 9, 16)).unwrap();
        assert!(theme_sheet_in(&[root.clone()], None, "output16.bmp", 16, 9).is_none());
        // A name is not allowed to walk out of the theme folder either.
        assert!(
            theme_sheet_in(&[root.clone()], Some("..\\Bold"), "output16.bmp", 16, 9).is_none()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn theme_names_lists_directories_that_carry_a_sheet() {
        let root = temp_root("names");
        let a = root.join("Alpha");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("toolbar16.bmp"), bmp_bytes(16 * 112, 16)).unwrap();
        // An empty folder and a loose file are not themes.
        std::fs::create_dir_all(root.join("Empty")).unwrap();
        std::fs::write(root.join("loose.bmp"), bmp_bytes(16, 16)).unwrap();
        assert_eq!(theme_names(&[root.clone()]), vec!["Alpha".to_string()]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
mod tooltip_tests {
    use super::{PALETTE, TB_SCRIPT_BASE, TbRow, ToolbarWin, default_layout};

    /// Every button on the default bar has tooltip text.
    ///
    /// The control is created with `TBSTYLE_TOOLTIPS`, so comctl32's own
    /// tooltip asks the frame through `TTN_GETDISPINFOW`; the frame answers
    /// with the 说明 column of `CMD_TEXT`, looked up by the command id the
    /// notification carries. A button absent from that table therefore gets an
    /// empty tip — which is what 自动换行 did until its row was added.
    #[test]
    fn every_default_button_has_a_tooltip() {
        let mut missing: Vec<u32> = default_layout()
            .iter()
            .filter(|row| row.id != 0)
            .map(|row| row.id)
            .filter(|id| crate::win32::cmdtext::text_for(*id).1.is_empty())
            .collect();
        missing.sort_unstable();
        missing.dedup();
        assert!(missing.is_empty(), "no tooltip text for {missing:?}");
    }

    /// 皮肤按钮的 id 起点必须**高过所有内置命令号**。
    ///
    /// 「把皮肤那几行摘出来」靠的是 `id >= TB_SCRIPT_BASE`，所以起点只要压到某个
    /// 内置命令下面，那一组按钮就会被当成皮肤行摘掉。第一次取的是 0x9000
    /// （36864），比 57600（文件-新建）还低 —— 文件、编辑、查看三组直接从工具栏
    /// 上消失，而且画面上看不出是掉了，只像是「排得少」。这条测试守着它。
    #[test]
    fn script_ids_sit_above_every_builtin_command() {
        let highest = PALETTE.iter().map(|(id, _)| *id).max().unwrap_or(0);
        assert!(
            TB_SCRIPT_BASE > highest,
            "TB_SCRIPT_BASE {TB_SCRIPT_BASE} <= highest palette id {highest}"
        );
        // 命令号是 `WM_COMMAND` 的低字，只有 16 位。
        assert!(TB_SCRIPT_BASE + 0x0F00 < 0x1_0000, "script ids overflow the low word");
        // 和状态栏那一段错开：两条 id 都从 `WM_COMMAND` 低字进来。
        assert_ne!(TB_SCRIPT_BASE, crate::win32::statusbar::SCRIPT_BASE);
    }

    /// 皮肤按钮插在 `after` 那一格后面；`after = 0` 和「找不到锚点」都落到末尾。
    #[test]
    fn script_rows_are_inserted_after_their_anchor() {
        let mut bar = ToolbarWin::new();
        let base = default_layout();
        let anchor = base[0].id;
        bar.script_rows = vec![
            dg_script::ToolbarButton {
                icon: 60,
                tip: "时钟".into(),
                command: "a".into(),
                after: anchor,
            },
            dg_script::ToolbarButton {
                icon: 61,
                tip: "b".into(),
                command: "b".into(),
                after: 0,
            },
            dg_script::ToolbarButton {
                icon: 62,
                tip: "c".into(),
                command: "c".into(),
                after: 999_999,
            },
        ];
        let merged = bar.with_script(base.clone());
        assert_eq!(merged.len(), base.len() + 3);
        assert_eq!(merged[0].id, anchor);
        assert_eq!(merged[1].id, TB_SCRIPT_BASE);
        assert_eq!(merged[merged.len() - 2].id, TB_SCRIPT_BASE + 1);
        assert_eq!(merged[merged.len() - 1].id, TB_SCRIPT_BASE + 2);

        // 再合一次不会长出第二份 —— `rebuild` 那条路就是把 `self.buttons`
        // （已经带着皮肤行）交回 `with_script`。
        bar.buttons = merged.clone();
        let again = bar.with_script(bar.base_layout());
        let ids = |rows: &[TbRow]| rows.iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(&again), ids(&merged));
    }
}
