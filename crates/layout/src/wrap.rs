use crate::measure::{attaches_to_previous, Font, TextMeasurer};

/// Layout context threaded through the wrap
/// strategies. Uses dynamic dispatch, so a new
/// wrap rule is a new implementor of
/// [`Wrapper`].
pub struct LayoutCtx<'a> {
    pub measurer: &'a mut dyn TextMeasurer,
    pub font: Font,
    pub tab_px: u32,
}

/// Wrap strategy — the trait object the layout
/// holds, whose `limit_px` answers where a line
/// must break. Selected by the wrap mode.
pub trait Wrapper {
    /// Maximum x before a break is forced, in
    /// pixels.
    fn limit_px(&self, ctx: &mut LayoutCtx) -> u32;

    /// Whether to break at word edges (spaces
    /// and punctuation) instead of at the
    /// exact overflow character.
    fn breaks_at_word_edges(&self) -> bool {
        true
    }

    /// Column budget (CharWrapperEx): when
    /// set, the engine counts display columns
    /// instead of measuring pixels.
    fn column_limit(&self) -> Option<u32> {
        None
    }

    /// The mode lays every line out as a single row and never measures one
    /// (不换行). The engine then skips the per-line walk entirely: the row's own
    /// paint measures only the window it needs, and the horizontal range comes
    /// from the line's char count.
    fn is_virtual(&self) -> bool {
        false
    }
}

/// mode 1 — window edge: width =
/// the view - the view, minimum
/// `8 * xWidth`; word-wrap iff the view.
pub struct WinEdgeWrapper {
    pub width_px: i32,
    pub word_wrap: bool,
}

impl Wrapper for WinEdgeWrapper {
    fn limit_px(&self, ctx: &mut LayoutCtx) -> u32 {
        let min = 8 * ctx.measurer.char_width('x', ctx.font) as u32;
        (self.width_px.max(min as i32) as u32).max(min)
    }

    fn breaks_at_word_edges(&self) -> bool {
        self.word_wrap
    }
}

/// mode 2 — fixed pixel budget, word edges.
pub struct WordEdgeWrapper {
    pub limit: u32,
}

impl Wrapper for WordEdgeWrapper {
    fn limit_px(&self, _ctx: &mut LayoutCtx) -> u32 {
        self.limit
    }
}

/// modes 3–5 — N characters (converted via
/// average width).
pub struct CharWrapper {
    pub chars: u32,
}

impl Wrapper for CharWrapper {
    fn limit_px(&self, ctx: &mut LayoutCtx) -> u32 {
        self.chars * ctx.measurer.avg_width(ctx.font) as u32
    }

    fn breaks_at_word_edges(&self) -> bool {
        false
    }
}

/// mode 4 — N display columns, tab-aware.
/// Break points are exclusive, so a
/// single-width line wraps to exactly
/// `chars` columns per row.
pub struct CharWrapperEx {
    pub chars: u32,
}

impl Wrapper for CharWrapperEx {
    fn limit_px(&self, ctx: &mut LayoutCtx) -> u32 {
        self.chars * ctx.measurer.avg_width(ctx.font) as u32
    }

    fn breaks_at_word_edges(&self) -> bool {
        false
    }

    fn column_limit(&self) -> Option<u32> {
        Some(self.chars)
    }
}

/// mode 6 — fixed pixels (printing).
pub struct PixelWrapper {
    pub px: u32,
}

impl Wrapper for PixelWrapper {
    fn limit_px(&self, _ctx: &mut LayoutCtx) -> u32 {
        self.px
    }

    fn breaks_at_word_edges(&self) -> bool {
        false
    }
}

/// Placeholder before the first layout
/// (`VirtualWrapper`).
pub struct VirtualWrapper;

impl Wrapper for VirtualWrapper {
    fn limit_px(&self, _ctx: &mut LayoutCtx) -> u32 {
        u32::MAX
    }

    fn is_virtual(&self) -> bool {
        true
    }
}

/// Break-position array: the capacity, the size,
/// the position array and a count.
/// Capacity grows to `2n+4` whenever the
/// array fills.
#[derive(Default)]
pub struct WrapArray {
    pub data: Vec<i32>,
    pub dirty: bool,
    /// Display columns of the line (CJK counts two), measured during
    /// the wrap walk — the horizontal-scroll range source, kept here
    /// so sizing the scrollbar never re-measures the text.
    pub cells: i32,
}

