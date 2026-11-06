//! The bottom dock's 输出窗口.
//!
//! The output pane is a second instance of the document editor control, so a
//! report is drawn by a view rather than by rows the frame measures. Its
//! messages set the two-cell tab grid, turn most visual features off, enable
//! wrapping and append at the end of the buffer; writing to a hidden pane
//! docks it.
//!
//! The bar is a `ToolbarWindow32` built from twelve rows (previous/next
//! result, stop, clear, expand/collapse all, wrap, new output pane, close)
//! over the nine glyphs in `assets/sprites/output16.bmp`.

#![allow(unsafe_op_in_unsafe_fn)]

use dg_core::linedoc::Pos;
use std::ffi::c_void;
use std::sync::Once;

use windows::core::{PCWSTR, w};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, FillRect, GetDC, GetTextMetricsW, HBRUSH, HGDIOBJ, HFONT, InvalidateRect,
    PAINTSTRUCT, ReleaseDC, SelectObject, SRCCOPY, TEXTMETRICW, ValidateRect,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{NMHDR, NM_CUSTOMDRAW};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetParent, KillTimer,
    LoadCursorW, RegisterClassW, SendMessageW, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, CS_DBLCLKS, GWLP_WNDPROC, HMENU, IDC_ARROW, MA_NOACTIVATE, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, WINDOW_EX_STYLE, WM_COMMAND, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NOTIFY,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
};

use crate::app::{Editor, HighlightToggles};
use crate::win32::chrome;
use crate::win32::dwrite::DWriteMeasurer;
use crate::win32::dx::{DxFrame, DxSurface};
use crate::win32::overlay;
use crate::win32::toolbar;
use crate::win32::window::{high_word, low_word, state_mut, to_colorref};
use dg_layout::measure::{CachedMeasurer, TextMeasurer};
use dg_layout::wrap::{LayoutCtx, VirtualWrapper, WinEdgeWrapper, Wrapper};
use dg_render::Rect;
use dg_render::pipeline::DrawList;
use dg_render::theme::Theme;
use dg_render::ViewFlags;
use dg_ui::panel::PanelSide;

const OUT_CLASS: PCWSTR = w!("dogedit_output");
/// `GetDlgItem` id of the pane, one above the results tree's 2020.
const OUT_ID: usize = 2021;
/// Lines a wheel notch moves, the same step the editor's own wheel uses.
const WHEEL_LINES: i32 = 3;

/// `zh-CN.lng`, 1436
/// string of `cm_dock_output` and hands it to the control
/// Which is what its dock tab shows.
pub(super) const PANE_TITLE: &str = "输出窗口";

/// The caption of the output pane with `id`. The number follows the pane, not
/// its place in the list, so closing one never renames the ones that are left:
/// the first pane keeps the plain name and every one 新建输出窗口 adds takes
/// the number that is its id, so two cells side by side never read the same.
pub(super) fn tab_title(id: u64) -> String {
    if id == 0 {
        PANE_TITLE.to_string()
    } else {
        format!("{PANE_TITLE}{id}")
    }
}

pub(super) struct OutputWin {
    hwnd: HWND,
    /// Stable identity for tool runs: a run targets the pane that started it
    /// even if captions are closed and the list renumbers.
    id: u64,
    frame: Rect,
    /// The pane's own editor instance: the report is its document, and every
    /// pixel of it comes from the same view the tabs are drawn by.
    pub(super) ed: Editor,
    draw: DrawList,
    /// The rows' composed surface — see [`OutView`]. The pane's own GDI pass
    /// keeps the toolbar band, the face and the caption strip; the text lives
    /// in this child.
    view: OutView,
    /// The pane's own overlay bars. It *is* the same control — and the report
    /// wraps, so only the vertical one ever has a range to show.
    pub(super) bars: overlay::OverlayBars,
    /// The pane's own bar, the control the output bar builder builds
    /// as a child of the pane window. Its rows answer the commands themselves,
    /// which is why the ids are handled here rather than by the frame.
    pub(super) tb: toolbar::ToolbarWin,
    /// The band that bar holds along the pane's top edge, as the control
    /// measured it — `0.0` while `[Output] ToolBar` is off.
    band: f32,
    /// `[Output] Wrap`, the mode the output pane creator sets at creation and 自动换行
    /// flips.
    wrap: bool,
    /// 显示颜文字 ([Setting] ShowEmoji): whether document emoji paint as colour
    /// glyphs. The frame owns the switch and hands it to every pane; a pane
    /// starts with it on, like the frame does.
    show_emoji: bool,
    /// The report has changed since the last layout. Every content edit defers
    /// its own relayout region, and the merge in `Editor::apply_edit` drops that
    /// record when two edits overlap — which is exactly what a 清除 followed by
    /// the next report is — so the pane keeps its own flag, the same reason the
    /// frame keeps `layout_dirty`.
    dirty: bool,
    /// A 折叠全部 / 展开全部 waiting for the next layout, which is where the wrapper
    /// and measurer `fold_all` needs exist. The pane has no fold points by
    /// construction.
    pending_fold: Option<bool>,
    /// Height of one text row, from the font the frame gave the pane.
    row_h: i32,
    font: HFONT,
    bg: COLORREF,
    /// The side host this pane is docked to. Moving a caption moves this pane
    /// alone; the panes sharing the side stay put.
    side: PanelSide,
}

