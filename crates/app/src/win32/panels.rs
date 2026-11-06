//! Win32 host glue for the docked panels: dock geometry, the child windows that
//! fill the docks, and the background workers that feed them.
//!
//! The panels themselves are windows now — [`crate::win32::dirtree`],
//! [`crate::win32::resultlist`], [`crate::win32::statusbar`] and
//! [`crate::win32::overlay`] — so this file routes nothing. Each of those modules
//! owns its own hit-testing, selection, scrolling and keyboard handling, because
//! that is what a window with a message queue gets for free, and each answers
//! `NM_CUSTOMDRAW`/`WM_ERASEBKGND` with `Theme` colours, so none of them gives up
//! the look to get it.
//!
//! What is left here is the part no window can decide for itself: the pane table
//! (`hosts`), the one rule that docks, shows, hides and closes a pane, and the
//! partition of the client rectangle (`layout`).
//!
//! Every docked pane is a tab of the host on its side — there is one host per
//! side, and a host is a caption strip plus whichever of its panes the strip
//! selected. The directory tree, the open-document list, the search results and
//! every output window are therefore the same kind of thing here, which is what
//! keeps them from drifting apart: they differ in their caption and their
//! content, never in how they are docked, switched or closed.

use crate::win32::dirtree::DockTree;
use crate::win32::docktabs::{DockTabs, TabEntry};
use crate::win32::opendocs::OpenDocsWin;
use crate::win32::overlay::{self, OverlayBars};
use crate::win32::outputwin::OutputWin;
use crate::win32::outlinewin::OutlineWin;
use crate::win32::resultlist::{ResultGroup, ResultListWin};
use crate::win32::bookmarkwin::BookmarkWin;
use crate::win32::snippetwin::SnippetWin;
use crate::win32::skinpane::SkinPane;
use crate::win32::statusbar::StatusWin;
use crate::win32::tabstrip::TabStrip;
use crate::win32::toolbar::ToolbarWin;
use dg_core::FileWatcher;
use dg_core::filesearch::FileMatch;
use dg_render::Rect;
use dg_render::theme::Theme;
use dg_ui::panel::{DockPanel, PanelSide};
use dg_ui::statusbar::StatusBar;

/// A pane the dock can hold. The id outlives a dock/undock cycle and a closed
/// output window, so a caption click, a command and a running tool all name the
/// same pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaneId {
    /// 快捷目录: the directory tree.
    DirView,
    /// 文件列表: the open documents.
    FileList,
    /// 查找结果: the find-in-files groups.
    Results,
    /// 大纲视图: the symbols of the document on screen.
    Outline,
    /// 代码片段: the snippets the document's language ships.
    Snippet,
    /// 书签管理器: the bookmarks of every open document.
    Bookmark,
    /// 输出窗口, by the id the window was created with.
    Output(u64),
    /// 脚本窗格（`s:pane{...}`），by the index it was created with.
    ///
    /// 和输出窗口一样是「数量不定」的那一类，所以它的侧边和厚度存在窗口自己
    /// 身上（[`crate::win32::skinpane::SkinPane`]），不在 `fixed_side` 里。
    Script(u64),
}

/// The code a caption strip reports a pane with. Output panes start at
/// [`OUTPUT_BASE`] so the fixed panes never collide with them; script panes
/// above that so a skin can add as many output windows as it likes.
const OUTPUT_BASE: u64 = 100;
const SCRIPT_BASE: u64 = 1000;

impl PaneId {
    pub(super) fn code(self) -> u64 {
        match self {
            PaneId::DirView => 0,
            PaneId::FileList => 1,
            PaneId::Results => 2,
            PaneId::Outline => 3,
            PaneId::Snippet => 4,
            PaneId::Bookmark => 5,
            PaneId::Output(id) => OUTPUT_BASE + id,
            PaneId::Script(id) => SCRIPT_BASE + id,
        }
    }

    pub(super) fn from_code(code: u64) -> Option<PaneId> {
        match code {
            0 => Some(PaneId::DirView),
            1 => Some(PaneId::FileList),
            2 => Some(PaneId::Results),
            3 => Some(PaneId::Outline),
            4 => Some(PaneId::Snippet),
            5 => Some(PaneId::Bookmark),
            n if n >= SCRIPT_BASE => Some(PaneId::Script(n - SCRIPT_BASE)),
            n if n >= OUTPUT_BASE => Some(PaneId::Output(n - OUTPUT_BASE)),
            _ => None,
        }
    }

    /// The cell's caption. The fixed panes take theirs from the command that
    /// toggles them, so a translated label widens its cell instead of being
    /// clipped.
    ///
    /// `script` is the script panes' `(title, icon cell)` table: `PaneId` cannot
    /// reach the windows (the enum is `Copy` and carries no context), so the one
    /// caller that builds captions hands the two values in.
    pub(super) fn label(self, script: &[(String, i32)]) -> String {
        match self {
            PaneId::DirView => crate::win32::lang::pane_title(1292, "快捷目录"),
            PaneId::FileList => crate::win32::lang::pane_title(1291, "文件列表"),
            // The find-in-files pane has no command of its own: it is where the
            // groups land, and the strip has to name it.
            PaneId::Results => "查找结果".to_string(),
            PaneId::Outline => crate::win32::lang::pane_title(1312, "大纲视图"),
            PaneId::Snippet => crate::win32::lang::pane_title(1329, "代码片段"),
            PaneId::Bookmark => crate::win32::lang::pane_title(1289, "书签管理器"),
            PaneId::Output(id) => crate::win32::outputwin::tab_title(id),
            PaneId::Script(id) => script
                .get(id as usize)
                .map(|s| s.0.clone())
                .unwrap_or_else(|| "脚本窗格".to_string()),
        }
    }

