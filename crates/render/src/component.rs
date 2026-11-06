use crate::pipeline::DrawList;
use crate::theme::{Color, Theme};
use crate::viewflags::ViewFlags;

/// Axis-aligned rectangle in document/window space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// 2D point in DIP. Shared by the widget/event layer and the skin op layer;
/// both used to keep their own identical copy.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// per-segment `rect ∩ clipRect`.
    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.w).min(other.x + other.w);
        let bottom = (self.y + self.h).min(other.y + other.h);
        if right <= x || bottom <= y {
            Rect::new(0.0, 0.0, 0.0, 0.0)
        } else {
            Rect::new(x, y, right - x, bottom - y)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// Per-paint context, threaded through render components.
pub struct PaintCtx<'a> {
    pub clip: Rect,
    pub row: usize,
    pub line: usize,
    /// The logical line's whole band: x = 0,
    /// y = the first wrap row's top, height =
    /// `wrap_rows * line_height`.
    pub row_rect: Rect,
    pub theme: &'a Theme,
    pub view: ViewFlags,
    pub draw: &'a mut DrawListRef,
    /// The line's display number;
    /// `<= 0` marks a hidden line, which paints
    /// no number at all.
    pub display_no: i64,
    /// `line > 0 && eol_kind(line - 1) == 0`:
    /// the previous line carried no terminator,
    /// so the gutter shows `~` instead of a
    /// number.
    pub tilde: bool,
    /// Wrap-row count of the logical line
    /// (`max(1, break_count)`).
    pub wrap_rows: usize,
    /// EOL kind of the line itself
    /// (1=CRLF, 2=LF, 3=CR, 0=none), driving the
    /// continuation markers' glyph.
    pub eol_kind: u8,
    /// 行修改状态: the line's two-bit edit state — 0 untouched, 1 modified
    /// since the last save, 2 written back by it. The edit-state strip paints
    /// one band colour per value.
    pub line_state: u8,
    /// `FoldingMethod != None`: the folding strip's visibility gate.
    pub folding_on: bool,
    /// The caret's wrap row inside its logical
    /// line, for the active-line band.
    pub active_subrow: usize,
    pub caret_line: usize,
}

/// Alias used by components; the concrete draw list
/// lives in `pipeline`.
pub type DrawListRef = crate::pipeline::DrawList;

/// Pluggable decoration layer: one implementor per gutter
/// strip (line numbers, fold marks, edit state, left margin,
/// icons, bookmarks).
pub trait RenderComponent {
    fn paint(&mut self, ctx: &mut PaintCtx);
    /// The strip below the last text row
    /// (`below(hdc, rcDst, y, view)`), called
    /// once after the line loop.
    fn below(&mut self, _ctx: &mut PaintCtx) {}
}

/// The gutter's active-line band: every gutter strip extends
/// the caret-line highlight only under
/// `ExtendActive`, and then only when
/// the line is the caret's and
/// `ActiveLineStyle == 1`.
fn active_line(ctx: &PaintCtx) -> bool {
    ctx.view.get(ViewFlags::EXTEND_ACTIVE)
        && ctx.view.get(ViewFlags::ACTIVE_LINE)
        && ctx.line == ctx.caret_line
}

/// The active-line band inside one strip, at the caret's wrap row.
/// This implementation keeps its own `line_height` row step.
fn paint_active_band(ctx: &mut PaintCtx, x: f32, width: f32) {
    if active_line(ctx) {
        let y = ctx.row_rect.y + ctx.active_subrow as f32 * ctx.theme.line_height;
        ctx.draw.fill(
            Rect::new(x, y, width, ctx.theme.line_height),
            ctx.theme.current_line_bg,
        );
    }
}

/// The 1 px 边界线 at a strip's right
/// edge (`BoundaryLine`): one vertical stroke
/// drawn down the clip in the theme's own
/// boundary colour.
fn paint_boundary(ctx: &mut PaintCtx, x: f32, y: f32, h: f32) {
    ctx.draw
        .fill(Rect::new(x, y, 1.0, h), ctx.theme.boundary_line);
}

