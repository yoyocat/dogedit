//! The dock host's caption strip: the row along a host's edge that names the
//! panes docked there, plus the box at its right end that closes the pane the
//! host is showing.
//!
//! There is one of these per host and one host per side, so every docked pane —
//! the directory tree, the open-document list, the search results and every
//! output window — is a cell of exactly this control. That is the whole of the
//! switching UI: one cell per pane in dock order, the cell of the pane on
//! screen wearing the selected face, and a click that names the pane under it.
//!
//! The row is the same control the document tabs are: the same `Metrics`, the
//! same tab body (a fill two rows in from the frame, verticals on both sides, a
//! rule across the top), the same bold label on the selected cell. A pane tab
//! therefore reads as a document tab, which is what makes the strip look like
//! part of the frame rather than a second control with its own mind.
//!
//! Clicks leave as [`TabNotify`] through the `WM_APP` queue the directory tree
//! and the results list already post to: a click handler that called back into
//! the frame's state would be re-entering the borrow it is sitting on.

#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::Cell;
use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontIndirectW, DC_PEN,
    DeleteDC, DeleteObject, DrawTextW, DT_CALCRECT, DT_EDITCONTROL, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, EndPaint, FillRect, GetDC, GetStockObject, GetTextMetricsW, HDC, HFONT, HGDIOBJ,
    InvalidateRect, LineTo, LOGFONTW, MoveToEx, NULL_BRUSH, PAINTSTRUCT, PS_SOLID, ReleaseDC,
    RoundRect, SelectObject, SetBkMode, SetDCPenColor, SetTextColor, SRCCOPY, TEXTMETRICW,
    TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    HIMAGELIST, ILD_TRANSPARENT, ImageList_Draw, ImageList_Destroy, WM_MOUSELEAVE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TrackMouseEvent, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetParent, HTCLIENT, HMENU, IDC_ARROW,
    LoadCursorW, PostMessageW, RegisterClassW, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SetWindowPos,
    ShowWindow, WINDOW_EX_STYLE, WM_APP, WM_ERASEBKGND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WNDCLASSW, WS_CHILD, CS_DBLCLKS,
};
use windows::core::{PCWSTR, w};

use crate::win32::gdi::pixel_height;
use crate::win32::shape::CellShape;
use crate::win32::tabstrip::{Metrics, layout_rects, tab_width};
use crate::win32::window::{state_mut, to_colorref};
use dg_render::Rect;
use dg_render::theme::Theme;

/// The strip's class, named after what it is rather than after a control it
/// imitates: nothing outside the process looks for it.
const CAPTION_CLASS: PCWSTR = w!("DockCaption");

/// `dock.tab` 这个区域名：绘制和悬停声明用的是同一个词。
const SKIN_REGION: &str = "dock.tab";

/// The strip's control id base; each host takes the next one.
const CTRL_ID: usize = 2040;

/// Padding inside a cell, either side of its label. The cell's own measure
/// takes these from [`Metrics`]; this is only the close box's inset.
const CLOSE_BOX: i32 = 12;
/// Distance from the close box to the strip's right edge.
const CLOSE_MARGIN: i32 = 6;

/// The label font's size in points, the same the document tabs are drawn at.
const MESSAGE_POINTS: f32 = 9.0;

/// One cell: the caller's own id for the pane, the caption it draws, and the
/// sprite cell its icon comes from. The id is what a click reports back, so the
/// strip never has to guess which pane a position in its list means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TabEntry {
    pub id: u64,
    pub label: String,
    /// The cell of the toolbar's sprite sheet this pane's icon draws — the same
    /// cell the button for the pane's own command carries, so a caption and its
    /// toolbar button are never two different pictures of one pane. `None` for
    /// a command the sheet has no cell for.
    pub icon: Option<i32>,
}

/// What a click on the strip asks the frame to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabNotify {
    /// Put this pane on screen.
    Select(u64),
    /// Close this pane; the host falls back to the pane next to it.
    Close(u64),
    /// The caption menu, at this screen position.
    Menu(u64, i32, i32),
}

