/// Column ruler model: the tick spacing and the
/// width the ruler paints into.
pub struct RulerModel {
    pub tab_px: f32,
    pub width: f32,
}

impl RulerModel {
    pub fn new(tab_px: f32, width: f32) -> Self {
        Self { tab_px, width }
    }

    /// Tick positions: every tab stop within
    /// the ruler width.
    pub fn ticks(&self) -> Vec<f32> {
        let mut v = Vec::new();
        let mut x = self.tab_px;
        while x < self.width {
            v.push(x);
            x += self.tab_px;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_at_tab_stops() {
        let r = RulerModel::new(80.0, 300.0);
        assert_eq!(r.ticks(), vec![80.0, 160.0, 240.0]);
    }
}
