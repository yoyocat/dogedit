//! 解析器注册表（registry.json）：「解析器下载」一侧的数据模型。
//!
//! 注册表是一份 JSON：`packages` 数组里每条描述一个可下载的解析器包
//! （名字、扩展名、版本、体积、sha256、下载路径）。本模块只做三件事：
//! 解析并校验文本、按本机 tree-sitter ABI 过滤候选、点分版本号比较。
//! 网络请求、落盘缓存、zip 解包分别住在宿主 crate 与
//! [`crate::install`]，这里保持纯逻辑，单测全部离线。
//!
//! 字段与 [`crate::manifest::PluginManifest`] 对齐：注册表条目是包内
//! manifest 的投影，装完之后扫描器以包目录里的 manifest 为准。

use serde::{Deserialize, Serialize};

/// 当前认得的注册表 schema 号。更高的号意味着字段语义可能变了：
/// 拒绝解析并提示升级，而不是猜。
pub const REGISTRY_SCHEMA: u32 = 1;

/// 注册表里的一个解析器包条目。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotePackage {
    /// 包名，全局唯一，`[a-z0-9_-]` —— 与装好后的目录名一致。
    pub name: String,
    /// 列表/状态栏显示名。
    pub display: String,
    /// 包版本，点分数字（`1.2.0`），比较规则见 [`version_cmp`]。
    pub version: String,
    /// 自动映射的文件扩展名（小写、无点）。
    #[serde(default)]
    pub extensions: Vec<String>,
    /// 精确文件名匹配（如 `Dockerfile`）。
    #[serde(default)]
    pub filenames: Vec<String>,
    /// 首行前缀（shebang）。
    #[serde(default)]
    pub first_lines: Vec<String>,
    /// 声明的 tree-sitter ABI 区间 `[min, max]`；缺省 = 未声明，
    /// 兼容性留给 DLL 加载时的真校验。
    #[serde(default)]
    pub abi: Option<(u32, u32)>,
    /// 包 zip 的字节数，下载进度与预检用。
    pub size: u64,
    /// 包 zip 的 sha256，小写 hex（解析时统一转小写）。
    pub sha256: String,
    /// 包 zip 相对注册表基址的路径（`/` 分隔，不允许 `..`）。
    pub path: String,
    /// 一句话简介（列表里直接展示）。
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
}

/// 解析后的注册表文档。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryDoc {
    pub updated: String,
    pub packages: Vec<RemotePackage>,
}

/// `RegistryDoc` 解析前的原始形状：schema 在这里查，其余字段进
/// [`RemotePackage`] 后逐条校验。
#[derive(Deserialize)]
struct RegistryJson {
    schema: u32,
    #[serde(default)]
    updated: String,
    #[serde(default)]
    packages: Vec<RemotePackage>,
}

impl RegistryDoc {
    /// 从注册表文本解析。校验失败的条目让**整份**文档失败 —— 注册表
    /// 是我们自己的发布物，发布端就该保证每条都干净；客户端对坏条目
    /// 静默降级反而会把错误藏进「列表里少了谁」。
    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: RegistryJson =
            serde_json::from_str(text).map_err(|e| format!("registry.json 解析失败: {e}"))?;
        if raw.schema != REGISTRY_SCHEMA {
            return Err(format!(
                "注册表 schema {} 与本程序认得的 {} 不一致{}",
                raw.schema,
                REGISTRY_SCHEMA,
                if raw.schema > REGISTRY_SCHEMA {
                    "，请升级 DogEdit"
                } else {
                    ""
                }
            ));
        }
        let mut packages: Vec<RemotePackage> = Vec::with_capacity(raw.packages.len());
        for mut p in raw.packages {
            validate(&p)?;
            p.sha256 = p.sha256.to_ascii_lowercase();
            // 匹配端（首开提醒）用小写扩展名查，认领端统一小写。
            p.extensions = p.extensions.iter().map(|e| e.to_ascii_lowercase()).collect();
            if packages.iter().any(|q| q.name == p.name) {
                return Err(format!("注册表里有重复的包名 `{}`", p.name));
            }
            packages.push(p);
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self {
            updated: raw.updated,
            packages,
        })
    }

    /// 扩展名 → 候选包：ABI 与本机兼容的、声明了这个扩展名的包，按包名
    /// 字典序。`host_abi` 用 tree-sitter 的
    /// `MIN_COMPATIBLE_LANGUAGE_VERSION..=LANGUAGE_VERSION`（与
    /// [`crate::dynlang`] 加载时的真校验同一套常量）。
    pub fn candidates_for_ext(&self, ext: &str, host_abi: (u32, u32)) -> Vec<RemotePackage> {
        self.packages
            .iter()
            .filter(|p| p.extensions.iter().any(|e| e == ext))
            .filter(|p| abi_compatible(p.abi, host_abi))
            .cloned()
            .collect()
    }

    /// 按包名取条目（管理框比对已装/远端用）。
    pub fn find(&self, name: &str) -> Option<&RemotePackage> {
        self.packages.iter().find(|p| p.name == name)
    }
}