pub struct LeftMargin {
    pub width: f32,
    pub x: f32,
}

impl RenderComponent for LeftMargin {
    fn paint(&mut self, ctx: &mut PaintCtx) {
        // the whole line band, no
        // clip intersection, in the default
        // background colour.
        let r = Rect::new(self.x, ctx.row_rect.y, self.width, ctx.row_rect.h);
        ctx.draw.fill(r, ctx.theme.background);
        paint_active_band(ctx, self.x, self.width);
    }

    fn below(&mut self, ctx: &mut PaintCtx) {
        // strip ∩ clip in the
        // below-text colour (DefaultBgNull).
        let r = Rect::new(self.x, ctx.clip.y, self.width, ctx.clip.h)
            .intersect(&ctx.clip);
        if !r.is_empty() {
            ctx.draw.fill(r, ctx.theme.below_bg);
        }
    }
}

pub struct LineNumber {
    pub width: f32,
    pub x: f32,
}

impl RenderComponent for LineNumber {
    fn paint(&mut self, ctx: &mut PaintCtx) {
        // cm_view_linenumber ( bit 0) takes the strip away entirely:
        // no background, no digits, no separator.
        if !ctx.view.get(ViewFlags::LINE_NUMBER) {
            return;
        }
        let r = Rect::new(self.x, ctx.row_rect.y, self.width, ctx.row_rect.h)
            .intersect(&ctx.clip);
        if r.is_empty() {
            return;
        }
        ctx.draw.fill(r, ctx.theme.gutter_bg);
        paint_active_band(ctx, self.x, self.width);
        // TA_RIGHT, TRANSPARENT: the digits' right
        // edge sits 3 px inside the strip's right
        // edge. A `~` replaces the number when the
        // previous line carried no terminator; a
        // hidden line (display number <= 0) shows
        // nothing at all.
        let text: String = if ctx.tilde {
            "~".to_string()
        } else if ctx.display_no > 0 {
            ctx.display_no.to_string()
        } else {
            String::new()
        };
        if !text.is_empty() {
            let x = self.x + self.width - 3.0
                - text.chars().count() as f32 * ctx.theme.x_width;
            ctx.draw.text(x, r.y, &text, ctx.theme.gutter_fg);
        }
        // Continuation markers: one per wrap row
        // below the first, `~` when the line
        // itself carries no terminator, `.`
        // `line_height + padding + 1` and offsets
        // the glyph 2 px up; this implementation keeps its
        // own row step.
        //
        // Only the subrows the clip can show: a line wrapped a hundred
        // thousand times owns a band that tall, and a mark per subrow of it
        // is a mark per subrow of the whole line.
        let xw = ctx.theme.x_width;
        let line_h = ctx.theme.line_height.max(1.0);
        let top = ((ctx.clip.y - ctx.row_rect.y) / line_h).floor().max(0.0) as usize;
        let bottom = ((ctx.clip.y + ctx.clip.h - ctx.row_rect.y) / line_h).ceil().max(0.0) as usize;
        // One subrow of slack each way: the glyph sits 2 px above its band.
        let last = bottom.saturating_add(1).min(ctx.wrap_rows.saturating_sub(1));
        for i in top.saturating_sub(1).max(1)..=last {
            let y = ctx.row_rect.y + i as f32 * line_h - 2.0;
            let glyph = if ctx.eol_kind == 0 { "~" } else { "." };
            let x = self.x + self.width - 3.0 - xw;
            ctx.draw.text(x, y, glyph, ctx.theme.gutter_fg);
        }
        // The separator at the strip's right edge
        // is gated on
        // BoundaryLine and on the icon area being
        // visible or the folding strip not: the
        // folding strip owns the line otherwise.
        if ctx.view.get(ViewFlags::BOUNDARY_LINE)
            && (ctx.view.get(ViewFlags::ICON_AREA) || !ctx.folding_on)
        {
            paint_boundary(ctx, r.x + r.w - 1.0, r.y, r.h);
        }
    }

