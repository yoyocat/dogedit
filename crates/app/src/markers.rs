//! 自定义标记: the process-wide marker set behind `cm_misc_usermarker_*`.
//!
//! The set is a singleton shared by every view. An entry holds the text, its
//! row checkbox, the marker slot the view draws with, and the per-entry match
//! options. Adding a text that already exists is an upsert: the existing slot
//! is reused. `cm_misc_usermarker_enable` toggles one flag and refreshes every
//! view.
//!
//! Marker paint uses the theme's marker colours; the active-line band is
//! composited at half strength on the caret row.

use dg_core::{Document, FindOptions};

/// The eight slots 使用标记1 … 使用标记8 address.
pub const SLOTS: usize = 8;

/// One row of the dialog's list.
#[derive(Clone, Debug)]
pub struct Marker {
    pub text: String,
    /// Which of the eight colours draws this entry. Two entries may share a
    /// slot.
    pub slot: usize,
    /// The row's own check box: an unchecked entry stays in the list but stops
    /// marking.
    pub checked: bool,
    /// 匹配大小写 / 全字匹配 / 正则表达式, per entry.
    pub opts: FindOptions,
}

impl Marker {
    fn from_slot(text: &str, slot: usize) -> Self {
        Self {
            text: text.to_string(),
            slot,
            checked: true,
            opts: FindOptions {
                case_sensitive: true,
                whole_word: false,
                regex: false,
                backward: false,
            },
        }
    }
}

/// The singleton list plus the 开启该功能 flag.
#[derive(Clone, Debug, Default)]
pub struct MarkSet {
    pub entries: Vec<Marker>,
    /// the state field. Off means the list is kept but nothing is drawn.
    pub enabled: bool,
    /// The round-robin slot a brand-new entry receives.
    next_slot: usize,
    /// Bumped by every mutation, so a cached span list can tell it is stale.
    version: u64,
}

/// One marked span: `(line, start_col, end_col, slot)`.
pub type Span = (usize, usize, usize, usize);

impl MarkSet {
    pub fn store(&mut self, text: &str, slot: usize) {
        if text.is_empty() || slot >= SLOTS {
            return;
        }
        self.enabled = true;
        if let Some(m) = self.entries.iter_mut().find(|m| m.text == text) {
            m.slot = slot;
            m.checked = true;
        } else {
            self.entries
                .push(Marker::from_slot(text, self.next_slot % SLOTS));
            self.next_slot += 1;
            if let Some(last) = self.entries.last_mut() {
                last.slot = slot;
            }
        }
        self.version += 1;
    }

    /// `cm_misc_usermarker_enable` (1471). Returns the new flag so the caller
    /// knows a repaint is owed.
    pub fn toggle_enabled(&mut self) -> bool {
        self.enabled = !self.enabled;
        self.version += 1;
        self.enabled
    }

    /// 添加: the dialog's own way in, keeping the options the check boxes
    /// currently show.
    pub fn add(&mut self, text: &str, opts: FindOptions) {
        if text.is_empty() || self.entries.iter().any(|m| m.text == text) {
            return;
        }
        let slot = self.next_slot % SLOTS;
        self.next_slot += 1;
        self.entries.push(Marker {
            text: text.to_string(),
            slot,
            checked: true,
            opts,
        });
        self.version += 1;
    }

    pub fn remove(&mut self, idx: usize) {
        if idx < self.entries.len() {
            self.entries.remove(idx);
            self.version += 1;
        }
    }

    /// The pencil button: rename the selected row's text. A name already in the
    /// list is not a second row, so this keeps the upsert's meaning.
    pub fn rename(&mut self, idx: usize, text: &str) {
        let Some(old) = self.entries.get(idx).map(|m| m.text.clone()) else {
            return;
        };
        if text.is_empty() || old == text {
            return;
        }
        match self.entries.iter().position(|e| e.text == text) {
            // A name already in the list is not a second row: the row taking it
            // is the one that goes.
            Some(at) if at != idx => {
                self.entries.remove(idx);
            }
            _ => {
                if let Some(m) = self.entries.get_mut(idx) {
                    m.text = text.to_string();
                }
            }
        }
        self.version += 1;
    }

