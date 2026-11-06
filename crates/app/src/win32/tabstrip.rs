//! The document tab strip: a native tab-control child window.
//!
//! The strip keeps a scrollable band, a tail of close/list buttons and the
//! `[Tab]` settings (`ShowIcon`, `ShowMark`, `Scroll`, `ItemClose`,
//! `HideOnlyOne`). Geometry is a running right edge over the item list; the
//! selected item is measured with the bold font and may append a `" *"` mark
//! when `ShowMark` is on. The scroll offset is clamped so the content cannot
//! be scrolled past its own end, and the ◀/▶ buttons grey out when there is
//! nothing to scroll to.

#![allow(unsafe_op_in_unsafe_fn)]

use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateFontIndirectW, DC_PEN, DeleteDC, DeleteObject, DrawTextW, DT_CALCRECT, DT_CENTER,
    DT_EDITCONTROL, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, DT_WORD_ELLIPSIS, EndPaint, FillRect, FONT_CHARSET, GetDC,
    GetStockObject, GetTextExtentPoint32W, GetTextMetricsW, HGDIOBJ, HDC, HFONT, InvalidateRect,
    LOGFONTW, LineTo, MoveToEx, NULL_BRUSH, PAINTSTRUCT, PS_SOLID, PtInRect, ReleaseDC, RoundRect,
    SelectObject, SetBkMode, SetDCPenColor, SetTextColor, SRCCOPY, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    HIMAGELIST, IMAGE_LIST_DRAW_STYLE, ImageList_Destroy, ImageList_GetIcon, WM_MOUSELEAVE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_LBUTTON,
};
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyIcon, DI_NORMAL,
    DrawIconEx, GetClientRect, GetParent, GetWindowRect, GWLP_WNDPROC, HCURSOR, HMENU, HICON,
    IDC_ARROW, IDC_NO, IDC_SIZEALL, KillTimer,
    LoadCursorW, NONCLIENTMETRICSW, RegisterClassW, SetCursor, SetTimer, SetWindowLongPtrW,
    SetWindowPos, SPI_GETNONCLIENTMETRICS, SWP_NOACTIVATE, SWP_NOZORDER,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW, HTCLIENT, WM_CAPTURECHANGED,
    WM_ERASEBKGND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN, WM_SETCURSOR, WM_TIMER, WINDOW_EX_STYLE, WNDCLASSW,
    WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::win32::gdi::pixel_height;
use crate::win32::shape::CellShape;
use crate::win32::toolbar::{
    dark_icons, decode_strip, dpi_scale, image_list, strip_at_dpi, theme_sheet,
};
use crate::win32::view::instance;
use crate::win32::window::{high_word, low_word, state_mut, to_colorref};
use dg_render::Rect;
use dg_render::theme::Theme;

use windows::Win32::UI::HiDpi::GetDpiForWindow;

/// Part of the harness contract the editor view's class is: `tools/uicheck`
/// enumerates the child tree by class, so the tab strip is visible from
/// outside the process.
const TAB_CLASS: PCWSTR = w!("SimpleTabCtrl");

/// The strip's control id. The strip takes an id of its own and `GetDlgCtrlID`
/// still names it.
pub(super) const CTRL_ID: u32 = 3200;

/// Timer ids of the two scroll buttons, `0x20` for ▶ and `0x10` for ◀ as
/// the tab hit test arms them.
const TIMER_RIGHT: usize = 0x20;
const TIMER_LEFT: usize = 0x10;

/// The auto-repeat ramp, from the same function's `v & 0xC000` ladder.
const REPEAT_FIRST_MS: u32 = 100;
const REPEAT_FAST_MS: u32 = 25;
const REPEAT_FASTEST_MS: u32 = 10;

/// `Marlett` glyphs of the tail cells, drawn `DT_CENTER|DT_VCENTER|DT_SINGLELINE`.
const GLYPH_LEFT: &str = "3";
const GLYPH_RIGHT: &str = "4";
const GLYPH_DROP: &str = "6";

/// Which cell each part of the strip is: 1 close, 2 ▶, 3 ◀, 4 the tab
/// band, 5 ▾. The same ids number the hover nibble (bits 16-19), the pressed
/// nibble (bits 20-23) and the cursor-inside nibble (bits 24-27), so one value
/// names a cell in all three.
const PRESS_NONE: u8 = 0;
const PRESS_RIGHT: u8 = 2;
const PRESS_LEFT: u8 = 3;
const PRESS_DROP: u8 = 5;
const CELL_BAND: u8 = 4;

/// The ✕ cell's state byte. The close-button state machine's move branch resets
/// every tab to `2` and puts `3` on the one whose cell holds the cursor, and
/// the tab item painter draws the hover glyph only for a hot tab sitting at `3`.
const CLOSE_NORMAL: u8 = 2;
const CLOSE_HOT: u8 = 3;

/// The three cells of the ✕ strip that the tab item painter asks
/// `ImageList_GetIcon` for: plain, hovered, and hovered with the left button
/// held.
const X_NORMAL: i32 = 56;
const X_HOT: i32 = 22;
const X_PRESS: i32 = 57;

/// The ✕ strip — 1200×16, i.e. 75 cells of 16 px — which the close-glyph
/// load hands the image-list fill to fill the global `himl` the ✕ glyphs come
/// out of.
const X_CELLS: i32 = 75;

fn x_strip_bytes() -> Option<Vec<u8>> {
    if let Some(bytes) = theme_sheet("tabstrip16.bmp", 16, X_CELLS) {
        return Some(bytes);
    }
    // The close glyphs sit on the tab row, which follows the same light or
    // dark face as the toolbar, so they take the matching built-in ink.
    let sheet: &'static [u8] = if dark_icons() {
        include_bytes!("../../assets/sprites/dark/tabstrip16.bmp")
    } else {
        include_bytes!("../../assets/sprites/tabstrip16.bmp")
    };
    Some(sheet.to_vec())
}

/// The three ✕ glyphs, in the order the tab item painter picks between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum XState {
    Normal,
    Hot,
    Pressed,
}

impl XState {
    /// The cell of the ✕ strip this state is drawn from.
    fn cell(self) -> i32 {
        match self {
            XState::Normal => X_NORMAL,
            XState::Hot => X_HOT,
            XState::Pressed => X_PRESS,
        }
    }
}

/// The `[Tab]` style settings the strip honours. Four of them change the window
/// itself — the file-list and scroll flags, the shared close button and the
/// per-item close button — and the rest only repaint it. The defaults are the
/// shipped `setting.ini`'s: `ShowIcon=Y ShowMark=Y FixedWidth=N Scroll=Y
/// FileList=Y ItemClose=Y TabBar=Y HideOnlyOne=Y`.
#[derive(Clone, Copy, Debug)]
pub(super) struct TabSettings {
    pub show_icon: bool,
    pub show_mark: bool,
    pub scroll: bool,
    pub dropdown: bool,
    /// Style bit `0x20` (`ItemClose=Y`): every tab carries a ✕ cell on its right. That
    /// form never runs and is not implemented yet.
    pub item_close: bool,
    /// `HideOnlyOne=Y`: one document leaves no row at all.
    pub hide_only_one: bool,
}

impl Default for TabSettings {
    fn default() -> Self {
        Self {
            show_icon: true,
            show_mark: true,
            scroll: true,
            dropdown: true,
            item_close: true,
            hide_only_one: true,
        }
    }
}

/// The strip's pixel metrics
/// which is why they are a struct rather than constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Metrics {
    /// `cyWidth = (int)(scale * 16.0)`: the icon cell, and the height a tail
    /// cell is measured against. 20 at 120 DPI.
    pub cy: i32,
    /// `cyWidth - 1` — a tail button's width (the tab-buttons layout's `v4`). 19.
    pub cell: i32,
    /// Where the layout cursor starts.
    pub inset: i32,
    /// Label padding; used twice per tab.
    pub pad: i32,
    /// The gap on each side of the icon.
    pub extra: i32,
    pub height: i32,
    /// The ✕ cell's width — `tab.close`, or the icon cell when the skin leaves
    /// it at 0. Reserved in every tab's width whether or not the tab is wide
    /// enough to reach it.
    pub close: i32,
    /// Pixels a ◀/▶ press or a wheel notch moves the strip, read out of a 6-bit
    /// field, so the step caps at 63 px, which rules out a tab width (99-337 px in
    /// the capture), so it is a per-notch step. `cyWidth` is the value every other
    /// cell of the strip is built from.
    pub step: i32,
    /// 建这套度量用的 DPI。皮肤绘制要把设备像素折回 DIP，别的路径用不到它 ——
    /// 但漏了它就得再存一份 `dpi` 到 `TabStrip` 上，两处容易走散。
    pub dpi: u32,
}

impl Metrics {
    pub(super) fn at(dpi: u32, tm_height: i32) -> Self {
        Self::with(
            dpi,
            tm_height,
            crate::skin::metrics(),
            dg_skin::MetricId::TabHeight,
        )
    }

    /// 停靠页签条（`DockCaption`）用的是同一套度量，只有高度那项不一样：
    /// `dock.tab.height` 而不是 `tab.height`。
    pub(super) fn for_dock(dpi: u32, tm_height: i32) -> Self {
        Self::with(
            dpi,
            tm_height,
            crate::skin::metrics(),
            dg_skin::MetricId::DockTabHeight,
        )
    }

    /// 和 [`Metrics::at`] 一样，只是度量表和高度项从外面给 —— 皮肤那套读的是
    /// 全局，但这条路径要能单独测。
    pub(super) fn with(
        dpi: u32,
        tm_height: i32,
        skin: dg_skin::Metrics,
        height_id: dg_skin::MetricId,
    ) -> Self {
        let cy = (dpi_scale(dpi) * 16.0) as i32;
        // 皮肤可以改高度、内边距、关闭钮格宽（`[metrics]`）。0 表示「跟字体/
        // 图标格走」，所以出厂值下这里算出来的数字和以前一模一样。
        //
        // 高度按 DIP 放大；pad/close 不放大 —— 这个绘制器里的 `inset`/`extra`
        // 本来就是字面量设备像素，只有图标格跟着 DPI 走。见 `MetricId` 的注释。
        let scale = dpi as f32 / 96.0;
        let height = match skin.get(height_id) {
            h if h > 0.0 => (h * scale).round() as i32,
            _ => 22.max(tm_height * 13 / 10),
        };
        let pad = skin.get(dg_skin::MetricId::TabPadX).round() as i32;
        let close = match skin.get(dg_skin::MetricId::TabClose).round() as i32 {
            0 => cy,
            w => w,
        };
        Self {
            cy,
            cell: cy - 1,
            inset: 5,
            pad,
            extra: 3,
            height,
            close,
            step: cy,
            dpi,
        }
    }
}

/// One tab's horizontal extent before the scroll offset
/// keeps at `item + 8`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TabRect {
    pub left: i32,
    pub right: i32,
}

/// A tab's natural width from its measured label — the tab scroll handler's loop body.
pub(super) fn tab_width(
    textw: i32,
    m: &Metrics,
    show_icon: bool,
    show_mark: bool,
    close_cell: i32,
) -> i32 {
    let mut w = m.extra + textw + 2 * m.pad + m.extra + close_cell;
    if show_icon {
        w += m.cy;
    }
    if show_mark {
        w += 2;
    }
    w
}

/// The row of tabs: rects from the inset with no gaps, plus the cursor after the
/// last one — the content's right edge.
pub(super) fn layout_rects(widths: &[i32], inset: i32) -> (Vec<TabRect>, i32) {
    let mut cursor = inset;
    let mut rects = Vec::with_capacity(widths.len());
    for &w in widths {
        rects.push(TabRect {
            left: cursor,
            right: cursor + w,
        });
        cursor += w;
    }
    (rects, cursor)
}

/// Where the tail cells sit and what band is left for the tabs. The tab-buttons
/// layout packs them against the band's right edge, right to left in the order ▶, ◀,
/// ▾, each `cell` wide.
pub(super) fn tail_rects(
    client_right: i32,
    m: &Metrics,
    scroll: bool,
    dropdown: bool,
) -> (Option<TabRect>, Option<TabRect>, Option<TabRect>, i32) {
    if client_right < m.cell {
        return (None, None, None, client_right);
    }
    let mut edge = client_right - 2;
    let (mut right, mut left) = (None, None);
    if scroll {
        right = Some(TabRect {
            left: edge - m.cell,
            right: edge,
        });
        left = Some(TabRect {
            left: edge - 2 * m.cell,
            right: edge - m.cell,
        });
        edge -= 2 * m.cell;
    }
    let drop = if dropdown {
        let r = TabRect {
            left: edge - m.cell,
            right: edge,
        };
        edge -= m.cell;
        Some(r)
    } else {
        None
    };
    (right, left, drop, edge)
}

/// The content's right edge can never sit left of the band's right edge, and the offset can
/// never be positive, which is exactly what left-aligns a list that fits.
pub(super) fn clamp_xoff(xoff: i32, content: i32, band_right: i32) -> i32 {
    xoff.clamp((band_right - content).min(0), 0)
}

/// The scroll capability bits: ◀ is live while something is scrolled off to
/// the left, ▶ while the last tab still overhangs the band.
pub(super) fn scroll_state(xoff: i32, content: i32, band_right: i32) -> (bool, bool) {
    (xoff < 0, content + xoff > band_right)
}

/// the close press handler's scroll-into-view. The item's rect is offset by
/// the current scroll position and then compared against the *client* rect —
/// the `GetClientRect` the layout also reads — not against the band the
/// tail was carved from, so a tab brought in from the right can end up under
/// the tail cells. Both branches overshoot by 20 px: the left branch adds the
/// gap to the client's left edge plus 20, the right branch the gap to its right
/// edge minus 20. The whole block sits behind the two live-scroll-button bits.
pub(super) fn scroll_into_view(xoff: i32, rect: TabRect, client: &RECT, live: bool) -> i32 {
    if !live {
        return xoff;
    }
    let left = rect.left + xoff;
    let right = rect.right + xoff;
    if left < client.left {
        xoff + client.left - left + 20
    } else if right > client.right {
        xoff + client.right - right - 20
    } else {
        xoff
    }
}