impl WrapArray {
    pub fn count(&self) -> usize {
        self.data.len()
    }

    pub fn reset(&mut self) {
        self.data.clear();
        self.dirty = false;
        self.cells = 0;
    }

    pub fn append(&mut self, pos: i32) {
        // parity: capacity 2n+4
        if self.data.len() == self.data.capacity() {
            let cap = self.data.len() * 2 + 4;
            self.data.reserve(cap.saturating_sub(self.data.len()));
        }
        self.data.push(pos);
    }
}

/// Per-folding-line wrap cache — parity with
/// `GapBufferPointer<WrapPos*>` at the view.
#[derive(Default)]
pub struct WrapStore {
    pub arrays: Vec<WrapArray>,
}

impl WrapStore {
    /// Grows the store to at least `lines` arrays. Never shrinks:
    /// `wrap_line` ensures `fline + 1` per line, and a shrinking
    /// ensure would drop the arrays of the lines it has not reached
    /// yet (the incremental relayout walks lines out of order).
    pub fn ensure(&mut self, lines: usize) {
        if self.arrays.len() < lines {
            self.arrays.resize_with(lines, WrapArray::default);
        }
    }

    /// Drops arrays past `lines` (after a document swap shrank the
    /// line count); the full relayout calls both.
    pub fn truncate(&mut self, lines: usize) {
        self.arrays.truncate(lines);
    }

    pub fn mark_dirty(&mut self, line: usize) {
        if let Some(a) = self.arrays.get_mut(line) {
            a.dirty = true;
        }
    }

    pub fn mark_range_dirty(&mut self, start: usize, end: usize) {
        for a in self.arrays.iter_mut().take(end).skip(start) {
            a.dirty = true;
        }
    }

    /// Inserts `count` fresh arrays at `at`, shifting the rest right
    /// (an edit split one line into several). The shifted arrays keep
    /// their wrap data — only the inserted lines need re-wrapping.
    pub fn insert_lines(&mut self, at: usize, count: usize) {
        if self.arrays.len() < at {
            self.arrays.resize_with(at, WrapArray::default);
        }
        for i in 0..count {
            self.arrays.insert(at + i, WrapArray::default());
        }
    }

    /// Removes `count` arrays at `at` (an edit joined lines).
    pub fn remove_lines(&mut self, at: usize, count: usize) {
        let end = (at + count).min(self.arrays.len());
        if at < self.arrays.len() {
            self.arrays.drain(at..end);
        }
    }
}

/// The wrap engine — the per-line wrap loops
/// and the full relayout.
pub struct WrapEngine {
    pub store: WrapStore,
    /// 断行禁则：不许顶在一行之首的字符（`Edit/NoHeadChars`）。
    pub break_head: Vec<char>,
    /// 断行禁则：不许留在一行之尾的字符（`Edit/NoTailChars`）。
    pub break_tail: Vec<char>,
}

impl Default for WrapEngine {
    fn default() -> Self {
        Self {
            store: WrapStore::default(),
            break_head: Vec::new(),
            break_tail: Vec::new(),
        }
    }
}

impl WrapEngine {
    /// 把断点挪到不犯禁则的位置上。`at` 是下一行的第一个字符，所以 `at` 那一格
    /// 不能是行头禁排字，`at - 1` 那一格不能是行尾禁排字。犯了就往前挪一格再看，
    /// 最多回退 32 格；`min_at` 是这一行至少得吃下的字符数，退到那里还躲不开就
    /// 停在原地 —— 一整段都是禁则字符时，宁可让它犯一次，也不能排出一个空行。
    ///
    /// 关联函数而不是方法：调用点上 `store.arrays[fline]` 正被可变借用，走
    /// `&self` 会撞车，而按字段分开借 `break_head` / `break_tail` 是允许的。
    fn land(
        at: usize,
        min_at: usize,
        chars: &[char],
        head: &[char],
        tail: &[char],
    ) -> usize {
        if head.is_empty() && tail.is_empty() {
            return at;
        }
        let bad = |i: usize| -> bool {
            chars.get(i).is_some_and(|c| head.contains(c))
                || i > 0 && chars.get(i - 1).is_some_and(|c| tail.contains(c))
        };
        let mut at = at;
        for _ in 0..32 {
            if at <= min_at || at > chars.len() || !bad(at) {
                break;
            }
            at -= 1;
        }
        at
    }

