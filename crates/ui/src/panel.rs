//! Docking geometry for the custom-drawn shell.
//!
//! The client area is partitioned into: the four dock hosts (left, bottom,
//! right, top), the editor column (gutter + text) and the fixed status bar
//! strip. The scrollbars are **overlays**: their strips lie *inside* the editor
//! rect, so text runs under them and the editor keeps its full width. All rects
//! are computed here so the Win32 backend and the tests share one geometry
//! source.
//!
//! Every host is the same shape — a tab strip along one edge and the pane the
//! strip selected filling the rest — because every docked pane is a tab of the
//! host on its side. The left and right hosts take a column of their own; the
//! bottom and top hosts live inside the column that is left between them.

use std::sync::RwLock;

use dg_render::Rect;

/// Thickness of a host's splitter bar. The host's `WM_PAINT` (the host
/// paint) fills it with the face brush, so the bar is a strip of the
/// host's own surface rather than a line drawn over the editor.
pub const SPLITTER: i32 = 4;

/// The face alone is two levels away from the editor's `#1E1F22`, so without
/// the rules the bar reads as no bar at all — and because a child window
/// clips the frame's paint, the rules have to sit inside the band rather than
/// be written over the host or the editor.
///
/// This is the **default**; a skin can override it with `[metrics] split.band_v`.
pub const SPLIT_BAND: i32 = SPLITTER + 2;

/// `y=774 #464646` then `775..778 #202020` face, with no rule back at `779`.
/// Its top host is the mirror of that, so a horizontal bar is a face and the
/// single rule on the row that faces the editor.
///
/// Default; a skin can override it with `[metrics] split.band_h`.
pub const SPLIT_BAND_H: i32 = SPLITTER + 1;

/// The shell's metric table. The shell composes it at startup
/// (`crate::skin::install`) and recomposes it whenever the theme changes —
/// a theme's `[Metrics]` section is the base, the skin's `[metrics]` sits on
/// top, so the table is not a one-shot value. Layout reads it afterwards.
/// Tests never set it, so they see the constants above.
static METRICS: RwLock<dg_skin::Metrics> = RwLock::new(dg_skin::Metrics::new());

/// Installs (or re-installs) the shell's geometry.
pub fn set_metrics(metrics: dg_skin::Metrics) {
    *METRICS.write().unwrap_or_else(|e| e.into_inner()) = metrics;
}

fn metrics() -> dg_skin::Metrics {
    *METRICS.read().unwrap_or_else(|e| e.into_inner())
}

/// The thickness the bar of a host on `side` takes, and the number of rules it
/// carries with it: two for a column bar, one for a band bar.
pub fn split_band_thickness(side: PanelSide) -> i32 {
    let m = metrics();
    match side {
        PanelSide::Top | PanelSide::Bottom => m.get(dg_skin::MetricId::SplitBandH) as i32,
        PanelSide::Left | PanelSide::Right => m.get(dg_skin::MetricId::SplitBandV) as i32,
    }
}

/// Which side of the client a panel is docked to. A panel can be moved between them
/// at any time, one panel at a time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelSide {
    Left,
    Right,
    Top,
    #[default]
    Bottom,
}

impl PanelSide {
    pub const ALL: [PanelSide; 4] = [
        PanelSide::Left,
        PanelSide::Bottom,
        PanelSide::Right,
        PanelSide::Top,
    ];

    pub fn slot(self) -> usize {
        match self {
            PanelSide::Left => 0,
            PanelSide::Bottom => 1,
            PanelSide::Right => 2,
            PanelSide::Top => 3,
        }
    }
}

/// One side host of the dock: whether it shows and how much it takes from the
/// editor. Each host owns its own strip, so the panes on one side are tabs of
/// that side alone.
#[derive(Clone, Copy, Debug)]
pub struct SideDock {
    pub visible: bool,
    /// Width on the left/right, height on the top/bottom.
    pub size: f32,
}

impl SideDock {
    fn new(size: f32) -> Self {
        Self {
            visible: false,
            size,
        }
    }
}

