use bitflags::bitflags;

bitflags! {
    /// Per-character line flags: one byte per char covers
    /// these bits.
    pub struct CharFlags: u8 {
        const NONE = 0;
        const IN_SELECTION = 0x20;
        const BOUNDARY = 0x40;
        const ANCHOR = 0x0B;
    }
}

/// (line, char-within-line) point.
pub type Point = (usize, usize);

/// Selection range in document coordinates.
/// Also the shape of a find match (see [`crate::find::Match`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: Point,
    pub end: Point,
}

impl Range {
    /// Normalizes so start <= end in (line, col) order.
    pub fn normalized(mut self) -> Self {
        if (self.end.0, self.end.1) < (self.start.0, self.start.1) {
            std::mem::swap(&mut self.start, &mut self.end);
        }
        self
    }
}

/// Interval set, not a range tree.
#[derive(Default)]
pub struct SelectionSet {
    pub ranges: Vec<Range>,
}

impl SelectionSet {
    pub fn clear(&mut self) {
        self.ranges.clear();
    }

    pub fn add(&mut self, range: Range) {
        self.ranges.push(range.normalized());
    }

    pub fn contains(&self, line: usize, col: usize) -> bool {
        self.ranges
            .iter()
            .any(|r| (line, col) >= r.start && (line, col) < r.end)
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

/// Reused per-view flag buffer
/// arrays (the field family): no allocation on the paint
/// path, just `clear` + `extend`.
pub struct FlagBuffer {
    pub flags: Vec<u8>,
}

impl Default for FlagBuffer {
    fn default() -> Self {
        Self { flags: Vec::new() }
    }
}

impl FlagBuffer {
    /// Computes selection flags for one line. Two passes: in-selection
    /// membership, then boundary detection (a char is a boundary when
    /// its membership differs from a neighbour's).
    pub fn compute_line(&mut self, line_len: usize, sel: &SelectionSet, line_no: usize) {
        self.flags.clear();
        self.flags.reserve(line_len);
        let mut membership = Vec::with_capacity(line_len);
        for col in 0..line_len {
            membership.push(sel.contains(line_no, col));
        }
        for (col, &inside) in membership.iter().enumerate() {
            let mut f = if inside { CharFlags::IN_SELECTION } else { CharFlags::NONE };
            let prev = membership.get(col.wrapping_sub(0)).copied().unwrap_or(false);
            let before = if col == 0 { false } else { membership[col - 1] };
            let after = membership.get(col + 1).copied().unwrap_or(false);
            if inside != before || inside != after {
                f |= CharFlags::BOUNDARY;
            }
            let _ = prev;
            self.flags.push(f.bits());
        }
    }
}