    /// 断行禁则要不要读这一行的字符：两串都空时一个字符也不该多解出来。
    fn rules_on(&self) -> bool {
        !self.break_head.is_empty() || !self.break_tail.is_empty()
    }

    /// Wraps one folding line; returns the
    /// visual rows it contributes
    /// (`count + 1`).
    pub fn wrap_line(
        &mut self,
        fline: usize,
        text: &str,
        wrapper: &dyn Wrapper,
        ctx: &mut LayoutCtx,
    ) -> usize {
        // 不换行: one row, no break array and no walk over the text — the row's
        // own paint measures the window it draws.
        if wrapper.is_virtual() {
            return self.wrap_line_virtual(fline, text.chars().count() as i32);
        }
        // mode 4 (CharWrapperEx): display
        // columns instead of pixels
        if let Some(cols) = wrapper.column_limit() {
            return self.wrap_line_columns(fline, text, cols, ctx);
        }
        // 断行禁则（`Edit/NoHeadChars` / `Edit/NoTailChars`）要按字符看每个断点，
        // 两串都空时一个字符也不该多解出来。
        let rules = self.rules_on();
        let chars: Vec<char> = if rules {
            text.chars().collect()
        } else {
            Vec::new()
        };
        self.store.ensure(fline + 1);
        let arr = &mut self.store.arrays[fline];
        arr.reset();
        let limit = wrapper.limit_px(ctx);
        let word_wrap = wrapper.breaks_at_word_edges();
        let x_width = ctx.measurer.char_width('x', ctx.font) as u32;
        let tab_px = ctx.tab_px.max(x_width);

        let mut x: u32 = 0;
        let mut last_break: Option<usize> = None;
        let mut break_x: u32 = 0;
        let mut i = 0usize;
        // 上一个断点：禁则往前挪时不许越过它，否则这一行就成了空行。
        let mut prev_break = 0usize;
        // Line width in x-width units (tabs count their snap), for
        // the scrollbar range — mirrors `cells_of`.
        let mut width_px: u32 = 0;

        for ch in text.chars() {
            // CR/LF stop the walk — the display
            // row ends at the control character
            // (defensive branch: Document.line_text
            // already strips the terminator).
            if ch == '\r' || ch == '\n' {
                arr.append(i as i32);
                arr.cells = cells_of_px(width_px, x_width);
                return arr.count() + 1;
            }
            // TAB: snap to the next tab stop,
            // min advance 1*xWidth
            if ch == '\t' {
                x = (x / tab_px + 1) * tab_px;
                width_px += tab_px;
                // A snap past the limit moves the tab
                // to the next row, where it re-snaps
                // from zero.
                if x > limit {
                    let at = Self::land(i, prev_break + 1, &chars, &self.break_head, &self.break_tail);
                    prev_break = at;
                    arr.append(at as i32);
                    x = if at == i {
                        tab_px
                    } else {
                        width_from(&chars, at, i, ctx.measurer, ctx.font, tab_px)
                    };
                    last_break = None;
                    break_x = 0;
                }
                i += 1;
                continue;
            }
            // x before this character: the restart
            // point of a char-mode break.
            let pre_x = x;
            // SPACE (and, in word mode, other
            // word edges) records the last
            // breakable position.
            if ch == ' ' || (word_wrap && is_word_break(ch)) {
                last_break = Some(i);
                break_x = pre_x;
            }
            let w = ctx.measurer.char_width(ch, ctx.font) as u32;
            width_px += w;
            x += w;
            if x > limit {
                let raw = last_break.unwrap_or(i);
                let at = Self::land(raw, prev_break + 1, &chars, &self.break_head, &self.break_tail);
                prev_break = at;
                arr.append(at as i32);
                // restart from the break point: word
                // mode carries the width from the break
                // to the current char, char mode just
                // this character. 禁则把断点往前挪过时，那两笔账记的都是原来
                // 的落点，只能照新的重量一遍。
                x = if at != raw {
                    width_from(&chars, at, i, ctx.measurer, ctx.font, tab_px)
                } else if last_break.is_some() {
                    x - break_x
                } else {
                    x - pre_x
                };
                last_break = None;
                break_x = 0;
            }
            i += 1;
        }
        arr.cells = cells_of_px(width_px, x_width);
        arr.count() + 1
    }

