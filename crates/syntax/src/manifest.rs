//! 插件包清单 `manifest.toml`（parser_plugin_plan.md §2）。
//!
//! 一个包是 `parsers\<name>\` 下的一个目录：`manifest.toml` + `<name>.dll`
//! （+ 可选 `queries\`）。本模块只负责把清单文本解析成 [`PluginManifest`]
//! 并做字段校验；目录名一致性、DLL 存在性由 [`crate::registry`] 的扫描把关。

use std::collections::BTreeMap;

/// 解析后的插件包清单。字段语义见仓库 `docs/parser_plugin_plan.md` §2。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginManifest {
    /// registry 键，全局唯一，`[a-z0-9_-]`，必须与包目录名一致。
    pub name: String,
    /// 状态栏/列表显示名；缺省 = `name`。
    pub display: String,
    /// 包版本，仅作文档。
    pub version: String,
    /// DLL 导出符号；缺省 = `tree_sitter_{name}`（`-` 一律映射为 `_`，
    /// 与 Helix 官方 DLL 的导出约定一致）。
    pub entry: String,
    /// DLL 文件名；缺省 = `{name}.dll`。
    pub dll: String,
    /// 自动映射的文件扩展名（小写、无点）。
    pub extensions: Vec<String>,
    /// 精确文件名匹配（如 `Dockerfile`、`CMakeLists.txt`），大小写
    /// 不敏感；命中的文件即使没有扩展名也会附加本语法。
    pub filenames: Vec<String>,
    /// 首行前缀嗅探（如 shebang）。按包名字典序做 `starts_with` 匹配，
    /// 用于没有扩展名或扩展名未注册的脚本（`#!/usr/bin/env python3`）。
    pub first_lines: Vec<String>,
    /// 声明的兼容 ABI 区间（含端点），仅作文档性快速拒绝；真正的校验在
    /// [`crate::dynlang::load_parser`] 运行时做。
    pub abi: Option<(u32, u32)>,
    /// 超过该字节数的文档不附加本语法（包自己的上限，缺省 = 不设限）。
    /// 判定在 [`crate::registry::LanguageRegistry::fits_file`]。
    pub max_file_bytes: Option<u64>,
}

/// 清单解析/校验失败的可读原因。
type ManifestError = String;

#[derive(serde::Deserialize)]
struct ManifestToml {
    name: String,
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    dll: Option<String>,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    filenames: Vec<String>,
    #[serde(default)]
    first_lines: Vec<String>,
    /// v1 的 node kind 配色表，**已废弃**。留着只为了把「写了但不生效」变成
    /// 一条读得懂的报错（见下面的拒绝分支），不再进 [`PluginManifest`]。
    #[serde(default)]
    theme: Option<BTreeMap<String, i64>>,
    #[serde(default)]
    abi: Option<(i64, i64)>,
    #[serde(default)]
    max_file_bytes: Option<u64>,
}