    fn below(&mut self, ctx: &mut PaintCtx) {
        // strip ∩ clip in the
        // gutter colour, plus the same separator
        // rule as the per-line pass.
        if !ctx.view.get(ViewFlags::LINE_NUMBER) {
            return;
        }
        let r = Rect::new(self.x, ctx.clip.y, self.width, ctx.clip.h)
            .intersect(&ctx.clip);
        if r.is_empty() {
            return;
        }
        ctx.draw.fill(r, ctx.theme.gutter_bg);
        if ctx.view.get(ViewFlags::BOUNDARY_LINE)
            && (ctx.view.get(ViewFlags::ICON_AREA) || !ctx.folding_on)
        {
            paint_boundary(ctx, r.x + r.w - 1.0, r.y, r.h);
        }
    }
}

pub struct Folding {
    pub width: f32,
    pub x: f32,
    /// Folded flag per document line.
    pub folded: Vec<bool>,
    /// Lines that start a collapsible block.
    pub starts: Vec<bool>,
}

impl Folding {
    pub fn new(width: f32, x: f32) -> Self {
        Self {
            width,
            x,
            folded: Vec::new(),
            starts: Vec::new(),
        }
    }

    /// Refreshes the marker data from
    /// the editor's folding state right
    /// before a paint pass.
    pub fn sync(&mut self, folded: &[bool], starts: &[bool]) {
        self.folded.clear();
        self.folded.extend_from_slice(folded);
        self.starts.clear();
        self.starts.extend_from_slice(starts);
    }
}

impl Default for Folding {
    fn default() -> Self {
        Self::new(0.0, 0.0)
    }
}

impl RenderComponent for Folding {
    fn paint(&mut self, ctx: &mut PaintCtx) {
        // The strip's visibility follows
        // the document's FoldingMethod, not a view
        // bit: "Cancel Folding" removes it.
        if !ctx.folding_on {
            return;
        }
        let r = Rect::new(self.x, ctx.row_rect.y, self.width, ctx.row_rect.h)
            .intersect(&ctx.clip);
        if r.is_empty() {
            return;
        }
        ctx.draw.fill(r, ctx.theme.folding_bg);
        paint_active_band(ctx, self.x, self.width);
        // `is_folded(line + 1)`: the next line is
        // hidden under a fold that opens here
        // (collapsed, the `+` state); the fold
        // levels say whether a block opens at
        // this line (expandable, the `-`
        // state). Either state draws a marker.
        let collapsed = self
            .folded
            .get(ctx.line + 1)
            .copied()
            .unwrap_or(false);
        let expandable = self
            .starts
            .get(ctx.line)
            .copied()
            .unwrap_or(false);
        if collapsed || expandable {
            let line_h = ctx.theme.line_height;
            let xw = ctx.theme.x_width;
            // the marker's centre
            // sits (aveCharWidth / 3) left of the
            // strip's centre, one half-row down.
            let cx = r.x + (r.w - xw / 3.0) / 2.0;
            let cy = r.y + (line_h + 1.0) / 2.0 - 1.0;
            // Style 2 geometry. The default FoldingStyle is 3
            // (Marlett glyphs), which this implementation has no font
            // for. The box: four 1 px edges over a 9x9 extent.
            ctx.draw.fill(
                Rect::new(cx - 4.0, cy - 4.0, 9.0, 1.0),
                ctx.theme.folding_fg,
            );
            ctx.draw.fill(
                Rect::new(cx - 4.0, cy + 4.0, 9.0, 1.0),
                ctx.theme.folding_fg,
            );
            ctx.draw.fill(
                Rect::new(cx - 4.0, cy - 4.0, 1.0, 9.0),
                ctx.theme.folding_fg,
            );
            ctx.draw.fill(
                Rect::new(cx + 4.0, cy - 4.0, 1.0, 9.0),
                ctx.theme.folding_fg,
            );
            // the horizontal bar crosses the box
            ctx.draw.fill(
                Rect::new(cx - 2.0, cy, 6.0, 1.0),
                ctx.theme.folding_fg,
            );
            if collapsed {
                // the vertical bar only when collapsed
                ctx.draw.fill(
                    Rect::new(cx, cy - 2.0, 1.0, 6.0),
                    ctx.theme.folding_fg,
                );
            }
        }
        // finally the boundary line, owned by
        // the folding strip whenever the icon area
        // is away.
        if ctx.view.get(ViewFlags::BOUNDARY_LINE)
            && !ctx.view.get(ViewFlags::ICON_AREA)
        {
            paint_boundary(ctx, r.x + r.w - 1.0, r.y, r.h);
        }
    }

