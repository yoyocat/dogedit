//! The bookmark model behind the Bookmark menu: Toggle Ctrl+F2, Next F2, Previous
//! Shift+F2, Remove All.
//!
//! Bookmarks are line-indexed. The `adjust` hook shifts them when lines are inserted or
//! removed above them, so they follow the text rather than a fixed number.

use std::collections::BTreeSet;

#[derive(Default)]
pub struct Bookmarks {
    lines: BTreeSet<usize>,
}

impl Bookmarks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Toggles `line`; returns the new
    /// state (true = bookmarked).
    pub fn toggle(&mut self, line: usize) -> bool {
        if self.lines.remove(&line) {
            false
        } else {
            self.lines.insert(line);
            true
        }
    }

    pub fn has(&self, line: usize) -> bool {
        self.lines.contains(&line)
    }

    /// Clears the mark on `line`.
    pub fn remove(&mut self, line: usize) -> bool {
        self.lines.remove(&line)
    }

    /// The marked lines, ascending. 复制/剪切标记行 walks them in this order
    /// because the clipboard block has to read top to bottom.
    pub fn lines(&self) -> Vec<usize> {
        self.lines.iter().copied().collect()
    }

    /// First bookmark strictly after
    /// `from`, wrapping to the first.
    pub fn next(&self, from: usize) -> Option<usize> {
        self.lines
            .range((from + 1)..)
            .next()
            .copied()
            .or_else(|| self.lines.iter().next().copied())
    }

    /// Last bookmark strictly before
    /// `from`, wrapping to the last.
    pub fn prev(&self, from: usize) -> Option<usize> {
        self.lines
            .range(..from)
            .next_back()
            .copied()
            .or_else(|| self.lines.iter().next_back().copied())
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// Replaces the whole set — 会话恢复 hands back the lines a file carried
    /// when it was closed, and that is a set, not a sequence of toggles.
    pub fn set_lines(&mut self, lines: impl IntoIterator<Item = usize>) {
        self.lines = lines.into_iter().collect();
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Shifts bookmarks at or after
    /// `line` by `delta` lines (positive
    /// when lines are inserted above,
    /// negative when removed).
    pub fn adjust(&mut self, line: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        let moved: Vec<usize> = self
            .lines
            .range(line..)
            .copied()
            .collect();
        for l in moved {
            self.lines.remove(&l);
            if delta > 0 {
                self.lines.insert(l + delta as usize);
            } else {
                let new = l as isize + delta;
                if new >= 0 {
                    self.lines.insert(new as usize);
                }
            }
        }
    }

    /// Bookmark flags per line, for the
    /// render-layer markers.
    pub fn flags(&self, line_count: usize) -> Vec<bool> {
        let mut out = vec![false; line_count];
        for l in &self.lines {
            if *l < line_count {
                out[*l] = true;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_and_has() {
        let mut b = Bookmarks::new();
        assert!(b.toggle(3));
        assert!(b.has(3));
        assert!(!b.toggle(3));
        assert!(!b.has(3));
    }

    #[test]
    fn next_wraps() {
        let mut b = Bookmarks::new();
        b.toggle(2);
        b.toggle(5);
        b.toggle(8);
        assert_eq!(b.next(0), Some(2));
        assert_eq!(b.next(2), Some(5));
        assert_eq!(b.next(5), Some(8));
        // wraps to the first
        assert_eq!(b.next(8), Some(2));
    }

    #[test]
    fn prev_wraps() {
        let mut b = Bookmarks::new();
        b.toggle(2);
        b.toggle(5);
        assert_eq!(b.prev(8), Some(5));
        assert_eq!(b.prev(5), Some(2));
        // wraps to the last
        assert_eq!(b.prev(2), Some(5));
    }

    #[test]
    fn adjust_shifts() {
        let mut b = Bookmarks::new();
        b.toggle(5);
        b.toggle(1);
        // two lines inserted at line 3:
        // the bookmark at 5 moves to 7,
        // the one at 1 stays
        b.adjust(3, 2);
        assert!(b.has(7));
        assert!(b.has(1));
        assert!(!b.has(5));
        // one line removed at line 2:
        // 7 -> 6, 1 stays
        b.adjust(2, -1);
        assert!(b.has(6));
        assert!(b.has(1));
    }

    #[test]
    fn clear_and_flags() {
        let mut b = Bookmarks::new();
        b.toggle(0);
        b.toggle(2);
        let f = b.flags(4);
        assert_eq!(f, vec![true, false, true, false]);
        b.clear();
        assert!(b.is_empty());
    }
}
