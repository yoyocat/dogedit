use tree_sitter::{
    InputEdit, Node, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};

/// 画字词汇（`[Style]` 字形位、`HighlightRun`）的单一定义在 `dg-paint`。
/// 本模块照旧 `pub use`，`dg_syntax::STYLE_*` / `dg_syntax::HighlightRun`
/// 的导入路径不变；这样 `dg-render` 只需依赖那个零依赖叶子 crate。
pub use dg_paint::{
    HighlightRun, STYLE_BOLD, STYLE_BOX, STYLE_DOTTED, STYLE_DOUBLE, STYLE_ITALIC,
    STYLE_STRIKE, STYLE_UNDERLINE, STYLE_UNDERLINE_FAMILY, STYLE_WAVY,
};

/// Maps tree-sitter node kinds (kind path) and capture names (query path)
/// to colors.
#[derive(Clone)]
pub struct ThemeConfig {
    pub default_color: u32,
    pub rules: Vec<(&'static str, u32)>,
    /// Capture 前缀 → 颜色（query 路径）：`@keyword.control` 按点分层级做
    /// **最长前缀**匹配（`keyword` 命中 `keyword.control.import`，
    /// `keyword.control` 命中更深一层时优先）。空表 = 只有默认色。
    pub capture_rules: Vec<(String, u32)>,
    /// 与 `rules` 同一套键的**稀疏**字形表：只列带样式的条目，没列到的
    /// 是正体。`[Style]` 的值只有低两位有意义（[`STYLE_BOLD`] /
    /// [`STYLE_ITALIC`]），其余位在主题文件里存在但没有画的地方。
    pub style_rules: Vec<(&'static str, u8)>,
    /// `capture_rules` 的字形版，匹配规则相同（最长前缀）。
    pub capture_styles: Vec<(String, u8)>,
}

impl ThemeConfig {
    pub fn color_for(&self, kind: &str) -> u32 {
        for (k, c) in &self.rules {
            if kind.contains(k) {
                return *c;
            }
        }
        self.default_color
    }

    /// kind → 字形位。匹配顺序与 [`Self::color_for`] 一致（第一条命中
    /// 的规则说了算），这样一条规则的颜色和字形不会来自两个不同的键。
    pub fn style_for(&self, kind: &str) -> u8 {
        for (k, s) in &self.style_rules {
            if kind.contains(k) {
                return *s;
            }
        }
        0
    }

    /// Capture 名 → 颜色：精确匹配或点分前缀，取**最长**命中的规则。
    pub fn capture_color(&self, capture: &str) -> u32 {
        let mut best: Option<(&str, u32)> = None;
        for (prefix, color) in &self.capture_rules {
            let hit = capture == prefix
                || capture
                    .strip_prefix(prefix.as_str())
                    .is_some_and(|rest| rest.starts_with('.'));
            if hit && best.is_none_or(|(p, _)| prefix.len() > p.len()) {
                best = Some((prefix, *color));
            }
        }
        best.map(|(_, c)| c).unwrap_or(self.default_color)
    }

    /// Capture 名 → 字形位，最长前缀，与 [`Self::capture_color`] 同一条规则。
    pub fn capture_style(&self, capture: &str) -> u8 {
        let mut best: Option<(&str, u8)> = None;
        for (prefix, style) in &self.capture_styles {
            let hit = capture == prefix
                || capture
                    .strip_prefix(prefix.as_str())
                    .is_some_and(|rest| rest.starts_with('.'));
            if hit && best.is_none_or(|(p, _)| prefix.len() > p.len()) {
                best = Some((prefix, *style));
            }
        }
        best.map(|(_, s)| s).unwrap_or(0)
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            default_color: 0xE2E8F0,
            rules: vec![
                ("keyword", 0x569CD6),
                ("string", 0xCE9178),
                ("comment", 0x6A9955),
                ("number", 0xB5CEA8),
                ("property", 0x9CDCFE),
                ("function", 0xDCDCAA),
                ("type", 0x4EC9B0),
                ("operator", 0xD4D4D4),
                ("punctuation", 0x808080),
            ],
            // 标准 Helix scope 名 → 与 kind 表同一套色板。拷来的
            // highlights.scm 不开主题也能上色（parser_plugin_plan.md §6 v2）。
            capture_rules: [
                ("keyword", 0x569CD6),
                ("string", 0xCE9178),
                ("string.escape", 0xD7BA7D),
                ("comment", 0x6A9955),
                ("constant.numeric", 0xB5CEA8),
                ("constant", 0x569CD6),
                ("function", 0xDCDCAA),
                ("function.macro", 0xDCDCAA),
                ("constructor", 0x4EC9B0),
                ("type", 0x4EC9B0),
                ("variable", 0x9CDCFE),
                ("variable.other.member", 0x9CDCFE),
                ("variable.parameter", 0x9CDCFE),
                ("operator", 0xD4D4D4),
                ("punctuation", 0x808080),
                ("tag", 0x569CD6),
                ("attribute", 0x9CDCFE),
                ("label", 0xDCDCAA),
                ("namespace", 0x4EC9B0),
            ]
            .into_iter()
            .map(|(k, c)| (k.to_string(), c))
            .collect(),
            // 出厂不带字形：没有主题文件时编辑器就是一片正体，和以前一样。
            style_rules: Vec::new(),
            capture_styles: Vec::new(),
        }
    }
}

/// 一段高亮区间：起始字节、字节长度、样式类。
///
/// `class` 是 query 的 capture 下标，或 kind 路径下节点 kind 在
/// [`Snapshot`] 名字表里的下标。颜色和字形在绘制时按**当前**主题解析，
/// 换主题不必重新解析。
#[derive(Clone, Copy)]
struct SnapSeg {
    start: u32,
    len: u32,
    class: u16,
}

/// 整文档高亮快照。
///
/// 语法树的内存按源文件大小的几十倍长（1.4 MB 的 Rust 文件约 42 MB）。
/// 大文档把颜色压成这份按字节升序、互不重叠的区间之后，树就可以丢掉：
/// 同一份文件压完约 2 MB，绘制只读快照。区间按行二分切出相交的那几段，
/// 后面走和树路径同一套 [`runs_in_range`] 补缝/合并，所以画出来的 run
/// 与树路径一致。
pub struct Snapshot {
    segs: Vec<SnapSeg>,
    /// query 路径：capture 名字表；`SnapSeg::class` 是下标。
    captures: Vec<String>,
    /// kind 路径：节点 kind 表；`SnapSeg::class` 是下标。
    kinds: Vec<String>,
    /// 折叠区（字节区间）。
    folds: Vec<(u32, u32)>,
    /// 可以重新起解析的字节位置（升序）：这些节点与顶层构造同类，
    /// 从这里用空解析器往后解析，语法状态与整篇解析到该处时一致。
    /// 编辑后的局部重算从最近的一个开始，不用每次从头解析整篇。
    restarts: Vec<u32>,
    /// 哪些字节段的颜色是真算出来的（升序、已并邻接）。立表那一趟只覆盖
    /// 光标/视野那一窗，绘制要能问出「视野这一屏算过没有」，没算过就再派
    /// 一窗；整篇压表把它铺满，之后这个问题永远答案是「有」。
    covered: Vec<(u32, u32)>,
}

/// 编辑之后重新解析出来的一段颜色（工作线程）。
///
/// `start`/`end` 是整篇文档的字节域，`segs` 的偏移也已换成整篇的；
/// 拼接时只替换这一段，段外的颜色（含跨过窗口起点的多行串/注释）
/// 原样保留。
pub struct WindowPaint {
    start: u32,
    end: u32,
    segs: Vec<SnapSeg>,
    captures: Vec<String>,
    kinds: Vec<String>,
    restarts: Vec<u32>,
}

impl Snapshot {
    /// 一个样式类在当前主题下的颜色与字形。
    fn class_style(&self, class: usize, theme: &ThemeConfig) -> (u32, u8) {
        if !self.captures.is_empty() {
            match self.captures.get(class) {
                Some(name) => (theme.capture_color(name), theme.capture_style(name)),
                None => (theme.default_color, 0),
            }
        } else {
            match self.kinds.get(class) {
                Some(kind) => (theme.color_for(kind), theme.style_for(kind)),
                None => (theme.default_color, 0),
            }
        }
    }

    /// 与 `[byte_start, byte_end)` 相交的段，颜色按主题解析。
    fn segments_in(
        &self,
        byte_start: usize,
        byte_end: usize,
        theme: &ThemeConfig,
    ) -> Vec<(usize, usize, u32, u8)> {
        if self.segs.is_empty() || byte_end <= byte_start {
            return Vec::new();
        }
        let from = self
            .segs
            .partition_point(|s| s.start as usize + s.len as usize <= byte_start);
        let to = from + self.segs[from..].partition_point(|s| (s.start as usize) < byte_end);
        let classes = self.captures.len().max(self.kinds.len());
        // 一行里的段数是个位数；同一个类的颜色只解析一次。
        let mut memo: Vec<Option<(u32, u8)>> = vec![None; classes];
        let mut out = Vec::with_capacity(to - from);
        for s in &self.segs[from..to] {
            let idx = s.class as usize;
            let (color, style) = match memo.get(idx).copied().flatten() {
                Some(v) => v,
                None => {
                    let v = self.class_style(idx, theme);
                    if let Some(slot) = memo.get_mut(idx) {
                        *slot = Some(v);
                    }
                    v
                }
            };
            out.push((
                s.start as usize,
                s.start as usize + s.len as usize,
                color,
                style,
            ));
        }
        out
    }

    /// 不晚于 `byte` 的最近一个可重新起解析的位置（没有就是文档开头）。
    fn restart_floor(&self, byte: usize) -> usize {
        let i = self.restarts.partition_point(|&r| (r as usize) <= byte);
        if i == 0 {
            0
        } else {
            self.restarts[i - 1] as usize
        }
    }

