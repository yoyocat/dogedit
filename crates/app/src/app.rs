use dg_core::linedoc::Pos;
use dg_core::{
    Bookmarks, Document, EditOp, Eol, FindOptions, FlagBuffer, Folding, FoldingMethod,
    LineTips, MacroCommand, Match, SelectionSet, UndoStack,
};
use dg_core::filesearch::RowClass;
use dg_layout::measure::{attaches_to_previous, Font};
use dg_layout::vertical::RowIndex;
use dg_layout::wrap::LayoutCtx;
use dg_layout::wrap::{WrapEngine, Wrapper};
use dg_render::pipeline::{CharClass, DrawList, PaintPipeline};
use dg_render::theme::{Color, Theme, ThemeFile, parse_theme_ini};
use dg_render::{MiniMapModel, RulerModel, ViewFlags};
use dg_syntax::{Highlighter, ThemeConfig};

/// URL detector for 高亮网址和邮件 (cm_highlight_urlemail,
/// 1327). Compiled once: the pattern is matched against every
/// visible row on each paint, so it must not be recompiled.
static URL_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r#"(?i)\b(?:https?://|www\.)[^\s<>"']+"#).unwrap()
});

/// Email detector for the same command.
static EMAIL_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b").unwrap()
});

/// `#RGB` / `#RRGGBB` / `#RRGGBBAA` — the three-digit form
/// duplicates its digits (`#F` → `#FF`), as CSS does.

/// Serves parse chunks straight from the line store
/// — from byte `off` (the tree-sitter domain,
/// answered by the sparse byte checkpoints), take consecutive lines
/// plus their real terminators until ~64 KiB or EOF. One owned
/// chunk per call; nothing document-sized is ever allocated.
pub(crate) fn serve_chunk(doc: &Document, off: usize) -> Vec<u8> {
    serve_bytes(doc, off, 64 * 1024)
}

/// [`serve_chunk`] with an explicit byte budget: the query-predicate text
/// provider asks for exactly one node's bytes, and a 64 KiB window per
/// `#match?` would be wasteful on long lines.
pub(crate) fn serve_bytes(doc: &Document, off: usize, cap: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(cap.min(64 * 1024));
    let mut l = doc.byte_line(off);
    let mut skip = off - doc.line_byte_offset(l);
    while out.len() < cap && l < doc.line_count() {
        let mut s = String::new();
        doc.line_text(l, &mut s);
        let term: &[u8] = match doc.eol_kind(l) {
            1 => b"\r\n",
            2 => b"\n",
            3 => b"\r",
            _ => b"",
        };
        if skip > 0 {
            // The skip may land inside the terminator: the line plus
            // its terminator is one byte stream (content UTF-8, then
            // the ASCII terminator).
            let content = s.len();
            if skip >= content {
                let t_skip = (skip - content).min(term.len());
                out.extend_from_slice(&term[t_skip..]);
            } else {
                out.extend_from_slice(&s.as_bytes()[skip..]);
                out.extend_from_slice(term);
            }
            skip = 0;
            l += 1;
            continue;
        }
        out.extend_from_slice(s.as_bytes());
        out.extend_from_slice(term);
        l += 1;
    }
    out.truncate(cap);
    out
}

/// The document's bytes for `[start, end)` — what a `Query` predicate
/// (`#match?`, `#eq?`, `#any-of?`) needs for one captured node.
pub(crate) fn doc_slice(doc: &Document, start: usize, end: usize) -> Vec<u8> {
    let end = end.min(doc.total_bytes());
    if start >= end {
        return Vec::new();
    }
    serve_bytes(doc, start, end - start)
}

/// (Re)parses `h` from the document's own line store — the R4
/// replacement for `h.reset(&doc.to_text())`.
pub(crate) fn parse_from_doc(doc: &Document, h: &mut Highlighter) {
    let doc_len = doc.total_bytes();
    let mut feed = |off: usize| serve_chunk(doc, off);
    h.reset_feed(doc_len, &mut feed);
}

fn parse_hex_literal(hex: &str) -> Option<Color> {
    let v = u32::from_str_radix(hex, 16).ok()?;
    let (r, g, b, a) = match hex.len() {
        3 => (
            (((v >> 8) & 0xF) * 0x11) as u8,
            (((v >> 4) & 0xF) * 0x11) as u8,
            ((v & 0xF) * 0x11) as u8,
            255u8,
        ),
        6 => (
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
            255u8,
        ),
        8 => (
            ((v >> 24) & 0xFF) as u8,
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        ),
        _ => return None,
    };
    Some(Color::rgba(r, g, b, a))
}

/// `rgb(r, g, b)` / `rgba(r, g, b, a)` starting at `chars[i]`.
/// Returns the char index just past the closing paren and the
/// color. The `rgba()` alpha may be a 0–255 integer or a 0–1
/// fraction
/// reads them.
fn parse_rgb_literal(chars: &[char], i: usize) -> Option<(usize, Color)> {
    let n = chars.len();
    let lower: Vec<char> = chars[i..n.min(i + 5)]
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let is_rgba = lower.starts_with(&['r', 'g', 'b', 'a', '(']);
    let is_rgb = lower.starts_with(&['r', 'g', 'b', '(']);
    if !is_rgba && !is_rgb {
        return None;
    }
    let open = if is_rgba { i + 5 } else { i + 4 };
    let mut close = open;
    while close < n && chars[close] != ')' {
        close += 1;
    }
    if close >= n {
        return None;
    }
    let body: String = chars[open..close].iter().collect();
    let parts: Vec<&str> = body.split(',').map(|s| s.trim()).collect();
    let parse_int = |s: &str| -> Option<u8> {
        let v = s.parse::<i32>().ok()?;
        if (0..=255).contains(&v) {
            Some(v as u8)
        } else {
            None
        }
    };
    let (r, g, b, a) = if is_rgba && parts.len() == 4 {
        let r = parse_int(parts[0])?;
        let g = parse_int(parts[1])?;
        let b = parse_int(parts[2])?;
        let a = if let Ok(v) = parts[3].parse::<i32>() {
            v.clamp(0, 255) as u8
        } else if let Ok(f) = parts[3].parse::<f32>() {
            (f.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            255u8
        };
        (r, g, b, a)
    } else if parts.len() == 3 {
        let r = parse_int(parts[0])?;
        let g = parse_int(parts[1])?;
        let b = parse_int(parts[2])?;
        (r, g, b, 255u8)
    } else {
        return None;
    };
    Some((close + 1, Color::rgba(r, g, b, a)))
}

/// `(line, byte column)` → tree-sitter `Point`.
fn point_of((row, column): (usize, usize)) -> tree_sitter::Point {
    tree_sitter::Point { row, column }
}

/// The line space an edit replaces: where it starts, how many lines the text it
/// removes spans, and how many the text it writes spans. Both the forward door
/// ([`Editor::apply_edit`]) and a revert folded back through it derive the relayout
/// region and the bookmark shift from this.
fn edit_region(op: &dg_core::EditOp) -> (Pos, usize, usize) {
    match op {
        dg_core::EditOp::Insert { pos, text } => (
            *pos,
            1,
            (dg_core::undo::text_lines(text) as usize) + 1,
        ),
        dg_core::EditOp::Delete { start, text, .. } => (
            *start,
            (dg_core::undo::text_lines(text) as usize) + 1,
            1,
        ),
        dg_core::EditOp::Replace {
            start,
            old_text,
            new_text,
            ..
        } => (
            *start,
            (dg_core::undo::text_lines(old_text) as usize) + 1,
            (dg_core::undo::text_lines(new_text) as usize) + 1,
        ),
    }
}

/// 撤销 / 重做后光标的落点，按这次手势的编辑描述推算：撤销落在每个
/// 编辑的起点（逆编辑都从那里把文本放回来，也就是编辑发生前光标所
/// 在的位置），重做落在编辑落完后的位置（插入看插入文本的结尾）。
/// 一次手势的多个编辑彼此不相交，取最靠前的一处；描述不出来（比如
/// 回放的宏）就答 `None`，光标留在原地。
fn revert_caret_target(ops: Option<&[EditOp]>, redo: bool) -> Option<(usize, usize)> {
    let mut best: Option<Pos> = None;
    for op in ops? {
        let p = if redo {
            match op {
                EditOp::Insert { pos, text } => dg_core::undo::text_end_pos(*pos, text),
                EditOp::Delete { start, .. } => *start,
                EditOp::Replace { start, new_text, .. } => {
                    dg_core::undo::text_end_pos(*start, new_text)
                }
            }
        } else {
            match op {
                EditOp::Insert { pos, .. } => *pos,
                EditOp::Delete { start, .. } => *start,
                EditOp::Replace { start, .. } => *start,
            }
        };
        if best.is_none_or(|b| p < b) {
            best = Some(p);
        }
    }
    best.map(|p| (p.line, p.col))
}

/// The one contiguous region covering two edited line spans. The unchanged lines
/// between them keep their count in both the old and the new line space; an overlap
/// has no exact region description and answers `None`.
fn span_layout_regions(
    a: (usize, usize, usize),
    b: (usize, usize, usize),
) -> Option<(usize, usize, usize)> {
    let ((lo_f, lo_o, lo_n), (hi_f, hi_o, hi_n)) = if b.0 < a.0 { (b, a) } else { (a, b) };
    let gap = hi_f.saturating_sub(lo_f + lo_n);
    if gap == 0 && hi_f < lo_f + lo_n {
        // overlap: no exact region description
        return None;
    }
    Some((lo_f, lo_o + gap + hi_o, lo_n + gap + hi_n))
}

/// Records one more edited line span for the incremental relayout.
///
/// A span that moves no lines keeps its own entry, so an edit at five carets
/// re-wraps five lines instead of the whole distance between the outer two; spans
/// that reach each other fold together, and an overlapping pair empties the record
/// because nothing describes it cheaply — the view then owes a full relayout. A span
/// that *does* change the line count moves every line below it, so the whole record
/// collapses into the single contiguous region over it.
fn record_layout_region(
    pending: &mut Vec<(usize, usize, usize)>,
    first: usize,
    old_lines: usize,
    new_lines: usize,
) {
    let structural = old_lines != new_lines;
    let mut spans = std::mem::take(pending);
    spans.push((first, old_lines, new_lines));
    spans.sort_unstable_by_key(|r| r.0);
    let mut folded: Vec<(usize, usize, usize)> = Vec::with_capacity(spans.len());
    for r in spans {
        match folded.last().copied() {
            Some(prev) if r.0 <= prev.0 + prev.2 => {
                let Some(m) = span_layout_regions(prev, r) else {
                    return;
                };
                *folded.last_mut().unwrap() = m;
            }
            _ => folded.push(r),
        }
    }
    if structural {
        let mut acc: Option<(usize, usize, usize)> = None;
        for r in folded {
            let next = match acc {
                None => Some(r),
                Some(prev) => span_layout_regions(prev, r),
            };
            let Some(m) = next else {
                return;
            };
            acc = Some(m);
        }
        folded = acc.map(|r| vec![r]).unwrap_or_default();
    }
    *pending = folded;
}

/// Word-wrap mode — the Document
/// menu's Word Wrap submenu (cmd
/// 1294/1297/1296/1295/1293/1283).
/// The GUI backend builds the
/// matching `Wrapper` from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WrapMode {
    /// Cancel Wrap: no wrapping.
    Cancel,
    /// Smart Wrap: window edge, break at word edges.
    #[default]
    Smart,
    /// Wrap by Window: window edge,
    /// break anywhere.
    Window,
    /// Fixed Column: N characters.
    FixedColumn,
    /// Fixed Column with Tabs: N
    /// display columns, tab-aware.
    FixedColumnTabs,
    /// Fixed Pixel Width.
    FixedPixel,
}

/// Frame-level highlight toggles — the 颜色高亮 (cm_highlight_color,
/// 1271), 选区高亮 (cm_highlight_sel, 1313) and 高亮网址和邮件
/// (cm_highlight_urlemail, 1327) families. The paint is handed them
/// instead of reading them off the editor.
#[derive(Clone, Copy, Default)]
pub struct HighlightToggles {
    pub color: bool,
    pub sel: bool,
    pub url: bool,
}

/// 监视该文件变更子菜单对这篇文档的设值。`Global` 跟随全局开关
/// （`[Setting] MonitorFile`），其余三档盖过它：变更时提示、自动重新
/// 读取、读取后滚到末尾。定时自动读取是叠加位，只对两档自动读取
/// 有意义。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WatchMode {
    /// 使用全局监视设置.
    #[default]
    Global = 0,
    /// 提示变更.
    Prompt = 1,
    /// 自动读取.
    AutoRead = 2,
    /// 自动读取并滚动到最后.
    AutoReadEnd = 3,
}

/// Movement directions for the caret. Each one names a position, so a `cm_move_*`
/// command and its `cm_select_*` twin differ only by the `extend` flag of
/// [`Editor::caret_motion`]
/// one handler between the two.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoveDir {
    Left,
    Right,
    Up,
    Down,
    /// `cm_move_toheadofsubline` (73): the start of the wrapped row.
    SublineHome,
    /// `cm_move_toendofsubline` (74).
    SublineEnd,
    /// `cm_move_toheadofline` (5): smart Home over the whole logical line.
    LineHead,
    /// `cm_move_toendofline` (6).
    LineEnd,
    /// `cm_move_toheadoffile` (7) / `cm_move_toendoffile` (8).
    DocHead,
    DocEnd,
    /// `cm_move_nextword` (11) / `cm_move_prevword` (12).
    WordRight,
    WordLeft,
    PageUp,
    PageDown,
}

/// Platform-neutral editor state. The GUI backends (win32 / cocoa /
/// gtk) own an `Editor`, a `TextMeasurer`, a `Wrapper` and a
/// `DrawList`, and pump the message loop. Case sensitivity
/// (`Edit/QuickCase`) and the whole-word flag the `Edit/QuickSmart`
/// word-edge check produces.
#[derive(Clone, PartialEq, Debug)]
pub struct SelHighlightPat {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

pub struct Editor {
    /// Stable identity across tab reordering and
    /// closes. Asynchronous work (the progressive
    /// loader) reports back with this, so a result
    /// lands in the document it was started for
    /// instead of whatever tab happens to be active
    /// when it finishes.
    pub id: u64,
    pub doc: Document,
    pub wrap: WrapEngine,
    pub rows: RowIndex,
    pub folding: Folding,
    pub selection: SelectionSet,
    pub flags: FlagBuffer,
    pub highlighter: Option<Highlighter>,
    pub syntax_theme: ThemeConfig,
    /// 最近一次附加语法时 query 的编译错误（`queries\*.scm` 写坏了）。
    /// 编译失败只是回落 kind 路径，但要让用户看见原因 —— 「重新扫描
    /// 解析器」的报告会把它列出来。
    pub syntax_query_error: Option<String>,
    pub theme: Theme,
    /// First visible visual row.
    pub first_row: usize,
    /// Horizontal scroll in characters.
    pub h_scroll: i32,
    /// Caret (line, char-within-line).
    pub caret: (usize, usize),
    /// 激活选区锚点 (`cm_select_anchor`, the view): the
    /// fixed edge anchor mode rebuilds every selection from,
    /// or `None` while the mode is off. Every plain caret
    /// move commits `[anchor, caret]` as the selection; the
    /// select twins and a mouse press release the mode first.
    pub anchor: Option<(usize, usize)>,
    /// 加大字号/减小字号 (`cm_view_zoominfont`, `cm_view_zoomoutfont`):
    /// the size this document's editor font is drawn at, in points. `None` is the
    /// size the configuration names — 原始字号 (`cm_view_original_fontsize`,
    /// "使用字体的原始大小") puts the document back to it, and a document nobody
    /// zoomed never builds a second font.
    ///
    /// Per-document because the menu text says so: 加大字号
    /// 增大的是**当前文档**的字号.
    pub font_points: Option<f32>,
    /// Additional carets for
    /// multi-caret editing; the
    /// primary caret is `caret`.
    /// Each extra caret pairs with
    /// a selection range whose end
    /// it sits at.
    pub extra_carets: Vec<(usize, usize)>,
    /// the view - the view: the width of the text area in pixels, which is what
    /// the caret's horizontal reveal is measured against. `0` until the layout has
    /// run once, and the caret reveal's `IsRectEmpty` test keeps that case a no-op.
    pub text_w: i32,
    /// the view != 0`: the document is being wrapped, so there is no horizontal
    /// scroll to reveal into — the gate the caret reveal puts in front of its whole
    /// horizontal arm.
    pub wrapped: bool,
    /// Total visual rows.
    pub total_rows: usize,
    /// Column ruler (ruler paint).
    pub ruler: RulerModel,
    /// Whole-document minimap.
    pub minimap: MiniMapModel,
    /// `[Edit] MiniMapStyle`: 1 动态隐藏 (the box shows while the pointer is
    /// on the strip), 2 背景色 (always), 3 边框 (an outline instead of a fill).
    pub minimap_style: u8,
    /// The overlay scrollbar strip the map's column is measured against: the
    /// strip keeps the map off the scrollbar's own pixels.
    pub map_scroll_w: f32,
    /// The map's strip at the last layout, in the editor's own space. Cached
    /// because the pointer asks for it on every move, and asking the window
    /// for its client rect on every move is a syscall the text area pays for.
    pub map_strip: dg_render::component::Rect,
    /// The pointer is on the map's strip. The strip repaints when this flips
    /// under style 1, because the box comes and goes with it.
    pub map_hover: bool,
    /// A drag on the map's box: the pointer's offset inside the box, in
    /// pixels, from the press that grabbed it.
    pub map_drag: Option<f32>,
    /// Visual-row count and syntax version
    /// at the last minimap rebuild, avoids
    /// re-walking the highlighter on every
    /// paint.
    minimap_rows: usize,
    minimap_version: u64,
    /// 折叠区缓存，键是解析代数：一趟折叠区计算要走整棵树（插件包给了
    /// folds.scm 时还是 query cursor），而绘制每帧都要问一次。
    fold_cache: std::cell::RefCell<Option<(u64, Vec<(usize, usize)>)>>,
    /// 语法高亮 runs 的按行缓存: `(syntax version, text version)` → per-line runs.
    /// The query is the expensive half of painting a row, and a wrapped line asks
    /// for the same line once per visual row. Dropped whole when either version
    /// moves, and capped so a long scroll cannot grow it without bound.
    hl_run_cache: std::cell::RefCell<Option<(u64, u64, std::collections::HashMap<usize, Vec<(u32, u32, u8)>>)>>,
    /// 折叠区缓存，键是文本版本 + 折叠方法 + 制表位：缩进折叠要逐行走完整篇，
    /// 而每帧的折叠标记问的就是这份结果。`starts` 是同一份区间的按行展开。
    fold_regions_cache: std::cell::RefCell<Option<FoldRegions>>,
    /// Bumped whenever the highlighted text
    /// changes (open, attach, edit).
    pub syntax_version: u64,
    /// Undo/redo history (the view's command
    /// stack).
    pub undo: UndoStack,
    /// Bookmarks (the line array the bookmark strip draws; the Bookmark
    /// menu: Toggle/Next/Previous/Remove All).
    pub bookmarks: Bookmarks,
    /// 行注记: per-line icon + tooltip text, drawn in the icon strip and
    /// shown as a hover tip over the icon (行注记 API + 下一个/上一个提示).
    pub line_tips: LineTips,
    /// Paired-tag highlight (the view
    /// TagHighlight): recomputed when the
    /// caret or the syntax tree changes.
    pub tag_hl: dg_render::TagHighlight,
    /// Cache key (caret line, caret col,
    /// syntax version) of the last tag
    /// highlight computation.
    tag_hl_key: Option<(usize, usize, u64)>,
    /// 自定义标记: the marker set. Shared, not copied
    /// process-wide singleton rather than per-view state, so
    /// every document holds an [`Rc`] to the same set and 使用标记N on the
    /// visible one marks text in all of them.
    pub markers: std::rc::Rc<std::cell::RefCell<crate::markers::MarkSet>>,
    /// `markers.spans(doc)` cached on (syntax version, marker version), so the
    /// whole-document scan runs once per edit instead of once per painted row.
    mark_spans: Vec<crate::markers::Span>,
    mark_key: Option<(u64, u64)>,
    /// [`Editor::grep_classes`] for the one line the paint pass is on: the run
    /// list, the marks and the attribute classes all read it.
    grep_key: std::cell::RefCell<Option<(usize, u64, Vec<(usize, usize, RowClass)>)>>,
    /// Binary-document state ([HEX] Ext opens the file in the hex engine's
    /// HexView32 instead of the text editor): the parked `HexView` — view
    /// state *and* snapshot in one — while the tab is not on screen. `None`
    /// for every text document.
    pub hex: Option<dg_hex::HexView>,
    /// File on disk, `None` for a buffer that
    /// was never saved. Per-document: it is the
    /// tab label and the save target.
    pub path: Option<String>,
    /// Syntax language chosen at open time
    /// (before the grammar is attached, and the
    /// key to re-attach it after a reload).
    pub syntax_lang: Option<String>,
    /// The hex view's palette, derived from the active theme at every
    /// theme load (see [`hex_colors_from_theme`]). `hex` documents draw
    /// with it; text documents just carry it.
    pub hex_colors: dg_hex::Colors,
    /// Line ending written back on save
    /// (Document menu → EOL).
    pub eol: Eol,
    /// Tab width for display and insertion.
    pub tab_size: u32,
    /// Buffer refuses edits
    /// it for read-only files and the File menu
    /// toggles it.
    /// 部分编辑 (`cm_edit_partially`): the one span the buffer still takes
    /// edits in. `None` is the ordinary case, where only `read_only` answers
    /// for the buffer.
    pub partial_edit: Option<dg_core::Range>,
    pub read_only: bool,
    pub tab_title: Option<String>,
    /// The number a new untouched document's generated name carries. Chosen
    /// when the tab is made; a document with a path or a title of its own
    /// never uses it.
    pub untitled_no: Option<u32>,
    /// 自动备份的目录键。有路径的文档由路径算出，未命名文档在第一次
    /// 备份时随机生成一个 —— 它们没有路径，但恰恰最需要第二份。
    pub backup_key: Option<String>,
    /// 上一次成功备份时的历史位置（[`UndoStack::position`]）与时刻。
    /// 位置没动过就不再写一版，免得把「敲一下再撤销」记成两版。
    pub backup_rev: usize,
    pub backup_at: Option<std::time::Instant>,
    /// The document is the result of a find-in-files run. It is also what makes
    /// the rows addresses — double-clicking one opens the file it names.
    pub grep_rows: Option<dg_core::filesearch::RowHighlighter>,
    /// 大文件模式的行式词法器（log/jsonl/csv）+ 每行 runs 的 LRU
    /// 缓存：tree-sitter 被禁用时由它接管 `highlight_for`
    /// （[`crate::largehl`]）。
    pub large_hl: Option<crate::largehl::LargeHl>,
    /// 大文件编辑纪元：每次成功编辑递增一次。搜索命中缓存的是编辑前的
    /// raw 坐标，纪元变化后命中视为过期（find_next/select_all 重查）。
    pub large_edits: u64,
    /// 尾随模式（大文件右键 36004）：poll 定时器驱动
    /// [`dg_large::LargeDoc::follow_tail_tick`]，扫描完成自动滚底。
    pub follow_tail: bool,
    /// Which rows of the Code Folding submenu are checked: the region derivation
    /// `setting.ini`'s `Edit/FoldingMethod` selects.
    pub folding_method: FoldingMethod,
    /// Column Mode: the selection mode the frame latches on the editor itself.
    /// While it is set, the
    /// movement commands extend a rectangle from the saved anchor instead of
    /// moving a single caret.
    pub column_mode: bool,
    /// Per-document for the same reason the caret is: two tabs of one window can
    /// disagree, and the View menu edits only the active one.
    pub view_flags: ViewFlags,
    /// `[Edit] SpaceStyle` / `TabStyle` / `IndentStyle`: how 空白字符 draws a
    /// blank, 换行符's sibling row draws a tab, and 缩进线 draws a guide. The
    /// shipped config carries 1 / 4 / 1.
    pub space_style: u8,
    pub tab_style: u8,
    pub indent_style: u8,
    /// 列标记 (`ColMarker`, 1337): the columns the view paints a full-height
    /// vertical line down, at most ten — the list's own cap. They are set by
    /// double-clicking the ruler and belong to the document, like the options.
    pub col_markers: Vec<usize>,
    /// The caret-position ring `cm_move_nextcaretpos`/`cm_move_prevcaretpos`
    /// walk — the view.
    pub caret_history: CaretHistory,
    pub edit_history: EditHistory,
    /// What the last selection collapse threw away, kept for
    /// `cm_select_restore` — the view.
    pub saved_selection: Vec<dg_core::Range>,
    /// 选区高亮 (cm_highlight_sel, 1313 / Edit/SelHighlight): the
    /// selected text's occurrence pattern. Recomputed from the
    /// current selection at every paint
    /// does the same work behind a 300 ms
    /// debounce, and this implementation's per-visible-line scan is cheap
    /// enough to ride the paint instead of a timer.
    sel_highlight_pat: Option<SelHighlightPat>,
    /// 监视该文件变更：这篇文档自己的档位，加上定时读取的秒数。
    /// `watch_stamp` 是最近一次从磁盘读进来的修改时间和长度，轮询
    /// 拿它对比出「外部改过」。
    pub watch_mode: WatchMode,
    pub watch_timed: bool,
    pub watch_interval: u32,
    pub watch_stamp: Option<(std::time::SystemTime, u64)>,
    /// `Edit/QuickCase` (default 1): the
    /// occurrence match's case sensitivity. The finder's fold
    /// function is the identity while the flag
    /// is set, i.e. the default is case-sensitive.
    sel_match_case: bool,
    /// `Edit/QuickSmart` (default 0): the
    /// whole-word gate. While set, only selections whose two
    /// edges sit on word boundaries get the whole-word flag, and
    /// only whole-word matches are painted.
    sel_match_word: bool,
    /// `Edit/MaxSelHighlight` (default 64):
    /// selections longer than this clear the highlight instead
    /// of matching
    /// into 1..=1024.
    sel_max_len: usize,
    /// `Edit/KeepHighlight`（选项 → 编辑页「移动光标时保持选区或者查找高亮」，
    /// 出厂 N）：选区塌掉后，上一次选字留下的「选中词全文高亮」是丢掉还是留着。
    keep_highlight: bool,
    /// `Edit/ScrollLimit`（选项 → 编辑页「当光标距边界?行时进行滚动」，出厂 0，
    /// 大于 16 归 0）：翻页时留给自己的上下文行数。只有 PageUp/PageDown 用它，
    /// 方向键和滚动条仍是贴边揭示。
    scroll_limit: usize,
    /// 刚刚自动补出的那一对括号中间的位置。`Edit/JumpWithTab`（选项 → 编辑页
    /// 「按下Tab时跳出刚刚自动输入的配对字符」）认的就是这一格：光标还停在这
    /// 里，Tab 才越过闭括号；走开了、或者被别的编辑挪动了，这一格自然对不上。
    pub pair_jump: Option<(usize, usize)>,
    /// 高亮匹配: the active find query's matches as `(line, start, end)`
    /// char ranges, painted in the `Found` colours under the text like the
    /// selection-highlight occurrences. Empty while the option is off.
    pub find_marks: Vec<(usize, usize, usize)>,
    /// True when the highlighter's mirror text no longer matches
    /// the buffer (load, undo/redo, EOL rewrite, grammar attach):
    /// the next [`Self::sync_syntax`] does a full parse instead of
    /// an incremental one.
    hl_stale: bool,
    /// 快照模式收到了一次「该重新解析了」的请求（`sync_syntax` 的直接调用，
    /// 如全部替换、EOL 重写）。窗口在下一帧看到它就把全文派给工作线程。
    snapshot_requested: bool,
    /// 快照模式下，颜色已经跟到哪一个文本纪元（局部或整篇重算的结果）。
    /// 和 [`Self::edit_epoch`] 不等就说明有改动还没重算 —— 只有这时才值得
    /// 再派工作线程；快照本身「全局还没算」的标志不能单独当触发条件
    /// （局部重算完它仍然成立，会变成空转）。
    snapshot_epoch: Option<u64>,
    /// 即时窗口重算的退避期：上一趟超过了 [`SYNTAX_UI_BUDGET_MS`]，在这个
    /// 时刻之前 [`Self::syntax_window_tick`] 直接让路（退回工作线程的任务），
    /// 别让一次重算连吃几帧。
    syntax_tick_backoff: Option<std::time::Instant>,
    /// Bumped whenever the document's text changes — an edit, undo/redo, a load,
    /// an EOL rewrite. The per-line caches below key on it, so an entry can never
    /// outlive the text it described.
    pub edit_epoch: u64,
    /// The epoch [`Self::measured_advances`] last ran under, plus the rows it
    /// already walked, as `(line, segment, first column)`. The advance table is
    /// a property of the font, so a re-measure resets both.
    measured_epoch: u64,
    measured_lines: std::collections::HashSet<(usize, usize, usize)>,
    /// 不换行: the last full layout ran with a wrapper that never breaks or
    /// measures a line. The wrap arrays then carry char counts, not display
    /// columns, and the horizontal range follows the caret instead.
    pub virtual_wrap: bool,
    /// The widest column the caret has reached while 不换行, in display cells.
    /// Only grows until the next full layout: a document whose lines were never
    /// measured has no exact answer, and this one is exact for the column the
    /// view must be able to show.
    pub h_max_cells: i32,
    /// 光标行位置缓存: `(line, col, epoch, cell width, tab cells, wrapped)` →
    /// `(subrow, px)`. [`Self::caret_row`] runs on every paint and every caret
    /// reveal; without this a caret parked deep inside a long line is re-walked
    /// from the segment's first character each time.
    caret_px_cache:
        std::cell::Cell<Option<((usize, usize, u64, i32, usize, bool, usize), usize, i32)>>,
    /// 窗口锚点: the window the last paint put on each row it drew, by
    /// `(line, segment)`. A repaint that moved the view by a few columns walks
    /// the difference instead of the text in front of the window; a jump fails
    /// the match and the row measures its way in.
    paint_win: dg_render::pipeline::WindowAnchor,
    /// 颜色高亮 (1271): the column ranges the painter has scanned, and their
    /// bands. The scan is bounded to the columns a row shows, so a wrapped line
    /// keeps one entry per row of it and a line of any length costs a screenful.
    color_band_cache: std::cell::RefCell<
        Option<(u64, std::collections::HashMap<(usize, usize, usize), Vec<(usize, usize, Color)>>)>,
    >,
    /// 高亮网址和邮件 (1327): the same table for its spans.
    url_span_cache: std::cell::RefCell<
        Option<
            (
                u64,
                std::collections::HashMap<(usize, usize, usize), Vec<(usize, usize, Color, Color)>>,
            ),
        >,
    >,
    /// The line spans the edits since the last relayout touched, each as
    /// `(first line, old line count, new line count)`, ascending and non-overlapping.
    /// An edit that moves no lines keeps its own entry, so a multi-caret edit (and
    /// its undo) re-wraps the touched lines only. A span that changes the line count
    /// moves every line below it, so the record collapses into one contiguous region.
    /// Overlapping spans have no cheap description and empty the record; undo, redo,
    /// load and EOL rewrites take that route too.
    pending_layout: Vec<(usize, usize, usize)>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

/// The fold derivation of one text version: the regions and the per-line start
/// flags the paint reads. Both come out of the same walk, so they are cached
/// together and can never disagree.
struct FoldRegions {
    epoch: u64,
    /// 语法结果换过一版（快照落地、局部重算拼进表）就要重推：折叠区来自
    /// 区间表，正文没动而表动了，只认 `epoch` 会把「还没有折叠区」一直
    /// 缓存下去。
    syntax: u64,
    method: FoldingMethod,
    tab: u32,
    regions: Vec<(usize, usize)>,
    starts: Vec<bool>,
}

/// The pairs `cm_move_brace` and the two `cm_select_topair` commands work on.
/// This implementation ships the table itself, and leaves `<`/`>` out because in
/// markup they are unmatchable the moment the caret is inside a tag.
const BRACKET_PAIRS: [(char, char); 6] = [
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('（', '）'),
    ('【', '】'),
    ('《', '》'),
];

/// 大文件 undo 捕获上限（字符，M6）：超过此量的删除/替换直接拒绝，
/// 不物化进 undo 栈 —— GB 级全选删除的正确出口是 M5 的选区导出，
/// 而不是把整个文件装进一条撤销记录。
pub const LARGE_UNDO_CAPTURE_CHARS: usize = 8 << 20;

/// How far past the last painted window a 不换行 row measures its advances, in
/// characters. A window is a screenful of columns — a couple of hundred at
/// most — so this is slack, not a bound the row can reach.
const MEASURE_AHEAD: usize = 512;

/// How many columns a caret may travel from its last measured position and
/// still continue from it, in characters. One arrow key or one typed character
/// is the case this serves; a jump beyond it measures from the segment's start.
const CARET_WALK_MAX: usize = 64;

/// How many re-wrapped lines it still pays to check one at a time against the
/// rows' cached windows. Watching a line costs a copy of its break array; past
/// this many, the whole table is dropped instead.
const LAYOUT_WATCH_MAX: usize = 64;

/// 网址/邮件与颜色字面量扫描的左右余量，按列算。两项都只扫一行能显示的那几列，
/// 而一个字面量或网址可能从窗口左边之外就开始了，所以两头各留出这一段；起点落在
/// 余量之外的装饰会被窗口边界裁掉，那是几兆字节的行才碰得到的边角。
const DECOR_SCAN_SLACK: usize = 256;

/// 装饰扫描缓存的条数上限：一行 × 一个窗口一段。超了整表重来，代价是下一帧重扫
/// 可见行，不改变任何结果。
const DECOR_CACHE_MAX: usize = 4096;

/// 界面线程即时局部重算的窗口字节上限（快照模式，[`Editor::syntax_window_tick`]）：
/// 从最近的同类节点起点到光标后 [`SYNTAX_UI_AHEAD_LINES`] 行，超过就退回工作线程
/// 的窗口任务 —— 巨型构造里的一次重算不该花在画帧的路上。
pub(crate) const SYNTAX_UI_WINDOW_BYTES: usize = 32 * 1024;

/// 窗口终点往光标之后多带的行数：一屏以外还有余量，滚屏时下半屏的颜色也在。
const SYNTAX_UI_AHEAD_LINES: usize = 200;

/// 一趟窗口重算的预算：超过就退避一会儿（[`SYNTAX_UI_BACKOFF_MS`] 内不再
/// 逐帧重算，落回定时器节拍），别让一次解析把帧预算吃穿。
const SYNTAX_UI_BUDGET_MS: u128 = 6;
const SYNTAX_UI_BACKOFF_MS: u64 = 50;

/// The closers `cm_move_jumpoutofpair` ( an EXE-side command) scans for.
/// The implementation derives it from [`BRACKET_PAIRS`].
fn is_closer(c: char) -> bool {
    BRACKET_PAIRS.iter().any(|&(_, close)| close == c)
}

/// The caret-position ring behind `cm_move_nextcaretpos` and `cm_move_prevcaretpos` — the
/// view, shape `{cursor, -1, -, enabled, array, count}`. Pushes come from the refresh path,
/// which before appending drops every entry whose line is equal to or one away from the
/// position being recorded, caps the ring at 10 by shifting the oldest out, and parks the
/// cursor on the newest entry. A run of arrow keys inside one paragraph therefore records one
/// point, which is what makes the two commands useful: they walk paragraphs, not keystrokes.
#[derive(Debug)]
pub struct CaretHistory {
    entries: Vec<(usize, usize)>,
    /// `-1` while the ring is empty: the walk
    /// has no current entry until the first
    /// point is recorded.
    cursor: isize,
    /// Suppression latch: the jump itself must not be recorded, or walking the ring would
    /// append the entry it just landed on and the walk would never move.
    recording: bool,
}

impl Default for CaretHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl CaretHistory {
    const CAP: usize = 10;

    fn new() -> Self {
        Self {
            entries: Vec::new(),
            cursor: -1,
            recording: true,
        }
    }

    fn record(&mut self, at: (usize, usize)) {
        if !self.recording {
            return;
        }
        self.entries
            .retain(|e| (e.0 as isize - at.0 as isize).abs() > 1);
        self.entries.push(at);
        if self.entries.len() > Self::CAP {
            self.entries.remove(0);
        }
        self.cursor = self.entries.len() as isize - 1;
    }

    /// Step the cursor and report where to jump. Wraps at both ends
    /// (`++cursor`/`--cursor` with the count as modulus).
    fn jump(&mut self, next: bool) -> Option<(usize, usize)> {
        let count = self.entries.len() as isize;
        if count == 0 || self.cursor < 0 {
            return None;
        }
        self.cursor = if next {
            (self.cursor + 1) % count
        } else {
            (self.cursor - 1 + count) % count
        };
        self.recording = false;
        Some(self.entries[self.cursor as usize])
    }
}

/// 编辑位置环，`cm_move_nexteditpos` / `cm_move_preveditpos` 背后那只。机制
/// 与 [`CaretHistory`] 同构：定长环、游标、两端回绕，跳转本身不落账。记录
/// 端不同——它记的是编辑，[`Editor::apply_edit`] 落笔成功时记下落笔后光标
/// 所在的那格；同行的旧条目被这次落笔刷新（连打一个词只算一处编辑），
/// 不同行的编辑各自成点，走的是编辑落点，不是击键。撤销与重做是历史
/// 导航，不经 [`Editor::apply_edit`]，也就不记。
#[derive(Debug)]
pub struct EditHistory {
    entries: Vec<(usize, usize)>,
    /// 最新一条的下标；环空时为 `-1`。
    cursor: isize,
}

impl Default for EditHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl EditHistory {
    const CAP: usize = 10;

    fn new() -> Self {
        Self {
            entries: Vec::new(),
            cursor: -1,
        }
    }

    /// 同行的旧条目被这次落笔刷新并挪到最新：环按新旧排序，游标回到
    /// 最新——编辑把走到半路的游标拽回来。
    fn record(&mut self, at: (usize, usize)) {
        self.entries.retain(|e| e.0 != at.0);
        self.entries.push(at);
        if self.entries.len() > Self::CAP {
            self.entries.remove(0);
        }
        self.cursor = self.entries.len() as isize - 1;
    }

    /// 步进一格并报落点，两端回绕（与光标环同一口径）。环空无可走。
    fn jump(&mut self, next: bool) -> Option<(usize, usize)> {
        let count = self.entries.len() as isize;
        if count == 0 {
            return None;
        }
        self.cursor = if next {
            (self.cursor + 1) % count
        } else {
            (self.cursor - 1 + count) % count
        };
        Some(self.entries[self.cursor as usize])
    }
}

impl Editor {
    pub fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            hex: None,
            doc: Document::empty(),
            wrap: WrapEngine::default(),
            rows: RowIndex::new(),
            folding: Folding::default(),
            selection: SelectionSet::default(),
            flags: FlagBuffer::default(),
            highlighter: None,
            syntax_theme: ThemeConfig::default(),
            syntax_query_error: None,
            theme: Theme::default(),
            first_row: 0,
            h_scroll: 0,
            caret: (0, 0),
            anchor: None,
            font_points: None,
            extra_carets: Vec::new(),
            text_w: 0,
            wrapped: false,
            total_rows: 0,
            ruler: RulerModel::new(0.0, 0.0),
            minimap: MiniMapModel::default(),
            minimap_style: 1,
            map_scroll_w: 0.0,
            map_strip: dg_render::component::Rect::new(0.0, 0.0, 0.0, 0.0),
            map_hover: false,
            map_drag: None,
            minimap_rows: 0,
            minimap_version: 0,
            fold_cache: std::cell::RefCell::new(None),
            hl_run_cache: std::cell::RefCell::new(None),
            fold_regions_cache: std::cell::RefCell::new(None),
            syntax_version: 0,
            undo: UndoStack::new(),
            bookmarks: Bookmarks::new(),
            line_tips: LineTips::new(),
            tag_hl: dg_render::TagHighlight::default(),
            tag_hl_key: None,
            markers: std::rc::Rc::default(),
            mark_spans: Vec::new(),
            mark_key: None,
            grep_key: std::cell::RefCell::new(None),
            path: None,
            syntax_lang: None,
            hex_colors: dg_hex::Colors::default(),
            eol: Eol::default(),
            tab_size: 4,
            read_only: false,
            partial_edit: None,            tab_title: None,
            untitled_no: None,
            backup_key: None,
            backup_rev: 0,
            backup_at: None,
            grep_rows: None,
            folding_method: FoldingMethod::default(),
            column_mode: false,
            view_flags: ViewFlags::shipped_defaults(),
            space_style: 1,
            tab_style: 4,
            indent_style: 1,
            col_markers: Vec::new(),
            caret_history: CaretHistory::new(),
            edit_history: EditHistory::new(),
            saved_selection: Vec::new(),
            sel_highlight_pat: None,
            watch_mode: WatchMode::Global,
            watch_timed: false,
            watch_interval: 0,
            watch_stamp: None,
            sel_match_case: true,
            sel_match_word: false,
            find_marks: Vec::new(),
            sel_max_len: 64,
            keep_highlight: false,
            scroll_limit: 0,
            pair_jump: None,
            hl_stale: true,
            snapshot_requested: false,
            snapshot_epoch: None,
            syntax_tick_backoff: None,
            pending_layout: Vec::new(),
            edit_epoch: 1,
            measured_epoch: 0,
            measured_lines: std::collections::HashSet::new(),
            virtual_wrap: false,
            h_max_cells: 0,
            caret_px_cache: std::cell::Cell::new(None),
            paint_win: dg_render::pipeline::WindowAnchor::default(),
            color_band_cache: std::cell::RefCell::new(None),
            url_span_cache: std::cell::RefCell::new(None),
            large_hl: None,
            large_edits: 0,
            follow_tail: false,
        }
    }

    /// Loads decoded text (file content). Folding state and
    /// scroll position reset; layout must be run
    /// afterwards via [`Self::relayout`].
    pub fn open_text(&mut self, text: &str) {
        self.doc = Document::from_text(text);
        self.bump_edit_epoch();
        self.hl_stale = true;
        self.pending_layout.clear();
        self.syntax_version += 1;
        // 换文档就丢掉上一份的 query 错误，attach 时重新填。
        self.syntax_query_error = None;
        self.large_hl = None;
        self.large_edits = 0;
        self.follow_tail = false;
        self.sync_folding();
        self.caret = (0, 0);
        self.first_row = 0;
        self.h_scroll = 0;
        self.selection.clear();
        self.undo.clear();
        self.bookmarks.clear();
        self.tag_hl.clear();
        self.tag_hl_key = None;
        self.find_marks.clear();
    }

    /// The loader's landing point: the whole file as raw bytes — the
    /// store splits raw-domain and decodes per line on demand, so
    /// opening never materializes the decoded text, and the reset tail
    /// [`Self::open_text`].
    pub fn open_bytes(
        &mut self,
        raw: std::sync::Arc<Vec<u8>>,
        enc: &'static encoding_rs::Encoding,
        bom_len: usize,
    ) {
        self.doc = Document::from_bytes(raw, enc, bom_len);
        self.bump_edit_epoch();
        self.hl_stale = true;
        self.pending_layout.clear();
        self.syntax_version += 1;
        // 换文档就丢掉上一份的 query 错误，attach 时重新填。
        self.syntax_query_error = None;
        self.large_hl = None;
        self.large_edits = 0;
        self.follow_tail = false;
        self.sync_folding();
        self.caret = (0, 0);
        self.first_row = 0;
        self.h_scroll = 0;
        self.selection.clear();
        self.undo.clear();
        self.bookmarks.clear();
        self.tag_hl.clear();
        self.tag_hl_key = None;
        self.find_marks.clear();
    }

    /// 大文件模式的打开入口：接管一个 Large 后端的 [`Document`]。
    /// 与 [`Self::open_text`] 同一套复位，但 tree-sitter 与折叠/
    /// 大纲类全文计算全部禁用（Zed/EmEditor Fast View 同款降级）。
    pub fn open_large_doc(&mut self, doc: Document) {
        self.doc = doc;
        self.bump_edit_epoch();
        self.highlighter = None;
        self.hl_stale = true;
        self.pending_layout.clear();
        self.syntax_version += 1;
        // 换文档就丢掉上一份的 query 错误，attach 时重新填。
        self.syntax_query_error = None;
        self.large_hl = None;
        self.large_edits = 0;
        self.follow_tail = false;
        self.caret = (0, 0);
        self.first_row = 0;
        self.h_scroll = 0;
        self.selection.clear();
        self.undo.clear();
        self.bookmarks.clear();
        self.tag_hl.clear();
        self.tag_hl_key = None;
        self.folding = Folding::default();
        self.wrapped = false;
    }

    /// Re-establishes the fold-state arrays for the document's
    /// current line count. Every path that changes the line count
    /// owes this call.
    pub fn sync_folding(&mut self) {
        self.folding.ensure(self.doc.line_count());
    }

    /// Rewrites every terminator (Document menu → EOL). The next
    /// [`Self::sync_syntax`] does a full parse: every line's EOL
    /// bytes moved, so no single edit describes the change.
    pub fn set_eol_all(&mut self, eol: Eol) {
        self.doc.set_eol_all(eol);
        self.bump_edit_epoch();
        self.hl_stale = true;
        self.pending_layout.clear();
    }

    /// Attaches a tree-sitter highlighter for the
    /// current document text.
    pub fn attach_syntax(&mut self, lang: tree_sitter::Language) {
        self.attach_syntax_with(lang, None, None);
    }

    /// [`Self::attach_syntax`] plus the plugin package's query files
    /// (plan §6): `highlights` colours captures, `folds` supplies the
    /// fold regions. A query that fails to compile is ignored — the
    /// highlighter keeps node-kind colouring and the kind heuristic for
    /// folds, so a bad `.scm` degrades instead of blanking the file.
    pub fn attach_syntax_with(
        &mut self,
        lang: tree_sitter::Language,
        highlights: Option<&str>,
        folds: Option<&str>,
    ) {
        if self.doc.is_large() {
            // 大文件禁用 tree-sitter（树内存 >10× 源文本，GLR 解析
            // 仅数十 MB/s —— 大文件路径
            // 改由行式词法器承担）。
            return;
        }
        let mut h = Highlighter::new(lang);
        let mut qerr: Vec<String> = Vec::new();
        if let Some(scm) = highlights {
            // 编译失败 = 保留 kind 路径，不回滚整个语法；原因留给报告。
            if let Err(e) = h.set_query(scm) {
                qerr.push(format!("highlights.scm: {e}"));
            }
        }
        if let Some(scm) = folds {
            if let Err(e) = h.set_fold_query(scm) {
                qerr.push(format!("folds.scm: {e}"));
            }
        }
        self.syntax_query_error = if qerr.is_empty() {
            None
        } else {
            Some(qerr.join("; "))
        };
        {
            let doc = &self.doc;
            parse_from_doc(doc, &mut h);
        }
        self.highlighter = Some(h);
        self.hl_stale = false;
        self.snapshot_epoch = None;
        self.syntax_version += 1;
    }

    /// 大文档的语法挂载：先登记一个「等快照」的高亮器，不在界面线程上
    /// 建树。窗口随后把全文派给解析线程，结果由 [`Self::install_syntax`]
    /// 落地。树的大小是源文件的几十倍（1.4 MB 的源文件约 42 MB），大文档
    /// 常驻一棵树不值当。
    pub fn attach_syntax_snapshot(
        &mut self,
        lang: tree_sitter::Language,
        highlights: Option<&str>,
        folds: Option<&str>,
    ) {
        if self.doc.is_large() {
            return;
        }
        let mut h = Highlighter::new(lang);
        // 字节长度从挂载那一刻的真实值起步：挂载前就在历史里的编辑，撤销的
        // 反向记账也减不掉底数（见 [`Highlighter::defer_snapshot`]）。
        h.defer_snapshot(self.doc.total_bytes());
        // 脚本登记在高亮器上：局部/整篇重算派任务时直接取，不回磁盘重读。
        h.set_pending_queries(highlights, folds);
        // 界面线程的即时窗口重算（[`Self::syntax_window_tick`]）也要 query：
        // 挂载时编译一次常驻（毫秒级，不在击键路径上）。编译失败只退 kind
        // 配色，窗口重算退回工作线程，整篇任务照旧。
        let mut qerr: Vec<String> = Vec::new();
        if let Some(scm) = highlights {
            if let Err(e) = h.set_query(scm) {
                qerr.push(format!("highlights.scm: {e}"));
            }
        }
        self.highlighter = Some(h);
        self.syntax_query_error = if qerr.is_empty() {
            None
        } else {
            Some(qerr.join("; "))
        };
        self.hl_stale = false;
        self.snapshot_epoch = None;
        self.syntax_version += 1;
    }

    /// Installs a highlighter that was parsed elsewhere — the worker's answer
    /// for a file that is still exactly the bytes it parsed. The tree's byte
    /// offsets are the document's own, so nothing re-parses here.
    pub fn install_syntax(&mut self, h: Highlighter, query_error: Option<String>) {
        self.highlighter = Some(h);
        self.syntax_query_error = query_error;
        self.hl_stale = false;
        self.snapshot_requested = false;
        self.syntax_version += 1;
    }

    /// 快照模式下文本已经变了、等后台重算。窗口据此派解析任务，而不是
    /// 走界面线程上的增量解析。
    pub fn snapshot_stale(&self) -> bool {
        match self.highlighter.as_ref() {
            // 直接改正文的命令只立 `hl_stale`，没有走 `edit_at`；两条都算。
            Some(h) if h.is_snapshot_mode() => self.hl_stale || h.snapshot_stale(),
            _ => false,
        }
    }

    /// 区间表已经立起来了（整篇压完，或引导那一窗先落了地）：有底表可拼，
    /// 编辑点的即时重算才能在界面线程上补颜色。
    pub fn has_snapshot(&self) -> bool {
        self.highlighter
            .as_ref()
            .is_some_and(|h| h.is_snapshot_mode() && h.has_snapshot())
    }

    /// `[start, end)` 这段字节的颜色算出来了没有：整篇压完的表全覆盖，刚
    /// 立起来的表只覆盖那一窗。窗口层拿它判断「视野走到没算过的字节上了」。
    pub fn syntax_covers(&self, start: usize, end: usize) -> bool {
        self.highlighter
            .as_ref()
            .is_some_and(|h| h.is_snapshot_mode() && h.covers(start, end))
    }

    /// 取走一次「快照该重算了」的请求（[`Self::sync_syntax`] 的直接调用
    /// 留下的）。窗口在下一帧派工作线程。
    pub fn take_snapshot_request(&mut self) -> bool {
        std::mem::take(&mut self.snapshot_requested)
    }

    /// 工作线程只重解析了编辑附近的一段：并进快照，窗口外颜色不动。
    pub fn apply_syntax_window(&mut self, paint: dg_syntax::WindowPaint) -> bool {
        let applied = self
            .highlighter
            .as_mut()
            .is_some_and(|h| h.apply_window(paint));
        if applied {
            self.syntax_version += 1;
        }
        applied
    }

    /// 记下颜色已经跟到哪个文本纪元（局部或整篇重算落地时）。
    pub fn mark_syntax_synced(&mut self, epoch: u64) {
        self.snapshot_epoch = Some(epoch);
    }

    /// 界面线程的即时局部重算（快照模式的跟手颜色）：从最近的同类节点
    /// 起点到光标后 [`SYNTAX_UI_AHEAD_LINES`] 行，解析 ≤
    /// [`SYNTAX_UI_WINDOW_BYTES`] 的一小段，当场拼进快照。返回 true 表示
    /// 颜色已经跟上，本帧就能画出来；false 表示这趟不适合界面线程（窗口
    /// 超预算、快照没到、偏移已歪、正处退避期），调用方退回工作线程的
    /// 任务。树只在这一趟里活着，常驻内存不变。
    pub fn syntax_window_tick(&mut self) -> bool {
        if !self.syntax_window_due() || self.snapshot_offsets_lost() {
            return false;
        }
        if let Some(until) = self.syntax_tick_backoff {
            if std::time::Instant::now() < until {
                return false;
            }
            self.syntax_tick_backoff = None;
        }
        let Some(h) = self.highlighter.as_ref() else {
            return false;
        };
        // 快照还没落地（打开后的第一趟整篇还没回来）：没有底表可拼。
        if !h.has_snapshot() {
            return false;
        }
        let lines = self.doc.line_count();
        let (line, col) = self.caret;
        let line = line.min(lines.saturating_sub(1));
        let caret_byte = self.doc.line_byte_col(line, col);
        // 窗口起点从 dirty_from（最早动过的字节）往回收：退格、多光标都会
        // 让改动落在光标之前。
        let from = h
            .dirty_from()
            .map_or(caret_byte, |dirty| dirty.min(caret_byte));
        let Some(floor) = h.restart_floor(from) else {
            return false;
        };
        let start = floor.min(from);
        let last = (line + SYNTAX_UI_AHEAD_LINES).min(lines.saturating_sub(1));
        let end = if last + 1 < lines {
            self.doc.line_byte_offset(last + 1)
        } else {
            self.doc.total_bytes()
        };
        let (start, end) = (start.min(end), end.min(self.doc.total_bytes()));
        if end <= start || end - start > SYNTAX_UI_WINDOW_BYTES {
            return false;
        }
        let window = self.range_bytes(start, end);
        let node_text =
            |s: usize, e: usize| window[s.min(window.len())..e.min(window.len())].to_vec();
        let t0 = std::time::Instant::now();
        let ok = self
            .highlighter
            .as_mut()
            .is_some_and(|h| h.parse_window(start, &window, &node_text));
        // 一趟超过预算就退避：接下来一会儿不再逐帧重算，落回定时器节拍。
        if t0.elapsed().as_millis() >= SYNTAX_UI_BUDGET_MS {
            self.syntax_tick_backoff = Some(
                std::time::Instant::now() + std::time::Duration::from_millis(SYNTAX_UI_BACKOFF_MS),
            );
        }
        if !ok {
            return false;
        }
        self.syntax_version += 1;
        self.mark_syntax_synced(self.edit_epoch);
        true
    }

    /// 整篇字节域里 `[start, end)` 那一段的原始字节。快照模式（UTF-8：
    /// 解码文本的字节就是文档字节）的窗口重算用 —— 只取这一段，不整篇
    /// 物化。
    pub fn range_bytes(&self, start: usize, end: usize) -> Vec<u8> {
        let total = self.doc.total_bytes();
        let (start, end) = (start.min(end), end.min(total));
        let (sp, ep) = (self.byte_point(start), self.byte_point(end));
        self.doc
            .range_text(
                Pos {
                    line: sp.0,
                    col: sp.1,
                },
                Pos {
                    line: ep.0,
                    col: ep.1,
                },
            )
            .into_bytes()
    }

    /// 快照模式下还有没重算的改动：编辑纪元走在了已着色纪元前面。
    /// 快照自身的 `snapshot_stale` 只说明「整篇还没重算」，局部重算完
    /// 它仍然为真 —— 不能拿它当派任务的触发条件，否则定时器空转。
    pub fn syntax_window_due(&self) -> bool {
        self.highlighter
            .as_ref()
            .is_some_and(|h| h.is_snapshot_mode())
            && self.snapshot_epoch != Some(self.edit_epoch)
    }

    /// 快照的字节偏移已经和正文对不上：直接改正文的命令（EOL 重写、
    /// 全部替换）没有逐条走 [`Highlighter::edit_at`] 的平移。这种时候
    /// 只能整篇重算 —— 按区间补一段会把歪掉的偏移一起搬进来。
    pub fn snapshot_offsets_lost(&self) -> bool {
        self.hl_stale
            && self
                .highlighter
                .as_ref()
                .is_some_and(|h| h.is_snapshot_mode())
    }

    /// Lines that begin a collapsible block, derived from the folding method's
    /// regions. One entry per document line.
    pub fn folding_starts(&self) -> Vec<bool> {
        self.fold_regions_cached().starts.clone()
    }

    /// The leading indentation of every line in tab-stop columns, with blank and
    /// whitespace-only lines as `None` — indent folding lets them take part in no
    /// decision, so a trailing blank line stays inside the region above it.
    fn indentations(&self) -> Vec<Option<usize>> {
        let tab = self.tab_size.max(1) as usize;
        let mut buf = String::new();
        (0..self.doc.line_count())
            .map(|line| {
                buf.clear();
                self.doc.line_text(line, &mut buf);
                let mut col = 0usize;
                for ch in buf.chars() {
                    match ch {
                        ' ' => col += 1,
                        '\t' => col += tab - col % tab,
                        c if c.is_whitespace() => {}
                        _ => return Some(col),
                    }
                }
                None
            })
            .collect()
    }

    /// The collapsible regions as `( first line, last line)`, derived the way the
    /// active `FoldingMethod` says. 折叠当前级别 and 展开当前级别 act on the region that
    /// *contains* the caret rather than on the caret's own line, which is why the
    /// span ends are needed here while `folding_starts` only marks the openings.
    /// The grammar's spans arrive flattened per start line (`folding_spans` keeps
    /// the outermost node of each line), so a line owns at most one marker while a
    /// nested block still reports the region that starts on its own line.
    pub fn fold_regions(&self) -> Vec<(usize, usize)> {
        self.fold_regions_cached().regions.clone()
    }

    /// [`Self::fold_regions`] plus the per-line start flags, derived once per
    /// text version: the paint asks for both on every frame, and the indent
    /// method walks every line of the document to answer.
    fn fold_regions_cached(&self) -> std::cell::Ref<'_, FoldRegions> {
        {
            let c = self.fold_regions_cache.borrow();
            if let Some(e) = c.as_ref() {
                if e.epoch == self.edit_epoch
                    && e.syntax == self.syntax_version
                    && e.method == self.folding_method
                    && e.tab == self.tab_size
                {
                    return std::cell::Ref::map(c, |c| c.as_ref().unwrap());
                }
            }
        }
        let regions = self.derive_fold_regions();
        let mut starts = vec![false; self.doc.line_count()];
        for (line, _end) in &regions {
            if *line < starts.len() {
                starts[*line] = true;
            }
        }
        *self.fold_regions_cache.borrow_mut() = Some(FoldRegions {
            epoch: self.edit_epoch,
            syntax: self.syntax_version,
            method: self.folding_method,
            tab: self.tab_size,
            regions,
            starts,
        });
        std::cell::Ref::map(self.fold_regions_cache.borrow(), |c| c.as_ref().unwrap())
    }

    /// The region derivation itself — the uncached body of [`Self::fold_regions`].
    fn derive_fold_regions(&self) -> Vec<(usize, usize)> {
        if self.doc.is_large() {
            // 大文件禁用折叠：indent/syntax 折叠都要全文逐行扫描。
            return Vec::new();
        }
        match self.folding_method {
            FoldingMethod::Syntax => self.syntax_regions(),
            FoldingMethod::Indent => dg_core::folding::indent_regions(&self.indentations()),
            // Cancel Folding leaves the document with no regions, and the syntax
            // method, promoted to the tag-based one on the `anytext` lexer, has
            // no scanner in this implementation yet.
            FoldingMethod::None | FoldingMethod::Tag => Vec::new(),
        }
    }

    fn syntax_regions(&self) -> Vec<(usize, usize)> {
        let Some(h) = self.highlighter.as_ref() else {
            return Vec::new();
        };
        // 快照模式树已经丢掉，但折叠区在压表时整份取出来了；两者都没有
        // 才是真的「还没解析」。
        if h.tree().is_none() && !h.is_snapshot_mode() {
            return Vec::new();
        }
        // 树只在解析落地时动，而解析是防抖的 —— 每帧重走一遍整棵树不值当。
        if let Some((version, regions)) = self.fold_cache.borrow().as_ref() {
            if *version == self.syntax_version {
                return regions.clone();
            }
        }
        // folds.scm 的谓词可能要节点文本（与高亮同一套字节源）。
        let nt = |s: usize, e: usize| doc_slice(&self.doc, s, e);
        let mut out = Vec::new();
        for (s, e) in h.fold_spans(&nt) {
            let start = self.doc.byte_line(s);
            let end = self.doc.byte_line(e);
            if end > start {
                out.push((start, end));
            }
        }
        *self.fold_cache.borrow_mut() = Some((self.syntax_version, out.clone()));
        out
    }

    /// The rows' windows and the caret's pixel are measured against the current
    /// break arrays. A full layout rebuilds every one of them, so both go.
    fn invalidate_layout(&mut self) {
        self.paint_win.borrow_mut().clear();
        self.caret_px_cache.set(None);
    }

    /// Which row of a line the caret sits in, and the column that row's segment
    /// starts at — the pair [`Self::caret_row`] measures the pixel from. A
    /// re-wrap that leaves the pair where it was leaves that pixel honest.
    fn segment_at(data: &[i32], col: usize) -> (usize, usize) {
        let sub = data.partition_point(|&b| ((b.max(0)) as usize) < col);
        let start = if sub == 0 {
            0
        } else {
            data[sub - 1].max(0) as usize
        };
        (sub, start)
    }

    /// Full relayout: wraps every line
    /// and rebuilds the vertical row index. The slow
    /// path — load, resize, wrap-mode change, undo.
    pub fn relayout(&mut self, wrapper: &dyn Wrapper, ctx: &mut LayoutCtx) {
        // Every line's break array is about to be rebuilt, so nothing that was
        // measured against the old ones — a row's window, the caret's pixel
        // inside its segment — carries over.
        self.invalidate_layout();
        let n = self.doc.line_count();
        if self.doc.is_large() {
            // 大文件：不建包装数组（千万行 × 每行一个 Vec 分配不可
            // 接受），不换行不折叠 ⇒ 行==行，恒等索引 O(1) 直通。
            self.wrap.store.truncate(0);
            self.total_rows = n;
            self.rows = RowIndex::identity(n);
            return;
        }
        self.wrap.store.ensure(n);
        // A document swap can leave stale arrays past the new line
        // count; the row index and the scroll range read them.
        self.wrap.store.truncate(n);
        self.virtual_wrap = wrapper.is_virtual();
        if self.virtual_wrap {
            // 不换行: every line is one row, and no line is walked or measured —
            // the cost of a keystroke no longer follows the text. The cell count
            // the horizontal range reads is the line's own char count, which the
            // store answers without decoding it.
            for line in 0..n {
                let cells = self.doc.line_len(line) as i32;
                self.wrap.wrap_line_virtual(line, cells);
            }
            self.total_rows = n;
            self.rows.rebuild(&self.wrap.store, &self.folding);
            self.h_max_cells = self.caret_cells();
            return;
        }
        let mut total = 0usize;
        let mut buf = String::new();
        for line in 0..n {
            buf.clear();
            self.doc.line_text(line, &mut buf);
            total += self.wrap.wrap_line(line, &buf, wrapper, ctx);
        }
        self.total_rows = total;
        self.rows.rebuild(&self.wrap.store, &self.folding);
    }

    /// The caret's column in display cells — the horizontal range's exact lower
    /// bound, and what 不换行 resets it to when a layout changes.
    fn caret_cells(&self) -> i32 {
        let (_, px) = self.caret_row();
        let x = self.theme.x_width.max(1.0).round() as i32;
        (px / x).max(0)
    }

    /// Visual rows a line currently occupies (0 when folded).
    fn visual_rows_of(&self, line: usize) -> i64 {
        if self.doc.is_large() {
            return 1;
        }
        if self.folding.is_folded(line) {
            return 0;
        }
        match self.wrap.store.arrays.get(line) {
            Some(a) => a.count() as i64 + 1,
            None => 1,
        }
    }

    /// Incremental relayout for an edit that re-mapped document
    /// lines `[first, first + old_lines)` into
    /// `[first, first + new_lines)`. Only the lines in that region
    /// are re-wrapped; the row index is point-updated per line, or
    /// rebuilt when the line count changed (every later line's
    /// index shifts by the delta). This is the keystroke path's
    /// counterpart to [`Self::relayout`]: O(edited lines) plus, for
    /// structural edits, one O(n log n) index rebuild.
    pub fn relayout_region(
        &mut self,
        first: usize,
        old_lines: usize,
        new_lines: usize,
        wrapper: &dyn Wrapper,
        ctx: &mut LayoutCtx,
    ) {
        let n = self.doc.line_count();
        if self.doc.is_large() {
            // 大文件与 [`Self::relayout`] 同款恒等索引：无包装数组、
            // 无折叠 ⇒ 行==行，重建是 O(1) 的行数直读。
            self.invalidate_layout();
            self.wrap.store.truncate(0);
            self.total_rows = n;
            self.rows = RowIndex::identity(n);
            return;
        }
        let structural = new_lines != old_lines;
        if structural {
            if new_lines > old_lines {
                self.wrap
                    .store
                    .insert_lines(first + old_lines, new_lines - old_lines);
            } else {
                self.wrap
                    .store
                    .remove_lines(first + new_lines, old_lines - new_lines);
            }
            // Lines below the edit moved: an entry recorded for line `n` now
            // describes a different line, and so does a pixel measured inside a
            // segment whose row count changed.
            self.invalidate_layout();
        }
        self.wrap.store.ensure(n);
        // A fresh (or reloaded) index owes a full rebuild — the
        // point-update path needs the index to already cover every
        // line.
        let rebuild_index = structural || self.rows.len() != self.wrap.store.arrays.len();
        // The mode decides how a line is laid out; the flag only records which
        // one the arrays were last built for.
        self.virtual_wrap = wrapper.is_virtual();
        let mut delta: isize = 0;
        let mut buf = String::new();
        let region = first..(first + new_lines).min(n);
        // Watching one line costs a copy of its break array, so a region wider
        // than a screenful of lines drops the entries outright instead of
        // proving them stale one at a time.
        let track = !structural && region.len() <= LAYOUT_WATCH_MAX;
        if !structural && region.len() > LAYOUT_WATCH_MAX {
            self.invalidate_layout();
        }
        for line in region {
            let old_rows = self.visual_rows_of(line);
            // The rows whose cached positions came out of this line's current
            // break array: the re-wrap either leaves that array where it was —
            // a keystroke at the end of a long line, where everything keeps its
            // pixel — or moves the positions along with it.
            let tracked = track && (self.caret.0 == line || self.paint_win.borrow().has_line(line));
            let before = tracked.then(|| {
                self.wrap
                    .store
                    .arrays
                    .get(line)
                    .map_or_else(Vec::new, |a| a.data.clone())
            });
            let new_rows = if wrapper.is_virtual() {
                // 不换行: one row, no walk, and no copy of the line — the char
                // count the range reads comes straight from the store.
                self.wrap.wrap_line_virtual(line, self.doc.line_len(line) as i32) as i64
            } else {
                buf.clear();
                self.doc.line_text(line, &mut buf);
                self.wrap.wrap_line(line, &buf, wrapper, ctx) as i64
            };
            if let Some(before) = before {
                let after = self
                    .wrap
                    .store
                    .arrays
                    .get(line)
                    .map_or_else(Vec::new, |a| a.data.clone());
                if after != before {
                    self.paint_win.borrow_mut().drop_line(line);
                    if self.caret.0 == line
                        && Self::segment_at(&before, self.caret.1)
                            != Self::segment_at(&after, self.caret.1)
                    {
                        self.caret_px_cache.set(None);
                    }
                }
            }
            delta += new_rows as isize - old_rows as isize;
            if !rebuild_index {
                self.rows.set_line(line, old_rows, new_rows);
            }
        }
        if rebuild_index {
            self.rows.rebuild(&self.wrap.store, &self.folding);
            self.total_rows = self.rows.total_rows();
        } else {
            self.total_rows = (self.total_rows as isize + delta).max(0) as usize;
        }
    }

    /// The pending relayout regions (taken), ascending and non-overlapping: each is
    /// re-wrapped on its own. Empty means no cheap description — the view owes a
    /// full relayout.
    pub fn take_layout_regions(&mut self) -> Vec<(usize, usize, usize)> {
        std::mem::take(&mut self.pending_layout)
    }

    /// Widest visible line in display columns — the horizontal scroll
    /// range. Reads the wrap cache's per-line `cells`, so it is a pure
    /// integer scan (no measuring); folded lines are skipped because their
    /// text cannot be scrolled into view.
    pub fn max_cells(&self) -> i32 {
        if self.doc.is_large() {
            // 包装数组为空：用可视窗口附近的行长近似水平滚动范围
            // （制表符按 1 列计，v1 近似；caret 水平 reveal 用的是
            // 字形宽度表算出的像素，不受这里影响）。
            let n = self.doc.line_count();
            let from = self.first_row.saturating_sub(512);
            let to = (self.first_row + 512).min(n);
            return (from..to)
                .map(|l| self.doc.line_len(l) as i32)
                .max()
                .unwrap_or(0);
        }
        let scanned = self
            .wrap
            .store
            .arrays
            .iter()
            .enumerate()
            .filter(|(line, _)| !self.folding.is_folded(*line))
            .map(|(_, a)| a.cells)
            .max()
            .unwrap_or(0);
        if self.virtual_wrap {
            // 不换行: the per-line count is a char count, a lower bound on the
            // display columns. The caret's own offset is exact, so the range
            // never cuts off the column the view is about to reveal.
            return scanned.max(self.h_max_cells);
        }
        scanned
    }

    /// Re-wraps a single line (line count unchanged).
    pub fn relayout_line(&mut self, line: usize, wrapper: &dyn Wrapper, ctx: &mut LayoutCtx) {
        self.relayout_region(line, 1, 1, wrapper, ctx);
    }

    /// Re-syncs the syntax highlighter with the document text.
    /// Incremental while the mirror text is in lockstep with the
    /// buffer, so only the edited line and the lines after it re-lex
    /// until the resume state converges; a full parse when a load,
    /// undo/redo or EOL rewrite moved everything at once.
    pub fn sync_syntax(&mut self) {
        if self.doc.is_large() {
            self.hl_stale = false;
            return;
        }
        let Some(h) = self.highlighter.as_mut() else {
            self.syntax_version += 1;
            return;
        };
        if h.is_snapshot_mode() {
            // 快照模式不在界面线程上建树：整文件解析 + 压表要几百毫秒，
            // 由窗口的工作线程去做，结果回来时换整只高亮器。直接改正文
            // 的命令（全部替换、EOL 重写）把请求记下来，下一帧派出去。
            self.snapshot_requested = true;
            self.syntax_version += 1;
            return;
        }
        let doc = &self.doc;
        if self.hl_stale {
            parse_from_doc(doc, h);
            self.hl_stale = false;
        } else if h.needs_parse() {
            let mut feed = |off: usize| serve_chunk(doc, off);
            h.parse_feed(&mut feed);
        }
        self.syntax_version += 1;
    }

    /// True when the syntax state is behind the text: the next
    /// [`Self::sync_syntax`] would actually pay for a parse. The
    /// window layer debounces the parse off the keystroke path
    /// with this
    /// state instead of a whole-file one. Without a highlighter
    /// (plain text) nothing can be dirty.
    pub fn syntax_dirty(&self) -> bool {
        let Some(h) = self.highlighter.as_ref() else {
            return false;
        };
        if h.is_snapshot_mode() {
            // 直接改正文的命令只立 `hl_stale`，快照本身还没标脏。
            return self.hl_stale || h.snapshot_stale();
        }
        if self.hl_stale {
            return true;
        }
        h.needs_parse()
    }

    /// tree-sitter `Point` (row, byte column) of a flat byte offset —
    /// checkpoint arithmetic against the sparse byte index.
    fn byte_point(&self, byte: usize) -> (usize, usize) {
        let line = self.doc.byte_line(byte);
        (line, byte - self.doc.line_byte_offset(line))
    }

    /// Folds an op into the tree's node positions (cheap — it only
    /// touches intersecting nodes), so a paint that lands before the
    /// debounced parse still reads the right offsets; the parse
    /// itself is what the sync pays for, and it stays incremental.
    /// Runs BEFORE the edit lands (apply_edit calls it first), so the
    /// byte checkpoints are current and every query is arithmetic.
    fn highlighter_note_edit(&mut self, op: &EditOp) {
        if self.hl_stale || self.highlighter.is_none() {
            return;
        }
        let (start, _end, old_text, new_text) = match op {
            EditOp::Insert { pos, text } => (*pos, *pos, String::new(), text.clone()),
            EditOp::Delete { start, end, text } => (*start, *end, text.clone(), String::new()),
            EditOp::Replace {
                start,
                end,
                old_text,
                new_text,
            } => (*start, *end, old_text.clone(), new_text.clone()),
        };
        let start_byte = self.doc.line_byte_col(start.line, start.col);
        // The captured op texts are the exact pre-edit bytes of the
        // range, so the end offsets are start + text length — no
        // re-measure of the document.
        let old_end_byte = start_byte + old_text.len();
        let new_end_byte = start_byte + new_text.len();
        let start_point = point_of(self.byte_point(start_byte));
        let old_end_point = point_of(self.byte_point(old_end_byte));
        let new_end_point = point_of(self.byte_point(new_end_byte));
        if let Some(h) = self.highlighter.as_mut() {
            h.edit_at(
                start_byte,
                old_end_byte,
                new_end_byte,
                start_point,
                old_end_point,
                new_end_point,
            );
            h.fold_pending();
        }
    }

    /// The edit landed at `line`: the cached runs of the lines before it still
    /// describe what they were parsed from — the parse re-lexes from the edit
    /// point forward — so those are the ones that survive the new text version.
    fn keep_highlights_before(&mut self, line: usize) {
        let mut c = self.hl_run_cache.borrow_mut();
        match c.as_mut() {
            Some((sv, ev, map)) if *sv == self.syntax_version => {
                *ev = self.edit_epoch;
                map.retain(|&l, _| l < line);
            }
            _ => *c = None,
        }
    }

    /// Highlight runs for one line, in the
    /// `{color, len, style}` format the painter consumes.
    pub fn highlight_for(&self, line: usize) -> Option<Vec<(u32, u32, u8)>> {
        if self.grep_rows.is_some() {
            // Only `Found` has a background of its own — that one reaches the painter
            // as a mark — so the classes that change only the glyph colour are the
            // run list's job, and the rest of the row keeps `Default`.
            let spans = self.grep_classes(line);
            if spans.is_empty() {
                return None;
            }
            let n = self.doc.line_len(line) as usize;
            let plain = self.theme.foreground.to_u32();
            // 查找结果行没有语法树，字形位一律 0（`[Style] Found` 还没接）。
            let mut runs: Vec<(u32, u32, u8)> = Vec::new();
            let mut at = 0usize;
            for (s, e, class) in spans {
                if s > at {
                    runs.push((plain, (s - at) as u32, 0));
                }
                if e > s {
                    runs.push((self.class_fg(class), (e - s) as u32, 0));
                }
                at = e.max(at).min(n);
            }
            if n > at {
                runs.push((plain, (n - at) as u32, 0));
            }
            return Some(runs);
        }
        // 大文件：行式词法器（无 tree-sitter）。runs 走 LRU 缓存，
        // 滚动只对首见行付一次词法化。
        if self.doc.is_large() {
            return self.large_line_runs(line);
        }
        let h = self.highlighter.as_ref()?;
        // A wrapped row asks for its line once per visual row, and the query is
        // the expensive half of a row: one entry per (parse, text) version.
        {
            let mut c = self.hl_run_cache.borrow_mut();
            let stale = match c.as_ref() {
                Some((sv, ev, _)) => *sv != self.syntax_version || *ev != self.edit_epoch,
                None => true,
            };
            if stale {
                *c = Some((
                    self.syntax_version,
                    self.edit_epoch,
                    std::collections::HashMap::new(),
                ));
            }
            if let Some((_, _, map)) = c.as_ref() {
                if let Some(runs) = map.get(&line) {
                    return Some(runs.clone());
                }
            }
        }
        let byte_start = self.doc.line_byte_offset(line);
        let next = (line + 1).min(self.doc.line_count());
        let byte_end = self.doc.line_byte_offset(next);
        let mut text = String::new();
        self.doc.line_text(line, &mut text);
        // Runs lengths are chars: count the lead bytes (any byte that
        // is not a UTF-8 continuation) of the in-line slice.
        let cl = |s: usize, e: usize| -> u32 {
            let ls = (s - byte_start).min(text.len());
            let le = (e - byte_start).min(text.len());
            text.as_bytes()[ls..le]
                .iter()
                .filter(|b| (*b & 0xC0) != 0x80)
                .count() as u32
        };
        // Query predicates (`#match?` etc.) ask for one captured node's
        // bytes, which may span lines — serve them from the line store
        // rather than from this row's `text`.
        let nt = |s: usize, e: usize| doc_slice(&self.doc, s, e);
        let runs = h.highlight_range(byte_start, byte_end, &self.syntax_theme, &cl, &nt);
        let out: Vec<(u32, u32, u8)> = runs.iter().map(|r| (r.color, r.len, r.style)).collect();
        if let Some((_, _, map)) = self.hl_run_cache.borrow_mut().as_mut() {
            // A screenful of rows is a few hundred entries; the cap only guards a
            // document long enough to scroll through thousands of lines.
            if map.len() >= 4096 {
                map.clear();
            }
            map.insert(line, out.clone());
        }
        Some(out)
    }

    /// [`Self::highlight_for`] 的大文件臂：行式词法器 + LRU 缓存。
    fn large_line_runs(&self, line: usize) -> Option<Vec<(u32, u32, u8)>> {
        let hl = self.large_hl.as_ref()?;
        if let Some(runs) = hl.get(line) {
            return Some(runs);
        }
        let mut text = String::new();
        if !self.doc.line_text(line, &mut text) {
            return None;
        }
        let runs = hl.lexer.runs(line, &text, &hl.palette)?;
        hl.insert(line, runs.clone());
        Some(runs)
    }

    /// The theme pair the colour resolve answers for one attribute class.
    fn class_fg(&self, class: RowClass) -> u32 {
        let c = match class {
            // 30 has no key of its own, so it falls through the switch to the
            // `Default` pair — and the line painter skips it off the caret row.
            RowClass::Marker => self.theme.foreground,
            RowClass::Path => self.theme.url_fg,
            RowClass::Position => self.theme.word2_fg,
            RowClass::Found => self.theme.found_fg,
        };
        c.to_u32()
    }

    fn grep_classes(&self, line: usize) -> Vec<(usize, usize, RowClass)> {
        let Some(hl) = self.grep_rows.as_ref() else {
            return Vec::new();
        };
        let key = (line, self.syntax_version);
        if let Some(g) = self.grep_key.borrow().as_ref() {
            if g.0 == key.0 && g.1 == key.1 {
                return g.2.clone();
            }
        }
        let mut text = String::new();
        if !self.doc.line_text(line, &mut text) {
            return Vec::new();
        }
        let spans = dg_core::filesearch::style_row(&text, Some(hl));
        *self.grep_key.borrow_mut() = Some((key.0, key.1, spans.clone()));
        spans
    }

    /// The per-character attribute classes of one line. Only class 30 reaches the
    /// painter, and only a 查找结果 path row's `@~` carries it. The hit test asks the same
    /// question, because it has to invert the row as drawn.
    pub(crate) fn flags_for(&self, line: usize) -> Vec<CharClass> {
        if self.grep_rows.is_none() {
            return Vec::new();
        }
        let n = self.doc.line_len(line) as usize;
        let mut flags = vec![0u8; n];
        for (s, e, class) in self.grep_classes(line) {
            if class == RowClass::Marker {
                for f in flags[s.min(n)..e.min(n)].iter_mut() {
                    *f = dg_render::pipeline::CLASS_SUPPRESSED;
                }
            }
        }
        flags
    }

    /// Recomputes the marker span list when the text or the marker set moved. Caching it
    /// here keeps a repaint from re-scanning the document once per visible row.
    fn refresh_marks(&mut self) {
        let key = (self.syntax_version, self.markers.borrow().version());
        if self.mark_key == Some(key) {
            return;
        }
        self.mark_key = Some(key);
        self.mark_spans = self.markers.borrow().spans(&self.doc);
    }

    /// The marked spans of one line, with the slot's two theme colours resolved.
    fn marks_for(&self, line: usize) -> Vec<(usize, usize, dg_render::Color, dg_render::Color)> {
        let mut marks: Vec<_> = crate::markers::MarkSet::spans_for(&self.mark_spans, line)
            .into_iter()
            .map(|(s, e, slot)| {
                let slot = slot % dg_render::Theme::MARKER_SLOTS;
                (s, e, self.theme.marker_bg[slot], self.theme.marker_fg[slot])
            })
            .collect();
        // Attribute class 10 is the one class of a 查找结果 row with a background
        // of its own (`Found=#5C4A16,#FFECB3` in the dark theme), so it joins the
        // marks; the path and position classes are glyph colours only.
        if self.grep_rows.is_some() {
            marks.extend(self.grep_classes(line).into_iter().filter_map(|(s, e, class)| {
                (class == RowClass::Found).then(|| (s, e, self.theme.found_bg, self.theme.found_fg))
            }));
        }
        if !self.find_marks.is_empty() {
            marks.extend(
                self.find_marks
                    .iter()
                    .filter(|(l, _, _)| *l == line)
                    .map(|(_, s, e)| (*s, *e, self.theme.found_bg, self.theme.found_fg)),
            );
        }
        if marks.len() > 1 {
            marks.sort_by_key(|m| m.0);
        }
        marks
    }

    /// Recomputes the paired-tag highlight. The caret must sit on a
    /// tag-name character (ident-like, syntax class 5); range A is
    /// the tag under the caret, range B its nesting-aware counterpart
    /// (searched backward first, then forward).
    pub fn update_tag_highlight(&mut self) {
        // 标签配对靠 node kind 判「这一格是不是标签名」，没有高亮器就
        // 无从判起；大文件正是这种情况（[`Self::attach_syntax_with`]
        // 的 large 分支直接返回），而 `line_byte_col` 在大文件臂上是
        // panic 不是空答案 —— 点一下正文就会走进来，所以先退。
        if self.highlighter.is_none() || self.doc.is_large() {
            self.tag_hl.clear();
            self.tag_hl_key = None;
            return;
        }
        // 快照模式没有节点 kind 可查（树已丢掉）：标签配对不参与。
        if self.highlighter.as_ref().is_some_and(|h| h.is_snapshot_mode()) {
            self.tag_hl.clear();
            self.tag_hl_key = None;
            return;
        }
        let key = (self.caret.0, self.caret.1, self.syntax_version);
        if self.tag_hl_key == Some(key) {
            return;
        }
        self.tag_hl_key = Some(key);
        self.tag_hl.clear();
        let (line, col) = self.caret;
        let line_len = self.doc.line_len(line);
        // original scan cap: 500 chars per line
        if line_len == 0 || line_len > 500 {
            return;
        }
        // 1 <= col < line_len
        if col == 0 || col >= line_len {
            return;
        }
        let mut text = String::new();
        if !self.doc.line_text(line, &mut text) {
            return;
        }
        let chars: Vec<char> = text.chars().collect();
        if !is_tag_char(chars[col]) {
            return;
        }
        let kind_is_tag = |c: usize| -> bool {
            let byte = self.doc.line_byte_col(line, c);
            self.highlighter
                .as_ref()
                .and_then(|h| h.kind_at(byte))
                .map(|k| k.contains("tag"))
                .unwrap_or(false)
        };
        if !kind_is_tag(col) {
            return;
        }
        // expand the tag-name span [start, end)
        let mut start = col;
        while start > 0 && is_tag_char(chars[start - 1]) && kind_is_tag(start - 1) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < chars.len() && is_tag_char(chars[end]) && kind_is_tag(end) {
            end += 1;
        }
        if end <= start {
            return;
        }
        self.tag_hl.a = Some((line, start, end));
        let name: String = chars[start..end].iter().collect::<String>().to_lowercase();
        // backward first (the caret tag counts
        // as one close tag), then forward
        // (counts as one open tag)
        let matched = self
            .find_tag_match(line, start, &name, true)
            .or_else(|| self.find_tag_match(line, end, &name, false));
        if let Some((mline, mpos)) = matched {
            self.tag_hl.b = Some((mline, mpos, mpos + name.chars().count()));
        }
    }

    /// Scans for the counterpart of the tag
    /// ending/starting at `caret_pos` on
    /// `caret_line`. `backward` scans up to
    /// 200 lines above (open = 0, close = 1);
    /// forward scans to the end of the
    /// document (open = 1, close = 0). A name
    /// occurrence preceded by `<` counts as an
    /// open tag, otherwise as a close tag; the
    /// match is where the counts meet.
    fn find_tag_match(
        &self,
        caret_line: usize,
        caret_pos: usize,
        name: &str,
        backward: bool,
    ) -> Option<(usize, usize)> {
        if name.is_empty() {
            return None;
        }
        let line_count = self.doc.line_count();
        let name_len = name.chars().count();
        let mut open: u32 = if backward { 0 } else { 1 };
        let mut close: u32 = if backward { 1 } else { 0 };
        let line_window = if backward { 200 } else { usize::MAX };
        let mut line = caret_line;
        let mut lines_scanned = 0usize;
        loop {
            let mut text = String::new();
            if !self.doc.line_text(line, &mut text) {
                return None;
            }
            let lower = text.to_lowercase();
            let line_chars: Vec<char> = text.chars().collect();
            // occurrences on the caret line must
            // not overlap the caret's own tag
            let mut hits: Vec<usize> = lower
                .match_indices(name)
                .map(|(byte_pos, _)| lower[..byte_pos].chars().count())
                .filter(|&p| {
                    if line == caret_line {
                        if backward {
                            p + name_len <= caret_pos
                        } else {
                            p >= caret_pos
                        }
                    } else {
                        true
                    }
                })
                .collect();
            if backward {
                hits.reverse();
            }
            for p in hits {
                if p > 0 && line_chars[p - 1] == '<' {
                    open += 1;
                } else {
                    close += 1;
                }
                if open == close {
                    return Some((line, p));
                }
            }
            lines_scanned += 1;
            if lines_scanned >= line_window {
                return None;
            }
            if backward {
                if line == 0 {
                    return None;
                }
                line -= 1;
            } else {
                line += 1;
                if line >= line_count {
                    return None;
                }
            }
        }
    }

    /// Loads a theme/*.ini text into the
    /// editor theme and the syntax color
    /// table; `false` when the text is not
    /// a theme file (no `[Theme]` colors).
    pub fn load_theme_text(&mut self, text: &str) -> bool {
        let file = parse_theme_ini(text);
        self.load_theme(&file)
    }

    /// Applies an already-parsed theme file. This is the shape the shell uses:
    /// an external `themes/<name>.ini` is merged onto the bundled copy before it
    /// reaches here (`window.rs`'s `theme_file_for`), so the file that lands is
    /// one table rather than two passes.
    pub fn load_theme(&mut self, file: &ThemeFile) -> bool {
        if file.colors.is_empty() {
            return false;
        }
        self.theme.apply_theme_file(file);
        self.syntax_theme = syntax_config_from_theme(file);
        // The hex view's palette rides the same theme load
        // refills its 64-slot table from the theme and pushes it to the
        // HexView32 window on every apply.
        self.hex_colors = hex_colors_from_theme(file);
        // syntax colors are resolved at
        // paint time; the bump only drops
        // the cached tag highlight
        self.syntax_version += 1;
        self.tag_hl_key = None;
        true
    }

    /// Loads a theme/*.ini file by path.
    pub fn load_theme_file(&mut self, path: &str) -> bool {
        match std::fs::read_to_string(path) {
            Ok(text) => self.load_theme_text(&text),
            Err(_) => false,
        }
    }
    pub fn visible_rows(&self, height_rows: usize) -> std::ops::Range<usize> {
        self.rows.visible_range(self.first_row, height_rows)
    }

    /// Measure the characters of the rows about to be painted that the
    /// two-cell grid cannot place: each distinct one is asked of the platform
    /// once and, when its real advance differs from the grid cell it would
    /// otherwise get, recorded in the theme. The row layout, the caret, the hit
    /// test and the glyph shaper all read the same table, so a caret after a
    /// colour emoji lands on the glyph's own edge — the emoji's bitmap is wider
    /// than a CJK cell, and without this the caret was drawn inside it. Chars
    /// below U+0080 never leave the fixed-pitch editor face; a tab is a stop,
    /// not a measured cell; and a modifier takes no cell at all, so there is
    /// nothing about it to measure.
    pub fn measure_visible_advances(
        &mut self,
        rows: std::ops::Range<usize>,
        mut measure: impl FnMut(char) -> i32,
    ) {
        // A row is walked once per text version. The table only grows, so the
        // second frame of a scroll pays nothing — which matters most for the one
        // line a window can show sixty rows of.
        if self.measured_epoch != self.edit_epoch {
            self.measured_epoch = self.edit_epoch;
            self.measured_lines.clear();
        }
        let x = self.theme.x_width.max(1.0).round() as i32;
        let wide = self.theme.cjk_width.max(self.theme.x_width).round() as i32;
        for row in rows {
            let Some((line, seg)) = self.row_to_line_segment(row) else {
                continue;
            };
            let Some(chars) = self.doc.line_chars(line) else {
                continue;
            };
            // A line of single-cell characters has nothing to measure: the
            // walk below only ever asks about a character at or above 0x80.
            // Skipping it is what keeps a megabyte-long line of source from
            // being scanned once per keystroke.
            if self.doc.line_uniform(line) {
                continue;
            }
            // The columns this row can show. A wrapped row is one segment — a
            // screenful of characters at most. A 不换行 row is the whole line,
            // and there the memo bounds it to the window the last paint settled
            // on; a line with no memo (the first paint of a position) is
            // measured whole.
            let (seg_start, seg_end, wrapped) = match self.wrap.store.arrays.get(line) {
                Some(a) if !a.data.is_empty() => {
                    let s = if seg == 0 {
                        0
                    } else {
                        a.data.get(seg - 1).copied().unwrap_or(0).max(0) as usize
                    };
                    let e = a
                        .data
                        .get(seg)
                        .map_or(chars.len(), |&e| (e.max(0) as usize).min(chars.len()));
                    (s, e.max(s), true)
                }
                _ => (0, chars.len(), false),
            };
            let (from, to) = if wrapped {
                (seg_start, seg_end)
            } else {
                match self.paint_win.borrow().get(line, seg) {
                    Some(w) => (
                        w.col.min(chars.len()),
                        (w.col + MEASURE_AHEAD).min(chars.len()),
                    ),
                    None => (0, chars.len()),
                }
            };
            // The window is part of the key: a row scrolled to a new position
            // shows characters the last one did not, and those still have to be
            // measured. A window that has not moved is walked once.
            if !self.measured_lines.insert((line, seg, from)) {
                continue;
            }
            for &ch in &chars[from..to] {
                let cp = ch as u32;
                if cp < 0x80 || ch == '\t' || attaches_to_previous(ch) {
                    continue;
                }
                if self.theme.measured.binary_search_by_key(&ch, |&(c, _)| c).is_ok() {
                    continue;
                }
                let grid = if cp >= 0x2000 { wide } else { x };
                let real = measure(ch);
                if real != grid {
                    let at = self.theme.measured.partition_point(|&(c, _)| c < ch);
                    self.theme.measured.insert(at, (ch, real));
                }
            }
        }
    }

    pub fn row_to_line_segment(&self, row: usize) -> Option<(usize, usize)> {
        self.rows.row_to_line_segment(row)
    }

    /// 修改行标记: the document's marked lines lifted into the visual-row space
    /// the vertical bar's track stands for, so a mark sits at the rows its lines
    /// occupy whether or not anything wraps or folds.
    ///
    /// The runs come straight out of the line store, which carries them itself,
    /// so this costs one Fenwick walk per run and nothing per line.
    pub fn change_marks(&self) -> Vec<dg_ui::scrollbar::Mark> {
        let lines = self.doc.line_count();
        let mut out = Vec::new();
        for (first, last, state) in self.doc.state_runs() {
            let start = self.rows.line_to_visual_row(first as usize);
            let end = self.rows.line_to_visual_row((last as usize + 1).min(lines));
            // A run under a fold owns no rows at all; keep it on the row the
            // fold starts at rather than dropping the change off the bar.
            out.push(dg_ui::scrollbar::Mark {
                start: start as i32,
                end: end.max(start + 1) as i32,
                state,
            });
        }
        out
    }

    /// The columns a row can put on screen: a wrapped row's own segment, an
    /// unwrapped one the screenful the horizontal scroll exposes — doubled,
    /// because a wide glyph answers to two cells — with
    /// [`DECOR_SCAN_SLACK`] columns of overhang on each side. The decoration
    /// scans bound themselves to this range, so a row costs a screenful however
    /// long the line it sits in is.
    pub fn row_cols(&self, line: usize, seg: usize) -> (usize, usize) {
        let len = self.doc.line_len(line);
        let (from, to) = match self.wrap.store.arrays.get(line) {
            Some(a) if !a.data.is_empty() => {
                let f = if seg == 0 {
                    0
                } else {
                    a.data.get(seg - 1).copied().unwrap_or(0).max(0) as usize
                };
                let e = a
                    .data
                    .get(seg)
                    .map_or(len, |&b| (b.max(0)) as usize)
                    .min(len);
                (f.min(len), e.max(f.min(len)))
            }
            _ => {
                let x = self.theme.x_width.max(1.0) as i32;
                let cols = (self.text_w.max(x) / x * 2) as usize;
                let f = (self.h_scroll.max(0) as usize).min(len);
                (f, (f + cols).min(len))
            }
        };
        (
            from.saturating_sub(DECOR_SCAN_SLACK),
            (to + DECOR_SCAN_SLACK).min(len),
        )
    }

    /// One entry of a decoration scan's table: the cached answer for
    /// `(line, from, to)` under the current text version, else the freshly
    /// scanned one, kept for the next row that asks the same thing.
    fn decor_cache<T: Clone>(
        cache: &std::cell::RefCell<
            Option<(u64, std::collections::HashMap<(usize, usize, usize), Vec<T>>)>,
        >,
        epoch: u64,
        key: (usize, usize, usize),
        scan: impl FnOnce() -> Vec<T>,
    ) -> Vec<T> {
        if let Some(v) = cache
            .borrow()
            .as_ref()
            .filter(|(e, _)| *e == epoch)
            .and_then(|(_, m)| m.get(&key))
        {
            return v.clone();
        }
        let out = scan();
        let mut c = cache.borrow_mut();
        if c.as_ref().map_or(true, |(e, _)| *e != epoch) {
            *c = Some((epoch, std::collections::HashMap::new()));
        }
        let map = &mut c.as_mut().unwrap().1;
        if map.len() >= DECOR_CACHE_MAX {
            map.clear();
        }
        map.insert(key, out.clone());
        out
    }

    /// 颜色高亮 (cm_highlight_color, 1271): the color literals inside one line's
    /// `[from, to)` columns as `(start, end, color)` char ranges. The band under
    /// each literal takes the literal's own color, which is what the 3 px
    /// underline paints.
    fn color_literal_bands(
        &self,
        line: usize,
        from: usize,
        to: usize,
    ) -> Vec<(usize, usize, Color)> {
        if line >= self.doc.line_count() {
            return Vec::new();
        }
        let to = to.min(self.doc.line_len(line));
        if from >= to {
            return Vec::new();
        }
        Self::decor_cache(
            &self.color_band_cache,
            self.edit_epoch,
            (line, from, to),
            || {
                let Some(chars) = self.doc.line_chars(line) else {
                    return Vec::new();
                };
                let mut out = Vec::new();
                let mut i = from;
                while i < to {
                    match chars[i] {
                        '#' => {
                            let mut j = i + 1;
                            while j < to && chars[j].is_ascii_hexdigit() {
                                j += 1;
                            }
                            let hexlen = j - i - 1;
                            if matches!(hexlen, 3 | 6 | 8) {
                                let hex: String = chars[i + 1..j].iter().collect();
                                if let Some(c) = parse_hex_literal(&hex) {
                                    out.push((i, j, c));
                                }
                            }
                            i = j.max(i + 1);
                        }
                        'r' | 'R' => {
                            if let Some((end, col)) = parse_rgb_literal(&chars, i) {
                                out.push((i, end, col));
                                i = end;
                            } else {
                                i += 1;
                            }
                        }
                        _ => i += 1,
                    }
                }
                out
            },
        )
    }

    /// 高亮网址和邮件 (cm_highlight_urlemail, 1327): the URLs and email
    /// addresses inside one line's `[from, to)` columns as
    /// `(start, end, bg, fg)` char ranges, recolored to the theme's `Url` /
    /// `Email` keys. Alt+double-click opens the link.
    fn url_email_spans(
        &self,
        line: usize,
        from: usize,
        to: usize,
    ) -> Vec<(usize, usize, Color, Color)> {
        if line >= self.doc.line_count() {
            return Vec::new();
        }
        let to = to.min(self.doc.line_len(line));
        if from >= to {
            return Vec::new();
        }
        let (url_bg, url_fg) = (self.theme.url_bg, self.theme.url_fg);
        let (email_bg, email_fg) = (self.theme.email_bg, self.theme.email_fg);
        Self::decor_cache(
            &self.url_span_cache,
            self.edit_epoch,
            (line, from, to),
            || {
                let Some(chars) = self.doc.line_chars(line) else {
                    return Vec::new();
                };
                // The regexes match on byte offsets; the painter wants char
                // indices, so every boundary is mapped through the window's own
                // char table — a screenful of it, not a line's worth.
                let text: String = chars[from..to].iter().collect();
                let char_at: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
                let byte_to_char = |b: usize| from + char_at.partition_point(|&x| x < b);
                let mut out = Vec::new();
                for m in URL_RE.find_iter(&text) {
                    let (s, e) = (byte_to_char(m.start()), byte_to_char(m.end()));
                    if e > s {
                        out.push((s, e, url_bg, url_fg));
                    }
                }
                for m in EMAIL_RE.find_iter(&text) {
                    let (s, e) = (byte_to_char(m.start()), byte_to_char(m.end()));
                    if e > s {
                        out.push((s, e, email_bg, email_fg));
                    }
                }
                out.sort_by_key(|s| s.0);
                out
            },
        )
    }

    /// 选区高亮 (cm_highlight_sel, 1313): the single-line selection
    /// ranges of one line as `(start, end, color)` char ranges,
    /// painted as solid bands. A selection that spans lines is left
    /// to the ordinary selection blend.
    fn sel_highlight_bands(&self, line: usize) -> Vec<(usize, usize, Color)> {
        let band = Color::rgb(
            self.theme.selection_bg.r,
            self.theme.selection_bg.g,
            self.theme.selection_bg.b,
        );
        let mut out = Vec::new();
        for r in &self.selection.ranges {
            if r.start.0 == line && r.end.0 == line && r.end.1 > r.start.1 {
                out.push((r.start.1, r.end.1, band));
            }
        }
        out
    }

    /// Otherwise the text with the case flag and the word-edge result. This
    /// implementation's per-line scan is cheap enough to ride the paint.
    fn update_sel_highlight(&mut self) {
        let pat = self.sel_highlight_pat_of();
        match pat {
            Some(pat) => self.sel_highlight_pat = Some(pat),
            // 「移动光标时保持选区或者查找高亮」开着时，这一趟问不出模式不算换词：
            // 上一次选字留下的模式继续画，直到真的一次选择问出新模式。关掉（出厂
            // 值）就是塌掉即清。
            None if self.keep_highlight => {}
            None => self.sel_highlight_pat = None,
        }
    }

    /// 当前选区对应的「选中词全文高亮」模式，问不出来就是 `None`。
    fn sel_highlight_pat_of(&self) -> Option<SelHighlightPat> {
        // exactly one range, on one line — the shape a single
        // interval read answers
        if self.selection.ranges.len() != 1 {
            return None;
        }
        let r = &self.selection.ranges[0];
        if r.start.0 != r.end.0 {
            return None;
        }
        let len = r.end.1.saturating_sub(r.start.1);
        if len == 0 || len > self.sel_max_len {
            return None;
        }
        let chars = self.line_chars(r.start.0);
        let sel: String = chars
            .get(r.start.1..r.end.1)
            .map(|s| s.iter().collect())
            .unwrap_or_default();
        if sel.is_empty() || sel.chars().all(char::is_whitespace) {
            return None;
        }
        // the word-edge check (`Edit/QuickSmart`): both edges must
        // sit on a boundary — the selection at the line's edge, or
        // a non-word char beside it (the word-char test inverted) —
        // before the whole-word flag goes on.
        let whole_word = if self.sel_match_word {
            let left_ok = r.start.1 == 0
                || chars
                    .get(r.start.1 - 1)
                    .map_or(true, |c| !dg_core::textops::is_stat_word_char(*c));
            let right_ok = r.end.1 >= chars.len()
                || chars
                    .get(r.end.1)
                    .map_or(true, |c| !dg_core::textops::is_stat_word_char(*c));
            left_ok && right_ok
        } else {
            false
        };
        Some(SelHighlightPat {
            text: sel,
            case_sensitive: self.sel_match_case,
            whole_word,
        })
    }

    /// The occurrences of the selection-highlight pattern on one line as
    /// `(start, end, bg, fg)` char ranges in the `Found` theme colours: the
    /// walk of the selection-highlight list marks every match with
    /// attribute class 0xA, which the colour resolve's switch resolves
    /// through the theme's `Found` key. The selected range itself is left
    /// out.
    fn sel_highlight_occurrences(&self, line: usize) -> Vec<(usize, usize, Color, Color)> {
        let Some(pat) = &self.sel_highlight_pat else {
            return Vec::new();
        };
        let pat_chars: Vec<char> = pat.text.chars().collect();
        let plen = pat_chars.len();
        if plen == 0 {
            return Vec::new();
        }
        let mut text = String::new();
        if !self.doc.line_text(line, &mut text) {
            return Vec::new();
        }
        let chars: Vec<char> = text.chars().collect();
        if chars.len() < plen {
            return Vec::new();
        }
        // the selected range when it sits on this line — the one
        // interval the occurrence scan skips.
        let sel = self
            .selection
            .ranges
            .first()
            .filter(|r| r.start.0 == line && r.end.0 == line)
            .map(|r| (r.start.1, r.end.1));
        let eq = |a: char, b: char| {
            if pat.case_sensitive {
                a == b
            } else {
                a.to_lowercase().eq(b.to_lowercase())
            }
        };
        let (bg, fg) = (self.theme.found_bg, self.theme.found_fg);
        let mut out = Vec::new();
        let mut i = 0;
        while i + plen <= chars.len() {
            if (0..plen).all(|k| eq(chars[i + k], pat_chars[k])) {
                let word_ok = !pat.whole_word
                    || ((i == 0
                        || chars
                            .get(i - 1)
                            .map_or(true, |c| !dg_core::textops::is_stat_word_char(*c)))
                        && (i + plen >= chars.len()
                            || chars
                                .get(i + plen)
                                .map_or(true, |c| !dg_core::textops::is_stat_word_char(*c))));
                let in_sel = sel.map_or(false, |(s, e)| i >= s && i + plen <= e);
                if word_ok && !in_sel {
                    out.push((i, i + plen, bg, fg));
                }
                i += plen;
            } else {
                i += 1;
            }
        }
        out
    }

    /// The 括弧高亮 (1326) and 高亮网址和邮件 (1327) spans
    /// of one line, merged: `(start, end, bg, fg)` char
    /// ranges. `bracket_pair` is the caret's bracket and
    /// its partner, computed once per paint; each end that
    /// lands on `line` becomes a one-char range in the
    /// `Pair` theme colours (`pair_bg` + the `tag_fg` the
    /// implementation already reads from the same key).
    fn recolor_spans(
        &self,
        line: usize,
        from: usize,
        to: usize,
        highlights: HighlightToggles,
        bracket_pair: Option<((usize, usize), (usize, usize))>,
    ) -> Vec<(usize, usize, Color, Color)> {
        let mut out = Vec::new();
        if highlights.url {
            out.extend(self.url_email_spans(line, from, to));
        }
        if let Some((at, partner)) = bracket_pair {
            let (bg, fg) = (self.theme.pair_bg, self.theme.tag_fg);
            if line == at.0 {
                out.push((at.1, at.1 + 1, bg, fg));
            }
            if line == partner.0 {
                out.push((partner.1, partner.1 + 1, bg, fg));
            }
        }
        out.sort_by_key(|s| s.0);
        out
    }

    pub fn paint(
        &mut self,
        height_rows: usize,
        clip: dg_render::component::Rect,
        // Right edge of the caret row's band, in view pixels: the text column's
        // end while the lines wrap, the scrollbar strip's left edge while they
        // do not. The row's background still spans `clip`.
        band_right: f32,
        draw: &mut DrawList,
        top_offset: f32,
        highlights: HighlightToggles,
    ) {
        // paired-tag highlight is refreshed
        // on caret moves, and paint always
        // follows one, so the visible result
        // matches.
        self.update_tag_highlight();
        self.refresh_marks();
        // 选区高亮: the occurrence pattern rides the paint, the
        // same lazy computation the paired-tag highlight uses.
        self.update_sel_highlight();
        let rows = self.visible_rows(height_rows);
        // 同层背景色 asks for the caret line's indent level once per paint; every
        // row is compared against it, so it cannot be per-row work.
        let caret_level = if self.view_flags.get(ViewFlags::LEVEL_BG) {
            dg_render::pipeline::indent_level(&self.line_chars(self.caret.0), self.tab_size as usize)
        } else {
            0
        };
        // 符号显示: the marks the row pass draws. A row's own bit says whether the
        // mark shows at all; 选择时全部显示 widens that to the selection.
        let chars_opt = dg_render::pipeline::InvisibleChars {
            whitespace: self.view_flags.get(ViewFlags::WHITESPACE),
            control: self.view_flags.get(ViewFlags::CONTROL_CHAR),
            eol: self.view_flags.get(ViewFlags::END_OF_LINE),
            eof: self.view_flags.get(ViewFlags::END_OF_FILE),
            in_sel: self.view_flags.get(ViewFlags::INVISIBLE_CHAR_IN_SEL),
            space_style: self.space_style,
            tab_style: self.tab_style,
            indent: self.view_flags.get(ViewFlags::INDENT_LINE),
            indent_style: self.indent_style,
            level_bg: self.view_flags.get(ViewFlags::LEVEL_BG),
            caret_level,
        };
        let doc = &self.doc;
        let store = &self.wrap.store;
        let sel = &self.selection;
        let theme = &self.theme;
        // the primary caret is the system caret and is not in the draw list;
        // only the multi-caret strips are painted here. `caret` still picks
        // this row's active-line band.
        // 括弧高亮 (cm_highlight_brace, 1326): the caret's
        // bracket and its partner, computed once per paint —
        // the partner scan walks up to 2000 lines, so it must
        // not run for every visible row.
        let bracket_pair = if self.view_flags.get(ViewFlags::BRACKET_HIGHLIGHT) {
            self.caret_bracket().and_then(|(at, (idx, opens))| {
                self.bracket_partner(at, idx, opens)
                    .map(|partner| (at, partner))
            })
        } else {
            None
        };
        let row_to = |row: usize| self.row_to_line_segment(row);
        let hl_for = |line: usize| self.highlight_for(line);
        let flags_for = |line: usize| self.flags_for(line);
        // The three frame-global families (1271 / 1313 /
        // 1327) and the bracket pair (1326) reach the painter
        // through the same per-line closures the markers use,
        // so a family that is off costs one empty Vec per
        // visible row and nothing else.
        let mk_for = |line: usize, seg: usize| {
            // The URL family scans the columns this row shows, not the line.
            let (from, to) = self.row_cols(line, seg);
            let mut marks = self.marks_for(line);
            let extra = self.recolor_spans(line, from, to, highlights, bracket_pair);
            if !extra.is_empty() {
                marks.extend(extra);
            }
            // 选区高亮 occurrences join the marks: class 0xA
            // paints bg and fg exactly like the markers do,
            // and the paint order puts them over the selection
            // blend but under the text runs' own recolour.
            if highlights.sel {
                let occ = self.sel_highlight_occurrences(line);
                if !occ.is_empty() {
                    marks.extend(occ);
                }
            }
            if marks.len() > 1 {
                marks.sort_by_key(|s| s.0);
            }
            marks
        };
        let cb_for = |line: usize, seg: usize| {
            if highlights.color {
                let (from, to) = self.row_cols(line, seg);
                self.color_literal_bands(line, from, to)
            } else {
                Vec::new()
            }
        };
        let selb_for = |line: usize| {
            if highlights.sel {
                self.sel_highlight_bands(line)
            } else {
                Vec::new()
            }
        };
        // 折叠尾部: the indicator box shows on every folded
        // opener (`is_folded(line + 1)`), and the preview text —
        // 折叠尾部字符 (FoldingTail, view option 9) — follows it
        // when the option is on: the collapsed run's last line,
        // leading blanks skipped, capped at 128 characters
        // (the update step).
        let fold_tail_on = self.view_flags.get(ViewFlags::FOLDING_TAIL);
        let fold_end_for = |line: usize| self.folding.is_folded(line + 1);
        let fold_tail_for = |line: usize| {
            if !fold_tail_on || !self.folding.is_folded(line + 1) {
                return None;
            }
            let lines = self.doc.line_count();
            let mut l = line + 1;
            while l < lines && self.folding.is_folded(l) {
                l += 1;
            }
            let mut text = String::new();
            if !self.doc.line_text(l - 1, &mut text) {
                return None;
            }
            let tail: String = text.trim_start().chars().take(128).collect();
            if tail.is_empty() { None } else { Some(tail) }
        };
        let mut no_components: Vec<Box<dyn dg_render::component::RenderComponent>> = Vec::new();
        PaintPipeline::paint_visible(
            doc,
            &rows,
            row_to,
            store,
            hl_for,
            mk_for,
            flags_for,
            cb_for,
            selb_for,
            fold_end_for,
            fold_tail_for,
            Some(&self.tag_hl),
            sel,
            theme,
            self.tab_size as usize,
            self.view_flags,
            chars_opt,
            clip,
            band_right,
            top_offset,
            self.h_scroll,
            self.caret,
            &self.extra_carets,
            &mut no_components,
            &self.paint_win,
            draw,
        );
    }

    /// Paints the ruler and minimap over the
    /// visible area, the decoration passes
    /// of OnPaint (ruler, minimap).
    ///
    /// The minimap is drawn while the document's `MINIMAP` bit
    /// (`cm_view_minimap`, our `[Edit]/MiniMap`, `N` in this repository's shipped default) is set.
    /// It costs the rows the strip shows, not the document's: see
    /// [`Self::rebuild_minimap`].
    ///
    /// `ruler_h` is the dock layout's ruler
    /// strip height (`0` when the ruler is
    /// off), so the decorations line up with
    /// the text area the caller clipped to.
    pub fn paint_decorations(
        &mut self,
        clip: dg_render::component::Rect,
        visible_rows: usize,
        ruler_h: f32,
        draw: &mut DrawList,
    ) {
        self.paint_ruler(clip, ruler_h, draw);
        // 列标记 (`ColMarker`, 1337): one full-height vertical line per marked
        // column, down the text area only — the ruler keeps its own ticks. The
        // stroke takes the invisible-character colour, the same pen the marks
        // are drawn with.
        if self.view_flags.get(ViewFlags::COL_MARKER) && !self.col_markers.is_empty() {
            let xw = self.theme.x_width;
            let top = clip.y + ruler_h;
            let h = (clip.h - ruler_h).max(0.0);
            for &col in &self.col_markers {
                let x = self.theme.gutter_x + (col as f32 - self.h_scroll as f32) * xw;
                if x >= clip.x && x < clip.x + clip.w && h > 0.0 {
                    draw.fill(
                        dg_render::component::Rect::new(x, top, 1.0, h),
                        self.theme.whitespace_fg,
                    );
                }
            }
        }
        if !self.view_flags.get(ViewFlags::MINIMAP) {
            return;
        }
        // The strip sits beside the text, under the ruler: it takes its own
        // column out of the text area rather than floating over it.
        self.paint_minimap(clip, ruler_h, visible_rows, draw);
    }

    /// Ruler strip at the top: one row
    /// high, Ruler bg band, column ticks
    /// and decimal numbers every 10
    /// columns, 1 px separator at the
    /// bottom (ruler paint).
    fn paint_ruler(&mut self, clip: dg_render::component::Rect, ruler_h: f32, draw: &mut DrawList) {
        // The dock layout's strip height: one
        // text row plus 2 px when the ruler is
        // shown, 0 when the `Ruler` setting is
        // off (the view update).
        if ruler_h <= 0.0 || clip.w <= 0.0 {
            return;
        }
        // Band across the full width in
        // the Ruler bg (ETO_OPAQUE over
        // the whole client rect).
        draw.fill(
            dg_render::component::Rect::new(clip.x, clip.y, clip.w, ruler_h),
            self.theme.margin_bg,
        );
        let char_w = self.theme.x_width;
        if char_w <= 0.0 {
            self.ruler.tab_px = 1.0;
            self.ruler.width = clip.w;
            return;
        }
        // Ticks, column numbers and the
        // bottom separator share the
        // Ruler fg color.
        let fg = self.theme.ruler_fg;
        let bottom = clip.y + ruler_h;
        // 1 px separator just above the
        // text area (MoveToEx/LineTo at
        // bottom - 1).
        draw.fill(
            dg_render::component::Rect::new(clip.x, bottom - 1.0, clip.w, 1.0),
            fg,
        );
        // Tick ends: long (every 10
        // columns) at bottom / 2, medium
        // (every 5) at bottom - bottom/3
        // - 2, short at bottom - bottom/3.
        let long_end = clip.y + ruler_h / 2.0;
        let short_end = clip.y + ruler_h - ruler_h / 3.0;
        // Column numbers sit at
        // bottom / 10 - 2, centered on
        // their tick.
        let digit_y = clip.y + ruler_h / 10.0 - 2.0;
        // The first visible column lands
        // on the text-area left edge (the
        // view layout copies the text
        // rect's left into the ruler's
        // start x, the view update).
        let right = clip.x + clip.w;
        let mut col = self.h_scroll.max(0);
        let mut x = clip.x + self.theme.gutter_x;
        loop {
            let top = if col % 10 == 0 {
                long_end
            } else if col % 5 == 0 {
                short_end - 2.0
            } else {
                short_end
            };
            // 1 px tick hanging from the
            // bottom separator.
            draw.fill(
                dg_render::component::Rect::new(x, top, 1.0, (bottom - 1.0 - top).max(0.0)),
                fg,
            );
            if col % 10 == 0 && col > 0 {
                // Centered decimal column number, with the leading 0
                // skipped
                let s = col.to_string();
                let w = s.len() as f32 * char_w;
                draw.text(x - w / 2.0, digit_y, &s, fg);
            }
            x += char_w;
            col += 1;
            if x > right {
                break;
            }
        }
        self.ruler.tab_px = char_w;
        self.ruler.width = clip.w;
    }

    /// Minimap at the right edge: the strip shows a window of the document at
    /// one pixel per column and two per visual row, with the viewport's box
    /// over it.
    fn paint_minimap(
        &mut self,
        clip: dg_render::component::Rect,
        ruler_h: f32,
        visible_rows: usize,
        draw: &mut DrawList,
    ) {
        let strip = self.minimap_strip(clip.w, clip.h, ruler_h);
        // The paint is the authority on where the strip is: the hit test reads
        // this back, so a press can never land on a strip the paint did not
        // draw.
        self.map_strip = strip;
        if strip.is_empty() || self.total_rows == 0 {
            return;
        }
        // The strip is opaque, so the rows that run under it stop at its
        // edge — the text area was narrowed by the same width, and a long
        // unwrapped line would otherwise show through the map.
        draw.fill(
            dg_render::component::Rect::new(strip.x, strip.y, strip.w, strip.h),
            self.theme.background,
        );
        // The strip shows a window of the document rather than the document,
        // so lay that window out first and then build only the rows it
        // covers — plus a screen of slack, which is what makes a wheel notch
        // or a drag free.
        self.minimap
            .layout(self.total_rows, self.first_row, visible_rows, strip.h);
        let first = self.minimap.top;
        let last = (first + (strip.h / dg_render::minimap::ROW_PX).ceil() as usize + 1)
            .min(self.total_rows - 1);
        if self.total_rows != self.minimap_rows
            || self.syntax_version != self.minimap_version
            || !self.minimap.covers(first, last)
        {
            self.rebuild_minimap(first, last);
        }
        // The viewport's box goes down first: it is the backdrop the rows
        // inside it are drawn over, so the text under the box stays the
        // brightest thing in the map instead of being tinted by it.
        // 动态隐藏 draws it only while the pointer is on the strip; 背景色
        // always fills it; 边框 outlines it instead of filling.
        let box_y = strip.y + self.minimap.viewport.y;
        let box_h = self
            .minimap
            .viewport
            .h
            .min(strip.y + strip.h - box_y)
            .max(2.0);
        let rect = dg_render::component::Rect::new(strip.x, box_y, strip.w, box_h);
        let edge = dg_render::minimap::box_edge(self.theme.foreground, self.theme.background);
        match self.minimap_style {
            // 边框: the edge alone, so the rows under it keep the strip's own
            // background.
            3 => Self::outline_box(draw, rect, edge),
            style => {
                if style == 2 || self.map_hover {
                    let fill =
                        dg_render::minimap::box_fill(self.theme.foreground, self.theme.background);
                    draw.fill(rect, fill);
                    Self::outline_box(draw, rect, edge);
                }
            }
        }
        // Only the rows the strip shows: the rest of the document is not
        // painted at all, which is what keeps a long file's map cheap.
        let from = self.minimap.strips.partition_point(|s| (s.row as usize) < first);
        let bg = self.theme.background;
        let inset = dg_render::minimap::LEFT_INSET;
        let run_w = (strip.w - inset).max(1.0);
        for s in &self.minimap.strips[from..] {
            let row = s.row as usize;
            if row > last {
                break;
            }
            let y = strip.y + (row - first) as f32 * dg_render::minimap::ROW_PX;
            draw.fill(
                dg_render::component::Rect::new(
                    strip.x + inset,
                    y,
                    (s.cols as f32).min(run_w).max(1.0),
                    dg_render::minimap::ROW_PX,
                ),
                dg_render::minimap::dim(s.color, bg),
            );
        }
    }

    /// Builds the strip's runs for the rows `first..=last`, widened by a
    /// screen on either side so the next few scrolls reuse them.
    ///
    /// This is what makes a large file's map affordable. The rows come out of
    /// **one** syntax query over the window ([`Highlighter::segments`]) cut
    /// per line, instead of one query per line: every `highlight_range` walks
    /// the query cursor over the tree from the top, so asking 24 766 times
    /// cost the whole file 24 766 times — nine seconds on `window.rs`.
    fn rebuild_minimap(&mut self, first: usize, last: usize) {
        let span = last + 1 - first;
        let win_first = first.saturating_sub(span);
        let win_last = (last + span).min(self.total_rows.saturating_sub(1));
        // `None` means the per-line painter path is already cheap enough:
        // large documents lex one line at a time, 查找结果 rows restyle one
        // line at a time, and no tree at all means plain text.
        let segs = self.map_window_segments(win_first, win_last);
        let mut row_runs: Vec<Vec<(u32, u32)>> = Vec::with_capacity(win_last + 1 - win_first);
        // Wrapped rows repeat their line: decode and colour it once.
        let mut cache: Option<(usize, Vec<(u32, u32)>)> = None;
        for row in win_first..=win_last {
            let runs = match self.row_to_line_segment(row) {
                Some((line, _)) => match &cache {
                    Some((cached, runs)) if *cached == line => runs.clone(),
                    _ => {
                        let runs = self.map_line_runs(line, segs.as_deref());
                        cache = Some((line, runs.clone()));
                        runs
                    }
                },
                None => Vec::new(),
            };
            row_runs.push(runs);
        }
        self.minimap.rebuild(win_first, row_runs);
        self.minimap_rows = self.total_rows;
        self.minimap_version = self.syntax_version;
    }

    /// The coloured byte spans covering the rows `first..=last`, from one
    /// query. `None` when there is nothing to batch — the caller then asks
    /// per line, which for those paths is a single line's work anyway.
    fn map_window_segments(&self, first: usize, last: usize) -> Option<Vec<(usize, usize, u32, u8)>> {
        if self.doc.is_large() || self.grep_rows.is_some() {
            return None;
        }
        let h = self.highlighter.as_ref()?;
        // An unparsed tree answers nothing; the rows stay blank until the
        // parse lands and bumps the version. 快照没有树，但区间表就是解析
        // 结果。
        if h.tree().is_none() && !h.is_snapshot_mode() {
            return None;
        }
        let (line_first, _) = self.row_to_line_segment(first)?;
        let (line_last, _) = self.row_to_line_segment(last)?;
        let start = self.doc.line_byte_offset(line_first);
        let end = self.doc.line_byte_offset(line_last + 1);
        let nt = |s: usize, e: usize| doc_slice(&self.doc, s, e);
        Some(h.segments(start, end, &self.syntax_theme, &nt))
    }

    /// The map's runs for one line: cut out of the window's segments when
    /// there are any, otherwise the per-line painter path's answer.
    fn map_line_runs(
        &self,
        line: usize,
        segs: Option<&[(usize, usize, u32, u8)]>,
    ) -> Vec<(u32, u32)> {
        let Some(segs) = segs else {
            return match self.highlight_for(line) {
                // 地图只上色不排字，字形位在这里没有用处。
                Some(runs) => runs.into_iter().map(|(c, l, _)| (c, l)).collect(),
                None => self.map_plain_runs(line),
            };
        };
        let start = self.doc.line_byte_offset(line);
        let mut text = String::new();
        if !self.doc.line_text(line, &mut text) || text.is_empty() {
            return Vec::new();
        }
        // The line's own text, without its terminator — the same span
        // `highlight_for` asks the tree for, so the colours agree.
        let end = start + text.len();
        // Only the segments that reach into this line take part; a block
        // comment's middle line has one that started long before it.
        let from = segs.partition_point(|&(_, e, _, _)| e <= start);
        let to = from + segs[from..].partition_point(|&(s, _, _, _)| s < end);
        // Runs are chars: count the lead bytes (any byte that is not a UTF-8
        // continuation) of the in-line slice.
        let cl = |s: usize, e: usize| -> u32 {
            let ls = s.saturating_sub(start).min(text.len());
            let le = e.saturating_sub(start).min(text.len());
            text.as_bytes()[ls..le]
                .iter()
                .filter(|b| (*b & 0xC0) != 0x80)
                .count() as u32
        };
        let doc_len = self.highlighter.as_ref().map_or(0, |h| h.doc_len());
        dg_syntax::runs_in_range(
            &segs[from..to],
            start,
            end,
            doc_len,
            self.syntax_theme.default_color,
            &cl,
        )
        .iter()
        .map(|r| (r.color, r.len))
        .collect()
    }

    /// A row's mark when there is no syntax colour to give it: one run of the
    /// line's own length in the text colour. A large document without a line
    /// lexer is left out instead — measuring a million lines for a strip
    /// nobody can read is not worth a paint.
    fn map_plain_runs(&self, line: usize) -> Vec<(u32, u32)> {
        if self.doc.is_large() {
            return Vec::new();
        }
        let n = self.doc.line_len(line);
        if n == 0 {
            return Vec::new();
        }
        vec![(
            self.theme.foreground.to_u32(),
            n.min(u32::MAX as usize) as u32,
        )]
    }

    /// The map's strip in the editor's own space: `(x, y, w, h)`. Empty when
    /// the option is off or the text area leaves no room beside the
    /// scrollbar's strip.
    ///
    /// The width is four fifths of the columns that fit beside the
    /// scrollbar — a line of ordinary length reaches most of the way across
    /// the strip — clamped to 50..160 px.
    pub fn minimap_strip(&self, cw: f32, ch: f32, ruler_h: f32) -> dg_render::component::Rect {
        let empty = dg_render::component::Rect::new(0.0, 0.0, 0.0, 0.0);
        if !self.view_flags.get(ViewFlags::MINIMAP) {
            return empty;
        }
        let avail = (cw - self.theme.gutter_x - self.map_scroll_w).max(0.0);
        if avail <= 0.0 || ch <= ruler_h {
            return empty;
        }
        let w = dg_render::minimap::width(avail, self.theme.x_width);
        dg_render::component::Rect::new(
            cw - self.map_scroll_w - w,
            ruler_h,
            w,
            ch - ruler_h,
        )
    }

    /// The width the map's column takes out of a `cw` x `ch` text area whose
    /// scrollbar overlay is `scroll_w` thick, with the overlay width kept for
    /// the paint and the press. Zero when the option is off.
    ///
    /// The strip it lays out is kept: [`Editor::map_strip`] is what the
    /// pointer's hit test reads, so a move over the text area costs no
    /// window query.
    pub fn map_column(&mut self, cw: f32, ch: f32, ruler_h: f32, scroll_w: f32) -> f32 {
        self.map_scroll_w = scroll_w;
        let strip = self.minimap_strip(cw, ch, ruler_h);
        self.map_strip = strip;
        strip.w
    }

    /// Lays the strip's window out for the current first row and answers the
    /// viewport's box in editor space. The press, the drag and the paint all
    /// read the box through here, so a click cannot land on a box that the
    /// next paint would draw somewhere else.
    pub fn minimap_box(
        &mut self,
        strip: dg_render::component::Rect,
        visible_rows: usize,
    ) -> dg_render::component::Rect {
        self.minimap
            .layout(self.total_rows, self.first_row, visible_rows, strip.h);
        dg_render::component::Rect::new(
            strip.x,
            strip.y + self.minimap.viewport.y,
            strip.w,
            self.minimap.viewport.h,
        )
    }

    /// The one pixel frame the map's viewport box wears on every side. It is
    /// what makes the box read as a band rather than as a patch of slightly
    /// wrong background, and it is the whole box under 边框.
    fn outline_box(
        draw: &mut DrawList,
        rect: dg_render::component::Rect,
        color: dg_render::theme::Color,
    ) {
        let r = dg_render::component::Rect::new;
        draw.fill(r(rect.x, rect.y, rect.w, 1.0), color);
        draw.fill(r(rect.x, rect.y + rect.h - 1.0, rect.w, 1.0), color);
        draw.fill(r(rect.x, rect.y, 1.0, rect.h), color);
        draw.fill(r(rect.x + rect.w - 1.0, rect.y, 1.0, rect.h), color);
    }

    /// 光标落点之后的揭示。翻页先按 `Edit/ScrollLimit` 把光标停在离窗口边
    /// `limit` 行的地方，再走普通的揭示：留边小于一整屏，所以那一步在竖直方向
    /// 上不会再动，补的只是横向和光标历史。方向键、滚轮、滚动条都不走这一档，
    /// 直接是贴边揭示。
    fn reveal_after_motion(&mut self, dir: MoveDir, page: usize) {
        if matches!(dir, MoveDir::PageUp | MoveDir::PageDown) && self.scroll_limit > 0 {
            let (row, _) = self.caret_row();
            let row = row.max(0) as usize;
            self.reveal_rows(row, page, self.scroll_limit);
        }
        self.ensure_caret_visible(page);
    }

    /// The one caret primitive, shared by every `cm_move_*` and its
    /// `cm_select_*` twin. The select handlers differ only in that they keep
    /// the far edge of the selection as the anchor instead of collapsing it
    /// (the selection helper).
    pub fn caret_motion(&mut self, dir: MoveDir, page: usize, extend: bool) {
        if !extend {
            if let Some(anchor) = self.anchor {
                // cm_move_* with the anchor armed: the caret
                // commit (the refresh step) drops every range
                // and extra caret and rebuilds the selection
                // as [anchor, caret], so a plain move extends
                // the anchored selection instead of collapsing
                // it.
                self.extra_carets.clear();
                let to = self.caret_target(dir, page);
                self.caret = to;
                self.selection.clear();
                if anchor != to {
                    self.selection.add(dg_core::Range {
                        start: anchor,
                        end: to,
                    });
                }
                self.reveal_after_motion(dir, page);
                return;
            }
            self.collapse_selection(dir);
        } else if self.anchor.is_some() {
            // cm_select_* runs the select command first, which
            // releases the anchor before the extending move.
            self.anchor = None;
        }
        let anchor = if extend {
            self.motion_anchor()
        } else {
            self.caret
        };
        let to = self.caret_target(dir, page);
        self.caret = to;
        if extend {
            self.selection.clear();
            if anchor != to {
                self.selection.add(dg_core::Range {
                    start: anchor,
                    end: to,
                });
            }
        }
        self.reveal_after_motion(dir, page);
    }

    /// The fixed edge of an extending move: the end of the live selection that the caret is
    /// not on, or the caret itself when nothing is selected yet.
    fn motion_anchor(&self) -> (usize, usize) {
        if self.selection.ranges.len() == 1 {
            let r = &self.selection.ranges[0];
            if r.end == self.caret {
                return r.start;
            }
            if r.start == self.caret {
                return r.end;
            }
        }
        self.caret
    }

    /// Whether several carets are live. With a lone range the navigation
    /// handlers take their own path. A multi-caret edit leaves the set without
    /// any range at all — every caret is a bare point then — so the extra
    /// carets answer on their own.
    pub fn multi_caret(&self) -> bool {
        self.selection.ranges.len() > 1 || !self.extra_carets.is_empty()
    }

    /// The range the primary caret owns: the first range whose end is at or
    /// past the caret, provided the caret sits inside it — the interval walk
    /// every multi-caret repeater opens with. Falls back to the first range
    /// when the caret precedes them all, the walk's default answer.
    fn primary_range_index(&self) -> usize {
        let mut idx = 0usize;
        for (i, r) in self.selection.ranges.iter().enumerate() {
            if r.end >= self.caret {
                idx = i;
                break;
            }
        }
        if self.caret < self.selection.ranges[idx].start {
            0
        } else {
            idx
        }
    }

    /// Rebuilds the caret pair from the range set after a multi-caret move:
    /// every range's end holds one caret and the primary caret keeps the
    /// range it already sat on. What keeps each caret paired with the range
    /// its own edit deletes through (`range_at_caret`).
    fn sync_multi_carets(&mut self, primary: usize) {
        let mut extras: Vec<(usize, usize)> = Vec::with_capacity(self.selection.ranges.len());
        for (i, r) in self.selection.ranges.iter().enumerate() {
            if i == primary {
                continue;
            }
            extras.push(r.end);
        }
        self.caret = self.selection.ranges[primary].end;
        self.extra_carets = extras;
    }

    /// How many visual rows the caret's line wraps into.
    fn caret_line_rows(&self) -> usize {
        let line = self.caret.0;
        let start = self.rows.line_to_visual_row(line);
        let end = if line + 1 < self.doc.line_count() {
            self.rows.line_to_visual_row(line + 1)
        } else {
            self.rows.total_rows()
        };
        end.saturating_sub(start).max(1)
    }

    /// the selection helper as a move consequence: the run ends. The ranges and
    /// the extra carets go, the old set is stashed for the restore path
    /// the way the single-caret collapse stashes it, and the
    /// move that follows runs on the primary caret alone.
    fn end_multi_caret(&mut self) {
        if !self.column_mode {
            self.saved_selection = self.selection.ranges.clone();
        }
        self.selection.clear();
        self.extra_carets.clear();
    }

    /// The caret move with several carets live. The plain move (Left/Right),
    /// the extending move (Shift+Left/Right and, with a delta of
    /// &plusmn;0x1FFFFFFF, Shift+Home/End), the word moves and the
    /// Home move / End move branches all repeat the command on
    /// every range instead of moving the primary caret alone.
    /// Returns
    /// whether the run survived — `false` means the move has no repeater,
    /// the set was dropped and the caller still owes the primary caret the
    /// ordinary single-caret move.
    pub fn caret_motion_multi(&mut self, dir: MoveDir, page: usize, extend: bool) -> bool {
        // A multi-caret edit leaves the caret set as bare points: the repeaters
        // walk ranges, so give every live caret its own empty range first. The
        // primary is one of the points, and `sync_multi_carets` hands it back to
        // the range it lands on by position, not by a stored index.
        if self.selection.ranges.is_empty() {
            let mut points: Vec<(usize, usize)> = self.extra_carets.clone();
            points.push(self.caret);
            points.sort_unstable();
            self.selection.ranges = points
                .iter()
                .map(|&p| dg_core::Range { start: p, end: p })
                .collect();
        }
        // nothing in the implementation enforces that on insert, and the repeaters
        // below all walk the set in document order.
        if self
            .selection
            .ranges
            .windows(2)
            .any(|w| w[0].start > w[1].start)
        {
            self.selection.ranges.sort_by_key(|r| r.start);
        }
        // Take the single-caret path on a wrapped row.
        let dir = match dir {
            MoveDir::SublineHome | MoveDir::SublineEnd if self.caret_line_rows() > 1 => {
                self.end_multi_caret();
                return false;
            }
            MoveDir::SublineHome => MoveDir::LineHead,
            MoveDir::SublineEnd => MoveDir::LineEnd,
            other => other,
        };
        match (dir, extend) {
            // the multi-caret move: every range collapses to a caret — a bare
            // caret steps one column inside its own line, clamped at both
            // ends and never wrapping, and a live selection falls back onto
            // the edge the move walks away from (Left: its start, Right: its
            // end). Two carets landing on the same point merge.
            (MoveDir::Left, false) => self.multi_step_col(-1),
            (MoveDir::Right, false) => self.multi_step_col(1),
            // every range grows from its start (a backward
            // move) or its end (a forward move), clamped to the line bounds.
            // Shift+Home/End are the same repeater with a delta past any
            // line length, and a range whose start runs into the previous
            // range's end merges into it.
            (MoveDir::Left, true) => self.multi_extend_col(-1),
            (MoveDir::LineHead, true) => self.multi_extend_col(-0x1FFF_FFFF),
            (MoveDir::Right, true) => self.multi_extend_col(1),
            (MoveDir::LineEnd, true) => self.multi_extend_col(0x1FFF_FFFF),
            // the Home move / the End move: one caret per line, at
            // column 0 or at the line's end; a later range on a line an
            // earlier one already covers is erased.
            (MoveDir::LineHead, false) => self.multi_line_edge(true),
            (MoveDir::LineEnd, false) => self.multi_line_edge(false),
            // every caret steps one word, scanning from the
            // end of its range forward or its start backward.
            (MoveDir::WordRight, false) => self.multi_word_step(true),
            (MoveDir::WordLeft, false) => self.multi_word_step(false),
            // every range grows a word at the end the move
            // walks towards.
            (MoveDir::WordRight, true) => self.multi_word_extend(true),
            (MoveDir::WordLeft, true) => self.multi_word_extend(false),
            // Up/Down, the page moves and the document ends have no
            // repeater at all: their handlers drop the set and move the
            // primary caret.
            _ => {
                self.end_multi_caret();
                return false;
            }
        }
        self.ensure_caret_visible(page);
        true
    }

    /// the multi-caret move, the plain Left/Right repeater. The merge step
    /// compares each node's new end with the previous node's end
    /// and erases the later one, so collided carets fold together; the
    /// primary follows the node it sat on, or the node that swallowed it.
    fn multi_step_col(&mut self, delta: i32) {
        let primary = self.primary_range_index();
        let mut kept: Vec<dg_core::Range> = Vec::with_capacity(self.selection.ranges.len());
        let mut primary_out: Option<usize> = None;
        for (i, mut r) in std::mem::take(&mut self.selection.ranges)
            .into_iter()
            .enumerate()
        {
            if r.start == r.end {
                // a bare caret steps inside its own line, clamped at both
                // ends — the repeater never wraps to the neighbouring line.
                // Left walks the start, Right walks the end, and the point
                // that did not move follows the one that did.
                if delta > 0 {
                    let len = self.doc.line_len(r.start.0);
                    r.start.1 = (r.start.1 + delta as usize).min(len);
                } else {
                    r.start.1 = r.start.1.saturating_sub((-delta) as usize);
                }
                r.end = r.start;
            } else if delta > 0 {
                r.start = r.end;
            } else {
                r.end = r.start;
            }
            if kept.last().map(|p| p.end) == Some(r.end) {
                if i == primary && primary_out.is_none() {
                    primary_out = Some(kept.len().saturating_sub(1));
                }
                continue;
            }
            if i == primary {
                primary_out = Some(kept.len());
            }
            kept.push(r);
        }
        self.selection.ranges = kept;
        self.sync_multi_carets(primary_out.unwrap_or(0));
    }

    /// The multi-caret extend: the Shift+Left/Right repeater — and, with a delta
    /// past any line length, the Shift+Home/End pair. A range whose start lands
    /// inside the previous
    /// range's span merges into it, and one bare caret left behind is no
    /// run at all (the selection helper).
    fn multi_extend_col(&mut self, delta: i32) {
        let primary = self.primary_range_index();
        let mut kept: Vec<dg_core::Range> = Vec::with_capacity(self.selection.ranges.len());
        let mut primary_out: Option<usize> = None;
        for (i, mut r) in std::mem::take(&mut self.selection.ranges)
            .into_iter()
            .enumerate()
        {
            if delta <= 0 {
                // start walks back, clamped at column 0
                let moved = r.start.1 as i64 + delta as i64;
                r.start.1 = if moved < 0 { 0 } else { moved as usize };
            } else {
                // end walks forward, clamped at the line's end
                let len = self.doc.line_len(r.end.0) as i64;
                let moved = r.end.1 as i64 + delta as i64;
                r.end.1 = moved.clamp(0, len) as usize;
            }
            if let Some(prev) = kept.last() {
                if r.start <= prev.end {
                    if i == primary && primary_out.is_none() {
                        primary_out = Some(kept.len() - 1);
                    }
                    continue;
                }
            }
            if i == primary {
                primary_out = Some(kept.len());
            }
            kept.push(r);
        }
        self.selection.ranges = kept;
        if self.selection.ranges.len() == 1
            && self.selection.ranges[0].start == self.selection.ranges[0].end
        {
            self.end_multi_caret();
            return;
        }
        self.sync_multi_carets(primary_out.unwrap_or(0));
    }

    /// the Home move (Home) and the End move (End): every range becomes
    /// a caret at the head or the end of its line, and a later range on a
    /// line an earlier one already covers is erased, so one caret per line
    /// survives — the first one, which is the leftmost on that line.
    fn multi_line_edge(&mut self, home: bool) {
        let primary = self.primary_range_index();
        let mut kept: Vec<dg_core::Range> = Vec::with_capacity(self.selection.ranges.len());
        let mut primary_out: Option<usize> = None;
        let mut last_line: Option<usize> = None;
        for (i, mut r) in std::mem::take(&mut self.selection.ranges)
            .into_iter()
            .enumerate()
        {
            if Some(r.start.0) == last_line {
                if i == primary && primary_out.is_none() {
                    primary_out = Some(kept.len().saturating_sub(1));
                }
                continue;
            }
            if home {
                r.start = (r.start.0, 0);
            } else {
                r.start.1 = self.doc.line_len(r.start.0);
            }
            r.end = r.start;
            if i == primary {
                primary_out = Some(kept.len());
            }
            last_line = Some(r.start.0);
            kept.push(r);
        }
        self.selection.ranges = kept;
        self.sync_multi_carets(primary_out.unwrap_or(0));
    }

    /// The insert rejects a point an earlier one already covers, so collided carets
    /// merge. The primary caret lands on the point nearest its own scan.
    fn multi_word_step(&mut self, forward: bool) {
        let target = self.word_scan_at(self.caret.0, self.caret.1, forward);
        let mut points: Vec<(usize, usize)> = Vec::with_capacity(self.selection.ranges.len());
        for r in std::mem::take(&mut self.selection.ranges).iter() {
            let from = if forward { r.end } else { r.start };
            let p = self.word_scan_at(from.0, from.1, forward);
            // the scan is monotonic across a sorted set, so an equal point
            // is always the one just pushed
            if points.last() != Some(&p) {
                points.push(p);
            }
        }
        let idx = points.iter().position(|&p| p >= target).unwrap_or(0);
        self.selection.ranges = points
            .into_iter()
            .map(|p| dg_core::Range { start: p, end: p })
            .collect();
        self.sync_multi_carets(idx);
    }

    /// The end forward, the start backward — and ranges that overlap after the step
    /// merge.
    fn multi_word_extend(&mut self, forward: bool) {
        let primary = self.primary_range_index();
        let mut kept: Vec<dg_core::Range> = Vec::with_capacity(self.selection.ranges.len());
        let mut primary_out: Option<usize> = None;
        for (i, mut r) in std::mem::take(&mut self.selection.ranges)
            .into_iter()
            .enumerate()
        {
            if forward {
                let p = self.word_scan_at(r.end.0, r.end.1, true);
                if p > r.end {
                    r.end = p;
                }
            } else {
                let p = self.word_scan_at(r.start.0, r.start.1, false);
                if p < r.start {
                    r.start = p;
                }
            }
            if let Some(prev) = kept.last() {
                if r.start <= prev.end {
                    if i == primary && primary_out.is_none() {
                        primary_out = Some(kept.len() - 1);
                    }
                    continue;
                }
            }
            if i == primary {
                primary_out = Some(kept.len());
            }
            kept.push(r);
        }
        self.selection.ranges = kept;
        if self.selection.ranges.len() == 1
            && self.selection.ranges[0].start == self.selection.ranges[0].end
        {
            self.end_multi_caret();
            return;
        }
        self.sync_multi_carets(primary_out.unwrap_or(0));
    }

    /// The next line in `down`'s direction that is on screen: a folded line is
    /// hidden, so a move that crosses a line boundary steps over the fold
    /// instead of landing on a row the view does not show.
    fn visible_line(&self, line: usize, down: bool) -> Option<usize> {
        let n = self.doc.line_count();
        let mut l = line;
        loop {
            if down {
                l += 1;
                if l >= n {
                    return None;
                }
            } else {
                if l == 0 {
                    return None;
                }
                l -= 1;
            }
            if !self.folding.is_folded(l) {
                return Some(l);
            }
        }
    }

    /// Where `dir` puts the caret, with no side effects.
    fn caret_target(&self, dir: MoveDir, page: usize) -> (usize, usize) {
        let (line, col) = self.caret;
        let lines = self.doc.line_count();
        let len = self.doc.line_len(line);
        let clamp = |line: usize, col: usize| (line, col.min(self.doc.line_len(line)));
        match dir {
            MoveDir::Left => {
                if col > 0 {
                    (line, col - 1)
                } else if let Some(l) = self.visible_line(line, false) {
                    (l, self.doc.line_len(l))
                } else {
                    (line, col)
                }
            }
            MoveDir::Right => {
                if col < len {
                    (line, col + 1)
                } else if let Some(l) = self.visible_line(line, true) {
                    (l, 0)
                } else {
                    (line, col)
                }
            }
            MoveDir::Up => match self.visible_line(line, false) {
                Some(l) => clamp(l, col),
                None => (line, col),
            },
            MoveDir::Down => match self.visible_line(line, true) {
                Some(l) => clamp(l, col),
                None => (line, col),
            },
            // `cm_move_toheadofsubline` / `toendofsubline` — what key.ini binds
            // the bare Home and End keys to: the bounds of the wrapped row the
            // caret sits on, not of the whole logical line.
            MoveDir::SublineHome => {
                let (_, start, _) = self.caret_segment();
                (line, start)
            }
            MoveDir::SublineEnd => {
                let (_, _, end) = self.caret_segment();
                (line, end)
            }
            // `cm_move_toheadofline` is three-state, from the Home move: past
            // the indentation it goes to the first non-blank, in the
            // indentation it goes to column 0, and at column 0 it goes back to
            // the first non-blank.
            MoveDir::LineHead => {
                let blank = self.leading_blank_len(line);
                (
                    line,
                    if col == 0 {
                        blank
                    } else if col > blank {
                        blank
                    } else {
                        0
                    },
                )
            }
            MoveDir::LineEnd => (line, len),
            MoveDir::DocHead => (0, 0),
            // `cm_move_toendoffile`: the last line's end, `` giving
            // its length.
            MoveDir::DocEnd => {
                let last = lines.saturating_sub(1);
                (last, self.doc.line_len(last))
            }
            MoveDir::WordRight => self.word_scan(true),
            MoveDir::WordLeft => self.word_scan(false),
            // Page moves travel by visual row (the page move multiplies the
            // delta by the row height), keeping the caret's offset from the
            // start of its row.
            MoveDir::PageUp => self.page_target(page, true),
            MoveDir::PageDown => self.page_target(page, false),
        }
    }

    /// The visual row under the caret as (index of that row within the line,
    /// first char, one past last char), sliced out of the same break-position
    /// array the paint path uses: it holds each break's end offset and the
    /// final row runs to the line's end.
    fn caret_segment(&self) -> (usize, usize, usize) {
        let (line, col) = self.caret;
        let breaks = self.segment_breaks(line);
        let mut start = 0usize;
        for (i, &b) in breaks.iter().enumerate() {
            if col <= b {
                return (i, start, b);
            }
            start = b;
        }
        (breaks.len(), start, self.doc.line_len(line))
    }

    /// A line's break offsets, clamped to its length.
    fn segment_breaks(&self, line: usize) -> Vec<usize> {
        let len = self.doc.line_len(line);
        self.wrap
            .store
            .arrays
            .get(line)
            .map(|a| {
                a.data
                    .iter()
                    .map(|&b| b.max(0).min(len as i32) as usize)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The char span of visual row `seg` of `line`.
    fn segment_span(&self, line: usize, seg: usize) -> (usize, usize) {
        let breaks = self.segment_breaks(line);
        let start = if seg == 0 {
            0
        } else {
            breaks.get(seg - 1).copied().unwrap_or(0)
        };
        let end = breaks
            .get(seg)
            .copied()
            .unwrap_or_else(|| self.doc.line_len(line));
        (start, end)
    }

    /// `page` visual rows up (`up`) or down, keeping the caret's offset from
    /// the start of its row — the reason the page move works from the row
    /// array and not from a line number.
    fn page_target(&self, page: usize, up: bool) -> (usize, usize) {
        let (line, col) = self.caret;
        let seg = self.caret_segment();
        let row = self.rows.line_to_visual_row(line) + seg.0;
        let total = self.rows.total_rows();
        let target = if up {
            row.saturating_sub(page)
        } else {
            (row + page).min(total.saturating_sub(1))
        };
        let (l, s) = match self.row_to_line_segment(target) {
            Some(p) => p,
            None => return self.caret,
        };
        let (start, end) = self.segment_span(l, s);
        (l, start + (col - seg.1).min(end.saturating_sub(start)))
    }

    /// the class-run forward move forward / the class-run backward move backward. One class run is
    /// consumed per press, so the caret steps to the far edge of the word under
    /// it and the next press crosses the punctuation or blank run after it.
    /// At the end of a line the scan continues at column 0 of the next line,
    /// and at the end of the document it does not move.
    fn word_scan(&self, forward: bool) -> (usize, usize) {
        let (line, col) = self.caret;
        self.word_scan_at(line, col, forward)
    }

    /// The position-parameterized form of [`word_scan`](Self::word_scan),
    /// for the commands that scan from a caret of their own choosing
    /// (the multi-caret word delete reads every caret, not just the
    /// primary one). Same scan, same edges, same document-end stop.
    pub(crate) fn word_scan_at(&self, line: usize, col: usize, forward: bool) -> (usize, usize) {
        let len = self.doc.line_len(line);
        // A caret past the line end scans from the end: word commands must
        // never index the line with a column its text does not have.
        let col = col.min(len);
        if forward {
            if col < len {
                let chars = self.line_chars(line);
                let cls = dg_core::textops::char_class(chars[col]);
                let mut c = col + 1;
                while c < len && dg_core::textops::char_class(chars[c]) == cls {
                    c += 1;
                }
                return (line, c);
            }
            return if let Some(l) = self.visible_line(line, true) {
                (l, 0)
            } else {
                (line, col)
            };
        }
        if col > 0 {
            let chars = self.line_chars(line);
            let cls = dg_core::textops::char_class(chars[col - 1]);
            let mut p = col - 1;
            let mut start = p;
            while p > 0 {
                p -= 1;
                if dg_core::textops::char_class(chars[p]) != cls {
                    break;
                }
                start = p;
            }
            return (line, start);
        }
        if let Some(l) = self.visible_line(line, false) {
            return (l, self.doc.line_len(l));
        }
        (line, col)
    }

    /// One character's width in visual cells: this implementation's unit for the
    /// pixel widths the pixel measure measures through GDI. A monospace
    /// font gives every character below 0x2000 one cell and the wide
    /// (CJK and friends) ones two, which is also the `hit_test` model.
    /// A modifier takes none — it attaches to the cell before it, and a
    /// column pasted under a `☕️` has to line up with the `☕` above it.
    fn char_cell(ch: char) -> usize {
        if attaches_to_previous(ch) {
            0
        } else if (ch as u32) >= 0x2000 {
            2
        } else {
            1
        }
    }

    /// The visual width of `line[0..col]` in cells — this implementation's stand-in
    /// for the caret's pixel x, which is the target the
    /// column paste aligns to. Tabs advance to the next `tab_size` stop,
    /// the tab-width branch.
    pub(crate) fn visual_cells_at(&self, line: usize, col: usize) -> usize {
        let chars = self.line_chars(line);
        let tab = self.tab_size.max(1) as usize;
        let mut width = 0;
        for ch in chars.iter().take(col) {
            if *ch == '\t' {
                width += tab - width % tab;
            } else {
                width += Self::char_cell(*ch);
            }
        }
        width
    }

    /// the tab-width branch: walk `line` accumulating cell widths until the
    /// total passes `target`. The return is the character column the scan
    /// stopped at — the insert position — and the width including the
    /// character that passed the target, which is what the column paste
    /// subtracts the target from to size its space padding. A tab jumps
    /// to the next `tab_size` stop, so a target inside a tab's span
    /// lands on the tab's far edge.
    pub(crate) fn scan_visual(&self, line: usize, target: usize) -> (usize, usize) {
        let chars = self.line_chars(line);
        let tab = self.tab_size.max(1) as usize;
        let len = chars.len();
        let mut width = 0;
        let mut i = 0;
        while i < len {
            let ch = chars[i];
            if ch == '\r' || ch == '\n' {
                break;
            }
            width += if ch == '\t' {
                tab - width % tab
            } else {
                Self::char_cell(ch)
            };
            if width <= target {
                i += 1;
            } else {
                break;
            }
        }
        (i, width)
    }

    /// One line's characters, for the scans that walk a line by class. The
    /// handle is the document's own cached view, so a scan over a long line
    /// shares one decode with the paint instead of building its own.
    fn line_chars(&self, line: usize) -> std::rc::Rc<Vec<char>> {
        match self.doc.line_chars(line) {
            Some(chars) => chars,
            None => std::rc::Rc::new(Vec::new()),
        }
    }

    /// The length of a line's leading blank run — the loop in
    /// the Home move that smart Home targets.
    fn leading_blank_len(&self, line: usize) -> usize {
        self.line_chars(line)
            .iter()
            .take_while(|&&c| dg_core::textops::char_class(c) == dg_core::textops::CharClass::Blank)
            .count()
    }

    /// The two outer endpoints the selection touches, in document order.
    fn outer_points(&self) -> ((usize, usize), (usize, usize)) {
        let mut lo = self.caret;
        let mut hi = self.caret;
        for r in self.selection.ranges.iter() {
            lo = lo.min(r.start).min(r.end);
            hi = hi.max(r.start).max(r.end);
        }
        (lo, hi)
    }

    /// The lines the caret and every selection range touch: the scope the
    /// whole-line commands work on, which is why a transform with no selection
    /// still rewrites the caret's own line.
    pub fn touched_lines(&self) -> (usize, usize) {
        let last = self.doc.line_count().saturating_sub(1);
        let ((a, _), (b, _)) = self.outer_points();
        (a.min(last), b.min(last))
    }

    /// `cm_move_lineup` and `cm_move_linedown`: slide the caret's line — or the whole block of
    /// lines the selection touches — past its neighbour. Only the line *contents* rotate;
    /// every position keeps the terminator it already had, which is what stops a block moved
    /// down onto the last line from leaving an empty line behind and keeps a mixed-EOL file
    /// mixed. The caret and the ranges travel with the block. The return is the region the
    /// rewrite covers, so the caller can tell the language server; None means the block is
    /// already at that edge and both handlers return without touching the document.
    pub fn move_lines(&mut self, up: bool) -> Option<(Pos, Pos, String)> {
        let count = self.doc.line_count();
        if count < 2 {
            return None;
        }
        let (first, block_end) = self.touched_lines();
        if up {
            if first == 0 {
                return None;
            }
        } else if block_end + 1 >= count {
            return None;
        }
        let (r0, r1) = if up {
            (first - 1, block_end)
        } else {
            (first, block_end + 1)
        };
        let n = r1 - r0 + 1;
        // (text, terminator) per line, before anything changes.
        let mut lines: Vec<(String, String)> = Vec::with_capacity(n);
        for line in r0..=r1 {
            let mut text = String::new();
            self.doc.line_text(line, &mut text);
            let term = match self.doc.line_eol(line) {
                Some(Eol::Crlf) => "\r\n",
                Some(Eol::Lf) => "\n",
                Some(Eol::Cr) => "\r",
                None => "",
            };
            lines.push((text, term.to_string()));
        }
        let mut moved = String::new();
        for pos in 0..n {
            let src = if up {
                (pos + 1) % n
            } else if pos == 0 {
                n - 1
            } else {
                pos - 1
            };
            moved.push_str(&lines[src].0);
            moved.push_str(&lines[pos].1);
        }
        let start = Pos { line: r0, col: 0 };
        // The replaced region covers the block's lines, plus the
        // terminator that follows the last one when it has one (the
        // (K, len)..(K + 1, 0) gap).
        let end = if lines[n - 1].1.is_empty() {
            Pos {
                line: r1,
                col: self.doc.line_len(r1),
            }
        } else {
            Pos { line: r1 + 1, col: 0 }
        };
        if !self.apply_replace(start, end, &moved) {
            return None;
        }
        let shift = |p: (usize, usize)| -> (usize, usize) {
            if p.0 < r0 || p.0 > r1 {
                return p;
            }
            let line = if up {
                if p.0 == r0 { r1 } else { p.0 - 1 }
            } else if p.0 == r1 {
                r0
            } else {
                p.0 + 1
            };
            (line, p.1.min(self.doc.line_len(line)))
        };
        self.caret = shift(self.caret);
        let ranges = self
            .selection
            .ranges
            .iter()
            .map(|r| dg_core::Range {
                start: shift(r.start),
                end: shift(r.end),
            })
            .collect::<Vec<_>>();
        self.selection.clear();
        for r in ranges {
            self.selection.add(r);
        }
        Some((start, end, moved))
    }

    /// `cm_select_toprevline` and `cm_select_tonextline`, as both paths implement them: both end
    /// in `the selection collapse( view, from, to)`, which collapses the ranges, adds the one
    /// range `from`..`to`, and leaves the caret on `to`. Only the far anchor therefore moves,
    /// one whole line per press, while the near edge keeps its own position. A still-single-line
    /// selection has no far edge to keep, and the handlers snap it to that line's bounds — ``
    /// for 55, `( line, 0)` for 56 — which is why the first press covers two full lines. At the
    /// top or bottom of the document the line count check fails and nothing moves.
    pub fn extend_line_selection(&mut self, up: bool, page: usize) -> bool {
        let last = self.doc.line_count().saturating_sub(1);
        let (lo, hi) = self.outer_points();
        let (from, to) = if up {
            if lo.0 == 0 {
                return false;
            }
            let near = if lo.0 == hi.0 {
                (hi.0, self.doc.line_len(hi.0))
            } else {
                hi
            };
            (near, (lo.0 - 1, 0))
        } else {
            if hi.0 >= last {
                return false;
            }
            let near = if lo.0 == hi.0 { lo } else { (lo.0, 0) };
            (near, (hi.0 + 1, self.doc.line_len(hi.0 + 1)))
        };
        self.caret = to;
        self.selection.clear();
        self.selection.add(dg_core::Range {
            start: from,
            end: to,
        });
        self.ensure_caret_visible(page);
        true
    }

    /// Which end of a pair a character is: its index into
    /// [`BRACKET_PAIRS`] and whether it opens or closes that pair.
    fn bracket_side(c: char) -> Option<(usize, bool)> {
        if let Some(i) = BRACKET_PAIRS.iter().position(|p| p.0 == c) {
            return Some((i, true));
        }
        BRACKET_PAIRS
            .iter()
            .position(|p| p.1 == c)
            .map(|i| (i, false))
    }

    /// `idx` and `open` describe the anchor: its pair index, and which end of that pair it
    /// is. `open` fixes the scan direction and decides what the stack counts — from an
    /// opener forward the closers pay the stack down, from a closer backward the openers
    /// do — and the anchor itself starts the stack, so the pop that empties it is its
    /// partner. A bracket of the wrong pair on top of the stack ends the search, which is
    /// what keeps `( [ ) ]` from reporting a match.
    fn bracket_partner(
        &self,
        at: (usize, usize),
        idx: usize,
        open: bool,
    ) -> Option<(usize, usize)> {
        const WINDOW: usize = 2000;
        let last = self.doc.line_count().saturating_sub(1);
        let mut stack: Vec<usize> = vec![idx];
        let lines: Box<dyn Iterator<Item = usize>> = if open {
            Box::new(at.0..=(at.0 + WINDOW - 1).min(last))
        } else {
            Box::new((at.0.saturating_sub(WINDOW - 1)..=at.0).rev())
        };
        for line in lines {
            let chars = self.line_chars(line);
            let span: Box<dyn Iterator<Item = usize>> = if open {
                let from = if line == at.0 { at.1 + 1 } else { 0 };
                Box::new(from..chars.len().max(from))
            } else {
                let to = if line == at.0 { at.1 } else { chars.len() };
                Box::new((0..to).rev())
            };
            for col in span {
                let Some((i, opens)) = Self::bracket_side(chars[col]) else {
                    continue;
                };
                if opens == open {
                    stack.push(i);
                } else if stack.last() == Some(&i) {
                    stack.pop();
                    if stack.is_empty() {
                        return Some((line, col));
                    }
                } else {
                    return None;
                }
            }
        }
        None
    }

    /// The bracket the caret points at: the character under it, else the one just before.
    fn caret_bracket(&self) -> Option<((usize, usize), (usize, bool))> {
        let (line, col) = self.caret;
        let chars = self.line_chars(line);
        for c in [Some(col), col.checked_sub(1)].into_iter().flatten() {
            if let Some(side) = chars.get(c).and_then(|&ch| Self::bracket_side(ch)) {
                return Some(((line, c), side));
            }
        }
        None
    }

    /// `cm_move_brace`: put the caret exactly on the matching bracket and drop the
    /// selection. Recomputing it here is the same rule with less state.
    pub fn goto_matching_bracket(&mut self, page: usize) -> bool {
        let Some((at, pair)) = self.caret_bracket() else {
            return false;
        };
        let Some(target) = self.bracket_partner(at, pair.0, pair.1) else {
            return false;
        };
        self.selection.clear();
        self.caret = target;
        self.ensure_caret_visible(page);
        true
    }

    /// The innermost pair around the caret, as a stack replay of the window above
    /// it: openers before the caret are pushed, a closer pops its own opener, and
    /// whatever is left on the stack encloses the caret. Its partner has to come
    /// at or after the caret, or the pair closed before we got here.
    fn enclosing_bracket_pair(&self) -> Option<((usize, usize), (usize, usize))> {
        const WINDOW: usize = 2000;
        let first = self.caret.0.saturating_sub(WINDOW - 1);
        let mut stack: Vec<((usize, usize), usize)> = Vec::new();
        for line in first..=self.caret.0 {
            let chars = self.line_chars(line);
            for (col, ch) in chars.iter().enumerate() {
                if line == self.caret.0 && col > self.caret.1 {
                    break;
                }
                let Some((idx, opens)) = Self::bracket_side(*ch) else {
                    continue;
                };
                if opens {
                    stack.push(((line, col), idx));
                } else if stack.last().map(|(_, i)| *i) == Some(idx) {
                    stack.pop();
                }
            }
        }
        let (open, idx) = stack.pop()?;
        let close = self.bracket_partner(open, idx, true)?;
        (close >= self.caret).then_some((open, close))
    }

    /// `cm_select_topair` and `cm_select_topair2`: select the text inside the pair the caret
    /// sits in, or the same range with both brackets. One-shot — each press recomputes the pair
    /// from the caret, so pressing again does not walk outward.
    pub fn select_bracket_pair(&mut self, inclusive: bool, page: usize) -> bool {
        let Some((open, close)) = self.enclosing_bracket_pair() else {
            return false;
        };
        let from = if inclusive {
            open
        } else {
            (open.0, open.1 + 1)
        };
        let to = if inclusive {
            (close.0, close.1 + 1)
        } else {
            close
        };
        self.caret = to;
        self.selection.clear();
        self.selection.add(dg_core::Range {
            start: from,
            end: to,
        });
        self.ensure_caret_visible(page);
        true
    }

    /// `cm_select_line`. With no selection it marks the whole caret line, and on an empty
    /// line it marks nothing: the range goes through the selection collapse, which
    /// returns before inserting when its two points are equal. With a selection it
    /// rewrites every range to whole-line and lets the ones that now touch merge, and it
    /// neither moves the caret nor scrolls — the handler only invalidates, which is why
    /// pressing it twice does nothing more.
    pub fn select_line(&mut self) {
        if self.selection.is_empty() {
            let line = self.caret.0;
            let len = self.doc.line_len(line);
            if len == 0 {
                return;
            }
            self.selection.add(dg_core::Range {
                start: (line, 0),
                end: (line, len),
            });
            return;
        }
        let whole: Vec<dg_core::Range> = {
            let mut acc: Vec<dg_core::Range> = self
                .selection
                .ranges
                .iter()
                .map(|r| {
                    let first = r.start.0.min(r.end.0);
                    let last = r.start.0.max(r.end.0);
                    dg_core::Range {
                        start: (first, 0),
                        end: (last, self.doc.line_len(last)),
                    }
                })
                .collect();
            acc.sort_by_key(|r| (r.start.0, r.end.0));
            acc.dedup_by(|a, b| b.start.0 <= a.end.0);
            acc
        };
        self.selection.ranges = whole;
    }

    /// A blank in the sense the paragraph commands use: the CRT `_SPACE` class — the line
    /// terminator is not part of the line text — plus the ideographic space the handler ORs
    /// in.
    fn is_para_blank(c: char) -> bool {
        matches!(c, ' ' | '\t' | '\u{b}' | '\u{c}' | '\r') || c == '\u{3000}'
    }

    /// `cm_move_nextpara` and `cm_move_prevpara`. Not blank-line delimited: the target
    /// is the first line walking in that direction that *starts* indented, and the caret
    /// lands on its first non-blank column. Lines that are completely empty ( zero
    /// leading blanks counted) and lines that start flush left are both skipped. Failing
    /// that it goes to the end ( next) or the start ( prev) of the document, unless the
    /// caret already sits on that line. The selection survives — the selection helper is
    /// never called — so only the caret endpoint moves.
    pub fn paragraph_motion(&mut self, next: bool) -> bool {
        let last = self.doc.line_count().saturating_sub(1);
        let mut line = self.caret.0 as isize + if next { 1 } else { -1 };
        while (0..=last as isize).contains(&line) {
            let chars = self.line_chars(line as usize);
            let indent = chars
                .iter()
                .take_while(|c| Self::is_para_blank(**c))
                .count();
            if indent > 0 {
                self.caret = (line as usize, indent);
                return true;
            }
            line += if next { 1 } else { -1 };
        }
        let target = if next { (last, 0) } else { (0, 0) };
        if target == self.caret {
            return false;
        }
        self.caret = target;
        true
    }

    /// `cm_move_jumpoutofpair`: scan forward from the caret across the whole
    /// document and land one character past the first closer found set in a string
    /// the frame configures and walks by `SendMessageW` line queries; the column it
    /// carries from one line to the next is the previous line's length, i.e. the
    /// scan is a plain forward text walk.
    pub fn jump_out_of_pair(&mut self, page: usize) -> bool {
        let (line, col) = self.caret;
        let last = self.doc.line_count().saturating_sub(1);
        for l in line..=last {
            let chars = self.line_chars(l);
            let from = if l == line { col } else { 0 };
            let found = chars
                .iter()
                .skip(from)
                .position(|c| is_closer(*c))
                .map(|p| p + from);
            if let Some(i) = found {
                self.caret = (l, i + 1);
                self.selection.clear();
                self.extra_carets.clear();
                self.ensure_caret_visible(page);
                return true;
            }
        }
        false
    }

    /// `cm_move_nextcaretpos` and `cm_move_prevcaretpos`: step the caret ring and
    /// jump. The jump clears the selection, is not itself recorded, and ends by
    /// sending `cm_move_middleline` — so the caller re-centres the view rather than
    /// scrolling minimally.
    pub fn history_move(&mut self, next: bool) -> Option<(usize, usize)> {
        let target = self.caret_history.jump(next)?;
        self.selection.clear();
        self.extra_carets.clear();
        self.caret = target;
        self.caret_history.recording = true;
        Some(target)
    }

    /// `cm_move_nexteditpos` / `cm_move_preveditpos`: step the edit ring and jump.
    /// The jump puts down the selection and the multi-carets the same way the
    /// caret ring's does, and is not itself recorded — records come only from
    /// landed edits ([`Self::apply_edit`]).
    pub fn edit_history_move(&mut self, next: bool) -> Option<(usize, usize)> {
        let target = self.edit_history.jump(next)?;
        self.selection.clear();
        self.extra_carets.clear();
        self.caret = target;
        Some(target)
    }

    /// `cm_move_middleline`: scroll without moving the caret, so the caret's own
    /// visual row — its line's first row plus the row it sits on after wrapping —
    /// lands in the middle of the viewport.
    pub fn center_caret_view(&mut self, viewport_rows: usize) -> bool {
        if viewport_rows == 0 {
            return false;
        }
        let row = self.rows.line_to_visual_row(self.caret.0) + self.caret_segment().0;
        let before = self.first_row;
        self.first_row = row
            .saturating_sub(viewport_rows / 2)
            .min(self.rows.total_rows().saturating_sub(1));
        self.first_row != before
    }

    /// `cm_select_restore`: swap the container the last collapse handed to the
    /// view back in. One level deep, because the save overwrites, and the caret
    /// goes to the end point of the leftmost restored range.
    pub fn restore_selection(&mut self) -> bool {
        if self.saved_selection.is_empty() {
            return false;
        }
        let mut saved = std::mem::take(&mut self.saved_selection);
        std::mem::swap(&mut self.selection.ranges, &mut saved);
        self.saved_selection = saved;
        self.caret = self
            .selection
            .ranges
            .iter()
            .min_by_key(|r| (r.start.0, r.start.1))
            .map(|r| r.end)
            .unwrap_or(self.caret);
        true
    }

    /// A plain caret move drops the selection, landing the caret on
    /// the edge it moves towards. A selection that outlives an arrow
    /// key turns the next character into "replace the document", so
    /// after Select All one keystroke was enough to lose a file -
    /// the live input harness walked into exactly that.
    fn collapse_selection(&mut self, dir: MoveDir) {
        if self.selection.is_empty() {
            return;
        }
        let backward = matches!(
            dir,
            MoveDir::Left
                | MoveDir::Up
                | MoveDir::SublineHome
                | MoveDir::LineHead
                | MoveDir::DocHead
                | MoveDir::WordLeft
                | MoveDir::PageUp
        );
        let mut edge: Option<(usize, usize)> = None;
        for r in self.selection.ranges.iter() {
            let p = if backward { r.start } else { r.end };
            edge = Some(match edge {
                None => p,
                Some(cur) if backward => cur.min(p),
                Some(cur) => cur.max(p),
            });
        }
        if let Some(p) = edge {
            self.caret = p;
        }
        if !self.column_mode {
            self.saved_selection = self.selection.ranges.clone();
        }
        self.selection.clear();
    }

    /// The caret's row record (the view/the view): the visual row it sits on,
    /// wrap continuation included, and its pixel offset inside that row's segment.
    /// The offset is segment-relative because that is how the painter places a row —
    /// a caret on the second wrap row of a long line is measured from that row's
    /// first character, not from the line's.
    pub fn caret_row(&self) -> (i32, i32) {
        let (line, col) = self.caret;
        let m = dg_render::pipeline::LineMetrics::new(&self.theme, self.tab_size as usize);
        // The reveal asks this on every keystroke and the paint asks it on every
        // frame; a caret that has not moved (and text that has not changed) gets
        // the same answer without walking the line again.
        let key = (line, col, self.edit_epoch, m.x, m.tab_cells, self.wrapped, self.theme.measured.len());
        if let Some((cached, subrow, px)) = self.caret_px_cache.get() {
            if cached == key {
                let row = self.rows.line_to_visual_row(line) + subrow;
                return (row as i32, px);
            }
            // A caret that moved a few columns along the same ruler continues
            // from where it was: an arrow key moves one character, and the walk
            // from the segment's first character is the whole line.
            if cached.0 == line
                && cached.2 == self.edit_epoch
                && cached.3 == m.x
                && cached.4 == m.tab_cells
                && cached.5 == self.wrapped
                && cached.6 == self.theme.measured.len()
                && cached.1.abs_diff(col) <= CARET_WALK_MAX
            {
                if let Some(near) = self.caret_px_near(&m, line, cached.1, px, col) {
                    let row = self.rows.line_to_visual_row(line) + subrow;
                    self.caret_px_cache.set(Some((key, subrow, near)));
                    return (row as i32, near);
                }
            }
        }
        // The break array is sorted, so the subrow is a partition point and the
        // segment's first column an index — a wrapped 5 MB line has sixty thousand
        // breaks, and none of them is copied here.
        let (subrow, start) = match self.wrap.store.arrays.get(line) {
            Some(a) => Self::segment_at(&a.data, col),
            None => (0, 0),
        };
        let Some(chars) = self.doc.line_chars(line) else {
            return (self.rows.line_to_visual_row(line) as i32, 0);
        };
        // Nothing is hidden on the caret's own line: the caret-line painter draws an
        // attribute-class-30 character whenever the row *is* the view, so the
        // caret measures the row exactly as it looks.
        let slice = &chars[start.min(chars.len())..];
        let counted = col.saturating_sub(start).min(slice.len());
        let mut px = m.x_at(slice, counted, &[]);
        // 虚拟空格：光标列在行尾之后时，像素按整格继续往外走，光标才
        // 能落在行尾的空地上。折行的行没有这块空地，列也不会越过行尾。
        let over = col.saturating_sub(start) - counted;
        if over > 0 && !self.wrapped {
            px += (over as i32) * m.x.max(1);
        }
        self.caret_px_cache.set(Some((key, subrow, px)));
        let row = self.rows.line_to_visual_row(line) + subrow;
        (row as i32, px)
    }

    /// The pixel of column `target`, derived from the pixel of `cached_col` a
    /// few characters away. Both ends have to sit in one wrap segment (the
    /// offset is measured from the segment's first character) and the run
    /// between them has to be free of tabs — a tab's width is the distance to
    /// the next stop, which does not add up in either direction. `None` sends
    /// the caller back to measuring from the segment's start.
    fn caret_px_near(
        &self,
        m: &dg_render::pipeline::LineMetrics<'_>,
        line: usize,
        cached_col: usize,
        px: i32,
        target: usize,
    ) -> Option<i32> {
        let chars = self.doc.line_chars(line)?;
        // A column past the line's end carries the 虚拟空格 cells, which are
        // not a distance over any character.
        if cached_col > chars.len() || target > chars.len() {
            return None;
        }
        let (from, to) = (cached_col.min(target), cached_col.max(target));
        let to = to.min(chars.len());
        let from = from.min(to);
        if let Some(a) = self.wrap.store.arrays.get(line) {
            let seg = |c: usize| a.data.partition_point(|&b| (b.max(0) as usize) < c);
            if seg(from) != seg(to) {
                return None;
            }
        }
        let slice = &chars[from..to];
        if slice.contains(&'\t') {
            return None;
        }
        let mut p = px;
        if cached_col <= target {
            for &ch in slice {
                p += m.advance(ch);
            }
        } else {
            for &ch in slice {
                p -= m.advance(ch);
            }
        }
        Some(p)
    }

    /// 竖直方向的揭示：把 `row` 收进 `[first_row, first_row + page)`。
    /// `context` 是这一行上下各自留出的上下文行数 —— 翻页的滚动留边
    /// （`Edit/ScrollLimit`）走的就是这一档，普通揭示传 0，也就是贴边。
    /// 留边不小于一整屏时等于没留，翻页条件本身就不成立。
    fn reveal_rows(&mut self, row: usize, page: usize, context: usize) {
        let context = if context >= page { 0 } else { context };
        if self.first_row + context > row {
            self.first_row = row.saturating_sub(context);
        } else if row + context >= self.first_row + page {
            self.first_row = (row + context + 1).saturating_sub(page);
        }
    }

    /// the caret reveal — bring the caret back into the visible rectangle, in cells
    /// horizontally and in rows vertically. This asks [`Self::caret_row`] rather than
    /// counting the column: a line with a tab is wider in pixels than it is in characters.
    pub fn ensure_caret_visible(&mut self, page: usize) {
        let (row, px) = self.caret_row();
        let row = row.max(0) as usize;
        self.reveal_rows(row, page, 0);
        // A non-empty text rectangle. The delta is whole cells, and the caret is
        // never left flush against an edge — 10 cells of context on the left, 40
        // px plus 10 cells on the right.
        if !self.wrapped && self.text_w > 0 {
            let xw = dg_render::pipeline::LineMetrics::new(&self.theme, self.tab_size as usize).x;
            let left = self.h_scroll * xw;
            let delta = if px >= left {
                let limit = (left + self.text_w - 40).max(0);
                if px > limit {
                    (px - limit) / xw + 10
                } else {
                    0
                }
            } else {
                (px - left) / xw - 10
            };
            self.h_scroll = (self.h_scroll + delta).max(0);
        }
        if self.virtual_wrap {
            let xw = dg_render::pipeline::LineMetrics::new(&self.theme, self.tab_size as usize).x;
            // 不换行: the range follows the caret, so the reveal always leaves room
            // for the column it just scrolled to. Ten cells of headroom keep the
            // strip from jumping at the exact moment the caret reaches the edge.
            self.h_max_cells = self.h_max_cells.max(px / xw.max(1) + 10);
        }
        // Every route to a new caret lands in the ring without each command having to know about it.
        self.caret_history.record(self.caret);
    }

    /// All matches of `needle` under the
    /// find options (the search engine's scan).
    pub fn find_all(&self, needle: &str, opts: FindOptions) -> Vec<Match> {
        dg_core::find_all(&self.doc, needle, opts)
    }

    pub fn count_matches(&self, needle: &str, opts: FindOptions) -> usize {
        dg_core::count(&self.doc, needle, opts)
    }

    /// Replace every match; each edit is
    /// one undo step. Returns the count.
    pub fn replace_all(&mut self, needle: &str, replacement: &str, opts: FindOptions) -> usize {
        dg_core::replace_all(&mut self.doc, &mut self.undo, needle, replacement, opts)
    }

    /// [`Self::replace_all`] limited to the matches inside `within`
    /// (the 替换对话框's 选区 range).
    pub fn replace_all_within(
        &mut self,
        needle: &str,
        replacement: &str,
        opts: FindOptions,
        within: Option<Match>,
    ) -> usize {
        dg_core::replace_all_within(&mut self.doc, &mut self.undo, needle, replacement, opts, within)
    }

    /// Stores the active find matches as per-line paint ranges (高亮匹配).
    /// A match that crosses a line break paints every line it touches.
    pub fn set_find_marks(&mut self, matches: &[Match]) {
        self.find_marks.clear();
        for m in matches {
            let (sl, sc, el, ec) = (m.start.0, m.start.1, m.end.0, m.end.1);
            if sl == el {
                if ec > sc {
                    self.find_marks.push((sl, sc, ec));
                }
                continue;
            }
            let first_len = self.doc.line_len(sl);
            if first_len > sc {
                self.find_marks.push((sl, sc, first_len));
            }
            for line in sl + 1..el {
                let len = self.doc.line_len(line);
                if len > 0 {
                    self.find_marks.push((line, 0, len));
                }
            }
            if ec > 0 {
                self.find_marks.push((el, 0, ec));
            }
        }
    }

    pub fn clear_find_marks(&mut self) {
        self.find_marks.clear();
    }

    /// Apply an insert edit through the
    /// undo stack (the insert command),
    /// shifting bookmarks below the edit.
    pub fn apply_insert(&mut self, pos: Pos, text: &str) -> bool {
        self.apply_edit(EditOp::Insert {
            pos,
            text: text.to_string(),
        })
    }

    /// Apply a range delete through the
    /// undo stack (the delete command).
    /// The removed text is captured
    /// automatically.
    pub fn apply_delete(&mut self, start: Pos, end: Pos) -> bool {
        if self.doc.is_large() {
            let ld = self.doc.large().unwrap();
            let span = ld.line_to_char(end.line).unwrap_or_else(|| ld.len_chars())
                + end.col
                - (ld.line_to_char(start.line).unwrap_or_else(|| ld.len_chars()) + start.col);
            if span > LARGE_UNDO_CAPTURE_CHARS {
                return false;
            }
        }
        let text = UndoStack::capture(&self.doc, start, end);
        self.apply_edit(EditOp::Delete { start, end, text })
    }

    /// Apply a replace through the
    /// undo stack (the replace command).
    pub fn apply_replace(&mut self, start: Pos, end: Pos, text: &str) -> bool {
        if self.doc.is_large() {
            let ld = self.doc.large().unwrap();
            let span = ld.line_to_char(end.line).unwrap_or_else(|| ld.len_chars())
                + end.col
                - (ld.line_to_char(start.line).unwrap_or_else(|| ld.len_chars()) + start.col);
            if span > LARGE_UNDO_CAPTURE_CHARS {
                return false;
            }
        }
        let old_text = UndoStack::capture(&self.doc, start, end);
        self.apply_edit(EditOp::Replace {
            start,
            end,
            old_text,
            new_text: text.to_string(),
        })
    }

    /// The font changed: the advance table is rebuilt, so every line that was
    /// already measured against the old one has to be walked again.
    pub fn reset_measured(&mut self) {
        self.measured_epoch = self.measured_epoch.wrapping_add(1);
        self.measured_lines.clear();
    }

    /// The text changed: every cache keyed on [`Self::edit_epoch`] is stale, and
    /// the two one-entry scans are dropped outright.
    fn bump_edit_epoch(&mut self) {
        self.edit_epoch = self.edit_epoch.wrapping_add(1);
        self.color_band_cache.borrow_mut().take();
        self.url_span_cache.borrow_mut().take();
        // A window is a column and a pixel of the text in front of it: an edit
        // anywhere on the line can move both, so the next paint measures
        // rather than reusing what the last one settled on. [`Self::apply_edit`]
        // puts back the rows its edit did not move.
        self.paint_win.borrow_mut().clear();
    }

    /// The single door every content mutation
    /// passes through. Read-only buffers apply
    /// nothing, the undo stack's history
    /// position is what makes the buffer
    /// modified, and bookmark shifting is keyed
    /// off the op here so a future edit path
    /// cannot forget either of them.
    fn apply_edit(&mut self, op: EditOp) -> bool {
        if self.read_only {
            return false;
        }
        // 大文件预检（M6）：预算/范围先于落栈 —— Document::edit 对
        // Large 的失败是静默 eprintln，若不预检，undo 栈会记录一条
        // 未生效的编辑，undo/redo 视图从此漂移。
        if self.doc.is_large() {
            let (s, e, t): (Pos, Pos, &str) = match &op {
                EditOp::Insert { pos, text } => (*pos, *pos, text.as_str()),
                EditOp::Delete { start, end, .. } => (*start, *end, ""),
                EditOp::Replace { start, end, new_text, .. } => (*start, *end, new_text.as_str()),
            };
            let ld = self.doc.large().unwrap();
            let to_char = |p: Pos| {
                ld.line_to_char(p.line).unwrap_or_else(|| ld.len_chars()) + p.col
            };
            if self
                .doc
                .large_edit_would_fail(to_char(s), to_char(e), t)
                .is_some()
            {
                return false;
            }
        }
        self.highlighter_note_edit(&op);
        // Record the line span this edit touches for the incremental
        // relayout (the old and new line counts of the replaced
        // range).
        let (start, old_lines, new_lines) = edit_region(&op);
        record_layout_region(&mut self.pending_layout, start.line, old_lines, new_lines);
        let (at, delta) = match &op {
            EditOp::Insert { pos, text } => (*pos, dg_core::undo::text_lines(text)),
            EditOp::Delete { start, text, .. } => (*start, -(dg_core::undo::text_lines(text))),
            EditOp::Replace {
                start,
                old_text,
                new_text,
                ..
            } => (
                *start,
                dg_core::undo::text_lines(new_text) - dg_core::undo::text_lines(old_text),
            ),
        };
        // 落笔后的光标落点（编辑位置环记的就是它）：插入跟插入文本的
        // 结尾，删除跟删除的起点——撤销的重做侧推光标用的同一口径。
        let landed = match &op {
            EditOp::Insert { pos, text } => dg_core::undo::text_end_pos(*pos, text),
            EditOp::Delete { start, .. } => *start,
            EditOp::Replace { start, new_text, .. } => {
                dg_core::undo::text_end_pos(*start, new_text)
            }
        };
        let ok = self.undo.apply(&mut self.doc, op);
        if ok {
            // The epoch bump drops the windows with the rest of the
            // content-keyed caches, but a window is a column and a pixel of the
            // text in front of it: an edit after that column moves neither, and
            // the next paint keeps the window instead of measuring its way in
            // from the line's first character. An edit that changed the line
            // count moved every line below it, so then nothing carries over.
            let windows: Vec<dg_render::pipeline::WindowMemo> = if delta == 0
                && old_lines == new_lines
            {
                self.paint_win
                    .borrow()
                    .windows()
                    .filter(|w| start.line != w.line || start.col > w.col)
                    .collect()
            } else {
                Vec::new()
            };
            // The caret's pixel is a position of the same kind: an edit at or
            // after the cached column leaves every character in front of it
            // where it was, so the entry still describes that column and only
            // its epoch needs refreshing. A line count change would move the
            // row, and a grown advance table would change the widths the walk
            // used, so either of those drops it instead.
            let caret_px = self.caret_px_cache.get().filter(|c| {
                let (line, col, _, _, _, _, measured) = c.0;
                delta == 0
                    && old_lines == new_lines
                    && measured == self.theme.measured.len()
                    && (start.line > line || (start.line == line && start.col >= col))
            });
            self.bump_edit_epoch();
            if !windows.is_empty() {
                let mut anchors = self.paint_win.borrow_mut();
                for w in windows {
                    anchors.set(w);
                }
            }
            if let Some((mut key, subrow, px)) = caret_px {
                key.2 = self.edit_epoch;
                self.caret_px_cache.set(Some((key, subrow, px)));
            }
            self.keep_highlights_before(start.line);
            self.shift_bookmarks((at.line, at.col), delta);
            if self.doc.is_large() {
                // 编辑使行号漂移：行式染色缓存整体失效，搜索命中
                // （raw 坐标）过期。纪元递增，供 find/select_all 校验。
                if let Some(hl) = self.large_hl.as_ref() {
                    hl.clear();
                }
                self.large_edits += 1;
            }
            // 落笔成功才记：只读或超预算的尝试不算编辑。
            self.edit_history.record((landed.line, landed.col));
        }
        ok
    }

    /// Buffer differs from what is on disk.
    /// Backed by the undo history position, so
    /// undoing everything since the save clears
    /// it.
    pub fn is_modified(&self) -> bool {
        if self.hex.is_some() {
            // The snapshot's from-file flag is the hex view's clean bit
            // ([snap], WM_USER's "modified?" query).
            return !self.hex.as_ref().unwrap().snap.from_file();
        }
        self.undo.is_modified()
    }

    /// Records the buffer as matching the file
    /// on disk — after a successful save, and
    /// after reading one in.
    pub fn mark_saved(&mut self) {
        if let Some(v) = self.hex.as_mut() {
            // A save reloads from the target, restoring the clean bit.
            v.snap.save_file(None);
            return;
        }
        self.undo.mark_saved();
        // 行修改状态: every line the save just wrote drops back to the strip's
        // "written back" colour; the untouched ones keep their plain band.
        self.doc.mark_all_saved();
    }

    /// The tab an opened file takes over instead of adding one: never saved,
    /// nothing typed into it, and not one of the frame's own generated buffers.
    /// Ctrl+N and the reset that follows closing the last tab both leave exactly
    /// this state, which is why opening a file from a fresh start does not strand
    /// an `Untitled` tab in front of it.
    pub fn is_untitled_blank(&self) -> bool {
        self.path.is_none()
            && self.tab_title.is_none()
            && self.grep_rows.is_none()
            && !self.read_only
            && !self.is_modified()
            && self.doc.line_count() == 1
            && self.doc.line_len(0) == 0
    }

    /// Tab label: the generated document's own title, else the file name without
    /// its directory
    /// saved.
    pub fn display_name(&self) -> String {
        if let Some(title) = &self.tab_title {
            return title.clone();
        }
        match &self.path {
            Some(p) => std::path::Path::new(p)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone()),
            None => match self.untitled_no {
                Some(n) => format!("Untitled{n}"),
                None => "Untitled".to_string(),
            },
        }
    }

    pub fn undo(&mut self) -> bool {
        if !self.undo.can_undo() {
            return false;
        }
        let ops = self.undo.undo_ops();
        let caret_target = revert_caret_target(ops.as_deref(), false);
        self.note_revert_ops(ops);
        let spans = self.undo.undo(&mut self.doc);
        if spans.is_empty() {
            return false;
        }
        self.bump_edit_epoch();
        for span in spans {
            self.shift_bookmarks((span.line, span.col), span.line_delta);
        }
        // 撤销只还原文本，不动输入状态；这里把光标带回这次编辑之前
        // 的位置 —— 行中回车再撤销，光标回到按回车前的那一格，而不是
        // 留在多出来的那一行上。
        if let Some(target) = caret_target {
            self.caret = target;
        }
        self.fit_input_to_document();
        true
    }

    pub fn redo(&mut self) -> bool {
        if !self.undo.can_redo() {
            return false;
        }
        let ops = self.undo.redo_ops();
        let caret_target = revert_caret_target(ops.as_deref(), true);
        self.note_revert_ops(ops);
        let spans = self.undo.redo(&mut self.doc);
        if spans.is_empty() {
            return false;
        }
        self.bump_edit_epoch();
        for span in spans {
            self.shift_bookmarks((span.line, span.col), span.line_delta);
        }
        // 重做把编辑重新落下：光标回到上次编辑落完后的位置（回车落在
        // 新行行首，重做后也回到那里）。
        if let Some(target) = caret_target {
            self.caret = target;
        }
        self.fit_input_to_document();
        true
    }

    /// Folds the edits an undo (or a redo) is about to land into the doors a
    /// forward edit already uses: the tree's byte checkpoints and the relayout
    /// region. Skipping them is what made a revert cost a whole-file re-parse and a
    /// whole-file re-wrap to put five characters back.
    ///
    /// The blunt path stays for the cases with no cheap description: a replayed
    /// macro (its edits are offsets into each other's output, not one gesture's
    /// disjoint edits), a large document (its layout and lexing run through their
    /// own windowed stores), and an edit set that lands nothing.
    fn note_revert_ops(&mut self, ops: Option<Vec<dg_core::EditOp>>) {
        let incremental = !self.doc.is_large()
            && ops.as_ref().is_some_and(|ops| !ops.is_empty());
        let Some(mut ops) = ops.filter(|_| incremental) else {
            self.hl_stale = true;
            self.pending_layout.clear();
            return;
        };
        // Noted highest first: every offset is expressed against the document the
        // revert starts from, and editing below cannot move what sits above.
        ops.sort_by(|a, b| edit_region(b).0.cmp(&edit_region(a).0));
        let mut lowest = usize::MAX;
        for op in &ops {
            let (start, old_lines, new_lines) = edit_region(op);
            lowest = lowest.min(start.line);
            self.highlighter_note_edit(op);
            record_layout_region(&mut self.pending_layout, start.line, old_lines, new_lines);
        }
        self.keep_highlights_before(lowest);
    }

    /// Walks the caret, the selection and the extra carets back into a document
    /// that has just shrunk. Undo and redo apply the inverse edit without
    /// touching the input state, so a caret parked past the end of the text that
    /// used to be there names a line that no longer exists — and the paint path
    /// reads it: [`Self::update_tag_highlight`] asks the document for the caret's
    /// line length. 字符统计 is the reproducible case, because it deletes the
    /// whole buffer and then writes its report into it.
    pub fn fit_input_to_document(&mut self) {
        let last = self.doc.line_count().saturating_sub(1);
        let fit = |line: usize, col: usize| {
            let line = line.min(last);
            (line, col.min(self.doc.line_len(line)))
        };
        self.caret = fit(self.caret.0, self.caret.1);
        for range in self.selection.ranges.iter_mut() {
            let start = fit(range.start.0, range.start.1);
            let end = fit(range.end.0, range.end.1);
            if end < start {
                range.start = end;
                range.end = start;
            } else {
                range.start = start;
                range.end = end;
            }
        }
        for extra in self.extra_carets.iter_mut() {
            *extra = fit(extra.0, extra.1);
        }
        // The tag highlight caches against the caret's line, so a moved caret has
        // to drop the key rather than reuse ranges from the text before the undo.
        self.tag_hl_key = None;
    }

    pub fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.undo.can_redo()
    }

    pub fn begin_macro(&mut self) {
        self.undo.begin_macro();
    }

    pub fn end_macro(&mut self) -> Option<MacroCommand> {
        self.undo.end_macro()
    }

    /// 撤销分组：打开到关闭之间的编辑落进同一条历史。一次多点编辑（每个光标
    /// 各删各的选区）就是这么一步 —— 删五处，撤销也得是一次按回来。
    pub fn begin_undo_group(&mut self) {
        self.undo.begin_group();
    }

    pub fn end_undo_group(&mut self) {
        self.undo.end_group()
    }

    pub fn play_macro(&mut self, macro_cmd: MacroCommand) -> bool {
        self.undo.play_macro(&mut self.doc, macro_cmd)
    }

    /// Replays recorded edit ops as
    /// one composite command. A fresh
    /// `MacroCommand` is minted on
    /// every call, so the same
    /// recording replays repeatedly.
    pub fn replay_macro(&mut self, ops: &[dg_core::EditOp]) -> bool {
        let Some(cmd) = self.undo.macro_from_ops(ops) else {
            return false;
        };
        self.play_macro(cmd)
    }

    /// Shifts bookmarks and 行注记 for an edit at
    /// `offset` that changed the line
    /// count by `delta`. An edit at the
    /// very start of a line moves the
    /// bookmarks on that line too; a
    /// mid-line edit leaves the line's
    /// own bookmark in place.
    fn shift_bookmarks(&mut self, at: (usize, usize), delta: isize) {
        if delta == 0 {
            return;
        }
        // An edit starting at col 0 shifts the anchor line's own
        // bookmark too (the new lines land before its content);
        // a mid-line edit only shifts the lines after it.
        let first = if at.1 == 0 { at.0 } else { at.0 + 1 };
        self.bookmarks.adjust(first, delta);
        self.line_tips.adjust(first, delta);
    }

    /// Sets the 行注记 on `line` (icon +
    /// tooltip text shown over the icon strip).
    pub fn line_tip_set(&mut self, line: usize, icon: u16, text: &str) {
        self.line_tips.set(line, icon, text);
    }

    /// Appends a paragraph to the 行注记 on
    /// `line`, creating it when absent.
    pub fn line_tip_append(&mut self, line: usize, icon: u16, text: &str) {
        self.line_tips.append(line, icon, text);
    }

    /// The 行注记 on `line`.
    pub fn line_tip(&self, line: usize) -> Option<&dg_core::LineTip> {
        self.line_tips.get(line)
    }

    /// Removes every 行注记 (下一个/上一个提示's
    /// clear-all companion).
    pub fn line_tip_clear_all(&mut self) {
        self.line_tips.clear();
    }

    /// First annotated line after `from`,
    /// wrapping (下一个提示).
    pub fn line_tip_next(&self, from: usize) -> Option<usize> {
        self.line_tips.next(from)
    }

    /// Last annotated line before `from`,
    /// wrapping (上一个提示).
    pub fn line_tip_prev(&self, from: usize) -> Option<usize> {
        self.line_tips.prev(from)
    }

    /// Ctrl+F2: toggle a bookmark on
    /// `line` (cm_marker_toggle).
    pub fn bookmark_toggle(&mut self, line: usize) -> bool {
        self.bookmarks.toggle(line)
    }

    /// First bookmark after `from`,
    /// wrapping to the first
    /// (cm_marker_next).
    pub fn bookmark_next(&self, from: usize) -> Option<usize> {
        self.bookmarks.next(from)
    }

    /// Last bookmark before `from`,
    /// wrapping to the last
    /// (cm_marker_prev).
    pub fn bookmark_prev(&self, from: usize) -> Option<usize> {
        self.bookmarks.prev(from)
    }

    /// Remove all bookmarks
    /// (cm_marker_clearall).
    pub fn bookmark_clear(&mut self) {
        self.bookmarks.clear();
    }

    /// Per-line bookmark flags for the
    /// gutter renderer.
    pub fn bookmark_flags(&self) -> Vec<bool> {
        self.bookmarks.flags(self.doc.line_count())
    }

    /// Toggles folding on the caret line and relayouts. 折叠/展开 — the fold
    /// marker's press and the keyboard's toggle, at the caret's line. A
    /// line with no region changes nothing.
    pub fn toggle_fold(&mut self, wrapper: &dyn Wrapper, ctx: &mut LayoutCtx) {
        let (line, _) = self.caret;
        self.toggle_fold_at(line, wrapper, ctx);
    }

    /// [`Self::toggle_fold`]'s body at an explicit line — the
    /// fold marker's press hit-tests its row and calls this with
    /// the line it landed on, which is why the toggle takes the
    /// line as a parameter and the caret never moves. Returns
    /// false when the line holds no region to toggle.
    pub fn toggle_fold_at(
        &mut self,
        line: usize,
        wrapper: &dyn Wrapper,
        ctx: &mut LayoutCtx,
    ) -> bool {
        let lines = self.doc.line_count();
        self.folding.ensure(lines);
        if self.folding.is_folded(line + 1) {
            // unfold: the run of hidden lines under the marker,
            // which for a fully collapsed region is the whole
            // region's inner span.
            let mut l = line + 1;
            while l < lines && self.folding.is_folded(l) {
                self.folding.set(l, false);
                l += 1;
            }
            self.relayout(wrapper, ctx);
            true
        } else if let Some((_, end)) = self
            .fold_regions()
            .into_iter()
            .find(|(start, _)| *start == line)
        {
            let end = end.min(lines.saturating_sub(1));
            for l in (line + 1)..=end {
                self.folding.set(l, true);
            }
            self.relayout(wrapper, ctx);
            true
        } else {
            false
        }
    }

    /// 折叠当前级别 / 展开当前级别: set the fold state of the smallest region that contains
    /// `line`, which is the level the tooltips name ( "Collapse/Expand current
    /// level"). False when the line is not inside a collapsible region, so the
    /// caller can leave the view alone.
    pub fn fold_level_at(
        &mut self,
        line: usize,
        folded: bool,
        wrapper: &dyn Wrapper,
        ctx: &mut LayoutCtx,
    ) -> bool {
        let Some((start, end)) = self
            .fold_regions()
            .into_iter()
            .filter(|(s, e)| *s <= line && line <= *e)
            .min_by_key(|(s, e)| e - s)
        else {
            return false;
        };
        for l in (start + 1)..=end {
            self.folding.set(l, folded);
        }
        self.relayout(wrapper, ctx);
        true
    }

    /// Fold All / Expand All: every region at once, as opposed to, which stop
    /// at the caret's own level.
    pub fn fold_all(&mut self, folded: bool, wrapper: &dyn Wrapper, ctx: &mut LayoutCtx) {
        let regions = self.fold_regions();
        self.folding.ensure(self.doc.line_count());
        for (start, end) in regions {
            for line in (start + 1)..=end {
                self.folding.set(line, folded);
            }
        }
        self.relayout(wrapper, ctx);
    }

    /// Fold Level 2 … Fold Level 9 (`cm_folding_levelN`, the last ids of the
    /// folding group): collapsing to a level folds every block nested that deeply or
    /// deeper. The rows establish the view rather than
    /// layering on it, so earlier folds are dropped first. Depth counts the regions that
    /// contain a region; both derivations nest (a block inside a function is a region
    /// inside a region), so level 1 folds everything and level 2 only the inner blocks.
    pub fn fold_to_level(&mut self, level: usize, wrapper: &dyn Wrapper, ctx: &mut LayoutCtx) {
        let regions = self.fold_regions();
        self.folding.clear();
        self.folding.ensure(self.doc.line_count());
        for &(start, end) in &regions {
            let depth = regions
                .iter()
                .filter(|(s, e)| *s < start && *e >= end)
                .count()
                + 1;
            if depth < level {
                continue;
            }
            for line in (start + 1)..=end {
                self.folding.set(line, true);
            }
        }
        self.relayout(wrapper, ctx);
    }

    /// Switching derivation re-derives the markers. Cancelling folding also drops the folded
    /// lines, since no region contains them any more.
    pub fn set_folding_method(
        &mut self,
        method: FoldingMethod,
        wrapper: &dyn Wrapper,
        ctx: &mut LayoutCtx,
    ) {
        self.folding_method = method;
        if method == FoldingMethod::None {
            self.folding.clear();
        }
        self.folding.ensure(self.doc.line_count());
        self.relayout(wrapper, ctx);
    }

    /// `Edit/QuickCase` (default 1): whether
    /// the selection highlight's occurrences match
    /// case-sensitively.
    pub fn set_sel_match_case(&mut self, on: bool) {
        self.sel_match_case = on;
    }

    pub fn sel_match_case(&self) -> bool {
        self.sel_match_case
    }

    /// `Edit/QuickSmart` (default 0): the
    /// whole-word gate on the selection highlight.
    pub fn set_sel_match_word(&mut self, on: bool) {
        self.sel_match_word = on;
    }

    pub fn sel_match_word(&self) -> bool {
        self.sel_match_word
    }

    /// `Edit/MaxSelHighlight` (default 64): the length above which a
    /// selection stops producing an occurrence highlight.
    pub fn set_sel_max_len(&mut self, len: usize) {
        self.sel_max_len = len.clamp(1, 1024);
    }

    pub fn sel_max_len(&self) -> usize {
        self.sel_max_len
    }

    /// `Edit/KeepHighlight`（选项 → 编辑页，出厂 N）：选区塌掉后留着上一次的
    /// 「选中词全文高亮」，还是跟着一起清。
    pub fn set_keep_highlight(&mut self, on: bool) {
        self.keep_highlight = on;
    }

    pub fn keep_highlight(&self) -> bool {
        self.keep_highlight
    }

    /// `Edit/ScrollLimit`（选项 → 编辑页，0~16，出厂 0）：翻页留给自己的
    /// 上下文行数。界面上限是 16，写超了归 0 —— 和选项页读进来时同一套钳位。
    pub fn set_scroll_limit(&mut self, lines: usize) {
        self.scroll_limit = if lines > 16 { 0 } else { lines };
    }

    pub fn scroll_limit(&self) -> usize {
        self.scroll_limit
    }

    /// Headless end-to-end check: load, layout,
    /// paint. Returns true when the draw list is
    /// non-empty.
    /// Fold markers: attaching the JSON grammar
    /// to an object literal must produce a `+`
    /// marker on the opening line.
    ///
    /// 只在测试里用 —— grammar 不再随二进制分发（`parsers\` 才是来源），
    /// 这里直接借 dev-dependency 的 json grammar。
    #[cfg(test)]
    pub fn folding_marker_test() -> bool {
        let sample = "{\"a\": 1,\n\"b\": [2, 3]\n}\n";
        let mut editor = Editor::new();
        editor.open_text(sample);
        let lang = tree_sitter::Language::from(tree_sitter_json::LANGUAGE);
        editor.attach_syntax(lang);
        let starts = editor.folding_starts();
        if starts.is_empty() {
            return false;
        }
        starts[0]
    }

    pub fn selftest() -> bool {
        let sample = "fn main() {\n    let x = 42;\n    println!(\"{}\", x);\n}\n";
        let mut editor = Editor::new();
        editor.open_text(sample);
        let mut measurer =
            dg_layout::measure::CachedMeasurer::new(dg_layout::measure::TableMeasurer::default());
        let wrapper = dg_layout::wrap::WinEdgeWrapper {
            width_px: 400,
            word_wrap: true,
        };
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        editor.relayout(&wrapper, &mut ctx);
        if editor.total_rows < 4 {
            eprintln!(
                "selftest: relayout produced {} rows (want >= 4)",
                editor.total_rows
            );
            return false;
        }
        let mut draw = DrawList::default();
        editor.paint(
            24,
            dg_render::component::Rect::new(0.0, 0.0, 800.0, 400.0),
            800.0,
            &mut draw,
            0.0,
            HighlightToggles::default(),
        );
        if draw.cmds.is_empty() {
            eprintln!("selftest: paint produced no commands");
            return false;
        }
        if !ui_selftest() {
            eprintln!("selftest: ui_selftest failed");
            return false;
        }
        if !parser_download_selftest() {
            eprintln!("selftest: parser_download_selftest failed");
            return false;
        }
        true
    }
}

/// 解析器下载链路的冒烟（「解析器下载」功能）：注册表解析、ABI 过滤、
/// 版本比较 —— 纯数据层，不需要网络与窗口。
fn parser_download_selftest() -> bool {
    let doc = match dg_syntax::RegistryDoc::parse(
        r#"{"schema":1,"updated":"","packages":[
            {"name":"demo","display":"Demo","version":"1.2.0","extensions":["dm"],"abi":[14,15],
             "size":10,"sha256":"0000000000000000000000000000000000000000000000000000000000000000",
             "path":"pkg/demo/1.2.0.zip"}]}"#,
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("selftest: registry parse failed: {e}");
            return false;
        }
    };
    let host = (
        tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION as u32,
        tree_sitter::LANGUAGE_VERSION as u32,
    );
    if doc.candidates_for_ext("dm", host).len() != 1 {
        eprintln!("selftest: registry ext filter lost the candidate");
        return false;
    }
    if !doc.candidates_for_ext("nomatch", host).is_empty() {
        eprintln!("selftest: registry ext filter leaked a non-candidate");
        return false;
    }
    if dg_syntax::remote::version_cmp("1.10", "1.2") != Some(std::cmp::Ordering::Greater) {
        eprintln!("selftest: dotted version compare is wrong");
        return false;
    }
    true
}

/// UI model smoke check (U5):
/// Headless check of the models that are
/// still shared with the other backends: the
/// dock partitioning, the overlay scrollbar
/// and the status bar's pane table. Panels
/// that have a window of their own (the
/// directory tree, the results list) are
/// exercised by the platform backend instead.
pub fn ui_selftest() -> bool {
    use dg_render::Rect;
    use dg_render::theme::Theme;
    use dg_ui::event::{Event, MouseButton, Point, mods};
    use dg_ui::menu::MenuItem;
    use dg_ui::panel::{DockPanel, PanelSide};
    use dg_ui::scrollbar::{FADE_H, FADE_V, Metrics, Scrollbar};
    use dg_ui::statusbar::{
        EditMode, Eol, Selection, StatusBar, StatusField, StatusModel, encoding_label, format_size,
        pane_width,
    };
    use dg_ui::widget::{Action, HostCmd, WidgetCtx};

    // Every failing branch names its line, so `--selftest` reports *which*
    // check broke instead of just "FAILED" — this function is the one place
    // where a bare `false` used to be undiagnosable.
    let fail = |line: u32| {
        eprintln!("ui_selftest: check failed at app.rs:{line}");
        false
    };

    /// Counts host hook calls so the
    /// scrollbar fade contract
    /// is observable headlessly.
    struct ProbeCtx {
        timers: u32,
        last_id: u64,
    }
    impl WidgetCtx for ProbeCtx {
        fn set_timer(&mut self, _id: u64, _ms: u32) {
            self.timers += 1;
            self.last_id = _id;
        }
        fn kill_timer(&mut self, _id: u64) {}
        fn host_command(&mut self, _cmd: HostCmd) -> Action {
            Action::NONE
        }
    }

    let theme = Theme::default();

    // DockPanel: the overlay bars live *inside* the editor
    // rect — they cover text instead of taking a column off
    // it — while the docks and the status strip still
    // partition the client without overlap.
    let mut dock = DockPanel::default();
    // The host derives the strip thickness from the theme (`panels::layout` →
    // `OverlayBars::strip`). Without that step the dock keeps its placeholder
    // 18 px and the theme/geometry cross-checks below compare two unrelated
    // numbers — which is exactly how this selftest started failing when the
    // pill rework changed the theme's strip to 12.
    dock.scrollbar_w = theme.scrollbar_strip_width.max(theme.scrollbar_thumb_width);
    dock.scrollbar_h = dock.scrollbar_w;
    dock.left_panel.visible = true;
    dock.bottom_panel.visible = true;
    let client = Rect::new(0.0, 0.0, 1200.0, 800.0);
    let ed = dock.editor_rect(client);
    let left = dock.panel_rect(PanelSide::Left, client);
    let bottom = dock.panel_rect(PanelSide::Bottom, client);
    let status = dock.status_rect(client);
    let vscroll = dock.vscroll_rect(client);
    let hscroll = dock.hscroll_rect(client);
    if (ed.x - left.w).abs() > 0.01 || (ed.w - (client.w - left.w)).abs() > 0.01 {
        return fail(line!());
    }
    // each strip is flush with the edge it overlays
    if (vscroll.x + vscroll.w - (ed.x + ed.w)).abs() > 0.01
        || (hscroll.x + hscroll.w - (ed.x + ed.w)).abs() > 0.01
        || (hscroll.y + hscroll.h - (ed.y + ed.h)).abs() > 0.01
    {
        return fail(line!());
    }
    //...and never reaches past the editor into the docks
    if vscroll.y < ed.y
        || vscroll.y + vscroll.h > ed.y + ed.h
        || hscroll.x < ed.x
        || hscroll.y < ed.y
    {
        return fail(line!());
    }
    if hscroll.y + hscroll.h > bottom.y
        || bottom.y + bottom.h > status.y
        || status.y + status.h > client.h
    {
        return fail(line!());
    }

    // Scrollbar (overlay model, window-local coords): an
    // empty model has no bar at all; a scrollable one is
    // always on screen as a fixed pill; revealing arms the
    // fade and engages whole-strip hit testing without
    // reshaping anything; a track click jumps and starts the
    // drag; the thumb drag reports value changes; releasing
    // re-arms the fade; the tick disengages again.
    let mut sb = Scrollbar::new(true);
    sb.set_metrics(Metrics {
        width: theme.scrollbar_thumb_width,
        strip: theme.scrollbar_strip_width,
        fade_ms: theme.scrollbar_fade_ms,
    });
    let strip_w = theme.scrollbar_strip_width.max(theme.scrollbar_thumb_width);
    sb.layout(Rect::new(0.0, 0.0, strip_w, 600.0));
    if sb.visible() || sb.engaged() {
        return fail(line!());
    }
    if sb.set_model(0, 100, 50) {
        return fail(line!());
    }
    // something to scroll => a pill on screen, not a hidden bar
    if !sb.visible() || sb.pos() != 0 || sb.engaged() {
        return fail(line!());
    }
    if (sb.pill_width() - theme.scrollbar_thumb_width).abs() > 0.01 {
        return fail(line!());
    }
    let mut probe = ProbeCtx {
        timers: 0,
        last_id: 0,
    };
    if !sb.reveal(&mut probe) || !sb.engaged() {
        return fail(line!());
    }
    if probe.last_id != FADE_V {
        return fail(line!());
    }
    // engagement is hit state only: the pill keeps its shape
    if (sb.pill_width() - theme.scrollbar_thumb_width).abs() > 0.01 {
        return fail(line!());
    }
    if (vscroll.w - strip_w).abs() > 0.01 {
        return fail(line!());
    }
    // click below the thumb (track is 600 long, thumb 200
    // at the top): jump to the click and follow the drag
    let a = sb.on_event(
        Event::MouseDown {
            pos: Point::new(9.0, 400.0),
            button: MouseButton::Left,
            mods: mods::NONE,
        },
        &mut probe,
    );
    if !a.value_changed || !a.consumed || sb.pos() != 75 {
        return fail(line!());
    }
    let a = sb.on_event(
        Event::MouseMove {
            pos: Point::new(9.0, 300.0),
        },
        &mut probe,
    );
    if !a.value_changed || sb.pos() != 50 || !sb.dragging() {
        return fail(line!());
    }
    let before = probe.timers;
    let a = sb.on_event(
        Event::MouseUp {
            pos: Point::new(9.0, 300.0),
            button: MouseButton::Left,
            mods: mods::NONE,
        },
        &mut probe,
    );
    if !a.repaint || sb.dragging() || probe.timers <= before {
        return fail(line!());
    }
    // the fade is per axis: the horizontal bar's id must
    // leave this one alone
    let a = sb.on_event(Event::Timer { id: FADE_H }, &mut probe);
    if a.repaint || !sb.engaged() {
        return fail(line!());
    }
    let a = sb.on_event(Event::Timer { id: FADE_V }, &mut probe);
    if !a.repaint || sb.engaged() {
        return fail(line!());
    }
    // resting again: only the pill is the bar's, and the rest
    // of the strip is still the text's — which is the whole
    // point of not hiding the bar. The pill is centred in the
    // strip (pos 50 puts the thumb over y 200..400).
    if !sb.interactive_at(Point::new(6.0, 300.0)) {
        return fail(line!());
    }
    if sb.interactive_at(Point::new(6.0, 500.0)) {
        return fail(line!());
    }
    let a = sb.on_event(
        Event::Wheel {
            delta: -120,
            horiz: true,
            pos: Point::new(9.0, 300.0),
            mods: mods::NONE,
        },
        &mut probe,
    );
    if a.value_changed || sb.pos() != 50 {
        return fail(line!());
    }
    let a = sb.on_event(
        Event::Wheel {
            delta: -120,
            horiz: false,
            pos: Point::new(9.0, 300.0),
            mods: mods::NONE,
        },
        &mut probe,
    );
    if !a.value_changed || sb.pos() != 53 || !sb.engaged() {
        return fail(line!());
    }
    // geometry the host paints: the pill stays inside the window, centred
    let tr = sb.thumb_rect();
    if tr.x + tr.w > sb.rect().w
        || tr.y + tr.h > sb.rect().h
        || tr.h < 20.0
        || (tr.w - sb.pill_width()).abs() > 0.01
    {
        return fail(line!());
    }
    if !sb.interactive_at(Point::new(6.0, tr.y + tr.h / 2.0)) {
        return fail(line!());
    }

    // Horizontal bar: same model, transposed.
    let mut hs = Scrollbar::new(false);
    hs.layout(Rect::new(0.0, 0.0, 1200.0, strip_w));
    hs.set_model(60, 200, 80);
    hs.reveal(&mut probe);
    if probe.last_id != FADE_H || !hs.engaged() {
        return fail(line!());
    }
    let tr = hs.thumb_rect();
    if tr.x < 0.0
        || tr.y < 0.0
        || tr.y + tr.h > hs.rect().h + 0.01
        || (tr.h - hs.pill_width()).abs() > 0.01
    {
        return fail(line!());
    }
    let a = hs.on_event(
        Event::Wheel {
            delta: 120,
            horiz: true,
            pos: Point::new(600.0, 9.0),
            mods: mods::NONE,
        },
        &mut probe,
    );
    if !a.value_changed || hs.pos() != 57 {
        return fail(line!());
    }
    // a document that shrank under the thumb pulls it back
    if !hs.set_model(500, 40, 80) || hs.pos() != 40 {
        return fail(line!());
    }

    // StatusBar: the pane table a native `msctls_statusbar32` is
    // filled with (geometry, hit testing and painting are the
    // control's now).
    let mut bar = StatusBar::new();
    let model = StatusModel {
        message: None,
        line: Some(2),
        col: Some(0),
        char_value: None,
        line_count: Some(3),
        encoding: Some(encoding_label("UTF-8", false)),
        edit_mode: Some(EditMode::Insert),
        eol: Some(Eol::Lf),
        tab_size: Some(4),
        soft_tab: false,
        syntax: Some("Rust".to_string()),
        selection: Some(Selection { chars: 0, lines: 0 }),
        sync: false,
        file_size: Some(12),
        hex: None,
    };
    if !bar.set_model(&model) {
        return fail(line!());
    }
    // The same model again is not a change: this is what keeps an idle paint
    // from re-sending the pane table to the control.
    if bar.set_model(&model) {
        return fail(line!());
    }
    let panes = bar.panes();
    if panes.len() != 13 {
        return fail(line!());
    }
    let texts: Vec<&str> = panes.iter().map(|p| p.text.as_str()).collect();
    if texts
        != [
            "就绪",
            "12 B",
            "",
            "",
            "Rust",
            "制表符:4",
            "行 2",
            "列 0",
            "",
            "3",
            "LF",
            "插入",
            "UTF-8",
        ]
    {
        return fail(line!());
    }
    // The ids the control reports back on a click
    let ids: Vec<u32> = panes.iter().map(|p| p.id).collect();
    if ids != [0, 12, 11, 10, 9, 8, 1, 2, 3, 4, 7, 6, 5] {
        return fail(line!());
    }
    if panes.iter().any(|p| p.id != p.field.pane_id()) {
        return fail(line!());
    }
    for field in [
        StatusField::Ln,
        StatusField::Col,
        StatusField::LineCount,
        StatusField::EditMode,
        StatusField::Eol,
        StatusField::TabSize,
        StatusField::Syntax,
        StatusField::Selection,
        StatusField::Sync,
        StatusField::FileSize,
    ] {
        if StatusField::from_pane_id(field.pane_id()) != Some(field) {
            return fail(line!());
        }
    }
    // the status-pane width rule: 4 px of padding, 36 more on the icon
    // panes, floor 45.
    let rules = dg_ui::statusbar::WidthRules::DEFAULT;
    if pane_width(0, 0, rules) != 45
        || pane_width(10, 8, rules) != 46
        || pane_width(10, 7, rules) != 46
        || pane_width(42, 1, rules) != 46
    {
        return fail(line!());
    }
    if format_size(297_696) != "290.7 KB" {
        return fail(line!());
    }
    if encoding_label("windows-1252", false) != "ANSI" || encoding_label("UTF-8", true) != "UTF-8+"
    {
        return fail(line!());
    }

    // MenuItem: the rows a popup is built
    // from. A menu draws itself natively on
    // every platform, so the shared model is
    // only the list of rows and their state.
    let items = vec![
        MenuItem::new(1, "Open"),
        MenuItem::separator(),
        MenuItem::new(5, "Delete").with_enabled(false),
        MenuItem::new(6, "Word Wrap").with_checked(true),
    ];
    if items.len() != 4 {
        return fail(line!());
    }
    // A separator carries no command and no text, and the builders have to
    // touch the flag they name and nothing else.
    if !items[1].separator || items[1].enabled || !items[1].label.is_empty() || items[1].id != 0 {
        return fail(line!());
    }
    if items[0].separator
        || items[0].enabled != true
        || items[0].checked
        || items[2].enabled
        || items[3].enabled != true
        || !items[3].checked
    {
        return fail(line!());
    }

    true
}

/// Identifier-like character test from
/// `-`, `_` or any alphanumeric.
fn is_tag_char(ch: char) -> bool {
    ch == '-' || ch == '_' || ch.is_alphanumeric()
}

/// The hex view's palette from a parsed theme — the refill of the
/// HexView32 colour table, resolved slot by slot against
/// the shipped default table (Evening's values) and the Black theme's
/// runtime look. Slot list and their meaning live in `dg-hex`'s `Colors`;
/// the themes carry no hex-specific keys, the view draws with the standard
/// ones:
/// - window/addresses background ← `Default` bg
/// - hex bytes and the ASCII column ← `Default` fg (screenshot-verified:
/// both bands sample #D4D7DD under the dark theme)
/// - odd groups of bytes ← `Word1` fg (the dark theme's #C792EA purple)
/// - undo-run foreground ← `Selection` fg, background ← `Selection` bg
/// - address text ← `LineNumber` fg
/// - modified bytes ← `WaveLine`
/// - the address-band paint branch ← `IndentLine`; search-flash ← `Found` bg
///
/// Keys the theme does not carry keep the value they already had (the
/// ctor defaults or the previous theme's)
/// theme variables that a partial theme leaves alone.
fn hex_colors_from_theme(file: &ThemeFile) -> dg_hex::Colors {
    fn colorref(c: Color) -> u32 {
        // GDI COLORREF is 0x00bbggrr.
        ((c.b as u32) << 16) | ((c.g as u32) << 8) | c.r as u32
    }
    let mut colors = dg_hex::Colors::default();
    if let Some(c) = file.bg("Default") {
        colors.table[0] = colorref(c);
        colors.address_bg = colorref(c);
    }
    if let Some(c) = file.fg("Default") {
        colors.table[4] = colorref(c);
        colors.table[10] = colorref(c);
    }
    if let Some(c) = file.fg("Word1") {
        colors.table[7] = colorref(c);
    }
    if let Some(c) = file.fg("Selection") {
        colors.table[12] = colorref(c);
        colors.undo_fg = colorref(c);
    }
    if let Some(c) = file.bg("Selection") {
        colors.table[11] = colorref(c);
        colors.undo_bg = colorref(c);
    }
    if let Some(c) = file.fg("LineNumber") {
        colors.table[3] = colorref(c);
    }
    if let Some(c) = file.bg("WaveLine").or_else(|| file.fg("WaveLine")) {
        colors.modified_fg = colorref(c);
        colors.table[13] = colorref(c);
    }
    if let Some(c) = file.bg("IndentLine").or_else(|| file.fg("IndentLine")) {
        colors.table[19] = colorref(c);
    }
    if let Some(c) = file.bg("Found") {
        colors.table[23] = colorref(c);
    }
    colors
}

/// Maps the theme's syntax color keys
/// (`Comment1`, `String1`, `Number`,
/// `Word1`, `Function`, `Type`, `Var`,
/// `Operator`, `Tag`, `Macro`) onto the
/// tree-sitter kind table. Keys the
/// theme file does not carry keep the
/// built-in defaults.
fn syntax_config_from_theme(file: &ThemeFile) -> dg_syntax::ThemeConfig {
    let mut cfg = dg_syntax::ThemeConfig::default();
    // Ordinary text is the color most of the buffer is, and
    // `ThemeConfig::default()` carries a compiled constant:
    // leaving it meant every theme drew plain text in the same
    // color, so switching themes only moved the keywords.
    if let Some(c) = file.fg("Default") {
        cfg.default_color = c.to_u32();
    }
    let mapping: [(&str, &str); 10] = [
        ("Comment1", "comment"),
        ("String1", "string"),
        ("Number", "number"),
        ("Word1", "keyword"),
        ("Function", "function"),
        ("Type", "type"),
        ("Var", "property"),
        ("Operator", "operator"),
        ("Tag", "tag"),
        ("Macro", "macro"),
    ];
    for (key, kind) in mapping {
        let Some(color) = file.fg(key) else {
            continue;
        };
        if let Some(slot) = cfg.rules.iter_mut().find(|(k, _)| *k == kind) {
            slot.1 = color.to_u32();
        } else {
            cfg.rules.push((kind, color.to_u32()));
        }
    }
    // 字形：`[Style]` 的同一个键，低两位是粗体 / 斜体。颜色和字形是**两条
    // 独立的表**（`rules` 与 `style_rules`），因为一条规则可以有颜色没字形；
    // 但键是同一个，所以注释不会出现「颜色来自 Comment1、字形来自 Comment2」
    // 这种错位。没写这一行 = 正体，和没有 `[Style]` 段时一样。
    const STYLE_KEYS: [(&str, &str); 11] = [
        ("Comment1", "comment"),
        ("Comment2", "comment"),
        ("String1", "string"),
        ("Number", "number"),
        ("Word1", "keyword"),
        ("Function", "function"),
        ("Type", "type"),
        ("Var", "property"),
        ("Operator", "operator"),
        ("Tag", "tag"),
        ("Macro", "macro"),
    ];
    for (key, kind) in STYLE_KEYS {
        let Some(bits) = file.style(key) else {
            continue;
        };
        // 低八位是字形（粗/斜/框/删/下/双/点/波），高位的来历见
        // `dg_syntax::STYLE_BOLD` 上面那张表 —— 认不出来的位留着不动。
        let style = (bits & 0xFF) as u8;
        // 同一个 kind 只认第一条（Comment1 先于 Comment2）：两条都写了字形
        // 时按文件顺序取前者，和后出现的覆盖先出现的颜色规则相反 —— 颜色
        // 是"最后写的说了算"，字形是"最具体的说了算"。
        if style == 0 || cfg.style_rules.iter().any(|(k, _)| *k == kind) {
            continue;
        }
        cfg.style_rules.push((kind, style));
    }
    // query 路径（`highlights.scm` 的 capture）用同一套色板：每个
    // capture 前缀跟着它对应的 kind 规则走，换主题时两条路径不会漂。
    const CAPTURE_SOURCES: [(&str, &str); 18] = [
        ("comment", "comment"),
        ("string", "string"),
        ("string.escape", "string"),
        ("constant.numeric", "number"),
        ("constant", "keyword"),
        ("keyword", "keyword"),
        ("function", "function"),
        ("function.macro", "function"),
        ("label", "function"),
        ("type", "type"),
        ("constructor", "type"),
        ("namespace", "type"),
        ("variable", "property"),
        ("variable.other.member", "property"),
        ("variable.parameter", "property"),
        ("attribute", "property"),
        ("tag", "tag"),
        ("operator", "operator"),
    ];
    for (capture, kind) in CAPTURE_SOURCES {
        let Some((_, color)) = cfg.rules.iter().find(|(k, _)| *k == kind) else {
            continue;
        };
        let color = *color;
        if let Some(slot) = cfg.capture_rules.iter_mut().find(|(c, _)| c == capture) {
            slot.1 = color;
        } else {
            cfg.capture_rules.push((capture.to_string(), color));
        }
    }
    // 字形走同一张 capture 表：`@comment` 和 `@comment.documentation` 都落到
    // "comment" 那条规则上，最长前缀仍然由 `capture_style` 决定。
    for (capture, kind) in CAPTURE_SOURCES {
        let Some((_, style)) = cfg.style_rules.iter().find(|(k, _)| *k == kind) else {
            continue;
        };
        let style = *style;
        if let Some(slot) = cfg.capture_styles.iter_mut().find(|(c, _)| c == capture) {
            slot.1 = style;
        } else {
            cfg.capture_styles.push((capture.to_string(), style));
        }
    }
    cfg
}

#[cfg(test)]
mod edit_history_tests {
    use super::*;

    /// 没编辑过的文档：无可走，命令返回 false 的那一路。
    #[test]
    fn an_untouched_document_has_nowhere_to_walk() {
        let mut ed = Editor::new();
        ed.open_text("text");
        assert_eq!(ed.edit_history_move(false), None);
        assert_eq!(ed.edit_history_move(true), None);
    }

    /// 两处编辑：上一个编辑位置走回更早的那处，下一个再走回来。
    #[test]
    fn the_edit_ring_walks_between_two_edit_sites() {
        let mut ed = Editor::new();
        ed.open_text("l0\nl1\nl2\nl3\nl4\nl5");
        assert!(ed.apply_insert(Pos { line: 0, col: 2 }, "A"));
        assert!(ed.apply_insert(Pos { line: 5, col: 2 }, "B"));
        assert_eq!(ed.edit_history_move(false), Some((0, 3)));
        assert_eq!(ed.caret, (0, 3));
        assert_eq!(ed.edit_history_move(true), Some((5, 3)));
        assert_eq!(ed.caret, (5, 3));
    }

    /// 连打的每一键都在同一行：折叠成一条，落点是最后一键的结尾；
    /// 跳转不落账，来回走停在同一条上。
    #[test]
    fn a_typing_run_collapses_into_one_entry() {
        let mut ed = Editor::new();
        ed.open_text("");
        for col in 0..5 {
            assert!(ed.apply_insert(Pos { line: 0, col }, "x"));
        }
        assert_eq!(ed.edit_history_move(false), Some((0, 5)));
        assert_eq!(ed.edit_history_move(true), Some((0, 5)));
    }

    /// 同行的再次落笔刷新旧条目并把它挪到最新：环按新旧排序，游标
    /// 回到最新。
    #[test]
    fn a_later_edit_on_the_same_line_refreshes_the_entry() {
        let mut ed = Editor::new();
        ed.open_text("l0\nl1\nl2");
        assert!(ed.apply_insert(Pos { line: 0, col: 2 }, "A"));
        assert!(ed.apply_insert(Pos { line: 2, col: 2 }, "B"));
        // 回到第 0 行再落笔：第 0 行的条目刷新成新落点，且变成最新。
        assert!(ed.apply_insert(Pos { line: 0, col: 3 }, "C"));
        assert_eq!(ed.edit_history_move(false), Some((2, 3)));
        // 从最旧一条再 prev，回绕回最新。
        assert_eq!(ed.edit_history_move(false), Some((0, 4)));
    }

    /// 两端回绕：从最新一路 prev 到最旧，再 prev 回绕回最新；next
    /// 同样绕回。
    #[test]
    fn the_walk_wraps_at_both_ends() {
        let mut ed = Editor::new();
        ed.open_text("l0\nl1\nl2");
        assert!(ed.apply_insert(Pos { line: 0, col: 2 }, "A"));
        assert!(ed.apply_insert(Pos { line: 1, col: 2 }, "B"));
        assert!(ed.apply_insert(Pos { line: 2, col: 2 }, "C"));
        assert_eq!(ed.edit_history_move(false), Some((1, 3)));
        assert_eq!(ed.edit_history_move(false), Some((0, 3)));
        assert_eq!(ed.edit_history_move(false), Some((2, 3)));
        assert_eq!(ed.edit_history_move(true), Some((0, 3)));
    }

    /// 撤销与重做是历史导航：光标动了，环不动。
    #[test]
    fn undo_and_redo_do_not_record_an_edit_position() {
        let mut ed = Editor::new();
        ed.open_text("l0\nl1");
        assert!(ed.apply_insert(Pos { line: 0, col: 2 }, "A"));
        assert!(ed.apply_insert(Pos { line: 1, col: 2 }, "B"));
        ed.undo();
        assert_eq!(ed.edit_history_move(false), Some((0, 3)), "撤销不落账");
        ed.redo();
        assert_eq!(ed.edit_history_move(true), Some((1, 3)), "重做同样不落账");
    }

    /// 删除的落点是删除的起点（撤销重做侧推光标的同一口径）。
    #[test]
    fn a_delete_records_where_it_started() {
        let mut ed = Editor::new();
        ed.open_text("abcd\nefgh");
        assert!(ed.apply_delete(Pos { line: 0, col: 1 }, Pos { line: 0, col: 3 }));
        assert_eq!(ed.edit_history_move(false), Some((0, 1)));
    }

    /// 环满十条后最旧的被挤出去。
    #[test]
    fn the_ring_caps_at_ten_entries() {
        let mut ed = Editor::new();
        let text: String = (0..12).map(|i| format!("line{i}\n")).collect();
        ed.open_text(&text);
        for i in 0..12 {
            assert!(ed.apply_insert(Pos { line: i, col: 0 }, "x"));
        }
        // 最旧两条（第 0、1 行）被挤出环：一路 prev 走到底，最旧是第 2 行。
        let mut last = None;
        for _ in 0..9 {
            last = ed.edit_history_move(false);
        }
        assert_eq!(last, Some((2, 1)));
        // 再 prev 从最旧回绕回最新。
        assert_eq!(ed.edit_history_move(false), Some((11, 1)));
    }
}

#[cfg(test)]
mod undo_tests {
    use super::*;
    use dg_layout::measure::TableMeasurer;
    use dg_layout::wrap::WinEdgeWrapper;

    fn laid_out(text: &str) -> Editor {
        let mut editor = Editor::new();
        editor.open_text(text);
        let mut measurer = dg_layout::measure::CachedMeasurer::new(TableMeasurer::default());
        let wrapper = WinEdgeWrapper {
            width_px: 400,
            word_wrap: true,
        };
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        editor.relayout(&wrapper, &mut ctx);
        editor
    }

    #[test]
    fn insert_delete_undo_redo_roundtrip() {
        let mut editor = laid_out("hello");
        assert!(editor.apply_insert(Pos { line: 0, col: 5 }, " world"));
        assert_eq!(editor.doc.to_text(), "hello world");
        assert!(editor.apply_delete(Pos::default(), Pos { line: 0, col: 6 }));
        assert_eq!(editor.doc.to_text(), "world");
        // undo delete, then undo insert
        assert!(editor.undo());
        assert_eq!(editor.doc.to_text(), "hello world");
        assert!(editor.undo());
        assert_eq!(editor.doc.to_text(), "hello");
        // redo both
        assert!(editor.redo());
        assert_eq!(editor.doc.to_text(), "hello world");
        assert!(editor.redo());
        assert_eq!(editor.doc.to_text(), "world");
    }

    #[test]
    fn dirty_flag_tracks_the_save_point() {
        let mut editor = laid_out("hello");
        assert!(!editor.is_modified());
        assert!(editor.apply_insert(Pos { line: 0, col: 5 }, "!"));
        assert!(editor.is_modified());
        editor.mark_saved();
        assert!(!editor.is_modified());
        // editing again makes it dirty, and undoing back to the
        // save point clears it — the flag is the history position,
        // not a latch.
        assert!(editor.apply_replace(Pos::default(), Pos { line: 0, col: 5 }, "H"));
        assert!(editor.is_modified());
        editor.mark_saved();
        assert!(!editor.is_modified());
        assert!(editor.undo());
        assert!(editor.is_modified());
        assert!(editor.redo());
        assert!(!editor.is_modified());
    }

    /// Undo applies the inverse edit without touching the input state, so a caret
    /// or selection naming text the undo removed points at lines that are gone —
    /// which is what crashed the paint after 字符统计 → 撤销.
    #[test]
    fn undo_walks_the_input_back_into_a_shrunk_document() {
        let mut editor = laid_out("ab cd\n漢字 x\n🎋");
        let last = editor.doc.line_count().saturating_sub(1);
        let all_end = Pos {
            line: last,
            col: editor.doc.line_len(last),
        };
        assert!(editor.apply_delete(Pos::default(), all_end));
        assert!(editor.apply_insert(Pos::default(), "report\nlines\nhere\n"));
        // Where the 字符统计 command leaves the input state: in the report it wrote.
        editor.caret = (3, 0);
        editor.selection.ranges.push(dg_core::Range {
            start: (2, 0),
            end: (3, 0),
        });
        assert!(editor.undo());
        assert_eq!(editor.doc.line_count(), 1);
        assert_eq!(editor.caret, (0, 0));
        for range in &editor.selection.ranges {
            assert!(range.start.0 <= range.end.0);
            assert!(range.end.1 <= editor.doc.line_len(range.end.0));
        }
        // The paint path that read the caret's line is the thing that panicked.
        editor.update_tag_highlight();
    }

    /// 快照模式的挂载要把字节长度底数带上：挂载之前就在历史里的编辑，撤销
    /// 退回它们时反向记账不能把 `doc_len` 减穿。此前挂载从 0 起步，撤销一
    /// 旦退过挂载点，`edit_at` 的字节算术直接下溢 —— 在窗口过程里 panic
    /// 就是整个进程崩掉。
    #[test]
    fn snapshot_attach_carries_the_byte_baseline_into_undo() {
        let base = "fn main() {\n    let x = 1;\n}\n".repeat(300);
        let mut editor = Editor::new();
        editor.open_text(&base);
        // 挂载之前的历史：先落两笔编辑 —— 第二笔故意带与文档不同的行尾
        // （LF 文档里插 CRLF），存储按插入文本自己的行尾种类落账，撤销的
        // 反向删除也要收走同样的字节数，记账才不会漂。
        assert!(editor.apply_insert(Pos { line: 0, col: 0 }, "abc"));
        assert!(editor.apply_insert(Pos { line: 0, col: 3 }, "\r\n"));
        let lang = tree_sitter::Language::from(tree_sitter_json::LANGUAGE);
        editor.attach_syntax_snapshot(lang, None, None);
        let dl = |ed: &Editor| {
            ed.highlighter
                .as_ref()
                .map(|h| h.doc_len())
                .unwrap_or(usize::MAX)
        };
        let total = editor.doc.total_bytes();
        assert_eq!(dl(&editor), total, "挂载即对上文档字节数");
        // 挂载之后再打字，然后一路撤销退过挂载点：每一步 undo 的反向记账
        // 都要和文档的真实字节数一致。
        assert!(editor.apply_insert(Pos { line: 1, col: 0 }, "d"));
        assert_eq!(dl(&editor), editor.doc.total_bytes());
        while editor.undo() {
            assert_eq!(
                dl(&editor),
                editor.doc.total_bytes(),
                "撤销的每一步都和文档字节数一致"
            );
        }
        assert_eq!(editor.doc.to_text(), base);
    }

    #[test]
    fn read_only_blocks_every_edit_path() {
        let mut editor = laid_out("hello");
        editor.read_only = true;
        assert!(!editor.apply_insert(Pos { line: 0, col: 5 }, "!"));
        assert!(!editor.apply_delete(Pos::default(), Pos { line: 0, col: 1 }));
        assert!(!editor.apply_replace(Pos::default(), Pos { line: 0, col: 5 }, "bye"));
        assert_eq!(editor.doc.to_text(), "hello");
        assert!(!editor.is_modified());
    }

    #[test]
    fn display_name_is_the_file_name_or_untitled() {
        let mut editor = Editor::new();
        assert_eq!(editor.display_name(), "Untitled");
        editor.path = Some("C:\\work\\notes.txt".to_string());
        assert_eq!(editor.display_name(), "notes.txt");
    }

    #[test]
    fn only_a_blank_untitled_tab_is_taken_over_by_an_open() {
        let mut editor = Editor::new();
        assert!(editor.is_untitled_blank());
        // Typed into, then undone: the save point is what the dirty flag reads,
        // so a buffer walked back to empty is again the one an open may take.
        assert!(editor.apply_insert(Pos::default(), "x"));
        assert!(!editor.is_untitled_blank());
        assert!(editor.undo());
        assert!(editor.is_untitled_blank());
        // A tab that already names a file, one the frame generated, and a
        // read-only buffer all keep their own tab.
        editor.path = Some("C:\\work\\notes.txt".to_string());
        assert!(!editor.is_untitled_blank());
        let mut grep = Editor::new();
        grep.tab_title = Some("查找:[x]在[y]中".to_string());
        assert!(!grep.is_untitled_blank());
        let mut locked = Editor::new();
        locked.read_only = true;
        assert!(!locked.is_untitled_blank());
    }

    #[test]
    fn undo_preserves_layout_invariants() {
        let mut editor = laid_out("a\nb\nc\n");
        let rows_before = editor.total_rows;
        assert!(editor.apply_insert(Pos { line: 1, col: 0 }, "x\ny\n"));
        // relayout to observe the grown row count
        let mut measurer = dg_layout::measure::CachedMeasurer::new(TableMeasurer::default());
        let wrapper = WinEdgeWrapper {
            width_px: 400,
            word_wrap: true,
        };
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        editor.relayout(&wrapper, &mut ctx);
        assert!(editor.total_rows > rows_before);
        assert_eq!(editor.doc.line_count(), 6);
        // undo must restore the exact row count
        assert!(editor.undo());
        editor.relayout(&wrapper, &mut ctx);
        assert_eq!(editor.total_rows, rows_before);
        assert_eq!(editor.doc.line_count(), 4);
    }

    #[test]
    fn new_edit_clears_redo_stack() {
        let mut editor = laid_out("x");
        assert!(editor.apply_insert(Pos { line: 0, col: 1 }, "y"));
        assert!(editor.undo());
        assert!(editor.can_redo());
        assert!(editor.apply_insert(Pos::default(), "z"));
        assert!(!editor.can_redo());
    }
}

#[cfg(test)]
mod wrap_tests {
    use super::*;
    use dg_layout::measure::{CachedMeasurer, TableMeasurer, TextMeasurer};
    use dg_layout::wrap::{
        CharWrapper, CharWrapperEx, PixelWrapper, VirtualWrapper, WinEdgeWrapper, WordEdgeWrapper,
    };

    fn ctx(measurer: &mut CachedMeasurer<TableMeasurer>) -> LayoutCtx<'_> {
        let tab_px = measurer.tab_width(4, Font::Primary);
        LayoutCtx {
            measurer,
            font: Font::Primary,
            tab_px,
        }
    }

    fn laid_out(text: &str) -> Editor {
        let mut editor = Editor::new();
        editor.open_text(text);
        editor
    }

    #[test]
    fn cancel_wrap_keeps_single_row() {
        let long = "word ".repeat(40); // 200 chars, one line
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut editor = laid_out(&long);
        let mut c = ctx(&mut measurer);
        editor.relayout(&VirtualWrapper, &mut c);
        assert_eq!(editor.total_rows, 1);
    }

    #[test]
    fn cancel_wrap_counts_cells_from_the_line_length() {
        // 不换行: no break array, no measurement — the cell count the horizontal
        // range reads is the line's char count, and the caret's column raises it.
        let text = "abcdefghij
abc";
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut editor = laid_out(text);
        let mut c = ctx(&mut measurer);
        editor.relayout(&VirtualWrapper, &mut c);
        assert!(editor.virtual_wrap);
        assert_eq!(editor.total_rows, 2);
        assert!(editor.wrap.store.arrays[0].data.is_empty());
        assert_eq!(editor.wrap.store.arrays[0].cells, 10);
        assert_eq!(editor.max_cells(), 10);
        // The caret's own column is exact, and the range never falls below it.
        editor.caret = (0, 8);
        editor.h_max_cells = 0;
        let page = 4;
        editor.text_w = 32;
        editor.ensure_caret_visible(page);
        assert!(editor.max_cells() >= 10);
    }

    #[test]
    fn a_relayout_drops_the_caret_pixel_of_the_old_layout() {
        // A resize re-wraps every line without touching the text: the row the
        // caret sits in and the column that row starts at belong to the break
        // arrays that just went away, and an entry keyed on the text version
        // alone keeps answering with them.
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut editor = laid_out(&"a".repeat(200));
        let mut c = ctx(&mut measurer);
        editor.relayout(&CharWrapper { chars: 20 }, &mut c);
        editor.caret = (0, 150);
        let narrow = editor.caret_row();
        editor.relayout(&CharWrapper { chars: 40 }, &mut c);
        let after = editor.caret_row();
        let cold = {
            editor.caret_px_cache.set(None);
            editor.caret_row()
        };
        assert_ne!(
            narrow, cold,
            "both layouts put the caret in the same place — the test would prove nothing"
        );
        assert_eq!(after, cold, "the wider layout still answers with the old row");
    }

    #[test]
    fn re_wrapping_an_unwrapped_line_keeps_the_caret_pixel() {
        // 不换行 re-wraps into the same empty break array, so the caret's row and
        // its segment's first column did not move and the pixel measured from
        // them carries over. Dropping it would put a walk over the whole line
        // back onto every keystroke.
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut editor = laid_out(&"a".repeat(200));
        let mut c = ctx(&mut measurer);
        editor.relayout(&VirtualWrapper, &mut c);
        editor.caret = (0, 200);
        let _ = editor.caret_row();
        assert!(editor.apply_insert(Pos { line: 0, col: 200 }, "b"));
        editor.relayout_region(0, 1, 1, &VirtualWrapper, &mut c);
        assert!(
            editor.caret_px_cache.get().is_some(),
            "a re-wrap that moved nothing dropped the caret's pixel"
        );
        let kept = editor.caret_row();
        let cold = {
            editor.caret_px_cache.set(None);
            editor.caret_row()
        };
        assert_eq!(kept, cold, "the kept pixel no longer describes the caret");
    }

    #[test]
    fn a_re_wrap_drops_only_the_windows_it_moved() {
        let memo = |line: usize, seg: usize| dg_render::pipeline::WindowMemo {
            line,
            seg,
            budget: 0,
            measured: 0,
            col: 60,
            px: 480,
        };
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut editor = laid_out(&"a".repeat(200));
        let mut c = ctx(&mut measurer);
        editor.relayout(&CharWrapper { chars: 20 }, &mut c);
        editor.paint_win.borrow_mut().set(memo(0, 3));
        // Re-wrapping the line moves the break its window was measured
        // against, so the row has to find its way in again.
        editor.relayout_region(0, 1, 1, &CharWrapper { chars: 40 }, &mut c);
        assert!(
            editor.paint_win.borrow().get(0, 3).is_none(),
            "a moved break array left the row's window in place"
        );
        // The line below, which this region never re-wrapped, keeps its own.
        editor.paint_win.borrow_mut().set(memo(1, 0));
        editor.relayout_region(0, 1, 1, &CharWrapper { chars: 40 }, &mut c);
        assert!(
            editor.paint_win.borrow().get(1, 0).is_some(),
            "a neighbouring line's window went with a region that did not touch it"
        );
        // A full layout rebuilds every line's array.
        editor.relayout(&CharWrapper { chars: 20 }, &mut c);
        assert!(
            editor.paint_win.borrow().get(1, 0).is_none(),
            "a full relayout kept a window of the old layout"
        );
    }

    #[test]
    fn fixed_column_wraps_every_n_chars() {
        // space-free text: char mode breaks at
        // exactly `chars` characters per row
        // (limit = chars * avg width, break on
        // strict overflow with no word edges)
        let long = "a".repeat(200);
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut editor = laid_out(&long);
        let mut c = ctx(&mut measurer);
        editor.relayout(&CharWrapper { chars: 20 }, &mut c);
        assert_eq!(editor.total_rows, 10);
    }

    #[test]
    fn fixed_column_tabs_counts_tab_columns() {
        // "ab" fills columns 0-1; the tab snaps
        // to the 4-column stop, exhausting the
        // budget, so the rest wraps to row 2.
        let text = "ab\tcd";
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut editor = laid_out(text);
        let mut c = ctx(&mut measurer);
        editor.relayout(&CharWrapperEx { chars: 4 }, &mut c);
        assert_eq!(editor.total_rows, 2);
    }

    #[test]
    fn window_edge_wraps_both_modes() {
        let long = "word ".repeat(40);
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut editor = laid_out(&long);
        let mut c = ctx(&mut measurer);
        editor.relayout(
            &WinEdgeWrapper {
                width_px: 100,
                word_wrap: true,
            },
            &mut c,
        );
        let smart_rows = editor.total_rows;
        assert!(smart_rows > 1);

        let mut editor = laid_out(&long);
        let mut c = ctx(&mut measurer);
        editor.relayout(
            &WinEdgeWrapper {
                width_px: 100,
                word_wrap: false,
            },
            &mut c,
        );
        // hard wrap packs at least as many rows
        // (word wrap wastes the tail of each row)
        assert!(editor.total_rows > 1);
        assert!(editor.total_rows <= smart_rows);
    }

    #[test]
    fn fixed_pixel_wraps_by_budget() {
        let long = "word ".repeat(40);
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut editor = laid_out(&long);
        let mut c = ctx(&mut measurer);
        editor.relayout(&PixelWrapper { px: 100 }, &mut c);
        assert!(editor.total_rows > 1);
    }

    #[test]
    fn word_edge_wrapper_wraps_at_limit() {
        let long = "word ".repeat(40);
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut editor = laid_out(&long);
        let mut c = ctx(&mut measurer);
        editor.relayout(&WordEdgeWrapper { limit: 100 }, &mut c);
        assert!(editor.total_rows > 1);
    }

    /// Walks every visual row after an incremental relayout and checks the
    /// row index still describes exactly one (line, segment) per row — the
    /// invariant the paint walk relies on. A stale per-line count makes two
    /// rows map to the same line (the "next line drawn twice" symptom).
    fn assert_rows_consistent(editor: &Editor) {
        let total = editor.rows.total_rows();
        let mut seen = std::collections::HashSet::new();
        let mut last_line = 0usize;
        for row in 0..total {
            let (line, seg) = editor
                .rows
                .row_to_line_segment(row)
                .unwrap_or_else(|| panic!("row {row} of {total} has no line"));
            assert!(
                seen.insert((line, seg)),
                "row {row} re-maps to (line {line}, seg {seg})"
            );
            assert!(
                line >= last_line,
                "row {row}: line {line} after {last_line}"
            );
            last_line = line;
            let segs = editor.wrap.store.arrays[line].count() + 1;
            assert!(
                seg < segs,
                "row {row}: seg {seg} past line {line}'s {segs} rows"
            );
        }
    }

    #[test]
    fn insert_then_region_relayout_keeps_rows_consistent() {
        let text = "alpha one\nbeta two\ngamma three\n";
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let wrapper = WinEdgeWrapper {
            width_px: 400,
            word_wrap: true,
        };
        let mut editor = laid_out(text);
        let mut c = ctx(&mut measurer);
        editor.relayout(&wrapper, &mut c);
        assert_rows_consistent(&editor);
        // type "X" at the start of line 1 (char offset 10): the region
        // relayout the typing path runs after the edit
        assert!(editor.apply_insert(Pos { line: 1, col: 0 }, "X"));
        let (first, old, new) = editor.take_layout_regions().pop().expect("region");
        editor.relayout_region(first, old, new, &wrapper, &mut c);
        assert_rows_consistent(&editor);
    }

    #[test]
    #[ignore = "dump helper"]
    fn dump_paint_after_insert() {
        let text = "alpha one\nbeta two\ngamma three\n";
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let wrapper = WinEdgeWrapper {
            width_px: 60,
            word_wrap: true,
        };
        let mut editor = laid_out(text);
        let mut c = ctx(&mut measurer);
        editor.relayout(&wrapper, &mut c);
        println!(
            "=== before insert === total_rows={}",
            editor.rows.total_rows()
        );
        for row in 0..editor.rows.total_rows() {
            println!("  row {row} -> {:?}", editor.rows.row_to_line_segment(row));
        }
        assert!(editor.apply_insert(Pos { line: 1, col: 0 }, "X"));
        let (first, old, new) = editor.take_layout_regions().pop().expect("region");
        editor.relayout_region(first, old, new, &wrapper, &mut c);
        println!(
            "=== after insert === total_rows={}",
            editor.rows.total_rows()
        );
        for row in 0..editor.rows.total_rows() {
            println!("  row {row} -> {:?}", editor.rows.row_to_line_segment(row));
        }
        let mut draw = DrawList::default();
        let clip = dg_render::component::Rect::new(0.0, 0.0, 800.0, 600.0);
        editor.paint(
            20,
            clip,
            800.0,
            &mut draw,
            0.0,
            HighlightToggles {
                color: false,
                sel: false,
                url: false,
            },
        );
        println!("=== text runs ===");
        for cmd in &draw.cmds {
            if let dg_render::pipeline::DrawCmd::TextRun { text, y, .. } = cmd {
                println!("  y={y:.1} text={text:?}");
            }
        }
    }

    #[test]
    fn insert_newline_then_region_relayout_keeps_rows_consistent() {
        let text = "alpha one\nbeta two\ngamma three\n";
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let wrapper = WinEdgeWrapper {
            width_px: 400,
            word_wrap: true,
        };
        let mut editor = laid_out(text);
        let mut c = ctx(&mut measurer);
        editor.relayout(&wrapper, &mut c);
        // Enter at the end of line 0: splits the line (structural region)
        assert!(editor.apply_insert(Pos { line: 0, col: 9 }, "\n"));
        let (first, old, new) = editor.take_layout_regions().pop().expect("region");
        editor.relayout_region(first, old, new, &wrapper, &mut c);
        assert_rows_consistent(&editor);
    }

    #[test]
    fn delete_join_then_region_relayout_keeps_rows_consistent() {
        let text = "alpha one\nbeta two\ngamma three\n";
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let wrapper = WinEdgeWrapper {
            width_px: 400,
            word_wrap: true,
        };
        let mut editor = laid_out(text);
        let mut c = ctx(&mut measurer);
        editor.relayout(&wrapper, &mut c);
        // Backspace at the start of line 1: joins lines 0 and 1
        // the (K, len)..(K + 1, 0) gap: the line terminator
        assert!(editor.apply_delete(Pos { line: 0, col: 9 }, Pos { line: 1, col: 0 }));
        let (first, old, new) = editor.take_layout_regions().pop().expect("region");
        editor.relayout_region(first, old, new, &wrapper, &mut c);
        assert_rows_consistent(&editor);
    }

    /// The record a multi-caret edit leaves behind: each touched line keeps its own
    /// entry, so the view re-wraps five lines instead of the distance between the
    /// outer two.
    #[test]
    fn disjoint_spans_stay_separate_regions() {
        let mut pending = Vec::new();
        record_layout_region(&mut pending, 100, 1, 1);
        record_layout_region(&mut pending, 10, 1, 1);
        assert_eq!(pending, vec![(10, 1, 1), (100, 1, 1)]);
        // A span that reaches the previous one folds into it; the gap stays a gap.
        record_layout_region(&mut pending, 11, 1, 1);
        assert_eq!(pending, vec![(10, 2, 2), (100, 1, 1)]);
    }

    /// A span that changes the line count moves every line below it, so nothing
    /// keeps its index and the record collapses to the one region over the lot.
    #[test]
    fn a_structural_span_collapses_the_record() {
        let mut pending = vec![(10, 2, 2), (100, 1, 1)];
        record_layout_region(&mut pending, 50, 3, 1);
        assert_eq!(pending, vec![(10, 93, 91)]);
    }

    /// Two spans that overlap have no exact description at all: the record empties
    /// and the view owes a full relayout.
    #[test]
    fn overlapping_spans_leave_nothing_to_report() {
        let mut pending = Vec::new();
        record_layout_region(&mut pending, 10, 3, 3);
        record_layout_region(&mut pending, 11, 1, 1);
        assert!(pending.is_empty());
    }
}

#[cfg(test)]
mod decorations_tests {
    use super::*;
    use dg_render::pipeline::DrawCmd;

    #[test]
    fn fold_markers_from_tree_sitter_spans() {
        assert!(Editor::folding_marker_test());
    }

    #[test]
    fn paint_decorations_emits_ruler_and_minimap() {
        let sample = "{\"a\": 1}\n{\"b\": \"x\"}\n";
        let mut editor = Editor::new();
        editor.open_text(sample);
        let lang = tree_sitter::Language::from(tree_sitter_json::LANGUAGE);
        editor.attach_syntax(lang);
        let mut measurer =
            dg_layout::measure::CachedMeasurer::new(dg_layout::measure::TableMeasurer::default());
        let wrapper = dg_layout::wrap::WinEdgeWrapper {
            width_px: 400,
            word_wrap: true,
        };
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        editor.relayout(&wrapper, &mut ctx);
        let mut draw = DrawList::default();
        let ruler_h = editor.theme.line_height + 2.0;
        editor.view_flags.set(ViewFlags::MINIMAP, true);
        // 动态隐藏 draws the box only while the pointer is on the strip
        editor.map_hover = true;
        editor.paint_decorations(
            dg_render::component::Rect::new(0.0, 0.0, 800.0, 400.0),
            24,
            ruler_h,
            &mut draw,
        );
        let fills = draw
            .cmds
            .iter()
            .filter(|c| matches!(c, DrawCmd::FillRect { .. }))
            .count();
        assert!(fills >= 2, "ruler band + ticks, got {fills}");
        // the box: one fill and its four edges, in the two blend colours the
        // map derives from the theme rather than an alpha blend
        let fill = dg_render::minimap::box_fill(editor.theme.foreground, editor.theme.background);
        let edge = dg_render::minimap::box_edge(editor.theme.foreground, editor.theme.background);
        let count = |want: dg_render::theme::Color| {
            draw.cmds
                .iter()
                .filter(|c| matches!(c, DrawCmd::FillRect { color, .. } if *color == want))
                .count()
        };
        assert_eq!(count(fill), 1, "the box's fill");
        assert_eq!(count(edge), 4, "the box's edges");
    }

    /// The strip is two pixels a row, so the map must build the rows it shows
    /// and not the document. Building every row cost one syntax query per row
    /// — nine seconds on a 24 766-row file — for rows that were then never
    /// drawn.
    #[test]
    fn the_map_builds_the_rows_the_strip_shows_not_the_document() {
        let sample: String = (0..4000).map(|i| format!("{{\"k{i}\": {i}}}\n")).collect();
        let mut editor = Editor::new();
        editor.open_text(&sample);
        let lang = tree_sitter::Language::from(tree_sitter_json::LANGUAGE);
        editor.attach_syntax(lang);
        let mut measurer =
            dg_layout::measure::CachedMeasurer::new(dg_layout::measure::TableMeasurer::default());
        let wrapper = dg_layout::wrap::WinEdgeWrapper {
            width_px: 400,
            word_wrap: false,
        };
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        editor.relayout(&wrapper, &mut ctx);
        editor.view_flags.set(ViewFlags::MINIMAP, true);
        let clip = dg_render::component::Rect::new(0.0, 0.0, 800.0, 400.0);
        let ruler_h = editor.theme.line_height + 2.0;
        let mut draw = DrawList::default();
        editor.paint_decorations(clip, 24, ruler_h, &mut draw);
        assert_eq!(editor.total_rows, 4001, "one row per line, no wrapping");
        let total = editor.total_rows;
        let strip = editor.map_strip;
        let rows = (strip.h / dg_render::minimap::ROW_PX).ceil() as usize;
        let (lo, hi) = editor.minimap.window.expect("the first paint builds a window");
        assert_eq!(lo, 0, "the view starts at the first row");
        // the strip's own rows plus a screen of slack on either side
        assert!(
            hi + 1 <= rows * 3 + 4,
            "built {} rows for a {rows}-row strip",
            hi + 1
        );
        assert!(hi + 1 < total, "the document was built whole");

        // Scrolled to the end, the window moves with the view and stops at the
        // document's last row.
        editor.first_row = total - 24;
        editor.relayout(&wrapper, &mut ctx);
        let mut draw = DrawList::default();
        editor.paint_decorations(clip, 24, ruler_h, &mut draw);
        let (_, hi) = editor.minimap.window.expect("a window");
        assert_eq!(hi, total - 1, "the last row is built, and nothing past it");
    }

    #[test]
    fn ruler_paints_ticks_and_numbers_per_column() {
        let mut editor = Editor::new();
        // deterministic metrics: one row
        // 18 px, one column 8 px, gutter
        // strip 20 px
        editor.theme.line_height = 18.0;
        editor.theme.x_width = 8.0;
        editor.theme.gutter_x = 20.0;
        let mut draw = DrawList::default();
        let ruler_h = editor.theme.line_height + 2.0;
        editor.paint_decorations(
            dg_render::component::Rect::new(0.0, 0.0, 800.0, 400.0),
            24,
            ruler_h,
            &mut draw,
        );
        // The shipped defaults leave the minimap bit off, so the
        // minimap viewport blend must be absent.
        assert!(
            !draw
                .cmds
                .iter()
                .any(|c| matches!(c, DrawCmd::AlphaBlend { .. })),
            "minimap off must not emit a viewport blend"
        );
        // ruler height: one row plus 2 px
        let ruler_h = 20.0;
        let fg = editor.theme.ruler_fg;
        // full-width band in the Ruler bg
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect {
                rect,
                color,
            } if rect.x == 0.0
                && rect.y == 0.0
                && rect.w == 800.0
                && rect.h == ruler_h
                && *color == editor.theme.margin_bg
        )));
        // 1 px separator just above the
        // text area, in the Ruler fg
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect {
                rect,
                color,
            } if rect.x == 0.0
                && rect.y == ruler_h - 1.0
                && rect.w == 800.0
                && rect.h == 1.0
                && *color == fg
        )));
        // long tick end: bottom / 2; medium:
        // bottom - bottom/3 - 2; short:
        // bottom - bottom/3
        let long_top = ruler_h / 2.0;
        let med_top = ruler_h - ruler_h / 3.0 - 2.0;
        let short_top = ruler_h - ruler_h / 3.0;
        // col 0 lands on the text-area left edge (gutter_x): long
        // tick, but no number for 0
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect {
                rect,
                color,
            } if rect.x == 20.0
                && rect.y == long_top
                && rect.w == 1.0
                && rect.h == ruler_h - 1.0 - long_top
                && *color == fg
        )));
        assert!(!draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::TextRun {
                text,
                ..
            } if text == "0"
        )));
        // col 1 (x = 28): short tick
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect {
                rect,
                color,
            } if rect.x == 28.0
                && rect.y == short_top
                && rect.w == 1.0
                && *color == fg
        )));
        // col 5 (x = 60): medium tick
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect {
                rect,
                color,
            } if rect.x == 60.0
                && rect.y == med_top
                && rect.w == 1.0
                && *color == fg
        )));
        // col 10 (x = 100): long tick plus
        // the centered number "10" at
        // x - len*char_w/2 = 100 - 8
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect {
                rect,
                color,
            } if rect.x == 100.0
                && rect.y == long_top
                && *color == fg
        )));
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::TextRun {
                x,
                y,
                text,
                color,
                ..
            } if *x == 92.0
                && *y == ruler_h / 10.0 - 2.0
                && text == "10"
                && *color == fg
        )));
        // ticks stop past the right edge:
        // the last one sits at 20 + 97*8
        // = 796, none at or beyond 800
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, .. }
                if rect.x == 796.0
        )));
        assert!(!draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, .. }
                if rect.x >= 800.0
        )));
    }

    #[test]
    fn gutter_numbers_right_align_and_separator() {
        use dg_render::component::{LineNumber, RenderComponent};
        let mut theme = dg_render::theme::Theme::default();
        theme.x_width = 8.0;
        let mut gutter = LineNumber {
            // 2 digits * 8 px + 4 px
            width: 20.0,
            x: 0.0,
        };
        let mut draw = DrawList::default();
        let mut ctx = dg_render::component::PaintCtx {
            clip: dg_render::component::Rect::new(0.0, 100.0, 20.0, 18.0),
            row: 0,
            line: 9,
            row_rect: dg_render::component::Rect::new(0.0, 100.0, 20.0, 18.0),
            theme: &theme,
            view: dg_render::ViewFlags::shipped_defaults(),
            draw: &mut draw,
            display_no: 10,
            tilde: false,
            wrap_rows: 1,
            eol_kind: 1,
            line_state: 0,
            // folding off: with the icon area
            // away too, the line-number strip
            // owns the boundary line
            folding_on: false,
            active_subrow: 0,
            caret_line: usize::MAX,
        };
        gutter.paint(&mut ctx);
        // opaque background over the strip
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect {
                rect,
                color,
            } if rect.x == 0.0
                && rect.y == 100.0
                && rect.w == 20.0
                && rect.h == 18.0
                && *color == theme.gutter_bg
        )));
        // "10" right-aligned with the
        // digits' right edge at width - 3
        // (TA_RIGHT at rect.right - 3)
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::TextRun {
                x,
                y,
                text,
                color,
                ..
            } if *x == 1.0
                && *y == 100.0
                && text == "10"
                && *color == theme.gutter_fg
        )));
        // 1 px separator at width - 1 in
        // the BoundaryLine color
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect {
                rect,
                color,
            } if rect.x == 19.0
                && rect.y == 100.0
                && rect.w == 1.0
                && rect.h == 18.0
                && *color == theme.boundary_line
        )));
    }

    #[test]
    fn the_primary_caret_never_reaches_the_draw_list() {
        // The bug this pins: this implementation used to paint a 2 px `theme.caret` strip
        // into the text pass, and the gutter pass (`LeftMargin`, 2 px of
        // `theme.background` at `x == gutter_x`) ran after it and erased it at column 0.
        // The implementation's invariant is therefore stronger than "the strip survives":
        // there is nothing in the draw list to survive.
        use dg_layout::measure::TableMeasurer;
        use dg_layout::wrap::WinEdgeWrapper;
        let caret_strips = |draw: &DrawList, theme: &Theme, line_h: f32| {
            draw.cmds
                .iter()
                .filter(|c| {
                    matches!(c, DrawCmd::FillRect { rect, color }
                        if *color == theme.caret_indicator()
                            && rect.w == theme.caret_w
                            && rect.h == line_h)
                })
                .count()
        };
        let laid_out = |text: &str| {
            let mut editor = Editor::new();
            editor.open_text(text);
            let mut measurer = dg_layout::measure::CachedMeasurer::new(TableMeasurer::default());
            let wrapper = WinEdgeWrapper {
                width_px: 400,
                word_wrap: true,
            };
            let mut ctx = LayoutCtx {
                measurer: &mut measurer,
                font: Font::Primary,
                tab_px: 32,
            };
            editor.relayout(&wrapper, &mut ctx);
            editor
        };
        let mut editor = laid_out("hello\nworld\n");
        let line_h = editor.theme.line_height;
        editor.caret = (0, 2);
        let mut draw = DrawList::default();
        editor.paint(
            10,
            dg_render::component::Rect::new(0.0, 0.0, 800.0, 600.0),
            800.0,
            &mut draw,
            0.0,
            HighlightToggles::default(),
        );
        // The harness must actually be painting, or "no caret command" is
        // vacuous.
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::TextRun { text, .. } if text == "hello"
        )));
        // The invariant: nothing in the primary caret's colour reaches the
        // draw list at all — the system caret owns it — and the multi-caret
        // indicator line (SelIndicator) has no caret to serve here.
        let primary_colored = draw
            .cmds
            .iter()
            .filter(
                |c| matches!(c, DrawCmd::FillRect { color, .. } if *color == editor.theme.caret),
            )
            .count();
        assert_eq!(primary_colored, 0);
        assert_eq!(caret_strips(&draw, &editor.theme, line_h), 0);

        // Multi-caret editing has no system caret to borrow, so those strips
        // stay self-drawn — one per extra caret, and none on other lines.
        editor.extra_carets = vec![(0, 4), (1, 1), (5, 0)];
        let mut draw = DrawList::default();
        editor.paint(
            10,
            dg_render::component::Rect::new(0.0, 0.0, 800.0, 600.0),
            800.0,
            &mut draw,
            0.0,
            HighlightToggles::default(),
        );
        assert_eq!(caret_strips(&draw, &editor.theme, line_h), 2);
        // The marks take the thickness the platform reports for the primary
        // caret: raise it (the high-DPI bar, 3 px) and every strip follows,
        // still one per extra caret.
        editor.theme.caret_w = 3.0;
        let mut draw = DrawList::default();
        editor.paint(
            10,
            dg_render::component::Rect::new(0.0, 0.0, 800.0, 600.0),
            800.0,
            &mut draw,
            0.0,
            HighlightToggles::default(),
        );
        assert_eq!(caret_strips(&draw, &editor.theme, line_h), 2);
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if *color == editor.theme.caret_indicator() && rect.w == 3.0
        )));
    }

    #[test]
    fn folding_markers_collapsed_and_expandable() {
        use dg_render::component::{Folding, RenderComponent};
        let mut theme = dg_render::theme::Theme::default();
        theme.x_width = 8.0;
        theme.line_height = 18.0;
        // max(12, x_width - 2) = 12 px
        let mut folding = Folding::new(12.0, 0.0);
        let mut draw = DrawList::default();
        let row_rect = dg_render::component::Rect::new(0.0, 100.0, 12.0, 18.0);
        let ctx_rect = row_rect;
        // the marker's centre
        // sits (aveCharWidth / 3) left of the
        // strip's centre, one half-row down.
        let cx = (12.0 - 8.0 / 3.0) / 2.0;
        let cy = 100.0 + (18.0 + 1.0) / 2.0 - 1.0;
        // A line whose next line is hidden
        // under a fold opening here draws
        // the `+` marker: a 9x9 hollow box
        // with a cross.
        folding.folded = vec![false, true];
        folding.starts = vec![false, false];
        folding.paint(&mut dg_render::component::PaintCtx {
            clip: ctx_rect,
            row: 0,
            line: 0,
            row_rect,
            theme: &theme,
            view: dg_render::ViewFlags::shipped_defaults(),
            draw: &mut draw,
            display_no: 1,
            tilde: false,
            wrap_rows: 1,
            eol_kind: 1,
            line_state: 0,
            folding_on: true,
            active_subrow: 0,
            caret_line: usize::MAX,
        });
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if rect.x == cx - 4.0
                    && rect.y == cy - 4.0
                    && rect.w == 9.0
                    && rect.h == 1.0
                    && *color == theme.folding_fg
        )));
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if rect.x == cx - 2.0
                    && rect.y == cy
                    && rect.w == 6.0
                    && rect.h == 1.0
                    && *color == theme.folding_fg
        )));
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if rect.x == cx
                    && rect.y == cy - 2.0
                    && rect.w == 1.0
                    && rect.h == 6.0
                    && *color == theme.folding_fg
        )));
        // A line that opens a block but
        // hides nothing draws the `-`
        // marker: the same box, the
        // horizontal bar only.
        draw.cmds.clear();
        folding.folded = vec![false, false];
        folding.starts = vec![true, false];
        folding.paint(&mut dg_render::component::PaintCtx {
            clip: ctx_rect,
            row: 0,
            line: 0,
            row_rect,
            theme: &theme,
            view: dg_render::ViewFlags::shipped_defaults(),
            draw: &mut draw,
            display_no: 1,
            tilde: false,
            wrap_rows: 1,
            eol_kind: 1,
            line_state: 0,
            folding_on: true,
            active_subrow: 0,
            caret_line: usize::MAX,
        });
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if rect.x == cx - 4.0
                    && rect.y == cy - 4.0
                    && rect.w == 9.0
                    && rect.h == 1.0
                    && *color == theme.folding_fg
        )));
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if rect.x == cx - 2.0
                    && rect.y == cy
                    && rect.w == 6.0
                    && rect.h == 1.0
                    && *color == theme.folding_fg
        )));
        assert!(!draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, .. }
                if rect.x == cx
                    && rect.y == cy - 2.0
                    && rect.w == 1.0
                    && rect.h == 6.0
        )));
    }

    #[test]
    fn bookmark_bar_paints_the_middle_third() {
        use dg_render::component::{BookMarkRender, RenderComponent};
        let mut theme = dg_render::theme::Theme::default();
        theme.x_width = 8.0;
        theme.line_height = 18.0;
        // BookmarkStyle 1's DPI table at
        // <120 DPI: a 7 px strip.
        let mut bookmark = BookMarkRender::new(7.0, 0.0);
        bookmark.marks = vec![true];
        let mut draw = DrawList::default();
        let row_rect = dg_render::component::Rect::new(0.0, 100.0, 7.0, 18.0);
        let third = ((18.0f32 + 1.0) / 3.0).floor();
        // the bookmark strip is off in the
        // shipped defaults (no Bookmark key
        // in setting.ini); turn it on for
        // the pass under test
        let mut view = dg_render::ViewFlags::shipped_defaults();
        view.set(dg_render::ViewFlags::BOOKMARK, true);
        bookmark.paint(&mut dg_render::component::PaintCtx {
            clip: row_rect,
            row: 0,
            line: 0,
            row_rect,
            theme: &theme,
            view,
            draw: &mut draw,
            display_no: 1,
            tilde: false,
            wrap_rows: 1,
            eol_kind: 1,
            line_state: 0,
            folding_on: true,
            active_subrow: 0,
            caret_line: usize::MAX,
        });
        // the strip's gutter background
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if rect.x == 0.0
                    && rect.y == 100.0
                    && rect.w == 7.0
                    && rect.h == 18.0
                    && *color == theme.gutter_bg
        )));
        // the outer bar: 1 px inside the
        // left edge, the middle third of
        // the row, in gutter_fg
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if rect.x == 1.0
                    && rect.y == 100.0 + third
                    && rect.w == 6.0
                    && rect.h == third
                    && *color == theme.gutter_fg
        )));
        // the inner bar: inflated by 1 on
        // every side, in icon_fg
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if rect.x == 2.0
                    && rect.y == 100.0 + third + 1.0
                    && rect.w == 4.0
                    && rect.h == third - 2.0
                    && *color == theme.icon_fg
        )));
    }
}

#[cfg(test)]
mod bookmark_tests {
    use super::*;

    #[test]
    fn insert_shifts_bookmarks_below() {
        let mut editor = Editor::new();
        editor.open_text("a\nb\nc\nd\n");
        editor.bookmark_toggle(3); // 'd'
        // two lines inserted at the top:
        // 'd' moves 3 -> 5
        assert!(editor.apply_insert(Pos::default(), "x\ny\n"));
        assert!(editor.bookmarks.has(5));
        assert!(!editor.bookmarks.has(3));
    }

    #[test]
    fn midline_insert_keeps_own_bookmark() {
        let mut editor = Editor::new();
        editor.open_text("a\nb\nc\nd\n");
        editor.bookmark_toggle(1); // 'b'
        editor.bookmark_toggle(3); // 'd'
        // newline in the middle of line 1:
        // 'b' keeps its bookmark, 'd' moves 3 -> 5
        assert!(editor.apply_insert(Pos { line: 1, col: 1 }, "X\nY\n"));
        assert!(editor.bookmarks.has(1));
        assert!(editor.bookmarks.has(5));
    }

    #[test]
    fn delete_shifts_bookmarks_up() {
        let mut editor = Editor::new();
        editor.open_text("a\nb\nc\nd\n");
        editor.bookmark_toggle(1); // 'b'
        editor.bookmark_toggle(3); // 'd'
        // delete line 2 ('c') entirely:
        // 'd' moves 3 -> 2, 'b' stays
        assert!(editor.apply_delete(Pos { line: 2, col: 0 }, Pos { line: 3, col: 0 }));
        assert!(editor.bookmarks.has(1));
        assert!(editor.bookmarks.has(2));
    }

    #[test]
    fn replace_shifts_by_delta() {
        let mut editor = Editor::new();
        editor.open_text("a\nb\nc\n");
        editor.bookmark_toggle(2); // 'c'
        // replace 'b\n' with two lines:
        // 'c' moves 2 -> 3
        assert!(editor.apply_replace(
            Pos { line: 1, col: 0 },
            Pos { line: 2, col: 0 },
            "x\ny\n",
        ));
        assert!(editor.bookmarks.has(3));
    }

    #[test]
    fn next_prev_wrap_and_clear() {
        let mut editor = Editor::new();
        editor.open_text("a\nb\nc\n");
        assert_eq!(editor.bookmark_next(0), None);
        editor.bookmark_toggle(2);
        editor.bookmark_toggle(0);
        assert_eq!(editor.bookmark_next(0), Some(2));
        // wraps to the first
        assert_eq!(editor.bookmark_next(2), Some(0));
        // wraps to the last
        assert_eq!(editor.bookmark_prev(0), Some(2));
        editor.bookmark_clear();
        assert_eq!(editor.bookmark_next(0), None);
    }

    #[test]
    fn open_text_clears_bookmarks() {
        let mut editor = Editor::new();
        editor.open_text("a\nb\n");
        editor.bookmark_toggle(1);
        editor.open_text("c\n");
        assert!(!editor.bookmarks.has(1));
        assert!(editor.bookmark_flags().iter().all(|f| !*f));
    }

    #[test]
    fn undo_redo_shifts_bookmarks() {
        let mut editor = Editor::new();
        editor.open_text("one\ntwo\nthree\n");
        editor.bookmark_toggle(2); // 'three'
        // two lines inserted at the start of line 1:
        // 'three' moves 2 -> 4
        assert!(editor.apply_insert(Pos { line: 1, col: 0 }, "a\nb\n"));
        assert!(editor.bookmarks.has(4));
        assert!(!editor.bookmarks.has(2));
        // undo returns the bookmark to line 2
        assert!(editor.undo());
        assert!(editor.bookmarks.has(2));
        assert!(!editor.bookmarks.has(4));
        // redo shifts it down again
        assert!(editor.redo());
        assert!(editor.bookmarks.has(4));
    }

    #[test]
    fn macro_undo_restores_bookmark() {
        let mut editor = Editor::new();
        editor.open_text("x\ny\nz\n");
        editor.bookmark_toggle(2); // 'z'
        editor.begin_macro();
        assert!(editor.apply_insert(Pos { line: 1, col: 0 }, "a\n"));
        assert!(editor.apply_insert(Pos { line: 2, col: 0 }, "b\n"));
        assert!(editor.end_macro().is_some());
        assert!(editor.bookmarks.has(4));
        // one undo step reverts both edits and
        // restores the bookmark
        assert!(editor.undo());
        assert!(editor.bookmarks.has(2));
        assert!(!editor.bookmarks.has(4));
    }

    #[test]
    fn undo_midline_insert_keeps_own_bookmark() {
        let mut editor = Editor::new();
        editor.open_text("ab\ncd");
        editor.bookmark_toggle(0);
        editor.bookmark_toggle(1);
        // mid-line insert with a newline: only
        // lines below shift
        assert!(editor.apply_insert(Pos { line: 0, col: 1 }, "X\nY"));
        assert!(editor.bookmarks.has(0));
        assert!(editor.bookmarks.has(2));
        assert!(!editor.bookmarks.has(1));
        assert!(editor.undo());
        assert!(editor.bookmarks.has(1));
    }
}

/// Type alias used by `attach_syntax` — re-exported
/// tree-sitter Language handle.
pub type TreeSitterLanguageHandle = tree_sitter::Language;

#[cfg(test)]
mod tag_tests {
    use super::*;
    use dg_layout::measure::{CachedMeasurer, TableMeasurer};
    use dg_layout::wrap::WinEdgeWrapper;

    fn laid_out(text: &str) -> Editor {
        let mut editor = Editor::new();
        editor.open_text(text);
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let wrapper = WinEdgeWrapper {
            width_px: 400,
            word_wrap: true,
        };
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        editor.relayout(&wrapper, &mut ctx);
        editor
    }

    #[test]
    fn tag_match_backward_finds_open_tag() {
        // caret on the closing `</div>` name
        // (line 2, col 2): the backward scan
        // must land on the opening `<div>`
        // (line 0, name start col 1)
        let text = "<div>\n  <span>hi</span>\n</div>\n";
        let mut editor = laid_out(text);
        editor.caret = (2, 2);
        let m = editor.find_tag_match(2, 2, "div", true);
        assert_eq!(m, Some((0, 1)));
    }

    #[test]
    fn tag_match_forward_finds_close_tag() {
        let text = "<div>\n  <span>hi</span>\n</div>\n";
        let editor = laid_out(text);
        // caret tag ends at col 4 on line 0;
        // the closing name starts at col 2
        let m = editor.find_tag_match(0, 4, "div", false);
        assert_eq!(m, Some((2, 2)));
    }

    #[test]
    fn tag_match_nesting_aware() {
        let text = "<div>\n  <div>\n    <b>x</b>\n  </div>\n</div>\n";
        let editor = laid_out(text);
        // the innermost closing `</div>` (line 3)
        // matches the inner opening (line 1)
        let m = editor.find_tag_match(3, 2, "div", true);
        assert_eq!(m, Some((1, 3)));
        // the outermost opening (line 0) matches
        // the outermost closing (line 4, name
        // starts at col 2)
        let m = editor.find_tag_match(0, 4, "div", false);
        assert_eq!(m, Some((4, 2)));
    }

    #[test]
    fn tag_match_backward_line_window() {
        // more than 200 lines above the caret:
        // the backward scan gives up
        let mut text = String::new();
        for _ in 0..250 {
            text.push_str("x\n");
        }
        text.push_str("<div>\n");
        let editor = laid_out(&text);
        // the `<div>` line is the last non-empty
        // line (ropey counts a trailing empty line)
        let caret_line = editor.doc.line_count() - 2;
        let m = editor.find_tag_match(caret_line, 2, "div", true);
        assert_eq!(m, None);
    }

    #[test]
    fn tag_highlight_end_to_end() {
        let text = "<div>\n  <span>hi</span>\n</div>\n";
        let mut editor = laid_out(text);
        let lang = tree_sitter::Language::from(tree_sitter_html::LANGUAGE);
        editor.attach_syntax(lang);
        // caret on the closing tag name
        editor.caret = (2, 2);
        editor.update_tag_highlight();
        assert_eq!(editor.tag_hl.a, Some((2, 2, 5)));
        assert_eq!(editor.tag_hl.b, Some((0, 1, 4)));
        // caret on the opening tag name
        editor.caret = (0, 1);
        editor.update_tag_highlight();
        assert_eq!(editor.tag_hl.a, Some((0, 1, 4)));
        assert_eq!(editor.tag_hl.b, Some((2, 2, 5)));
        // caret on whitespace: no highlight
        editor.caret = (1, 2);
        editor.update_tag_highlight();
        assert_eq!(editor.tag_hl.a, None);
        assert_eq!(editor.tag_hl.b, None);
        // caret on raw text (`hi` is a raw
        // text node, not a tag_name)
        editor.caret = (1, 11);
        editor.update_tag_highlight();
        assert_eq!(editor.tag_hl.a, None);
    }

    #[test]
    fn tag_highlight_case_insensitive() {
        // a lowercase closing tag matches an
        // uppercase opening tag
        let text = "<DIV>\n  <span>hi</span>\n</div>\n";
        let mut editor = laid_out(text);
        let lang = tree_sitter::Language::from(tree_sitter_html::LANGUAGE);
        editor.attach_syntax(lang);
        editor.caret = (2, 2);
        editor.update_tag_highlight();
        assert_eq!(editor.tag_hl.a, Some((2, 2, 5)));
        assert_eq!(editor.tag_hl.b, Some((0, 1, 4)));
    }

    #[test]
    fn tag_highlight_paint_recolors_tag_runs() {
        let text = "<div>\n  <span>hi</span>\n</div>\n";
        let mut editor = laid_out(text);
        let lang = tree_sitter::Language::from(tree_sitter_html::LANGUAGE);
        editor.attach_syntax(lang);
        editor.caret = (0, 1);
        editor.update_tag_highlight();
        let mut draw = DrawList::default();
        editor.paint(
            24,
            dg_render::component::Rect::new(0.0, 0.0, 800.0, 400.0),
            800.0,
            &mut draw,
            0.0,
            HighlightToggles::default(),
        );
        // the tag-name runs on lines 0 and 2 are
        // painted with the tag color
        let tag_color = editor.theme.tag_fg.to_u32();
        let mut found = 0;
        for cmd in &draw.cmds {
            if let dg_render::pipeline::DrawCmd::TextRun { text, color, .. } = cmd {
                if (text == "div" || text == "span") && color.to_u32() == tag_color {
                    found += 1;
                }
            }
        }
        assert!(found >= 2, "expected recolored tag runs, got {}", found);
    }
}

#[cfg(test)]
mod theme_tests {
    use super::*;
    use dg_render::theme::Color;

    const DOGEDIT_DARK: &str = "\
[Theme]
Default=#1E1F22,#D4D7DD
Selection=#2F4A63,#EAEDF2
LineNumber=#1B1C1F,#666E80
BookMark=#1B1C1F,#82AAFF
Pair=#1E1F22,#FFCB6B
Ruler=#24262B,#4A5162
ActiveLine=#26282E
Caret=#E6E9EF
DefaultBgNull=#1E1F22
Comment1=#1E1F22,#7C8494
String1=#1E1F22,#C3E88D
Number=#1E1F22,#F78C6C
Word1=#1E1F22,#C792EA
Function=#1E1F22,#82AAFF
Type=#1E1F22,#4EC9B0
Var=#1E1F22,#E1A6C0
Operator=#1E1F22,#89DDFF
Tag=#1E1F22,#F07178
";

    #[test]
    fn load_theme_text_applies_colors() {
        let mut editor = Editor::new();
        assert!(editor.load_theme_text(DOGEDIT_DARK));
        assert_eq!(editor.theme.background, Color::rgb(0x1E, 0x1F, 0x22));
        assert_eq!(editor.theme.foreground, Color::rgb(0xD4, 0xD7, 0xDD));
        assert_eq!(editor.theme.bookmark_fg, Color::rgb(0x82, 0xAA, 0xFF));
        assert_eq!(editor.theme.tag_fg, Color::rgb(0xFF, 0xCB, 0x6B));
        assert_eq!(editor.theme.caret, Color::rgb(0xE6, 0xE9, 0xEF));
    }

    #[test]
    fn syntax_config_from_theme_maps_colors() {
        let file = parse_theme_ini(DOGEDIT_DARK);
        let cfg = syntax_config_from_theme(&file);
        let color = |kind: &str| cfg.rules.iter().find(|(k, _)| *k == kind).map(|(_, c)| *c);
        assert_eq!(color("comment"), Some(0x7C8494));
        assert_eq!(color("string"), Some(0xC3E88D));
        assert_eq!(color("number"), Some(0xF78C6C));
        assert_eq!(color("keyword"), Some(0xC792EA));
        assert_eq!(color("function"), Some(0x82AAFF));
        assert_eq!(color("type"), Some(0x4EC9B0));
        assert_eq!(color("property"), Some(0xE1A6C0));
        assert_eq!(color("operator"), Some(0x89DDFF));
        assert_eq!(color("tag"), Some(0xF07178));
        // query 路径跟着同一套色板：capture 前缀从它对应的 kind 取色。
        let cap = |name: &str| {
            cfg.capture_rules
                .iter()
                .find(|(c, _)| c == name)
                .map(|(_, c)| *c)
        };
        assert_eq!(cap("comment"), Some(0x7C8494));
        assert_eq!(cap("string.escape"), Some(0xC3E88D), "string 的深色");
        assert_eq!(cap("constant.numeric"), Some(0xF78C6C), "number 的色");
        assert_eq!(cap("function.macro"), Some(0x82AAFF), "function 的色");
        assert_eq!(cap("constructor"), Some(0x4EC9B0), "type 的色");
        assert_eq!(cap("variable.other.member"), Some(0xE1A6C0), "property 的色");
        assert_eq!(cap("tag"), Some(0xF07178));
    }

    /// `[Style]` 的八位一路走到 `ThemeConfig`：随包主题的粗/斜、以及六个新位
    /// （矩/删/下/双/点/波）都原样搬过来，只有高位被掩掉 —— `Pair=257` 那种
    /// 带 256 的值在这套布局里没有定义（见 `dg_syntax::STYLE_BOLD`
    /// 上面那张表）。
    #[test]
    fn syntax_config_carries_the_style_bits() {
        let cfg = syntax_config_from_theme(&parse_theme_ini(DOGEDIT_DARK));
        assert_eq!(cfg.style_for("comment"), 0, "测试用的这份没有 [Style]");

        // 随包的那份：`Comment1=2` 斜、`Word1=1` 粗 —— 和主题对话框里显示的
        // 勾是同一份数据。
        let bundled = crate::themes::BUNDLED_THEMES
            .iter()
            .find(|(n, _)| *n == "DogEdit_Dark")
            .map(|(_, t)| *t)
            .expect("DogEdit_Dark 在随包主题里");
        let cfg = syntax_config_from_theme(&parse_theme_ini(bundled));
        assert_eq!(cfg.style_for("comment"), dg_syntax::STYLE_ITALIC, "Comment1=2");
        assert_eq!(cfg.style_for("keyword"), dg_syntax::STYLE_BOLD, "Word1=1");
        assert_eq!(cfg.style_for("string"), 0, "String1=0 是正体");

        let text = "[Theme]\nDefault=#FFFFFF\n[Style]\nComment1=18\nString1=8\n\
                    Number=4\nWord1=32\nFunction=64\nType=128\nOperator=257\n";
        let cfg = syntax_config_from_theme(&parse_theme_ini(text));
        assert_eq!(cfg.style_for("comment"), 18, "斜 + 下");
        assert_eq!(cfg.style_for("string"), 8, "删");
        assert_eq!(cfg.style_for("number"), 4, "矩");
        assert_eq!(cfg.style_for("keyword"), 32, "双");
        assert_eq!(cfg.style_for("function"), 64, "点");
        assert_eq!(cfg.style_for("type"), 128, "波");
        assert_eq!(
            cfg.style_for("operator"),
            1,
            "257 的高位没有定义，读出来只剩粗"
        );
        // query 路径（capture）跟着同一条规则走，换主题不会两条路径漂。
        assert_eq!(cfg.capture_style("comment"), 18);
        assert_eq!(cfg.capture_style("constant.numeric"), 4);
    }

    /// Plain text is the bulk of a buffer, and the highlighter
    /// used to answer with a compiled constant for it, so every
    /// theme drew identical text and only the keywords moved.
    #[test]
    fn plain_text_color_follows_the_theme() {
        for (name, text) in crate::themes::BUNDLED_THEMES {
            let file = parse_theme_ini(text);
            let want = file
                .fg("Default")
                .unwrap_or_else(|| panic!("{name} has no Default fg"));
            assert_eq!(
                syntax_config_from_theme(&file).default_color,
                want.to_u32(),
                "{name}: plain text ignores its own theme"
            );
        }
        let mut editor = Editor::new();
        assert!(editor.load_theme_text(DOGEDIT_DARK));
        assert_eq!(editor.syntax_theme.default_color, 0xD4D7DD);
        assert_ne!(
            editor.syntax_theme.default_color,
            dg_syntax::ThemeConfig::default().default_color
        );
    }

    #[test]
    fn load_theme_text_rejects_non_theme() {
        let mut editor = Editor::new();
        assert!(!editor.load_theme_text(""));
        assert!(!editor.load_theme_text("[Other]\nDefault=#FFFFFF\n"));
        assert!(!editor.load_theme_text("[Theme]\nDefault=notacolor\n"));
    }

    #[test]
    fn every_bundled_theme_parses_and_applies() {
        for (name, text) in crate::themes::BUNDLED_THEMES {
            let file = parse_theme_ini(text);
            assert!(!file.colors.is_empty(), "{} carries no colors", name);
            assert!(file.fg("Default").is_some(), "{} carries no Default", name);
            let mut theme = Theme::default();
            theme.apply_theme_file(&file);
        }
        assert!(
            crate::themes::BUNDLED_THEMES
                .iter()
                .any(|(n, _)| *n == "DogEdit_Dark")
        );
    }

    /// 主题文件的四段全是「按名字查表、查不到就跳过」的结构，所以写错一个
    /// 字母**不报错，只是没反应**：`staus.bg=#151A22` 那一行等于没写，颜色停在
    /// 出厂值，而文件看着完全正常。带点的键是 token 拼法（89 个），不带点的是
    /// 命名色组（`Comment1`、`Word1`、`Url` 这些），两者混在同一张
    /// `[Theme]` 表里；只有这条测试能把它们分开，也只有它能在改主题文件时先炸。
    ///
    /// `[Metrics]` 顺带查了越界：`Metrics::apply_theme` 会夹取，夹取不报错，
    /// 所以「写了 500 存进去 48」也得算一条问题。
    #[test]
    fn bundled_theme_keys_all_resolve() {
        for (name, text) in crate::themes::BUNDLED_THEMES {
            let file = parse_theme_ini(text);
            for key in file.colors.keys().filter(|k| k.contains('.')) {
                assert!(
                    dg_render::tokens::TOKEN_NAMES.contains(&key.as_str()),
                    "{name}: {key:?} 不是 token 名，这一行不会生效"
                );
            }
            let mut metrics = dg_skin::Metrics::new();
            let report = metrics.apply_theme(&file);
            assert!(report.is_empty(), "{name}: 度量 {}", report.summary());
            let (_, shapes) = dg_skin::Recipe::from_theme_file(&file);
            assert!(shapes.is_empty(), "{name}: 形状 {shapes:?}");
            let (_, fonts) = dg_skin::Fonts::from_theme_file(&file);
            assert!(fonts.is_empty(), "{name}: 字体 {fonts:?}");
        }
    }
}

#[cfg(test)]
mod caret_tests {
    use super::*;
    use dg_core::Range;

    fn editor(text: &str) -> Editor {
        let mut e = Editor::new();
        e.open_text(text);
        e.selection.add(Range {
            start: (0, 0),
            end: (0, text.chars().count()),
        });
        e
    }

    /// Select All leaves the whole file selected, and the caret
    /// used to move away with that selection still up. The next
    /// character then replaced the document: one arrow key plus
    /// one keystroke was enough to destroy a file. The live input
    /// harness hit this while reading a buffer back.
    #[test]
    fn a_plain_move_collapses_the_selection() {
        let mut right = editor("hello");
        right.caret_motion(MoveDir::Right, 1, false);
        assert!(right.selection.is_empty());
        assert_eq!(right.caret, (0, 5));

        let mut left = editor("hello");
        left.caret_motion(MoveDir::Left, 1, false);
        assert!(left.selection.is_empty());
        assert_eq!(left.caret, (0, 0));

        let mut down = editor("hello\nworld");
        down.selection.clear();
        down.selection.add(Range {
            start: (0, 0),
            end: (1, 5),
        });
        down.caret_motion(MoveDir::Down, 1, false);
        assert_eq!(down.caret, (1, 5));
    }

    #[test]
    fn typing_after_a_move_adds_instead_of_replacing() {
        let mut e = editor("hello");
        e.caret_motion(MoveDir::Right, 1, false);
        e.apply_insert(Pos { line: 0, col: 5 }, "X");
        assert_eq!(e.doc.to_text(), "helloX");
    }

    /// A folded run is not on screen, so every move that crosses a line
    /// boundary lands on the next visible row — the char and word moves step
    /// over the whole run rather than into it.
    #[test]
    fn moves_step_over_folded_rows() {
        let mut e = editor("a\nb\nc\nd");
        e.folding.set(1, true);
        e.folding.set(2, true);

        e.caret = (0, 1);
        e.caret_motion(MoveDir::Right, 1, false);
        assert_eq!(e.caret, (3, 0), "right crosses the fold");
        e.caret_motion(MoveDir::Left, 1, false);
        assert_eq!(e.caret, (0, 1), "left crosses it back");

        e.caret_motion(MoveDir::Down, 1, false);
        assert_eq!(e.caret, (3, 1), "down keeps the column, clamped");
        e.caret_motion(MoveDir::Up, 1, false);
        assert_eq!(e.caret, (0, 1));

        e.caret = (0, 1);
        e.caret_motion(MoveDir::WordRight, 1, false);
        assert_eq!(e.caret, (3, 0));
        e.caret_motion(MoveDir::WordLeft, 1, false);
        assert_eq!(e.caret, (0, 1));
    }

    /// A document laid out the way the running view lays out: 9 px cells,
    /// 4-cell tabs, an unwrapped text area `text_w` wide.
    fn unwrapped(text: &str, text_w: i32) -> Editor {
        let mut e = Editor::new();
        e.open_text(text);
        e.theme.x_width = 9.0;
        e.theme.cjk_width = 18.0;
        let n = e.doc.line_count();
        e.wrap.store.ensure(n);
        e.rows.rebuild(&e.wrap.store, &e.folding);
        e.total_rows = e.rows.total_rows();
        e.text_w = text_w;
        e.wrapped = false;
        e
    }

    /// the caret reveal's horizontal arm: a caret past the right edge scrolls the
    /// document by whole cells, keeping 40 px of the area in reserve and landing 10
    /// cells of context behind the caret. This implementation used to compare the caret's
    /// *character column* with the scroll and only ever move it left, so a caret
    /// walking off the right edge never came back — on a tab-indented line the tab
    /// stops put it thousands of pixels out.
    #[test]
    fn a_caret_past_the_right_edge_reveals_itself() {
        let text = "\t".repeat(100) + "ENDMARK";
        let mut e = unwrapped(&text, 1209);
        e.caret = (0, text.chars().count());
        // 100 tabs at a 36 px period, then the seven letters.
        assert_eq!(e.caret_row(), (0, 3600 + 63));
        e.ensure_caret_visible(40);
        assert_eq!(e.h_scroll, (3600 + 63 - 1169) / 9 + 10);
        // The caret is inside the area, and not flush against its edge.
        let px = 3600 + 63 - e.h_scroll * 9;
        assert!(px > 0 && px <= 1169, "caret left at {px} of 1169");
    }

    /// The same rule backwards: a caret left of the view pulls the scroll back by
    /// its own cells plus 10.
    #[test]
    fn a_caret_left_of_the_view_reveals_back() {
        let text = "\t".repeat(100) + "ENDMARK";
        let mut e = unwrapped(&text, 1209);
        e.caret = (0, text.chars().count());
        e.ensure_caret_visible(40);
        let scrolled = e.h_scroll;
        e.caret = (0, 0);
        e.ensure_caret_visible(40);
        assert_eq!(e.h_scroll, (scrolled + (0 - scrolled * 9) / 9 - 10).max(0));
        assert_eq!(e.h_scroll, 0);
    }

    /// A wrapped document has no horizontal scroll to reveal into — the view is
    /// non-null and the caret reveal skips its whole horizontal arm.
    #[test]
    fn a_wrapped_document_never_scrolls_sideways() {
        let text = "x".repeat(400);
        let mut e = unwrapped(&text, 200);
        e.wrapped = true;
        e.caret = (0, 400);
        e.ensure_caret_visible(40);
        assert_eq!(e.h_scroll, 0);
    }
}

#[cfg(test)]
mod highlight_tests {
    use super::*;
    use dg_core::Range;
    use dg_layout::measure::{CachedMeasurer, TableMeasurer};
    use dg_layout::wrap::CharWrapperEx;

    fn editor(text: &str) -> Editor {
        let mut editor = Editor::new();
        editor.open_text(text);
        editor
    }

    /// The decoration scans take a column window; a test that is not about the
    /// window asks for the whole line.
    trait WholeLine {
        fn bands(&self, line: usize) -> Vec<(usize, usize, Color)>;
        fn spans(&self, line: usize) -> Vec<(usize, usize, Color, Color)>;
    }

    impl WholeLine for Editor {
        fn bands(&self, line: usize) -> Vec<(usize, usize, Color)> {
            self.color_literal_bands(line, 0, self.doc.line_len(line))
        }

        fn spans(&self, line: usize) -> Vec<(usize, usize, Color, Color)> {
            self.url_email_spans(line, 0, self.doc.line_len(line))
        }
    }

        /// 装饰扫描只扫一行能显示的那几列：一条几兆字节的行照样有网址和颜色字面量，
    /// 代价是一屏，不是整行。落在窗口之外的装饰不归它管。
    #[test]
    fn the_decoration_scans_cover_the_columns_a_row_shows() {
        let mut text = String::from("https://example.com #ff0000 ");
        while text.chars().count() < 300_000 {
            text.push_str("filler ");
        }
        let e = editor(&text);
        let len = e.doc.line_len(0);
        assert!(len > 300_000);
        let (from, to) = e.row_cols(0, 0);
        assert!(to - from < 4_000, "the row asked for {} columns", to - from);
        let spans = e.url_email_spans(0, from, to);
        assert_eq!(spans.len(), 1, "the URL inside the row's columns was missed");
        assert_eq!((spans[0].0, spans[0].1), (0, 19));
        let bands = e.color_literal_bands(0, from, to);
        assert_eq!(
            bands.len(),
            1,
            "the color literal inside the row's columns was missed"
        );
        assert_eq!(bands[0], (20, 27, Color::rgb(255, 0, 0)));
        // A window over the far end of the same line finds neither, and pays
        // nothing for the text in front of it.
        assert!(e.url_email_spans(0, len - 100, len).is_empty());
        assert!(e.color_literal_bands(0, len - 100, len).is_empty());
    }

    /// 一次编辑只作废它那一行之后的染色缓存：语法从编辑点往后重扫，前面的行还是
    /// 原来那份 runs，一屏行的重绘不会因为一个键全部重来。
    #[test]
    fn an_edit_keeps_the_highlight_runs_of_the_lines_above_it() {
        let mut e = editor("one\ntwo\nthree");
        let mut map = std::collections::HashMap::new();
        for l in 0..3usize {
            map.insert(l, vec![(7u32, 1u32, 0u8)]);
        }
        e.hl_run_cache =
            std::cell::RefCell::new(Some((e.syntax_version, e.edit_epoch, map)));
        e.keep_highlights_before(2);
        {
            let c = e.hl_run_cache.borrow();
            let (_, _, kept) = c.as_ref().expect("the table survives the epoch bump");
            assert!(kept.contains_key(&0) && kept.contains_key(&1));
            assert!(!kept.contains_key(&2), "the edited line kept its runs");
        }
        // Another parse is a different answer for every line.
        e.syntax_version += 1;
        e.keep_highlights_before(2);
        assert!(e.hl_run_cache.borrow().is_none());
    }

    /// 1271 颜色高亮: `#RGB`, `#RRGGBB` and
    /// `#RRGGBBAA` each become one band painted
    /// in the literal's own colour.
    #[test]
    fn color_bands_read_all_three_hex_widths() {
        let e = editor("#f00 #ff0000 #ff0000ff");
        let len = e.doc.line_len(0);
        let bands = e.color_literal_bands(0, 0, len);
        assert_eq!(bands.len(), 3);
        assert_eq!(bands[0], (0, 4, Color::rgb(255, 0, 0)));
        assert_eq!(bands[1], (5, 12, Color::rgb(255, 0, 0)));
        assert_eq!(bands[2], (13, 22, Color::rgba(255, 0, 0, 255)));
    }

    /// 1271: the `rgb()` / `rgba()` functions, with
    /// a fractional alpha rounding to a byte.
    #[test]
    fn color_bands_read_the_rgb_functions() {
        let e = editor("rgb(1, 2, 3) rgba(4, 5, 6, 0.5)");
        let len = e.doc.line_len(0);
        let bands = e.color_literal_bands(0, 0, len);
        assert_eq!(bands.len(), 2);
        assert_eq!(bands[0], (0, 12, Color::rgba(1, 2, 3, 255)));
        assert_eq!(bands[1], (13, 31, Color::rgba(4, 5, 6, 128)));
    }

    /// 1271: wrong hex widths, non-hex digits and an
    /// unterminated `rgb(` are all rejected.
    #[test]
    fn color_bands_reject_wrong_widths_and_garbage() {
        let e = editor("#ff #ggg #12345 rgb(1, 2");
        let len = e.doc.line_len(0);
        assert!(e.color_literal_bands(0, 0, len).is_empty());
    }

    /// Which the painter then hides off the caret row.
    #[test]
    fn grep_rows_take_their_classes_from_the_query() {
        let mut q = dg_core::filesearch::SearchQuery::new("alpha", "E:\\x");
        q.case_sensitive = true;
        let mut e = Editor::new();
        e.open_text("@~a.txt\n\t(3,1):x alphay\n\t共找到1行.");
        e.grep_rows = Some(dg_core::filesearch::RowHighlighter::from_query(&q));
        let t = e.theme.clone();
        let plain = t.foreground.to_u32();
        assert_eq!(
            e.highlight_for(0),
            Some(vec![(plain, 2, 0), (t.url_fg.to_u32(), 5, 0)]),
            "the two marker characters keep the row's own colour, the path is `Url`"
        );
        assert_eq!(
            e.highlight_for(1),
            Some(vec![
                (t.word2_fg.to_u32(), 7, 0),
                (plain, 2, 0),
                (t.found_fg.to_u32(), 5, 0),
                (plain, 1, 0),
            ]),
            "`\\t(3,1):` is `Word2`, then the row's own text with the hit in `Found`"
        );
        assert_eq!(e.marks_for(1), vec![(9, 14, t.found_bg, t.found_fg)]);
        // Only the path row carries class 30, and only on its marker.
        assert_eq!(
            e.flags_for(0),
            vec![
                dg_render::pipeline::CLASS_SUPPRESSED,
                dg_render::pipeline::CLASS_SUPPRESSED,
                0,
                0,
                0,
                0,
                0
            ]
        );
        assert!(e.flags_for(1).iter().all(|f| *f == 0));
        // A row neither rules matches — the totals — keeps the plain colours.
        assert_eq!(e.highlight_for(2), None);
        assert!(e.marks_for(2).is_empty());
    }

    /// 1271: only the line asked for is scanned.
    #[test]
    fn color_bands_scan_only_the_asked_line() {
        let e = editor("plain\n#abc\nmore");
        assert!(e.bands(0).is_empty());
        assert_eq!(
            e.bands(1),
            vec![(0, 4, Color::rgb(0xaa, 0xbb, 0xcc))]
        );
        assert!(e.bands(2).is_empty());
    }

    /// 1327 高亮网址和邮件: URLs and emails become
    /// spans in the theme's Url / Email colours,
    /// sorted by start.
    #[test]
    fn url_email_spans_match_both_families() {
        let e = editor("see https://e.com and www.x.org mail a@b.com");
        let spans = e.spans(0);
        assert_eq!(spans.len(), 3);
        assert_eq!((spans[0].0, spans[0].1), (4, 17));
        assert_eq!((spans[1].0, spans[1].1), (22, 31));
        assert_eq!((spans[2].0, spans[2].1), (37, 44));
        assert_eq!(spans[0].2, e.theme.url_bg);
        assert_eq!(spans[0].3, e.theme.url_fg);
        assert_eq!(spans[2].2, e.theme.email_bg);
        assert_eq!(spans[2].3, e.theme.email_fg);
    }

    /// 1327: span boundaries are char offsets, so a
    /// line of CJK text still maps the URL right.
    #[test]
    fn url_email_spans_map_bytes_to_chars_on_cjk() {
        // a space before the URL gives the `\b` of
        // the detector a boundary to hold on to
        let e = editor("漢字 https://e.com");
        let spans = e.spans(0);
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].0, spans[0].1), (3, 16));
    }

    /// 1313 选区高亮: a single-line selection becomes
    /// one band in the theme's selection colour.
    #[test]
    fn sel_highlight_bands_cover_a_single_line_selection() {
        let mut e = editor("hello");
        e.selection.add(Range {
            start: (0, 1),
            end: (0, 4),
        });
        let bands = e.sel_highlight_bands(0);
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].0, 1);
        assert_eq!(bands[0].1, 4);
        assert_eq!(
            bands[0].2,
            Color::rgb(
                e.theme.selection_bg.r,
                e.theme.selection_bg.g,
                e.theme.selection_bg.b,
            )
        );
    }

    /// 1313: multi-line and empty selections stay with
    /// the ordinary selection blend.
    #[test]
    fn sel_highlight_bands_skip_multiline_and_empty() {
        let mut e = editor("one\ntwo");
        e.selection.add(Range {
            start: (0, 1),
            end: (1, 1),
        });
        assert!(e.sel_highlight_bands(0).is_empty());
        assert!(e.sel_highlight_bands(1).is_empty());
        e.selection.clear();
        e.selection.add(Range {
            start: (0, 2),
            end: (0, 2),
        });
        assert!(e.sel_highlight_bands(0).is_empty());
    }

    /// 1313: the selected text's occurrences are painted in the
    /// Found colours, the selected range itself excluded (it
    /// keeps its own band).
    #[test]
    fn sel_highlight_occurrences_mark_every_match() {
        let mut e = editor("btn + btnx\nok btn ok\nplain");
        e.selection.add(Range {
            start: (0, 0),
            end: (0, 3),
        });
        e.update_sel_highlight();
        // line 0: the selection at 0..3 is skipped, "btn" inside
        // "btnx" still matches (the word gate is off).
        let occ = e.sel_highlight_occurrences(0);
        assert_eq!(occ.len(), 1);
        assert_eq!((occ[0].0, occ[0].1), (6, 9));
        assert_eq!(occ[0].2, e.theme.found_bg);
        assert_eq!(occ[0].3, e.theme.found_fg);
        // line 1: the standalone "btn" matches.
        let occ = e.sel_highlight_occurrences(1);
        assert_eq!(occ.len(), 1);
        assert_eq!((occ[0].0, occ[0].1), (3, 6));
        // line 2: no match.
        assert!(e.sel_highlight_occurrences(2).is_empty());
    }

    /// 「移动光标时保持选区或者查找高亮」：关掉（出厂值）时选区一塌，全文高亮
    /// 跟着没；打开时留着上一次选的那个字，直到下一次真选出新的字。
    #[test]
    fn keep_highlight_survives_a_collapsed_selection() {
        for keep in [false, true] {
            let mut e = editor("btn + btnx\nok btn ok\nplain");
            e.set_keep_highlight(keep);
            e.selection.add(Range {
                start: (0, 0),
                end: (0, 3),
            });
            e.update_sel_highlight();
            assert!(!e.sel_highlight_occurrences(1).is_empty(), "选中的时候总该有");
            // 光标一动，选区塌掉。
            e.selection.clear();
            e.update_sel_highlight();
            assert_eq!(
                !e.sel_highlight_occurrences(1).is_empty(),
                keep,
                "KeepHighlight={keep}"
            );
            // 真选出新的字，两种姿态都跟着换。
            e.selection.clear();
            e.selection.add(Range {
                start: (1, 0),
                end: (1, 2),
            });
            e.update_sel_highlight();
            assert!(e.sel_highlight_occurrences(0).is_empty(), "换词后要按新的词来");
            assert_eq!(e.sel_highlight_occurrences(1).len(), 1, "同一行的另一处 ok");
        }
    }

    /// 1313: a multi-line selection, an all-whitespace one and
    /// one over `Edit/MaxSelHighlight` all clear the pattern —
    /// the trigger's own filter chain.
    #[test]
    fn sel_highlight_occurrences_clear_on_invalid_selections() {
        let long: String = "a".repeat(65);
        let text = format!("{long}\nx\n");
        let mut e = editor(&text);
        // multi-line
        e.selection.add(Range {
            start: (0, 0),
            end: (1, 1),
        });
        e.update_sel_highlight();
        assert!(e.sel_highlight_pat.is_none());
        assert!(e.sel_highlight_occurrences(0).is_empty());
        // over the 64-char ceiling
        e.selection.clear();
        e.selection.add(Range {
            start: (0, 0),
            end: (0, 65),
        });
        e.update_sel_highlight();
        assert!(e.sel_highlight_pat.is_none());
        // exactly at the ceiling is fine
        e.selection.clear();
        e.selection.add(Range {
            start: (0, 0),
            end: (0, 64),
        });
        e.update_sel_highlight();
        assert!(e.sel_highlight_pat.is_some());
        // all-whitespace
        let mut e = editor("  x  ");
        e.selection.add(Range {
            start: (0, 0),
            end: (0, 2),
        });
        e.update_sel_highlight();
        assert!(e.sel_highlight_pat.is_none());
    }

    /// 1313: `Edit/QuickCase` off folds case, so "BTN" selects
    /// "btn" occurrences.
    #[test]
    fn sel_highlight_occurrences_fold_case_when_quickcase_off() {
        let mut e = editor("BTN ok btn ok");
        e.selection.add(Range {
            start: (0, 0),
            end: (0, 3),
        });
        e.set_sel_match_case(false);
        e.update_sel_highlight();
        let occ = e.sel_highlight_occurrences(0);
        assert_eq!(occ.len(), 1);
        assert_eq!((occ[0].0, occ[0].1), (7, 10));
        // the shipped default is case-sensitive: no match for
        // the lower-case run.
        let mut e = editor("BTN ok btn ok");
        e.selection.add(Range {
            start: (0, 0),
            end: (0, 3),
        });
        e.update_sel_highlight();
        assert!(e.sel_highlight_occurrences(0).is_empty());
    }

    /// 1313: `Edit/QuickSmart` on gates the pattern on the
    /// selection's own word edges, and then every match on
    /// theirs — "but" inside "button" never matches.
    #[test]
    fn sel_highlight_occurrences_whole_word_gate() {
        let mut e = editor("button but abut");
        e.selection.add(Range {
            start: (0, 0),
            end: (0, 3),
        });
        // the selection "but" is not a whole word (the char
        // after it is 't'), so the flag stays off even with the
        // gate on: plain matching.
        e.set_sel_match_word(true);
        e.update_sel_highlight();
        let occ = e.sel_highlight_occurrences(0);
        assert_eq!(occ.len(), 2); // "but" at 7 and inside "abut" at 12
        // a whole-word selection only matches whole words: the
        // "but" inside "button" on line 0 is rejected by its
        // edges, the standalone one on line 1 matches.
        let mut e = editor("button ok\nbut x but");
        e.selection.add(Range {
            start: (1, 0),
            end: (1, 3),
        });
        e.set_sel_match_word(true);
        e.update_sel_highlight();
        assert!(e.sel_highlight_occurrences(0).is_empty());
        let occ = e.sel_highlight_occurrences(1);
        assert_eq!(occ.len(), 1);
        assert_eq!((occ[0].0, occ[0].1), (6, 9));
    }

    /// The fold marker's press toggles the region at its line:
    /// collapse hides the region's inner lines, the next press
    /// on the same marker unfolds them again.
    #[test]
    fn toggle_fold_at_round_trips_the_region() {
        let mut e = editor("fn f() {\n    a;\n    b;\n}\n");
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        let wrapper = CharWrapperEx { chars: 80 };
        // no region opens on line 0 until the grammar says so —
        // indent folding derives one from the deeper line.
        e.folding_method = FoldingMethod::Indent;
        assert!(e.toggle_fold_at(0, &wrapper, &mut ctx));
        assert!(e.folding.is_folded(1));
        assert!(e.folding.is_folded(2));
        assert!(!e.folding.is_folded(3));
        // the marker is now collapsed: the next press unfolds.
        assert!(e.toggle_fold_at(0, &wrapper, &mut ctx));
        assert!(!e.folding.is_folded(1));
        assert!(!e.folding.is_folded(2));
        // a line with no region changes nothing.
        assert!(!e.toggle_fold_at(1, &wrapper, &mut ctx));
    }

    /// 1326 括弧高亮: the caret's bracket and its
    /// partner each become a one-char span in the
    /// Pair colours.
    #[test]
    fn recolor_spans_paint_the_bracket_pair() {
        let e = editor("if (a) { b }");
        let pair = Some(((0, 3), (0, 9)));
        let spans = e.recolor_spans(0, 0, e.doc.line_len(0), HighlightToggles::default(), pair);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0], (3, 4, e.theme.pair_bg, e.theme.tag_fg));
        assert_eq!(spans[1], (9, 10, e.theme.pair_bg, e.theme.tag_fg));
    }

    /// 1326: a partner on another line paints only on
    /// its own line.
    #[test]
    fn recolor_spans_paint_each_end_on_its_own_line() {
        let e = editor("if (a)\n{ b }");
        let pair = Some(((0, 3), (1, 0)));
        let off = HighlightToggles::default();
        assert_eq!(e.recolor_spans(0, 0, e.doc.line_len(0), off, pair).len(), 1);
        assert_eq!(e.recolor_spans(1, 0, e.doc.line_len(1), off, pair).len(), 1);
        assert!(e.recolor_spans(0, 0, e.doc.line_len(0), off, None).is_empty());
    }

    /// 1326/1327: with the URL family armed its spans
    /// merge with the bracket pair, sorted by start.
    #[test]
    fn recolor_spans_merge_url_spans_when_armed() {
        let e = editor("a@b.com https://e.com");
        let mut h = HighlightToggles::default();
        h.url = true;
        let spans = e.recolor_spans(0, 0, e.doc.line_len(0), h, None);
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].0, spans[0].1), (0, 7));
        assert_eq!((spans[1].0, spans[1].1), (8, 21));
        assert_eq!(spans[0].2, e.theme.email_bg);
        assert_eq!(spans[1].2, e.theme.url_bg);
    }

    /// Everything an incremental relayout must keep consistent with
    /// a from-scratch one, line by line.
    #[derive(PartialEq, Debug)]
    struct LayoutMetrics {
        total_rows: usize,
        row_starts: Vec<usize>,
        wrap_data: Vec<Vec<i32>>,
        max_cells: i32,
    }

    fn metrics(ed: &Editor) -> LayoutMetrics {
        let n = ed.doc.line_count();
        LayoutMetrics {
            total_rows: ed.total_rows,
            row_starts: (0..n).map(|l| ed.rows.line_to_visual_row(l)).collect(),
            wrap_data: ed
                .wrap
                .store
                .arrays
                .iter()
                .map(|a| a.data.clone())
                .collect(),
            max_cells: ed.max_cells(),
        }
    }

    /// The keystroke path: apply an op, resync syntax, take the
    /// recorded line span and relayout just it (falling back to the
    /// full relayout when no span was recorded — exactly what
    /// `relayout_after_edit` does in the win32 backend).
    fn apply_and_relayout(
        ed: &mut Editor,
        measurer: &mut CachedMeasurer<TableMeasurer>,
        wrapper: &dyn Wrapper,
        op: EditOp,
    ) {
        ed.apply_edit(op);
        ed.sync_syntax();
        let mut ctx = LayoutCtx {
            measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        let regions = ed.take_layout_regions();
        if regions.is_empty() {
            ed.relayout(wrapper, &mut ctx);
        } else {
            for (first, old_lines, new_lines) in regions {
                ed.relayout_region(first, old_lines, new_lines, wrapper, &mut ctx);
            }
        }
    }

    /// The typing path defers the parse: edits land (and the tree
    /// is folded to the new offsets) while the parse rides the
    /// debounce timer. A burst of K edits between parses must
    /// produce exactly the same colors as parsing after every
    /// edit, and the paint against the folded tree must never
    /// panic or outrun the mirror text.
    #[test]
    fn deferred_syntax_sync_matches_immediate_sync() {
        let lang = tree_sitter::Language::from(tree_sitter_json::LANGUAGE);
        let text = String::from("{\n  \"a\": 1,\n  \"b\": [true, null, \"x\"]\n}\n");
        let mut deferred = Editor::new();
        deferred.open_text(&text);
        deferred.attach_syntax(lang.clone());
        let mut immediate = Editor::new();
        immediate.open_text(&text);
        immediate.attach_syntax(lang);

        let mut seed = 0x0BAD_F00D_1234_5678u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet = ['{', '}', '"', 'a', '1', ':', ',', ' ', '\n', '[', ']'];
        for step in 0..300u32 {
            let pick = |next: &mut dyn FnMut() -> u64, ed: &Editor| -> Pos {
                let lines = ed.doc.line_count().max(1);
                let line = (next() as usize) % lines;
                let col = (next() as usize) % (ed.doc.line_len(line) + 1);
                Pos { line, col }
            };
            let op = {
                match next() % 3 {
                    0 => EditOp::Insert {
                        pos: pick(&mut next, &deferred),
                        text: alphabet[(next() as usize) % alphabet.len()].to_string(),
                    },
                    1 if deferred.doc.line_count() > 1 => {
                        let a = pick(&mut next, &deferred);
                        let b = pick(&mut next, &deferred);
                        let (a, b) = if a <= b { (a, b) } else { (b, a) };
                        let old = deferred.doc.range_text(a, b);
                        EditOp::Delete {
                            start: a,
                            end: b,
                            text: old,
                        }
                    }
                    _ => {
                        let a = pick(&mut next, &deferred);
                        let b = pick(&mut next, &deferred);
                        let (a, b) = if a <= b { (a, b) } else { (b, a) };
                        let ins = alphabet[(next() as usize) % alphabet.len()].to_string();
                        let old = deferred.doc.range_text(a, b);
                        EditOp::Replace {
                            start: a,
                            end: b,
                            old_text: old,
                            new_text: ins,
                        }
                    }
                }
            };
            immediate.apply_edit(op.clone());
            immediate.sync_syntax();
            deferred.apply_edit(op);
            // The paint against the folded tree is what a
            // keystroke triggers before the timer fires: it must
            // be readable for every line, not panic.
            for line in 0..deferred.doc.line_count() {
                let _ = deferred.highlight_for(line);
            }
            assert!(
                deferred.syntax_dirty(),
                "step {step}: parse should be pending"
            );
            // Debounce fires every third keystroke.
            if step % 3 == 2 {
                deferred.sync_syntax();
                assert!(
                    !deferred.syntax_dirty(),
                    "step {step}: sync must clear the pending flag"
                );
            }
            assert_eq!(
                deferred.doc.to_text(),
                immediate.doc.to_text(),
                "step {step}"
            );
            // Colors are only asserted once the debounce has fired:
            // between keystrokes the tree legitimately carries the
            // previous tokenization (the primary check for that
            // window is that painting never panics above).
            if step % 3 == 2 {
                let lines = deferred.doc.line_count();
                for line in 0..lines {
                    assert_eq!(
                        deferred.highlight_for(line),
                        immediate.highlight_for(line),
                        "step {step} line {line}: deferred colors trailing the immediate ones"
                    );
                }
            }
        }
    }

    #[test]
    fn incremental_relayout_matches_full_relayout() {
        let wrapper = CharWrapperEx { chars: 8 };
        let mut m = CachedMeasurer::new(TableMeasurer::default());
        let mut ed = Editor::new();
        ed.open_text("fn main() {\n    let 世界 = \"héllo\";\n}\n");
        // First layout is full, like the load path.
        {
            let mut ctx = LayoutCtx {
                measurer: &mut m,
                font: Font::Primary,
                tab_px: 32,
            };
            ed.relayout(&wrapper, &mut ctx);
        }
        let mut seed = 0xDEAD_BEEF_1234_5678u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet = ['x', '\n', '(', '世', 'é', ' ', '\t', '"'];
        let pick = |next: &mut dyn FnMut() -> u64, doc: &dg_core::Document| -> Pos {
            let lines = doc.line_count().max(1);
            let line = (next() as usize) % lines;
            let col = (next() as usize) % (doc.line_len(line) + 1);
            Pos { line, col }
        };
        for step in 0..400 {
            let op = match next() % 4 {
                0 => EditOp::Insert {
                    pos: pick(&mut next, &ed.doc),
                    text: alphabet[(next() as usize) % alphabet.len()].to_string(),
                },
                1 => {
                    let len = 1 + (next() as usize) % 10;
                    let text: String = (0..len)
                        .map(|_| alphabet[(next() as usize) % alphabet.len()])
                        .collect();
                    EditOp::Insert {
                        pos: pick(&mut next, &ed.doc),
                        text,
                    }
                }
                2 if ed.doc.line_count() > 1 => {
                    let a = pick(&mut next, &ed.doc);
                    let b = pick(&mut next, &ed.doc);
                    let (a, b) = if a <= b { (a, b) } else { (b, a) };
                    let old = ed.doc.range_text(a, b);
                    EditOp::Delete {
                        start: a,
                        end: b,
                        text: old,
                    }
                }
                _ => {
                    let a = pick(&mut next, &ed.doc);
                    let b = pick(&mut next, &ed.doc);
                    let (a, b) = if a <= b { (a, b) } else { (b, a) };
                    let old = ed.doc.range_text(a, b);
                    let len = 1 + (next() as usize) % 6;
                    let new: String = (0..len)
                        .map(|_| alphabet[(next() as usize) % alphabet.len()])
                        .collect();
                    EditOp::Replace {
                        start: a,
                        end: b,
                        old_text: old,
                        new_text: new,
                    }
                }
            };
            apply_and_relayout(&mut ed, &mut m, &wrapper, op);
            // Oracle: a from-scratch relayout of the same text.
            let incremental = metrics(&ed);
            {
                let mut ctx = LayoutCtx {
                    measurer: &mut m,
                    font: Font::Primary,
                    tab_px: 32,
                };
                ed.relayout(&wrapper, &mut ctx);
            }
            let full = metrics(&ed);
            assert_eq!(
                incremental, full,
                "incremental relayout diverged at step {step}"
            );
        }
    }
}

#[cfg(test)]
mod change_mark_tests {
    use super::*;
    use dg_layout::measure::TableMeasurer;
    use dg_layout::wrap::WinEdgeWrapper;

    /// A laid-out editor over `n` lines, none of them edited.
    fn laid_out(n: usize) -> Editor {
        let text: String = (0..n).map(|i| format!("line {i}\n")).collect();
        let mut ed = Editor::new();
        ed.open_text(&text);
        let mut measurer = dg_layout::measure::CachedMeasurer::new(TableMeasurer::default());
        let wrapper = WinEdgeWrapper {
            width_px: 4000,
            word_wrap: false,
        };
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        ed.relayout(&wrapper, &mut ctx);
        ed
    }

    /// 修改行标记: an edit deep in a long file puts one mark on the bar, at the
    /// row of the line it landed on.
    #[test]
    fn an_edit_marks_its_own_row_on_the_bar() {
        let mut ed = laid_out(1000);
        assert!(ed.change_marks().is_empty(), "nothing edited yet");
        assert!(ed.apply_insert(Pos { line: 500, col: 0 }, "X"));
        let marks = ed.change_marks();
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert_eq!((marks[0].start, marks[0].end, marks[0].state), (500, 501, 1));
    }

    /// The save recolours the marks where they stand — the revision moves on the
    /// state, not on the position, which is what the bar's repaint key reads.
    #[test]
    fn the_save_recolours_the_marks_it_leaves_in_place() {
        let mut ed = laid_out(100);
        assert!(ed.apply_insert(Pos { line: 9, col: 0 }, "X"));
        assert_eq!(ed.change_marks()[0].state, 1);
        ed.doc.mark_all_saved();
        let marks = ed.change_marks();
        assert_eq!((marks[0].start, marks[0].end, marks[0].state), (9, 10, 2));
    }

    /// A wrapped line owns several rows, and its mark has to cover all of them:
    /// the bar shows where the change is, not where its first row is.
    #[test]
    fn a_wrapped_line_marks_every_row_it_takes() {
        let long = "word ".repeat(200);
        let text = format!("first\n{long}\nlast");
        let mut ed = Editor::new();
        ed.open_text(&text);
        let mut measurer = dg_layout::measure::CachedMeasurer::new(TableMeasurer::default());
        let wrapper = WinEdgeWrapper {
            width_px: 400,
            word_wrap: true,
        };
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        ed.relayout(&wrapper, &mut ctx);
        assert!(ed.apply_insert(Pos { line: 1, col: 0 }, "X"));
        let marks = ed.change_marks();
        let wrapped_rows = marks[0].end - marks[0].start;
        assert!(
            wrapped_rows > 1,
            "the middle line wraps over {wrapped_rows} rows"
        );
        assert_eq!(marks[0].start, ed.rows.line_to_visual_row(1) as i32);
    }
}

#[cfg(test)]
mod virtual_space_tests {
    use super::*;
    use dg_layout::measure::TableMeasurer;

    fn plain(text: &str) -> Editor {
        let mut editor = Editor::new();
        editor.open_text(text);
        let mut measurer = dg_layout::measure::CachedMeasurer::new(TableMeasurer::default());
        let wrapper = dg_layout::wrap::VirtualWrapper;
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px: 32,
        };
        editor.relayout(&wrapper, &mut ctx);
        editor
    }

    #[test]
    fn caret_past_eol_walks_whole_cells() {
        let mut ed = plain("abcde");
        ed.wrapped = false;
        let m = dg_render::pipeline::LineMetrics::new(&ed.theme, ed.tab_size as usize);
        // 行尾上的光标：像素落在第五格的右缘。
        ed.caret = (0, 5);
        let (_, at_end) = ed.caret_row();
        // 虚拟空格：往右的列按整格继续走。
        ed.caret = (0, 8);
        let (_, in_virtual) = ed.caret_row();
        assert_eq!(in_virtual - at_end, 3 * m.x);
    }

    #[test]
    fn caret_at_eol_is_not_past_it() {
        let mut ed = plain("abcde");
        ed.wrapped = false;
        ed.caret = (0, 5);
        let (_, at_end) = ed.caret_row();
        let m = dg_render::pipeline::LineMetrics::new(&ed.theme, ed.tab_size as usize);
        assert_eq!(at_end, 5 * m.x);
    }

    #[test]
    fn caret_column_beyond_the_line_on_a_wrapped_row_stays_put() {
        // 折行的行没有行尾空地：列不往外走，多出来的按整格折进去。
        let mut ed = plain("abcde");
        ed.wrapped = true;
        ed.caret = (0, 8);
        let m = dg_render::pipeline::LineMetrics::new(&ed.theme, ed.tab_size as usize);
        let (_, px) = ed.caret_row();
        assert_eq!(px, 5 * m.x);
    }

    #[test]
    fn a_caret_stepped_one_column_at_a_time_lands_where_a_cold_measure_puts_it() {
        // The step path answers from the pixel the caret was last at. Every
        // column of the line, walked one at a time, has to agree with the
        // answer a caret that arrives there with no cache behind it gets.
        for text in ["abcdefghijklmnopqrstuvwxyz0123456789", "a\tbcdefghij\tklmnop"] {
            let mut ed = plain(text);
            ed.wrapped = false;
            let len = text.chars().count();
            for col in 1..=len {
                ed.caret = (0, col);
                let (_, stepped) = ed.caret_row();
                let mut fresh = plain(text);
                fresh.wrapped = false;
                fresh.caret = (0, col);
                let (_, cold) = fresh.caret_row();
                assert_eq!(stepped, cold, "{text:?} column {col}");
            }
        }
    }

    #[test]
    fn an_edit_at_the_caret_keeps_the_cached_pixel_honest() {
        // The cached pixel describes a column, not a document: typing at the
        // caret leaves every character in front of it alone, so the next read
        // steps one advance from the cache. It has to agree with a cold
        // measure of the same column.
        let mut ed = plain("abcdefghij");
        ed.wrapped = false;
        ed.caret = (0, 6);
        let (_, before) = ed.caret_row();
        assert!(ed.apply_insert(Pos { line: 0, col: 6 }, "x"));
        ed.caret = (0, 7);
        let (_, stepped) = ed.caret_row();
        let m = dg_render::pipeline::LineMetrics::new(&ed.theme, ed.tab_size as usize);
        assert_eq!(stepped, before + m.advance('x'));
        let mut fresh = plain("abcdefxghij");
        fresh.wrapped = false;
        fresh.caret = (0, 7);
        assert_eq!(fresh.caret_row().1, stepped);
        // A delete in front of the caret moves it, and the entry describes a
        // column whose text changed — it must not be reused.
        assert!(ed.apply_delete(Pos { line: 0, col: 0 }, Pos { line: 0, col: 2 }));
        ed.caret = (0, 5);
        let (_, after_delete) = ed.caret_row();
        let mut fresh = plain("cdefxghij");
        fresh.wrapped = false;
        fresh.caret = (0, 5);
        assert_eq!(fresh.caret_row().1, after_delete);
    }
}
