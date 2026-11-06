//! What a popup menu is *made of*, with no opinion about how it is drawn.
//!
//! There is deliberately no popup widget in this crate. A menu is modal: it
//! grabs the mouse and the keyboard, it must cover every window of the
//! application, and it must hand back exactly one command id. Every platform
//! already ships something that does that — Win32 builds an `HMENU` from this
//! list and tracks it at the system level — so a hand-drawn menu here would buy
//! nothing but the two things the native control is better at: clipping itself
//! to the monitor it opened on, and staying on top of the document window.
//!
//! The ids are the shell's command ids, not a row index: the row that comes
//! back from tracking is dispatched straight into the command layer.

/// One row of a popup menu.
#[derive(Clone, Debug)]
pub struct MenuItem {
    /// Command id reported back when the row is chosen.
    pub id: u32,
    pub label: String,
    pub enabled: bool,
    /// A rule between groups rather than a row.
    pub separator: bool,
    /// The row that matches the document's current value.
    pub checked: bool,
    /// Rows of a submenu. A popup row carries no dispatchable id of its own.
    pub children: Vec<MenuItem>,
}

impl MenuItem {
    pub fn new(id: u32, label: &str) -> Self {
        Self {
            id,
            label: label.to_string(),
            enabled: true,
            separator: false,
            checked: false,
            children: Vec::new(),
        }
    }

    /// A submenu parent. `id` is kept for the lang/state lookup even though
    /// choosing the row itself opens the submenu instead of dispatching.
    pub fn popup(id: u32, label: &str, children: Vec<MenuItem>) -> Self {
        Self {
            id,
            label: label.to_string(),
            enabled: true,
            separator: false,
            checked: false,
            children,
        }
    }

    pub fn separator() -> Self {
        Self {
            id: 0,
            label: String::new(),
            enabled: false,
            separator: true,
            checked: false,
            children: Vec::new(),
        }
    }

    pub fn with_checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A separator has nothing to check and nothing to dispatch, and the
    /// backend keys off `separator` before it looks at either, so the defaults
    /// have to be inert rather than merely unused.
    #[test]
    fn separator_rows_are_inert() {
        let s = MenuItem::separator();
        assert!(s.separator);
        assert!(!s.enabled);
        assert_eq!(s.id, 0);
        assert!(!s.checked);
        assert!(s.label.is_empty());
    }

    #[test]
    fn builders_touch_only_their_own_field() {
        let m = MenuItem::new(7, "CRLF").with_checked(true);
        assert_eq!((m.id, m.label.as_str(), m.enabled, m.checked), (7, "CRLF", true, true));
        let m = m.with_enabled(false);
        assert!(!m.enabled && m.checked);
    }

    /// A leaf has none, which is what the backend builds `MF_POPUP` from.
    #[test]
    fn popup_rows_keep_their_children_and_their_own_id() {
        let m = MenuItem::popup(
            91,
            "更多粘贴(&M)...",
            vec![MenuItem::new(91, "列模式粘贴(&P)"), MenuItem::separator()],
        );
        assert_eq!(m.id, 91);
        assert_eq!(m.children.len(), 2);
        assert!(!m.separator && m.enabled);
        assert!(MenuItem::new(91, "x").children.is_empty());
    }
}