    fn below(&mut self, ctx: &mut PaintCtx) {
        // strip ∩ clip in the
        // folding colour, plus the same
        // separator rule as the per-line pass.
        if !ctx.folding_on {
            return;
        }
        let r = Rect::new(self.x, ctx.clip.y, self.width, ctx.clip.h)
            .intersect(&ctx.clip);
        if r.is_empty() {
            return;
        }
        ctx.draw.fill(r, ctx.theme.folding_bg);
        if ctx.view.get(ViewFlags::BOUNDARY_LINE)
            && !ctx.view.get(ViewFlags::ICON_AREA)
        {
            paint_boundary(ctx, r.x + r.w - 1.0, r.y, r.h);
        }
    }
}

/// Bookmark gutter markers (the "Bookmark
/// Gutter" view option, `BookMark` theme key).
pub struct BookMarkRender {
    pub width: f32,
    pub x: f32,
    /// Bookmarked flag per document
    /// line.
    pub marks: Vec<bool>,
}

impl BookMarkRender {
    pub fn new(width: f32, x: f32) -> Self {
        Self {
            width,
            x,
            marks: Vec::new(),
        }
    }

    /// Refreshes the marker data from
    /// the editor's bookmark state right
    /// before a paint pass.
    pub fn sync(&mut self, marks: &[bool]) {
        self.marks.clear();
        self.marks.extend_from_slice(marks);
    }
}

impl Default for BookMarkRender {
    fn default() -> Self {
        Self::new(0.0, 0.0)
    }
}

impl RenderComponent for BookMarkRender {
    fn paint(&mut self, ctx: &mut PaintCtx) {
        if !ctx.view.get(ViewFlags::BOOKMARK) {
            return;
        }
        let r = Rect::new(self.x, ctx.row_rect.y, self.width, ctx.row_rect.h)
            .intersect(&ctx.clip);
        if r.is_empty() {
            return;
        }
        ctx.draw.fill(r, ctx.theme.gutter_bg);
        paint_active_band(ctx, self.x, self.width);
        if self
            .marks
            .get(ctx.line)
            .copied()
            .unwrap_or(false)
        {
            // BookmarkStyle 1 (shipped): a bar
            // across the middle third of the row,
            // `third = (line_height + padding + 1)
            // / 3` with this implementation's zero padding.
            // The outer bar takes gutter_fg, the
            // inner (inflated by 1) bar icon_fg.
            let line_h = ctx.theme.line_height;
            let third = ((line_h + 1.0) / 3.0).floor();
            ctx.draw.fill(
                Rect::new(r.x + 1.0, r.y + third, r.w - 1.0, third),
                ctx.theme.gutter_fg,
            );
            if third > 2.0 {
                ctx.draw.fill(
                    Rect::new(
                        r.x + 2.0,
                        r.y + third + 1.0,
                        r.w - 3.0,
                        third - 2.0,
                    ),
                    ctx.theme.icon_fg,
                );
            }
        }
    }

    fn below(&mut self, ctx: &mut PaintCtx) {
        // strip ∩ clip in the
        // gutter colour.
        if !ctx.view.get(ViewFlags::BOOKMARK) {
            return;
        }
        let r = Rect::new(self.x, ctx.clip.y, self.width, ctx.clip.h)
            .intersect(&ctx.clip);
        if r.is_empty() {
            return;
        }
        ctx.draw.fill(r, ctx.theme.gutter_bg);
    }
}

/// 行修改状态: the strip paints each line's whole band in the colour its edit
/// state calls for, so a modified line reads as a red column and a line the
/// last save wrote back as a green one. An untouched line takes the plain
/// background, which is what makes the other two stand out.
pub struct EditState {
    pub width: f32,
    pub x: f32,
}

