//! Bundled `theme/*.ini` files, compiled into the binary. External themes in
//! `themes/` next to the executable (or the working directory) win over the
//! bundled copies.

/// (name, ini source) pairs.
pub const BUNDLED_THEMES: &[(&str, &str)] = &[
    ("Acid", include_str!("../../../themes/Acid.ini")),
    ("Andes", include_str!("../../../themes/Andes.ini")),
    ("Black", include_str!("../../../themes/Black.ini")),
    ("Blue", include_str!("../../../themes/Blue.ini")),
    ("Blue Green", include_str!("../../../themes/Blue Green.ini")),
    ("Cobalt", include_str!("../../../themes/Cobalt.ini")),
    ("Dark Blue", include_str!("../../../themes/Dark Blue.ini")),
    ("Darkness", include_str!("../../../themes/Darkness.ini")),
    ("Desert", include_str!("../../../themes/Desert.ini")),
    ("Eclipse", include_str!("../../../themes/Eclipse.ini")),
    (
        "Espresso Libre",
        include_str!("../../../themes/Espresso Libre.ini"),
    ),
    ("Evening", include_str!("../../../themes/Evening.ini")),
    ("DogEdit", include_str!("../../../themes/DogEdit.ini")),
    (
        "DogEdit_Dark",
        include_str!("../../../themes/DogEdit_Dark.ini"),
    ),
    ("Fish", include_str!("../../../themes/Fish.ini")),
    ("Markdown", include_str!("../../../themes/Markdown.ini")),
    (
        "Markdown_Dark",
        include_str!("../../../themes/Markdown_Dark.ini"),
    ),
    ("Monokai", include_str!("../../../themes/Monokai.ini")),
    ("Neon", include_str!("../../../themes/Neon.ini")),
    ("Obsidian", include_str!("../../../themes/Obsidian.ini")),
    (
        "PlasticCodeWrap",
        include_str!("../../../themes/PlasticCodeWrap.ini"),
    ),
    ("Slate", include_str!("../../../themes/Slate.ini")),
    (
        "Slate Forever",
        include_str!("../../../themes/Slate Forever.ini"),
    ),
    (
        "Solarized (Dark)",
        include_str!("../../../themes/Solarized (Dark).ini"),
    ),
    (
        "Solarized (Light)",
        include_str!("../../../themes/Solarized (Light).ini"),
    ),
    ("Tabula", include_str!("../../../themes/Tabula.ini")),
    ("mildBlue", include_str!("../../../themes/mildBlue.ini")),
];

/// The theme `setting.ini` names by
/// default (`EditTheme`).
pub const DEFAULT_THEME: &str = "DogEdit_Dark";

/// 浅色模式那一套（`[Setting] LightEditTheme` 没写时用它）：`DEFAULT_THEME`
/// 的浅色同名兄弟，随包的 `themes/DogEdit.ini`。
pub const LIGHT_THEME: &str = "DogEdit";

/// 磁盘上的主题目录：exe 旁的 `themes/`，再是工作目录的 `themes/`。
///
/// 这条顺序就是「读哪」和「写哪」共用的那一条 —— 放在这里（而不是
/// `win32::themeext`）是因为 `--ui-doctor` 也要用它，而诊断不该只在 Windows
/// 上能跑。`themeext` 的写入路径原样调这里。
pub fn dirs() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("themes"));
        }
    }
    out.push(std::path::PathBuf::from("themes"));
    out
}

/// 内置里有没有这个名字（不分大小写），有就返回它的正式写法。
///
/// `setting.ini` 是手写的，`fish` / `Fish` 都得认；返回正式写法是因为菜单、
/// 主题对话框、`[Setting] EditTheme` 三处存的是同一个名字。
pub fn bundled(name: &str) -> Option<&'static str> {
    BUNDLED_THEMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(n, _)| *n)
}

