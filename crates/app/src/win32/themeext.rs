//! 主题的**外部副本**：随包内置的那份是只读的，用户改的是 exe 旁
//! （或工作目录）`themes/<name>.ini` 这一份。
//!
//! 这里只管三件事：文件在哪、指纹是多少、什么时候重读。解析和合成在
//! `dg-render`（`ThemeFile` / `parse_theme_ini`）和 `window.rs`（`compose_theme`）
//! 里，本模块不碰 `Theme`。
//!
//! 为什么单独一个模块而不是塞进 `window.rs`：热重载是「文件 → 状态」的一小段，
//! 和窗口过程没有关系；放这里下一版加「首次编辑自动复制」「另存为」时不用再从
//! 五千行的 `impl` 里翻。

use std::path::{Path, PathBuf};

/// 搜索顺序：exe 旁的 `themes/`，再是工作目录的 `themes/`。
///
/// 和 `EditorState::theme_dirs` 是同一份口径 —— 那里委托到这里，免得「读哪」
/// 和「写哪」各说一套。实现在 `crate::themes::dirs`：诊断（`--ui-doctor`）也要
/// 用它，而诊断不该只在 Windows 上能跑。
pub(super) fn dirs() -> Vec<PathBuf> {
    crate::themes::dirs()
}

/// 这个名字的外部文件（**不管存不存在**）。写新文件时用它，读已有文件时用
/// [`existing`]。
///
/// 落点是第一个能写的目录：exe 旁优先，装到 `Program Files` 里写不进去时退回
/// 工作目录。`probe` 传 `true` 只算路径不落盘（管理器要把「会写到哪」先摆出来）。
pub(super) fn user_path(name: &str, probe: bool) -> Option<PathBuf> {
    let dirs = dirs();
    if probe {
        return dirs.into_iter().next().map(|d| d.join(format!("{name}.ini")));
    }
    for dir in &dirs {
        // 目录不在就先试着建：`themes/` 是用户第一次微调时才出现的，不该要求
        // 他先手动建目录。
        if std::fs::create_dir_all(dir).is_err() {
            continue;
        }
        let path = dir.join(format!("{name}.ini"));
        // 存在就算数（哪怕只读 —— 那是「文件在那儿但改不了」，报错交给写的那一步）。
        if path.exists() {
            return Some(path);
        }
        if can_write(dir) {
            return Some(path);
        }
    }
    None
}

/// 目录能不能写：建一个空文件再删掉。`access()` 在 Windows 上不认 ACL，
/// 这是唯一可靠的办法。
fn can_write(dir: &Path) -> bool {
    let probe = dir.join(".dg-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 已经存在的那个外部文件。
pub(super) fn existing(name: &str) -> Option<PathBuf> {
    dirs()
        .into_iter()
        .map(|d| d.join(format!("{name}.ini")))
        .find(|p| p.is_file())
}

/// 所有可选的主题名：随包内置的，加上 `themes/*.ini` 里多出来的。
///
/// 同名（不分大小写）只留内置那一条 —— 外部文件是**覆盖**，不是第二个主题。
/// 排序后返回：主题对话框的列表、`[Setting] EditTheme` 的取值、切换命令的索引
/// 都读这一份，三处不能各排各的。
pub(super) fn names() -> Vec<String> {
    let mut names: Vec<String> = crate::themes::BUNDLED_THEMES
        .iter()
        .map(|(n, _)| n.to_string())
        .collect();
    for dir in dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ini") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if names.iter().any(|n| n.eq_ignore_ascii_case(stem)) {
                continue;
            }
            names.push(stem.to_string());
        }
    }
    names.sort();
    names
}

/// 一份**完整**的主题文本：内置那份，叠上外部覆盖里的每一条。
///
/// `+`（另存为）要用它。外部副本现在是最小覆盖（只有改过的键），把它原样当成
/// 新主题会得到一个一两个颜色的文件 —— 新名字没有内置底子可叠，其余颜色全退回
/// `Theme::default()`，看着像"复制出来的主题坏了"。
///
/// 自建主题（没有内置原文）外部那份就是全部；两边都没有就 `None`。
pub(super) fn merged_text(name: &str) -> Option<String> {
    let bundled = crate::themes::BUNDLED_THEMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, t)| *t);
    let over = existing(name).and_then(|p| std::fs::read_to_string(p).ok());
    let (base, over) = match (bundled, over) {
        (None, over) => return over,
        (Some(b), None) => return Some(b.to_string()),
        (Some(b), Some(o)) => (b, o),
    };
    let file = dg_render::theme::parse_theme_ini(&over);
    let mut out = base.to_string();
    for (key, c) in &file.colors {
        let v = dg_render::theme::theme_color_ini(c.bg, c.fg);
        if !v.is_empty() {
            out = dg_render::patch_theme_ini(&out, "Theme", key, &v);
        }
    }
    for (key, bits) in &file.styles {
        out = dg_render::patch_theme_ini(&out, "Style", key, &bits.to_string());
    }
    for (key, v) in &file.metrics {
        out = dg_render::patch_theme_ini(&out, "Metrics", key, &format!("{v}"));
    }
    for (key, v) in &file.shapes {
        out = dg_render::patch_theme_ini(&out, "Shape", key, v);
    }
    for (key, v) in &file.fonts {
        out = dg_render::patch_theme_ini(&out, "Fonts", key, v);
    }
    Some(out)
}

