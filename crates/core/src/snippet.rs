//! Snippet engine — the `.snippet` format
//! (TextMate-style): `#title=` / `#t=` /
//! `#g=` directives, `${n:default}` and
//! `$n` tab stops, `$0` final cursor
//! position, `$SELECTION` substitution
//! and `\#` / `\$` / `\\` escapes.

/// One numbered placeholder in the
/// expanded text (char offsets, so the
/// caller can add them to the insertion
/// point to address the document).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabStop {
    pub index: u32,
    pub start: usize,
    pub end: usize,
}

/// The result of expanding one snippet.
#[derive(Clone, Debug)]
pub struct ExpandedSnippet {
    pub text: String,
    /// Every stop occurrence (mirrors
    /// included), in source order.
    pub tab_stops: Vec<TabStop>,
    /// Char offset of `$0`; the end of
    /// the text when the snippet has no
    /// explicit `$0`.
    pub final_pos: usize,
}

impl ExpandedSnippet {
    /// The distinct stop indices in
    /// navigation order (1, 2,... then
    /// 0 last).
    pub fn stop_order(&self) -> Vec<u32> {
        let mut indices: Vec<u32> = self
            .tab_stops
            .iter()
            .map(|s| s.index)
            .filter(|&i| i != 0)
            .collect();
        indices.sort_unstable();
        indices.dedup();
        // the final position ($0, or the
        // end of the text) is the last
        // navigation target whenever the
        // snippet has numbered stops
        if !indices.is_empty() {
            indices.push(0);
        }
        indices
    }

    /// The first (mirrors excluded) range
    /// of stop `index`.
    pub fn range_of(&self, index: u32) -> Option<TabStop> {
        self.tab_stops
            .iter()
            .find(|s| s.index == index)
            .copied()
    }
}

/// One snippet: a trigger word, its
/// display title and the raw body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub title: String,
    pub trigger: String,
    pub body: String,
}

/// A parsed `.snippet` file.
#[derive(Clone, Debug, Default)]
pub struct SnippetFile {
    pub title: String,
    pub snippets: Vec<Snippet>,
}

impl SnippetFile {
    /// Parses `.snippet` content; `None`
    /// when no snippet carries both a
    /// title and a trigger.
    pub fn parse(text: &str) -> Option<Self> {
        let mut file = SnippetFile::default();
        let mut cur_title = String::new();
        let mut cur_trigger = String::new();
        let mut cur_body: Vec<&str> = Vec::new();
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("#title=") {
                file.title = v.to_string();
            } else if let Some(v) = line.strip_prefix("#t=") {
                flush(
                    &mut file,
                    &cur_title,
                    &cur_trigger,
                    &cur_body,
                );
                cur_title = v.to_string();
                cur_trigger.clear();
                cur_body.clear();
            } else if let Some(v) = line.strip_prefix("#g=") {
                cur_trigger = v.to_string();
            } else {
                cur_body.push(line);
            }
        }
        flush(
            &mut file,
            &cur_title,
            &cur_trigger,
            &cur_body,
        );
        if file.snippets.is_empty() {
            None
        } else {
            Some(file)
        }
    }

    /// The first snippet whose trigger is
    /// exactly `word`.
    pub fn find(&self, word: &str) -> Option<&Snippet> {
        self.snippets
            .iter()
            .find(|s| s.trigger == word)
    }
}

/// Commits the snippet under assembly
/// (trailing blank lines are separators,
/// not body).
fn flush(
    file: &mut SnippetFile,
    title: &str,
    trigger: &str,
    body: &[&str],
) {
    if title.is_empty() || trigger.is_empty() {
        return;
    }
    let mut end = body.len();
    while end > 0 && body[end - 1].trim().is_empty() {
        end -= 1;
    }
    file.snippets.push(Snippet {
        title: title.to_string(),
        trigger: trigger.to_string(),
        body: body[..end].join("\n"),
    });
}