/// The pane's text area as its own child window, for the same reason the
/// document view is one: the rows are composed by DirectComposition, and a
/// composed visual sits above the pane's GDI surface. The caption strip, the
/// toolbar and the close box stay GDI in the pane; the text does not. The
/// child's client rect *is* the text band, so the row pass records in child
/// coordinates and no translation exists between layout and paint.
struct OutView {
    hwnd: HWND,
    dx: Option<DxSurface>,
    /// Attach attempts that failed, so a machine whose composition stack is
    /// down does not retry on every paint.
    dx_failures: u8,
    /// The rect the window was last given. The pane paints itself on every
    /// append and calls `place` from that paint; without this, every frame
    /// would move the composed child and invalidate it again.
    frame: Rect,
}

impl OutView {
    fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            dx: None,
            dx_failures: 0,
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    unsafe fn create(&mut self, parent: HWND) {
        REGISTER_VIEW.call_once(|| {
            let _ = RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(out_view_proc),
                hInstance: instance(),
                lpszClassName: OUT_VIEW_CLASS,
                ..Default::default()
            });
        });
        if self.alive() {
            return;
        }
        if let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            OUT_VIEW_CLASS,
            w!("output-text"),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            0,
            0,
            Some(parent),
            None,
            Some(instance()),
            None,
        ) {
            self.hwnd = hwnd;
        }
    }

    /// Places the text child at `rect` (pane-client coordinates).
    unsafe fn place(&mut self, rect: Rect) {
        if !self.alive() || rect.w <= 0.0 || rect.h <= 0.0 || self.frame == rect {
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
        let _ = InvalidateRect(Some(self.hwnd), None, false);
    }

    fn invalidate(&self) {
        if self.alive() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }

    /// Presents one text frame. `false` leaves the surface dropped; the child
    /// erases its own background in that case so a dead device does not freeze
    /// the previous frame.
    fn paint(&mut self, w: u32, h: u32, bg: dg_render::Color, frame: DxFrame) -> bool {
        if w == 0 || h == 0 {
            return false;
        }
        if self.dx.is_none() {
            if self.dx_failures >= 3 {
                return false;
            }
            match DxSurface::attach(self.hwnd, "output") {
                Some(surface) => self.dx = Some(surface),
                None => {
                    self.dx_failures += 1;
                    return false;
                }
            }
        }
        let Some(surface) = self.dx.as_mut() else {
            return false;
        };
        match surface.paint(w, h, bg, frame) {
            crate::win32::dx::DxPaint::Presented => true,
            crate::win32::dx::DxPaint::Skipped => {
                unsafe {
                    let _ = SetTimer(
                        Some(self.hwnd),
                        crate::win32::window::DX_RETRY_TIMER_ID,
                        crate::win32::window::DX_RETRY_MS,
                        None,
                    );
                }
                true
            }
            crate::win32::dx::DxPaint::Lost => {
                self.dx = None;
                self.dx_failures += 1;
                false
            }
        }
    }
}

const OUT_VIEW_CLASS: PCWSTR = w!("dogedit_output_view");
static REGISTER_VIEW: Once = Once::new();