    /// `[s, e)` 是不是已经算过颜色。按区间的**并集**判：编辑把区间平移之后
    /// 相邻的两条不一定并过，中间没有洞就算覆盖。
    fn covers(&self, s: usize, e: usize) -> bool {
        if e <= s {
            return true;
        }
        let end = e.min(u32::MAX as usize) as u32;
        let mut pos = s.min(u32::MAX as usize) as u32;
        let mut i = self.covered.partition_point(|&(_, ce)| ce < pos);
        while i < self.covered.len() {
            let (rs, re) = self.covered[i];
            if rs > pos {
                return false;
            }
            if re >= end {
                return true;
            }
            pos = re;
            i += 1;
        }
        false
    }

    /// 记下 `[s, e)` 算过了，顺手并进相邻的区间：连续滚动只会留一条。
    fn add_covered(&mut self, s: u32, e: u32) {
        if e <= s {
            return;
        }
        let at = self.covered.partition_point(|&(_, ce)| ce + 1 < s);
        let mut span = (s, e);
        // 吞掉所有相交或相接的区间，再回写。
        let mut to = at;
        while to < self.covered.len() && self.covered[to].0 <= span.1 + 1 {
            span.0 = span.0.min(self.covered[to].0);
            span.1 = span.1.max(self.covered[to].1);
            to += 1;
        }
        self.covered.splice(at..to, [span]);
    }

    /// 把一段重解析结果拼进来：落在窗口内的旧段换掉，窗口外的颜色不动
    /// （跨过窗口起点的段留前半截，跨过终点的留后半截）。类按名字并进
    /// 本地名字表 —— 同一份 query 时两边下标本来就一样，按名字并是防
    /// query 换过之后错位。
    fn splice(&mut self, mut w: WindowPaint) -> bool {
        let (start, end) = (w.start as usize, w.end as usize);
        let base_caps = !self.captures.is_empty();
        let win_caps = !w.captures.is_empty();
        if (base_caps || !self.kinds.is_empty()) && base_caps != win_caps {
            // 两边不是同一条配色路径（query 刚被加上/去掉）。窗口的类下标
            // 含义对不上，这一段弃掉，交给整篇重算。
            return false;
        }
        if win_caps {
            let map = class_remap(&w.captures, &mut self.captures);
            for s in &mut w.segs {
                s.class = map.get(s.class as usize).copied().unwrap_or(0);
            }
        } else if !w.kinds.is_empty() {
            let map = class_remap(&w.kinds, &mut self.kinds);
            for s in &mut w.segs {
                s.class = map.get(s.class as usize).copied().unwrap_or(0);
            }
        }
        let from = self
            .segs
            .partition_point(|s| (s.start as usize + s.len as usize) <= start);
        let to = from
            + self.segs[from..]
                .partition_point(|s| (s.start as usize) < end);
        let mut repl: Vec<SnapSeg> = Vec::with_capacity(w.segs.len() + 2);
        if from < to {
            let first = self.segs[from];
            let fs = first.start as usize;
            if fs < start {
                repl.push(SnapSeg {
                    start: first.start,
                    len: (start - fs) as u32,
                    class: first.class,
                });
            }
            let last = self.segs[to - 1];
            let le = last.start as usize + last.len as usize;
            if le > end {
                repl.push(SnapSeg {
                    start: end as u32,
                    len: (le - end) as u32,
                    class: last.class,
                });
            }
        }
        repl.extend(w.segs);
        self.segs.splice(from..to, repl);

        let from = self.restarts.partition_point(|&r| (r as usize) < start);
        let to = from
            + self.restarts[from..]
                .partition_point(|&r| (r as usize) < end);
        self.restarts.splice(from..to, w.restarts);
        // 这一窗的颜色是重解析出来的，覆盖记账跟着扩大。
        self.add_covered(w.start, w.end);
        true
    }
}

/// Incremental tree-sitter highlighter. No whole-document text lives
/// here: parse input is served chunk by chunk from the line store
/// through [`Self::parse_feed`]/[`Self::reset_feed`], and run-length
/// conversion asks the caller for char counts via a callback. The
/// highlighter tracks only the document's byte length.
pub struct Highlighter {
    lang: tree_sitter::Language,
    tree: Option<Tree>,
    /// Total document byte length, maintained across edits — the
    /// clamp the old full-text mirror's `text.len()` provided.
    doc_len: usize,
    /// Edits applied since the last parse. Usually one
    /// (a keystroke); a burst that outpaces the sync timer
    /// accumulates, and the parse folds them all into the
    /// tree in order before the incremental parse.
    pending_edits: Vec<InputEdit>,
    /// Edits already folded into the tree's node positions (via
    /// [`Self::fold_pending`]) but not yet re-parsed: the tree
    /// still *describes* the old tokenization, only shifted to
    /// the new byte offsets. The keystroke path keeps the tree
    /// folded so a paint before the parse lands still reads
    /// sensible colors; the parse itself is debounced.
    needs_reparse: bool,
    /// Kept across parses: constructing a `Parser` and loading the
    /// grammar per keystroke is pure overhead on the typing path.
    parser: Option<Parser>,
    /// 可选的高亮查询（`queries/highlights.scm`，plan §6 v2）。
    /// 有它走 capture 配色，没有退回 node kind 子串匹配。
    query: Option<Query>,
    /// 查出 `query` 的那份脚本。局部重算要把同样的输入再派给工作线程，
    /// 从这里拿，别每一键都回磁盘重读一遍。
    query_scm: Option<String>,
    /// 查出 `fold_query` 的那份脚本（同上）。
    fold_scm: Option<String>,
    /// 可选的折叠查询（`queries/folds.scm`）。有它折叠区来自 capture，
    /// 没有退回 [`crate::folding::folding_spans`] 的 kind 启发式。
    fold_query: Option<Query>,
    /// 折叠查询里有没有谓词（`#eq?`、`#match?` 之类）。没有的话节点文本
    /// 一次也不用取 —— 一趟折叠区计算要走过整棵树，每命中一次都切一份
    /// 文本，那份拷贝能占掉一半时间。
    fold_predicates: bool,
    /// 大文档的快照。有它时树不常驻（`tree` 为空），绘制只读快照。
    snapshot: Option<Snapshot>,
    /// 走快照模式（大文档）：树只在后台解析线程里活一趟，不等同于
    /// 「快照已经到了」—— 打开后到结果回来之前 `snapshot` 是空的。
    snapshot_mode: bool,
    /// 上一次整篇重算以来最早动过的字节（快照模式）：局部重算的窗口要
    /// 从它（而不是光标）往回收 —— 退格、多光标都会让改动落在光标之前。
    dirty_from: Option<usize>,
}

impl Highlighter {
    pub fn new(lang: tree_sitter::Language) -> Self {
        Self {
            lang,
            tree: None,
            doc_len: 0,
            pending_edits: Vec::new(),
            needs_reparse: false,
            parser: None,
            query: None,
            query_scm: None,
            fold_query: None,
            fold_scm: None,
            fold_predicates: false,
            snapshot: None,
            snapshot_mode: false,
            dirty_from: None,
        }
    }

    /// 大文档：树不常驻，快照由后台解析补上。设置之后树相关的方法都不再
    /// 解析；窗口那边看到 [`Self::snapshot_stale`] 就把全文派给工作线程。
    /// `doc_len` 得带上挂载那一刻的真实字节数：编辑的记账从它起步，快照
    /// 没落地之前的每一笔（含撤销的反向记账）才和文档的大小对得上 ——
    /// 从 0 起步的话，第一次撤销到挂载点之前的历史就会把字节长度减穿。
    pub fn defer_snapshot(&mut self, doc_len: usize) {
        self.snapshot_mode = true;
        self.doc_len = doc_len;
        self.tree = None;
        self.parser = None;
        self.pending_edits.clear();
        self.dirty_from = None;
        self.needs_reparse = true;
    }

    /// 上一次整篇重算以来最早动过的字节（局部重算的窗口起点参考）。
    pub fn dirty_from(&self) -> Option<usize> {
        self.dirty_from
    }

    pub fn is_snapshot_mode(&self) -> bool {
        self.snapshot_mode
    }

    pub fn has_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }

    /// 快照落后于文本：快照还没来，或编辑之后还没重算。
    pub fn snapshot_stale(&self) -> bool {
        self.snapshot_mode && (self.snapshot.is_none() || self.needs_reparse)
    }

    /// 把当前树压成快照，并丢掉树、折叠查询与 parser —— 调用之后这份
    /// 高亮器只剩区间表。折叠区一起取出（它在树还活着时算）。高亮 query
    /// 留着：它只有几十 KB，界面线程的即时窗口重算（[`Self::parse_window`]
    /// ）还要用它，重新编译是毫秒级，不能落到击键路径上。
    pub fn compact(&mut self, node_text: &dyn Fn(usize, usize) -> Vec<u8>) -> bool {
        self.snapshot_mode = true;
        if self.tree.is_none() {
            return false;
        }
        let mut folds: Vec<(u32, u32)> = self
            .fold_spans(node_text)
            .into_iter()
            .filter(|(s, e)| e > s)
            .map(|(s, e)| {
                (
                    s.min(u32::MAX as usize) as u32,
                    e.min(u32::MAX as usize) as u32,
                )
            })
            .collect();
        let tree = self.tree.take().expect("checked above");
        let root = tree.root_node();
        let restarts = restart_points(root, self.doc_len);
        let (mut segs, captures, kinds) = match self.query.as_ref() {
            Some(query) => {
                let names: Vec<String> = query
                    .capture_names()
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect();
                let segs = capture_ids(query, root, self.doc_len, node_text);
                (segs, names, Vec::new())
            }
            None => {
                let (segs, kinds) = kind_ids(root, self.doc_len);
                (segs, Vec::new(), kinds)
            }
        };
        self.fold_query = None;
        self.parser = None;
        self.pending_edits.clear();
        self.dirty_from = None;
        self.needs_reparse = false;
        // 推送时按倍数长出来的容量原样留着就是白占：整表只读，收成实际长度。
        segs.shrink_to_fit();
        folds.shrink_to_fit();
        self.snapshot = Some(Snapshot {
            segs,
            captures,
            kinds,
            folds,
            restarts,
            // 整篇压完就是全覆盖：视野那一屏此后一律答「算过了」。
            covered: vec![(0, self.doc_len.min(u32::MAX as usize) as u32)],
        });
        true
    }