pub struct DockPanel {
    /// Top band (main toolbar + its bottom rule) height, 0 when
    /// the toolbar is hidden. The toolbar itself is a window the
    /// host places; this is only the row it takes off the dock
    /// partition, the way `status_height` is the strip's.
    pub top_dock_h: f32,
    /// Tab strip row, directly under the toolbar band. 0 when the
    /// strip is hidden or only one document is open. Like
    /// `top_dock_h` this is only the row the dock reserves; the strip
    /// itself is a window the host places.
    pub tab_h: f32,
    /// Whether the toolbar band is docked at the bottom of the window, above
    /// the status strip, instead of at the top (`[Setting] BarPos1`). 上/下 in
    /// the bar's menu writes it; the tab row keeps the top edge either way, so
    /// the switch shows even with one document open and no tab row at all.
    pub toolbar_below: bool,
    /// The four hosts. There is one per side, so two panes moved to the left
    /// are tabs of that host, not two columns.
    pub left_panel: SideDock,
    pub bottom_panel: SideDock,
    pub right_panel: SideDock,
    pub top_panel: SideDock,
    /// The row a host reserves for its tab strip — the height the strip window
    /// measured for its own font. 0 while no strip exists, which leaves the
    /// pane the whole host.
    pub strip_h: f32,
    /// Whether the strips sit along the host's top edge instead of its bottom.
    /// One flag for every host, because they are one control.
    pub strip_top: bool,
    /// Status bar height.
    pub status_height: f32,
    /// Overlay strip thickness (both axes) — the strip window the pill
    /// floats in. The geometry is fixed: pointing at the bar recolours the
    /// pill, it never reshapes it. Host sets it from
    /// `Theme::scrollbar_strip_width`.
    pub scrollbar_w: f32,
    pub scrollbar_h: f32,
}

impl Default for DockPanel {
    fn default() -> Self {
        Self {
            top_dock_h: 0.0,
            tab_h: 0.0,
            toolbar_below: false,
            left_panel: SideDock::new(240.0),
            // A pane that opens with the report's head cut off looks broken
            // rather than scrolled.
            bottom_panel: SideDock::new(240.0),
            right_panel: SideDock::new(280.0),
            top_panel: SideDock::new(240.0),
            strip_h: 22.0,
            strip_top: false,
            status_height: crate::statusbar::HEIGHT,
            scrollbar_w: 18.0,
            scrollbar_h: 18.0,
        }
    }
}

impl DockPanel {
    /// Where the docked content starts: below the tab strip row and, unless
    /// the band was moved down, below the toolbar band too.
    fn content_top(&self) -> f32 {
        if self.toolbar_below {
            self.tab_h
        } else {
            self.top_dock_h + self.tab_h
        }
    }

    /// The band the toolbar window fills: the top row, or the one directly
    /// above the status strip once 下 has moved it down.
    pub fn toolbar_rect(&self, client: Rect) -> Rect {
        let y = if self.toolbar_below {
            client.y + client.h - self.status_height - self.top_dock_h
        } else {
            client.y
        };
        Rect::new(client.x, y, client.w, self.top_dock_h)
    }