    /// The sprite cell this pane's caption draws. Every pane is a command the
    /// toolbar can show, so its caption takes that command's own glyph: the tab
    /// and the button that opened it are one picture of the pane.
    fn icon(self, script: &[(String, i32)]) -> Option<i32> {
        let command = match self {
            PaneId::DirView => 1292,
            PaneId::FileList => 1291,
            // The results pane is where 在文件中查找 lands, so it wears that
            // command's glyph rather than one of its own.
            PaneId::Results => 1408,
            PaneId::Outline => 1312,
            PaneId::Snippet => 1329,
            PaneId::Bookmark => 1289,
            PaneId::Output(_) => 1325,
            // A script pane names a sprite cell or nothing at all.
            PaneId::Script(id) => {
                let cell = script.get(id as usize).map(|s| s.1).unwrap_or(0);
                return if cell == 0 {
                    None
                } else {
                    crate::win32::toolbar::image_of(cell as u32)
                };
            }
        };
        crate::win32::toolbar::image_of(command)
    }
}

/// One side host: its caption strip, the panes docked there in tab order, and
/// the one its strip has selected. The host's own visibility and thickness live
/// in [`DockPanel`], which is what the layout and the splitters read.
pub(super) struct Host {
    pub tabs: DockTabs,
    pub panes: Vec<PaneId>,
    pub active: Option<PaneId>,
}

impl Host {
    fn new() -> Self {
        Self {
            tabs: DockTabs::new(),
            panes: Vec::new(),
            active: None,
        }
    }
}

/// 一格的两条覆盖滚动条：竖条贴那一格的右边、停在横条之上，横条贴它的
/// 底边 —— 和 `DockPanel::vscroll_rect`/`hscroll_rect` 给整个编辑区算的是
/// 同一条规则，只是把「整个编辑区」换成「这一格」。
fn cell_bar_rects(cell: Rect, strip: f32) -> (Rect, Rect) {
    (
        Rect::new(
            cell.x + cell.w - strip,
            cell.y,
            strip,
            (cell.h - strip).max(0.0),
        ),
        Rect::new(cell.x, cell.y + cell.h - strip, cell.w, strip),
    )
}

/// The docked panels.
pub(super) struct Panels {
    pub dock: DockPanel,
    pub(super) bars: OverlayBars,
    /// 第二分组那格（分割开着时）自己的两条滚动条。未分割时它压在零矩形上，
    /// `OverlayBars::place` 把没有面积的条藏起来。
    pub(super) bars2: OverlayBars,
    /// 主分组这一格整个藏起来（一个页签也不剩，全部页签都在第二分组里）。
    /// 由框架按它画窗格用的同一份成员表报过来，布局只照它摆。
    pub(super) main_cell_hidden: bool,
    /// The strip's pane table: which panes exist, in what order, with what
    /// text. [`Panels::status_win`] is the window that shows it.
    pub status: StatusBar,
    pub(super) status_win: StatusWin,
    /// The main toolbar (ToolbarWindow32), a sibling of the
    /// editor window: the dock reserves its band (	op_dock_h),
    /// and this window fills it.
    pub(super) toolbar_win: ToolbarWin,
    /// The document tab strip (`the tab control`), a sibling of the editor like
    /// the toolbar: the dock reserves its row (under the toolbar band), and
    /// this window fills it.
    pub(super) tab_win: TabStrip,
    /// 分割后的第二条页签条：第二分组自己的页签摆它那里。未分割时窗口
    /// 在但不占位（零矩形等于藏起）。与 `tab_win` 同类同路：点击、拖
    /// 动、关闭、右键菜单都走同一套，按消息落在哪条上分派。
    pub(super) tab_win2: TabStrip,
    /// 分割时两条页签条的摆法：Some 记着分割的方向、比例和第二条占的
    /// 行高（0 = 那条此刻不显示，比如 仅一个时隐藏 开着而第二分组只有
    /// 一页）；None = 未分割，主条照旧占满整条 band。
    pub(super) tab_split: Option<(bool, f32, f32)>,
    /// The four hosts, in [`PanelSide::slot`] order.
    pub(super) hosts: [Host; 4],
    /// The side each fixed pane was last docked to. Output panes carry theirs
    /// in the window itself, because there can be any number of them.
    fixed_side: [PanelSide; 6],
    pub(super) dock_tree: DockTree,
    /// The 文件列表 pane: the documents that are open, as a list.
    pub(super) files_win: OpenDocsWin,
    pub(super) results_win: ResultListWin,
    /// The 大纲视图 pane: the symbols of the document on screen, as a tree.
    pub(super) outline_win: OutlineWin,
    /// The 代码片段 pane: the snippets the document's language ships.
    pub(super) snippet_win: SnippetWin,
    /// The 书签管理器 pane: the bookmarks of every open document, as a tree.
    pub(super) bookmark_win: BookmarkWin,
    /// The 输出窗口 panes, in creation order: 新建输出窗口 adds one, and each
    /// keeps its own buffer, caret, bar and toolbar.
    pub(super) outputs: Vec<OutputWin>,
    /// 脚本窗格（`s:pane{...}`），按皮肤声明顺序。下标就是
    /// `PaneId::Script(n)` 里的 `n`。
    pub(super) script_panes: Vec<SkinPane>,
    /// Which output pane commands act on. It is not the host's selection: a
    /// command names the pane it last wrote to, whether or not that pane is
    /// the one its strip shows.
    pub(super) active_output: usize,
    /// The id the next output pane gets. Ids outlive a closed pane, so a run
    /// can still name the pane it belongs to after the captions renumber.
    pub(super) next_output_id: u64,
    /// Where-field of the
    /// find-in-files bar.
    pub find_where: String,
    /// Find bar runs a files search
    /// (Ctrl+Shift+F) instead of an
    /// in-buffer search.
    pub find_files_mode: bool,
    /// Receiver of the background find-in-files result groups.
    pub search_rx: Option<crossbeam_channel::Receiver<Vec<ResultGroup>>>,
    /// Directory watcher feeding the left dock tree.
    pub watcher: dg_core::PollingWatcher,
    /// Roots the watcher follows — one per row of 快捷目录.
    pub watch_roots: Vec<String>,
}