    /// 编辑后局部重算的起点：不晚于 `byte` 的最近可起解析位置。
    pub fn restart_floor(&self, byte: usize) -> Option<usize> {
        self.snapshot.as_ref().map(|s| s.restart_floor(byte))
    }

    /// `[start, end)` 这段字节的颜色算出来了没有。没有表（整篇还没回来、
    /// 也还没立表）时答 `false` —— 那一屏还得有人去算。
    pub fn covers(&self, start: usize, end: usize) -> bool {
        match self.snapshot.as_ref() {
            Some(snap) => snap.covers(start, end),
            None => false,
        }
    }

    /// 把工作线程重解析的一小段拼进快照。还没有快照时，这一段就是第一张
    /// 表：打开之后先让光标附近那一段有颜色，折叠区留空，整篇那一趟回来
    /// 再换掉整张表（[`Self::snapshot_stale`] 在换掉之前一直为真）。
    pub fn apply_window(&mut self, paint: WindowPaint) -> bool {
        if !self.snapshot_mode {
            return false;
        }
        match self.snapshot.as_mut() {
            Some(snap) => snap.splice(paint),
            None => {
                let (s, e) = (paint.start, paint.end);
                self.snapshot = Some(Snapshot {
                    segs: paint.segs,
                    captures: paint.captures,
                    kinds: paint.kinds,
                    folds: Vec::new(),
                    restarts: paint.restarts,
                    covered: Vec::new(),
                });
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.add_covered(s, e);
                }
                true
            }
        }
    }

    /// 把刚解析的一小段压成颜色区间。这一段的偏移从 `offset` 起换成整篇的
    /// 字节域，`len` 是这一段的字节长（树只描述这一段，压表和重起点都以它
    /// 为界）；树只在这一趟里活着，返回前就丢掉。
    pub fn window_paint(
        &mut self,
        offset: usize,
        len: usize,
        node_text: &dyn Fn(usize, usize) -> Vec<u8>,
    ) -> WindowPaint {
        let mut segs = Vec::new();
        let mut captures = Vec::new();
        let mut kinds = Vec::new();
        let mut restarts = Vec::new();
        if let Some(tree) = self.tree.take() {
            let root = tree.root_node();
            match self.query.as_ref() {
                Some(query) => {
                    captures = query
                        .capture_names()
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect();
                    segs = capture_ids(query, root, len, node_text);
                }
                None => {
                    let (s, k) = kind_ids(root, len);
                    segs = s;
                    kinds = k;
                }
            }
            restarts = restart_points(root, len);
        }
        let shift = offset.min(u32::MAX as usize) as u32;
        for s in &mut segs {
            s.start = s.start.saturating_add(shift);
        }
        for r in &mut restarts {
            *r = r.saturating_add(shift);
        }
        segs.shrink_to_fit();
        WindowPaint {
            start: shift,
            end: (offset.saturating_add(len)).min(u32::MAX as usize) as u32,
            segs,
            captures,
            kinds,
            restarts,
        }
    }

    /// 界面线程的即时局部重算：从 `start`（一个可重起解析的字节位置）起
    /// 从零解析 `bytes`（整篇字节域里 start 之后的一小段），压成颜色区间
    /// 当场拼回快照。树只在这一趟里活着，返回前就丢掉 —— 常驻内存仍然是
    /// 区间表加一个小解析器。返回 false 表示这一趟没落进快照（快照还没
    /// 来、解析失败或两套配色路径对不上），调用方退回工作线程的任务。
    pub fn parse_window(
        &mut self,
        start: usize,
        bytes: &[u8],
        node_text: &dyn Fn(usize, usize) -> Vec<u8>,
    ) -> bool {
        if !self.snapshot_mode || self.snapshot.is_none() || bytes.is_empty() {
            return false;
        }
        let mut parser = self.parser.take().unwrap_or_else(Parser::new);
        if parser.set_language(&self.lang).is_err() {
            return false;
        }
        self.tree = parser.parse(bytes, None);
        self.parser = Some(parser);
        if self.tree.is_none() {
            return false;
        }
        let paint = self.window_paint(start, bytes.len(), node_text);
        self.apply_window(paint)
    }

    /// 编译并挂上高亮查询（`queries/highlights.scm` 的文本）。编译失败
    /// 返回原因并**保留原状**（kind 路径继续工作）。
    pub fn set_query(&mut self, scm: &str) -> Result<(), String> {
        let query = Query::new(&self.lang, scm).map_err(|e| e.to_string())?;
        self.query = Some(query);
        self.query_scm = Some(scm.to_string());
        Ok(())
    }

    /// 去掉查询，回到 node kind 配色。
    pub fn clear_query(&mut self) {
        self.query = None;
        self.query_scm = None;
    }

    /// 只登记脚本、不编译：窗口重算的整篇任务把脚本带去工作线程编译，
    /// 登记下来的文本就是派任务时用的那份，不从磁盘重读。界面线程自己的
    /// query 由挂载处（`attach_syntax_snapshot`）另行编译，给即时窗口
    /// 重算用。
    pub fn set_pending_queries(&mut self, highlights: Option<&str>, folds: Option<&str>) {
        self.query_scm = highlights.map(|s| s.to_string());
        self.fold_scm = folds.map(|s| s.to_string());
    }

    /// 这门语法的 `Language`（工作线程要用）。
    pub fn lang(&self) -> tree_sitter::Language {
        self.lang.clone()
    }

    /// 挂载时用的 `(highlights.scm, folds.scm)`。
    pub fn query_texts(&self) -> (Option<String>, Option<String>) {
        (self.query_scm.clone(), self.fold_scm.clone())
    }

    pub fn has_query(&self) -> bool {
        self.query.is_some()
    }

    /// 编译并挂上折叠查询（`queries/folds.scm`）。失败保留原状。
    pub fn set_fold_query(&mut self, scm: &str) -> Result<(), String> {
        let query = Query::new(&self.lang, scm).map_err(|e| e.to_string())?;
        self.fold_query = Some(query);
        self.fold_scm = Some(scm.to_string());
        self.fold_predicates = scm.contains('#');
        Ok(())
    }

    pub fn has_fold_query(&self) -> bool {
        self.fold_query.is_some()
    }

    /// 折叠区（字节区间）：有 folds.scm 走 capture（保留嵌套），否则
    /// 退回 kind 启发式。`node_text` 供 query 谓词取节点文本。
    /// 快照模式下读解析时存下的那份，不再有树可走。
    pub fn fold_spans(&self, node_text: &dyn Fn(usize, usize) -> Vec<u8>) -> Vec<(usize, usize)> {
        if let Some(snap) = &self.snapshot {
            return snap
                .folds
                .iter()
                .map(|&(s, e)| (s as usize, e as usize))
                .collect();
        }
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        match self.fold_query.as_ref() {
            Some(q) if self.fold_predicates => crate::folding::query_spans(tree, q, node_text),
            Some(q) => {
                // 没有谓词就没有人读节点文本：别为每次命中切一份。
                let none = |_s: usize, _e: usize| Vec::new();
                crate::folding::query_spans(tree, q, &none)
            }
            None => crate::folding::folding_spans(tree),
        }
    }

    /// The tracked document byte length.
    pub fn doc_len(&self) -> usize {
        self.doc_len
    }

    /// Syntax kind covering `byte_offset`.
    pub fn kind_at(&self, byte_offset: usize) -> Option<&str> {
        let tree = self.tree.as_ref()?;
        if byte_offset >= self.doc_len {
            return None;
        }
        let node = tree
            .root_node()
            .descendant_for_byte_range(byte_offset, byte_offset + 1)?;
        Some(node.kind())
    }

    /// Current syntax tree, when parsed.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Folds the pending edits into the tree's node positions
    /// *without* re-parsing, so byte queries against the stale
    /// tree still land on the right (if possibly stale-colored)
    /// nodes. Cheap — `Tree::edit` only walks the nodes that
    /// intersect the edit. The tree keeps its tokenization; only
    /// its offsets follow the text.
    pub fn fold_pending(&mut self) {
        if self.pending_edits.is_empty() {
            return;
        }
        if let Some(tree) = self.tree.as_mut() {
            let edits = std::mem::take(&mut self.pending_edits);
            for edit in edits {
                tree.edit(&edit);
            }
            self.needs_reparse = true;
        }
        self.pending_edits.clear();
    }

    /// Record an edit (byte-oriented). The next parse is
    /// incremental; the document's own byte length moves with
    /// `new_end_byte - old_end_byte`.
    pub fn edit_at(
        &mut self,
        start_byte: usize,
        old_end_byte: usize,
        new_end_byte: usize,
        start_position: Point,
        old_end_position: Point,
        new_end_position: Point,
    ) {
        // 饱和减：记账和正文一旦有出入（比如存储把插入文本的行尾归一化
        // 了而命令记的是原文），这里宁可停在 0 让颜色等下一趟整算，也不把
        // 算术下溢带进窗口过程。
        self.doc_len = (self.doc_len + new_end_byte).saturating_sub(old_end_byte);
        if self.snapshot_mode {
            // 没有树可以增量折叠：把快照里改点之后的区间整体平移，改点
            // 所在的区间截断（超出改动范围的颜色等后台重算）。
            self.shift_snapshot(start_byte, old_end_byte, new_end_byte);
            self.dirty_from = Some(self.dirty_from.map_or(start_byte, |d| d.min(start_byte)));
            self.needs_reparse = true;
            return;
        }
        self.pending_edits.push(InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        });
    }

    /// 编辑之后把快照区间跟到新的字节域：完全落在改点之后的平移
    /// `new_end - old_end`，与改动相交的只留改点之前那截，折叠区
    /// 相交的丢掉（等重算）。
    fn shift_snapshot(&mut self, start_byte: usize, old_end_byte: usize, new_end_byte: usize) {
        let Some(snap) = self.snapshot.as_mut() else {
            return;
        };
        let delta = new_end_byte as isize - old_end_byte as isize;
        let shift = |v: usize| (v as isize + delta).clamp(0, u32::MAX as isize) as u32;
        for seg in &mut snap.segs {
            let s = seg.start as usize;
            let e = s + seg.len as usize;
            if s >= old_end_byte {
                seg.start = shift(s);
            } else if e > start_byte {
                if s < start_byte {
                    seg.len = (start_byte - s) as u32;
                } else {
                    seg.len = 0;
                }
            }
        }
        snap.segs.retain(|s| s.len > 0);
        // 折叠区和「哪些字节算过」是同一类区间：起点跟着走，被改点盖住的
        // 终点收到新字节那里。
        let shift_ranges = |ranges: &mut Vec<(u32, u32)>| {
            for r in ranges.iter_mut() {
                let (s, e) = (r.0 as usize, r.1 as usize);
                if s >= old_end_byte {
                    r.0 = shift(s);
                    r.1 = shift(e);
                } else if e > start_byte {
                    // 改点落在区间里：起点不动，终点跟着新字节走。整趟编辑
                    // 只要没碰到收尾那行，这道折叠就还是对的 —— 保持箭头不闪，
                    // 精确区间由停手后的整篇重算补。
                    r.1 = shift(e).max(r.0);
                }
            }
            ranges.retain(|f| f.1 > f.0);
        };
        shift_ranges(&mut snap.folds);
        shift_ranges(&mut snap.covered);
        // 顶层起点：改动本身盖住的作废，其后的跟着平移。
        let mut i = 0;
        while i < snap.restarts.len() {
            let r = snap.restarts[i] as usize;
            if r >= old_end_byte {
                snap.restarts[i] = shift(r);
                i += 1;
            } else if r >= start_byte {
                snap.restarts.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Folds pending edits into the old tree and returns it, ready
    /// for a parse (shared by both parse entry points).
    fn take_old_tree(&mut self) -> Option<Tree> {
        match self.tree.take() {
            Some(mut t) => {
                for edit in self.pending_edits.drain(..) {
                    t.edit(&edit);
                }
                Some(t)
            }
            None => {
                self.pending_edits.clear();
                None
            }
        }
    }

    /// (Re)parses from `doc_len` bytes served by `feed` — chunked,
    /// no whole-document text required. The
    /// callback owns each chunk (an owned `Vec<u8>` per call keeps
    /// borrows trivial); ~64 KiB chunks cost one allocation each.
    pub fn parse_feed(&mut self, feed: &mut dyn FnMut(usize) -> Vec<u8>) {
        // 快照模式没有树；重算由窗口派给工作线程（一次整文件解析会
        // 让界面冻结两百毫秒，正好是打开时把解析挪出主线程要避免的）。
        if self.snapshot_mode {
            return;
        }
        if !self.needs_reparse && self.pending_edits.is_empty() && self.tree.is_some() {
            return;
        }
        let mut parser = self.parser.take().unwrap_or_else(Parser::new);
        if parser.set_language(&self.lang).is_err() {
            // Grammar gone (language dropped): full restart.
            self.parser = None;
            return;
        }
        let old_tree = self.take_old_tree();
        self.tree =
            parser.parse_with_options(&mut |off, _| feed(off), old_tree.as_ref(), None);
        self.needs_reparse = false;
        self.parser = Some(parser);
    }

    /// (Re)parses over a whole `&str` — tests and small callers; the
    /// string is only read, never stored.
    pub fn parse_str(&mut self, text: &str) {
        self.doc_len = text.len();
        let bytes = text.as_bytes();
        self.parse_feed(&mut |off| bytes[off.min(bytes.len())..].to_vec());
    }

    /// True when edits were applied to the text without a parse yet.
    pub fn has_pending_edit(&self) -> bool {
        !self.pending_edits.is_empty()
    }

    /// True when the tree no longer reflects the text: either
    /// edits are pending, or they were folded without a reparse.
    /// The keystroke path uses this to decide whether the
    /// debounced syntax sync has work to do.
    pub fn needs_parse(&self) -> bool {
        if self.snapshot_mode {
            return false;
        }
        self.tree.is_none() || !self.pending_edits.is_empty() || self.needs_reparse
    }

    /// Full re-parse from scratch over a `&str` (tests and small
    /// callers; app callers use [`Self::reset_feed`]).
    pub fn reset(&mut self, text: &str) {
        self.tree = None;
        self.pending_edits.clear();
        self.parse_str(text);
    }

    /// Full re-parse from scratch over a chunk feed (the app path —
    /// no whole-document text). `doc_len` is the store's byte length.
    pub fn reset_feed(&mut self, doc_len: usize, feed: &mut dyn FnMut(usize) -> Vec<u8>) {
        self.doc_len = doc_len;
        self.tree = None;
        self.pending_edits.clear();
        self.parse_feed(feed);
    }

    /// Highlight a byte range, emitting `{color, len}`
    /// runs in character units — the format the painter
    /// consumes (TextOutW runs,). `char_len` reports
    /// the character count of a byte range in the document
    /// (the store's decode-on-demand view answers it; nothing
    /// document-sized is allocated). `node_text` serves a byte range's
    /// text for query predicates (`#match?`/`#eq?`/`#any-of?`);
    /// only called when a query is attached.
    pub fn highlight_range(
        &self,
        byte_start: usize,
        byte_end: usize,
        theme: &ThemeConfig,
        char_len: &dyn Fn(usize, usize) -> u32,
        node_text: &dyn Fn(usize, usize) -> Vec<u8>,
    ) -> Vec<HighlightRun> {
        runs_from_segments(
            self.segments(byte_start, byte_end, theme, node_text),
            byte_start,
            byte_end,
            self.doc_len,
            theme.default_color,
            char_len,
        )
    }

    /// The coloured byte spans of a range, ascending and non-overlapping —
    /// the half of [`Self::highlight_range`] that walks the tree. Callers that
    /// want many small ranges out of one large one (the document map colours
    /// the whole strip's window in a single query, then cuts it per line) pay
    /// for the tree walk once instead of once per range.
    ///
    /// Each span is `(start, end, color, style)`; `style` rides along with
    /// the colour because the two come from the same `[Theme]`/`[Style]`
    /// entry and must never be resolved against different keys.
    pub fn segments(
        &self,
        byte_start: usize,
        byte_end: usize,
        theme: &ThemeConfig,
        node_text: &dyn Fn(usize, usize) -> Vec<u8>,
    ) -> Vec<(usize, usize, u32, u8)> {
        if let Some(snap) = &self.snapshot {
            return snap.segments_in(byte_start, byte_end, theme);
        }
        let tree = match &self.tree {
            Some(t) => t,
            None => return Vec::new(),
        };
        match &self.query {
            Some(query) => self.query_segments(tree, query, byte_start, byte_end, theme, node_text),
            None => self.kind_segments(tree, byte_start, byte_end, theme),
        }
    }

    /// kind 路径：叶子节点按 `ThemeConfig::rules` 的子串匹配上色。
    fn kind_segments(
        &self,
        tree: &Tree,
        byte_start: usize,
        byte_end: usize,
        theme: &ThemeConfig,
    ) -> Vec<(usize, usize, u32, u8)> {
        let mut leaves = Vec::new();
        collect_leaves(tree.root_node(), byte_start, byte_end, &mut leaves);
        leaves.sort_by_key(|n| n.start_byte());
        let mut segs = Vec::new();
        for node in &leaves {
            let s = node.start_byte().max(byte_start);
            let e = node.end_byte().min(byte_end).min(self.doc_len);
            if e <= s {
                continue;
            }
            let kind = node.kind();
            segs.push((s, e, theme.color_for(kind), theme.style_for(kind)));
        }
        segs
    }

    /// query 路径：`highlights.scm` 的 capture 按出现顺序上色，后出现的
    /// 覆盖先出现的（Helix 语义）。`set_byte_range` 的过滤是**相交**
    /// 语义，跨行 token（块注释等）在它覆盖的每一行都会命中。
    fn query_segments(
        &self,
        tree: &Tree,
        query: &Query,
        byte_start: usize,
        byte_end: usize,
        theme: &ThemeConfig,
        node_text: &dyn Fn(usize, usize) -> Vec<u8>,
    ) -> Vec<(usize, usize, u32, u8)> {
        let root = tree.root_node();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(byte_start..byte_end);
        // 谓词的文本来源：只取被捕获节点自己的字节（tree-sitter 会按
        // node 的 byte range 切），不需要整文档文本。
        let provider = |node: Node| std::iter::once(node_text(node.start_byte(), node.end_byte()));
        let names = query.capture_names();
        let mut caps: Vec<(usize, usize, u32, u8)> = Vec::new();
        let mut captures = cursor.captures(query, root, provider);
        while let Some((m, idx)) = captures.next() {
            let cap = &m.captures()[*idx];
            let name = names[cap.index as usize];
            let s = cap.node.start_byte().max(byte_start);
            let e = cap.node.end_byte().min(byte_end).min(self.doc_len);
            if e > s {
                caps.push((s, e, theme.capture_color(name), theme.capture_style(name)));
            }
        }
        flatten_captures(&caps)
    }
}

/// 把按出现顺序排列的 capture 段摊平成互不重叠的段：后出现的覆盖先
/// 出现的（Helix 语义）。
///
/// 一次边界扫描 + 活跃集（`seq → (color, style)`），O(n log n)。逐个 capture
/// 重建整条 Vec 的写法是 O(n²) —— 压缩过的单行文件有上万个 capture，
/// 那会把绘制线程卡死。
fn flatten_captures(caps: &[(usize, usize, u32, u8)]) -> Vec<(usize, usize, u32, u8)> {
    // (位置, 是否结束, 出现序号, 颜色, 字形)
    let mut evs: Vec<(usize, bool, usize, u32, u8)> = Vec::with_capacity(caps.len() * 2);
    for (seq, &(s, e, color, style)) in caps.iter().enumerate() {
        evs.push((s, false, seq, color, style));
        evs.push((e, true, seq, color, style));
    }
    evs.sort_unstable_by_key(|(pos, end, seq, _, _)| (*pos, *end, *seq));
    let mut active: std::collections::BTreeMap<usize, (u32, u8)> = std::collections::BTreeMap::new();
    let mut out: Vec<(usize, usize, u32, u8)> = Vec::new();
    let mut i = 0;
    while i < evs.len() {
        // 同一位置上的事件全部处理完再出段：结束/开始同点相接时不会
        // 产生零长段，也不会漏掉新开始的那一段。
        let pos = evs[i].0;
        while i < evs.len() && evs[i].0 == pos {
            let (_, end, seq, color, style) = evs[i];
            if end {
                active.remove(&seq);
            } else {
                active.insert(seq, (color, style));
            }
            i += 1;
        }
        // 该位置之后的一段归"序号最大"（最后出现）的那个 capture。
        if let (Some(next), Some((_, &(color, style)))) =
            (evs.get(i).map(|e| e.0), active.last_key_value())
        {
            if next > pos {
                out.push((pos, next, color, style));
            }
        }
    }
    // 相邻同色同字形合并：中间的事件（别的 capture 结束）会切出同色的两段，
    // 对绘制没意义。
    let mut merged: Vec<(usize, usize, u32, u8)> = Vec::with_capacity(out.len());
    for (s, e, color, style) in out {
        if let Some(last) = merged.last_mut() {
            if last.1 == s && last.2 == color && last.3 == style {
                last.1 = e;
                continue;
            }
        }
        merged.push((s, e, color, style));
    }
    merged
}

/// 局部重算可以起解析的位置：与顶层子节点同类的节点起点。
///
/// 同类意味着这门语法允许它出现在文档顶层，用空解析器从这里往后读，
/// 得到的局部语法状态和整篇解析到该处时一致 —— 例如方法从 `fn` 起算，
/// 不必退回到整个 `impl` 的开头。行内缩进不影响：slice 从节点首字节切。
/// 全是根节点自己的子节点时退化成「每个顶层构造」，行为仍然正确，只是
/// 窗口大一些。
fn restart_points(root: Node, doc_len: usize) -> Vec<u32> {
    // kind 符号是整数：这趟要走整棵树，比按名字查表省得多。
    let mut allowed: std::collections::HashSet<u16> = std::collections::HashSet::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        allowed.insert(child.kind_id());
    }
    let mut out = Vec::new();
    fn walk(node: Node, doc_len: usize, allowed: &std::collections::HashSet<u16>, out: &mut Vec<u32>) {
        if node.start_byte() >= doc_len {
            return;
        }
        if !node.is_extra() && allowed.contains(&node.kind_id()) {
            out.push(node.start_byte() as u32);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, doc_len, allowed, out);
        }
    }
    walk(root, doc_len, &allowed, &mut out);
    out.sort_unstable();
    out.dedup();
    out
}