/// A point's owner, tested in the tab hit test's order: the tail cells first, so
/// a tab that scrolled under one cannot steal the click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Hit {
    Right,
    Left,
    Dropdown,
    Tab(usize),
    None,
}

pub(super) fn hit_test(
    x: i32,
    rects: &[TabRect],
    xoff: i32,
    right: Option<TabRect>,
    left: Option<TabRect>,
    dropdown: Option<TabRect>,
) -> Hit {
    let hits = |r: Option<TabRect>| r.is_some_and(|r| x >= r.left && x < r.right);
    if hits(right) {
        return Hit::Right;
    }
    if hits(left) {
        return Hit::Left;
    }
    if hits(dropdown) {
        return Hit::Dropdown;
    }
    for (i, r) in rects.iter().enumerate() {
        if x >= r.left + xoff && x < r.right + xoff {
            return Hit::Tab(i);
        }
    }
    Hit::None
}

/// What the strip shows for one document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TabEntry {
    /// The bare name. The tab scroll handler appends the literal `" *"` when it
    /// measures a tab and `ShowMark` is on, so the mark is part of the tab's
    /// width whether or not this tab carries it — and the tab item painter decides at
    /// paint time, from this flag, whether to draw it.
    pub label: String,
    pub path: Option<String>,
    /// The item's state `1` (`item + 24`), which is what the tab item painter tests
    /// before appending the mark and the bar row handler before painting the red
    /// band across a tab's top edge.
    pub modified: bool,
}

/// What a press on the strip means for the frame. Returned rather than applied
/// because a tab action is a document action, and the document list lives with
/// the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Press {
    /// Activate tab `i`.
    Activate(usize),
    /// A tail scroll button went down; repeat under this timer id.
    Scroll(usize),
    /// ▾ went down: the open-document list opens.
    Dropdown,
    /// The band, or a button with nothing to scroll.
    None,
}

/// What a release means, in the capture release's order: the cell whose pressed and
/// inside nibbles agree answers first, and the ✕ state machine only runs when no
/// cell did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Release {
    Dropdown,
    /// Tab `i`'s armed ✕ released under the cursor: close it.
    Close(usize),
    Nothing,
}

/// The strip window.
pub(super) struct TabStrip {
    /// 皮肤的绘制宿主。`Rc` 共享：绘制期拿不到 `&mut EditorState`（重入是
    /// 硬约束），所以子窗口自己留一份强引用，和状态栏一个道理。
    script: Option<std::rc::Rc<dg_script::ScriptHost>>,
    hwnd: HWND,
    /// Last rect handed to `SetWindowPos`, so a layout that changes nothing
    /// costs no window message.
    frame: Rect,
    settings: TabSettings,
    metrics: Metrics,
    pub(super) entries: Vec<TabEntry>,
    rects: Vec<TabRect>,
    /// Content right edge: the layout cursor after the last tab.
    content: i32,
    band_right: i32,
    xoff: i32,
    can_left: bool,
    can_right: bool,
    tail_right: Option<TabRect>,
    tail_left: Option<TabRect>,
    tail_drop: Option<TabRect>,
    /// Vertical cell of the tail glyphs — the tab-buttons layout centres it.
    tail_top: i32,
    tail_bottom: i32,
    active: usize,
    pressed: u8,
    /// The cell the cursor is inside. The tab promote keeps it
    /// on every move, and a cell's hover state only arms one message *after* it:
    /// that cell is what the hover branch tests, which is why a tail glyph goes
    /// black on the second move into it rather than the first.
    inside_cell: u8,
    /// The hovered cell: the cell whose glyph is drawn in
    /// the hot colour, or `CELL_BAND` while a tab is hovered.
    hover_cell: u8,
    /// The hovered item, `-1` for none. This is what the driver turns into
    /// the draw struct's hot flag, and what makes the bar row handler paint the top
    /// band of that tab orange.
    hot_item: i32,
    /// 皮肤要的指针和提示（`s:hint("tab", …)`）。这一条页签条本来一条提示都没有，
    /// 所以提示也归皮肤。
    hint: crate::win32::skinhint::SkinHint,
    /// Set while a `TrackMouseEvent` leave notification is still owed.
    tracking: bool,
    /// How many repeats the held-down scroll button has fired, which indexes
    /// the 100/25/10 ms ramp.
    repeat: u32,
    font: HFONT,
    bold: HFONT,
    marlett: HFONT,
    /// `SHGetFileInfoW` handles, one per entry, so a repaint does not re-ask
    /// the shell. Freed when the entries change.
    icons: Vec<Option<isize>>,
    /// The client rect the last layout saw. The tab items' vertical extent is
    /// the whole row, so this is what each item's top and bottom are, and what
    /// the ✕ cell's hit rect is measured against.
    client: RECT,
    /// One byte per tab, the ✕ cell's state.
    close_state: Vec<u8>,
    /// Set when a press went down inside a tab's ✕ cell, so the
    /// release closes that tab if the cursor is still over the cell.
    close_armed: bool,
    /// The tab a band press landed on, and the x it landed at. The drag arms
    /// on the move whose x first leaves that x while the press is still down —
    /// the release keeps the pair until then, and a press that never dragged
    /// spends it on the way up.
    press_item: Option<usize>,
    press_x: i32,
    /// `Some(original position)` while a drag is on. The dragged tab's current
    /// position lives in `press_item`, which follows it across the moves, so
    /// an interrupted drag knows both ends of the way home.
    drag_from: Option<usize>,
    /// The drag's two cursors, loaded once: a four-headed arrow where a tab
    /// has somewhere to go — inside this strip, or over the other one — the
    /// no-entry sign everywhere else.
    drag_cur: HCURSOR,
    nodrop_cur: HCURSOR,
    /// 另一条页签条的窗口：拖到自己条外、落在它那一段矩形上，就是把这一页
    /// 换进那个分组。没有第二分组时它压在零矩形上，命中判断自己就不成立。
    other: HWND,
    /// The global image list the close-glyph load fills from
    /// `assets/sprites/tabstrip16.bmp` (16 px cells) and from which the tab
    /// item painter takes the ✕'s three glyphs.
    xlist: HIMAGELIST,
    xicons: [Option<isize>; 3],
}

/// 脚本绘制这一区时的名字，和 `dg_skin::Region` 同一套词。
const SKIN_REGION: &str = "tab";

impl TabStrip {
    pub(super) fn set_script(&mut self, host: Option<std::rc::Rc<dg_script::ScriptHost>>) {
        self.script = host;
    }

    pub(super) fn new() -> Self {
        Self {
            script: None,
            hwnd: HWND::default(),
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            settings: TabSettings::default(),
            metrics: Metrics::at(96, 16),
            entries: Vec::new(),
            rects: Vec::new(),
            content: 0,
            band_right: 0,
            xoff: 0,
            can_left: false,
            can_right: false,
            tail_right: None,
            tail_left: None,
            tail_drop: None,
            tail_top: 0,
            tail_bottom: 0,
            active: 0,
            pressed: PRESS_NONE,
            inside_cell: PRESS_NONE,
            hover_cell: PRESS_NONE,
            hot_item: -1,
            hint: crate::win32::skinhint::SkinHint::new(),
            tracking: false,
            repeat: 0,
            font: HFONT::default(),
            bold: HFONT::default(),
            marlett: HFONT::default(),
            icons: Vec::new(),
            client: RECT::default(),
            close_state: Vec::new(),
            close_armed: false,
            press_item: None,
            press_x: 0,
            drag_from: None,
            drag_cur: HCURSOR::default(),
            nodrop_cur: HCURSOR::default(),
            other: HWND::default(),
            xlist: HIMAGELIST::default(),
            xicons: [None, None, None],
        }
    }

    pub(super) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Left edge of the ▾ cell, in this window's client space. The frame needs
    /// it to hang the document list under the cell that was clicked; with no
    /// cell (a strip too narrow for a tail, or `[Tab] FileList=N`) the menu goes
    /// to the client's right edge, which is where the cell would have been.
    pub(super) fn dropdown_edge(&self) -> i32 {
        self.tail_drop.map(|r| r.left).unwrap_or(self.frame.w.round() as i32)
    }

    /// Height of the row the dock reserves: the strip's own height, or 0 when
    /// the bar is off or, under `HideOnlyOne`, when it holds a single document.
    /// The count comes from the entries the strip was last given, so the layout
    /// pass never re-walks the document list to answer it.
    pub(super) fn row_h(&self, bar_on: bool) -> f32 {
        if !bar_on || (self.settings.hide_only_one && self.entries.len() < 2) {
            return 0.0;
        }
        self.metrics.height as f32
    }

    /// 分割状态下的行高：两条页签条各管各的分组，「仅一个时隐藏」不再
    /// 收走它们 —— 条上有页签就占整行（搬进第二分组的那页要在第二条
    /// 上看得见）；条上没页签（克隆视图的第二分组）不占位。标签条总
    /// 闸关着时照旧为 0。
    pub(super) fn row_h_grouped(&self, bar_on: bool) -> f32 {
        if !bar_on || self.entries.is_empty() {
            return 0.0;
        }
        self.metrics.height as f32
    }

