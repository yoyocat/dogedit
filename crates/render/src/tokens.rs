//! 主题 token：给 [`Theme`] 的每个颜色字段起一个**名字**。
//!
//! 皮肤里写 `tab.active_bg = "#3A3A3A"`，而不是「第 63 个字段」。名字表是
//! 对外契约，和 `dg-skin` 的 `Region`/`State` 一样只能加不能改。
//!
//! 名字的形状是 `<region>.<slot>`，region 取自 [`dg_skin::Region`] 的名字
//! （`editor.gutter` 这种带点的 region 直接用），slot 是自由名。所以
//! `editor.gutter.folding_bg` 读作「editor.gutter 区域的 folding_bg 槽」。
//!
//! 为什么用宏而不是 89 个手写 `if`：`set_token` 和 `token` 必须覆盖**同一组**
//! 字段，分开手写迟早会漏一个 —— 漏的那个表现为「皮肤里写了但没反应」，最难查。
//! 宏把「名字 → 字段」写一次，两个方向同时生成，再加一个测试逐个走一遍。
//!
//! 这不是 `Theme` 的替代品：`Theme` 仍是唯一的数据结构，token 只是它的一张
//! 索引。`apply_theme_file` 那条老路径完全不受影响。

use crate::theme::{Color, Theme};

/// 名字 → 字段。`$field` 后面可选的 `[$idx]` 给 `[Color; 8]` 这种数组字段用。
macro_rules! theme_tokens {
    ($($name:literal => $field:ident $( [ $idx:literal ] )? ),* $(,)?) => {
        /// 全部 token 名字，顺序即定义顺序。
        pub const TOKEN_NAMES: &[&str] = &[$($name),*];

        impl Theme {
            /// 按名字写一个颜色。名字不认识返回 `false`，一个字段都不动。
            pub fn set_token(&mut self, name: &str, color: Color) -> bool {
                match name {
                    $($name => { self.$field $( [$idx] )? = color; true })*
                    _ => false,
                }
            }

            /// 按名字读一个颜色。名字不认识返回 `None`。
            pub fn token(&self, name: &str) -> Option<Color> {
                match name {
                    $($name => Some(self.$field $( [$idx] )?),)*
                    _ => None,
                }
            }
        }
    };
}