/// 窗口结果的名字表并进本地名字表，给出「窗口下标 → 本地下标」。
fn class_remap(names: &[String], local: &mut Vec<String>) -> Vec<u16> {
    names
        .iter()
        .map(|n| match local.iter().position(|m| m == n) {
            Some(i) => i.min(u16::MAX as usize) as u16,
            None => {
                let i = local.len().min(u16::MAX as usize);
                local.push(n.clone());
                i as u16
            }
        })
        .collect()
}

/// 快照区间入表：去掉零长段，与上一段同类且首尾相接的合并。
fn push_snap_seg(out: &mut Vec<SnapSeg>, start: usize, end: usize, class: u16) {
    if end <= start || start > u32::MAX as usize {
        return;
    }
    let end = end.min(u32::MAX as usize);
    if let Some(last) = out.last_mut() {
        if last.class == class && last.start as usize + last.len as usize == start {
            last.len = last.len.saturating_add((end - start) as u32);
            return;
        }
    }
    out.push(SnapSeg {
        start: start as u32,
        len: (end - start) as u32,
        class,
    });
}

/// query 路径的快照：capture 序号当类，摊平后只留类。
fn capture_ids(
    query: &Query,
    root: Node,
    doc_len: usize,
    node_text: &dyn Fn(usize, usize) -> Vec<u8>,
) -> Vec<SnapSeg> {
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(0..doc_len);
    let provider = |node: Node| std::iter::once(node_text(node.start_byte(), node.end_byte()));
    let mut caps: Vec<(usize, usize, u16)> = Vec::new();
    let mut captures = cursor.captures(query, root, provider);
    while let Some((m, idx)) = captures.next() {
        let cap = &m.captures()[*idx];
        let s = cap.node.start_byte().min(doc_len);
        let e = cap.node.end_byte().min(doc_len);
        if e > s {
            caps.push((s, e, cap.index.min(u16::MAX as u32) as u16));
        }
    }
    // 与 `flatten_captures` 同一套「后出现的覆盖先出现的」语义，只是
    // 活跃集里存的是类而不是已解析的颜色。
    let mut evs: Vec<(usize, bool, usize, u16)> = Vec::with_capacity(caps.len() * 2);
    for (seq, &(s, e, class)) in caps.iter().enumerate() {
        evs.push((s, false, seq, class));
        evs.push((e, true, seq, class));
    }
    evs.sort_unstable_by_key(|(pos, end, seq, _)| (*pos, *end, *seq));
    let mut active: std::collections::BTreeMap<usize, u16> = std::collections::BTreeMap::new();
    let mut out: Vec<SnapSeg> = Vec::new();
    let mut i = 0;
    while i < evs.len() {
        let pos = evs[i].0;
        while i < evs.len() && evs[i].0 == pos {
            let (_, end, seq, class) = evs[i];
            if end {
                active.remove(&seq);
            } else {
                active.insert(seq, class);
            }
            i += 1;
        }
        if let (Some(next), Some((_, &class))) = (evs.get(i).map(|e| e.0), active.last_key_value()) {
            if next > pos {
                push_snap_seg(&mut out, pos, next, class);
            }
        }
    }
    out
}