    /// Creates the window. The fonts need `dpi`, and the metrics need the fonts,
    /// so both happen here rather than at first paint.
    pub(super) unsafe fn create(&mut self, parent: HWND, dpi: u32, settings: TabSettings) {
        self.settings = settings;
        REGISTER_CLASS.call_once(|| {
            let _ = RegisterClassW(&WNDCLASSW {
                style: CS_DBLCLKS,
                // The stock arrow — ATL's `CWndClassInfo` registers every class
                // it makes with one, and the strip's own procedure leaves
                // `WM_SETCURSOR` to `DefWindowProc`, so the class cursor is the
                // only shape this window ever answers with. Leaving it NULL —
                // which is what the `..Default::default()` fills in — makes
                // `DefWindowProc` change nothing, and the I-beam the editor
                // view last left on the pointer survives the crossing onto the
                // tab.
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpfnWndProc: Some(tab_proc),
                lpszClassName: TAB_CLASS,
                hInstance: instance(),
                ..Default::default()
            });
        });
        self.build_fonts(dpi);
        self.metrics = Metrics::at(dpi, self.tm_height());
        if self.xlist.0 == 0 {
            self.build_x_list(dpi);
        }
        if self.alive() {
            return;
        }
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            TAB_CLASS,
            w!("tabs"),
            // `WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN`, the
            // pair included: the strip shares its band with nothing today, but
            // a sibling that overlaps it (the overlay bars) must not be painted
            // over by a strip repaint.
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
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
        // Registered rather than subclassed, so this only confirms the class
        // procedure survived `CreateWindowExW` — the same check the view does.
        let _ = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, tab_proc as *const () as isize);
        self.hwnd = hwnd;
    }

    /// The label font and its bold twin — the latter used to measure and draw the selected
    /// tab — plus the `Marlett` face the tail glyphs need. The face comes from
    /// `NONCLIENTMETRICSW` and the height from the DPI.
    ///
    /// 皮肤写了 `[fonts] tab` 就改用它的字面/字号/字重/斜体；没写就照旧。这一处
    /// 就是页签的**重算点**：`create` 里 `build_fonts` 先跑、`Metrics::at(dpi,
    /// tm_height())` 后跑，所以行高、`tab_width` 量出来的宽度、命中矩形全部跟着
    /// 新字体走一遍，不需要另开一条 `remeasure_at` 路径。
    unsafe fn build_fonts(&mut self, dpi: u32) {
        if !self.font.0.is_null() {
            return;
        }
        let mut nc = NONCLIENTMETRICSW {
            cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let _ = SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            Some(&mut nc as *mut _ as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let mut lf = nc.lfMessageFont;
        let spec = crate::skin::font_spec(dg_skin::FontToken::Tab);
        let points = spec.as_ref().map_or(Self::MESSAGE_POINTS, |s| s.size);
        let weight = spec.as_ref().map_or(400, |s| s.weight) as i32;
        lf.lfHeight = -pixel_height(dpi, points).max(1);
        if let Some(spec) = &spec {
            lf.lfItalic = u8::from(spec.italic);
            if !spec.face.is_empty() {
                lf.lfFaceName = [0; 32];
                for (c, slot) in spec
                    .face
                    .encode_utf16()
                    .take(lf.lfFaceName.len() - 1)
                    .zip(lf.lfFaceName.iter_mut())
                {
                    *slot = c;
                }
            }
        }
        lf.lfWeight = weight;
        self.font = CreateFontIndirectW(&lf);
        // 粗体那只：至少 700 —— 皮肤写了更粗（800/900）就照它，不然选中的页签
        // 会比常态还细。
        lf.lfWeight = weight.max(700);
        self.bold = CreateFontIndirectW(&lf);
        // The arrow font: `Marlett` at 9 pt, height through
        // `MulDiv(LOGPIXELSY, 90, 720)`, everything else left DONTCARE and the
        // charset at 1 (DEFAULT_CHARSET). ANSI_CHARSET makes GDI pick a
        // substitute face and the tail captions render as the digits 6 3 4.
        let mut ml = LOGFONTW::default();
        ml.lfHeight = -pixel_height(dpi, 9.0).max(1);
        ml.lfCharSet = FONT_CHARSET(1);
        for (c, slot) in "Marlett".encode_utf16().zip(ml.lfFaceName.iter_mut()) {
            *slot = c;
        }
        self.marlett = CreateFontIndirectW(&ml);
    }

    /// The interface font's size in points
    /// labels are drawn at.
    const MESSAGE_POINTS: f32 = 9.0;

    /// `tm.tmHeight` of the label font — the number the row height comes from.
    unsafe fn tm_height(&self) -> i32 {
        if self.font.0.is_null() {
            return 16;
        }
        let hdc = GetDC(None);
        let saved = SelectObject(hdc, HGDIOBJ(self.font.0));
        let mut tm = TEXTMETRICW::default();
        let _ = GetTextMetricsW(hdc, &mut tm);
        SelectObject(hdc, saved);
        ReleaseDC(None, hdc);
        tm.tmHeight
    }

    /// Moves the window. A zero-height window simply never gets painted.
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
        self.relayout();
    }

    /// Replaces what the strip shows. The caller builds the labels; this only
    /// works out whether anything changed, because the per-path icon handles
    /// must not be re-fetched on every paint.
    pub(super) unsafe fn set_entries(&mut self, entries: Vec<TabEntry>, active: usize) {
        let active = active.min(entries.len().saturating_sub(1));
        if entries == self.entries && active == self.active {
            return;
        }
        if entries != self.entries {
            self.drop_icons();
            self.entries = entries;
            self.relayout();
        }
        if active == self.active {
            self.invalidate();
        } else {
            self.set_current(active);
        }
    }

    /// The select path — the one way the selection moves. Only after that does the
    /// tab layout pass re-run — which matters because the selected tab is measured
    /// bold, so the layout can move the very tab it just lifted into view. An
    /// index at or past the item count is dropped and the selection stays where it
    /// was.
    pub(super) unsafe fn set_current(&mut self, index: usize) {
        self.active = if index >= self.entries.len() {
            self.active
        } else {
            index
        };
        if let Some(r) = self.rects.get(self.active).copied() {
            self.xoff = scroll_into_view(
                self.xoff,
                r,
                &self.client,
                self.can_left || self.can_right,
            );
        }
        self.relayout();
        self.invalidate();
    }

    unsafe fn drop_icons(&mut self) {
        for slot in self.icons.iter_mut() {
            if let Some(h) = slot.take() {
                let _ = DestroyIcon(HICON(h as *mut core::ffi::c_void));
            }
        }
        self.icons.clear();
    }

    /// 字体换了（主题的 `[Fonts]` 或 `App.ui.style:font` 在运行时改了）：扔掉
    /// 那两只 HFONT 重造一遍，再按新字体的 `tmHeight` 重算行高与每格宽度。
    ///
    /// [`Self::build_fonts`] 见到活句柄就直接返回，所以这里必须先删 ——
    /// 和 `docktabs::set_dpi` 是同一套动作，只是那条路本身就是「换 DPI」。
    ///
    /// 只在窗口活着时做得了：`relayout` 要 `GetClientRect`，窗口没建之前那是
    /// 零矩形，量出来的宽度全是 0。
    pub(super) unsafe fn refresh_fonts(&mut self, dpi: u32) {
        if !self.alive() {
            return;
        }
        for font in [self.font, self.bold] {
            if !font.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
        self.font = HFONT::default();
        self.bold = HFONT::default();
        self.build_fonts(dpi);
        // 行高、每格宽度、命中矩形全部跟着新字体走一遍 —— 和 `create` 里那两行
        // 是同一件事，只是那时窗口还没建。
        self.metrics = Metrics::at(dpi, self.tm_height());
        self.relayout();
        let _ = InvalidateRect(Some(self.hwnd), None, false);
    }

    pub(super) unsafe fn relayout(&mut self) {
        if !self.alive() {
            return;
        }
        let mut rc = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut rc);
        self.client = rc;
        let mut widths = Vec::with_capacity(self.entries.len());
        for (i, e) in self.entries.iter().enumerate() {
            // the tab scroll handler: the text is the label with `" *"`
            // appended whenever `ShowMark` is on — the mark is reserved for
            // every tab, not only the modified ones, which is why dirtying a
            // document does not move its neighbours.
            let text = if self.settings.show_mark {
                format!("{} *", e.label)
            } else {
                e.label.clone()
            };
            let textw = self.measure(&text, i == self.active);
            widths.push(tab_width(
                textw,
                &self.metrics,
                self.settings.show_icon,
                self.settings.show_mark,
                // The tab scroll handler charges the ✕ cell to the width whenever
                // the close button is on: the cell is reserved in every tab's
                // width, whether or not the tab is wide enough to reach it.
                if self.settings.item_close { self.metrics.close } else { 0 },
            ));
        }
        let (right, left, drop, band_right) = tail_rects(
            rc.right,
            &self.metrics,
            self.settings.scroll,
            self.settings.dropdown,
        );
        self.tail_right = right;
        self.tail_left = left;
        self.tail_drop = drop;
        self.band_right = band_right;
        // The tail cells are centred in the band, 1 px off the top and 2 off the
        // bottom for the border; the centre is that band's midpoint taken back by
        // half a cell.
        self.tail_top = (rc.top + 1 + (rc.bottom - 2) - self.metrics.cell) / 2;
        self.tail_bottom = self.tail_top + self.metrics.cell;
        let (rects, content) = layout_rects(&widths, self.metrics.inset);
        self.rects = rects;
        self.close_state.resize(self.rects.len(), CLOSE_NORMAL);
        self.content = content;
        self.xoff = clamp_xoff(self.xoff, content, band_right);
        let (can_left, can_right) = scroll_state(self.xoff, content, band_right);
        self.can_left = can_left;
        self.can_right = can_right;
    }

    /// `DrawTextW(…, 0xC20)` = `DT_CALCRECT|DT_EDITCONTROL|DT_SINGLELINE` over
    /// the first 32 characters, bold for the selected tab.
    unsafe fn measure(&self, label: &str, selected: bool) -> i32 {
        let mut buf: Vec<u16> = label.encode_utf16().take(32).collect();
        if buf.is_empty() {
            return 0;
        }
        let hdc = GetDC(None);
        let font = if selected { self.bold } else { self.font };
        let saved = SelectObject(hdc, HGDIOBJ(font.0));
        let mut rc = RECT::default();
        let _ = DrawTextW(
            hdc,
            &mut buf,
            &mut rc,
            DT_CALCRECT | DT_EDITCONTROL | DT_SINGLELINE,
        );
        SelectObject(hdc, saved);
        ReleaseDC(None, hdc);
        rc.right.max(0)
    }

    pub(super) unsafe fn invalidate(&self) {
        if self.alive() {
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
    }

    fn hit(&self, x: i32) -> Hit {
        hit_test(
            x,
            &self.rects,
            self.xoff,
            self.tail_right,
            self.tail_left,
            self.tail_drop,
        )
    }

    /// The tab under the point, which is the tab item index's item index: `-1` for
    /// the band with no tab under it.
    fn tab_at(&self, x: i32) -> Option<usize> {
        match self.hit(x) {
            Hit::Tab(i) => Some(i),
            _ => None,
        }
    }

    /// A press: what it means, after the offset has been moved and the layout
    /// re-run for a scroll button — the hit test does exactly that between
    /// marking the cell and arming the timer. Each tail button marks itself
    /// pressed *and* hovered in one step, while a tab marks only the pressed
    /// band; its hovered cell is whatever the moves left there. Every branch that
    /// lands in a cell ends at the same tail, which takes the mouse capture; the
    /// one with no tab under the point (the tab item index answering -1) does not
    /// get there.
    unsafe fn press(&mut self, x: i32, y: i32) -> Press {
        let p = match self.hit(x) {
            Hit::Right => {
                self.pressed = PRESS_RIGHT;
                self.inside_cell = PRESS_RIGHT;
                if self.can_right {
                    self.scroll_by(-self.metrics.step);
                    Press::Scroll(TIMER_RIGHT)
                } else {
                    Press::None
                }
            }
            Hit::Left => {
                self.pressed = PRESS_LEFT;
                self.inside_cell = PRESS_LEFT;
                if self.can_left {
                    self.scroll_by(self.metrics.step);
                    Press::Scroll(TIMER_LEFT)
                } else {
                    Press::None
                }
            }
            Hit::Dropdown => {
                self.pressed = PRESS_DROP;
                self.inside_cell = PRESS_DROP;
                Press::Dropdown
            }
            Hit::Tab(i) => {
                self.pressed = CELL_BAND;
                // The press keeps its tab and its x: the drag arms when a later
                // move's x leaves this one while the band is still down. A ✕
                // press records them too — dragging away from an armed ✕ moves
                // the tab and the release, no longer over the cell, closes
                // nothing.
                self.press_item = Some(i);
                self.press_x = x;
                // The tab's own ✕ cell is tested before any activation, and an
                // armed cell means `SetCurSel` is skipped — pressing a ✕ must not
                // select the tab it sits on.
                if self.arm_close(i, x, y) {
                    Press::Activate(i)
                } else {
                    Press::None
                }
            }
            Hit::None => return Press::None,
        };
        if self.alive() {
            SetCapture(self.hwnd);
        }
        p
    }

    unsafe fn scroll_by(&mut self, delta: i32) {
        self.xoff += delta;
        self.relayout();
        self.invalidate();
    }

    /// A wheel notch: the same step, in the direction the delta says.
    pub(super) unsafe fn wheel_scroll(&mut self, forward: bool) {
        if !self.settings.scroll && !self.settings.dropdown {
            return;
        }
        let step = self.metrics.step;
        self.scroll_by(if forward { step } else { -step });
    }

    fn release(&mut self) {
        self.pressed = PRESS_NONE;
        self.repeat = 0;
    }

    /// A release, as the capture release reads it. A cell only answers when its
    /// pressed *and* its cursor-inside nibble both name it. The strip repaints
    /// whole either way — the shared tail of both answers. A release that ends
    /// a drag answers nothing at all: the ✕ and ▾ machines are spent once the
    /// press went to the drag.
    unsafe fn release_at(&mut self, x: i32, y: i32) -> Release {
        if self.drag_from.is_some() {
            self.end_drag();
            self.release();
            self.relayout();
            self.invalidate();
            return Release::Nothing;
        }
        self.press_item = None;
        let rel = if self.pressed == PRESS_DROP && self.inside_cell == PRESS_DROP {
            Release::Dropdown
        } else {
            match self.tab_at(x) {
                Some(i) if self.release_close(i, x, y) => Release::Close(i),
                _ => Release::Nothing,
            }
        };
        self.release();
        self.relayout();
        self.invalidate();
        rel
    }

    /// The cell a point is in, by the state nibbles' numbering: a tail button, the tab
    /// band, or `PRESS_NONE` for the strip's dead area past the band. This is
    /// the tab item index's answer minus the item index.
    fn cell_at(&self, x: i32) -> u8 {
        match self.hit(x) {
            Hit::Right => PRESS_RIGHT,
            Hit::Left => PRESS_LEFT,
            Hit::Dropdown => PRESS_DROP,
            Hit::Tab(_) => CELL_BAND,
            Hit::None if x < self.band_right => CELL_BAND,
            Hit::None => PRESS_NONE,
        }
    }

    /// The ✕ cell of one tab, in client coordinates: the close-button state machine takes the
    /// item's own rect (which the layout makes the row's full
    /// height), offsets it by the scroll position, and pulls its left edge
    /// in to `right - cyWidth - 4`.
    fn close_cell(&self, index: usize) -> Option<RECT> {
        let r = self.rects.get(index).copied()?;
        Some(RECT {
            left: r.right + self.xoff - self.metrics.close - 4,
            top: self.client.top,
            right: r.right + self.xoff,
            bottom: self.client.bottom,
        })
    }

    /// Whether a point is in that tab's ✕ cell — the `PtInRect` of the state
    /// machine.
    fn close_hit(&self, index: usize, x: i32, y: i32) -> bool {
        self.close_cell(index)
            .is_some_and(|rc| unsafe { PtInRect(&rc, POINT { x, y }).as_bool() })
    }

    /// the close-button state machine's move branch: every tab's ✕ goes back to `2` and the one
    /// under the cursor takes `3`. It costs no repaint of its own — the caller's
    /// invalidate of the item rect is what redraws the glyph.
    fn track_close(&mut self, index: usize, x: i32, y: i32) {
        if !self.settings.item_close {
            return;
        }
        for s in self.close_state.iter_mut() {
            *s = CLOSE_NORMAL;
        }
        if self.close_hit(index, x, y) {
            if let Some(s) = self.close_state.get_mut(index) {
                *s = CLOSE_HOT;
            }
        }
    }

    /// the close-button state machine's down branch. The tab hit test then skips the close
    /// press handler — no `SetCurSel`, so pressing a ✕ does not select the tab it sits on.
    unsafe fn arm_close(&mut self, index: usize, x: i32, y: i32) -> bool {
        self.close_armed = false;
        if !self.settings.item_close || !self.close_hit(index, x, y) {
            return true;
        }
        if let Some(rc) = self.close_cell(index) {
            let _ = InvalidateRect(Some(self.hwnd), Some(&rc), true);
        }
        self.close_armed = true;
        false
    }

    /// the close-button state machine's up branch: an armed cell the cursor is still inside
    /// closes its tab. The arming byte goes on every path that reaches it, which
    /// is why a release outside the cell leaves nothing armed for the next one.
    fn release_close(&mut self, index: usize, x: i32, y: i32) -> bool {
        let armed = self.close_armed;
        self.close_armed = false;
        armed && self.close_hit(index, x, y)
    }

    /// The repaint a cell's state change costs. A tab's band covers the whole
    /// strip, which is the tab promote's own `InvalidateRect(hwnd, 0, 1)`.
    unsafe fn invalidate_cell(&self, cell: u8) {
        let rect = match cell {
            PRESS_RIGHT => self.tail_right,
            PRESS_LEFT => self.tail_left,
            PRESS_DROP => self.tail_drop,
            _ => None,
        };
        match rect {
            Some(r) => {
                let _ = InvalidateRect(
                    Some(self.hwnd),
                    Some(&RECT {
                        left: r.left,
                        top: 0,
                        right: r.right,
                        bottom: self.tail_bottom,
                    }),
                    true,
                );
            }
            None => self.invalidate(),
        }
    }

    /// The hovered tab's rect, shifted into client space by the scroll offset
    /// before invalidating
    unsafe fn invalidate_item(&self, index: usize) {
        let Some(r) = self.rects.get(index).copied() else {
            self.invalidate();
            return;
        };
        let _ = InvalidateRect(
            Some(self.hwnd),
            Some(&RECT {
                left: r.left + self.xoff,
                top: 0,
                right: r.right + self.xoff,
                bottom: self.tail_bottom,
            }),
            true,
        );
    }

    /// Leaving the hover nibble: the cell it was on is repainted, and a tab's
    /// hover also clears the hovered item, which is what takes its orange band away.
    unsafe fn clear_hover(&mut self) {
        if self.hover_cell == PRESS_NONE {
            return;
        }
        let old = self.hover_cell;
        self.hover_cell = PRESS_NONE;
        if old == CELL_BAND {
            self.hot_item = -1;
        }
        self.invalidate_cell(old);
    }

    /// Leaving the cursor-inside nibble.
    unsafe fn clear_inside(&mut self) {
        if self.inside_cell == PRESS_NONE {
            return;
        }
        let old = self.inside_cell;
        self.inside_cell = PRESS_NONE;
        self.invalidate_cell(old);
    }

    /// A move, as the tab promote does it. A move during a drag never reaches
    /// the promote: the drag owns the message, and its answer — `Some` — is the
    /// move the frame's document list has to follow.
    ///
    /// Both nibbles move one message apart: a cell the cursor enters only takes
    /// the inside nibble, and it is the *next* move within that cell, with no
    /// button held
    /// lag, and it is why a glyph does not go black on the entry message.
    unsafe fn mouse_move(&mut self, x: i32, y: i32) -> Option<(usize, usize)> {
        if !self.tracking {
            // The leave-pending flag. The leave notification is what ends a hover the
            // cursor left without a message for, and arming it costs a relayout
            // and a whole-strip repaint
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            if TrackMouseEvent(&mut tme).is_ok() {
                self.tracking = true;
                self.relayout();
                self.invalidate();
            }
        }
        // The drag arms on the move whose x first leaves the press's: a press
        // that only jiggles vertically, or has not moved at all, is still a
        // click waiting for its release. The arm keeps the capture the press
        // already took, and the hovered tab keeps its band — the promote below
        // is skipped for every message the drag answers.
        if self.drag_from.is_none()
            && self.tracking
            && self.pressed == CELL_BAND
            && x != self.press_x
            && let Some(item) = self.press_item
        {
            self.drag_from = Some(item);
        }
        if self.drag_from.is_some() {
            return self.drag_move(x, y);
        }
        let cell = self.cell_at(x);
        // Both of its nibbles go there (the state word cleared of those nibbles, then a
        // repaint of that cell's rect). So a message that leaves a hovered cell always
        // takes the hover with it — the hover never outlives the cell it was painted on.
        if cell != self.hover_cell {
            self.clear_hover();
        }
        match cell {
            CELL_BAND => {
                if self.inside_cell != CELL_BAND {
                    self.clear_inside();
                    self.inside_cell = CELL_BAND;
                    return None;
                }
                self.clear_hover();
                if let Some(i) = self.tab_at(x) {
                    self.hover_cell = CELL_BAND;
                    self.hot_item = i as i32;
                    self.track_close(i, x, y);
                    self.invalidate_item(i);
                }
            }
            PRESS_NONE => {
                if self.inside_cell == CELL_BAND {
                    self.inside_cell = PRESS_NONE;
                    self.invalidate();
                } else {
                    self.clear_inside();
                }
            }
            _ => {
                if self.inside_cell == cell {
                    if self.pressed == PRESS_NONE && self.hover_cell != cell {
                        self.hover_cell = cell;
                        self.invalidate_cell(cell);
                    }
                } else {
                    self.clear_inside();
                    self.inside_cell = cell;
                    if self.pressed == cell {
                        self.invalidate_cell(cell);
                    }
                }
            }
        }
        None
    }

    /// A drag's move. The cursor declares itself on every message — the
    /// arrow the class hands back through `WM_SETCURSOR` would otherwise win
    /// the next one. Hovering another tab only trades with it once the cursor
    /// is past that tab's midpoint, in the direction of the travel, and the
    /// two 20 px bands at the edges scroll while their buttons have somewhere
    /// to go, on the flat 100 ms the drag restarts with every move. Outside
    /// this strip only the other strip counts as somewhere to go — that one
    /// moves the tab into its group — so it gets the same arrow; anywhere
    /// else the drag has no answer. The `Some` is the move for the frame's
    /// document list.
    unsafe fn drag_move(&mut self, x: i32, y: i32) -> Option<(usize, usize)> {
        let inside =
            PtInRect(&self.client, POINT { x, y }).as_bool() || self.over_other(x, y);
        let (drag, nodrop) = self.cursors();
        let _ = SetCursor(Some(if inside { drag } else { nodrop }));
        let Some(cur) = self.press_item else {
            return None;
        };
        let mut action = None;
        if let Some(i) = self.tab_at(x) {
            if i != cur {
                if let Some(r) = self.rects.get(i) {
                    let mid = (r.left + r.right) / 2 + self.xoff;
                    if (cur < i && x > mid) || (cur > i && x < mid) {
                        self.move_item(cur, i);
                        self.press_item = Some(i);
                        action = Some((cur, i));
                    }
                }
            }
        }
        if self.settings.scroll {
            if x < self.client.left + 20 && self.can_left {
                self.scroll_by(self.metrics.step);
                let _ = SetTimer(Some(self.hwnd), TIMER_LEFT, REPEAT_FIRST_MS, None);
            } else if x > self.client.right - 20 && self.can_right {
                self.scroll_by(-self.metrics.step);
                let _ = SetTimer(Some(self.hwnd), TIMER_RIGHT, REPEAT_FIRST_MS, None);
            }
        }
        action
    }

    /// The strip's half of a drag move: the entry rides with its rect, its ✕
    /// byte and its icon slot, and the layout runs again over the new order.
    /// The ✕ bytes and the icon slots keep whatever length the last layout
    /// left them at, so a test-built strip (or one that has not laid out yet)
    /// permutes only what is there. The document list is the frame's to move —
    /// it hears the same move through the window procedure.
    fn move_item(&mut self, from: usize, to: usize) {
        if from >= self.entries.len() || to >= self.entries.len() || from == to {
            return;
        }
        let e = self.entries.remove(from);
        self.entries.insert(to, e);
        let r = self.rects.remove(from);
        self.rects.insert(to, r);
        if from < self.close_state.len() && to < self.close_state.len() {
            let s = self.close_state.remove(from);
            self.close_state.insert(to, s);
        }
        if from < self.icons.len() && to < self.icons.len() {
            let ic = self.icons.remove(from);
            self.icons.insert(to, ic);
        }
        unsafe {
            self.relayout();
            self.invalidate();
        }
    }

    /// The drag's two cursors, loaded once: the four-headed arrow where the
    /// tab has somewhere to go, the no-entry sign where it does not. The
    /// system's own shapes — the strip carries no cursor sheet to hand
    /// `LoadCursor` a custom glyph from.
    unsafe fn cursors(&mut self) -> (HCURSOR, HCURSOR) {
        if self.drag_cur.0.is_null() {
            self.drag_cur = LoadCursorW(None, IDC_SIZEALL).unwrap_or_default();
        }
        if self.nodrop_cur.0.is_null() {
            self.nodrop_cur = LoadCursorW(None, IDC_NO).unwrap_or_default();
        }
        (self.drag_cur, self.nodrop_cur)
    }

    /// 另一条页签条的窗口。两条页签条各记对方，摆完那一步更新：第二条的窗
    /// 口可能刚创建，之前记下的还是空的。
    pub(super) fn set_other(&mut self, hwnd: HWND) {
        self.other = hwnd;
    }

    /// 这一点（这一条的客户坐标）落在另一条页签条上吗。两条页签条各占哪里
    /// 只有系统知道，所以先把这一点搬到屏幕，再和对方的窗口矩形比。
    unsafe fn over_other(&self, x: i32, y: i32) -> bool {
        if self.other.0.is_null() {
            return false;
        }
        let mut at = POINT { x, y };
        let mut rect = RECT::default();
        if ClientToScreen(self.hwnd, &mut at).0 == 0 {
            return false;
        }
        if GetWindowRect(self.other, &mut rect).is_err() {
            return false;
        }
        at.x >= rect.left && at.x < rect.right && at.y >= rect.top && at.y < rect.bottom
    }

    /// Whether a drag is on. The window procedure reads it to keep
    /// `WM_SETCURSOR` and the scroll ramp off the drag's back.
    fn dragging(&self) -> bool {
        self.drag_from.is_some()
    }

    /// The drag's common tail: the state goes home and the arrow cursor comes
    /// back — the class registered this strip with one, so that is the shape
    /// the next `WM_SETCURSOR` would settle on anyway.
    unsafe fn end_drag(&mut self) {
        if self.drag_from.is_none() {
            return;
        }
        self.drag_from = None;
        self.press_item = None;
        let _ = SetCursor(LoadCursorW(None, IDC_ARROW).ok());
    }

    /// The capture went elsewhere mid-drag: the tab snaps back to where the
    /// drag picked it up — selection included, which is why the move comes
    /// back for the frame's document list — and the drag is over.
    unsafe fn cancel_drag(&mut self) -> Option<(usize, usize)> {
        let from = self.drag_from.take()?;
        let cur = self.press_item.take()?;
        self.release();
        let undone = if cur != from {
            self.move_item(cur, from);
            Some((cur, from))
        } else {
            None
        };
        let _ = SetCursor(LoadCursorW(None, IDC_ARROW).ok());
        self.invalidate();
        undone
    }

    /// The strip repaints whole. The pressed nibble survives: the release
    /// still holds the capture and reads it, so a ▾ pressed while the cursor
    /// stepped out of the window still opens the list on its way up.
    unsafe fn mouse_leave(&mut self) {
        self.tracking = false;
        self.hover_cell = PRESS_NONE;
        self.inside_cell = PRESS_NONE;
        self.hot_item = -1;
        self.invalidate();
    }

    /// The ramp's next delay: 100 ms for the first repeat, then 25, then 10.
    fn next_repeat(&mut self) -> u32 {
        self.repeat += 1;
        match self.repeat {
            0..=1 => REPEAT_FIRST_MS,
            2 => REPEAT_FAST_MS,
            _ => REPEAT_FASTEST_MS,
        }
    }

    /// There is no divider column to paint. The bar row handler gives each tab its own
    /// `#919B9C` left and right verticals, so a boundary is simply where two tabs' lines fall
    /// next to each other.
    unsafe fn paint(&mut self, hdc: HDC, theme: &Theme) {
        let _stats = super::paintstats::begin(super::paintstats::Region::TabStrip);
        let mut rc = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut rc);
        // Under `[Tab] Style=0` it is the rebar background; in this implementation's
        // dark theme that is a flat `tab_bg`.
        FillRect(hdc, &rc, super::gdicache::brush(to_colorref(theme.tab_bg)));
        let _ = SetBkMode(hdc, TRANSPARENT);

        // The frames are drawn with the DC pen, so one select covers the whole
        // pass and only the state band has to change the colour.
        let saved_pen = SelectObject(hdc, GetStockObject(DC_PEN));
        SetDCPenColor(hdc, to_colorref(theme.tab_frame));
        for i in 0..self.rects.len() {
            if i != self.active {
                self.paint_one(hdc, i, &rc, theme);
            }
        }
        self.paint_one(hdc, self.active, &rc, theme);
        SelectObject(hdc, saved_pen);
        self.paint_tail(hdc, theme);
    }

    /// 这一格的状态位。
    ///
    /// 绘制（配方 `states.*`）、点击、悬停三处读同一个函数 —— 皮肤里写的
    /// `[style.tab.states.active]` 和脚本 `ctx.state.active` 必须是同一个位，
    /// 否则同一格在配方里算「活动」、在脚本里算「常态」，作者没法解释。
    fn tab_state(&self, index: usize) -> dg_skin::State {
        let mut state = dg_skin::State::normal();
        if self.hot_item >= 0 && self.hot_item as usize == index {
            state = state.with(dg_skin::State::HOT);
        }
        if index == self.active {
            state = state.with(dg_skin::State::ACTIVE);
        }
        if self.entries.get(index).is_some_and(|e| e.modified) {
            state = state.with(dg_skin::State::MODIFIED);
        }
        state
    }

    /// 这一格交给脚本看的那份上下文：绘制和点击共用，免得两处算得不一样。
    fn tab_input(
        &self,
        index: usize,
        left: i32,
        right: i32,
        top: i32,
        bottom: i32,
    ) -> Option<dg_script::PaintInput> {
        let entry = self.entries.get(index)?;
        let dpi = self.metrics.dpi as i32;
        let scale = if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 };
        let state = self.tab_state(index).bits();
        Some(dg_script::PaintInput {
            rect: (
                left as f32 / scale,
                top as f32 / scale,
                (right - left) as f32 / scale,
                (bottom - top) as f32 / scale,
            ),
            // 页签里没有会动的部件，`ctx.thumb` 与 `ctx.rect` 同值。
            thumb: (
                left as f32 / scale,
                top as f32 / scale,
                (right - left) as f32 / scale,
                (bottom - top) as f32 / scale,
            ),
            bar: (0.0, 0.0, 0.0, 0.0),
            scroll: 0.0,
            state,
            part: "tab".to_string(),
            text: entry.label.clone(),
            label: String::new(),
            value: String::new(),
            dpi,
            // 缓存键的区分位：第几个页签。页签标题在 `text` 里，键里放不下，
            // 序号先把它们分开。
            index: index.min(u16::MAX as usize) as u16,
        })
    }

    /// 这一格的点击上下文，交给 frame 那边派发。
    ///
    /// 派发不在这里做：点击回调是 Intent 档，要装 bridge，而 bridge 借的是
    /// `&mut EditorState` —— 页签条只有 `&self`。窗口过程手里有 `st`，它调。
    pub(super) fn press_input(&self, index: usize) -> Option<dg_script::PaintInput> {
        let r = self.rects.get(index).copied()?;
        let left = r.left + self.xoff;
        let right = r.right + self.xoff;
        self.tab_input(index, left, right, self.client.top, self.client.bottom)
    }

    /// 第 `index` 个页签的悬停上下文和落点。
    ///
    /// 上下文就是 [`press_input`](Self::press_input) 那一份 —— 绘制、点击、悬停
    /// 三处读同一个函数，`@text` 展开的永远是画在格子里的标题。
    pub(super) fn hint_at(
        &self,
        index: usize,
    ) -> Option<(dg_script::PaintInput, windows::Win32::Foundation::POINT)> {
        let r = self.rects.get(index).copied()?;
        let input = self.press_input(index)?;
        Some((
            input,
            crate::win32::skinhint::under(r.left + self.xoff, self.client.bottom),
        ))
    }

    /// 让皮肤画这一格。`false` = 内置画法接着画。
    ///
    /// 状态位给的是**这一格自己的**：悬停 / 选中 / 改过。`ctx.text` 是页签
    /// 标题（不带 `*` 标记，那个标记是 `modified` 位的事，脚本自己决定怎么画）。
    unsafe fn paint_by_script(
        &self,
        hdc: HDC,
        index: usize,
        left: i32,
        right: i32,
        top: i32,
        bottom: i32,
    ) -> bool {
        let Some(host) = &self.script else {
            return false;
        };
        if !host.has_paint(SKIN_REGION) {
            return false;
        }
        let Some(entry) = self.entries.get(index) else {
            return false;
        };
        let dpi = self.metrics.dpi as i32;
        let Some(input) = self.tab_input(index, left, right, top, bottom) else {
            return false;
        };
        let Some(out) = host.paint(SKIN_REGION, &input) else {
            return false;
        };
        if !out.handled {
            return false;
        }
        let ops = out.compacted();
        // 字体表只有第 0 格：这条页签条自己的字体（选中页签用的是粗体，
        // 脚本要粗体得等 M3 的字体 token）。
        let fonts = [HFONT(self.font.0)];
        let ctx = super::skinpaint::ReplayCtx {
            fonts: &fonts,
            images: &[],
            strings: &out.strings,
            points: &out.points,
            text: &entry.label,
            label: "",
            value: "",
            dpi,
        };
        let _ = super::skinpaint::replay(hdc, ops.as_slice(), &ctx);
        true
    }

    /// One tab, skipped when it has scrolled out of the band editor relies on its
    /// clip rect for the same thing. Shifts it into place with a viewport origin
    /// rather than by moving the rect. The item's vertical extent is the whole
    /// row, so the client rect the strip is sized to *is* the item's top and
    /// bottom — which is all `TabRect` needs to record.
    unsafe fn paint_one(&mut self, hdc: HDC, index: usize, rc: &RECT, theme: &Theme) {
        let Some(r) = self.rects.get(index).copied() else {
            return;
        };
        let left = r.left + self.xoff;
        let right = r.right + self.xoff;
        if right < rc.left || left > rc.right {
            return;
        }
        // 皮肤接管这一格：它画完就到此为止，内置的框和文字都让位。
        if self.paint_by_script(hdc, index, left, right, rc.top, rc.bottom) {
            return;
        }
        // 配方只读一次：同一格的两半（框、文字）必须用同一份形状，否则
        // `states.hot` 里的两半会在悬停那一帧各算各的。
        let shape = CellShape::of(
            &crate::skin::shape("tab", self.tab_state(index)),
            self.metrics.dpi as u32,
        );
        self.paint_tab(hdc, index, left, right, rc.top, rc.bottom, theme, &shape);
        self.paint_label(hdc, index, left, right, rc.top, rc.bottom, theme, &shape);
    }

    /// A tab's body and frame — the bar row handler's tabs-on-top branch.
    ///
    /// The fill sits two rows below the top and two columns short of the right
    /// edge, and runs past the client's bottom: the top inset clears the frame's
    /// top line, and the overshoot closes the row below. An unselected tab leaves
    /// the band showing, which is what
    /// its own band fill amounts to here.
    ///
    /// 配方四个键都落在这里：`gap` 把整格（框和填充一起）左右各缩一半、`border`
    /// 是框线宽度、`radius` 是选中页签填充块的圆角、`pad_y` 只作用在文字上
    /// （见 `paint_label`）。四个键的出厂值让这里的几何和以前逐像素一致 ——
    /// 这是「空皮肤 0 像素」那条验收线的落点。
    unsafe fn paint_tab(
        &self,
        hdc: HDC,
        index: usize,
        left: i32,
        right: i32,
        top: i32,
        bottom: i32,
        theme: &Theme,
        shape: &CellShape,
    ) {
        let left = left + shape.left_inset;
        let right = right - shape.right_inset;
        let inner_top = shape.inner_top(top);
        let inner_right = shape.inner_right(right);
        // 填充和（圆角时的）框都画在这个矩形上，两者的角才对得上。
        let body = shape.body(left, top, right, bottom);

        if index == self.active {
            super::gdicache::fill_round(hdc, &body, to_colorref(theme.tab_active_bg), shape.radius);
        }
        if shape.radius > 0 {
            // 圆角时框也得磨圆：只磨填充、框还是方的，四个角上会露出一圈底色，
            // 看着像画歪了。用真正的画笔（`pen` 缓存里那支）画 `border` 宽的空心圆角矩形。
            let saved_pen = SelectObject(
                hdc,
                HGDIOBJ(super::gdicache::pen(PS_SOLID, shape.border, to_colorref(theme.tab_frame)).0),
            );
            let saved_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
            let _ = RoundRect(
                hdc,
                body.left,
                body.top,
                body.right,
                body.bottom,
                shape.radius * 2,
                shape.radius * 2,
            );
            SelectObject(hdc, saved_brush);
            SelectObject(hdc, saved_pen);
        } else {
            // 三条线各画 `border` 遍：`border = 1` 就是原来那三条。
            for i in 0..shape.border {
                let _ = MoveToEx(hdc, left + i, inner_top, None);
                let _ = LineTo(hdc, left + i, bottom);
                let _ = MoveToEx(hdc, inner_right + i, inner_top, None);
                let _ = LineTo(hdc, inner_right + i, bottom);
                let _ = MoveToEx(hdc, left + 1, top + 1 + i, None);
                let _ = LineTo(hdc, inner_right, top + 1 + i);
            }
        }

        // The state band, across the tab's top edge. The bar row handler picks it
        // from the item's state value — 1 red, 2 green, 3 grey — and a
        // hovered tab overrides it with orange for *any* tab, selected or not and
        // modified or not. This implementation only sets state 1, for a modified document.
        let hot = self.hot_item >= 0 && self.hot_item as usize == index;
        if hot || self.entries.get(index).is_some_and(|e| e.modified) {
            SetDCPenColor(
                hdc,
                to_colorref(if hot {
                    theme.tab_hot_band
                } else {
                    theme.tab_mark_fg
                }),
            );
            let _ = MoveToEx(hdc, left + 1, top + 1, None);
            let _ = LineTo(hdc, inner_right, top + 1);
            let _ = MoveToEx(hdc, left, top + 2, None);
            let _ = LineTo(hdc, inner_right + 1, top + 2);
            SetDCPenColor(hdc, to_colorref(theme.tab_frame));
        }
    }

    /// One tab's icon cell and label — the tab item painter's order.
    ///
    /// `&mut` for the icon cache: the first paint of a tab asks the shell for
    /// its small icon and every later one reads the handle back.
    ///
    /// 配方只碰 `pad_y`（文字框上下各缩进，出厂 0 = 原来那个 `top + 3`）和
    /// `gap`（文字跟着填充块一起缩，免得空格之后字还压到隔壁）。`font` 还没
    /// 接线 —— 换字体要连带重算页签宽度，那是 M3 的 `remeasure_at`。
    unsafe fn paint_label(
        &mut self,
        hdc: HDC,
        index: usize,
        left: i32,
        right: i32,
        top: i32,
        bottom: i32,
        theme: &Theme,
        shape: &CellShape,
    ) {
        let Some(entry) = self.entries.get(index).cloned() else {
            return;
        };
        // 图标和文字都按布局给的位置画，**不跟着 `gap` 挪** —— 格子宽度是
        // `tab_width` 量的，量的时候不知道 `gap`，挪了只会把贴边的标签截掉。
        // `gap` 只用来把文字框夹进块里（见下面的 `fit_text`）。
        let cy = self.metrics.cy;
        let selected = index == self.active;

        // The glyph is drawn only while the tab is wider than the icon cell, but
        // `ShowIcon` costs the cell either way — that is what keeps the labels of
        // narrow tabs starting at the same x.
        if self.settings.show_icon && cy < right - left {
            // The cell's centre: the tab item painter's `(top + 1) / 2 + (top + bottom)
            // / 2`, then up half a glyph and down one for the frame.
            let mid = (top + 1) / 2 + (top + bottom) / 2;
            if let Some(icon) = self.icon_of(index) {
                let _ = DrawIconEx(
                    hdc,
                    left + shape.left_inset + self.metrics.extra,
                    mid - cy / 2 + 1,
                    HICON(icon as *mut core::ffi::c_void),
                    cy,
                    cy,
                    0,
                    None,
                    DI_NORMAL,
                );
            }
        }

        let mut text = RECT {
            left: left
                + self.metrics.extra
                + if self.settings.show_icon { cy } else { 0 }
                + self.metrics.pad,
            top: top + 3 + shape.pad_y,
            // the tab item painter: under `style & 0x20` the label loses the ✕ cell's
            // `cyWidth` from its right edge, so the text never runs under it.
            right: right
                - self.metrics.extra
                - self.metrics.pad
                - if self.settings.item_close { cy } else { 0 },
            bottom: bottom - shape.pad_y,
        };
        // 夹进块里：`gap = 0` 时块比文字框大，四次 max/min 全不生效。
        shape.fit_text(
            &mut text,
            &shape.body(left + shape.left_inset, top, right - shape.right_inset, bottom),
        );
        // The label is skipped when there is no room for it
        // against a stored minimum text width, which nothing ever writes, so
        // the test is just this.
        if text.left >= text.right {
            return;
        }

        let font = if selected { self.bold } else { self.font };
        let saved = SelectObject(hdc, HGDIOBJ(font.0));
        let _ = SetTextColor(
            hdc,
            to_colorref(if selected {
                theme.tab_active_fg
            } else {
                theme.tab_fg
            }),
        );

        let mut buf: Vec<u16> = entry.label.encode_utf16().collect();
        let mut flags = DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
        // Truncation is the word ellipsis, and only past the 32 characters the
        // layout measured: within them a tab always fits its label.
        if buf.len() > 32 {
            flags |= DT_WORD_ELLIPSIS;
        }

        // `ShowMark` reserves `" *"` in every tab's width; state 1 spends it, and
        // spends it as a second draw when appending it would push the label out.
        if self.settings.show_mark && entry.modified {
            let mut mark: Vec<u16> = " *".encode_utf16().collect();
            let mut mark_sz = SIZE::default();
            let _ = GetTextExtentPoint32W(hdc, &mark, &mut mark_sz);
            let mut label_sz = SIZE::default();
            let _ = GetTextExtentPoint32W(hdc, &buf, &mut label_sz);
            let room = mark_sz.cx + 2;
            if label_sz.cx + room > text.right - text.left {
                let mut head = text;
                head.right -= room;
                let _ = DrawTextW(hdc, &mut buf, &mut head, flags);
                let mut tail = text;
                tail.left = head.right;
                // `0x24`: the same vertical centring without `DT_NOPREFIX` — the
                // mark carries no ampersand to lose.
                let _ = DrawTextW(hdc, &mut mark, &mut tail, DT_VCENTER | DT_SINGLELINE);
            } else {
                buf.append(&mut mark);
                let _ = DrawTextW(hdc, &mut buf, &mut text, flags);
            }
        } else {
            let _ = DrawTextW(hdc, &mut buf, &mut text, flags);
        }

        // The shared tail of both label paths: every tab carries the ✕, in
        // the cell the layout reserved, drawn `cyWidth` square at
        // `label.right + 2`. The hover glyph needs the hot flag *and* the item's
        // own `3`; holding the left button over it takes the pressed one.
        if self.settings.item_close {
            let hot = self.hot_item as usize == index
                && self.close_state.get(index).copied() == Some(CLOSE_HOT);
            let state = if !hot {
                XState::Normal
            } else if GetKeyState(VK_LBUTTON.0 as i32) < 0 {
                XState::Pressed
            } else {
                XState::Hot
            };
            if let Some(icon) = self.x_glyph(state) {
                // The glyph is a `cy`-square sprite; it is centred in the cell the
                // layout reserved (`tab.close`, or `cy` when the skin leaves it at
                // 0 — the two are equal by default, so this is the same pixel).
                let cell = self.metrics.close;
                let _ = DrawIconEx(
                    hdc,
                    right - shape.right_inset
                        - self.metrics.extra
                        - self.metrics.pad
                        - cell
                        + (cell - cy) / 2
                        + 2,
                    top + (bottom - top - cy) / 2,
                    HICON(icon as *mut core::ffi::c_void),
                    cy,
                    cy,
                    0,
                    None,
                    DI_NORMAL,
                );
            }
        }
        SelectObject(hdc, saved);
    }

    /// The tail glyphs
    /// from all three state nibbles: a cell with nothing to do goes grey,
    /// a held one goes red *only while the cursor is still in it* (a cell held
    /// with the cursor elsewhere falls back to `#808080`), and a hovered one goes
    /// black. ▾ has no disabled state, so it is always live.
    unsafe fn paint_tail(&self, hdc: HDC, theme: &Theme) {
        let saved = SelectObject(hdc, HGDIOBJ(self.marlett.0));
        for (cell, glyph, press, live) in [
            (
                self.tail_drop,
                GLYPH_DROP,
                PRESS_DROP,
                true,
            ),
            (self.tail_left, GLYPH_LEFT, PRESS_LEFT, self.can_left),
            (self.tail_right, GLYPH_RIGHT, PRESS_RIGHT, self.can_right),
        ] {
            let Some(r) = cell else { continue };
            let color = if !live {
                theme.tab_glyph_disabled
            } else if self.pressed == press {
                if self.inside_cell == press {
                    theme.tab_glyph_pressed
                } else {
                    theme.tab_glyph_fg
                }
            } else if self.hover_cell == press {
                theme.tab_glyph_hover
            } else {
                theme.tab_glyph_fg
            };
            let _ = SetTextColor(hdc, to_colorref(color));
            let mut rect = RECT {
                left: r.left,
                top: self.tail_top,
                right: r.right,
                bottom: self.tail_bottom,
            };
            let mut buf: Vec<u16> = glyph.encode_utf16().collect();
            let _ = DrawTextW(
                hdc,
                &mut buf,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
        SelectObject(hdc, saved);
    }

    /// The ✕ strip's one-time load: a WIC scale to the DPI, then
    /// `ImageList_Create` at the `16 * scale` cell and a single
    /// `ImageList_Add` of the whole strip — the body of the image-list fill,
    /// which this implementation already runs for the toolbar.
    unsafe fn build_x_list(&mut self, dpi: u32) {
        let Some(bytes) = x_strip_bytes() else {
            return;
        };
        let Some(strip) = decode_strip(&bytes) else {
            return;
        };
        let Some((w, h, px)) = strip_at_dpi(strip, dpi) else {
            return;
        };
        self.xlist = image_list(&px, w, h, self.metrics.cy, X_CELLS).unwrap_or_default();
    }

    /// Swaps the ✕ glyphs for the icon theme the frame just loaded: the
    /// cached icons go first (they were drawn from the old list), then the
    /// list itself, then a rebuild at the window's own DPI. Only the list
    /// holds a copy of the sheet, so this is the whole swap.
    pub(super) unsafe fn retheme(&mut self) {
        for slot in &mut self.xicons {
            if let Some(icon) = slot.take() {
                let _ = DestroyIcon(HICON(icon as *mut core::ffi::c_void));
            }
        }
        if self.xlist.0 != 0 {
            let _ = ImageList_Destroy(Some(self.xlist));
            self.xlist = HIMAGELIST(0);
        }
        if self.hwnd.0.is_null() {
            return;
        }
        let dpi = GetDpiForWindow(self.hwnd);
        self.build_x_list(dpi);
        let _ = InvalidateRect(Some(self.hwnd), None, true);
    }

    /// The icon for one ✕ state. `ImageList_GetIcon` runs once per state.
    unsafe fn x_glyph(&mut self, state: XState) -> Option<isize> {
        let slot = &mut self.xicons[state as usize];
        if slot.is_none() && self.xlist.0 != 0 {
            let icon = ImageList_GetIcon(self.xlist, state.cell(), IMAGE_LIST_DRAW_STYLE(0));
            if !icon.0.is_null() {
                *slot = Some(icon.0 as isize);
            }
        }
        *slot
    }

    /// The icon of one tab. This implementation has no per-document window, so it goes to the
    /// shell for the same small icon. An untitled buffer has no path and therefore no icon —
    /// the sheet carries no per-type glyph to fall back on.
    unsafe fn icon_of(&mut self, index: usize) -> Option<isize>
    where
        Self: Sized,
    {
        if let Some(slot) = self.icons.get(index) {
            return *slot;
        }
        let path = self
            .entries
            .get(index)
            .and_then(|e| e.path.clone())
            .unwrap_or_default();
        let icon = if path.is_empty() {
            None
        } else {
            let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
            let mut info = SHFILEINFOW::default();
            let ok = SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                Default::default(),
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_SMALLICON,
            );
            if ok != 0 && !info.hIcon.0.is_null() {
                Some(info.hIcon.0 as isize)
            } else {
                None
            }
        };
        self.icons.push(icon);
        icon
    }
}