impl RenderComponent for EditState {
    fn paint(&mut self, ctx: &mut PaintCtx) {
        if !ctx.view.get(ViewFlags::LINE_STATE) {
            return;
        }
        let band = match ctx.line_state {
            1 => ctx.theme.line_state_modified,
            2 => ctx.theme.line_state_saved,
            _ => ctx.theme.background,
        };
        // the whole line band, no
        // clip intersection, in the state's
        // own colour.
        let r = Rect::new(self.x, ctx.row_rect.y, self.width, ctx.row_rect.h);
        ctx.draw.fill(r, band);
        paint_active_band(ctx, self.x, self.width);
    }

    fn below(&mut self, ctx: &mut PaintCtx) {
        // strip ∩ clip in the
        // below-text colour (DefaultBgNull).
        if !ctx.view.get(ViewFlags::LINE_STATE) {
            return;
        }
        let r = Rect::new(self.x, ctx.clip.y, self.width, ctx.clip.h)
            .intersect(&ctx.clip);
        if !r.is_empty() {
            ctx.draw.fill(r, ctx.theme.below_bg);
        }
    }
}

// Tag-based rules painted on top of token runs —
// the paired-tag highlight: the
// data model and the token-overlay logic
// live in `pipeline` (TagHighlight), which
// `PaintPipeline::draw_line` consults per
// row.

/// 行注记的图标绘制: the glyph colours per icon
/// kind. The diagnostics share the first three
/// hues (error red / warning amber / info blue).
const ICON_INFO_BODY: Color = Color::rgb(0x37, 0x94, 0xFF);
const ICON_WARNING_BODY: Color = Color::rgb(0xCC, 0xA7, 0x00);
const ICON_ERROR_BODY: Color = Color::rgb(0xF4, 0x47, 0x47);
const ICON_FLAG_BODY: Color = Color::rgb(0x2E, 0xA0, 0x43);
const ICON_GLYPH_LIGHT: Color = Color::rgb(0xFF, 0xFF, 0xFF);
const ICON_GLYPH_DARK: Color = Color::rgb(0x1E, 0x1E, 0x1E);
const ICON_FLAG_POLE: Color = Color::rgb(0x8A, 0x8A, 0x8A);

/// Draws one code-generated glyph filling the
/// `s × s` square at (`cx`, `cy`). Everything is
/// fills — no image assets anywhere.
fn draw_line_icon(draw: &mut DrawList, cx: f32, cy: f32, s: f32, kind: u16) {
    match kind {
        // 信息: blue body, white "i".
        crate::component::LINE_ICON_INFO => {
            draw.fill(Rect::new(cx, cy, s, s), ICON_INFO_BODY);
            let w = (s * 0.16).max(1.0);
            draw.fill(Rect::new(cx + (s - w) / 2.0, cy + s * 0.16, w, w), ICON_GLYPH_LIGHT);
            draw.fill(Rect::new(cx + (s - w) / 2.0, cy + s * 0.42, w, s * 0.40), ICON_GLYPH_LIGHT);
        }
        // 警告: amber triangle, dark "!".
        crate::component::LINE_ICON_WARNING => {
            let rows = 7;
            let rh = s / rows as f32;
            for i in 0..rows {
                let frac = (i + 1) as f32 / rows as f32;
                let rw = (s * frac).max(1.0);
                draw.fill(
                    Rect::new(cx + (s - rw) / 2.0, cy + rh * i as f32, rw, rh + 0.5),
                    ICON_WARNING_BODY,
                );
            }
            let w = (s * 0.14).max(1.0);
            draw.fill(Rect::new(cx + (s - w) / 2.0, cy + s * 0.44, w, s * 0.26), ICON_GLYPH_DARK);
            draw.fill(Rect::new(cx + (s - w) / 2.0, cy + s * 0.76, w, w), ICON_GLYPH_DARK);
        }
        // 错误: red body, white "x".
        crate::component::LINE_ICON_ERROR => {
            draw.fill(Rect::new(cx, cy, s, s), ICON_ERROR_BODY);
            let arm = (s * 0.16).max(1.0);
            let steps = 5;
            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let ox = s * 0.16 + t * (s * 0.68 - arm);
                let oy = s * 0.16 + t * (s * 0.68 - arm);
                draw.fill(Rect::new(cx + ox, cy + oy, arm, arm), ICON_GLYPH_LIGHT);
                draw.fill(Rect::new(cx + s - arm - ox, cy + oy, arm, arm), ICON_GLYPH_LIGHT);
            }
        }
        // 旗帜: grey pole, green pennant.
        _ => {
            draw.fill(Rect::new(cx + s * 0.18, cy + s * 0.06, s * 0.10, s * 0.88), ICON_FLAG_POLE);
            let rows = 5;
            let ph = s * 0.60;
            let rh = ph / rows as f32;
            for i in 0..rows {
                let frac = 1.0 - i as f32 / rows as f32;
                let rw = (s * 0.62 * frac).max(1.0);
                draw.fill(
                    Rect::new(cx + s * 0.28, cy + s * 0.06 + rh * i as f32, rw, rh + 0.5),
                    ICON_FLAG_BODY,
                );
            }
        }
    }
}