/// 包声明的 ABI 区间与本机兼容吗：两个区间有交集就行。
/// 未声明（`None`）一律放行 —— 真校验在 DLL 加载时做，这里宁可多展示。
pub fn abi_compatible(decl: Option<(u32, u32)>, host: (u32, u32)) -> bool {
    match decl {
        None => true,
        Some((lo, hi)) => hi >= host.0 && lo <= host.1,
    }
}

/// 点分版本号比较：`Some(Ordering)` = 两边都是数字版本，比得出；
/// `None` = 有一边不是（比不了就不声称有更新）。
///
/// 规则与 dg-script `package.rs` 的 `version_ge` 一致：按 `.` 分段比数字，
/// 缺的段按 0；`1.2.0-beta` 只取数字前缀，后缀忽略。
pub fn version_cmp(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let (pa, pb) = (version_parts(a)?, version_parts(b)?);
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return Some(x.cmp(&y));
        }
    }
    Some(std::cmp::Ordering::Equal)
}

fn version_parts(text: &str) -> Option<Vec<u32>> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for seg in text.split('.') {
        let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        out.push(digits.parse().ok()?);
    }
    Some(out)
}

/// 单条注册表记录的校验。失败原因直接给用户看（状态栏/输出窗格）。
fn validate(p: &RemotePackage) -> Result<(), String> {
    let name = p.name.as_str();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(format!("包名 `{name}` 只允许 a-z 0-9 _ -"));
    }
    if p.sha256.len() != 64 || !p.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("包 `{name}` 的 sha256 不是 64 位 hex"));
    }
    if !safe_rel_path(&p.path) {
        return Err(format!("包 `{name}` 的下载路径 `{}` 不合法", p.path));
    }
    for e in &p.extensions {
        // 真实后缀的字符集：字母数字，外加 `+`（c++/h++）、`#`（f# 一族）
        // 与 `-`/`_`。点、空白、路径字符不进来 —— 扩展名取自最后一个点
        // 之后，本就不可能含点。
        if e.is_empty()
            || e.chars()
                .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '-' | '_')))
        {
            return Err(format!("包 `{name}` 的扩展名 `{e}` 只允许 a-z 0-9 + # - _"));
        }
    }
    Ok(())
}