/// 安全模式该用哪一份：内置里有的就是它，没有就退回默认那套。
///
/// 退回而不是「叠在一张白纸上」：白纸是**全零**的一份 `ThemeFile`，画出来是
/// 黑底黑字 —— 用户看到一个读不了的窗口，正是不安全的那一头。安全模式认的是
/// 「一定看得见」，所以只认内置，名字不认识就给默认。
pub fn bundled_or_default(name: &str) -> &'static str {
    bundled(name).unwrap_or(DEFAULT_THEME)
}

/// 一份**完整**的主题：内置那份叠上外部覆盖。
///
/// 名字不分大小写（`setting.ini` 是手写的）；外部文件只写一行也算数 —— 叠完
/// 就是一份完整主题，所以「改一个颜色」不需要先复制整本。内置里没有这个名字
/// （用户从零写的主题）就叠在一张白纸上，它自成一份。
pub fn compose(
    name: &str,
    external: Option<&str>,
) -> Option<(String, dg_render::theme::ThemeFile)> {
    use dg_render::theme::{parse_theme_ini, ThemeFile};
    let entry = BUNDLED_THEMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name));
    let mut file = match entry {
        Some((_, text)) => parse_theme_ini(text),
        None => ThemeFile::default(),
    };
    let mut canonical = entry.map(|(n, _)| (*n).to_string());
    if let Some(text) = external {
        file.merge(&parse_theme_ini(text));
        canonical.get_or_insert_with(|| name.to_string());
    }
    canonical.map(|c| (c, file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundled_name_comes_back_in_its_own_spelling() {
        assert_eq!(bundled("fish"), Some("Fish"));
        assert_eq!(bundled("FISH"), Some("Fish"));
        assert_eq!(bundled("No Such Theme"), None);
    }

    /// 随包 27 份主题的 `SelIndicator` 全是灰，跟它们自己的 `Caret` 只差深浅 —— 多
    /// 光标标记因此读起来像"第二根光标"。保底必须把每一份都换成带色相的标记色，
    /// 同时把主题自己挑的明度留住（不然一张白底上就是块刺眼的亮橙）。
    #[test]
    fn every_bundled_theme_marks_extra_carets_apart_from_its_caret() {
        use dg_render::theme::{parse_theme_ini, Color, Theme};
        let chroma = |c: Color| -> u32 {
            let m = c.r.max(c.g).max(c.b) as u32;
            let n = c.r.min(c.g).min(c.b) as u32;
            m - n
        };
        let luma = |c: Color| 3 * c.r as u32 + 6 * c.g as u32 + c.b as u32;
        let mut replaced = 0usize;
        assert_eq!(BUNDLED_THEMES.len(), 27, "随包主题的条数变了");
        for (name, ini) in BUNDLED_THEMES {
            let mut theme = Theme::default();
            theme.apply_theme_file(&parse_theme_ini(ini));
            let mark = theme.caret_indicator();
            assert_ne!(mark, theme.caret, "{name}: 标记穿的就是光标色");
            assert!(
                chroma(mark) >= 40,
                "{name}: 标记 {} 没有色相，和灰光标分不开",
                mark.to_u32()
            );
            if mark != theme.sel_indicator {
                replaced += 1;
                let (want, got) = (luma(theme.sel_indicator), luma(mark));
                assert!(
                    want.abs_diff(got) <= want / 4,
                    "{name}: 标记亮度 {got} 离开了主题自己的 {want}"
                );
            }
        }
        assert_eq!(replaced, 27, "每一份随包主题的灰指示色都该走保底");
    }

    /// 安全模式只认内置：用户自建的主题名（只有 `themes/Mine.ini`）在这里不
    /// 存在，退回默认那套 —— 叠在一张白纸上就是黑底黑字，正是不安全的那一头。
    #[test]
    fn a_name_nothing_bundles_falls_back_to_the_default() {
        assert_eq!(bundled_or_default("Fish"), "Fish");
        assert_eq!(bundled_or_default("Mine"), DEFAULT_THEME);
        // 退回去的那套自己必须在内置里，否则「退回默认」也是白纸。
        assert!(bundled(DEFAULT_THEME).is_some(), "{DEFAULT_THEME} 不在内置里");
    }
}