    /// 不换行: the whole line is one row, so there is no break array to build and
    /// nothing to measure. `cells` is the line's char count — the horizontal
    /// range's lower bound, and the store answers it without decoding the line.
    pub fn wrap_line_virtual(&mut self, fline: usize, cells: i32) -> usize {
        self.store.ensure(fline + 1);
        let arr = &mut self.store.arrays[fline];
        arr.reset();
        arr.cells = cells;
        1
    }

    /// mode 4 wrap loop:
    /// accumulate display columns (tab snaps
    /// to the next tab stop) and break whenever
    /// the budget is exhausted and more
    /// characters follow. Break points are
    /// exclusive (`i + 1`) — this implementation's
    /// `WrapArray` convention — so a
    /// single-width line wraps to exactly
    /// `max_cols` columns per row.
    fn wrap_line_columns(
        &mut self,
        fline: usize,
        text: &str,
        max_cols: u32,
        ctx: &mut LayoutCtx,
    ) -> usize {
        // 断行禁则，和像素那一趟同一条规则。
        let rules = self.rules_on();
        let chars: Vec<char> = if rules {
            text.chars().collect()
        } else {
            Vec::new()
        };
        self.store.ensure(fline + 1);
        let arr = &mut self.store.arrays[fline];
        arr.reset();
        let x_width = ctx.measurer.char_width('x', ctx.font).max(1) as u32;
        let tab_cols = (ctx.tab_px / x_width).max(1);
        let char_count = text.chars().count();
        let mut col: u32 = 0;
        let mut total_cols: u32 = 0;
        let mut prev_break = 0usize;
        for (i, ch) in text.chars().enumerate() {
            // CR/LF ends the walk (the line
            // terminator is stripped by
            // Document::line_text anyway)
            if ch == '\r' || ch == '\n' {
                break;
            }
            let advance = if ch == '\t' {
                tab_cols - (col % tab_cols)
            } else if attaches_to_previous(ch) {
                // A modifier is not a column: `☕️` wraps where `☕` does.
                0
            } else {
                1
            };
            col += advance;
            total_cols += advance;
            if col >= max_cols && i + 1 < char_count {
                let raw = i + 1;
                let at = Self::land(raw, prev_break + 1, &chars, &self.break_head, &self.break_tail);
                prev_break = at;
                arr.append(at as i32);
                // 挪过位置的那一行，开头的列数按同一套推进量重走一遍；没挪时
                // `at == raw`，下一行从 0 开始，和原来一样。
                col = if at == raw {
                    0
                } else {
                    let mut c = 0u32;
                    for k in at..=i {
                        c += if chars[k] == '\t' {
                            tab_cols - (c % tab_cols)
                        } else if attaches_to_previous(chars[k]) {
                            0
                        } else {
                            1
                        };
                    }
                    c
                };
            }
        }
        arr.cells = total_cols as i32;
        arr.count() + 1
    }

    /// Full relayout: wraps every
    /// line and sums the per-line counts into
    /// the total visual-row count.
    pub fn relayout<'a>(
        &mut self,
        lines: impl ExactSizeIterator<Item = &'a str>,
        wrapper: &dyn Wrapper,
        ctx: &mut LayoutCtx,
    ) -> usize {
        let n = lines.len();
        self.store.ensure(n);
        let mut total = 0;
        for (fline, text) in lines.enumerate() {
            total += self.wrap_line(fline, text, wrapper, ctx);
        }
        total
    }
}

/// Word-edge break detection for
/// `WordEdgeWrapper` / word mode: whitespace
/// or punctuation.
pub fn is_word_break(ch: char) -> bool {
    ch.is_whitespace() || ch.is_ascii_punctuation()
}