impl Panels {
    pub(super) fn new() -> Self {
        let mut status = StatusBar::new();
        status.set_labels(crate::win32::lang::status_labels());
        Self {
            dock: DockPanel::default(),
            bars: OverlayBars::new(),
            bars2: OverlayBars::new(),
            main_cell_hidden: false,
            status,
            status_win: StatusWin::new(),
            toolbar_win: ToolbarWin::new(),
            tab_win: TabStrip::new(),
            tab_win2: TabStrip::new(),
            tab_split: None,
            hosts: [
                Host::new(),
                Host::new(),
                Host::new(),
                Host::new(),
            ],
            // The fixed panes remember the side they were moved to, so a pane
            // that is closed and reopened comes back where the user left it.
            fixed_side: [
                PanelSide::Left,
                PanelSide::Left,
                PanelSide::Bottom,
                PanelSide::Left,
                PanelSide::Left,
                PanelSide::Left,
            ],
            dock_tree: DockTree::new(),
            files_win: OpenDocsWin::new(),
            results_win: ResultListWin::new(),
            outline_win: OutlineWin::new(),
            snippet_win: SnippetWin::new(),
            bookmark_win: BookmarkWin::new(),
            outputs: vec![OutputWin::new()],
            script_panes: Vec::new(),
            active_output: 0,
            next_output_id: 1,
            find_where: String::new(),
            find_files_mode: false,
            search_rx: None,
            watcher: dg_core::PollingWatcher::new(),
            watch_roots: Vec::new(),
        }
    }

    /// The host on `side`.
    pub(super) fn host(&self, side: PanelSide) -> &Host {
        &self.hosts[side.slot()]
    }

    fn host_mut(&mut self, side: PanelSide) -> &mut Host {
        &mut self.hosts[side.slot()]
    }

    /// Which host a caption strip belongs to. The strip's window proc needs it
    /// to know whose pane table a click is about.
    pub(super) fn host_of_tabs(&self, hwnd: windows::Win32::Foundation::HWND) -> Option<usize> {
        self.hosts.iter().position(|h| h.tabs.hwnd() == hwnd)
    }

    /// The side a pane is docked to, or the side it would dock to. A pane that
    /// is not in any host still answers, because that is where a command that
    /// reopens it puts it.
    pub(super) fn pane_side(&self, id: PaneId) -> PanelSide {
        if let PaneId::Script(script) = id {
            return self
                .script_panes
                .get(script as usize)
                .map(|w| w.side())
                .unwrap_or(PanelSide::Bottom);
        }
        if let PaneId::Output(out) = id {
            if let Some(win) = self.outputs.iter().find(|w| w.id() == out) {
                return win.side();
            }
            return PanelSide::Bottom;
        }
        for side in PanelSide::ALL {
            if self.host(side).panes.contains(&id) {
                return side;
            }
        }
        self.fixed_side[id.fixed_slot()]
    }

    fn set_pane_side(&mut self, id: PaneId, side: PanelSide) {
        match id {
            PaneId::Output(out) => {
                if let Some(win) = self.outputs.iter_mut().find(|w| w.id() == out) {
                    win.set_side(side);
                }
            }
            PaneId::Script(script) => {
                if let Some(win) = self.script_panes.get_mut(script as usize) {
                    win.set_side(side);
                }
            }
            _ => self.fixed_side[id.fixed_slot()] = side,
        }
    }

    /// 把皮肤声明的窗格和现有的对齐，返回**这一次新建**的下标。
    ///
    /// 名字是身份：同名的窗口留着（热重载不该抹掉焦点和滚动位置），只把标题、
    /// 图标、厚度、边换一遍；皮肤删掉的那几块拆窗口、从宿主里摘出去；新出现的
    /// 建窗口。调用方拿返回值去 `dock_pane` —— 只有新窗格才停靠，用户手动关掉
    /// 的那一块重载后不该自己冒出来。
    ///
    /// 返回 `(新建, 换了边)`。换了边的那几块必须重新停靠：宿主表里记的还是旧
    /// 那一边，不搬的话 `pane_showing` 按新边找不到它，窗格会直接消失。
    pub(super) unsafe fn sync_script_panes(
        &mut self,
        parent: windows::Win32::Foundation::HWND,
        specs: &[dg_script::PaneSpec],
        dpi: i32,
    ) -> (Vec<usize>, Vec<usize>) {
        let old_len = self.script_panes.len();
        let mut old = std::mem::take(&mut self.script_panes);
        let mut next: Vec<SkinPane> = Vec::with_capacity(specs.len());
        let mut created = Vec::new();
        let mut moved = Vec::new();
        for (i, spec) in specs.iter().enumerate() {
            let side = side_named(&spec.side);
            let mut win = match old.iter().position(|w| w.name() == spec.id) {
                Some(pos) => old.remove(pos),
                None => SkinPane::new(),
            };
            let fresh = !win.alive();
            let was = win.side();
            unsafe { win.describe(parent, i, spec, side, dpi) };
            if fresh && win.alive() {
                created.push(i);
            } else if was != side {
                moved.push(i);
            }
            next.push(win);
        }
        for mut gone in old {
            unsafe { gone.destroy() };
        }
        let len = next.len();
        self.script_panes = next;
        // 被删掉的那几块还挂在某个宿主的页签表里，得先摘出去 —— 下标已经
        // 不存在了，页签画到它会按「没有这一块」取到空标题。
        for i in len..old_len {
            self.undock_pane(PaneId::Script(i as u64));
        }
        (created, moved)
    }