/// Expands `body`, substituting `selection`
/// for `$SELECTION` and resolving tab
/// stops. `selection` is empty when no
/// text is selected.
pub fn expand(
    body: &str,
    selection: &str,
) -> ExpandedSnippet {
    let mut p = Parser {
        src: body.chars().collect(),
        pos: 0,
        out: String::new(),
        char_count: 0,
        stops: Vec::new(),
        final_pos: None,
        selection,
    };
    p.parse(0);
    let final_pos = p
        .final_pos
        .unwrap_or(p.char_count);
    ExpandedSnippet {
        text: p.out,
        tab_stops: p.stops,
        final_pos,
    }
}

struct Parser<'a> {
    src: Vec<char>,
    pos: usize,
    out: String,
    char_count: usize,
    stops: Vec<TabStop>,
    final_pos: Option<usize>,
    selection: &'a str,
}

impl<'a> Parser<'a> {
    /// Parses literal text and placeholders
    /// until the end of input or the
    /// matching `}` (when `depth > 0`,
    /// i.e. inside a `${n:default}`).
    fn parse(&mut self, depth: usize) {
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            match c {
                '\\' if self.pos + 1 < self.src.len() => {
                    // escape: the next char
                    // is emitted literally
                    self.pos += 1;
                    let e = self.src[self.pos];
                    self.out.push(e);
                    self.char_count += 1;
                    self.pos += 1;
                }
                '$' => {
                    self.pos += 1;
                    self.parse_dollar(depth);
                }
                '}' if depth > 0 => {
                    // closes the enclosing
                    // `${n:...}`; the caller
                    // consumes it
                    return;
                }
                _ => {
                    self.out.push(c);
                    self.char_count += 1;
                    self.pos += 1;
                }
            }
        }
    }

    /// Parses what follows a `$` (the `$`
    /// itself is already consumed).
    fn parse_dollar(&mut self, depth: usize) {
        let braced =
            self.pos < self.src.len() && self.src[self.pos] == '{';
        if braced {
            self.pos += 1;
        }
        let mut index = 0u32;
        let mut has_index = false;
        while self.pos < self.src.len()
            && self.src[self.pos].is_ascii_digit()
        {
            index = index * 10
                + (self.src[self.pos] as u32 - '0' as u32);
            has_index = true;
            self.pos += 1;
        }
        if braced {
            if self.pos < self.src.len()
                && self.src[self.pos] == ':'
            {
                // ${n:default} — the default
                // may nest further stops
                self.pos += 1;
                let start = self.char_count;
                self.parse(depth + 1);
                let end = self.char_count;
                if self.pos < self.src.len()
                    && self.src[self.pos] == '}'
                {
                    self.pos += 1;
                }
                self.stops.push(TabStop {
                    index,
                    start,
                    end,
                });
                return;
            }
            if self.pos < self.src.len()
                && self.src[self.pos] == '}'
            {
                // ${n} or ${0}
                self.pos += 1;
                self.push_stop(index);
                return;
            }
            // ${IDENT} — SELECTION or unknown
            let ident = self.read_ident();
            if self.pos < self.src.len()
                && self.src[self.pos] == '}'
            {
                self.pos += 1;
            }
            if ident == "SELECTION" {
                self.push_selection();
            } else {
                self.emit_literal_braced(&ident);
            }
            return;
        }
        if has_index {
            // $n or $0
            self.push_stop(index);
            return;
        }
        // $IDENT — SELECTION or literal $
        let ident = self.read_ident();
        if ident == "SELECTION" {
            self.push_selection();
        } else {
            self.out.push('$');
            self.char_count += 1;
            self.out.push_str(&ident);
            self.char_count += ident.chars().count();
        }
    }

    /// Reads an alphabetic identifier from
    /// the current position.
    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.src.len()
            && self.src[self.pos].is_alphabetic()
        {
            self.pos += 1;
        }
        self.src[start..self.pos].iter().collect()
    }

    /// Records a numbered stop; `0` is the
    /// final cursor position, not a stop.
    fn push_stop(&mut self, index: u32) {
        if index == 0 {
            self.final_pos = Some(self.char_count);
        } else {
            let p = self.char_count;
            self.stops.push(TabStop {
                index,
                start: p,
                end: p,
            });
        }
    }

    /// Substitutes the selected text.
    fn push_selection(&mut self) {
        self.out.push_str(self.selection);
        self.char_count += self.selection.chars().count();
    }

    /// Re-emits an unknown braced form
    /// (e.g. `${foo}`) verbatim.
    fn emit_literal_braced(&mut self, ident: &str) {
        self.out.push_str("${");
        self.out.push_str(ident);
        self.out.push('}');
        self.char_count += 2 + ident.chars().count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_directives() {
        let file = SnippetFile::parse(
            "#title=Py\n#t=For Loop\n#g=for\nfor ${1:x} in ${2:y}:\n\t$0\n",
        )
        .unwrap();
        assert_eq!(file.title, "Py");
        assert_eq!(file.snippets.len(), 1);
        let s = &file.snippets[0];
        assert_eq!(s.title, "For Loop");
        assert_eq!(s.trigger, "for");
        assert_eq!(s.body, "for ${1:x} in ${2:y}:\n\t$0");
    }

    #[test]
    fn multiple_snippets() {
        let file = SnippetFile::parse(
            "#title=T\n#t=A\n#g=a\nbodyA\n\n#t=B\n#g=b\nbodyB\n",
        )
        .unwrap();
        assert_eq!(file.snippets.len(), 2);
        assert_eq!(file.snippets[0].body, "bodyA");
        assert_eq!(file.snippets[1].body, "bodyB");
        assert!(file.find("b").is_some());
        assert!(file.find("z").is_none());
    }

    #[test]
    fn expands_stops_and_final() {
        let e = expand("def ${1:name}($2):\n\t$0", "");
        assert_eq!(e.text, "def name():\n\t");
        assert_eq!(e.final_pos, e.text.chars().count());
        assert_eq!(e.range_of(1), Some(TabStop { index: 1, start: 4, end: 8 }));
        assert_eq!(e.range_of(2), Some(TabStop { index: 2, start: 9, end: 9 }));
        assert_eq!(e.stop_order(), vec![1, 2, 0]);
    }

    #[test]
    fn selection_substitution() {
        let e = expand("if $SELECTION:\n\t$0", "foo");
        assert_eq!(e.text, "if foo:\n\t");
    }

    #[test]
    fn mirror_stops_recorded() {
        // both occurrences of stop 2 carry
        // a default, so both render "p"
        // and mirror at edit time
        let e = expand("<${2:p}>$0</${2:p}>", "");
        assert_eq!(e.text, "<p></p>");
        let twos: Vec<&TabStop> = e
            .tab_stops
            .iter()
            .filter(|s| s.index == 2)
            .collect();
        assert_eq!(twos.len(), 2);
        assert_eq!(twos[0].start, 1);
        assert_eq!(twos[0].end, 2);
        assert_eq!(twos[1].start, 5);
        assert_eq!(twos[1].end, 6);
    }

    #[test]
    fn escapes() {
        let e = expand("\\#define $1", "");
        assert_eq!(e.text, "#define ");
    }

    #[test]
    fn nested_default() {
        // ${2: ${3:Exception}} — the outer
        // default nests stop 3
        let e = expand("except ${2: ${3:Exception}}:", "");
        assert_eq!(e.text, "except  Exception:");
        assert_eq!(e.range_of(2).unwrap().start, 7);
        assert_eq!(e.range_of(2).unwrap().end, 17);
        assert_eq!(e.range_of(3).unwrap().start, 8);
        assert_eq!(e.range_of(3).unwrap().end, 17);
    }

    #[test]
    fn no_final_defaults_to_end() {
        let e = expand("plain text", "");
        assert_eq!(e.final_pos, 10);
        assert!(e.tab_stops.is_empty());
        assert_eq!(e.stop_order(), Vec::<u32>::new());
    }
}