/// 行注记 glyphs in the icon strip: one
/// icon per annotated line. Painted after
/// the diagnostics, so on a line that
/// carries both, the 行注记 glyph wins the
/// strip and the diagnostic keeps its
/// squiggle underlay in the text.
pub struct LineIconMarks {
    /// Strip width in px (shared with the
    /// diagnostics' strip).
    pub width: f32,
    /// Strip origin in px.
    pub x: f32,
    /// (line, icon kind) pairs for the
    /// visible lines, ascending.
    pub entries: Vec<(usize, u16)>,
}

/// 行注记 icon kinds (the 行注记 model's
/// numbering; the flag is the fourth).
pub const LINE_ICON_INFO: u16 = 0;
pub const LINE_ICON_WARNING: u16 = 1;
pub const LINE_ICON_ERROR: u16 = 2;
pub const LINE_ICON_FLAG: u16 = 3;

impl LineIconMarks {
    pub fn new() -> Self {
        Self {
            width: 0.0,
            x: 0.0,
            entries: Vec::new(),
        }
    }
}

impl Default for LineIconMarks {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderComponent for LineIconMarks {
    fn paint(&mut self, ctx: &mut PaintCtx) {
        if !ctx.view.get(ViewFlags::ICON_AREA) {
            return;
        }
        let Ok(idx) = self.entries.binary_search_by_key(&ctx.line, |&(l, _)| l) else {
            return;
        };
        let s = (ctx.theme.line_height * 0.55).min(self.width * 0.55).max(4.0);
        let cx = self.x + (self.width - s) / 2.0;
        let cy = ctx.row_rect.y + (ctx.row_rect.h - s) / 2.0;
        draw_line_icon(ctx.draw, cx, cy, s, self.entries[idx].1);
    }