    /// Whether the pane is docked in a host at all.
    pub(super) fn pane_docked(&self, id: PaneId) -> bool {
        PanelSide::ALL
            .iter()
            .any(|s| self.host(*s).panes.contains(&id))
    }

    /// Whether the pane is the one its host is showing.
    pub(super) fn pane_showing(&self, id: PaneId) -> bool {
        let side = self.pane_side(id);
        self.dock.side_visible(side) && self.host(side).active == Some(id)
    }

    /// Puts `pane` on screen: docks it on `side` first when it is not in a host
    /// yet, or when the command moves it there.
    pub(super) fn dock_pane(&mut self, pane: PaneId, side: PanelSide) {
        // A pane is a tab of exactly one host: the move is a removal plus an
        // insertion, never a second copy.
        for from in PanelSide::ALL {
            let slot = from.slot();
            let Some(pos) = self.hosts[slot].panes.iter().position(|p| *p == pane) else {
                continue;
            };
            if from == side {
                self.hosts[slot].active = Some(pane);
                self.dock.host_mut(side).visible = true;
                return;
            }
            self.hosts[slot].panes.remove(pos);
            if self.hosts[slot].active == Some(pane) {
                // The host falls back to the pane that took the closed tab's
                // place, or to the one before it.
                let next = pos.min(self.hosts[slot].panes.len().saturating_sub(1));
                self.hosts[slot].active = self.hosts[slot].panes.get(next).copied();
            }
            if self.hosts[slot].panes.is_empty() {
                self.hosts[slot].active = None;
                self.dock.host_mut(from).visible = false;
            }
        }
        let host = self.host_mut(side);
        host.panes.push(pane);
        host.active = Some(pane);
        self.dock.host_mut(side).visible = true;
        self.set_pane_side(pane, side);
    }

    /// Takes a pane out of its host. The host falls back to a neighbour and
    /// closes with its last pane; the pane keeps its state and comes back where
    /// it was.
    pub(super) fn undock_pane(&mut self, pane: PaneId) {
        for side in PanelSide::ALL {
            let slot = side.slot();
            let Some(pos) = self.hosts[slot].panes.iter().position(|p| *p == pane) else {
                continue;
            };
            self.hosts[slot].panes.remove(pos);
            if self.hosts[slot].active == Some(pane) {
                let next = pos.min(self.hosts[slot].panes.len().saturating_sub(1));
                self.hosts[slot].active = self.hosts[slot].panes.get(next).copied();
            }
            if self.hosts[slot].panes.is_empty() {
                self.hosts[slot].active = None;
                self.dock.host_mut(side).visible = false;
            }
            return;
        }
    }

    /// One command over a pane: the pane on screen goes away, one that is
    /// docked behind another comes up, one that is not docked at all is docked
    /// and comes up. Both 停靠窗格 rows, the caption menu and the caption
    /// strip's own ✕ all land here.
    pub(super) fn toggle_pane(&mut self, pane: PaneId) {
        if self.pane_showing(pane) {
            self.undock_pane(pane);
        } else {
            self.show_pane(pane);
        }
    }

    /// Brings a pane up on the side it belongs to.
    pub(super) fn show_pane(&mut self, pane: PaneId) {
        let side = self.pane_side(pane);
        self.dock_pane(pane, side);
        if let PaneId::Output(id) = pane {
            if let Some(i) = self.outputs.iter().position(|w| w.id() == id) {
                self.active_output = i;
            }
        }
    }

    /// The side the command's current output window lives on.
    pub(super) fn active_output_side(&self) -> PanelSide {
        self.outputs
            .get(self.active_output)
            .map(|w| w.side())
            .unwrap_or(PanelSide::Bottom)
    }

    /// The output pane commands act on.
    pub(super) fn output(&self) -> &OutputWin {
        let i = self.active_output.min(self.outputs.len().saturating_sub(1));
        &self.outputs[i]
    }

    /// The output pane commands act on: the visible one.
    pub(super) fn output_mut(&mut self) -> &mut OutputWin {
        let i = self.active_output.min(self.outputs.len().saturating_sub(1));
        &mut self.outputs[i]
    }