/// The text child's message loop: paint is its own, input belongs to the pane
/// (`WM_MOUSEWHEEL` scrolls the pane editor, the strip's clicks are the pane's
/// hit-test) and is forwarded with the caption-band offset added back.
unsafe extern "system" fn out_view_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let pane = match GetParent(hwnd) {
        Ok(p) if !p.0.is_null() => p,
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    let frame = match GetParent(pane) {
        Ok(f) if !f.0.is_null() => f,
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    let Some(st) = state_mut(frame) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    let Some(index) = st
        .panels
        .outputs
        .iter()
        .position(|w| w.view_hwnd() == hwnd)
    else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            // The composed frame does not consume the window's update region
            // the way a GDI paint would, so it has to be validated by hand or
            // WM_PAINT re-posts forever.
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            if st.panels.outputs[index].needs_relayout() {
                st.relayout_output_at(index);
            }
            st.paint_output_text(index, hwnd);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_TIMER => {
            let _ = KillTimer(Some(hwnd), crate::win32::window::DX_RETRY_TIMER_ID);
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        // The pane never takes the focus; a click on the report is not a caret
        // placement, so activating would only steal the keyboard from the text.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEWHEEL => {
            let _ = SendMessageW(pane, msg, Some(wparam), Some(lparam));
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_MOUSEMOVE => {
            let top = st.panels.outputs[index].band.round() as i32;
            let x = low_word(lparam);
            let y = high_word(lparam) + top;
            let shifted = LPARAM((((y as isize) & 0xFFFF) << 16) | (x as isize & 0xFFFF));
            let _ = SendMessageW(pane, msg, Some(wparam), Some(shifted));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

impl OutputWin {
    pub(super) fn new() -> Self {
        let mut ed = Editor::new();
        // the output pane starts with the synthetic document's two-cell tab grid and
        // is not put in read-only mode, the same state a 查找结果 buffer carries. Nothing
        // focuses this window, so in this implementation the buffer only ever grows
        // from a command.
        ed.tab_size = 2;
        // the output pane never takes the folding method a document reads from `[Edit]
        // FoldingMethod` — only its marker *style* — so the pane has no fold regions and
        // no folding strip. That is what leaves its text one bookmark column and the 2
        // px margin in from the edge.
        ed.folding_method = dg_core::FoldingMethod::None;
        ed.mark_saved();
        Self {
            hwnd: HWND::default(),
            id: 0,
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            ed,
            draw: DrawList::default(),
            view: OutView::new(),
            bars: overlay::OverlayBars::new(),
            tb: toolbar::ToolbarWin::new(),
            band: 0.0,
            wrap: true,
            show_emoji: true,
            dirty: true,
            pending_fold: None,
            row_h: 18,
            font: HFONT::default(),
            bg: COLORREF(0),
            side: PanelSide::Bottom,
        }
    }

    pub(super) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    pub(super) fn id(&self) -> u64 {
        self.id
    }

    pub(super) fn set_id(&mut self, id: u64) {
        self.id = id;
    }

    /// Takes the pane's windows down when its caption is closed. The toolbar is
    /// a child of the pane and dies with it; its image list is a separate
    /// object, and the overlay strips are children of the frame, so both are
    /// released here — nothing else would reach them.
    pub(super) unsafe fn destroy(&mut self) {
        if !self.hwnd.0.is_null() {
            let _ = DestroyWindow(self.hwnd);
            self.hwnd = HWND::default();
        }
        self.view.dx = None;
        self.view.hwnd = HWND::default();
        self.tb.release();
        self.bars.destroy();
    }

    /// What the pane holds. The frame's tests read what a command wrote here;
    /// nothing in this repository's build asks for it.
    #[cfg(test)]
    pub(super) fn lines(&self) -> Vec<String> {
        let n = self.ed.doc.line_count();
        (0..n)
            .map(|i| {
                let mut buf = String::new();
                self.ed.doc.line_text(i, &mut buf);
                buf
            })
            .collect()
    }

    /// The view options the output pane creator leaves on a fresh control: the shipped
    /// defaults with the seven it clears and the active line it turns on. The
    /// icon column goes with them
    /// bookmark column and the 2 px margin in from the pane's left edge, which
    /// is what its gutter is once `LineNumber` and `LineState` are off. The two
    /// indices the creator also writes (14 and 18) name no `[Edit]` key and no
    /// `cm_view_*` command, so they stay unmapped here.
    fn apply_options(&mut self) {
        let flags = &mut self.ed.view_flags;
        for bit in [
            ViewFlags::LINE_NUMBER,
            ViewFlags::WHITESPACE,
            ViewFlags::CONTROL_CHAR,
            ViewFlags::END_OF_LINE,
            ViewFlags::END_OF_FILE,
            ViewFlags::INDENT_LINE,
            ViewFlags::LINE_STATE,
            ViewFlags::ICON_AREA,
        ] {
            flags.set(bit, false);
        }
        flags.set(ViewFlags::ACTIVE_LINE, true);
    }

    /// Appends `text` at the end of the buffer: an insert over the empty range at
    /// the document's end. A report starts with its own line break because the pane
    /// keeps whatever it already holds, so a second one cannot run onto the last
    /// line.
    pub(super) fn append(&mut self, text: &str) {
        let last = self.ed.doc.line_count().saturating_sub(1);
        let mut insert = String::new();
        if self.ed.doc.line_len(last) > 0 {
            insert.push('\n');
        }
        insert.push_str(text);
        self.append_stream(&insert);
    }

    /// Appends one chunk of a streaming run — an insert at the buffer's end
    /// with no break in front of it. The break belongs to a *report's* first
    /// line, which [`Self::append`] owns; a chunk has to land exactly where
    /// the last one stopped, or every pipe read would start a new line.
    pub(super) fn append_stream(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let line = self.ed.doc.line_count().saturating_sub(1);
        let end = Pos {
            line,
            col: self.ed.doc.line_len(line),
        };
        if !self.ed.apply_insert(end, text) {
            return;
        }
        let line = self.ed.doc.line_count().saturating_sub(1);
        let col = self.ed.doc.line_len(line);
        self.ed.caret = (line, col);
        self.ed.selection.clear();
        self.dirty = true;
        self.scroll_to_caret();
        self.invalidate();
    }

    /// Whether the buffer's last line is empty — the footer's break test.
    pub(super) fn ends_with_break(&self) -> bool {
        let last = self.ed.doc.line_count().saturating_sub(1);
        self.ed.doc.line_len(last) == 0
    }

    /// Folding needs the wrapper and measurer, which only the layout has, so
    /// the request waits for the next one.
    pub(super) fn fold_all(&mut self, folded: bool) {
        self.pending_fold = Some(folded);
        self.dirty = true;
        self.invalidate();
    }

    /// Puts the caret's row on the last page. The pane has to ask, because it is
    /// painted rather than typed into. With no rect yet there is no page to be
    /// on, and scrolling to a one-row page is what buries the report: the layout
    /// that gives the pane its rect follows and asks again.
    fn scroll_to_caret(&mut self) {
        let visible = self.visible_rows();
        if visible == 0 {
            return;
        }
        let row = self.ed.caret_row().0.max(0) as usize;
        if row + 1 > self.ed.first_row + visible {
            self.ed.first_row = row + 1 - visible;
        }
        self.clamp_top();
    }

    fn clamp_top(&mut self) {
        self.ed.first_row = self.ed.first_row.min(self.ed.total_rows.saturating_sub(1));
    }

    /// Rows that fit between the bar's band and the tab row — the text rect
    /// the band measure leaves the edit control when it lays the pane out.
    pub(super) fn visible_rows(&self) -> usize {
        let text_h = self.text_h();
        if text_h <= 0.0 {
            return 0;
        }
        (text_h / self.ed.theme.line_height.max(1.0)) as usize
    }

    /// The pane's text height: its rect less the bar's band and the 1 px rule
    /// between them. The band measure's edit rect, in other words. The caption
    /// row is the host strip's, outside this rect.
    fn text_h(&self) -> f32 {
        (self.frame.h - self.band - 1.0).max(0.0)
    }

    /// Whether the rows the pane would paint are not the rows its document has.
    /// The frame's `paint` asks the same question of `layout_dirty` and relays
    /// out before it draws; the pane's paint arm does the same with this.
    pub(super) fn needs_relayout(&self) -> bool {
        self.dirty
    }

    /// Re-wraps and re-measures the pane's document for its current rect. The
    /// strip widths are `EditorState::strips_for` on *this* editor, so the text
    /// inset is the gutter the pane's own options leave behind.
    pub(super) fn relayout(
        &mut self,
        measurer: &mut dyn TextMeasurer,
        tab_px: u32,
        strips: [f32; 6],
    ) {
        let gx: f32 = strips.iter().sum();
        self.dirty = false;
        self.ed.theme.gutter_x = gx;
        let w = (self.frame.w - gx).max(0.0).round() as i32;
        self.ed.text_w = w;
        self.ed.wrapped = w > 0 && self.wrap;
        let wrapper: Box<dyn Wrapper> = if self.ed.wrapped {
            Box::new(WinEdgeWrapper {
                width_px: w,
                word_wrap: true,
            })
        } else {
            Box::new(VirtualWrapper)
        };
        let mut ctx = LayoutCtx {
            measurer,
            font: dg_layout::measure::Font::Primary,
            tab_px,
        };
        // The fold request has waited for exactly this: `fold_all` measures
        // through the wrapper and the ctx, and lays the rows out again itself.
        if let Some(folded) = self.pending_fold.take() {
            self.ed.fold_all(folded, &*wrapper, &mut ctx);
        } else {
            self.ed.relayout(&*wrapper, &mut ctx);
        }
        self.scroll_to_caret();
        self.sync_bars();
        // The layout is what makes the caret's row mean anything: the insert
        // that asked for this page ran against the row index it had *before*
        // the text landed, so the scroll above is the first one that can be
        // right — and a pane whose rect never changed gets no repaint from
        // `place`, which would leave the previous page on screen.
        self.invalidate();
    }

    /// Moves the pane's first row, which is what its own bar asks for.
    pub(super) fn set_scroll_row(&mut self, row: usize) {
        self.ed.first_row = row.min(self.ed.total_rows.saturating_sub(1));
        self.sync_bars();
        self.invalidate();
    }

    /// 清除: the buffer emptied, and the undo history
    /// collapsed with it so the walk back cannot reach what the button removed —
    /// which is also why a second 字符统计 starts the pane from nothing.
    pub(super) fn clear(&mut self) {
        let last = self.ed.doc.line_count().saturating_sub(1);
        if last == 0 && self.ed.doc.line_len(0) == 0 {
            return;
        }
        let end = Pos {
            line: last,
            col: self.ed.doc.line_len(last),
        };
        if !self.ed.apply_delete(Pos { line: 0, col: 0 }, end) {
            return;
        }
        self.ed.caret = (0, 0);
        self.ed.selection.clear();
        self.ed.first_row = 0;
        self.dirty = true;
        self.sync_bars();
        self.invalidate();
    }

    /// 自动换行: the pane's wrap-mode flip, answering with the new mode. Writing
    /// `[Output] Wrap` and re-laying the report out are the frame's, because
    /// wrapping is a layout question and only it has the measurer — see
    /// [`EditorState::toggle_output_wrap`].
    pub(super) fn toggle_wrap(&mut self) -> bool {
        self.wrap = !self.wrap;
        self.dirty = true;
        self.wrap
    }

    /// 下一个结果 / 上一个结果: move the caret to the next row the `^(.*)\((\d+),(\d+)\):`
    /// matcher accepts and answer what that row names. It refuses at both ends —
    /// 下一个结果 returns when the caret is already on the last anchor's row, 上一个结果
    /// when the caret is on the first — so here the caret stays put when there is
    /// nowhere to go. The caret lands at column 0, which is where `^` puts the
    /// match.
    pub(super) fn step_anchor(&mut self, next: bool) -> Option<(String, usize, usize)> {
        let count = self.ed.doc.line_count();
        if !next && self.ed.caret.0 == 0 {
            return None;
        }
        let mut at = if next {
            self.ed.caret.0 + 1
        } else {
            self.ed.caret.0 - 1
        };
        let hit = loop {
            if at >= count {
                break None;
            }
            let mut buf = String::new();
            self.ed.doc.line_text(at, &mut buf);
            if let Some(target) = dg_core::filesearch::output_target(&buf) {
                break Some((at, target));
            }
            if next {
                at += 1;
            } else if at == 0 {
                break None;
            } else {
                at -= 1;
            }
        };
        let (row, (path, line, col)) = hit?;
        self.ed.caret = (row, 0);
        self.scroll_to_caret();
        self.sync_bars();
        self.invalidate();
        Some((path, line, col))
    }

    /// Creates the pane's bar windows as children of the frame. They share the
    /// document view's window class, so `bar_proc` tells the sets apart by
    /// handle — see `EditorState::bar_owner`.
    pub(super) unsafe fn create_bars(&mut self, parent: HWND) {
        self.bars.create(parent);
    }

    /// Gives the vertical strip its rect and refreshes both models. The
    /// horizontal one is handed nothing: the report wraps, so there are no
    /// columns past the edge to reach, which is the same answer the editor gives
    /// while it is wrapping. The strip covers the text rows exactly, from the
    /// bottom of the bar's band to the pane's own bottom, because the control's
    /// bars live inside the rect the band measure gives the edit.
    pub(super) unsafe fn place_bars(&mut self, dock: Rect, strip: f32, theme: &Theme) {
        let v = if dock.w <= 0.0 || strip <= 0.0 {
            Rect::new(0.0, 0.0, 0.0, 0.0)
        } else {
            Rect::new(
                dock.x + dock.w - strip,
                dock.y + self.band,
                strip,
                (dock.h - self.band).max(0.0),
            )
        };
        let none = Rect::new(0.0, 0.0, 0.0, 0.0);
        self.bars.place(v, none);
        self.bars.apply_theme(theme);
        self.sync_bars();
    }

    /// Feeds the bars from the pane's own scroll state.
    pub(super) fn sync_bars(&mut self) {
        let visible = self.visible_rows();
        let range = self.ed.total_rows.saturating_sub(visible.max(1)) as i32;
        let moved = self
            .bars
            .set_model(overlay::V, self.ed.first_row as i32, range, visible as i32);
        self.bars.set_model(overlay::H, 0, 0, 1);
        unsafe { self.bars.sync((moved, false)) };
    }

    /// Builds the pane and its bar.
    pub(super) unsafe fn create(
        &mut self,
        parent: HWND,
        theme: &Theme,
        dpi: u32,
        bar_on: bool,
        wrap_on: bool,
    ) {
        REGISTER_CLASS.call_once(|| {
            let _ = RegisterClassW(&WNDCLASSW {
                style: CS_DBLCLKS,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpfnWndProc: Some(out_proc),
                lpszClassName: OUT_CLASS,
                hInstance: instance(),
                hbrBackground: HBRUSH::default(),
                ..Default::default()
            });
        });
        self.apply_theme(theme);
        if self.alive() {
            return;
        }
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            OUT_CLASS,
            w!("output"),
            // Not visible: `place` shows the pane once the dock has a rect for it.
            WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(OUT_ID as *mut c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        let _ = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, out_proc as *const () as isize);
        self.hwnd = hwnd;
        self.view.create(hwnd);
        self.wrap = wrap_on;
        // The bar is the pane window's own child, which is where the output pane creator
        // puts it: it hands the container it has just built to
        // the output bar builder, then hides it again when `[Output] ToolBar` says so.
        let dark = chrome::theme_is_dark(theme);
        self.tb.create_output(self.hwnd, dpi, bar_on, dark);
        self.band = if bar_on {
            self.tb.ideal_band_h()
        } else {
            0.0
        };
    }

    /// Places the pane in `rect`; an empty rect means the dock is closed.
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
        // The pane's own `WM_SIZE` pass: the bar takes the top band at the full
        // width and everything below it is the text's (the band measure).
        self.tb.place(Rect::new(0.0, 0.0, rect.w, self.band));
        self.place_view();
        self.invalidate();
    }

    pub(super) unsafe fn apply_theme(&mut self, theme: &Theme) {
        // The face is the document's: this is a view of a document, so it takes
        // the editor's colours and never a chrome colour of its own.
        self.ed.theme = theme.clone();
        self.bg = to_colorref(theme.background);
        self.apply_options();
        // The bar is chrome, so it wears the chrome palette the frame's own bar
        // does — and its band keeps its measured height through a theme switch,
        // because only the colours change.
        self.tb.set_dark(chrome::theme_is_dark(theme));
        self.invalidate();
    }

    /// Takes the editor's font so the report reads at the size the user is
    /// already looking at, and re-measures the row height that goes with it.
    pub(super) unsafe fn set_font(&mut self, font: HFONT) {
        if font.is_invalid() || font.0.is_null() {
            return;
        }
        self.font = font;
        let dc = GetDC(None);
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let mut tm = TEXTMETRICW::default();
        if GetTextMetricsW(dc, &mut tm).ok().is_ok() && tm.tmHeight > 0 {
            self.row_h = tm.tmHeight + tm.tmExternalLeading;
        }
        SelectObject(dc, old);
        ReleaseDC(None, dc);
        self.invalidate();
    }

    fn invalidate(&self) {
        if self.alive() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
        self.view.invalidate();
    }

    /// The pane window's handle, which is how the dock finds the pane that a
    /// message or a tool run belongs to.
    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub(super) fn side(&self) -> PanelSide {
        self.side
    }

    pub(super) fn set_side(&mut self, side: PanelSide) {
        self.side = side;
    }

    /// The frame's 显示颜文字 switch, applied to this pane's text band.
    pub(super) fn set_show_emoji(&mut self, on: bool) {
        if self.show_emoji == on {
            return;
        }
        self.show_emoji = on;
        self.invalidate();
    }

    unsafe fn paint(&mut self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.0.is_null() {
            // Nothing here will consume the update region, and a WM_PAINT that
            // is never consumed is re-delivered as fast as the loop can run.
            let _ = ValidateRect(Some(hwnd), None);
            return;
        }
        // `ps.rcPaint` is not a liveness test: this window is
        // `WS_CLIPCHILDREN` and its toolbar covers the top rows, so a paint
        // whose dirty rows are all under a child comes back empty — and it is
        // still a real paint that `EndPaint` closes. Returning early on it
        // would skip the pane's own frame, caption and text.
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let w = (rc.right - rc.left).max(1);
        let h = (rc.bottom - rc.top).max(1);
        // One memory DC and a single blit, the same contract the strip keeps: the
        // pane fills its whole face on every scroll, so painting direct would
        // flash the background between the fill and the rows.
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = if mem.0.is_null() {
            Default::default()
        } else {
            CreateCompatibleBitmap(hdc, w, h)
        };
        if mem.0.is_null() || bmp.0.is_null() {
            if !mem.0.is_null() {
                let _ = DeleteDC(mem);
            }
            let _ = EndPaint(hwnd, &ps);
            self.place_view();
            return;
        }
        let old_bmp = SelectObject(mem, bmp.into());
        let face = CreateSolidBrush(self.bg);
        let _ = FillRect(mem, &rc, face);
        let _ = DeleteObject(face.into());
        // The pane is the report and its bar, nothing else: the caption row
        // belongs to the host's strip, which is a window of its own. The text
        // band is the child's business (see `paint_text`), so this pass only has
        // to keep the face current.
        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old_bmp);
        let _ = DeleteObject(bmp.into());
        // The memory DC is not a GDI object handle: `DeleteObject` on it leaks
        // the DC, and a pane that repaints on every append exhausts the GDI
        // heap until the blit above starts failing and the pane goes blank.
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
        // The text child follows the band this pass has just measured.
        self.place_view();
    }

    /// The composed text child's `WM_PAINT`: the row pass, recorded in the
    /// child's own coordinates — its client rect *is* the text band, so the
    /// ruler term is zero and nothing is translated between layout and paint.
    pub(super) unsafe fn paint_text(
        &mut self,
        hwnd: HWND,
        measurer: &mut CachedMeasurer<DWriteMeasurer>,
    ) -> bool {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let w = (rc.right - rc.left).max(1);
        let h = (rc.bottom - rc.top).max(1);
        let rows = (h as f32 / self.ed.theme.line_height.max(1.0)) as usize;
        self.clamp_top();
        let clip = dg_render::component::Rect::new(0.0, 0.0, w as f32, h as f32);
        self.draw.clear();
        {
            let shaper = measurer.inner_mut().shaper();
            let visible = self.ed.visible_rows(rows);
            self.ed
                .measure_visible_advances(visible, |ch| shaper.advance(ch) as i32);
        }
        self.ed
            .paint(rows, clip, clip.w, &mut self.draw, 0.0, HighlightToggles::default());
        self.ed.paint_decorations(clip, rows, 0.0, &mut self.draw);
        let (x_width, wide) = (
            self.ed.theme.x_width.round(),
            self.ed.theme.cjk_width.round(),
        );
        let bg = self.ed.theme.background;
        let frame = DxFrame {
            cmds: &self.draw.cmds,
            overlay: &[],
            offset: (0.0, 0.0),
            shaper: measurer.inner_mut().shaper(),
            x_width,
            wide,
            measured: &self.ed.theme.measured,
            caret: None,
            show_emoji: self.show_emoji,
        };
        if self.view.paint(w as u32, h as u32, bg, frame) {
            return true;
        }
        // No surface to compose: erase the child's face so a dead device does
        // not leave the previous report frozen on screen.
        let dc = GetDC(Some(hwnd));
        if !dc.0.is_null() {
            let face = CreateSolidBrush(to_colorref(bg));
            let full = RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            };
            let _ = FillRect(dc, &full, face);
            let _ = DeleteObject(face.into());
            let _ = ReleaseDC(Some(hwnd), dc);
        }
        false
    }

    /// Places the text child over the band the pane's latest paint measured.
    unsafe fn place_view(&mut self) {
        if !self.alive() || !self.view.alive() {
            return;
        }
        let mut rc = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut rc);
        let w = (rc.right - rc.left).max(0);
        let h = (rc.bottom - rc.top).max(0);
        let band = self.band.round() as i32;
        let text_h = (h - band - 1).max(0);
        self.view.place(Rect::new(0.0, band as f32, w as f32, text_h as f32));
    }

    fn view_hwnd(&self) -> HWND {
        self.view.hwnd
    }
}