    fn below(&mut self, _ctx: &mut PaintCtx) {
        // The strip's background belongs to the
        // diagnostics' strip pass; glyphs only here.
    }
}

/// LSP diagnostic markers in the gutter.
/// The Icon
/// gutter strip: one
/// dot per line with diagnostics, colored by
/// severity.
pub struct DiagnosticMarks {
    /// Strip width in px (the icon area is a
    /// fixed 20 px).
    pub width: f32,
    /// Strip origin in px.
    pub x: f32,
    /// (line, severity) pairs from the
    /// LSP client.
    pub marks: Vec<(usize, u8)>,
}

impl DiagnosticMarks {
    pub fn new() -> Self {
        Self {
            width: 0.0,
            x: 0.0,
            marks: Vec::new(),
        }
    }
}

impl Default for DiagnosticMarks {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderComponent for DiagnosticMarks {
    fn paint(&mut self, ctx: &mut PaintCtx) {
        // cm_view_iconarea ( bit 22) takes the strip away entirely.
        if !ctx.view.get(ViewFlags::ICON_AREA) {
            return;
        }
        let r = Rect::new(self.x, ctx.row_rect.y, self.width, ctx.row_rect.h)
            .intersect(&ctx.clip);
        if r.is_empty() {
            return;
        }
        ctx.draw.fill(r, ctx.theme.icon_bg);
        paint_active_band(ctx, self.x, self.width);
        // the implementation carries no icons, so only the LSP
        // diagnostic marker draws.
        if let Some(&(_, sev)) = self
            .marks
            .iter()
            .find(|(l, _)| *l == ctx.line)
        {
            // severity: 1=error (red), 2=warning
            // (amber), 3=info (blue).
            let color = match sev {
                1 => Color::rgb(0xF4, 0x47, 0x47),
                2 => Color::rgb(0xCC, 0xA7, 0x00),
                _ => Color::rgb(0x37, 0x94, 0xFF),
            };
            // Squiggle underlay: a wavy alpha-blended line under the
            // text (the diagnostic underlay), starting at the text
            // area's origin.
            let line_h = ctx.theme.line_height;
            let xw = ctx.theme.x_width;
            let wave_h = (xw * 0.25).max(1.0);
            let base_y = r.y + line_h - wave_h - 1.0;
            let step = xw.max(1.0);
            let mut x = ctx.theme.gutter_x;
            let mut up = true;
            while x < ctx.theme.gutter_x + r.w {
                let y_off = if up { 0.0 } else { wave_h };
                let rect = Rect::new(x, base_y + y_off, step * 0.6, 1.0);
                ctx.draw.alpha_blend(rect, color, 0.85);
                up = !up;
                x += step * 0.6;
            }
            // The marker dot, 2 px inside the
            // strip's left edge, vertically
            // centred.
            let d = ctx.theme.x_width * 0.5;
            let x = r.x + 2.0;
            let y = r.y + (line_h - d) / 2.0;
            ctx.draw.fill(Rect::new(x, y, d, d), color);
        }
        // the boundary line is
        // unconditional — the icon area is the
        // rightmost gutter strip.
        if ctx.view.get(ViewFlags::BOUNDARY_LINE) {
            paint_boundary(ctx, r.x + r.w - 1.0, r.y, r.h);
        }
    }

    fn below(&mut self, ctx: &mut PaintCtx) {
        // strip ∩ clip in the
        // icon colour, plus the unconditional
        // boundary line.
        if !ctx.view.get(ViewFlags::ICON_AREA) {
            return;
        }
        let r = Rect::new(self.x, ctx.clip.y, self.width, ctx.clip.h)
            .intersect(&ctx.clip);
        if r.is_empty() {
            return;
        }
        ctx.draw.fill(r, ctx.theme.icon_bg);
        if ctx.view.get(ViewFlags::BOUNDARY_LINE) {
            paint_boundary(ctx, r.x + r.w - 1.0, r.y, r.h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::DrawCmd;

    /// 行修改状态: the strip's band takes the colour of the line's state — the
    /// plain background for an untouched line, red for a modified one and
    /// green for one the last save wrote back.
    #[test]
    fn the_edit_state_strip_paints_one_colour_per_line_state() {
        let theme = Theme::default();
        for (state, want) in [
            (0u8, theme.background),
            (1, theme.line_state_modified),
            (2, theme.line_state_saved),
        ] {
            let mut view = ViewFlags::shipped_defaults();
            view.set(ViewFlags::LINE_STATE, true);
            let mut draw = DrawList::default();
            let mut ctx = PaintCtx {
                clip: Rect::new(0.0, 0.0, 100.0, 100.0),
                row: 0,
                line: 0,
                row_rect: Rect::new(0.0, 0.0, 100.0, 18.0),
                theme: &theme,
                view,
                draw: &mut draw,
                display_no: 1,
                tilde: false,
                wrap_rows: 1,
                eol_kind: 2,
                line_state: state,
                folding_on: true,
                active_subrow: 0,
                caret_line: 99,
            };
            let mut strip = EditState {
                width: 2.0,
                x: 10.0,
            };
            strip.paint(&mut ctx);
            assert!(
                draw.cmds.iter().any(|c| matches!(
                    c,
                    DrawCmd::FillRect { rect, color }
                        if *color == want && rect.x == 10.0 && rect.w == 2.0
                )),
                "state {state} must paint its band"
            );
        }
    }
}