    /// The tab strip's row: full width, under the toolbar band — or at the top
    /// edge itself once the bar has been moved down.
    pub fn tab_rect(&self, client: Rect) -> Rect {
        if self.tab_h <= 0.0 {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let y = if self.toolbar_below {
            client.y
        } else {
            client.y + self.top_dock_h
        };
        Rect::new(client.x, y, client.w, self.tab_h)
    }

    /// Whole client area -> status bar strip.
    pub fn status_rect(&self, client: Rect) -> Rect {
        Rect::new(
            client.x,
            client.y + client.h - self.status_height,
            client.w,
            self.status_height,
        )
    }

    /// Whether a host is showing.
    pub fn side_visible(&self, side: PanelSide) -> bool {
        self.host(side).visible
    }

    /// The thickness a host takes from the editor: width for left/right,
    /// height for top/bottom.
    pub fn side_size(&self, side: PanelSide) -> f32 {
        self.host(side).size
    }

    /// One host, by side. The four are the same record, which is what makes
    /// the strips and the dock commands one code path.
    pub fn host(&self, side: PanelSide) -> &SideDock {
        match side {
            PanelSide::Left => &self.left_panel,
            PanelSide::Bottom => &self.bottom_panel,
            PanelSide::Right => &self.right_panel,
            PanelSide::Top => &self.top_panel,
        }
    }

    pub fn host_mut(&mut self, side: PanelSide) -> &mut SideDock {
        match side {
            PanelSide::Left => &mut self.left_panel,
            PanelSide::Bottom => &mut self.bottom_panel,
            PanelSide::Right => &mut self.right_panel,
            PanelSide::Top => &mut self.top_panel,
        }
    }

    fn left_host_w(&self) -> f32 {
        if self.left_panel.visible {
            self.left_panel.size
        } else {
            0.0
        }
    }

    fn right_host_w(&self) -> f32 {
        if self.right_panel.visible {
            self.right_panel.size
        } else {
            0.0
        }
    }

    fn top_host_h(&self) -> f32 {
        if self.top_panel.visible {
            self.top_panel.size
        } else {
            0.0
        }
    }

    fn bottom_host_h(&self) -> f32 {
        if self.bottom_panel.visible {
            self.bottom_panel.size
        } else {
            0.0
        }
    }

    /// Everything the four hosts and the editor share: below the frame's own
    /// bands, above the status strip. The hosts then divide it between
    /// themselves.
    pub fn dock_rect(&self, client: Rect) -> Rect {
        let top = self.content_top();
        // A band moved down takes its row off the bottom, above the strip.
        let bottom = self.status_height + if self.toolbar_below { self.top_dock_h } else { 0.0 };
        Rect::new(
            client.x,
            client.y + top,
            client.w,
            (client.h - top - bottom).max(0.0),
        )
    }

    /// The column between the two side hosts. The editor, the top host and the
    /// bottom host all live in it, which is what makes a bottom-docked pane as
    /// wide as the editor and no wider, and lets a left or right host run the
    /// full height beside it.
    fn center_rect(&self, client: Rect) -> Rect {
        let d = self.dock_rect(client);
        let left = self.left_host_w();
        Rect::new(
            d.x + left,
            d.y,
            (d.w - left - self.right_host_w()).max(0.0),
            d.h,
        )
    }

    /// A host's whole rect. The left and right hosts own a full-height column;
    /// the bottom and top hosts are bands inside the centre column.
    pub fn panel_rect(&self, side: PanelSide, client: Rect) -> Rect {
        if !self.side_visible(side) {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let d = self.dock_rect(client);
        match side {
            PanelSide::Left => Rect::new(d.x, d.y, self.left_panel.size.min(d.w), d.h),
            PanelSide::Right => {
                let w = self.right_panel.size.min(d.w);
                Rect::new(d.x + d.w - w, d.y, w, d.h)
            }
            PanelSide::Bottom => {
                let c = self.center_rect(client);
                let h = self.bottom_panel.size.min(c.h);
                Rect::new(c.x, c.y + (c.h - h).max(0.0), c.w, h)
            }
            PanelSide::Top => {
                let c = self.center_rect(client);
                let h = self.top_panel.size.min(c.h);
                Rect::new(c.x, c.y, c.w, h)
            }
        }
    }

    /// The strip row of a host: full width, along the host's top or bottom
    /// edge. It covers the splitter's column as well, because the strip is the
    /// host's own chrome and a bar that stopped short of it would leave a
    /// corner nothing can drag.
    pub fn strip_rect(&self, side: PanelSide, client: Rect) -> Rect {
        let r = self.panel_rect(side, client);
        if r.w <= 0.0 || r.h <= 0.0 || self.strip_h <= 0.0 {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let h = self.strip_h.min(r.h);
        let y = if self.strip_top { r.y } else { r.y + r.h - h };
        Rect::new(r.x, y, r.w, h)
    }

    /// What is left of a host once its strip row is gone: the band the
    /// splitter and the pane divide.
    fn body_rect(&self, side: PanelSide, client: Rect) -> Rect {
        let r = self.panel_rect(side, client);
        if r.w <= 0.0 || r.h <= 0.0 {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let s = self.strip_rect(side, client);
        let top = if self.strip_top { r.y + s.h } else { r.y };
        Rect::new(r.x, top, r.w, (r.h - s.h).max(0.0))
    }

    /// The splitter strip carved out of the host's inner edge — the edge that
    /// faces the editor. The bar belongs to the host, so the editor never loses
    /// a pixel to it. It runs the pane area, never the strip row: a bar over
    /// the captions would be a drag handle on top of a tab.
    pub fn split_rect(&self, side: PanelSide, client: Rect) -> Rect {
        let r = self.body_rect(side, client);
        if r.w <= 0.0 || r.h <= 0.0 {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let t = split_band_thickness(side) as f32;
        match side {
            PanelSide::Bottom => Rect::new(r.x, r.y, r.w, t.min(r.h)),
            PanelSide::Top => Rect::new(r.x, r.y + (r.h - t).max(0.0), r.w, t.min(r.h)),
            PanelSide::Left => Rect::new(r.x + (r.w - t).max(0.0), r.y, t.min(r.w), r.h),
            PanelSide::Right => Rect::new(r.x, r.y, t.min(r.w), r.h),
        }
    }

    /// The host's pane area: the body minus the splitter band. Whatever the
    /// host is showing fills this rect.
    pub fn pane_rect(&self, side: PanelSide, client: Rect) -> Rect {
        let r = self.body_rect(side, client);
        if r.w <= 0.0 || r.h <= 0.0 {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let t = split_band_thickness(side) as f32;
        match side {
            PanelSide::Bottom => Rect::new(r.x, r.y + t.min(r.h), r.w, (r.h - t).max(0.0)),
            PanelSide::Top => Rect::new(r.x, r.y, r.w, (r.h - t).max(0.0)),
            PanelSide::Left => Rect::new(r.x, r.y, (r.w - t).max(0.0), r.h),
            PanelSide::Right => Rect::new(r.x + t.min(r.w), r.y, (r.w - t).max(0.0), r.h),
        }
    }

    /// The editor area: gutter + text. Nothing is reserved for the overlay
    /// bars; only the hosts and the status bar shrink it. It is the centre
    /// column between the two side hosts, minus whatever the top and bottom
    /// hosts took from it.
    pub fn editor_rect(&self, client: Rect) -> Rect {
        let c = self.center_rect(client);
        let top = self.top_host_h();
        let bottom = self.bottom_host_h();
        Rect::new(
            c.x,
            c.y + top,
            c.w,
            (c.h - top - bottom).max(0.0),
        )
    }

    /// Vertical overlay strip: the editor's right edge,
    /// stopping above the horizontal strip so the two
    /// bars never overlap.
    pub fn vscroll_rect(&self, client: Rect) -> Rect {
        let ed = self.editor_rect(client);
        Rect::new(
            ed.x + ed.w - self.scrollbar_w,
            ed.y,
            self.scrollbar_w,
            (ed.h - self.scrollbar_h).max(0.0),
        )
    }

    /// Horizontal overlay strip: the editor's bottom
    /// edge, starting right of the left dock.
    pub fn hscroll_rect(&self, client: Rect) -> Rect {
        let ed = self.editor_rect(client);
        Rect::new(
            ed.x,
            ed.y + ed.h - self.scrollbar_h,
            ed.w,
            self.scrollbar_h,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Rect {
        Rect::new(0.0, 0.0, 1200.0, 800.0)
    }

    #[test]
    fn full_layout_partition() {
        let d = DockPanel {
            left_panel: SideDock { visible: true, size: 240.0 },
            bottom_panel: SideDock { visible: true, size: 180.0 },
            ..DockPanel::default()
        };
        let client = client();
        // the editor keeps the full width: the bars
        // overlay it instead of taking a column
        let ed = d.editor_rect(client);
        assert!((ed.x - 240.0).abs() < 0.01);
        assert!((ed.w - (1200.0 - 240.0)).abs() < 0.01);
        assert!((ed.h - (800.0 - 23.0 - 180.0)).abs() < 0.01);

        let st = d.status_rect(client);
        assert!((st.y - 777.0).abs() < 0.01);
        assert!((st.h - 23.0).abs() < 0.01);

        let bt = d.panel_rect(PanelSide::Bottom, client);
        // The bottom host is the editor's column, not the window's: the left
        // host runs past it all the way down to the status strip.
        assert!((bt.x - 240.0).abs() < 0.01);
        assert!((bt.w - 960.0).abs() < 0.01);
        assert!((bt.y - 597.0).abs() < 0.01);

        let vs = d.vscroll_rect(client);
        assert!((vs.x - (1200.0 - 18.0)).abs() < 0.01);
        assert!((vs.w - 18.0).abs() < 0.01);
        // spans the editor height minus the other strip
        assert!((vs.h - (597.0 - 18.0)).abs() < 0.01);
        assert!(vs.x >= ed.x && vs.x + vs.w <= ed.x + ed.w);

        let hs = d.hscroll_rect(client);
        assert!((hs.y - (597.0 - 18.0)).abs() < 0.01);
        assert!((hs.x - 240.0).abs() < 0.01);
        assert!((hs.w - (1200.0 - 240.0)).abs() < 0.01);

        let lf = d.panel_rect(PanelSide::Left, client);
        assert!((lf.w - 240.0).abs() < 0.01);
        assert!((lf.h - (800.0 - 23.0)).abs() < 0.01);
    }

    /// The tab row is a band off the top of the docked content, exactly like
    /// the toolbar band — and it sits *under* that one.
    #[test]
    fn tab_row_shrinks_the_editor() {
        let client = client();
        let d = DockPanel {
            top_dock_h: 30.0,
            tab_h: 26.0,
            left_panel: SideDock { visible: true, size: 240.0 },
            ..DockPanel::default()
        };
        let row = d.tab_rect(client);
        assert!((row.y - 30.0).abs() < 0.01);
        assert!((row.h - 26.0).abs() < 0.01);
        let ed = d.editor_rect(client);
        assert!((ed.y - 56.0).abs() < 0.01);
        assert!((ed.h - (800.0 - 23.0 - 56.0)).abs() < 0.01);
        let lf = d.panel_rect(PanelSide::Left, client);
        assert!((lf.y - 56.0).abs() < 0.01);
        assert!((lf.h - (800.0 - 23.0 - 56.0)).abs() < 0.01);
        // Hidden strip (or one document): the row is empty and nothing moves.
        let d = DockPanel::default();
        assert!(d.tab_rect(client).is_empty());
        assert!((d.editor_rect(client).y - 0.0).abs() < 0.01);
    }

    /// 上/下 in the bar's menu docks the band above the status strip instead of
    /// at the top edge. The tab row stays put, so the switch shows with one
    /// document open — where there is no tab row to trade places with.
    #[test]
    fn the_toolbar_band_can_dock_at_the_bottom() {
        let client = client();
        let below = DockPanel {
            top_dock_h: 30.0,
            tab_h: 26.0,
            toolbar_below: true,
            ..DockPanel::default()
        };
        let bar = below.toolbar_rect(client);
        let row = below.tab_rect(client);
        assert!((bar.y - (800.0 - 23.0 - 30.0)).abs() < 0.01);
        assert!((bar.h - 30.0).abs() < 0.01);
        assert!((row.y - 0.0).abs() < 0.01);
        assert!((row.h - 26.0).abs() < 0.01);
        // The band left the top row and took one off the bottom, so the editor
        // starts at the tab row and ends above the band.
        let ed = below.editor_rect(client);
        assert!((ed.y - 26.0).abs() < 0.01);
        assert!((ed.h - (800.0 - 26.0 - 23.0 - 30.0)).abs() < 0.01);
        // Above is the default: the band owns the top edge again.
        let above = DockPanel {
            toolbar_below: false,
            ..below
        };
        assert!((above.toolbar_rect(client).y - 0.0).abs() < 0.01);
        assert!((above.tab_rect(client).y - 30.0).abs() < 0.01);
        assert!((above.editor_rect(client).y - 56.0).abs() < 0.01);
        // One document, no tab row: the move is still a move.
        let single = DockPanel {
            tab_h: 0.0,
            ..below
        };
        assert!((single.editor_rect(client).y - 0.0).abs() < 0.01);
        assert!((single.toolbar_rect(client).y - (800.0 - 23.0 - 30.0)).abs() < 0.01);
    }

    #[test]
    fn the_output_panel_can_sit_on_every_side() {
        let client = client();
        let show = || DockPanel {
            top_dock_h: 30.0,
            tab_h: 26.0,
            left_panel: SideDock { visible: true, size: 280.0 },
            right_panel: SideDock { visible: true, size: 280.0 },
            top_panel: SideDock { visible: true, size: 240.0 },
            bottom_panel: SideDock { visible: true, size: 240.0 },
            ..DockPanel::default()
        };
        // With every host up, each owns its own edge and the editor the rest.
        // The side hosts take the full docked height and the top/bottom hosts
        // live inside the column that is left between them.
        let d = show();
        let want = [
            (PanelSide::Left, Rect::new(0.0, 56.0, 280.0, 721.0)),
            (PanelSide::Right, Rect::new(920.0, 56.0, 280.0, 721.0)),
            (PanelSide::Top, Rect::new(280.0, 56.0, 640.0, 240.0)),
            (PanelSide::Bottom, Rect::new(280.0, 537.0, 640.0, 240.0)),
        ];
        for (side, r) in want {
            let got = d.panel_rect(side, client);
            assert!((got.x - r.x).abs() < 0.01, "{side:?} x");
            assert!((got.y - r.y).abs() < 0.01, "{side:?} y");
            assert!((got.w - r.w).abs() < 0.01, "{side:?} w");
            assert!((got.h - r.h).abs() < 0.01, "{side:?} h");
        }
        let ed = d.editor_rect(client);
        assert!((ed.x - 280.0).abs() < 0.01);
        assert!((ed.w - (1200.0 - 560.0)).abs() < 0.01);
        assert!((ed.y - 296.0).abs() < 0.01);
        assert!((ed.h - 241.0).abs() < 0.01);
        // Only the left host up: the editor keeps everything right of it.
        let d = DockPanel {
            top_dock_h: 30.0,
            tab_h: 26.0,
            left_panel: SideDock { visible: true, size: 280.0 },
            ..DockPanel::default()
        };
        let got = d.panel_rect(PanelSide::Left, client);
        assert!((got.x - 0.0).abs() < 0.01);
        assert!((got.y - 56.0).abs() < 0.01);
        assert!((got.w - 280.0).abs() < 0.01);
        assert!((got.h - 721.0).abs() < 0.01);
        let ed = d.editor_rect(client);
        assert!((ed.x - 280.0).abs() < 0.01);
        assert!((ed.w - 920.0).abs() < 0.01);
        assert!((ed.h - 721.0).abs() < 0.01);
        // The four hosts are independent: hiding one leaves the others standing.
        let mut d = show();
        d.right_panel.visible = false;
        assert!(d.panel_rect(PanelSide::Right, client).is_empty());
        assert!(!d.panel_rect(PanelSide::Top, client).is_empty());
        assert!(!d.panel_rect(PanelSide::Bottom, client).is_empty());
    }

    /// Every bar is cut out of the host it resizes, on the edge that faces
    /// the editor. Getting this backwards is what used to put the bar under
    /// a child window and make it vanish.
    #[test]
    fn every_host_carves_its_splitter_out_of_its_own_edge() {
        let client = client();
        let d = DockPanel {
            left_panel: SideDock { visible: true, size: 240.0 },
            right_panel: SideDock { visible: true, size: 280.0 },
            top_panel: SideDock { visible: true, size: 240.0 },
            bottom_panel: SideDock { visible: true, size: 240.0 },
            ..DockPanel::default()
        };
        assert_eq!(SPLIT_BAND, SPLITTER + 2);
        assert_eq!(SPLIT_BAND_H, SPLITTER + 1);
        // The editor is the centre column with no bar taken from it.
        let ed = d.editor_rect(client);
        assert!((ed.x - 240.0).abs() < 0.01);
        assert!((ed.w - (1200.0 - 240.0 - 280.0)).abs() < 0.01);
        assert!((ed.y - 240.0).abs() < 0.01);
        for side in PanelSide::ALL {
            let host = d.panel_rect(side, client);
            let bar = d.split_rect(side, client);
            let pane = d.pane_rect(side, client);
            let t = split_band_thickness(side) as f32;
            match side {
                PanelSide::Left => {
                    assert!((bar.x - (host.x + host.w - t)).abs() < 0.01);
                    assert!((bar.w - t).abs() < 0.01);
                    assert!((pane.x - host.x).abs() < 0.01);
                    assert!((pane.w - (host.w - t)).abs() < 0.01);
                }
                PanelSide::Right => {
                    assert!((bar.x - host.x).abs() < 0.01);
                    assert!((pane.x - (host.x + t)).abs() < 0.01);
                    assert!((pane.w - (host.w - t)).abs() < 0.01);
                }
                PanelSide::Top => {
                    // The bar runs the pane area, so it stops above the strip
                    // row rather than at the host's own bottom edge.
                    assert!((bar.y - (host.y + host.h - d.strip_h - t)).abs() < 0.01);
                    assert!((bar.h - t).abs() < 0.01);
                    assert!((pane.y - host.y).abs() < 0.01);
                    assert!((pane.h - (host.h - t - d.strip_h)).abs() < 0.01);
                }
                PanelSide::Bottom => {
                    assert!((bar.y - host.y).abs() < 0.01);
                    assert!((pane.y - (host.y + t)).abs() < 0.01);
                    assert!((pane.h - (host.h - t - d.strip_h)).abs() < 0.01);
                }
            }
        }
    }

    /// Whether two rects share a pixel.
    fn overlaps(a: Rect, b: Rect) -> bool {
        a.w > 0.0
            && a.h > 0.0
            && b.w > 0.0
            && b.h > 0.0
            && a.x < b.x + b.w
            && b.x < a.x + a.w
            && a.y < b.y + b.h
            && b.y < a.y + a.h
    }

    /// Every host keeps one row for its captions, and the pane and its splitter
    /// divide what is left: the strip is the host's own chrome, the bar runs the
    /// pane area only, and the strip may sit at either end.
    #[test]
    fn every_host_keeps_a_row_for_its_captions() {
        let client = client();
        let d = DockPanel {
            left_panel: SideDock { visible: true, size: 240.0 },
            bottom_panel: SideDock { visible: true, size: 200.0 },
            strip_h: 22.0,
            ..DockPanel::default()
        };
        for side in [PanelSide::Left, PanelSide::Bottom] {
            let host = d.panel_rect(side, client);
            let strip = d.strip_rect(side, client);
            assert!((strip.x - host.x).abs() < 0.01, "{side:?} x");
            assert!((strip.w - host.w).abs() < 0.01, "{side:?} w");
            assert!((strip.y - (host.y + host.h - 22.0)).abs() < 0.01, "{side:?} y");
            assert!((strip.h - 22.0).abs() < 0.01);
            let pane = d.pane_rect(side, client);
            let bar = d.split_rect(side, client);
            // The strip row is the host's own: neither the bar nor the pane
            // reaches into it, whichever end it sits at.
            assert!(!overlaps(pane, strip), "{side:?} pane over the captions");
            assert!(!overlaps(bar, strip), "{side:?} bar over the captions");
            // Pane and bar together are the body, edge to edge.
            match side {
                PanelSide::Left => {
                    assert!((pane.x - host.x).abs() < 0.01);
                    assert!((bar.x - (pane.x + pane.w)).abs() < 0.01);
                    assert!((bar.y - host.y).abs() < 0.01);
                    assert!((bar.h - (host.h - 22.0)).abs() < 0.01);
                }
                _ => {
                    // The bottom host's inner edge is its top one, so the bar
                    // is up there and the pane runs from it down to the strip.
                    assert!((bar.y - host.y).abs() < 0.01);
                    assert!((pane.y - (bar.y + bar.h)).abs() < 0.01);
                    assert!((pane.y + pane.h - strip.y).abs() < 0.01);
                }
            }
        }
        // The strip moved to the top edge: the pane and the bar follow it down.
        let d = DockPanel {
            strip_top: true,
            ..d
        };
        let host = d.panel_rect(PanelSide::Left, client);
        let strip = d.strip_rect(PanelSide::Left, client);
        assert!((strip.y - host.y).abs() < 0.01);
        let pane = d.pane_rect(PanelSide::Left, client);
        assert!((pane.y - (host.y + 22.0)).abs() < 0.01);
        let bar = d.split_rect(PanelSide::Left, client);
        assert!((bar.y - (host.y + 22.0)).abs() < 0.01);
        assert!((bar.y + bar.h - (host.y + host.h)).abs() < 0.01);
        // No strip, no row: the pane keeps the whole column, and a closed host
        // has neither.
        let d = DockPanel {
            strip_h: 0.0,
            left_panel: SideDock { visible: true, size: 240.0 },
            ..DockPanel::default()
        };
        assert!(d.strip_rect(PanelSide::Left, client).is_empty());
        assert!(
            (d.pane_rect(PanelSide::Left, client).w
                - (d.panel_rect(PanelSide::Left, client).w - SPLIT_BAND as f32))
                .abs()
                < 0.01
        );
        let d = DockPanel::default();
        assert!(d.strip_rect(PanelSide::Left, client).is_empty());
        assert!(d.pane_rect(PanelSide::Left, client).is_empty());
    }

    /// The side hosts reach the dock bottom, so a pane moved between them never
    /// changes height; the bottom host is the editor's column, not the window's.
    #[test]
    fn the_side_hosts_reach_the_dock_bottom() {
        let client = client();
        let d = DockPanel {
            top_dock_h: 30.0,
            tab_h: 26.0,
            left_panel: SideDock { visible: true, size: 240.0 },
            right_panel: SideDock { visible: true, size: 280.0 },
            bottom_panel: SideDock { visible: true, size: 240.0 },
            ..DockPanel::default()
        };
        let bottom = 800.0 - d.status_height;
        for side in [PanelSide::Left, PanelSide::Right, PanelSide::Bottom] {
            let host = d.panel_rect(side, client);
            assert!((host.y + host.h - bottom).abs() < 0.01, "{side:?} bottom");
        }
        let bt = d.panel_rect(PanelSide::Bottom, client);
        assert!((bt.x - 240.0).abs() < 0.01);
        assert!((bt.w - (1200.0 - 240.0 - 280.0)).abs() < 0.01);
        assert!((bt.w - d.editor_rect(client).w).abs() < 0.01);
    }

    #[test]
    fn a_hidden_panel_gives_its_room_back() {
        let client = client();
        let d = DockPanel::default();
        assert!(d.panel_rect(PanelSide::Left, client).is_empty());
        assert!(d.panel_rect(PanelSide::Top, client).is_empty());
        let ed = d.editor_rect(client);
        assert!((ed.x - 0.0).abs() < 0.01);
        assert!((ed.y - 0.0).abs() < 0.01);
        assert!((ed.w - 1200.0).abs() < 0.01);
    }

    #[test]
    fn docks_hidden() {
        let d = DockPanel::default();
        let client = client();
        assert!(d.panel_rect(PanelSide::Left, client).is_empty());
        assert!(d.panel_rect(PanelSide::Bottom, client).is_empty());
        let ed = d.editor_rect(client);
        assert!((ed.w - 1200.0).abs() < 0.01);
        assert!((ed.h - (800.0 - 23.0)).abs() < 0.01);
        // the strips are inside that rect, not next to it
        assert!((d.vscroll_rect(client).x - 1182.0).abs() < 0.01);
        assert!((d.hscroll_rect(client).y - (777.0 - 18.0)).abs() < 0.01);
    }

    #[test]
    fn overlay_strips_stay_inside_the_editor() {
        let d = DockPanel::default();
        let client = Rect::new(0.0, 0.0, 640.0, 400.0);
        let ed = d.editor_rect(client);
        for strip in [d.vscroll_rect(client), d.hscroll_rect(client)] {
            assert!(strip.x >= ed.x - 0.001);
            assert!(strip.y >= ed.y - 0.001);
            assert!(strip.x + strip.w <= ed.x + ed.w + 0.001);
            assert!(strip.y + strip.h <= ed.y + ed.h + 0.001);
        }
        // and the corner belongs to exactly one of them
        let v = d.vscroll_rect(client);
        let h = d.hscroll_rect(client);
        assert!(v.y + v.h <= h.y + 0.001);
    }
}