theme_tokens! {
    // 编辑区
    "editor.bg" => background,
    "editor.fg" => foreground,
    "editor.caret" => caret,
    "editor.caret_glow" => caret_glow,
    "editor.caret_indicator" => sel_indicator,
    "editor.selection" => selection_bg,
    "editor.selection_fg" => selection_fg,
    "editor.current_line" => current_line_bg,
    "editor.below" => below_bg,
    "editor.margin" => margin_bg,
    "editor.indent_line" => indent_line,
    "editor.boundary" => boundary_line,
    "editor.whitespace" => whitespace_fg,
    "editor.ruler" => ruler_fg,
    "editor.minimap_viewport" => minimap_viewport,
    "editor.line_modified" => line_state_modified,
    "editor.line_saved" => line_state_saved,
    "editor.bookmark" => bookmark_fg,
    "editor.tag" => tag_fg,
    "editor.pair" => pair_bg,
    "editor.found_bg" => found_bg,
    "editor.found_fg" => found_fg,
    "editor.url_bg" => url_bg,
    "editor.url_fg" => url_fg,
    "editor.email_bg" => email_bg,
    "editor.email_fg" => email_fg,
    "editor.word2_bg" => word2_bg,
    "editor.word2_fg" => word2_fg,
    "editor.conchar_bg" => con_char_bg,
    "editor.conchar_fg" => con_char_fg,

    // 编辑区左侧的四条 gutter
    "editor.gutter.bg" => gutter_bg,
    "editor.gutter.fg" => gutter_fg,
    "editor.gutter.folding_bg" => folding_bg,
    "editor.gutter.folding_fg" => folding_fg,
    "editor.gutter.icon_bg" => icon_bg,
    "editor.gutter.icon_fg" => icon_fg,

    // 自动完成弹窗
    "popup.bg" => word_complete_bg,
    "popup.fg" => word_complete_fg,

    // 八组「标记」色对（自定义标记命令 1318）
    "editor.marker0.bg" => marker_bg[0],
    "editor.marker1.bg" => marker_bg[1],
    "editor.marker2.bg" => marker_bg[2],
    "editor.marker3.bg" => marker_bg[3],
    "editor.marker4.bg" => marker_bg[4],
    "editor.marker5.bg" => marker_bg[5],
    "editor.marker6.bg" => marker_bg[6],
    "editor.marker7.bg" => marker_bg[7],
    "editor.marker0.fg" => marker_fg[0],
    "editor.marker1.fg" => marker_fg[1],
    "editor.marker2.fg" => marker_fg[2],
    "editor.marker3.fg" => marker_fg[3],
    "editor.marker4.fg" => marker_fg[4],
    "editor.marker5.fg" => marker_fg[5],
    "editor.marker6.fg" => marker_fg[6],
    "editor.marker7.fg" => marker_fg[7],

    // 滚动条
    "scroll.bg" => scrollbar_bg,
    "scroll.thumb" => scrollbar_thumb,
    "scroll.thumb_hover" => scrollbar_thumb_hover,
    "scroll.arrow" => scrollbar_arrow,

    // 状态栏
    "status.bg" => statusbar_bg,
    "status.fg" => statusbar_fg,
    "status.divider" => statusbar_divider,

    // 停靠面板
    "panel.bg" => panel_bg,
    "panel.fg" => panel_fg,
    "panel.hover" => panel_hover,
    "panel.active" => panel_active,
    "panel.match" => match_fg,

    // 菜单
    "menu.bg" => menu_bg,
    "menu.fg" => menu_fg,
    "menu.hover" => menu_hover,
    "menu.disabled" => menu_disabled,
    "menu.separator" => menu_separator,

    // 工具栏
    "toolbar.bg" => toolbar_bg,
    "toolbar.border" => toolbar_border,
    "toolbar.field" => toolbar_field,
    "toolbar.button.fg" => toolbar_fg,
    "toolbar.button.disabled" => toolbar_disabled,
    "toolbar.button.hot" => toolbar_hot,
    "toolbar.button.selected" => toolbar_selected,
    "toolbar.button.frame" => toolbar_frame,

    // 文档页签
    "tabstrip.bg" => tab_bg,
    "tab.active_bg" => tab_active_bg,
    "tab.frame" => tab_frame,
    "tab.mark" => tab_mark_fg,
    "tab.hot_band" => tab_hot_band,
    "tab.fg" => tab_fg,
    "tab.active_fg" => tab_active_fg,
    "tab.glyph.fg" => tab_glyph_fg,
    "tab.glyph.hover" => tab_glyph_hover,
    "tab.glyph.pressed" => tab_glyph_pressed,
    "tab.glyph.disabled" => tab_glyph_disabled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 名字唯一。重名会让 `set_token` 命中先出现的那一个，后一个永远写不进去。
    #[test]
    fn no_two_tokens_share_a_name() {
        let mut seen = HashSet::new();
        for name in TOKEN_NAMES {
            assert!(seen.insert(*name), "duplicate token {name}");
        }
    }

    /// 90 个颜色字段，一个都不能漏 —— 漏的那个表现为「皮肤里写了但没反应」。
    #[test]
    fn every_colour_field_has_a_token() {
        assert_eq!(TOKEN_NAMES.len(), 90, "74 scalars + 2 x 8 markers");
    }

    /// 上面那条只数了总数，说不出漏的是哪一个。这条直接比对源码里的字段名：
    /// `Theme` 的每个 `Color` / `[Color; N]` 字段都必须在表里出现一次。
    ///
    /// 读自己的源码当然不算优雅，但它是**唯一**能在没有反射的语言里守住
    /// 「加了个颜色字段就顺手给它起名」的办法；字段总数也钉住了，免得哪天
    /// 结构体换了写法、这条测试静默地扫到 0 个字段还报绿。
    #[test]
    fn every_theme_colour_field_is_named() {
        const THEME: &str = include_str!("theme.rs");
        const TOKENS: &str = include_str!("tokens.rs");

        let mut fields: Vec<&str> = Vec::new();
        for line in THEME.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("pub ") else {
                continue;
            };
            let Some((name, ty)) = rest.split_once(':') else {
                continue;
            };
            let ty = ty.trim().trim_end_matches(',');
            if ty == "Color" || ty.starts_with("[Color;") {
                fields.push(name.trim());
            }
        }
        assert_eq!(fields.len(), 76, "扫到的字段数不对，检查 theme.rs 的写法");

        let mut named: Vec<&str> = Vec::new();
        for line in TOKENS.lines() {
            let Some((_, field)) = line.split_once("=> ") else {
                continue;
            };
            let field = field.trim().trim_end_matches(',');
            let field = field.split('[').next().unwrap_or(field).trim();
            if !field.is_empty() && !named.contains(&field) {
                named.push(field);
            }
        }

        for field in fields {
            assert!(
                named.contains(&field),
                "Theme::{field} 没有 token 名字，皮肤写不到它"
            );
        }
    }

    /// 每个名字都能写进去、读回来，而且只动自己那一个字段。
    #[test]
    fn every_token_sets_and_reads_back() {
        for (i, name) in TOKEN_NAMES.iter().enumerate() {
            let mut theme = Theme::default();
            // 一个不太可能撞上默认值的颜色。
            let probe = Color::rgba((i * 7 % 256) as u8, (i * 13 % 256) as u8, 17, 200);
            assert!(theme.set_token(name, probe), "{name} is not recognised");
            assert_eq!(theme.token(name), Some(probe), "{name} did not stick");
            // 再写一次别的颜色，确认它确实跟着变（不是常量折叠）。
            let other = Color::rgba(1, 2, 3, 4);
            assert!(theme.set_token(name, other));
            assert_eq!(theme.token(name), Some(other), "{name} is stuck");
        }
    }

    /// 不认识的名字什么都不改。
    #[test]
    fn an_unknown_name_changes_nothing() {
        let mut theme = Theme::default();
        let before = theme.token("tab.bg");
        assert!(!theme.set_token("tab.bgg", Color::rgb(255, 0, 0)));
        assert_eq!(theme.token("tab.bgg"), None);
        assert_eq!(theme.token("tab.bg"), before, "the neighbour was touched");
    }

    /// 两个数组字段互不串：写 marker3.bg 不许动 marker3.fg，也不许动 marker4.bg。
    #[test]
    fn the_marker_arrays_do_not_bleed() {
        let mut theme = Theme::default();
        let fg = theme.marker_fg[3];
        let next = theme.marker_bg[4];
        assert!(theme.set_token("editor.marker3.bg", Color::rgb(9, 9, 9)));
        assert_eq!(theme.marker_bg[3], Color::rgb(9, 9, 9));
        assert_eq!(theme.marker_fg[3], fg);
        assert_eq!(theme.marker_bg[4], next);
    }
}