    /// Recomputes the dock partition from the client size and places every dock
    /// window in it.
    ///
    /// The theme owns the strip thickness, so a theme switch also arrives here.
    pub(super) fn layout(&mut self, w: i32, h: i32, theme: &Theme) {
        let client = Rect::new(0.0, 0.0, w as f32, h as f32);
        let strip = OverlayBars::strip(theme);
        self.dock.scrollbar_w = strip;
        self.dock.scrollbar_h = strip;
        // 多行显示: a wrapping bar's band height is the row count the control
        // reports at this width, which the fixed square cannot predict.
        if self.dock.top_dock_h > 0.0 {
            self.dock.top_dock_h = unsafe { self.toolbar_win.band_h_at_width(w) };
        }
        // The caption row is drawn with the document strip's metrics, so it
        // takes the height those measured for this font.
        self.dock.strip_h = self
            .hosts
            .iter()
            .map(|h| h.tabs.row_h())
            .fold(0.0f32, f32::max);
        // 分割把编辑区切成两格，每格的滚动条按**那一格**的边摆。格子的算法
        // 和框架画窗格用的是同一个函数；主分组一个页签也不剩时那一格整个
        // 藏起来，第二分组占满编辑区。未分割时第 0 格就是整个编辑区，公式
        // 和 `DockPanel::vscroll_rect` 是同一条。
        let ed = self.dock.editor_rect(client);
        let zero = Rect::new(0.0, 0.0, 0.0, 0.0);
        let (cell0, cell1) = match self.tab_split {
            None => (ed, zero),
            Some((vertical, ratio, second_h)) => {
                if self.main_cell_hidden {
                    (zero, ed)
                } else {
                    let pair =
                        crate::win32::window::split_pane_rects_in(ed, vertical, ratio, second_h).0;
                    (pair[0], pair[1])
                }
            }
        };
        let bar = OverlayBars::strip(theme);
        let (v0, h0) = cell_bar_rects(cell0, bar);
        let (v1, h1) = cell_bar_rects(cell1, bar);
        unsafe {
            self.bars.place(v0, h0);
            self.bars2.place(v1, h1);
            self.bars.apply_theme(theme);
            self.bars2.apply_theme(theme);
            // The strip is a window now: its rect is the dock's strip, and its
            // parts are the model's pane table. Widths are measured from the
            // text inside the control
            // so the editor font's advance is not part of the geometry.
            self.status_win
                .layout(&mut self.status, self.dock.status_rect(client));
            // The toolbar band is the dock's own row — the top one, or the one
            // above the status strip once 下 has moved it down — and a hidden
            // bar leaves a zero-height row behind.
            self.toolbar_win.place(self.dock.toolbar_rect(client));
            // The dropdown arrow is the control's own drawing, so the theme
            // switch has to move the window's visual style with it.
            self.toolbar_win
                .set_dark(crate::win32::chrome::theme_is_dark(theme));
            // The tab row is the dock's second band, and the strip fills it the
            // same way the toolbar fills its own: `dock.tab_h` is the row the
            // host reserved, which is 0 when the bar is off or holds one
            // document, and a zero-height window is never painted.
            // 分割后页签条跟着分组走：左右两块时整条 band 从分割带上方
            // 切成两段，各归各的页签条；上下两块时主条照旧占 band，第二
            // 分组那条占 1 号窗格上方让出的一行（行高随它自己的条目数，
            // 仅一个页签的设置照旧管它 —— 行高为 0 就不占位）。未分割时
            // 主条占满 band，第二条压在零矩形上。
            let band = self.dock.tab_rect(client);
            let zero = Rect::new(0.0, 0.0, 0.0, 0.0);
            match self.tab_split {
                None => {
                    self.tab_win.place(band);
                    self.tab_win2.place(zero);
                }
                Some((vertical, ratio, second_h)) => {
                    if self.main_cell_hidden {
                        // 主分组一个页签也不剩：那条页签条没有可摆的，整条
                        // band 归第二分组，它那一格也占满编辑区。
                        self.tab_win.place(zero);
                        self.tab_win2.place(band);
                    } else if second_h <= 0.0 {
                        self.tab_win.place(band);
                        self.tab_win2.place(zero);
                    } else if vertical {
                        let editor = self.dock.editor_rect(client);
                        let d = crate::win32::window::EditorState::PANE_DIVIDER_PX;
                        let w = (editor.w - d).max(0.0);
                        let cut = editor.x + (w * ratio.clamp(0.1, 0.9)).round();
                        self.tab_win.place(Rect::new(
                            client.x,
                            band.y,
                            (cut - client.x).max(0.0),
                            band.h,
                        ));
                        self.tab_win2.place(Rect::new(
                            cut,
                            band.y,
                            (client.x + client.w - cut).max(0.0),
                            band.h,
                        ));
                    } else {
                        let editor = self.dock.editor_rect(client);
                        let d = crate::win32::window::EditorState::PANE_DIVIDER_PX;
                        let h = (editor.h - d).max(0.0);
                        let ah = (h * ratio.clamp(0.1, 0.9)).round();
                        self.tab_win.place(band);
                        self.tab_win2.place(Rect::new(
                            editor.x,
                            editor.y + ah + d,
                            editor.w,
                            second_h,
                        ));
                    }
                }
            }
            // 两条页签条互相认识：拖页签时落在对方那一段上就是把这一页换进
            // 那个分组，光标和松手都按这一句判断。第二条没摆（压在零矩形
            // 上）时那段矩形是空的，判断自然不成立。
            self.tab_win.set_other(self.tab_win2.hwnd());
            self.tab_win2.set_other(self.tab_win.hwnd());
            self.dock_tree.apply_theme(theme);
            self.results_win.apply_theme(theme);
            self.outline_win.apply_theme(theme);
            self.snippet_win.apply_theme(theme);
            self.bookmark_win.apply_theme(theme);
            self.files_win.apply_theme(theme);
            let empty = Rect::new(0.0, 0.0, 0.0, 0.0);
            // One pass over the four hosts: the strip takes its row, the pane
            // the strip selected takes the rest. A closed host gets empty rects
            // for both, which is how `place` hides a window without destroying
            // it.
            for side in PanelSide::ALL {
                let slot = side.slot();
                let visible = self.dock.side_visible(side);
                let strip_rect = if visible {
                    self.dock.strip_rect(side, client)
                } else {
                    empty
                };
                self.hosts[slot].tabs.apply_theme(theme);
                self.hosts[slot].tabs.place(strip_rect);
            }
            self.sync_tabs();
            let dir = self.pane_rect(PaneId::DirView, client);
            let files = self.pane_rect(PaneId::FileList, client);
            let results = self.pane_rect(PaneId::Results, client);
            let outline = self.pane_rect(PaneId::Outline, client);
            let snippet = self.pane_rect(PaneId::Snippet, client);
            let bookmark = self.pane_rect(PaneId::Bookmark, client);
            let out_rects: Vec<Rect> = self
                .outputs
                .iter()
                .map(|win| self.pane_rect(PaneId::Output(win.id()), client))
                .collect();
            self.dock_tree.place(dir);
            self.files_win.place(files);
            self.results_win.place(results);
            self.outline_win.place(outline);
            self.snippet_win.place(snippet);
            self.bookmark_win.place(bookmark);
            // The pane is a second instance of the edit control, so it gets its
            // own strip on its host's edge rather than borrowing the document
            // view's.
            for (i, win) in self.outputs.iter_mut().enumerate() {
                win.apply_theme(theme);
                let rect = out_rects.get(i).copied().unwrap_or(empty);
                win.place(rect);
                win.place_bars(rect, strip, theme);
            }
            // 脚本窗格：和上面那几块一样，只是内容归皮肤画。空矩形 = 藏起来。
            let script_rects: Vec<Rect> = self
                .script_panes
                .iter()
                .enumerate()
                .map(|(i, _)| self.pane_rect(PaneId::Script(i as u64), client))
                .collect();
            for (i, win) in self.script_panes.iter_mut().enumerate() {
                win.apply_theme(theme);
                win.place(script_rects.get(i).copied().unwrap_or(empty));
            }
        }
    }

