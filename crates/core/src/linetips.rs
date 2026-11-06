//! 行注记：每行可挂一个图标和一段提示文本，鼠标悬停在图标区的图标上时
//! 把文本弹成提示。条目按行号索引，`adjust` 在行被插入/删除时平移它们，
//! 让注记跟着文本走而不是钉死在行号上。

use std::collections::BTreeMap;

/// 图标种类。绘制层按编号画代码生成的矢量图形。
pub const ICON_INFO: u16 = 0;
pub const ICON_WARNING: u16 = 1;
pub const ICON_ERROR: u16 = 2;
pub const ICON_FLAG: u16 = 3;
pub const ICON_COUNT: u16 = 4;

/// One row of the annotation table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineTip {
    /// Which code-drawn glyph the icon strip shows.
    pub icon: u16,
    /// The tooltip text, one line per `\n`.
    pub text: String,
}

#[derive(Default)]
pub struct LineTips {
    map: BTreeMap<usize, LineTip>,
}

impl LineTips {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets (or replaces) the annotation on
    /// `line`.
    pub fn set(&mut self, line: usize, icon: u16, text: &str) {
        self.map.insert(line, LineTip {
            icon,
            text: text.to_string(),
        });
    }

    /// Appends `text` to the annotation on
    /// `line`, creating it (with `icon`) when
    /// the line has none yet. An existing
    /// entry keeps its icon unless it has none.
    pub fn append(&mut self, line: usize, icon: u16, text: &str) {
        match self.map.get_mut(&line) {
            Some(tip) => {
                if !tip.text.is_empty() && !text.is_empty() {
                    tip.text.push('\n');
                }
                tip.text.push_str(text);
            }
            None => self.set(line, icon, text),
        }
    }

    pub fn get(&self, line: usize) -> Option<&LineTip> {
        self.map.get(&line)
    }

    /// Drops the annotation on `line`; true
    /// when there was one.
    pub fn remove(&mut self, line: usize) -> bool {
        self.map.remove(&line).is_some()
    }

    /// The annotated lines, ascending.
    pub fn lines(&self) -> Vec<usize> {
        self.map.keys().copied().collect()
    }

    /// The annotations in `lo..hi`, ascending — what the icon
    /// strip paints for the visible rows.
    pub fn in_range(&self, lo: usize, hi: usize) -> Vec<(usize, &LineTip)> {
        self.map
            .range(lo..hi)
            .map(|(l, t)| (*l, t))
            .collect()
    }

    /// First annotated line strictly after
    /// `from`, wrapping to the first.
    pub fn next(&self, from: usize) -> Option<usize> {
        self.map
            .range((from + 1)..)
            .next()
            .map(|(l, _)| *l)
            .or_else(|| self.map.keys().next().copied())
    }

    /// Last annotated line strictly before
    /// `from`, wrapping to the last.
    pub fn prev(&self, from: usize) -> Option<usize> {
        self.map
            .range(..from)
            .next_back()
            .map(|(l, _)| *l)
            .or_else(|| self.map.keys().next_back().copied())
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Shifts annotations at or after `line` by
    /// `delta` lines (positive when lines are
    /// inserted above, negative when removed).
    /// Annotations shifted past the document
    /// end are dropped by the caller's follow-up
    /// range queries, not here.
    pub fn adjust(&mut self, line: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        let moved: Vec<usize> = self.map.range(line..).map(|(l, _)| *l).collect();
        for l in moved {
            let tip = self.map.remove(&l).unwrap();
            if delta > 0 {
                self.map.insert(l + delta as usize, tip);
            } else {
                let new = l as isize + delta;
                if new >= 0 {
                    self.map.insert(new as usize, tip);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_remove() {
        let mut t = LineTips::new();
        t.set(3, ICON_ERROR, "bad token");
        assert_eq!(t.get(3).map(|x| x.icon), Some(ICON_ERROR));
        assert_eq!(t.get(3).map(|x| x.text.as_str()), Some("bad token"));
        assert!(t.remove(3));
        assert!(!t.remove(3));
        assert!(t.get(3).is_none());
    }

    #[test]
    fn append_joins_with_newline() {
        let mut t = LineTips::new();
        t.append(1, ICON_INFO, "first");
        t.append(1, ICON_WARNING, "second");
        t.append(1, ICON_WARNING, "");
        assert_eq!(t.get(1).map(|x| x.text.as_str()), Some("first\nsecond"));
        // a fresh append keeps the requested icon
        assert_eq!(t.get(1).map(|x| x.icon), Some(ICON_INFO));
        t.remove(1);
        t.append(1, ICON_WARNING, "only");
        assert_eq!(t.get(1).map(|x| x.icon), Some(ICON_WARNING));
    }

    #[test]
    fn next_prev_wrap() {
        let mut t = LineTips::new();
        t.set(2, ICON_FLAG, "a");
        t.set(5, ICON_FLAG, "b");
        t.set(8, ICON_FLAG, "c");
        assert_eq!(t.next(0), Some(2));
        assert_eq!(t.next(2), Some(5));
        assert_eq!(t.next(8), Some(2));
        assert_eq!(t.prev(8), Some(5));
        assert_eq!(t.prev(2), Some(8));
    }

    #[test]
    fn adjust_shifts_like_bookmarks() {
        let mut t = LineTips::new();
        t.set(5, ICON_ERROR, "x");
        t.set(1, ICON_INFO, "y");
        // two lines inserted at line 3: 5 -> 7, 1 stays
        t.adjust(3, 2);
        assert!(t.get(7).is_some());
        assert!(t.get(1).is_some());
        assert!(t.get(5).is_none());
        // one line removed at line 2: 7 -> 6, 1 stays
        t.adjust(2, -1);
        assert!(t.get(6).is_some());
        assert!(t.get(1).is_some());
        // removing the anchor line's own content: 1 -> 0
        t.adjust(0, -1);
        assert!(t.get(0).is_some());
    }

    #[test]
    fn in_range_is_sorted_subslice() {
        let mut t = LineTips::new();
        for l in [1usize, 4, 7, 9] {
            t.set(l, ICON_INFO, "n");
        }
        let got: Vec<usize> = t.in_range(2, 8).iter().map(|(l, _)| *l).collect();
        assert_eq!(got, vec![4, 7]);
    }

    #[test]
    fn clear_empties() {
        let mut t = LineTips::new();
        t.set(0, ICON_FLAG, "x");
        t.set(3, ICON_FLAG, "y");
        t.clear();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
    }
}