/// 下载路径只许是包内相对路径：`/` 分隔、无盘符、无 `..`、无反斜杠。
fn safe_rel_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 256 || path.starts_with('/') || path.contains('\\') {
        return false;
    }
    path.split('/').all(|seg| {
        !seg.is_empty()
            && seg != "."
            && seg != ".."
            && !seg.chars().any(|c| c == ':' || c.is_control())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一条注册表记录的 JSON 文本（校验分支直接在这上面做替换）。
    fn pkg(name: &str, exts: &[&str], abi: Option<(u32, u32)>) -> String {
        let abi = abi
            .map(|(lo, hi)| format!(r#""abi": [{lo}, {hi}],"#))
            .unwrap_or_default();
        format!(
            r#"{{"name": "{name}", "display": "{name}", "version": "1.0.0",
                "extensions": [{}], {abi}
                "size": 1000, "sha256": "{}", "path": "pkg/{name}/1.0.0.zip",
                "description": "高亮与折叠"}}"#,
            exts.iter()
                .map(|e| format!(r#""{e}""#))
                .collect::<Vec<_>>()
                .join(", "),
            "a".repeat(64),
        )
    }

    /// 整份注册表文档。
    fn doc_of(items: &[String]) -> String {
        format!(
            r#"{{"schema":1,"updated":"2026-10-31T00:00:00Z","packages":[{}]}}"#,
            items.join(",")
        )
    }

    #[test]
    fn parses_and_sorts_by_name() {
        let doc = RegistryDoc::parse(&doc_of(&[pkg("zeta", &["z"], None), pkg("alpha", &["a"], None)]))
            .unwrap();
        assert_eq!(doc.packages.len(), 2);
        assert_eq!(doc.packages[0].name, "alpha");
        assert_eq!(doc.find("zeta").unwrap().path, "pkg/zeta/1.0.0.zip");
    }

    #[test]
    fn schema_mismatch_is_rejected_with_a_hint() {
        let err = RegistryDoc::parse(&doc_of(&[pkg("x", &["x"], None)]).replace("\"schema\":1", "\"schema\":2"))
            .unwrap_err();
        assert!(err.contains("升级"), "{err}");
        assert!(RegistryDoc::parse(&doc_of(&[pkg("x", &["x"], None)]).replace("\"schema\":1", "\"schema\":0")).is_err());
    }

    #[test]
    fn bad_name_sha_path_are_rejected() {
        assert!(RegistryDoc::parse(&doc_of(&[pkg("Bad-Name", &["x"], None)])).is_err());
        // sha256 太短。
        let text = doc_of(&[pkg("x", &["x"], None)]).replace(&"a".repeat(64), "abc");
        assert!(RegistryDoc::parse(&text).is_err());
        for bad in ["../evil.zip", "/abs/evil.zip", "pkg\\x\\a.zip", "C:/evil.zip", "pkg//a.zip"] {
            let text = doc_of(&[pkg("x", &["x"], None)]).replace("pkg/x/1.0.0.zip", bad);
            assert!(RegistryDoc::parse(&text).is_err(), "{bad} 应被拒绝");
        }
        // 大写 hex 合法，统一转小写。
        let text = doc_of(&[pkg("x", &["x"], None)]).replace(&"a".repeat(64), &"A".repeat(64));
        let doc = RegistryDoc::parse(&text).unwrap();
        assert_eq!(doc.packages[0].sha256, "a".repeat(64));
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let one = pkg("dup", &["x"], None);
        let err = RegistryDoc::parse(&doc_of(&[one.clone(), one])).unwrap_err();
        assert!(err.contains("重复"), "{err}");
    }

    #[test]
    fn signed_suffixes_are_legal_and_normalized_to_lowercase() {
        let doc =
            RegistryDoc::parse(&doc_of(&[pkg("cpp", &["cpp", "c++", "h++"], None)])).unwrap();
        assert_eq!(doc.packages[0].extensions, ["cpp", "c++", "h++"]);
        // 认领端统一小写：匹配端（首开提醒）按小写扩展名查。
        let doc = RegistryDoc::parse(&doc_of(&[pkg("cpp", &["CPP"], None)])).unwrap();
        assert_eq!(doc.packages[0].extensions, ["cpp"]);
        // 点、空白这类进不了扩展名。
        for bad in [&["a.b"][..], &["a b"][..], &["a/b"][..]] {
            assert!(
                RegistryDoc::parse(&doc_of(&[pkg("x", bad, None)])).is_err(),
                "{bad:?} 应被拒绝"
            );
        }
    }

    #[test]
    fn candidates_filter_by_ext_and_abi() {
        let doc = RegistryDoc::parse(&doc_of(&[
            pkg("old", &["x"], Some((10, 11))),
            pkg("new", &["x"], Some((14, 15))),
            pkg("other", &["y"], None),
        ]))
        .unwrap();
        let got = doc.candidates_for_ext("x", (14, 15));
        // ABI 区间 [10,11] 与本机 [14,15] 无交集 → 过滤掉；未声明的 other
        // 不声明 x，也不进候选。
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "new");
    }

    #[test]
    fn abi_compatible_is_interval_overlap() {
        let host = (14, 15);
        assert!(abi_compatible(None, host));
        assert!(abi_compatible(Some((14, 15)), host));
        assert!(abi_compatible(Some((10, 20)), host));
        assert!(!abi_compatible(Some((10, 13)), host));
        assert!(!abi_compatible(Some((16, 20)), host));
    }

    #[test]
    fn versions_compare_by_segments() {
        use std::cmp::Ordering::*;
        assert_eq!(version_cmp("1.2", "1.10"), Some(Less));
        assert_eq!(version_cmp("1.2.0", "1.2"), Some(Equal));
        assert_eq!(version_cmp("1.3.0-beta", "1.2.9"), Some(Greater));
        assert_eq!(version_cmp("abc", "1.0"), None);
        assert_eq!(version_cmp("", "1.0"), None);
    }

    #[test]
    fn rel_paths_are_checked_component_wise() {
        assert!(safe_rel_path("pkg/rust/1.0.0.zip"));
        assert!(safe_rel_path("a/b.zip"));
        assert!(!safe_rel_path(""));
        assert!(!safe_rel_path("/pkg/a.zip"));
        assert!(!safe_rel_path("pkg/../a.zip"));
        assert!(!safe_rel_path("pkg/./a.zip"));
        assert!(!safe_rel_path("pkg//a.zip"));
        assert!(!safe_rel_path("pkg/C:a.zip"));
    }
}