    /// The rect a pane fills right now: its host's pane area while the host
    /// shows it, an empty rect otherwise.
    fn pane_rect(&self, id: PaneId, client: Rect) -> Rect {
        if !self.pane_showing(id) {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        self.dock.pane_rect(self.pane_side(id), client)
    }

    /// Hands every strip the panes docked in its host, in dock order, and which
    /// of them is on screen. Called on every layout, so a pane that is docked,
    /// undocked or switched needs no separate repaint.
    fn sync_tabs(&mut self) {
        // 脚本窗格的标题和图标格号先摘出来：`PaneId` 是 `Copy` 的纯值，
        // 够不到窗口表，而下面循环里 `host` 已经借走了 `self.hosts`。
        let script: Vec<(String, i32)> = self
            .script_panes
            .iter()
            .map(|p| (p.title().to_string(), p.icon_cell()))
            .collect();
        for side in PanelSide::ALL {
            let host = &mut self.hosts[side.slot()];
            let entries: Vec<TabEntry> = host
                .panes
                .iter()
                .map(|id| TabEntry {
                    id: id.code(),
                    label: id.label(&script),
                    icon: id.icon(&script),
                })
                .collect();
            let active = host
                .active
                .and_then(|a| host.panes.iter().position(|p| *p == a))
                .unwrap_or(0);
            host.tabs.set_entries(&entries, active);
        }
    }

    /// Asks every script pane to repaint once. The frame is a `WS_CLIPCHILDREN`
    /// parent, so invalidating it leaves these child windows frozen — an
    /// animated pane needs its own `InvalidateRect`.
    pub(super) fn repaint_script_panes(&self) {
        for pane in &self.script_panes {
            pane.invalidate();
        }
    }

    /// 作废一块点名的脚本窗格。返回 `false` 表示没有这块窗格，调用方该退回
    /// 全量重画。区域名是 `pane.<id>`，这里收的只是 `<id>`。
    pub(super) fn invalidate_script_pane(&self, id: &str) -> bool {
        match self
            .script_panes
            .iter()
            .find(|pane| pane.name().eq_ignore_ascii_case(id))
        {
            Some(pane) => {
                pane.invalidate();
                true
            }
            None => false,
        }
    }

    /// Asks every scroll strip on screen to repaint once — the editor's pair and
    /// each pane's. This is the thaw after a splitter drag: the strips drew
    /// nothing while the frame was locked, so this is the paint that lands.
    pub(super) unsafe fn repaint_bars(&mut self) {
        unsafe {
            self.bars.repaint();
            self.bars2.repaint();
            for win in &mut self.outputs {
                win.bars.repaint();
            }
        }
    }

    /// Hands the frame's 显示颜文字 switch to every output pane — closed ones
    /// included, so a pane that comes back has the current mode.
    pub(super) fn set_show_emoji(&mut self, on: bool) {
        for win in &mut self.outputs {
            win.set_show_emoji(on);
        }
    }

    /// Editor area origin in client
    /// coordinates (the left host
    /// shifts it right).
    pub(super) fn editor_origin(&self) -> (f32, f32) {
        let x = if self.dock.side_visible(PanelSide::Left) {
            self.dock.side_size(PanelSide::Left)
        } else {
            0.0
        };
        (x, 0.0)
    }

    /// Refreshes both scrollbar models from
    /// the editor state, reporting which axis
    /// the *document* moved on: a bar reflecting
    /// its own drag must not re-arm the fade it
    /// is sitting under.
    /// 第 `cell` 格的那对滚动条。
    pub(super) fn bars_of(&mut self, cell: usize) -> &mut OverlayBars {
        if cell == 1 {
            &mut self.bars2
        } else {
            &mut self.bars
        }
    }

    pub(super) fn sync_scrollbars(
        &mut self,
        cell: usize,
        first_row: usize,
        total_rows: usize,
        visible_rows: usize,
        h_scroll: i32,
        h_range: i32,
        h_page: i32,
    ) -> (bool, bool) {
        let range = total_rows.saturating_sub(visible_rows.max(1)) as i32;
        let bars = self.bars_of(cell);
        let moved_v = bars.set_model(overlay::V, first_row as i32, range, visible_rows as i32);
        let moved_h = bars.set_model(overlay::H, h_scroll, h_range, h_page);
        (moved_v, moved_h)
    }

    /// Drains the find-in-files
    /// result channel; returns true
    /// when new results arrived.
    pub(super) fn drain_search(&mut self) -> bool {
        let Some(rx) = &self.search_rx else {
            return false;
        };
        let mut arrived = false;
        while let Ok(groups) = rx.try_recv() {
            self.results_win.set_results(groups);
            arrived = true;
        }
        if arrived {
            self.show_pane(PaneId::Results);
        }
        arrived
    }

    /// Polls the directory watcher;
    /// returns true when the tree
    /// changed.
    pub(super) fn poll_watcher(&mut self) -> bool {
        if !self.pane_docked(PaneId::DirView) {
            return false;
        }
        let events = self.watcher.poll();
        if events.is_empty() {
            return false;
        }
        self.dock_tree.refresh();
        true
    }

    /// Starts watching every root of the
    /// directory view. The watcher keeps
    /// one entry per path, so a second call
    /// only adds — which is what the list
    /// itself does.
    pub(super) fn watch_roots(&mut self, roots: &[String]) {
        for root in roots {
            self.watcher.watch(root);
        }
        self.watch_roots = roots.to_vec();
    }

    /// Groups matches by file path,
    /// preserving first-seen order.
    pub(super) fn group_matches(matches: &[FileMatch]) -> Vec<ResultGroup> {
        let mut groups: Vec<ResultGroup> = Vec::new();
        for m in matches {
            match groups.iter_mut().find(|g| g.path == m.path) {
                Some(g) => g.matches.push(m.clone()),
                None => {
                    let mut g = ResultGroup::new(m.path.clone());
                    g.matches.push(m.clone());
                    groups.push(g);
                }
            }
        }
        groups
    }
}

/// 皮肤写的边名 → `PanelSide`。`s:pane` 那边已经校验过取值范围，这里认不
/// 出来只可能是将来加了新名字，落到默认的底部。
fn side_named(name: &str) -> PanelSide {
    match name {
        "left" => PanelSide::Left,
        "right" => PanelSide::Right,
        "top" => PanelSide::Top,
        _ => PanelSide::Bottom,
    }
}

impl PaneId {
    /// The slot in `fixed_side`.
    fn fixed_slot(self) -> usize {
        match self {
            PaneId::DirView => 0,
            PaneId::FileList => 1,
            PaneId::Results => 2,
            PaneId::Outline => 3,
            PaneId::Snippet => 4,
            PaneId::Bookmark => 5,
            // Output and script panes carry their side in the window.
            PaneId::Output(_) | PaneId::Script(_) => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four names `s:pane` accepts, and what an unrecognised one means.
    /// The script side validates the spelling, so this only has to be total:
    /// every string lands on a side, never on a panic.
    #[test]
    fn a_side_name_maps_or_falls_back_to_the_bottom() {
        assert_eq!(side_named("left"), PanelSide::Left);
        assert_eq!(side_named("right"), PanelSide::Right);
        assert_eq!(side_named("top"), PanelSide::Top);
        assert_eq!(side_named("bottom"), PanelSide::Bottom);
        assert_eq!(side_named(""), PanelSide::Bottom);
        assert_eq!(side_named("Left"), PanelSide::Bottom, "case is the script's job");
    }

    /// Client geometry without a window: every `place`/`apply_theme` skips an
    /// uncreated HWND, so the dock maths is testable headlessly.
    #[test]
    fn layout_partitions_client() {
        let mut p = Panels::new();
        p.dock.left_panel.visible = true;
        p.dock.bottom_panel.visible = true;
        p.layout(1200, 800, &Theme::default());
        let client = Rect::new(0.0, 0.0, 1200.0, 800.0);
        // The dock still owns the partition the windows are placed in, which is
        // what the editor's origin and the strip's row are derived from.
        let lrect = p.dock.panel_rect(PanelSide::Left, client);
        assert!((lrect.w - 240.0).abs() < 0.01);
        // The strip's geometry now belongs to its window, so what this file
        // still owns is the dock reserving the row for it.
        let srect = p.dock.status_rect(client);
        assert!((srect.h - 23.0).abs() < 0.01);
        assert!((srect.y - 777.0).abs() < 0.01);
        let brect = p.dock.panel_rect(PanelSide::Bottom, client);
        assert!((brect.h - 240.0).abs() < 0.01);
        assert!((brect.y - (800.0 - 23.0 - 240.0)).abs() < 0.01);
        // Headless: no window exists, so the strip's layout pass handed the
        // pane table to nothing rather than crashing on a missing HWND, and the
        // row it would take is 0.
        assert!(!p.status_win.alive());
        assert_eq!(p.status.panes().len(), 13);
        assert_eq!(p.dock.strip_h, 0.0);
        // The tab row takes no room until the host reserves one: a fresh
        // session has a single document, and `HideOnlyOne` hides the bar.
        assert!(p.dock.tab_rect(client).is_empty());
        p.dock.tab_h = 26.0;
        let row = p.dock.tab_rect(client);
        assert_eq!(row.y, 0.0);
        assert_eq!(row.h, 26.0);
        // and the editor starts under it
        assert_eq!(p.dock.editor_rect(client).y, 26.0);
    }

    #[test]
    fn group_matches_groups_by_path() {
        let matches = vec![
            FileMatch {
                path: "a.rs".to_string(),
                line: 0,
                col: 0,
                text: "one".to_string(),
            },
            FileMatch {
                path: "b.rs".to_string(),
                line: 1,
                col: 2,
                text: "two".to_string(),
            },
            FileMatch {
                path: "a.rs".to_string(),
                line: 5,
                col: 0,
                text: "three".to_string(),
            },
        ];
        let groups = Panels::group_matches(&matches);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].path, "a.rs");
        assert_eq!(groups[0].matches.len(), 2);
        assert_eq!(groups[1].matches.len(), 1);
    }