pub(super) struct DockTabs {
    hwnd: HWND,
    frame: Rect,
    /// One entry per docked pane, in the order the strip shows them.
    entries: Vec<TabEntry>,
    /// Which entry is the pane on screen.
    active: usize,
    /// The cells the labels were last measured into, in client space.
    cells: Vec<RECT>,
    close_cell: RECT,
    dpi: i32,
    metrics: Metrics,
    font: HFONT,
    bold: HFONT,
    /// The toolbar's sprite sheet as an image list, at the caption's own icon
    /// cell: one cell per command, the same list the bar's buttons draw from.
    icons: HIMAGELIST,
    bg: COLORREF,
    fg: COLORREF,
    active_bg: COLORREF,
    active_fg: COLORREF,
    line: COLORREF,
    hot_band: COLORREF,
    /// The cell under the cursor, which paints the state band across its top
    /// rule the way a hovered document tab does.
    hot: Option<usize>,
    tracking: bool,
    /// 皮肤要的指针和提示（`s:hint("dock.tab", …)`）。这条页签条本来一条提示都
    /// 没有，所以提示也归皮肤。
    hint: crate::win32::skinhint::SkinHint,
    /// Notifies waiting for the frame to drain them.
    pending: Cell<Vec<TabNotify>>,
}

fn instance() -> HINSTANCE {
    unsafe {
        GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or(HINSTANCE(std::ptr::null_mut()))
    }
}

static REGISTER_CLASS: Once = Once::new();