impl PluginManifest {
    /// 从 TOML 文本解析并校验。
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let raw: ManifestToml =
            toml::from_str(text).map_err(|e| format!("manifest.toml 解析失败: {e}"))?;
        let name = raw.name.trim().to_string();
        if name.is_empty() {
            return Err("name 不能为空".into());
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(format!("name `{name}` 含非法字符（只允许 a-z 0-9 _ -）"));
        }
        let display = raw
            .display
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| name.clone());
        let version = raw.version.unwrap_or_else(|| "0.0.0".into());
        let entry = raw
            .entry
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("tree_sitter_{}", name.replace('-', "_")));
        let dll = raw
            .dll
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("{name}.dll"));
        if dll.contains(['/', '\\']) || dll.contains("..") {
            return Err(format!("dll `{dll}` 必须是包目录内的普通文件名"));
        }
        // 扩展名：小写、去前导点、去空、去重（保持首现顺序）。
        let mut extensions: Vec<String> = Vec::new();
        for ext in raw.extensions {
            let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
            if ext.is_empty() {
                continue;
            }
            if !extensions.contains(&ext) {
                extensions.push(ext);
            }
        }
        // `[theme]` 是 v1 的 node kind 配色表：当初解析并校验了，却从来没有
        // 消费点（配色早已改走 `queries/highlights.scm`，plan §6）。留着一个
        // 「写了但不生效」的字段比没有更糟 —— 直接拒绝，并说清该写哪儿。
        if raw.theme.is_some() {
            return Err(
                "[theme] 已废弃：配色请写 queries/highlights.scm，capture 名对应的颜色在主题文件里"
                    .into(),
            );
        }
        let abi = match raw.abi {
            Some((lo, hi)) if (0..=255).contains(&lo) && (0..=255).contains(&hi) && lo <= hi => {
                Some((lo as u32, hi as u32))
            }
            Some((lo, hi)) => return Err(format!("abi [{lo}, {hi}] 区间非法")),
            None => None,
        };
        Ok(Self {
            name,
            display,
            version,
            entry,
            dll,
            extensions,
            filenames: raw.filenames,
            first_lines: raw.first_lines,
            abi,
            max_file_bytes: raw.max_file_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_manifest() {
        let m = PluginManifest::parse(
            r#"
name = "go"
display = "Go"
version = "0.1.0"
extensions = ["go"]
filenames = ["go.mod"]
first_lines = []
abi = [13, 15]
max_file_bytes = 4194304
"#,
        )
        .expect("parses");
        assert_eq!(m.name, "go");
        assert_eq!(m.display, "Go");
        assert_eq!(m.entry, "tree_sitter_go");
        assert_eq!(m.dll, "go.dll");
        assert_eq!(m.extensions, vec!["go".to_string()]);
        assert_eq!(m.filenames, vec!["go.mod".to_string()]);
        assert_eq!(m.abi, Some((13, 15)));
        assert_eq!(m.max_file_bytes, Some(4194304));
    }

    #[test]
    fn defaults_fill_in_from_name() {
        let m = PluginManifest::parse("name = \"c-sharp\"\n").expect("parses");
        assert_eq!(m.display, "c-sharp");
        assert_eq!(m.entry, "tree_sitter_c_sharp", "- 映射为 _");
        assert_eq!(m.dll, "c-sharp.dll");
        assert!(m.extensions.is_empty());
        assert_eq!(m.abi, None);
    }

    #[test]
    fn extension_normalization_dedupes_and_lowercases() {
        let m = PluginManifest::parse("name = \"x\"\nextensions = [\"GO\", \".go\", \"\", \"Go\"]\n")
            .expect("parses");
        assert_eq!(m.extensions, vec!["go".to_string()]);
    }

    #[test]
    fn rejects_bad_names_and_values() {
        assert!(PluginManifest::parse("").is_err(), "缺 name");
        assert!(PluginManifest::parse("name = \"Go\"\n").is_err(), "大写被拒");
        assert!(PluginManifest::parse("name = \"x\"\ndll = \"../evil.dll\"\n").is_err());
        assert!(PluginManifest::parse("name = \"x\"\nabi = [15, 13]\n").is_err(), "区间倒置");
    }

    /// `[theme]` 已删（配色只走 queries，plan §6）。写了要**报错**而不是静默
    /// 忽略 —— 静默会让用户以为配上了，然后对着没变色的文件找 bug。
    #[test]
    fn the_theme_section_is_gone_and_says_where_to_go() {
        let e = PluginManifest::parse("name = \"x\"\n[theme]\nkeyword = 0x569CD6\n")
            .expect_err("废弃字段必须报错");
        assert!(e.contains("queries/highlights.scm"), "{e}");
        // 空表也算「写了」：`[theme]` 这个段本身就是要拒绝的东西。
        assert!(PluginManifest::parse("name = \"x\"\n[theme]\n").is_err());
    }

    /// 随仓库发布的示例包（`docs/parsers_example/`）必须一直能解析 —
    /// 它们是用户复制粘贴的模板，字段写错了比没写更糟。
    #[test]
    fn shipped_example_manifests_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/parsers_example");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("示例包目录存在") {
            let path = entry.unwrap().path().join("manifest.toml");
            if !path.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let m = PluginManifest::parse(&text)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let dir_name = path.parent().unwrap().file_name().unwrap().to_str().unwrap();
            assert_eq!(m.name, dir_name, "{} 目录名与 name 不一致", path.display());
            seen += 1;
        }
        assert!(seen >= 2, "至少 lua/go 两个示例包");
    }
}
