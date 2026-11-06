/// How the collapsible regions of a document are derived.
///
/// This is `setting.ini`'s `Edit/FoldingMethod`, the value the Code Folding
/// submenu writes: 0 for 取消折叠, 1 for 语法折叠 — promoted to 3 when the lexer is
/// the tag-based `anytext` one — and 2 for 缩进折叠; the submenu hands the number to
/// the editor before persisting it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FoldingMethod {
    /// Syntax folding is what the default config implies when the
    /// `FoldingMethod` key is absent.
    #[default]
    Syntax,
    Indent,
    /// this is the path for languages without a block grammar; without a
    /// tag scanner it derives no regions, so the gutter stays empty.
    Tag,
    /// Cancel Folding: the document has no regions at all.
    None,
}

impl FoldingMethod {
    /// The number written to the editor and to `setting.ini`.
    pub fn id(self) -> i64 {
        match self {
            FoldingMethod::Syntax => 1,
            FoldingMethod::Indent => 2,
            FoldingMethod::Tag => 3,
            FoldingMethod::None => 0,
        }
    }

    pub fn from_id(id: i64) -> Self {
        match id {
            2 => FoldingMethod::Indent,
            3 => FoldingMethod::Tag,
            0 => FoldingMethod::None,
            _ => FoldingMethod::Syntax,
        }
    }
}

/// Regions derived from indentation — the &Indent row (1332).
///
/// `indentations` holds each line's leading indentation in tab-stop columns, or
/// `None` for a blank line. Blank lines take part in no decision, which is what
/// keeps a trailing blank line inside the region above it instead of ending that
/// region one line early.
pub fn indent_regions(
    indentations: &[Option<usize>],
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    // (indentation of the opening line, the opening line itself).
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut prev: Option<(usize, usize)> = None;
    for (line, ind) in indentations.iter().enumerate() {
        let Some(indent) = *ind else {
            continue;
        };
        if let Some((before, last)) = prev {
            if indent > before {
                // A deeper line folds everything under the line that precedes it.
                open.push((before, last));
            } else {
                // Any region opened at this indentation level or deeper is complete.
                while open
                    .last()
                    .is_some_and(|(top, _)| indent <= *top)
                {
                    let (_, start) = open.pop().unwrap();
                    if last > start {
                        out.push((start, last));
                    }
                }
            }
        }
        prev = Some((indent, line));
    }
    if let Some((_, last)) = prev {
        while let Some((_, start)) = open.pop() {
            if last > start {
                out.push((start, last));
            }
        }
    }
    out.sort();
    out
}

/// Per-line folded flags, one per document line. Tree-sitter folding
/// spans (dg-syntax) are converted into these flags by the app layer.
#[derive(Default)]
pub struct Folding {
    folded: Vec<bool>,
}

impl Folding {
    pub fn ensure(&mut self, lines: usize) {
        if self.folded.len() < lines {
            self.folded.resize(lines, false);
        }
    }

    /// Folded flag count over `[0, line)` — the hidden tally kept
    /// while building the display-number cache, i.e. the number of
    /// lines hidden above `line`.
    pub fn folded_before(&self, line: usize) -> usize {
        self.folded
            .iter()
            .take(line)
            .filter(|folded| **folded)
            .count()
    }

    /// The number the gutter paints for `line`
    /// `line + 1` minus the lines hidden
    /// above it. Hidden lines themselves return `usize::MAX`
    /// but they are never painted.
    pub fn display_number(&self, line: usize) -> usize {
        if self.is_folded(line) {
            return usize::MAX;
        }
        line + 1 - self.folded_before(line)
    }

    /// Visible line count over `[0, total)` — the digit-count
    /// basis of the view update (the total minus the hidden count).
    pub fn visible_line_count(&self, total: usize) -> usize {
        total - self.folded_before(total)
    }

    pub fn is_folded(&self, line: usize) -> bool {
        self.folded.get(line).copied().unwrap_or(false)
    }

    pub fn set(&mut self, line: usize, folded: bool) {
        self.ensure(line + 1);
        self.folded[line] = folded;
    }

    pub fn toggle(&mut self, line: usize) -> bool {
        let cur = self.is_folded(line);
        self.set(line, !cur);
        !cur
    }

    /// Applies definition spans `(start_line, end_line)`; the lines
    /// strictly between them become folded.
    pub fn apply_spans(&mut self, spans: impl IntoIterator<Item = (usize, usize)>) {
        for (start, end) in spans {
            for line in (start + 1)..=end {
                self.set(line, true);
            }
        }
    }

    pub fn clear(&mut self) {
        self.folded.clear();
    }

    pub fn len(&self) -> usize {
        self.folded.len()
    }

    /// Folded flag per line, for the
    /// render-layer fold markers.
    pub fn as_slice(&self) -> &[bool] {
        &self.folded
    }
}