/// kind 路径的快照：叶子节点的 kind 进名字表，区间只存类。
fn kind_ids(root: Node, doc_len: usize) -> (Vec<SnapSeg>, Vec<String>) {
    let mut leaves = Vec::new();
    collect_leaves(root, 0, doc_len, &mut leaves);
    let mut kinds: Vec<String> = Vec::new();
    let mut ids: std::collections::HashMap<String, u16> = std::collections::HashMap::new();
    let mut out: Vec<SnapSeg> = Vec::with_capacity(leaves.len());
    for node in leaves {
        let (s, e) = (
            node.start_byte().min(doc_len),
            node.end_byte().min(doc_len),
        );
        if e <= s {
            continue;
        }
        let kind = node.kind();
        let class = match ids.get(kind) {
            Some(&c) => c,
            None => {
                let c = kinds.len().min(u16::MAX as usize) as u16;
                kinds.push(kind.to_string());
                ids.insert(kind.to_string(), c);
                c
            }
        };
        push_snap_seg(&mut out, s, e, class);
    }
    (out, kinds)
}

/// 区间段 → `{color, len, style}` 字符 run：排序、用默认色补缝、合并同色、
/// 按 `char_len` 折算字符长度。
fn runs_from_segments(
    mut segs: Vec<(usize, usize, u32, u8)>,
    byte_start: usize,
    byte_end: usize,
    doc_len: usize,
    default_color: u32,
    char_len: &dyn Fn(usize, usize) -> u32,
) -> Vec<HighlightRun> {
    segs.sort_by_key(|(s, e, _, _)| (*s, *e));
    runs_in_range(&segs, byte_start, byte_end, doc_len, default_color, char_len)
}

/// [`runs_from_segments`] 的只读版：段已按 `(s, e)` 升序，不再排序、不再
/// 复制。文档地图把一条带子窗口的段一次查出来，再按行切给
/// `runs_in_range` —— 每行只做补缝/合并，不用再走一遍语法树。
///
/// `segs` 必须是升序且互不重叠的（[`Highlighter::segments`] 和
/// [`flatten_captures`] 都保证这一点）；只与 `[byte_start, byte_end)` 相交
/// 的那些会参与拼接，其余忽略。
pub fn runs_in_range(
    segs: &[(usize, usize, u32, u8)],
    byte_start: usize,
    byte_end: usize,
    doc_len: usize,
    default_color: u32,
    char_len: &dyn Fn(usize, usize) -> u32,
) -> Vec<HighlightRun> {
    let mut tiled: Vec<(usize, usize, u32, u8)> = Vec::new();
    let mut cursor = byte_start;
    for &(s, e, color, style) in segs {
        let s = s.max(byte_start);
        let e = e.min(byte_end).min(doc_len);
        if e <= s {
            continue;
        }
        if s > cursor {
            tiled.push((cursor, s, default_color, 0));
        }
        tiled.push((s, e, color, style));
        cursor = e;
    }
    if cursor < byte_end {
        tiled.push((cursor, byte_end, default_color, 0));
    }
    let mut merged: Vec<(usize, usize, u32, u8)> = Vec::new();
    for (s, e, color, style) in tiled {
        if let Some(last) = merged.last_mut() {
            if last.1 == s && last.2 == color && last.3 == style {
                last.1 = e;
                continue;
            }
        }
        merged.push((s, e, color, style));
    }
    let mut runs: Vec<HighlightRun> = Vec::new();
    for (s, e, color, style) in merged {
        let n = char_len(s, e);
        if let Some(last) = runs.last_mut() {
            if last.color == color && last.style == style {
                last.len += n;
                continue;
            }
        }
        runs.push(HighlightRun {
            color,
            len: n,
            style,
        });
    }
    runs
}

