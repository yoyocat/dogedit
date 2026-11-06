//! One set of DPI scaling rules for the chrome.
//!
//! Two shapes exist because callers hold DPI in two forms: dialogs that
//! already computed a `dpi / 96` factor pass an `f32`, while the UI-font
//! helpers pass the raw `GetDpiForWindow` value and want integer rounding.
//! Both used to be copied into every dialog; there is now one definition each.

/// Scale `v` DIP by an already-computed `dpi / 96` factor.
pub(super) fn scale(v: i32, dpi: f32) -> i32 {
    (v as f32 * dpi).round() as i32
}

/// `px` scaled from 96 DPI to `dpi`, rounding halves up, never below 1 px.
pub(super) fn scaled(px: i32, dpi: i32) -> i32 {
    ((px * dpi + 48) / 96).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_rounds_halves_up_and_floors_at_one() {
        assert_eq!(scaled(8, 96), 8);
        assert_eq!(scaled(8, 144), 12);
        assert_eq!(scaled(1, 96), 1);
        assert_eq!(scaled(0, 96), 1);
    }

    #[test]
    fn scale_uses_the_factor_directly() {
        assert_eq!(scale(10, 1.5), 15);
        assert_eq!(scale(-10, 1.25), -13);
    }
}