/// 断点被禁则往前挪过之后，新行开头的宽度只能重新量：原来那套累加记的是别的
/// 落点，`break_x` 停在的是那个可断点的 x，往前退一格就少算了若干格。Tab 在
/// 新行开头重新对齐一个制表位，和原来那条规则一致。
fn width_from(
    chars: &[char],
    from: usize,
    thru: usize,
    measurer: &mut dyn TextMeasurer,
    font: Font,
    tab_px: u32,
) -> u32 {
    let mut w = 0u32;
    for &c in &chars[from..=thru] {
        w += if c == '\t' {
            tab_px
        } else {
            measurer.char_width(c, font) as u32
        };
    }
    w
}

/// Pixels → display columns, the `cells_of`
/// basis (one x-width unit per column, CJK
/// glyphs count two).
pub fn cells_of_px(px: u32, x_width: u32) -> i32 {
    let xw = x_width.max(1) as f32;
    (px as f32 / xw).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::{CachedMeasurer, TableMeasurer};

    fn engine_with(text: &str, wrapper: &dyn Wrapper, tab_px: u32) -> (WrapEngine, usize) {
        let mut engine = WrapEngine::default();
        let mut measurer = CachedMeasurer::new(TableMeasurer::default());
        let mut ctx = LayoutCtx {
            measurer: &mut measurer,
            font: Font::Primary,
            tab_px,
        };
        let rows = engine.wrap_line(0, text, wrapper, &mut ctx);
        (engine, rows)
    }

    #[test]
    fn char_wrapper_ex_wraps_every_n_columns() {
        let wrapper = CharWrapperEx { chars: 5 };
        // "hello" / " worl" / "d" — the
        // space is a display column too
        let (engine, rows) = engine_with("hello world", &wrapper, 32);
        assert_eq!(rows, 3);
        assert_eq!(engine.store.arrays[0].data, vec![5, 10]);
    }

    #[test]
    fn char_wrapper_ex_exact_multiple() {
        let wrapper = CharWrapperEx { chars: 5 };
        let (engine, rows) = engine_with("abcdefghij", &wrapper, 32);
        assert_eq!(rows, 2);
        assert_eq!(engine.store.arrays[0].data, vec![5]);
    }

    #[test]
    fn char_wrapper_ex_short_line_no_break() {
        let wrapper = CharWrapperEx { chars: 5 };
        let (engine, rows) = engine_with("abc", &wrapper, 32);
        assert_eq!(rows, 1);
        assert!(engine.store.arrays[0].data.is_empty());
    }

    #[test]
    fn char_wrapper_ex_tab_snap() {
        // tab_px=32, x_width=8 -> tab_cols=4;
        // "ab" is 2 cols, the tab advances 2
        // more, 'c' hits 5 -> break after 'c'
        let wrapper = CharWrapperEx { chars: 5 };
        let (engine, rows) = engine_with("ab\tcde", &wrapper, 32);
        assert_eq!(rows, 2);
        assert_eq!(engine.store.arrays[0].data, vec![4]);
    }

    /// A variation selector is not a display column, so a `☕️` wraps exactly
    /// where a `☕` does — one cell later and the row counts of two documents
    /// that look the same diverge. U+FE0F is the fourth character here: five
    /// columns are `a b ☕ VS16 c`, so the break after `c` is after six
    /// characters, not seven.
    #[test]
    fn char_wrapper_ex_counts_no_column_for_a_selector() {
        let wrapper = CharWrapperEx { chars: 5 };
        let (bare, bare_rows) = engine_with("ab\u{2615}cde", &wrapper, 32);
        let (with_vs, rows) = engine_with("ab\u{2615}\u{FE0F}cde", &wrapper, 32);
        assert_eq!(rows, bare_rows);
        assert_eq!(with_vs.store.arrays[0].data, vec![6]);
        assert_eq!(bare.store.arrays[0].data, vec![5]);
    }

    #[test]
    fn char_wrapper_ex_tab_crossing_breaks_after_tab() {
        let wrapper = CharWrapperEx { chars: 2 };
        let (engine, rows) = engine_with("a\tb", &wrapper, 32);
        assert_eq!(rows, 2);
        assert_eq!(engine.store.arrays[0].data, vec![2]);
    }
}