static REGISTER_CLASS: Once = Once::new();

unsafe extern "system" fn out_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let frame = match GetParent(hwnd) {
        Ok(f) if !f.0.is_null() => f,
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    let Some(st) = state_mut(frame) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    // The dock holds as many of these windows as 新建输出窗口 asked for, and a
    // message belongs to the one that received it — not to the visible pane.
    let Some(index) = st.panels.outputs.iter().position(|w| w.hwnd() == hwnd) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            // The rows a paint walks are the layout's, and the report's own
            // insert — or a 清除 — defers that layout, so the pane asks for it
            // first exactly as the frame's `paint` does on `layout_dirty`.
            // Painting without it indexes rows the document no longer has.
            if st.panels.outputs[index].needs_relayout() {
                st.relayout_output_at(index);
            }
            st.panels.outputs[index].paint(hwnd);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let out = &mut st.panels.outputs[index];
            let step = (wparam.0 >> 16) as u16 as i16 as i32 / 120 * WHEEL_LINES;
            let visible = out.visible_rows().max(1) as i32;
            let last = (out.ed.total_rows as i32 - visible).max(0);
            out.ed.first_row = (out.ed.first_row as i32 - step).clamp(0, last) as usize;
            out.sync_bars();
            out.invalidate();
            LRESULT(0)
        }
        // The pane's bar sends its rows here.
        WM_COMMAND => {
            let id = crate::win32::window::low_id(LPARAM(wparam.0 as isize));
            debug_assert!(
                super::toolbar::OUTPUT_ANSWERED.contains(&id),
                "the output bar offers a button this switch does not answer: {id}"
            );
            let jump = match id {
                1280 => {
                    st.panels.outputs[index].clear();
                    None
                }
                1320 => {
                    st.toggle_output_wrap_at(index);
                    None
                }
                1315 | 1316 => st.panels.outputs[index].step_anchor(id == 1315),
                // 1284 closes this output window: the last
                // pane of its host hides that host, a pane with neighbours is
                // dropped from the strip and destroyed.
                1284 => {
                    st.close_output_tab(frame, index);
                    None
                }
                // the frame owns the dock, so the new pane is its to add.
                1286 => {
                    let _ = SendMessageW(frame, WM_COMMAND, Some(WPARAM(1319)), None);
                    None
                }
                // 终结 ends the command the pane started; the run's
                // waiter closes it from `drain_tool`.
                1282 => {
                    st.terminate_tool();
                    None
                }
                // The control has no fold points by construction — the output pane
                // creator sends the marker style but never a folding method — so the
                // send is the whole mechanism and it can fold nothing here.
                1287 | 1288 => {
                    st.panels.outputs[index].fold_all(id == 1287);
                    None
                }
                _ => None,
            };
            if let Some((path, line, col)) = jump {
                // The anchor under the caret resolves to a file (the anchor resolve),
                // and the frame takes the caret back for it (`SetFocus`).
                st.open_target(path, line, col);
                let _ = SetFocus(Some(frame));
            }
            LRESULT(0)
        }
        // The bar's colours are answered by its parent, so the pane is the window
        // that sees its `NM_CUSTOMDRAW` — the same split the frame's bar has.
        WM_NOTIFY => {
            let nm = lparam.0 as *const NMHDR;
            if !nm.is_null() && unsafe { (*nm).hwndFrom } == st.panels.outputs[index].tb.hwnd() {
                if unsafe { (*nm).code } == NM_CUSTOMDRAW {
                    if let Some(r) = st.panels.outputs[index]
                        .tb
                        .custom_draw(lparam, &st.docs.act().theme)
                    {
                        return r;
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            st.panels.outputs[index].hwnd = HWND::default();
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn instance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).unwrap_or_default().into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pane's editor starts out as the control the output pane creator configures:
    /// the synthetic document's two-cell tab grid, and the seven view options it
    /// clears with the active line it turns on.
    #[test]
    fn the_output_pane_opens_with_its_own_option_set() {
        let mut win = OutputWin::new();
        win.apply_options();
        assert_eq!(win.ed.tab_size, 2);
        let flags = win.ed.view_flags;
        assert!(!flags.get(ViewFlags::LINE_NUMBER));
        assert!(!flags.get(ViewFlags::WHITESPACE));
        assert!(!flags.get(ViewFlags::END_OF_LINE));
        assert!(!flags.get(ViewFlags::LINE_STATE));
        assert!(!flags.get(ViewFlags::ICON_AREA));
        assert!(flags.get(ViewFlags::ACTIVE_LINE));
    }

    /// Appending never touches what is already there, and each report's own
    /// terminator leaves the line the next one starts on — the break guard in
    /// the line-break insert only fires when the last line still has text on it.
    #[test]
    fn append_lands_at_the_end_and_breaks_first() {
        let mut win = OutputWin::new();
        win.append("first\n");
        assert_eq!(win.lines(), vec!["first".to_string(), String::new()]);
        win.append("second\n");
        assert_eq!(
            win.lines(),
            vec!["first".to_string(), "second".to_string(), String::new()]
        );
        win.append("third");
        assert_eq!(win.lines().last().map(|s| s.as_str()), Some("third"));
    }

    /// A report lands at the top of a pane that has no rect yet: scrolling to
    /// the caret's page before there is a page is what buries the whole report
    /// under the empty line its own terminator leaves.
    #[test]
    fn a_report_stays_at_the_top_until_the_pane_has_a_rect() {
        let mut win = OutputWin::new();
        win.append("----统计信息---\n物理行数:1\n");
        assert_eq!(win.visible_rows(), 0);
        assert_eq!(win.ed.first_row, 0);
    }

    /// The caption follows the pane, not its place in the list: closing one
    /// never renames the ones that are left.
    #[test]
    fn the_caption_follows_the_pane_id() {
        assert_eq!(tab_title(0), "输出窗口");
        assert_eq!(tab_title(1), "输出窗口1");
        assert_eq!(tab_title(10), "输出窗口10");
    }

    /// the band measure hands the bar the top band and everything under it to the
    /// text, so a bar of 24 px costs the pane its first row and an off bar costs
    /// it nothing.
    #[test]
    fn the_bar_band_takes_rows_off_the_top_of_the_pane() {
        let mut win = OutputWin::new();
        win.ed.theme.line_height = 20.0;
        win.frame = Rect::new(0.0, 0.0, 400.0, 141.0);
        // 141 less the 1 px rule: seven rows.
        assert_eq!(win.visible_rows(), 7);
        win.band = 24.0;
        assert_eq!(win.visible_rows(), 5);
        win.band = 0.0;
        assert_eq!(win.visible_rows(), 7);
    }

    /// 清除 empties the buffer, and the report the pane held is
    /// gone rather than scrolled past.
    #[test]
    fn clear_empties_the_report() {
        let mut win = OutputWin::new();
        win.append("物理行数:12\n");
        win.append("字符数:100\n");
        assert_eq!(win.lines().len(), 3);
        win.clear();
        assert_eq!(win.ed.doc.total_bytes(), 0);
        assert_eq!(win.ed.caret, (0, 0));
        assert_eq!(win.ed.first_row, 0);
    }

    /// The next report still lands after a clear: the pane writes at the
    /// document's end, and an emptied buffer has to keep the one empty line that
    /// end is a position in.
    #[test]
    fn a_cleared_pane_takes_the_next_report() {
        let mut win = OutputWin::new();
        win.append("物理行数:12\n");
        win.clear();
        win.append("物理行数:3\n");
        assert_eq!(win.lines(), vec!["物理行数:3".to_string(), String::new()]);
    }

    /// A 清除 deletes the lines the report just inserted, and the two edits
    /// overlap on line 0 — the merge `Editor::apply_edit` records them with has
    /// no exact region for that, so it drops the record. A paint that asked the
    /// editor whether it owed a layout would then walk rows the document no
    /// longer has, which is what the pane's own flag is for.
    #[test]
    fn a_clear_leaves_the_pane_owed_a_layout_the_editor_cannot_name() {
        let report = "----统计信息---\n物理行数:3\n空白:2\n-----End-----\n";
        // One edit on its own does record the lines it touched.
        let mut one = OutputWin::new();
        one.append(report);
        assert_eq!(one.ed.take_layout_regions(), vec![(0, 1, 5)]);
        // The clear that follows deletes exactly those lines, and the merge has
        // no single region for an edit that overlaps one already recorded.
        let mut win = OutputWin::new();
        win.append(report);
        win.clear();
        assert!(
            win.ed.take_layout_regions().is_empty(),
            "the overlapping delete leaves the merge nothing to report"
        );
        assert!(win.needs_relayout());
    }

    /// 下一个结果 / 上一个结果 walk the `^(.*)\((\d+),(\d+)\):` rows and answer
    /// the file each names; a pane whose report carries no anchor — every
    /// 字符统计, for one — leaves the caret where it was.
    #[test]
    fn the_anchor_walk_stops_at_the_ends() {
        let mut win = OutputWin::new();
        win.append("E:\\x\\a.txt(12,5):first\n----统计信息---\nE:\\x\\b.txt(3,0):second\n");
        win.ed.caret = (0, 0);
        assert_eq!(
            win.step_anchor(true),
            Some(("E:\\x\\b.txt".to_string(), 2, 0))
        );
        assert_eq!(win.ed.caret, (2, 0));
        assert_eq!(win.step_anchor(true), None);
        assert_eq!(win.ed.caret, (2, 0));
        assert_eq!(
            win.step_anchor(false),
            Some(("E:\\x\\a.txt".to_string(), 11, 5))
        );
        assert_eq!(win.ed.caret, (0, 0));
        assert_eq!(win.step_anchor(false), None);

        let mut plain = OutputWin::new();
        plain.append("物理行数:12\n");
        plain.ed.caret = (0, 0);
        assert_eq!(plain.step_anchor(true), None);
        assert_eq!(plain.ed.caret, (0, 0));
    }

    /// A streaming chunk lands where the previous one stopped: the report's
    /// break belongs to the header, not to every pipe read.
    #[test]
    fn a_stream_chunk_continues_the_line() {
        let mut win = OutputWin::new();
        win.append("运行:工具\n");
        win.append_stream("line one");
        win.append_stream(" continues");
        assert_eq!(
            win.lines(),
            vec!["运行:工具".to_string(), "line one continues".to_string()]
        );
        assert!(!win.ends_with_break());
        win.append_stream("\n");
        assert!(win.ends_with_break());
        assert_eq!(win.lines().len(), 3);
    }

    /// 折叠全部 waits for the layout that has the wrapper and the measurer, then
    /// spends the request.
    #[test]
    fn a_fold_request_is_spent_by_the_next_layout() {
        use dg_layout::measure::{CachedMeasurer, TableMeasurer};
        let mut win = OutputWin::new();
        win.frame = Rect::new(0.0, 0.0, 400.0, 200.0);
        win.append("a\n  b\n    c\n");
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        win.fold_all(true);
        assert_eq!(win.pending_fold, Some(true));
        win.relayout(&mut measurer, 8, [10.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(win.pending_fold, None);
        assert!(!win.needs_relayout());
    }

    /// 自动换行 answers with the mode it left the pane in, which is what the frame writes to
    /// `[Output] Wrap`. The mode is what the layout wrapper is chosen from, so the flip is
    /// the whole command.
    #[test]
    fn wrap_toggle_reports_the_new_mode() {
        let mut win = OutputWin::new();
        assert!(win.wrap);
        assert!(!win.toggle_wrap());
        assert!(!win.wrap);
        assert!(win.toggle_wrap());
    }
}