/// 改外部副本里的一个键，落盘，返回落点。
///
/// 「改一个颜色」的**唯一**落盘口：主题对话框的色块、以后可能的「保存当前外观」
/// 都走它。
///
/// 没有外部副本时**只写这一行**，不整份复制：`compose_theme` 会把外部那份叠在
/// 内置那份上，所以一份只有 `[Theme]\nCaret=#222` 的文件就是一份完整的主题
/// ——「改一个颜色」不该让用户从此背着一份 45 行的副本，那会把当时的内置默认
/// 值冻在他的文件里，以后随包主题再改默认值他就永远看不见了。
///
/// 只认 UTF-8：读的那一头（`theme_file_for` 的 `read_to_string`）也只认 UTF-8，
/// 写一个读不回来的编码等于把主题改坏。读到非 UTF-8 就报错，让人自己转存，
/// 不猜。
pub(super) fn write_key(
    name: &str,
    section: &str,
    key: &str,
    value: &str,
) -> Result<PathBuf, String> {
    // 有副本就在副本上改，没有就从一张白纸开始 —— `patch_theme_ini` 会把缺的
    // 段和键补出来，所以「只有一行」的文件是合法的主题文件。
    let (path, text) = match existing(name) {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("{}: {e}（主题文件要 UTF-8）", path.display()))?;
            (path, text)
        }
        None => {
            let path =
                user_path(name, false).ok_or_else(|| "找不到可写的 themes 目录".to_string())?;
            (path, String::new())
        }
    };
    let next = dg_render::patch_theme_ini(&text, section, key, value);
    // 一个字节都没变就别碰 mtime：热重载盯着指纹，白写一次会让它重算一遍主题。
    if next != text {
        std::fs::write(&path, next.as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

/// 文件的 `(修改时间 ms, 长度)`。
///
/// 和皮肤热重载用同一个指纹口径（`luascript::stamp`）：两个定时器读同一类文件，
/// 没有理由各写一套。长度进指纹是为了兜住「一秒内改两次、mtime 没变」。
pub(super) fn stamp(path: &Path) -> Option<(u64, u64)> {
    super::luascript::stamp(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 另存为要的是**整份**：外部副本只写了一行时，复制出来的主题也得是完整的
    /// 一份 —— 新名字没有内置底子可叠，少一个颜色就退回默认值。
    #[test]
    fn merged_text_fills_in_the_bundled_defaults() {
        let name = "Fish";
        let path = user_path(name, false).expect("a themes dir");
        std::fs::write(&path, "[Theme]\nCaret=#010203\n").expect("seed");
        let text = merged_text(name).expect("merged");
        let _ = std::fs::remove_file(&path);
        assert!(text.contains("Caret=#010203"), "外部覆盖要生效");
        assert!(text.contains("[Style]"), "别的段也要带上");
        let file = dg_render::theme::parse_theme_ini(&text);
        assert!(file.colors.len() > 10, "整份而不是一行: {} 个键", file.colors.len());
    }

    /// 自建主题（没有内置原文）外部那份就是全部；两边都没有就是 `None`。
    #[test]
    fn merged_text_of_an_external_only_theme_is_that_file() {
        let name = "MergeProbe";
        let path = user_path(name, false).expect("a themes dir");
        std::fs::write(&path, "[Theme]\nCaret=#0A0B0C\n").expect("seed");
        assert_eq!(merged_text(name).as_deref(), Some("[Theme]\nCaret=#0A0B0C\n"));
        let _ = std::fs::remove_file(&path);
        assert!(merged_text(name).is_none(), "两边都没有");
    }

    /// 落点永远是 `.ini`，且带主题名。
    #[test]
    fn the_probe_path_names_the_theme() {
        let path = user_path("Fish", true).expect("a themes dir");
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some("Fish.ini"));
        assert!(path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str())
            == Some("themes"));
    }

    /// 改一个键 = 复制（如果还没外部副本）+ 只改那一行。读回来必须变，别的段
    /// 必须一字不动。
    #[test]
    fn writing_a_key_round_trips_through_the_file() {
        let name = "PatchProbe";
        // 起点：一份只有两段的最小外部文件。
        let path = user_path(name, false).expect("a themes dir");
        std::fs::write(&path, "[Theme]\nCaret=#111111\n[Style]\nPair=257\n").expect("seed");
        let out = write_key(name, "Theme", "Caret", "#222222").expect("written");
        assert_eq!(out, path);
        let text = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(text, "[Theme]\nCaret=#222222\n[Style]\nPair=257\n");
        // 换行不变：`write` 用 `as_bytes`，没有做换行归一。
        assert!(!text.contains("\r\n"));
        let _ = std::fs::remove_file(&path);
    }
}