    /// The dock's rows/chars become each bar's range, each bar gets its own
    /// axis' position, and the return value says whether the *document*
    /// moved — the signal that pops a faded bar back up.
    #[test]
    fn scrollbar_model_clamps() {
        let mut p = Panels::new();
        let moved = p.sync_scrollbars(0, 50, 100, 20, 10, 120, 80);
        assert_eq!(p.bars.pos(overlay::V), 50);
        assert_eq!(p.bars.pos(overlay::H), 10);
        assert_eq!(moved, (true, true));
        // Applying the same position twice is not a movement: this is what
        // keeps a bar's own drag from re-arming the fade it sits under.
        assert_eq!(
            p.sync_scrollbars(0, 50, 100, 20, 10, 120, 80),
            (false, false)
        );
        // A document that shrank under the thumb pulls it back into range
        // (100 rows - 20 visible = 80), and that clamp *is* a movement.
        assert!(p.sync_scrollbars(0, 999, 100, 20, 10, 120, 80).0);
        assert_eq!(p.bars.pos(overlay::V), 80);
        assert_eq!(p.bars.pos(overlay::H), 10);
    }

    /// A fresh frame has every host closed and no pane docked: a strip would
    /// otherwise offer a caption for a pane nobody opened.
    #[test]
    fn the_dock_starts_with_nothing_docked() {
        let mut p = Panels::new();
        for side in PanelSide::ALL {
            assert!(!p.dock.side_visible(side));
            assert!(p.host(side).panes.is_empty());
            assert_eq!(p.host(side).active, None);
        }
        // Opening the list alone and closing it must shut the host. A host
        // that kept a second pane here would fall back to a tree the user
        // never opened, leaving a blank column with no document list in it.
        p.toggle_pane(PaneId::FileList);
        assert!(p.pane_showing(PaneId::FileList));
        assert!(p.dock.left_panel.visible);
        p.toggle_pane(PaneId::FileList);
        assert!(!p.pane_showing(PaneId::FileList));
        assert!(!p.dock.left_panel.visible);
    }