fn collect_leaves<'a>(
    node: tree_sitter::Node<'a>,
    byte_start: usize,
    byte_end: usize,
    out: &mut Vec<tree_sitter::Node<'a>>,
) {
    if node.end_byte() <= byte_start || node.start_byte() >= byte_end {
        return;
    }
    // every childless node is a token leaf — named
    // or anonymous (punctuation); together they tile
    // the source range.
    if node.child_count() == 0 {
        out.push(node);
        return;
    }
    for i in 0..node.child_count() {
        if let Some(c) = node.child(i) {
            collect_leaves(c, byte_start, byte_end, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不喂谓词文本的 provider（query 无 `#match?` 类谓词时用）。
    fn no_text(_s: usize, _e: usize) -> Vec<u8> {
        Vec::new()
    }

    fn json_lang() -> tree_sitter::Language {
        tree_sitter::Language::from(tree_sitter_json::LANGUAGE)
    }

    #[test]
    fn json_highlight_emits_runs() {
        let lang = json_lang();
        let mut h = Highlighter::new(lang);
        let text = r#"{"a": 1, "b": "x"}"#;
        h.reset(text);
        let runs = h.highlight_range(0, text.len(), &ThemeConfig::default(), &|s, e| {
            (e - s) as u32
        }, &no_text);
        assert!(!runs.is_empty());
        // every run has a nonzero length and the lengths sum
        // to the text length in chars (ASCII: bytes == chars)
        let total: u32 = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, text.len() as u32);
    }

    #[test]
    fn incremental_edit_keeps_runs_consistent() {
        let lang = json_lang();
        let mut h = Highlighter::new(lang);
        let text = r#"{"a": 1}"#;
        h.reset(text);
        h.edit_at(6, 7, 8, Point { row: 0, column: 6 }, Point { row: 0, column: 7 }, Point { row: 0, column: 8 });
        h.parse_str(r#"{"a": 42}"#);
        let text2 = r#"{"a": 42}"#;
        let runs = h.highlight_range(0, text2.len(), &ThemeConfig::default(), &|s, e| {
            (e - s) as u32
        }, &no_text);
        let total: u32 = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, text2.len() as u32);
    }

    /// The typing path paints between keystrokes while the parse
    /// rides the debounce timer: the tree is folded to the new
    /// offsets but still carries the old tokenization. A delete
    /// leaves node ranges past the end of the (shorter) text, so
    /// `highlight_range` must clamp instead of panicking, and an
    /// insert must still produce runs that cover the new text.
    #[test]
    fn highlight_range_survives_a_folded_unparsed_edit() {
        let lang = json_lang();
        let mut h = Highlighter::new(lang);
        let text = String::from("{\n  \"abc\": \"value\",\n  \"d\": [1, 2, 3]\n}\n");
        h.reset(&text);

        // Delete a chunk in the middle (text shrinks, tree ranges overhang).
        let at = text.find("value").unwrap();
        h.edit_at(at, at + 6, at, Point { row: 1, column: 6 }, Point { row: 1, column: 12 }, Point { row: 1, column: 6 });
        h.fold_pending();
        let shorter: String = format!("{}{}", &text[..at], &text[at + 6..]);
        assert_eq!(h.doc_len(), shorter.len());
        let runs = h.highlight_range(0, shorter.len(), &ThemeConfig::default(), &|s, e| {
            (e - s) as u32
        }, &no_text);
        let total: u32 = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, shorter.len() as u32);

        // Insert a chunk (text grows past the tree's old end).
        h.edit_at(0, 0, 1, Point { row: 0, column: 0 }, Point { row: 0, column: 0 }, Point { row: 0, column: 1 });
        h.fold_pending();
        let longer = format!("z{shorter}");
        assert_eq!(h.doc_len(), longer.len());
        let runs = h.highlight_range(0, longer.len(), &ThemeConfig::default(), &|s, e| {
            (e - s) as u32
        }, &no_text);
        let total: u32 = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, longer.len() as u32);

        // Folding must not have moved the colors of the untouched
        // parts of the document: the `abc` inside `"abc"` is still
        // string-colored after the edits shifted it one byte right.
        let key_at = longer.find("\"abc\"").unwrap();
        let runs = h.highlight_range(key_at + 1, key_at + 4, &ThemeConfig::default(), &|s, e| {
            (e - s) as u32
        }, &no_text);
        assert_eq!(
            runs,
            vec![HighlightRun {
                color: 0xCE9178,
                len: 3,
                style: 0
            }],
            "string color must survive the fold"
        );
    }

    #[test]
    fn kind_at_returns_covering_leaf() {
        let lang = json_lang();
        let mut h = Highlighter::new(lang);
        let text = r#"{"a": 1}"#;
        h.reset(text);
        // byte 1 is the opening quote token
        // itself (the leaf covering that exact
        // byte, not the parent string node)
        assert_eq!(h.kind_at(1), Some("\""));
        // byte 2 (`a`) sits on the string's
        // content leaf
        assert_eq!(h.kind_at(2), Some("string_content"));
        // byte 0 is the anonymous `{` punctuation
        assert_eq!(h.kind_at(0), Some("{"));
        // at/after the end of the text there is
        // no covering leaf
        assert_eq!(h.kind_at(text.len()), None);
    }

    #[test]
    fn incremental_parse_matches_full_parse_under_random_edits() {
        // The typing path edits incrementally; this asserts the
        // incremental tree stays identical to a from-scratch parse
        // of the same text — highlighting and node kinds both.
        let lang = json_lang();
        let mut h = Highlighter::new(lang.clone());
        let mut full = Highlighter::new(lang);
        let mut text = String::from("{\n  \"a\": 1,\n  \"b\": [true, null]\n}\n");
        h.reset(&text);
        full.reset(&text);
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        // ASCII-only keeps byte offsets == char offsets.
        let alphabet = ['{', '}', '"', 'a', '1', ':', ',', ' ', '\n', '['];
        for step in 0..300 {
            let n = text.len();
            let at = (next() as usize) % (n + 1);
            let old_len = (next() as usize) % 6.min(n - at + 1);
            let ins_len = (next() as usize) % 4;
            let ins: String = (0..ins_len)
                .map(|_| alphabet[(next() as usize) % alphabet.len()])
                .collect();
            let point = |byte: usize, t: &str| {
                let mut row = 0usize;
                let mut col = 0usize;
                for (i, b) in t.bytes().enumerate() {
                    if i >= byte {
                        break;
                    }
                    if b == b'\n' {
                        row += 1;
                        col = 0;
                    } else {
                        col += 1;
                    }
                }
                Point { row, column: col }
            };
            let new_text = format!("{}{}{}", &text[..at], ins, &text[at + old_len..]);
            h.edit_at(
                at,
                at + old_len,
                at + ins.len(),
                point(at, &text),
                point(at + old_len, &text),
                point(at + ins.len(), &new_text),
            );
            h.parse_str(&new_text);
            text = new_text;
            full.reset(&text);
            assert_eq!(h.doc_len(), text.len(), "doc_len drifted at step {step}");
            let ranges = [
                (0, text.len()),
                (0, text.len() / 2),
                (text.len() / 3, (text.len() * 2 / 3).max(text.len() / 3 + 1)),
            ];
            let cl = &|s: usize, e: usize| (e - s) as u32;
            for (s, e) in ranges {
                let (s, e) = (s.min(text.len()), e.min(text.len()));
                assert_eq!(
                    h.highlight_range(s, e, &ThemeConfig::default(), cl, &no_text),
                    full.highlight_range(s, e, &ThemeConfig::default(), cl, &no_text),
                    "highlight mismatch on [{s},{e}) at step {step}"
                );
            }
        }
    }

    /// The feed-based parse (the app path) equals a whole-string
    /// parse: chunk boundaries must be invisible to the grammar.
    #[test]
    fn parse_feed_matches_parse_str() {
        let lang = json_lang();
        let text = "{\n  \"alpha\": [1, 2, 3],\n  \"beta\": {\"gamma\": true},\n  \"delta\": \"居中文本\"\n}\n";
        let mut a = Highlighter::new(lang.clone());
        let mut b = Highlighter::new(lang);
        a.parse_str(text);
        // 7-byte chunks: deliberately misaligned with every token
        b.reset_feed(text.len(), &mut |off| {
            let end = (off + 7).min(text.len());
            text.as_bytes()[off..end].to_vec()
        });
        let cl = &|s: usize, e: usize| (e - s) as u32;
        assert_eq!(
            a.highlight_range(0, text.len(), &ThemeConfig::default(), cl, &no_text),
            b.highlight_range(0, text.len(), &ThemeConfig::default(), cl, &no_text),
        );
        assert_eq!(a.kind_at(10), b.kind_at(10));
    }

    // ===== query 路径（queries/highlights.scm，plan §6 v2） =====

    /// capture 名按最长前缀落到默认色板：`string.escape` 比 `string`
    /// 更具体，`keyword.control.import` 落到 `keyword`。
    #[test]
    fn flatten_captures_last_writer_wins() {
        // [0,10) 红，[3,6) 蓝盖上去，[5,8) 绿再盖 —— 绿吃掉 5..6 的蓝
        // 和 6..8 的红，蓝只留 3..5，红只剩两头。
        let segs = flatten_captures(&[
            (0, 10, 0xFF0000, 0),
            (3, 6, 0x0000FF, 0),
            (5, 8, 0x00FF00, 0),
        ]);
        assert_eq!(
            segs,
            vec![
                (0, 3, 0xFF0000, 0),
                (3, 5, 0x0000FF, 0),
                (5, 8, 0x00FF00, 0),
                (8, 10, 0xFF0000, 0),
            ]
        );
    }

    #[test]
    fn flatten_captures_handles_adjacent_and_contained() {
        // 完全包含：后出现的整段覆盖。
        assert_eq!(
            flatten_captures(&[(0, 10, 1, 0), (2, 8, 2, 0)]),
            vec![(0, 2, 1, 0), (2, 8, 2, 0), (8, 10, 1, 0)]
        );
        // 首尾相接：不产生零长段，也不合并（颜色不同）。
        assert_eq!(
            flatten_captures(&[(0, 5, 1, 0), (5, 9, 2, 0)]),
            vec![(0, 5, 1, 0), (5, 9, 2, 0)]
        );
        // 同色相邻合成一段。
        assert_eq!(
            flatten_captures(&[(0, 5, 1, 0), (5, 9, 1, 0)]),
            vec![(0, 9, 1, 0)]
        );
        // 同色但字形不同：**不**合并 —— 合并会让后半段跟着前半段变粗。
        assert_eq!(
            flatten_captures(&[(0, 5, 1, 0), (5, 9, 1, STYLE_BOLD)]),
            vec![(0, 5, 1, 0), (5, 9, 1, STYLE_BOLD)]
        );
    }

    #[test]
    fn capture_color_uses_longest_prefix() {
        let theme = ThemeConfig::default();
        assert_eq!(theme.capture_color("keyword"), 0x569CD6);
        assert_eq!(theme.capture_color("keyword.control.import"), 0x569CD6);
        assert_eq!(theme.capture_color("string.special"), 0xCE9178);
        assert_eq!(theme.capture_color("string.escape"), 0xD7BA7D, "更长前缀胜出");
        assert_eq!(theme.capture_color("nonsense"), theme.default_color);
    }

    /// 字形和颜色走同一套查找：kind 子串、capture 最长前缀，没列到的是正体。
    #[test]
    fn styles_use_the_same_lookup_as_colors() {
        let mut theme = ThemeConfig::default();
        theme.style_rules.push(("comment", STYLE_ITALIC));
        theme.style_rules.push(("keyword", STYLE_BOLD));
        theme.capture_styles.push(("comment".to_string(), STYLE_ITALIC));
        theme
            .capture_styles
            .push(("comment.documentation".to_string(), STYLE_ITALIC | STYLE_BOLD));
        assert_eq!(theme.style_for("line_comment"), STYLE_ITALIC);
        assert_eq!(theme.style_for("keyword_return"), STYLE_BOLD);
        assert_eq!(theme.style_for("identifier"), 0);
        assert_eq!(theme.capture_style("comment.line"), STYLE_ITALIC);
        assert_eq!(
            theme.capture_style("comment.documentation"),
            STYLE_ITALIC | STYLE_BOLD,
            "最长前缀胜出"
        );
        assert_eq!(theme.capture_style("string"), 0, "没列进表的是正体");
    }

    /// 字形要一路跟到 run 上：`HighlightRun` 是绘制那条路唯一的输入。
    #[test]
    fn highlight_runs_carry_style() {
        let lang = json_lang();
        let mut h = Highlighter::new(lang);
        h.set_query("((string) @string)").expect("query compiles");
        let mut theme = ThemeConfig::default();
        theme.capture_styles.push(("string".to_string(), STYLE_BOLD));
        let text = r#"{"a": 1}"#;
        h.reset(text);
        let runs = h.highlight_range(0, text.len(), &theme, &|s, e| (e - s) as u32, &no_text);
        assert!(
            runs.iter().any(|r| r.len == 3 && r.style == STYLE_BOLD),
            "字符串那一段是粗体: {runs:?}"
        );
        assert!(
            runs.iter().filter(|r| r.len == 3).count() == 1,
            "只有一段 3 字的 run: {runs:?}"
        );
    }

    /// 挂上 highlights.scm 后，颜色来自 capture 而不是 node kind。
    #[test]
    fn query_highlights_use_capture_colors() {
        let lang = json_lang();
        let mut h = Highlighter::new(lang);
        h.set_query("((string) @string) ((number) @constant.numeric)")
            .expect("query compiles");
        assert!(h.has_query());
        let text = r#"{"a": 1}"#;
        h.reset(text);
        let runs = h.highlight_range(0, text.len(), &ThemeConfig::default(), &|s, e| {
            (e - s) as u32
        }, &no_text);
        let total: u32 = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, text.len() as u32, "runs 覆盖整行");
        assert!(
            runs.iter().any(|r| r.color == 0xCE9178 && r.len == 3),
            "字符串按 @string 上色: {runs:?}"
        );
        assert!(
            runs.iter().any(|r| r.color == 0xB5CEA8 && r.len == 1),
            "数字按 @constant.numeric 上色: {runs:?}"
        );
    }

    /// 跨行 token：块注释的**中间行**（没有 capture 从本行开始）也要
    /// 上色 —— 逐行查询靠 `set_byte_range` 的相交语义命中。
    #[test]
    fn query_multiline_capture_colors_middle_lines() {
        let lang = tree_sitter::Language::from(tree_sitter_rust::LANGUAGE);
        let mut h = Highlighter::new(lang);
        h.set_query("(block_comment) @comment").expect("query compiles");
        // line0 "/* aa\n" = 0..6, line1 " bb\n" = 6..10, line2 " cc */\n" = 10..17
        let text = "/* aa\n bb\n cc */\nlet x = 1;\n";
        h.reset(text);
        let runs = h.highlight_range(10, 16, &ThemeConfig::default(), &|s, e| {
            (e - s) as u32
        }, &no_text);
        assert_eq!(
            runs,
            vec![HighlightRun {
                color: 0x6A9955,
                len: 6,
                style: 0
            }],
            "中间行整行是注释色"
        );
    }

    /// 谓词（`#match?`）的文本由 `node_text` 提供：只有匹配的节点上色。
    #[test]
    fn query_predicate_uses_node_text() {
        let lang = json_lang();
        let mut h = Highlighter::new(lang);
        h.set_query(r#"((number) @constant.numeric (#match? @constant.numeric "^42$"))"#)
            .expect("query compiles");
        let text = r#"[42, 7]"#;
        h.reset(text);
        let bytes = text.as_bytes();
        let provider = |s: usize, e: usize| bytes[s.min(bytes.len())..e.min(bytes.len())].to_vec();
        let runs = h.highlight_range(0, text.len(), &ThemeConfig::default(), &|s, e| {
            (e - s) as u32
        }, &provider);
        assert!(
            runs.iter().any(|r| r.color == 0xB5CEA8 && r.len == 2),
            "42 命中谓词: {runs:?}"
        );
        assert!(
            !runs.iter().any(|r| r.color == 0xB5CEA8 && r.len == 1),
            "7 不满足 #match? 不上色: {runs:?}"
        );
    }

    /// 编译失败的 query 不改变行为：kind 路径继续出全量 runs。
    #[test]
    fn bad_query_keeps_kind_path() {
        let lang = json_lang();
        let mut h = Highlighter::new(lang);
        assert!(h.set_query("(((").is_err());
        assert!(!h.has_query());
        let text = r#"{"a": 1}"#;
        h.reset(text);
        let runs = h.highlight_range(0, text.len(), &ThemeConfig::default(), &|s, e| {
            (e - s) as u32
        }, &no_text);
        let total: u32 = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, text.len() as u32);
    }

    /// 快照路径必须和树路径画出同样的 run —— 它只是把同一棵树压小，
    /// 不是另一套配色。query 与 kind 两条路都验，整段与逐行都比。
    #[test]
    fn snapshot_matches_tree_runs() {
        let text = "{\n  \"alpha\": [1, 2, 3],\n  \"beta\": {\"gamma\": true},\n  \"delta\": \"居中文本\"\n}\n";
        let theme = ThemeConfig::default();
        let cl = |s: usize, e: usize| (e - s) as u32;
        for scm in [
            Some("((string) @string) ((number) @constant.numeric)"),
            None,
        ] {
            let mut tree = Highlighter::new(json_lang());
            if let Some(scm) = scm {
                tree.set_query(scm).unwrap();
            }
            tree.parse_str(text);
            let want = tree.highlight_range(0, text.len(), &theme, &cl, &no_text);

            let mut snap = Highlighter::new(json_lang());
            if let Some(scm) = scm {
                snap.set_query(scm).unwrap();
            }
            snap.parse_str(text);
            assert!(snap.compact(&no_text));
            assert!(snap.is_snapshot_mode());
            assert!(snap.tree().is_none(), "快照模式下树已经丢掉");
            assert!(!snap.snapshot_stale());
            assert_eq!(
                snap.highlight_range(0, text.len(), &theme, &cl, &no_text),
                want,
                "整段的 run 一致（scm={scm:?}）"
            );
            let mut start = 0;
            for line in text.split_inclusive('\n') {
                let end = start + line.len();
                assert_eq!(
                    snap.highlight_range(start, end, &theme, &cl, &no_text),
                    tree.highlight_range(start, end, &theme, &cl, &no_text),
                    "逐行一致：{line:?}"
                );
                start = end;
            }
        }
    }

    /// 折叠区在压表时整份取出：跨全文件的一个区域（从第二行到最后一
    /// 行）不能因为丢了树就变短或消失。
    #[test]
    fn snapshot_keeps_whole_fold_regions() {
        let text = "[\n  {\n    \"a\": 1\n}]\n";
        let mut h = Highlighter::new(json_lang());
        h.set_fold_query("((object) @fold)").unwrap();
        h.parse_str(text);
        let want = h.fold_spans(&no_text);
        assert_eq!(want.len(), 1, "只在对象上折一道: {want:?}");
        let (s, e) = want[0];
        assert_eq!(text[..s].matches('\n').count(), 1, "区域从第 2 行开始");
        assert_eq!(
            text[..e].matches('\n').count(),
            text.trim_end_matches('\n').lines().count() - 1,
            "区域一直到最后一行的对象结尾"
        );

        assert!(h.compact(&no_text));
        assert!(h.tree().is_none());
        assert_eq!(h.fold_spans(&no_text), want, "快照里的折叠区原样保留");
    }

    fn rust_lang() -> tree_sitter::Language {
        tree_sitter::Language::from(tree_sitter_rust::LANGUAGE)
    }

    const RUST_SCM: &str = "(function_item name: (identifier) @function)