    /// ↑ / ↓ (1205/1206): the row order *is* the slot order the dialog shows, so
    /// moving a row swaps the two entries' slots rather than their colours.
    pub fn move_row(&mut self, idx: usize, up: bool) {
        let other = if up { idx.checked_sub(1) } else { Some(idx + 1) };
        let Some(other) = other.filter(|o| *o < self.entries.len()) else {
            return;
        };
        self.entries.swap(idx, other);
        self.version += 1;
    }

    pub fn set_checked(&mut self, idx: usize, on: bool) {
        if let Some(m) = self.entries.get_mut(idx) {
            m.checked = on;
            self.version += 1;
        }
    }

    /// The ✓ / ▢ buttons at the bottom left of the list.
    pub fn set_all_checked(&mut self, on: bool) {
        for m in &mut self.entries {
            m.checked = on;
        }
        self.version += 1;
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// Every marked span of `doc`, sorted by line then column — the paint side
    /// slices this with a `partition_point` on the line number. An empty set or
    /// a disabled flag costs nothing.
    pub fn spans(&self, doc: &Document) -> Vec<Span> {
        if !self.enabled {
            return Vec::new();
        }
        let mut out: Vec<Span> = Vec::new();
        for m in self.entries.iter().filter(|m| m.checked) {
            for hit in dg_core::find_all(doc, &m.text, m.opts) {
                let (sl, sc) = hit.start;
                let (el, ec) = hit.end;
                // A multi-line marker (a pasted selection with newlines in it)
                // has no single span to draw
                if sl != el {
                    continue;
                }
                out.push((sl, sc, ec, m.slot));
            }
        }
        out.sort_unstable_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        out
    }

    /// The spans that fall on one line, as `(start, end, slot)`.
    pub fn spans_for(spans: &[Span], line: usize) -> Vec<(usize, usize, usize)> {
        let from = spans.partition_point(|s| s.0 < line);
        spans[from..]
            .iter()
            .take_while(|s| s.0 == line)
            .map(|&(_, a, b, slot)| (a, b, slot))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        Document::from_utf8(text.as_bytes())
    }

    /// Re-slot rather than duplicate.
    #[test]
    fn store_is_an_upsert_that_enables_the_set() {
        let mut s = MarkSet::default();
        s.store("alpha", 0);
        assert!(s.enabled);
        assert_eq!(s.entries.len(), 1);
        s.store("beta", 3);
        s.store("alpha", 5);
        assert_eq!(s.entries.len(), 2, "a known text must not add a row");
        assert_eq!(s.entries[0].slot, 5);
        assert_eq!(s.entries[1].slot, 3);
        s.store("", 1);
        assert_eq!(s.entries.len(), 2);
    }

    #[test]
    fn spans_track_the_options_and_the_enable_flag() {
        let d = doc("alpha here\r\nbeta again\r\nAlpha once\r\n");
        let mut s = MarkSet::default();
        s.store("alpha", 0);
        let spans = s.spans(&d);
        assert_eq!(spans, vec![(0, 0, 5, 0)], "case-sensitive by default");
        s.entries[0].opts.case_sensitive = false;
        assert_eq!(s.spans(&d), vec![(0, 0, 5, 0), (2, 0, 5, 0)]);
        s.entries[0].opts.whole_word = true;
        s.store("again", 1);
        assert_eq!(
            MarkSet::spans_for(&s.spans(&d), 1),
            vec![(5, 10, 1)],
            "the span list must stay sorted by line then column"
        );
        s.toggle_enabled();
        assert!(s.spans(&d).is_empty(), "off means nothing is drawn");
    }
}