    /// The host holds both panes and shows one. Closing the one on screen hands
    /// the column to the other; closing that one too shuts the host.
    #[test]
    fn closing_the_shown_pane_falls_back_to_the_one_behind_it() {
        let mut p = Panels::new();
        p.show_pane(PaneId::DirView);
        p.show_pane(PaneId::FileList);
        assert_eq!(p.host(PanelSide::Left).active, Some(PaneId::FileList));
        // Docking a pane brings it up without taking the other out of the host.
        assert_eq!(p.host(PanelSide::Left).panes.len(), 2);
        p.undock_pane(PaneId::FileList);
        assert_eq!(p.host(PanelSide::Left).active, Some(PaneId::DirView));
        assert!(p.dock.left_panel.visible);
        p.undock_pane(PaneId::DirView);
        assert!(!p.dock.left_panel.visible);
    }

    /// Every pane is a tab of one host, so a move is a removal plus an
    /// insertion: the old host loses the tab and falls back, the new one shows
    /// it. Nothing keeps a second copy.
    #[test]
    fn a_pane_moves_between_hosts_as_one_tab() {
        let mut p = Panels::new();
        p.show_pane(PaneId::DirView);
        p.show_pane(PaneId::FileList);
        p.dock_pane(PaneId::DirView, PanelSide::Right);
        assert!(!p.host(PanelSide::Left).panes.contains(&PaneId::DirView));
        assert!(p.host(PanelSide::Right).panes.contains(&PaneId::DirView));
        assert_eq!(p.host(PanelSide::Right).active, Some(PaneId::DirView));
        // The left host still holds the list and stays open.
        assert!(p.dock.left_panel.visible);
        assert_eq!(p.host(PanelSide::Left).active, Some(PaneId::FileList));
        // and the moved pane comes back to the side it was moved to.
        p.undock_pane(PaneId::DirView);
        p.show_pane(PaneId::DirView);
        assert!(p.host(PanelSide::Right).panes.contains(&PaneId::DirView));
        // The left host never lost its own pane, so it never closed either.
        assert!(p.dock.side_visible(PanelSide::Left));
        assert!(p.dock.side_visible(PanelSide::Right));
    }

    /// The three fixed panes and the output panes share one id space without
    /// colliding, and a code survives the round trip a caption click makes.
    #[test]
    fn pane_codes_round_trip() {
        for id in [
            PaneId::DirView,
            PaneId::FileList,
            PaneId::Results,
            PaneId::Output(1),
            PaneId::Output(7),
        ] {
            assert_eq!(PaneId::from_code(id.code()), Some(id));
        }
        assert_ne!(PaneId::Results.code(), PaneId::Output(0).code());
    }
}
