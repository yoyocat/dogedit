use crate::wrap::WrapStore;
use dg_core::Folding;

/// Vertical row index
/// `LineToVisualRow`. A Fenwick tree over per-line
/// visual-row counts (folded lines contribute 0).
///
/// 大文件模式用 [`RowIndex::identity`]：不换行不折叠 ⇒ 行号即行
/// （row == line），O(1) 直通且不为千万行建 Fenwick/包装数组。
pub struct RowIndex {
    imp: Imp,
    n: usize,
}

enum Imp {
    Fenwick(Vec<i64>),
    /// 行==行（无换行无折叠）：直通映射。
    Identity,
}

impl Default for RowIndex {
    fn default() -> Self {
        Self {
            imp: Imp::Fenwick(Vec::new()),
            n: 0,
        }
    }
}

impl RowIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// `lines` 行的恒等索引（大文件模式）。
    pub fn identity(lines: usize) -> Self {
        Self {
            imp: Imp::Identity,
            n: lines,
        }
    }

    /// Rebuild from the wrap store; folded lines contribute 0.
    pub fn rebuild(&mut self, store: &WrapStore, folding: &Folding) {
        self.n = store.arrays.len();
        self.imp = Imp::Fenwick(vec![0i64; self.n + 1]);
        let Imp::Fenwick(tree) = &mut self.imp else { unreachable!() };
        for (line, arr) in store.arrays.iter().enumerate() {
            let rows = if folding.is_folded(line) {
                0
            } else {
                arr.count() as i64 + 1
            };
            Self::add_to(tree, self.n, line, rows);
        }
    }

    fn add_to(tree: &mut [i64], n: usize, line: usize, delta: i64) {
        let mut i = line + 1;
        while i <= n {
            tree[i] += delta;
            i += i & i.wrapping_neg();
        }
    }

    /// Point-updates one line's visual-row count (the keystroke
    /// path: a re-wrap changed one line's rows; line indices did
    /// not move). O(log n). No-op on the identity index (nothing
    /// wraps there).
    pub fn set_line(&mut self, line: usize, old_rows: i64, new_rows: i64) {
        if old_rows == new_rows || line >= self.n {
            return;
        }
        if let Imp::Fenwick(tree) = &mut self.imp {
            Self::add_to(tree, self.n, line, new_rows - old_rows);
        }
    }

    /// Line count the index was built for; a mismatch with the wrap
    /// store means the index is stale and owes a rebuild.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Prefix sum of visual rows for lines `[0, line)`.
    pub fn line_to_visual_row(&self, line: usize) -> usize {
        match &self.imp {
            Imp::Identity => line.min(self.n),
            Imp::Fenwick(tree) => {
                let mut i = line.min(self.n);
                let mut sum = 0i64;
                while i > 0 {
                    sum += tree[i];
                    i -= i & i.wrapping_neg();
                }
                sum as usize
            }
        }
    }

    pub fn total_rows(&self) -> usize {
        match &self.imp {
            Imp::Identity => self.n,
            Imp::Fenwick(_) => self.line_to_visual_row(self.n),
        }
    }

    /// Inverse mapping: visual row -> (folding line, segment
    /// within the line). Fenwick binary search; folded lines are
    /// skipped naturally because they contribute 0.
    pub fn row_to_line_segment(&self, row: usize) -> Option<(usize, usize)> {
        if self.n == 0 {
            return None;
        }
        match &self.imp {
            Imp::Identity => {
                if row < self.n {
                    Some((row, 0))
                } else {
                    None
                }
            }
            Imp::Fenwick(tree) => {
                let target = row as i64;
                let mut pos = 0usize; // 1-based line count
                let mut sum = 0i64;
                let mut bit = 1usize << (usize::BITS - 1 - self.n.leading_zeros());
                while bit != 0 {
                    let next = pos + bit;
                    if next <= self.n && sum + tree[next] <= target {
                        pos = next;
                        sum += tree[next];
                    }
                    bit >>= 1;
                }
                if pos >= self.n {
                    return None;
                }
                Some((pos, (target - sum) as usize))
            }
        }
    }

    /// Visible visual-row range, clamped to the total.
    pub fn visible_range(&self, first_row: usize, height_rows: usize) -> std::ops::Range<usize> {
        let total = self.total_rows();
        let first = first_row.min(total);
        let end = (first + height_rows).min(total);
        first..end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenwick_prefix_and_inverse() {
        let mut store = WrapStore::default();
        store.ensure(3);
        // line 0: 2 breaks (3 rows), line 1: 1 break
        // (2 rows), line 2: 1 break (2 rows)
        store.arrays[0].data = vec![5, 10];
        store.arrays[1].data = vec![4];
        store.arrays[2].data = vec![7];
        let folding = Folding::default();
        let mut idx = RowIndex::new();
        idx.rebuild(&store, &folding);
        assert_eq!(idx.total_rows(), 3 + 2 + 2);
        assert_eq!(idx.line_to_visual_row(0), 0);
        assert_eq!(idx.line_to_visual_row(1), 3);
        assert_eq!(idx.line_to_visual_row(2), 5);
        // inverse mapping
        assert_eq!(idx.row_to_line_segment(0), Some((0, 0)));
        assert_eq!(idx.row_to_line_segment(2), Some((0, 2)));
        assert_eq!(idx.row_to_line_segment(3), Some((1, 0)));
        assert_eq!(idx.row_to_line_segment(4), Some((1, 1)));
        assert_eq!(idx.row_to_line_segment(6), Some((2, 1)));
        assert_eq!(idx.row_to_line_segment(7), None);
    }

    #[test]
    fn identity_index_maps_row_to_line() {
        let idx = RowIndex::identity(1000);
        assert_eq!(idx.total_rows(), 1000);
        assert_eq!(idx.line_to_visual_row(0), 0);
        assert_eq!(idx.line_to_visual_row(500), 500);
        assert_eq!(idx.line_to_visual_row(999), 999);
        assert_eq!(idx.line_to_visual_row(5000), 1000); // 夹取
        assert_eq!(idx.row_to_line_segment(0), Some((0, 0)));
        assert_eq!(idx.row_to_line_segment(999), Some((999, 0)));
        assert_eq!(idx.row_to_line_segment(1000), None);
        assert_eq!(idx.visible_range(995, 10).len(), 5);
        // 点更新是 no-op（大文件无换行）
        let mut idx2 = RowIndex::identity(10);
        idx2.set_line(3, 1, 2);
        assert_eq!(idx2.total_rows(), 10);
    }

    #[test]
    fn folded_lines_are_skipped() {
        let mut store = WrapStore::default();
        store.ensure(3);
        store.arrays[0].data = vec![5];
        store.arrays[1].data = vec![4];
        store.arrays[2].data = vec![7];
        let mut folding = Folding::default();
        folding.set(1, true);
        let mut idx = RowIndex::new();
        idx.rebuild(&store, &folding);
        assert_eq!(idx.total_rows(), 2 + 2);
        // row 2 belongs to line 2, segment 0 — line 1 is folded
        assert_eq!(idx.row_to_line_segment(2), Some((2, 0)));
        assert_eq!(idx.row_to_line_segment(1), Some((0, 1)));
    }
}