impl Default for TabStrip {
    fn default() -> Self {
        Self::new()
    }
}

static REGISTER_CLASS: Once = Once::new();

/// The class procedure. The strip owns its paint and its mouse handling; every
/// document action goes back through the frame's state, the way the status strip
/// and the editor view do.
unsafe extern "system" fn tab_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let frame = match GetParent(hwnd) {
        Ok(f) if !f.0.is_null() => f,
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    let Some(st) = state_mut(frame) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    let x = low_word(lparam);
    let y = high_word(lparam);
    // 这条消息落在哪条页签条上：分割后第二分组有自己的页签条窗口，与
    // 主条同类同路，绘制与交互按窗分派，落到分组上的动作各自认领。
    let second = st.panels.tab_win2.alive() && hwnd == st.panels.tab_win2.hwnd();
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if hdc.0.is_null() {
                return LRESULT(0);
            }
            let theme = st.docs.act().theme.clone();
            // The strip repaints whole on every hover change — one move
            // invalidates the cell it left and the one it entered — and
            // painting that straight onto the window DC lets the band fill
            // land before the tabs do, which is what flickers. One memory DC
            // and a single BitBlt at the end is the same double-buffer
            // contract the editor view keeps. The label painter selects its
            // own font per draw, so nothing has to go in here beforehand.
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let w = (rc.right - rc.left).max(1);
            let h = (rc.bottom - rc.top).max(1);
            let mem = CreateCompatibleDC(Some(hdc));
            if !mem.0.is_null() {
                let bmp = CreateCompatibleBitmap(hdc, w, h);
                if !bmp.0.is_null() {
                    let old = SelectObject(mem, HGDIOBJ(bmp.0));
                    let strip = if second {
                        &mut st.panels.tab_win2
                    } else {
                        &mut st.panels.tab_win
                    };
                    strip.paint(mem, &theme);
                    let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
                    let _ = SelectObject(mem, old);
                    let _ = DeleteObject(HGDIOBJ(bmp.0));
                }
                let _ = DeleteDC(mem);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let strip = if second {
                &mut st.panels.tab_win2
            } else {
                &mut st.panels.tab_win
            };
            match strip.press(x, y) {
                Press::Activate(i) => {
                    // 皮肤先看一眼：它接管了这次点击就整个跳过内置动作。
                    let input = if second {
                        st.panels.tab_win2.press_input(i)
                    } else {
                        st.panels.tab_win.press_input(i)
                    };
                    if let Some(input) = input
                        && super::luascript::press(st, frame, "tab", "press", &input)
                    {
                        return LRESULT(0);
                    }
                    // 第二条页签条上的页签归 1 号窗格：点它就是选中那一
                    // 分组的页签，焦点与活动文档跟着过去。
                    if second {
                        st.select_tab(true, i, frame);
                    } else {
                        st.select_tab(false, i, frame);
                    }
                    // A tab click that left the focus here would show no caret at
                    // all.
                    st.focus_editor();
                    LRESULT(0)
                }
            Press::Scroll(id) => {
                let _ = SetTimer(Some(hwnd), id, REPEAT_FIRST_MS, None);
                LRESULT(0)
            }
            Press::Dropdown => {
                // Nothing else: the tab hit test only arms the button, and the
                // list opens on the release, which is the only order that can
                // work — `TrackPopupMenu` cannot be handed a mouse this window
                // still holds.
                LRESULT(0)
            }
            Press::None => LRESULT(0),
            }
        }
        // the capture release runs only while this window still holds the capture it
        // took on the press; a release that arrives without one does nothing.
        // `ReleaseCapture` comes before the nibble tests, as it does there. Killing
        // it here is the same observable end of the ramp.
        WM_LBUTTONUP => {
            if GetCapture().0 == hwnd.0 {
                // 拖到另一条页签条上 = 那一页换到那一头。落点按两条页签条
                // 自己的窗口矩形比（屏幕坐标），不在这里重算一遍 band。
                let dragged = {
                    let strip = if second {
                        &st.panels.tab_win2
                    } else {
                        &st.panels.tab_win
                    };
                    if strip.dragging() {
                        strip.press_item
                    } else {
                        None
                    }
                };
                let into_other = dragged.is_some()
                    && {
                        let strip = if second {
                            &st.panels.tab_win2
                        } else {
                            &st.panels.tab_win
                        };
                        strip.over_other(x, y)
                    };
                // 放捕捉之前先收拖动的尾：ReleaseCapture 会给还握着捕捉的本
                // 窗口送来捕捉易主的通知，拖动那时还挂着，`cancel_drag` 就把
                // 页签搬回了起点，一次拖动等于白拖。
                let rel = if second {
                    st.panels.tab_win2.release_at(x, y)
                } else {
                    st.panels.tab_win.release_at(x, y)
                };
                let _ = ReleaseCapture();
                match rel {
                    Release::Dropdown => st.open_tab_list_popup(second),
                    // 第二条上的 ✕ 关的是那一分组的页签。
                    Release::Close(i) => {
                        if second {
                            st.close_tab(true, i, frame);
                        } else {
                            st.close_tab(false, i, frame);
                        }
                    }
                    Release::Nothing => {}
                }
                if let (Some(i), true) = (dragged, into_other) {
                    if let Some(id) = st.tab_id(second, i) {
                        st.move_tab_group(frame, !second, id);
                    }
                }
            }
            let _ = KillTimer(Some(hwnd), TIMER_LEFT);
            let _ = KillTimer(Some(hwnd), TIMER_RIGHT);
            LRESULT(0)
        }
        WM_TIMER => {
            let id = wparam.0;
            if id != TIMER_LEFT && id != TIMER_RIGHT {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let strip = if second {
                &mut st.panels.tab_win2
            } else {
                &mut st.panels.tab_win
            };
            let step = strip.metrics.step;
            strip.scroll_by(if id == TIMER_RIGHT { -step } else { step });
            // A drag's edge scroll keeps the flat 100 ms it armed with; the
            // buttons' ramp escalates the way it always has.
            let next = if strip.dragging() {
                REPEAT_FIRST_MS
            } else {
                strip.next_repeat()
            };
            let _ = SetTimer(Some(hwnd), id, next, None);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK | WM_MBUTTONDOWN => {
            let hit = if second {
                st.panels.tab_win2.hit(x)
            } else {
                st.panels.tab_win.hit(x)
            };
            if let Hit::Tab(i) = hit {
                if second {
                    st.close_tab(true, i, frame);
                } else {
                    st.close_tab(false, i, frame);
                }
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let hit = if second {
                st.panels.tab_win2.hit(x)
            } else {
                st.panels.tab_win.hit(x)
            };
            if let Hit::Tab(i) = hit {
                // 右键同理：接管了就不切页签、也不弹菜单。
                let input = if second {
                    st.panels.tab_win2.press_input(i)
                } else {
                    st.panels.tab_win.press_input(i)
                };
                if let Some(input) = input
                    && super::luascript::press(st, frame, "tab", "rclick", &input)
                {
                    return LRESULT(0);
                }
                if second {
                    st.select_tab(true, i, frame);
                } else {
                    st.select_tab(false, i, frame);
                }
                st.focus_editor();
                st.open_tab_menu(x, high_word(lparam));
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let strip = if second {
                &mut st.panels.tab_win2
            } else {
                &mut st.panels.tab_win
            };
            let before = strip.hot_item;
            let action = strip.mouse_move(x, y);
            let after = strip.hot_item;
            // 换格了才问皮肤：同一格里每秒几十条移动什么都不做。
            if before != after {
                let host = st.lua.clone();
                let tab = if second {
                    &mut st.panels.tab_win2
                } else {
                    &mut st.panels.tab_win
                };
                let at = if after < 0 {
                    None
                } else {
                    tab.hint_at(after as usize)
                };
                match at {
                    Some((input, anchor)) => tab.hint.retarget(
                        hwnd,
                        frame,
                        host.as_deref(),
                        SKIN_REGION,
                        &input,
                        anchor,
                        &mut st.tip_host,
                    ),
                    None => tab.hint.leave(&mut st.tip_host),
                }
            }
            // 拖着换的位：文档列表搬到同一个次序去，页签行再从那边推回
            // 来。第二条页签条有自己的次序，搬的是它自己的成员表。
            if let Some((from, to)) = action {
                if second {
                    st.move_tab(true, from, to, frame);
                } else {
                    st.move_tab(false, from, to, frame);
                }
            }
            LRESULT(0)
        }
        // 捕捉被别的窗口拿走，拖到一半的页签回到原位 —— strip 自己那半已经
        // 在 `cancel_drag` 里搬完，这边只补文档列表的那一半。
        WM_CAPTURECHANGED => {
            let cancelled = if second {
                st.panels.tab_win2.cancel_drag()
            } else {
                st.panels.tab_win.cancel_drag()
            };
            if let Some((from, to)) = cancelled {
                if second {
                    st.move_tab(true, from, to, frame);
                } else {
                    st.move_tab(false, from, to, frame);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let strip = if second {
                &mut st.panels.tab_win2
            } else {
                &mut st.panels.tab_win
            };
            strip.mouse_leave();
            strip.hint.leave(&mut st.tip_host);
            LRESULT(0)
        }
        // 皮肤声明了 `cursor` 就由它说了算。没声明时照旧交给 `DefWindowProc`
        // （类的光标是箭头）。拖动期间光标由 `drag_move` 每条移动自己申报，
        // `WM_SETCURSOR` 不再改道。
        WM_SETCURSOR if low_word(lparam) == HTCLIENT as i32 => {
            let strip = if second {
                &mut st.panels.tab_win2
            } else {
                &mut st.panels.tab_win
            };
            if strip.dragging() {
                LRESULT(1)
            } else if strip.hint.apply() {
                LRESULT(1)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_MOUSEWHEEL => {
            // Ctrl held walks the tabs, otherwise the notch
            // scrolls the strip. The notch is the high word of `wParam`; the
            // dispatcher hands that same value on, and
            // lParam here is the screen point, not the delta.
            let delta = ((wparam.0 as u32) >> 16) as i16;
            if GetKeyState(VK_CONTROL.0 as i32) < 0 {
                // A notch of zero or less is the branch that steps forward to
                // `+88 + 1`; a positive notch steps back.
                st.cycle_doc(frame, delta <= 0);
            } else if second {
                st.panels.tab_win2.wheel_scroll(delta > 0);
            } else {
                st.panels.tab_win.wheel_scroll(delta > 0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> Metrics {
        // 120 DPI: cyWidth 20, cell 19, row 26.
        Metrics::at(120, 20)
    }

    /// 出厂度量下，页签条和以前一模一样（「空皮肤逐像素一致」在这一层的证据）。
    #[test]
    fn the_factory_metrics_reproduce_the_old_constants() {
        let m = Metrics::with(120, 20, dg_skin::Metrics::new(), dg_skin::MetricId::TabHeight);
        assert_eq!(m.cy, 20);
        assert_eq!(m.cell, 19);
        assert_eq!(m.pad, 4, "was a literal 4 at every DPI");
        assert_eq!(m.extra, 3);
        assert_eq!(m.height, 26);
        assert_eq!(m.close, 20, "0 means the icon cell");
    }

    /// 皮肤设了 `tab.height` / `tab.pad_x` / `tab.close`，页签条跟着变。
    #[test]
    fn skin_metrics_move_the_strip() {
        let mut skin = dg_skin::Metrics::new();
        skin.set(dg_skin::MetricId::TabHeight, 30.0);
        skin.set(dg_skin::MetricId::TabPadX, 10.0);
        skin.set(dg_skin::MetricId::TabClose, 18.0);
        let m = Metrics::with(96, 16, skin, dg_skin::MetricId::TabHeight);
        assert_eq!(m.height, 30, "the height is honoured, not the font's");
        assert_eq!(m.pad, 10);
        assert_eq!(m.close, 18);
        // 高度按 DIP 放大：125% 下 30 → 38（30 * 1.25）。
        let hi = Metrics::with(120, 16, skin, dg_skin::MetricId::TabHeight);
        assert_eq!(hi.height, 38);
        assert_eq!(hi.pad, 10, "pad is a device pixel, not a DIP");
    }

    /// the tab scroll handler's loop body: `3 + cy + textw + 2*4 + (mark ? 2) + 3`.
    #[test]
    fn tab_width_matches_the_layout_loop() {
        let m = m();
        assert_eq!(tab_width(100, &m, true, true, 0), 136);
        assert_eq!(tab_width(100, &m, true, false, 0), 134);
        assert_eq!(tab_width(100, &m, false, false, 0), 114);
        // `style & 0x20` reserves an icon cell per tab for its overlay ✕.
        assert_eq!(tab_width(100, &m, true, true, m.cy), 156);
    }

    /// Tabs are contiguous from the inset, which is what the capture shows: the
    /// first boundary at 136 for a 131-px first tab at 120 DPI.
    #[test]
    fn layout_runs_from_the_inset() {
        let (rects, content) = layout_rects(&[131, 337, 141], 5);
        assert_eq!(rects[0].left, 5);
        assert_eq!(rects[0].right, 136);
        assert_eq!(rects[1].left, 136);
        assert_eq!(rects[2].right, 614);
        assert_eq!(content, 614);
        assert_eq!(layout_rects(&[], 5), (Vec::new(), 5));
    }

    /// ▶ then ◀ then ▾ against the right edge, 2 px off it, each one cell wide —
    /// and the band that is left is what the tabs get.
    #[test]
    fn tail_cells_pack_against_the_right_edge() {
        let m = m();
        let (right, left, drop, band) = tail_rects(1302, &m, true, true);
        assert_eq!(right, Some(TabRect { left: 1281, right: 1300 }));
        assert_eq!(left, Some(TabRect { left: 1262, right: 1281 }));
        assert_eq!(drop, Some(TabRect { left: 1243, right: 1262 }));
        assert_eq!(band, 1243);
        // Scroll=Y, FileList=N: no ▾ cell, and the band ends at ◀'s left edge.
        let (_, left, drop, band) = tail_rects(1302, &m, true, false);
        assert_eq!(drop, None);
        assert_eq!(left.unwrap().left, 1262);
        assert_eq!(band, 1262);
        // Neither: the band is the client minus the border adjustment.
        assert_eq!(tail_rects(1302, &m, false, false).3, 1300);
        // A band narrower than a cell takes no tail at all.
        assert_eq!(tail_rects(10, &m, true, true).3, 10);
    }

    /// An offset inside the range is left exactly as the wheel or ◀ left it.
    #[test]
    fn xoff_clamps_both_ways() {
        // Overflowing content: -10 is already inside the range, and fully
        // scrolled to the end is -40.
        assert_eq!(clamp_xoff(-10, 1300, 1260), -10);
        assert_eq!(clamp_xoff(-999, 1300, 1260), -40);
        assert_eq!(clamp_xoff(0, 1300, 1260), 0);
        // Content that fits: the positive clamp wins, and `B170` sends it to 0,
        // which is the left-aligned short list.
        assert_eq!(clamp_xoff(0, 400, 1260), 0);
        assert_eq!(clamp_xoff(-500, 400, 1260), 0);
    }

    #[test]
    fn scroll_bits_follow_the_offset() {
        assert_eq!(scroll_state(0, 1300, 1260), (false, true));
        assert_eq!(scroll_state(-20, 1300, 1260), (true, true));
        // Scrolled to the end: nothing left to reveal on the right.
        assert_eq!(scroll_state(-40, 1300, 1260), (true, false));
        assert_eq!(scroll_state(0, 400, 1260), (false, false));
    }

    /// the close press handler's lift, in both directions and with the `+292 & 6` gate
    /// open and shut. The 20 px is an *overshoot*, not a margin: coming in from
    /// the left the tab lands 20 px inside the client, and the same sign is added
    /// in the other branch, so neither pass leaves the tab flush with an edge.
    #[test]
    fn a_selected_tab_is_lifted_20_px_inside_the_client_rect() {
        let client = RECT {
            left: 0,
            top: 0,
            right: 128,
            bottom: 26,
        };
        // Wholly off the left edge at -50..-10: 20 px past the left edge, which
        // is an offset the following layout clamps back.
        assert_eq!(
            scroll_into_view(-50, TabRect { left: 0, right: 40 }, &client, true),
            20
        );
        // Overhanging the right edge at 100..140: 20 px short of it.
        assert_eq!(
            scroll_into_view(0, TabRect { left: 100, right: 140 }, &client, true),
            -32
        );
        // Already inside, the offset is left exactly as the wheel put it.
        assert_eq!(
            scroll_into_view(0, TabRect { left: 40, right: 80 }, &client, true),
            0
        );
        // No live scroll button: the whole block is skipped.
        assert_eq!(
            scroll_into_view(-50, TabRect { left: 0, right: 40 }, &client, false),
            -50
        );
    }

    /// The selection is stored before the lift, and an index at or past the item
    /// count is dropped rather than clamped — an index at or past the item count leaves
    /// the previous selection in place.
    #[test]
    fn a_selection_moves_the_offset_and_an_out_of_range_index_does_not() {
        let mut st = scrolled_three();
        unsafe {
            st.set_current(3);
            assert_eq!(st.active, 0);
            assert_eq!(st.xoff, 0);

            st.set_current(1);
            assert_eq!(st.active, 1);
            // 40..80 is inside the 128-px client, so the offset stays where it is
            // even with a live button.
            st.xoff = -60;
            st.can_right = true;
            st.set_current(2);
            assert_eq!(st.active, 2);
            assert_eq!(st.xoff, -60);

            // Tab 0 now sits at -60..-20, wholly off the left edge.
            st.set_current(0);
            assert_eq!(st.active, 0);
            assert_eq!(st.xoff, 20);
        }
    }

    /// Three 40-px tabs in a 128-px client: wide enough for the selected tab to
    /// fall off either edge, and holding the entries the index clamp reads.
    fn scrolled_three() -> TabStrip {
        let mut st = two_tabs();
        st.entries = (0..3)
            .map(|i| TabEntry {
                label: format!("{i}.txt"),
                path: None,
                modified: false,
            })
            .collect();
        st.rects = vec![
            TabRect { left: 0, right: 40 },
            TabRect { left: 40, right: 80 },
            TabRect { left: 80, right: 120 },
        ];
        st.content = 120;
        st.client = RECT {
            left: 0,
            top: 0,
            right: 128,
            bottom: 26,
        };
        st
    }

    /// Two 40-px tabs, the tail cells after them, and dead area from 128 on:
    /// the cell layout the nibble tests walk across.
    fn two_tabs() -> TabStrip {
        let mut st = TabStrip::new();
        st.rects = vec![TabRect { left: 0, right: 40 }, TabRect { left: 40, right: 80 }];
        st.band_right = 80;
        st.tail_left = Some(TabRect {
            left: 80,
            right: 96,
        });
        st.tail_right = Some(TabRect {
            left: 96,
            right: 112,
        });
        st.tail_drop = Some(TabRect {
            left: 112,
            right: 128,
        });
        st
    }

    /// The cell ids the tab promote moves through the state nibbles: the tail
    /// buttons by their own number, the tabs by the band's, and the dead area
    /// past the band by none at all.
    #[test]
    fn cells_follow_the_tail_first_order() {
        let mut st = two_tabs();
        assert_eq!(st.cell_at(10), CELL_BAND);
        assert_eq!(st.cell_at(50), CELL_BAND);
        assert_eq!(st.cell_at(88), PRESS_LEFT);
        assert_eq!(st.cell_at(104), PRESS_RIGHT);
        assert_eq!(st.cell_at(120), PRESS_DROP);
        assert_eq!(st.cell_at(200), PRESS_NONE);
        // A tab scrolled under a tail cell still belongs to the cell.
        st.xoff = -30;
        assert_eq!(st.cell_at(88), PRESS_LEFT);
        assert_eq!(st.cell_at(60), CELL_BAND);
    }

    /// the tab promote's band block: the message that enters the band only takes
    /// the cursor-inside nibble, and the one after it is what writes the hovered item.
    ///
    /// No live run shows that pair on this machine — the session has no
    /// interactive input desktop (`GetCursorPos`, `SetCursorPos` and
    /// `OpenInputDesktop` all give `ERROR_ACCESS_DENIED`), so
    /// `TrackMouseEvent` answers every synthetic move with a `WM_MOUSELEAVE`
    /// before the move after it can arrive. The algebra is asserted here
    /// instead, with bit 0 already armed.
    #[test]
    fn the_band_hovers_one_message_behind_the_cursor() {
        let mut st = two_tabs();
        st.tracking = true;
        unsafe {
            let _ = st.mouse_move(10, 0);
            assert_eq!(st.inside_cell, CELL_BAND);
            assert_eq!(st.hover_cell, PRESS_NONE);
            assert_eq!(st.hot_item, -1);

            let _ = st.mouse_move(11, 0);
            assert_eq!(st.hover_cell, CELL_BAND);
            assert_eq!(st.hot_item, 0);

            // Still the band, so no nibble leaves: only moving past the
            // tab's own width walks into the next item.
            let _ = st.mouse_move(50, 0);
            assert_eq!(st.inside_cell, CELL_BAND);
            assert_eq!(st.hover_cell, CELL_BAND);
            assert_eq!(st.hot_item, 1);

            // Past the band with no cell there: the hit test's false branch
            // drops both.
            let _ = st.mouse_move(200, 0);
            assert_eq!(st.inside_cell, PRESS_NONE);
            assert_eq!(st.hover_cell, PRESS_NONE);
            assert_eq!(st.hot_item, -1);

            // A leave with no message for it clears the same set, bit 0 with it.
            let _ = st.mouse_move(10, 0);
            let _ = st.mouse_move(11, 0);
            assert_eq!(st.hot_item, 0);
            st.mouse_leave();
            assert!(!st.tracking);
            assert_eq!(st.inside_cell, PRESS_NONE);
            assert_eq!(st.hover_cell, PRESS_NONE);
            assert_eq!(st.hot_item, -1);
        }
    }

    /// The fact that a cell's hover never outlives the cell.
    #[test]
    fn a_tail_cell_hovers_only_on_the_move_after_it() {
        let mut st = two_tabs();
        st.tracking = true;
        unsafe {
            let _ = st.mouse_move(104, 0);
            assert_eq!(st.inside_cell, PRESS_RIGHT);
            assert_eq!(st.hover_cell, PRESS_NONE);
            let _ = st.mouse_move(105, 0);
            assert_eq!(st.hover_cell, PRESS_RIGHT);

            // Across to ◀: ▶'s hover goes with the same message.
            let _ = st.mouse_move(88, 0);
            assert_eq!(st.hover_cell, PRESS_NONE);
            assert_eq!(st.inside_cell, PRESS_LEFT);
            let _ = st.mouse_move(89, 0);
            assert_eq!(st.hover_cell, PRESS_LEFT);

            // A hovered tab gives its band back the same way, not one later.
            let _ = st.mouse_move(10, 0);
            let _ = st.mouse_move(11, 0);
            assert_eq!(st.hover_cell, CELL_BAND);
            assert_eq!(st.hot_item, 0);
            let _ = st.mouse_move(104, 0);
            assert_eq!(st.hover_cell, PRESS_NONE);
            assert_eq!(st.hot_item, -1);
            let _ = st.mouse_move(105, 0);
            assert_eq!(st.hover_cell, PRESS_RIGHT);

            // Held down: the message after it no longer promotes.
            st.pressed = PRESS_DROP;
            let _ = st.mouse_move(120, 0);
            assert_eq!(st.inside_cell, PRESS_DROP);
            assert_eq!(st.hover_cell, PRESS_NONE);
            let _ = st.mouse_move(121, 0);
            assert_eq!(st.hover_cell, PRESS_NONE);
        }
    }

    /// Two tabs with a real row and the ✕ switch on: the strip the
    /// close-button state machine runs on. Tab 0 covers 0..40, tab 1 40..80, ◀ is
    /// 80..96, ▶ 96..112 and ▾ 112..128; at this DPI a cell is `20 + 4` wide.
    fn close_strip() -> TabStrip {
        let mut st = two_tabs();
        st.metrics = m();
        st.entries = (0..2)
            .map(|i| TabEntry {
                label: format!("{i}.txt"),
                path: None,
                modified: false,
            })
            .collect();
        st.client = RECT {
            left: 0,
            top: 0,
            right: 128,
            bottom: m().height,
        };
        st.close_state = vec![CLOSE_NORMAL; 2];
        st
    }

    /// `rc.left = rc.right - cyWidth - 4` on the item's own rect, at the
    /// row's full height.
    #[test]
    fn the_close_cell_is_the_tab_s_right_hand_square() {
        let st = close_strip();
        let cy = st.metrics.cy;
        assert_eq!(
            st.close_cell(0).unwrap(),
            RECT {
                left: 40 - cy - 4,
                top: 0,
                right: 40,
                bottom: st.client.bottom
            }
        );
        assert_eq!(st.close_cell(1).unwrap().right, 80);
        // The label half of the tab is not the cell, and the row's bottom edge
        // is exclusive, as `PtInRect` has it.
        assert!(st.close_hit(0, 30, 5));
        assert!(!st.close_hit(0, 10, 5));
        assert!(!st.close_hit(0, 30, st.client.bottom));
        // Scrolled: the cell rides with its tab, not with the strip.
        let mut st = close_strip();
        st.xoff = -30;
        assert_eq!(st.close_cell(1).unwrap().left, 80 - 30 - cy - 4);
        assert_eq!(st.close_cell(1).unwrap().right, 50);
        assert!(st.close_hit(1, 45, 5));
        assert!(!st.close_hit(1, 60, 5));
    }

    /// the close-button state machine's move branch: every tab goes back to `2` and only the
    /// one whose cell holds the cursor takes `3`.
    #[test]
    fn only_the_cell_under_the_cursor_goes_hot() {
        let mut st = close_strip();
        st.track_close(0, 30, 5);
        assert_eq!(st.close_state, vec![CLOSE_HOT, CLOSE_NORMAL]);
        st.track_close(1, 60, 5);
        assert_eq!(st.close_state, vec![CLOSE_NORMAL, CLOSE_HOT]);
        // Onto the tab but off its ✕: nothing is hot anywhere.
        st.track_close(1, 45, 5);
        assert_eq!(st.close_state, vec![CLOSE_NORMAL, CLOSE_NORMAL]);
        // `ItemClose=N`: the whole of the close-button state machine sits behind that style bit,
        // so the machine never runs and no cell is written.
        let mut st = close_strip();
        st.settings.item_close = false;
        st.track_close(1, 60, 5);
        assert_eq!(st.close_state, vec![CLOSE_NORMAL, CLOSE_NORMAL]);
    }

    /// the tab hit test's tab branch: the ✕ is tested before the activation, and
    /// the cell it arms makes the close press handler be skipped — pressing a ✕ must
    /// not select the tab it sits on.
    #[test]
    fn a_press_on_the_close_cell_does_not_activate() {
        let mut st = close_strip();
        unsafe {
            assert_eq!(st.press(10, 5), Press::Activate(0));
            assert!(!st.close_armed);
            assert_eq!(st.press(30, 5), Press::None);
            assert!(st.close_armed);
            assert_eq!(st.pressed, CELL_BAND);
            // A later press re-reads the point.
            assert_eq!(st.press(45, 5), Press::Activate(1));
            assert!(!st.close_armed);
            // Past the last tab the tab item index answers -1, and that branch
            // takes neither the nibble nor the capture.
            st.pressed = PRESS_NONE;
            assert_eq!(st.press(200, 5), Press::None);
            assert_eq!(st.pressed, PRESS_NONE);
        }
    }

    /// the close-button state machine's up branch as the capture release reaches it: only an armed
    /// cell with the cursor still inside it closes, and the arming byte is spent
    /// whichever way the release goes.
    #[test]
    fn a_release_needs_an_armed_cell_and_the_cursor_in_it() {
        let mut st = close_strip();
        unsafe {
            st.press(30, 5);
            assert_eq!(st.release_at(30, 5), Release::Close(0));
            assert!(!st.close_armed);
            st.press(30, 5);
            assert_eq!(st.release_at(10, 5), Release::Nothing);
            assert!(!st.close_armed);
            // A release over a cell that was never armed answers nothing.
            st.press(10, 5);
            assert_eq!(st.release_at(30, 5), Release::Nothing);
            // And a release on the dead area never reaches the state machine at
            // all (the tab item index answering -1 skips the close-button state machine), so the
            // arming byte is left for the next one.
            st.press(30, 5);
            assert_eq!(st.release_at(200, 5), Release::Nothing);
            assert!(st.close_armed);
            assert_eq!(st.release_at(30, 5), Release::Close(0));
        }
    }

    /// ▾ answers only when its pressed *and* its cursor-inside nibble both name
    /// it — the capture release's `(v & 0xF00000) == 0x500000 && (v & 0xF000000) ==
    /// 0x5000000` — and a tab's ✕ is not consulted after a cell has answered.
    #[test]
    fn the_dropdown_needs_both_nibbles() {
        let mut st = close_strip();
        unsafe {
            st.pressed = PRESS_DROP;
            st.inside_cell = CELL_BAND;
            assert_eq!(st.release_at(120, 5), Release::Nothing);
            st.pressed = PRESS_DROP;
            st.inside_cell = PRESS_DROP;
            assert_eq!(st.release_at(120, 5), Release::Dropdown);
            // A band press with ▾'s inside nibble left from an earlier move goes
            // to the ✕ machine instead.
            st.press(30, 5);
            st.inside_cell = PRESS_DROP;
            assert_eq!(st.release_at(30, 5), Release::Close(0));
        }
    }

    /// The drag arms on the move whose x first leaves the press's: a press that
    /// has not moved at all, or only vertically, is still a click waiting for
    /// its release. The ✕ press arms it too — dragging away from an armed ✕
    /// spends the press on the move.
    #[test]
    fn the_drag_arms_when_the_x_leaves_the_press() {
        let mut st = scrolled_three();
        st.tracking = true;
        unsafe {
            assert_eq!(st.press(10, 5), Press::Activate(0));
            assert!(!st.dragging());
            // No x change yet, and a vertical jiggle is not a drag either.
            st.mouse_move(10, 3);
            assert!(!st.dragging());
            st.mouse_move(12, 3);
            assert!(st.dragging());
            // A ✕ press arms the same way.
            let mut st = close_strip();
            st.tracking = true;
            assert_eq!(st.press(30, 5), Press::None);
            st.mouse_move(20, 0);
            assert!(st.dragging());
        }
    }

    /// The trade waits for the neighbour's midpoint, in the direction of the
    /// travel: coming from the left it is `x > mid`, coming back from the right
    /// it is `x < mid`. Each trade is one move for the frame — `(from, to)` of
    /// the document list — and the dragged tab's position rides with it. The
    /// crossings are read off the rects the layout computed, because every
    /// move re-runs it.
    #[test]
    fn a_drag_trades_at_the_neighbour_s_midpoint() {
        let mut st = scrolled_three();
        st.tracking = true;
        unsafe {
            st.press(10, 5);
            st.mouse_move(30, 0);
            // Tab 1's middle: a pixel short is not yet, a pixel past trades.
            let mid = {
                let r = st.rects[1];
                (r.left + r.right) / 2 + st.xoff
            };
            assert_eq!(st.mouse_move(mid - 1, 0), None);
            assert_eq!(st.mouse_move(mid + 1, 0), Some((0, 1)));
            assert_eq!(st.entries[0].label, "1.txt");
            assert_eq!(st.entries[1].label, "0.txt");
            assert_eq!(st.press_item, Some(1));
            // And back over tab 0's own middle, from the right this time.
            let mid = {
                let r = st.rects[0];
                (r.left + r.right) / 2 + st.xoff
            };
            assert_eq!(st.mouse_move(mid + 1, 0), None);
            assert_eq!(st.mouse_move(mid - 1, 0), Some((1, 0)));
            assert_eq!(st.entries[0].label, "0.txt");
            // The edge of the strip is not a trade: past the content the hit
            // test answers nothing and the drag keeps its tab.
            let past = st.content + 10;
            assert_eq!(st.mouse_move(past, 0), None);
            assert_eq!(st.press_item, Some(0));
        }
    }

    /// A release that ends a drag answers nothing — the ✕ and ▾ machines are
    /// spent once the press went to the drag — and leaves no press behind.
    #[test]
    fn a_release_after_a_drag_closes_nothing() {
        let mut st = close_strip();
        st.tracking = true;
        unsafe {
            // The press armed tab 0's ✕, then dragged away from it.
            assert_eq!(st.press(30, 5), Press::None);
            st.mouse_move(20, 0);
            st.mouse_move(61, 0);
            assert_eq!(st.entries[0].label, "1.txt");
            assert_eq!(st.release_at(61, 0), Release::Nothing);
            assert!(!st.dragging());
            assert_eq!(st.press_item, None);
        }
    }

    /// The capture lost mid-drag sends the tab home: the move back comes to the
    /// frame as the undo of the way out, selection included.
    #[test]
    fn a_capture_loss_walks_the_drag_back() {
        let mut st = scrolled_three();
        st.tracking = true;
        unsafe {
            st.press(10, 5);
            st.mouse_move(30, 0);
            st.mouse_move(61, 0);
            assert_eq!(st.cancel_drag(), Some((1, 0)));
            assert_eq!(st.entries[0].label, "0.txt");
            assert_eq!(st.entries[1].label, "1.txt");
            assert!(!st.dragging());
            assert_eq!(st.press_item, None);
            // A drag that never traded cancels to nothing.
            st.press(10, 5);
            st.mouse_move(30, 0);
            assert_eq!(st.cancel_drag(), None);
        }
    }

    /// A leave no longer clears the pressed nibble: the release still holds the
    /// capture and reads it, so a ▾ pressed while the cursor stepped out of the
    /// window still opens the list on its way up.
    #[test]
    fn a_leave_keeps_the_press_waiting_for_its_release() {
        let mut st = two_tabs();
        unsafe {
            st.press(10, 5);
            st.mouse_leave();
            assert_eq!(st.pressed, CELL_BAND);
            assert_eq!(st.press_item, Some(0));
            // The ▾ pair still answers after a leave.
            st.press(120, 5);
            st.mouse_leave();
            st.pressed = PRESS_DROP;
            st.inside_cell = PRESS_DROP;
            assert_eq!(st.release_at(120, 5), Release::Dropdown);
        }
    }

    /// The scaled half of that needs WIC, so the apartment is the one the
    /// frame's own start-up already initialised.
    #[test]
    fn the_close_glyphs_load_from_the_bundled_strip() {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Controls::{ImageList_Destroy, ImageList_GetImageCount};
        let mut st = close_strip();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            st.build_x_list(120);
            assert_ne!(st.xlist.0, 0);
            assert_eq!(ImageList_GetImageCount(st.xlist), X_CELLS);
            for state in [XState::Normal, XState::Hot, XState::Pressed] {
                let first = st.x_glyph(state);
                assert!(first.is_some(), "{state:?}");
                // Cached
                assert_eq!(first, st.x_glyph(state));
            }
            let _ = ImageList_Destroy(Some(st.xlist));
        }
    }

    /// The tail wins over a tab, the way the tab hit test tests it.
    #[test]
    fn hit_test_prefers_the_tail() {
        let m = m();
        let (rects, _) = layout_rects(&[131, 131], 5);
        let (right, left, drop, _) = tail_rects(1302, &m, true, true);
        let hit = |x| hit_test(x, &rects, 0, right, left, drop);
        assert_eq!(hit(1290), Hit::Right);
        assert_eq!(hit(1270), Hit::Left);
        assert_eq!(hit(1250), Hit::Dropdown);
        assert_eq!(hit(6), Hit::Tab(0));
        assert_eq!(hit(136), Hit::Tab(1));
        assert_eq!(hit(300), Hit::None);
        // Scrolled: the hit follows the rects, and the tail does not move. With
        // the band at -100, tab 0 ends at 36 and tab 1 covers 36..167.
        let hit = |x| hit_test(x, &rects, -100, right, left, drop);
        assert_eq!(hit(40), Hit::Tab(1));
        assert_eq!(hit(20), Hit::Tab(0));
        assert_eq!(hit(1290), Hit::Right);
    }

    /// `HideOnlyOne=Y` and the `TabBar` switch both end up as no row.
    #[test]
    fn row_height_is_zero_when_the_bar_is_not_shown() {
        let entries = |n: usize| {
            (0..n)
                .map(|i| TabEntry {
                    label: format!("{i}.txt"),
                    path: None,
                    modified: false,
                })
                .collect()
        };
        let mut s = TabStrip::new();
        unsafe {
            s.set_entries(entries(2), 0);
        }
        assert_eq!(s.row_h(true), s.metrics.height as f32);
        unsafe {
            s.set_entries(entries(1), 0);
        }
        assert_eq!(s.row_h(true), 0.0);
        unsafe {
            s.set_entries(entries(3), 0);
        }
        assert_eq!(s.row_h(false), 0.0);
    }
}