(string_literal) @string
(line_comment) @comment
(identifier) @variable
(type_identifier) @type
(integer_literal) @constant.numeric
";

    /// 编辑之后局部重算：窗口从所在方法的起点起解析，拼出来的快照要和
    /// 整篇重算逐段一致 —— 这是「打字跟手、又不常驻整棵树」的依据。
    /// query 与 kind 两条路都验。
    #[test]
    fn window_reparse_matches_full_snapshot() {
        let text = "use std::fmt;\n\nfn alpha() {\n    let a = 1;\n}\n\nfn beta() {\n    let b = \"two\";\n}\n\nfn gamma() {\n    let c = 3;\n}\n";
        let theme = ThemeConfig::default();
        let cl = |s: usize, e: usize| (e - s) as u32;
        for scm in [Some(RUST_SCM), None] {
            let mut h = Highlighter::new(rust_lang());
            if let Some(scm) = scm {
                h.set_query(scm).unwrap();
            }
            h.parse_str(text);
            assert!(h.compact(&no_text));
            // 在 beta 的字符串里插一个字符。
            let at = text.find("\"two\"").unwrap() + 1;
            let p = Point { row: 7, column: 13 };
            h.edit_at(at, at, at + 1, p, p, Point { row: 7, column: 14 });
            let floor = h.restart_floor(at + 1).expect("有快照");
            assert!(
                floor >= text.find("fn beta").unwrap(),
                "窗口要从 beta 起算（floor={floor}，scm={scm:?}）"
            );
            assert!(floor <= at, "起点不能晚于改点");
            let new_text = format!("{}x{}", &text[..at], &text[at..]);

            let mut full = Highlighter::new(rust_lang());
            if let Some(scm) = scm {
                full.set_query(scm).unwrap();
            }
            full.parse_str(&new_text);
            assert!(full.compact(&no_text));

            let slice = &new_text[floor..];
            let mut w = Highlighter::new(rust_lang());
            if let Some(scm) = scm {
                w.set_query(scm).unwrap();
            }
            w.parse_str(slice);
            let paint =
                w.window_paint(floor, slice.len(), &|s: usize, e: usize| {
                    slice.as_bytes()[s..e].to_vec()
                });
            assert!(h.apply_window(paint), "窗口并进快照（scm={scm:?}）");
            assert_eq!(
                h.highlight_range(0, new_text.len(), &theme, &cl, &no_text),
                full.highlight_range(0, new_text.len(), &theme, &cl, &no_text),
                "局部重算要和整篇重算一致（scm={scm:?}）"
            );
        }
    }

    /// 改点落在折叠区里时，这道折叠不能消失：终点跟着新字节平移，
    /// 箭头在打字期间不会闪。
    #[test]
    fn snapshot_keeps_fold_spanning_the_edit() {
        let text = "[\n  {\n    \"a\": 1\n}]\n";
        let mut h = Highlighter::new(json_lang());
        h.set_fold_query("((object) @fold)").unwrap();
        h.parse_str(text);
        let want = h.fold_spans(&no_text);
        assert_eq!(want.len(), 1);
        assert!(h.compact(&no_text));
        let at = text.find("1").unwrap();
        let p = Point { row: 2, column: 9 };
        h.edit_at(at, at, at + 1, p, p, Point { row: 2, column: 10 });
        let got = h.fold_spans(&no_text);
        assert_eq!(got.len(), 1, "跨过改点的折叠保留: {got:?}");
        assert_eq!(got[0].0, want[0].0, "起点不动");
        assert_eq!(got[0].1, want[0].1 + 1, "终点跟着 +1");
    }

    /// 快照模式下编辑只平移区间，不解析；doc_len 跟住，快照标记为落后。
    #[test]
    fn snapshot_edit_shifts_without_parsing() {
        let text = r#"{"a": 1}"#;
        let mut h = Highlighter::new(json_lang());
        h.set_query("((string) @string)").unwrap();
        h.parse_str(text);
        assert!(h.compact(&no_text));
        // 在 "a" 前面插入一个字符：字节域 +1。
        let at = 2;
        let (row, col) = (0, at);
        let p = Point {
            row,
            column: col,
        };
        h.edit_at(at, at, at + 1, p, p, Point { row, column: col + 1 });
        assert_eq!(h.doc_len(), text.len() + 1);
        assert!(h.snapshot_stale(), "编辑后等后台重算");
        assert!(!h.needs_parse(), "快照模式不在界面线程上建树");
    }

    /// 界面线程的即时局部重算（[`Highlighter::parse_window`]）：解析、压表、
    /// 拼回快照一次完成，结果和整篇重算一致；树在返回前就丢掉。快照还没
    /// 落地时这一趟必须拒绝 —— 没有底表可拼。
    #[test]
    fn parse_window_updates_the_snapshot_in_place() {
        let text = "use std::fmt;\n\nfn alpha() {\n    let a = 1;\n}\n\nfn beta() {\n    let b = \"two\";\n}\n";
        let theme = ThemeConfig::default();
        let cl = |s: usize, e: usize| (e - s) as u32;
        let mut h = Highlighter::new(rust_lang());
        h.set_query(RUST_SCM).unwrap();
        h.parse_str(text);
        assert!(h.compact(&no_text));
        // 快照没来（刚 defer、整篇还在后台跑）：界面线程这趟让路。
        let mut fresh = Highlighter::new(rust_lang());
        fresh.defer_snapshot(text.len());
        assert!(
            !fresh.parse_window(0, text.as_bytes(), &no_text),
            "没有快照可拼，退回工作线程"
        );
        // 在 beta 的字符串里插一个字符，然后一段式重算。
        let at = text.find("\"two\"").unwrap() + 1;
        let p = Point { row: 7, column: 13 };
        h.edit_at(at, at, at + 1, p, p, Point { row: 7, column: 14 });
        let new_text = format!("{}x{}", &text[..at], &text[at..]);
        let floor = h.restart_floor(at).expect("有快照");
        let bytes = new_text[floor..].as_bytes().to_vec();
        let nt =
            |s: usize, e: usize| bytes[s.min(bytes.len())..e.min(bytes.len())].to_vec();
        assert!(h.parse_window(floor, &bytes, &nt), "窗口当场拼进快照");
        assert!(h.tree().is_none(), "树只在这一趟里活着");
        let mut full = Highlighter::new(rust_lang());
        full.set_query(RUST_SCM).unwrap();
        full.parse_str(&new_text);
        assert!(full.compact(&no_text));
        assert_eq!(
            h.highlight_range(0, new_text.len(), &theme, &cl, &no_text),
            full.highlight_range(0, new_text.len(), &theme, &cl, &no_text),
            "即时重算要和整篇重算一致"
        );
    }

    /// 覆盖记账：立表只算过它那一窗，窗外答「没算过」，窗口层据此再派一窗；
    /// 补上的窗和已有的相接之后算全，整篇压完一律算过。
    #[test]
    fn snapshot_knows_which_bytes_it_computed() {
        let text = "fn alpha() {\n    let a = 1;\n}\n\nfn beta() {\n    let b = \"two\";\n}\n";
        let mid = text.find("fn beta").unwrap();
        let nt_all = |s: usize, e: usize| {
            text.as_bytes()[s.min(text.len())..e.min(text.len())].to_vec()
        };

        let mut h = Highlighter::new(rust_lang());
        h.set_query(RUST_SCM).unwrap();
        h.defer_snapshot(text.len());
        assert!(!h.covers(0, text.len()), "还没有表就是没算过");

        // 前半窗：立表。
        let mut w1 = Highlighter::new(rust_lang());
        w1.set_query(RUST_SCM).unwrap();
        w1.parse_str(&text[..mid]);
        let p1 = w1.window_paint(0, mid, &nt_all);
        assert!(h.apply_window(p1));
        assert!(h.covers(0, mid), "立表那一段算过了");
        assert!(h.covers(0, mid - 1), "段内任意一点也算");
        assert!(!h.covers(mid, text.len()), "窗外还没算");
        assert!(!h.covers(mid - 4, mid + 4), "跨出窗外就是没算全");

        // 后半窗：拼进来，两条相接 → 全文算过。
        let mut w2 = Highlighter::new(rust_lang());
        w2.set_query(RUST_SCM).unwrap();
        w2.parse_str(&text[mid..]);
        let p2 = w2.window_paint(mid, text.len() - mid, &nt_all);
        assert!(h.apply_window(p2));
        assert!(h.covers(0, text.len()), "接上的两窗算覆盖了整篇");

        // 整篇压表：一条铺满。
        let mut full = Highlighter::new(rust_lang());
        full.set_query(RUST_SCM).unwrap();
        full.parse_str(text);
        assert!(full.compact(&nt_all));
        assert!(full.covers(0, text.len()));

        // 编辑把区间跟着平移：窗外扩了一格，答案要跟着改。
        let mut h2 = Highlighter::new(rust_lang());
        h2.set_query(RUST_SCM).unwrap();
        h2.parse_str(text);
        assert!(h2.compact(&nt_all));
        let p = text.find("let a").unwrap();
        let pt = tree_sitter::Point { row: 1, column: 4 };
        h2.edit_at(p, p, p + 1, pt, pt, pt);
        assert!(h2.covers(0, text.len() + 1), "平移之后仍然算过");
    }

    /// 还没有底表时，一窗重解析就是第一张表：打开之后先让可见那一段有颜色，
    /// 它算出来的区间要和整篇压表的一致。立表不算整篇重算完 —— 折叠区和
    /// 远处的语法状态还等在那一趟（`snapshot_stale` 仍为真）。
    #[test]
    fn window_paint_seeds_the_snapshot() {
        let text = "fn alpha() {\n    let a = 1;\n}\n\nfn beta() {\n    let b = \"two\";\n}\n";
        let theme = ThemeConfig::default();
        let cl = |s: usize, e: usize| (e - s) as u32;
        let nt = |s: usize, e: usize| text.as_bytes()[s..e].to_vec();
        let mut w = Highlighter::new(rust_lang());
        w.set_query(RUST_SCM).unwrap();
        w.parse_str(text);
        let paint = w.window_paint(0, text.len(), &nt);

        let mut h = Highlighter::new(rust_lang());
        h.set_query(RUST_SCM).unwrap();
        h.defer_snapshot(text.len());
        assert!(!h.has_snapshot(), "整篇那趟还在后台");
        assert!(h.apply_window(paint), "没有底表时这一窗就是第一张表");
        assert!(h.has_snapshot());
        assert!(h.snapshot_stale(), "立表不冒充整篇重算");
        assert!(h.tree().is_none(), "界面线程仍然不常驻树");

        let mut full = Highlighter::new(rust_lang());
        full.set_query(RUST_SCM).unwrap();
        full.parse_str(text);
        assert!(full.compact(&no_text));
        assert_eq!(
            h.highlight_range(0, text.len(), &theme, &cl, &no_text),
            full.highlight_range(0, text.len(), &theme, &cl, &no_text),
            "立表那一段的颜色要和整篇一致"
        );
    }
}