impl DockTabs {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            entries: Vec::new(),
            active: 0,
            cells: Vec::new(),
            close_cell: RECT::default(),
            dpi: 96,
            metrics: Metrics::for_dock(96, 16),
            font: HFONT::default(),
            bold: HFONT::default(),
            icons: HIMAGELIST(0),
            bg: COLORREF(0),
            fg: COLORREF(0),
            active_bg: COLORREF(0),
            active_fg: COLORREF(0),
            line: COLORREF(0),
            hot_band: COLORREF(0),
            hot: None,
            tracking: false,
            hint: crate::win32::skinhint::SkinHint::new(),
            pending: Cell::new(Vec::new()),
        }
    }

    pub(super) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// The row the dock has to reserve for the strip. 0 before the window
    /// exists: a host with no strip keeps no row for one.
    pub(super) fn row_h(&self) -> f32 {
        if !self.alive() {
            return 0.0;
        }
        self.metrics.height as f32
    }

    pub(super) unsafe fn create(&mut self, parent: HWND, dpi: i32, theme: &Theme, slot: usize) {
        self.dpi = dpi.max(96);
        if self.font.0.is_null() {
            self.build_fonts();
        }
        if self.icons.0 == 0 {
            self.build_icons();
        }
        self.apply_theme(theme);
        if self.alive() {
            return;
        }
        REGISTER_CLASS.call_once(|| {
            let _ = RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(caption_proc),
                hInstance: instance(),
                lpszClassName: CAPTION_CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                // A double-clicked caption closes its pane, so the class has to
                // ask for the message: without this the pair arrives as two
                // single clicks.
                style: CS_DBLCLKS,
                ..Default::default()
            });
        });
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CAPTION_CLASS,
            w!("dock captions"),
            // Not WS_VISIBLE: `place` shows the strip once the host has a row
            // for it, so a closed dock owns no pixels.
            WS_CHILD,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU((CTRL_ID + slot) as *mut core::ffi::c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.hwnd = hwnd;
    }

    /// Re-measures the label font and the row it needs. The host calls this on
    /// a DPI change, before the layout pass that reserves the row.
    pub(super) unsafe fn set_dpi(&mut self, dpi: i32) {
        self.dpi = dpi.max(96);
        for font in [self.font, self.bold] {
            if !font.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
        self.font = HFONT::default();
        self.bold = HFONT::default();
        self.build_fonts();
        self.build_icons();
        self.relayout();
        if self.alive() {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// The label font and its bold twin — the latter used to measure and draw
    /// the selected cell, exactly as the document strip does.
    unsafe fn build_fonts(&mut self) {
        let height = -pixel_height(self.dpi as u32, MESSAGE_POINTS).max(1);
        let plain = LOGFONTW {
            lfHeight: height,
            lfWeight: 400,
            ..Default::default()
        };
        self.font = CreateFontIndirectW(&plain);
        let heavy = LOGFONTW {
            lfHeight: height,
            lfWeight: 700,
            ..Default::default()
        };
        self.bold = CreateFontIndirectW(&heavy);
        self.metrics = Metrics::for_dock(self.dpi as u32, self.tm_height());
    }

    /// `tm.tmHeight` of the label font — the number the row height comes from.
    unsafe fn tm_height(&self) -> i32 {
        if self.font.0.is_null() {
            return 16;
        }
        let dc = GetDC(None);
        if dc.0.is_null() {
            return 16;
        }
        let old = SelectObject(dc, HGDIOBJ(self.font.0));
        let mut tm = TEXTMETRICW::default();
        let ok = GetTextMetricsW(dc, &mut tm).ok().is_ok() && tm.tmHeight > 0;
        SelectObject(dc, old);
        let _ = ReleaseDC(None, dc);
        if ok { tm.tmHeight } else { 16 }
    }

    /// The icon list, from the sheet the toolbar loads — including a loaded
    /// icon theme, so a caption and its button change picture together. The
    /// list's cell is `16 * scale`, which is this strip's icon cell, so a cell
    /// lands in the box the label was measured around without any scaling.
    unsafe fn build_icons(&mut self) {
        self.drop_icons();
        if let Some((list, _)) = crate::win32::toolbar::dialog_icon_list(self.dpi as u32) {
            self.icons = list;
        }
    }

    unsafe fn drop_icons(&mut self) {
        if self.icons.0 != 0 {
            let _ = ImageList_Destroy(Some(self.icons));
            self.icons = HIMAGELIST(0);
        }
    }

    /// 只重画，不重建任何东西。热重载换了皮肤之后由
    /// [`super::window::EditorState::repaint_all_chrome`] 逐条点名。
    pub(super) unsafe fn invalidate(&self) {
        if self.alive() {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Rebuilds the icons after an icon-theme swap and repaints. The sheet the
    /// toolbar just loaded is the one the captions have to draw from.
    pub(super) unsafe fn retheme(&mut self) {
        self.build_icons();
        if self.alive() {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Places the strip in `rect`; an empty rect means the host is closed.
    pub(super) unsafe fn place(&mut self, rect: Rect) {
        if !self.alive() {
            return;
        }
        let open = rect.w > 0.0 && rect.h > 0.0;
        if !open {
            if self.frame.w > 0.0 {
                self.frame = Rect::new(0.0, 0.0, 0.0, 0.0);
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return;
        }
        if self.frame == rect {
            return;
        }
        self.frame = rect;
        let _ = SetWindowPos(
            self.hwnd,
            None,
            rect.x.round() as i32,
            rect.y.round() as i32,
            rect.w.round() as i32,
            rect.h.round() as i32,
            SWP_NOACTIVATE,
        );
        let _ = ShowWindow(self.hwnd, SW_SHOW);
        self.relayout();
    }

    /// Hands the strip the panes it names and which of them is on screen. The
    /// dock calls it on every layout, and only a real change invalidates.
    pub(super) fn set_entries(&mut self, entries: &[TabEntry], active: usize) {
        if self.entries.as_slice() == entries && self.active == active {
            return;
        }
        self.entries.clear();
        self.entries.extend_from_slice(entries);
        self.active = active.min(self.entries.len().saturating_sub(1));
        self.relayout();
        if self.alive() {
            let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        }
    }

    /// Measures the cells for the current width. Hit-testing reads the same
    /// rects the paint does, so a click that lands before the first paint still
    /// names the cell it is over.
    fn relayout(&mut self) {
        self.cells.clear();
        let w = self.frame.w.round() as i32;
        let h = self.frame.h.round() as i32;
        self.close_cell = self.close_box(w, h);
        if w <= 0 || h <= 0 {
            return;
        }
        let widths: Vec<i32> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let text = self.text_width(&e.label, i == self.active);
                tab_width(text, &self.metrics, true, false, 0)
            })
            .collect();
        let (rects, _) = layout_rects(&widths, self.metrics.inset);
        // The close box is the strip's own cell, so a tab stops short of it
        // rather than running under the ✕.
        let band_right = if self.entries.is_empty() {
            w
        } else {
            (self.close_cell.left - 2).max(0)
        };
        let row = self.metrics.height;
        let top = (h - row).max(0) / 2;
        for r in rects {
            if r.left >= band_right {
                break;
            }
            self.cells.push(RECT {
                left: r.left,
                top,
                right: r.right.min(band_right),
                bottom: top + row,
            });
        }
    }

    /// The label's advance at the strip's font — bold for the selected cell,
    /// which is what the tab layout measures with.
    fn text_width(&self, label: &str, selected: bool) -> i32 {
        let font = if selected { self.bold } else { self.font };
        if font.0.is_null() {
            return 0;
        }
        let mut wide: Vec<u16> = label.encode_utf16().take(32).collect();
        if wide.is_empty() {
            return 0;
        }
        unsafe {
            let dc = GetDC(None);
            if dc.0.is_null() {
                return 0;
            }
            let old = SelectObject(dc, HGDIOBJ(font.0));
            let mut rc = RECT::default();
            let _ = DrawTextW(
                dc,
                &mut wide,
                &mut rc,
                DT_CALCRECT | DT_EDITCONTROL | DT_SINGLELINE,
            );
            SelectObject(dc, old);
            let _ = ReleaseDC(None, dc);
            rc.right.max(0)
        }
    }

    pub(super) fn apply_theme(&mut self, theme: &Theme) {
        let bg = to_colorref(theme.tab_bg);
        let fg = to_colorref(theme.tab_fg);
        let active_bg = to_colorref(theme.tab_active_bg);
        let active_fg = to_colorref(theme.tab_active_fg);
        let line = to_colorref(theme.tab_frame);
        let hot_band = to_colorref(theme.tab_hot_band);
        // The layout pass hands the theme down on every resize; only a real
        // colour change is worth a repaint.
        let changed = (bg.0, fg.0, active_bg.0, active_fg.0, line.0, hot_band.0)
            != (
                self.bg.0,
                self.fg.0,
                self.active_bg.0,
                self.active_fg.0,
                self.line.0,
                self.hot_band.0,
            );
        self.bg = bg;
        self.fg = fg;
        self.active_bg = active_bg;
        self.active_fg = active_fg;
        self.line = line;
        self.hot_band = hot_band;
        if changed && self.alive() {
            let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        }
    }

    /// The cell holding `(x, y)` in client space.
    pub(super) fn hit_tab(&self, x: i32, y: i32) -> Option<usize> {
        self.cells
            .iter()
            .position(|c| x >= c.left && x < c.right && y >= c.top && y < c.bottom)
    }

    /// Whether `(x, y)` is the close box at the strip's right end.
    pub(super) fn hit_close(&self, x: i32, y: i32) -> bool {
        let c = self.close_cell;
        c.right > c.left && x >= c.left && x < c.right && y >= c.top && y < c.bottom
    }

    pub(super) fn drain_notifies(&self) -> Vec<TabNotify> {
        self.pending.take()
    }

    fn queue(&self, n: TabNotify) {
        let mut list = self.pending.take();
        list.push(n);
        self.pending.set(list);
        unsafe {
            if let Ok(frame) = GetParent(self.hwnd)
                && !frame.0.is_null()
            {
                let _ = PostMessageW(Some(frame), WM_APP, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// The close box, in client space.
    fn close_box(&self, w: i32, h: i32) -> RECT {
        let row = self.metrics.height;
        let size = CLOSE_BOX;
        let top = (h - row).max(0) / 2 + (row - size) / 2;
        RECT {
            left: (w - size - CLOSE_MARGIN).max(0),
            top,
            right: (w - CLOSE_MARGIN).max(0),
            bottom: top + size,
        }
    }

    /// Paints the band, the cells and the close box into `mem`, whose size is
    /// `w` by `h`.
    ///
    /// `host` 有 `dock.tab` 回调时，每一格的底色和那几道线交给它画。**图标和
    /// 文字仍归控件** —— 和文档页签、工具栏一个规矩（那套精灵图在 image list
    /// 里，脚本拿不到）。
    fn paint(
        &self,
        mem: HDC,
        w: i32,
        h: i32,
        host: Option<&std::rc::Rc<dg_script::ScriptHost>>,
    ) {
        let _stats = super::paintstats::begin(super::paintstats::Region::DockTabs);
        unsafe {
            let band = RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            };
            let _ = FillRect(mem, &band, super::gdicache::brush(self.bg));
            let _ = SetBkMode(mem, TRANSPARENT);
            let saved_pen = SelectObject(mem, GetStockObject(DC_PEN));
            SetDCPenColor(mem, self.line);
            // Unselected cells first: the selected one paints over the seam it
            // shares with its neighbour, which is how a tab reads as raised.
            for (index, cell) in self.cells.iter().enumerate() {
                if index != self.active {
                    self.paint_tab(mem, index, cell, host);
                }
            }
            if let Some(cell) = self.cells.get(self.active) {
                self.paint_tab(mem, self.active, cell, host);
            }
            for (index, (entry, cell)) in self.entries.iter().zip(&self.cells).enumerate() {
                self.paint_label(mem, entry, index, cell);
            }
            SelectObject(mem, saved_pen);
            // The close box closes the pane on screen, so a host with two panes
            // needs no extra row to say which one that is.
            if !self.entries.is_empty() {
                let close = self.close_cell;
                let pen = super::gdicache::pen(PS_SOLID, 1, self.fg);
                let old_pen = SelectObject(mem, pen.into());
                let _ = MoveToEx(mem, close.left, close.top, None);
                let _ = LineTo(mem, close.right, close.bottom);
                let _ = MoveToEx(mem, close.right, close.top, None);
                let _ = LineTo(mem, close.left, close.bottom);
                SelectObject(mem, old_pen);
            }
        }
    }

    /// 让皮肤画这一格。`true` 表示底色和那几道线它画了，内置那几笔让位。
    ///
    /// op 的坐标是**客户区绝对坐标**（和文档页签一样，这一格的位置就在
    /// `ctx.rect` 里），所以回放不用挪视口。
    /// 第 `index` 个页签的悬停上下文和落点。
    ///
    /// 和绘制读同一份 `cells`；状态位给 0 —— `s:hint` 只读 `text`/`label`/`value`，
    /// 「选没选中、是不是热格」是绘制档的事。
    pub(super) fn hint_at(&self, index: usize) -> Option<(dg_script::PaintInput, POINT)> {
        let cell = self.cells.get(index)?;
        let entry = self.entries.get(index)?;
        let dpi = if self.dpi > 0 { self.dpi } else { 96 };
        let scale = dpi as f32 / 96.0;
        let rect = (
            cell.left as f32 / scale,
            cell.top as f32 / scale,
            (cell.right - cell.left) as f32 / scale,
            (cell.bottom - cell.top) as f32 / scale,
        );
        let input = dg_script::PaintInput {
            rect,
            thumb: rect,
            // 页签条不滚：没有轨道、没有偏移。
            bar: (0.0, 0.0, 0.0, 0.0),
            scroll: 0.0,
            state: 0,
            part: SKIN_REGION.to_string(),
            text: entry.label.clone(),
            label: String::new(),
            value: entry.id.to_string(),
            dpi,
            index: index.min(u16::MAX as usize) as u16,
        };
        Some((
            input,
            crate::win32::skinhint::under(cell.left, cell.bottom),
        ))
    }

    /// 这一格的状态位。绘制（配方 `states.*`）和脚本 `ctx.state` 读同一个函数 ——
    /// 否则同一格在配方里算「活动」、在脚本里算「常态」，作者没法解释。
    fn tab_state(&self, index: usize) -> dg_skin::State {
        let mut state = dg_skin::State::normal();
        if index == self.active {
            state = state.with(dg_skin::State::ACTIVE);
        }
        if self.hot == Some(index) {
            state = state.with(dg_skin::State::HOT);
        }
        state
    }

    /// 窗口 DPI，没设过就按 96 算（`CellShape::of` 要 `u32`，字段是 `i32`）。
    fn dpi_u32(&self) -> u32 {
        if self.dpi > 0 { self.dpi as u32 } else { 96 }
    }

    unsafe fn paint_by_script(
        &self,
        mem: HDC,
        index: usize,
        cell: &RECT,
        host: Option<&std::rc::Rc<dg_script::ScriptHost>>,
    ) -> bool {
        const REGION: &str = SKIN_REGION;
        let Some(host) = host else {
            return false;
        };
        if !host.has_paint(REGION) {
            return false;
        }
        let Some(entry) = self.entries.get(index) else {
            return false;
        };
        let dpi = if self.dpi > 0 { self.dpi } else { 96 };
        let scale = dpi as f32 / 96.0;
        let state = self.tab_state(index).bits();
        let rect = (
            cell.left as f32 / scale,
            cell.top as f32 / scale,
            (cell.right - cell.left) as f32 / scale,
            (cell.bottom - cell.top) as f32 / scale,
        );
        let input = dg_script::PaintInput {
            rect,
            // 页签里没有会动的部件，`ctx.thumb` 与 `ctx.rect` 同值。
            thumb: rect,
            bar: (0.0, 0.0, 0.0, 0.0),
            scroll: 0.0,
            state,
            part: REGION.to_string(),
            text: entry.label.clone(),
            label: String::new(),
            // 面板 id：同一块面板的页签在重排后还是同一个值，比序号稳。
            value: entry.id.to_string(),
            dpi,
            index: index.min(u16::MAX as usize) as u16,
        };
        let Some(out) = host.paint(REGION, &input) else {
            return false;
        };
        if !out.handled {
            return false;
        }
        let ops = out.compacted();
        // 选中的那一格内置用的是粗体，皮肤画字时也拿同一只。
        let fonts = [if index == self.active {
            self.bold
        } else {
            self.font
        }];
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
        let _ = super::skinpaint::replay(mem, ops.as_slice(), &ctx);
        true
    }

    /// One cell's body: the fill two rows in from the frame, the verticals on
    /// both sides, the rule across the top — and the state band over that rule
    /// while the cursor is on it.
    ///
    /// 配方四个数值键在这里落地，几何和文档页签是同一套（[`CellShape`]）：
    /// `gap` 整格左右各缩一半、`border` 是框线宽度（至少 1 px）、`radius` 磨圆
    /// 填充块并把框也换成圆角轮廓、`pad_y` 缩块。出厂值让这里逐像素等于以前。
    unsafe fn paint_tab(
        &self,
        mem: HDC,
        index: usize,
        cell: &RECT,
        host: Option<&std::rc::Rc<dg_script::ScriptHost>>,
    ) {
        if self.paint_by_script(mem, index, cell, host) {
            return;
        }
        let shape = CellShape::of(
            &crate::skin::shape(SKIN_REGION, self.tab_state(index)),
            self.dpi_u32(),
        );
        let left = cell.left + shape.left_inset;
        let right = cell.right - shape.right_inset;
        let top = cell.top;
        let bottom = cell.bottom;
        let inner_top = shape.inner_top(top);
        let inner_right = shape.inner_right(right);
        let body = shape.body(left, top, right, bottom);

        if index == self.active {
            super::gdicache::fill_round(mem, &body, self.active_bg, shape.radius);
        }
        // 上一格的热带可能把 DC 画笔改成高亮色了，这里复位回框线色。
        SetDCPenColor(mem, self.line);
        if shape.radius > 0 {
            // 圆角时框也得磨圆，否则四个角上露出一圈底色（同 `tabstrip`）。
            let pen = super::gdicache::pen(PS_SOLID, shape.border, self.line);
            let saved_pen = SelectObject(mem, HGDIOBJ(pen.0));
            let saved_brush = SelectObject(mem, GetStockObject(NULL_BRUSH));
            let _ = RoundRect(
                mem,
                body.left,
                body.top,
                body.right,
                body.bottom,
                shape.radius * 2,
                shape.radius * 2,
            );
            SelectObject(mem, saved_brush);
            SelectObject(mem, saved_pen);
        } else {
            // 三条线各画 `border` 遍：`border = 1` 就是原来那三条。
            for i in 0..shape.border {
                let _ = MoveToEx(mem, left + i, inner_top, None);
                let _ = LineTo(mem, left + i, bottom);
                let _ = MoveToEx(mem, inner_right + i, inner_top, None);
                let _ = LineTo(mem, inner_right + i, bottom);
                let _ = MoveToEx(mem, left + 1, top + 1 + i, None);
                let _ = LineTo(mem, inner_right, top + 1 + i);
            }
        }
        if self.hot == Some(index) {
            SetDCPenColor(mem, self.hot_band);
            let _ = MoveToEx(mem, left + 1, top + 1, None);
            let _ = LineTo(mem, inner_right, top + 1);
            let _ = MoveToEx(mem, left, top + 2, None);
            let _ = LineTo(mem, inner_right + 1, top + 2);
            SetDCPenColor(mem, self.line);
        }
    }

    /// One cell's icon and label, at the offsets the tab item painter uses —
    /// the same numbers `tab_width` measured the cell with, so a label can
    /// never run past the cell it belongs to.
    ///
    /// 配方只碰 `gap`（把文字框夹进块里、图标跟着缩）和 `pad_y`（文字框上下各缩）。
    /// `font` 和文档页签一样还没接线。
    unsafe fn paint_label(&self, mem: HDC, entry: &TabEntry, index: usize, cell: &RECT) {
        let shape = CellShape::of(
            &crate::skin::shape(SKIN_REGION, self.tab_state(index)),
            self.dpi_u32(),
        );
        let active = index == self.active;
        let cy = self.metrics.cy;
        let icon_left = cell.left + shape.left_inset + self.metrics.extra;
        let mid = (cell.top + 1) / 2 + (cell.top + cell.bottom) / 2;
        // The icon box is reserved whether or not a glyph lands in it: a strip
        // with no sheet still starts its labels at the same x.
        if let Some(cell_index) = entry.icon
            && self.icons.0 != 0
        {
            let _ = ImageList_Draw(
                self.icons,
                cell_index,
                mem,
                icon_left,
                mid - cy / 2 + 1,
                ILD_TRANSPARENT,
            );
        }
        let font = if active { self.bold } else { self.font };
        let old = SelectObject(mem, HGDIOBJ(font.0));
        let _ = SetTextColor(mem, if active { self.active_fg } else { self.fg });
        let mut text = RECT {
            // 文字从**布局给的位置**起，不跟 `gap` 挪：`gap` 是对称缩的，
            // 左缩多少右边也缩多少，挪一下只会把贴边的标签截掉（实测
            // `gap = 4` 就把「快捷目录」最后一个字裁了）。
            left: cell.left + self.metrics.extra + cy + self.metrics.pad,
            top: cell.top + 3 + shape.pad_y,
            right: cell.right - self.metrics.extra - self.metrics.pad,
            bottom: cell.bottom - shape.pad_y,
        };
        // 夹进块里：`gap = 0` 时块比文字框大，四次 max/min 全不生效。
        shape.fit_text(
            &mut text,
            &shape.body(
                cell.left + shape.left_inset,
                cell.top,
                cell.right - shape.right_inset,
                cell.bottom,
            ),
        );
        if text.left < text.right {
            let mut wide: Vec<u16> = entry.label.encode_utf16().collect();
            let _ = DrawTextW(
                mem,
                &mut wide,
                &mut text as *mut RECT,
                DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
        SelectObject(mem, old);
    }
}

impl Drop for DockTabs {
    fn drop(&mut self) {
        unsafe {
            self.drop_icons();
            for font in [self.font, self.bold] {
                if !font.0.is_null() {
                    let _ = DeleteObject(HGDIOBJ(font.0));
                }
            }
        }
        self.font = HFONT::default();
        self.bold = HFONT::default();
    }
}

unsafe extern "system" fn caption_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let frame = match unsafe { GetParent(hwnd) } {
        Ok(f) if !f.0.is_null() => f,
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    let Some(st) = state_mut(frame) else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    // The strip belongs to the host that created it, and a message belongs to
    // the strip that received it.
    let Some(slot) = st.panels.host_of_tabs(hwnd) else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let x = crate::win32::window::low_word(lparam);
    let y = crate::win32::window::high_word(lparam);
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            if hdc.0.is_null() {
                return LRESULT(0);
            }
            let mut rc = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            let w = (rc.right - rc.left).max(1);
            let h = (rc.bottom - rc.top).max(1);
            // One memory DC and a single blit, the same contract the document
            // strip keeps: the band fill and the labels would otherwise land in
            // two passes and flicker.
            unsafe {
                let mem = CreateCompatibleDC(Some(hdc));
                if !mem.0.is_null() {
                    let bmp = CreateCompatibleBitmap(hdc, w, h);
                    if !bmp.0.is_null() {
                        let old = SelectObject(mem, HGDIOBJ(bmp.0));
                        st.panels.hosts[slot].tabs.paint(mem, w, h, st.lua.as_ref());
                        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
                        SelectObject(mem, old);
                        let _ = DeleteObject(HGDIOBJ(bmp.0));
                    }
                    let _ = DeleteDC(mem);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let tabs = &mut st.panels.hosts[slot].tabs;
            let hot = tabs.hit_tab(x, y);
            let changed = tabs.hot != hot;
            if changed {
                tabs.hot = hot;
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            if changed {
                // 换格了才问皮肤：同一格里每秒几十条移动什么都不做。
                let host = st.lua.clone();
                match hot.and_then(|i| tabs.hint_at(i)) {
                    Some((input, anchor)) => tabs.hint.retarget(
                        hwnd,
                        frame,
                        host.as_deref(),
                        SKIN_REGION,
                        &input,
                        anchor,
                        &mut st.tip_host,
                    ),
                    None => tabs.hint.leave(&mut st.tip_host),
                }
            }
            if !tabs.tracking {
                tabs.tracking = true;
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = unsafe { TrackMouseEvent(&mut track) };
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let tabs = &mut st.panels.hosts[slot].tabs;
            tabs.tracking = false;
            if tabs.hot.take().is_some() {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            tabs.hint.leave(&mut st.tip_host);
            LRESULT(0)
        }
        // 皮肤声明了 `cursor` 就由它说了算。没声明时照旧交给 `DefWindowProc`
        // （类的光标是箭头）。
        WM_SETCURSOR if crate::win32::window::low_word(lparam) == HTCLIENT as i32 => {
            if st.panels.hosts[slot].tabs.hint.apply() {
                LRESULT(1)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_LBUTTONDOWN => {
            let tabs = &st.panels.hosts[slot].tabs;
            if tabs.hit_close(x, y) {
                if let Some(entry) = tabs.entries.get(tabs.active) {
                    tabs.queue(TabNotify::Close(entry.id));
                }
            } else if let Some(entry) = tabs.hit_tab(x, y).and_then(|i| tabs.entries.get(i)) {
                tabs.queue(TabNotify::Select(entry.id));
            }
            LRESULT(0)
        }
        // A double-click on a caption closes that pane, the strip's own
        // spelling of the caption menu's 关闭 row.
        WM_LBUTTONDBLCLK => {
            let tabs = &st.panels.hosts[slot].tabs;
            if let Some(entry) = tabs.hit_tab(x, y).and_then(|i| tabs.entries.get(i)) {
                tabs.queue(TabNotify::Close(entry.id));
            }
            LRESULT(0)
        }
        // The tracking itself waits for the message loop (`request_popup`),
        // because a modal TrackPopupMenu would re-enter this proc while the
        // state is borrowed.
        WM_RBUTTONUP => {
            let tabs = &st.panels.hosts[slot].tabs;
            if let Some(entry) = tabs.hit_tab(x, y).and_then(|i| tabs.entries.get(i)) {
                let mut pt = POINT { x, y };
                let _ =
                    unsafe { windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut pt) };
                tabs.queue(TabNotify::Menu(entry.id, pt.x, pt.y));
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
