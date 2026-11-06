use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tree_sitter::Language;

use crate::dynlang;
use crate::manifest::PluginManifest;

/// 扩展名兜底映射 —— 收编自 dg-app 打开文件路径上散落的
/// `match ext { "rs" => "rust",... }` 硬编码。
///
/// **表里只有名字，没有 grammar**：命中的名字要装了同名插件包（或嵌入方
/// `register` 过）才解析得到。作用是在包没写 `manifest.extensions` 时
/// 仍能按常识接上；插件包 `manifest.extensions` 与 setting.ini 手工映射
/// 都优先于这张表。
pub fn default_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "cpp" | "cc" | "cxx" => Some("cpp"),
        "js" | "mjs" => Some("javascript"),
        "py" => Some("python"),
        "json" => Some("json"),
        "htm" | "html" => Some("html"),
        _ => None,
    }
}

/// 一个外部解析器包：manifest + 磁盘位置，DLL 在首次 [`Self::get`] 时
/// 才加载（§3.2 惰性）。加载失败缓存为 `None`，不再重试。
struct PluginEntry {
    manifest: PluginManifest,
    /// DLL 的绝对路径。
    dll_path: PathBuf,
    /// 包目录（`queries\` 的父目录）。
    dir: PathBuf,
    /// 用户在「语法着色」里取消勾选 = false：包还在、DLL 已加载也不动，
    /// 只是不再被解析到（`[Syntax] Disabled` 持久化）。
    enabled: bool,
    /// `None` = 已尝试且失败（诊断由扫描方上报）。
    lang: OnceCell<Option<Language>>,
}

/// 插件包的加载状态（UI 的「状态」列）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginState {
    /// 还没用到，DLL 未加载（惰性，§3.2）。
    Unloaded,
    /// DLL 已加载，能解析。
    Loaded,
    /// 加载失败（诊断在扫描报告里）。
    Failed,
}

/// 扫描/加载诊断，可直接写输出窗口。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDiag {
    /// 出问题的包（目录名或 manifest.name）。
    pub package: String,
    /// 可读原因。
    pub message: String,
}

/// 插件列表行（UI/诊断用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInfo {
    pub name: String,
    pub display: String,
    pub dll: PathBuf,
    /// 包版本（`manifest.version`，「解析器下载」比对新版用）。
    pub version: String,
    /// `manifest.extensions`（原样，小写）。
    pub extensions: Vec<String>,
    /// 包内 `queries\*.scm` 的文件名（不含扩展名），已排序。
    pub queries: Vec<String>,
    /// `[Syntax] Disabled` 的反面。
    pub enabled: bool,
    /// DLL 加载状态。
    pub state: PluginState,
}

/// Maps language names to tree-sitter grammars.
///
/// 两个来源（parser_plugin_plan.md §4）：
/// - **插件包**：`parsers\` 下的 `manifest.toml` + DLL（[`Self::scan_plugins`]），
/// 惰性加载 —— 发行版唯一的 grammar 来源，二进制里不打包任何语法；
/// - **编译期注册**：[`Self::register`]，供嵌入方/测试直接塞一个 `Language`。
///
/// 同名冲突时插件包覆盖编译期注册项（用户可以换新版本 grammar 而不必等发行版）。
#[derive(Default)]
pub struct LanguageRegistry {
    langs: HashMap<String, Language>,
    /// BTreeMap：`resolve_ext` 的命中顺序需要确定。
    plugins: std::collections::BTreeMap<String, PluginEntry>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个编译期 grammar（嵌入方/测试用；发行版不再内置）。
    pub fn register(&mut self, name: &str, lang: Language) {
        self.langs.insert(name.to_string(), lang);
    }

    /// 扫描一个插件根目录：每个子目录必须是一份完整插件包
    /// （`manifest.toml` + DLL）。返回本次扫描的诊断清单；
    /// 重名包保留先扫描到的，其余记诊断忽略。
    pub fn scan_plugins(&mut self, root: &Path) -> Vec<PluginDiag> {
        let mut diags = Vec::new();
        let Ok(entries) = std::fs::read_dir(root) else {
            // 目录不存在 = 没装插件，不是错误。
            return diags;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let dir_name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let manifest_path = dir.join("manifest.toml");
            let Ok(text) = std::fs::read_to_string(&manifest_path) else {
                diags.push(PluginDiag {
                    package: dir_name.clone(),
                    message: "缺少 manifest.toml".into(),
                });
                continue;
            };
            let manifest = match PluginManifest::parse(&text) {
                Ok(m) => m,
                Err(e) => {
                    diags.push(PluginDiag {
                        package: dir_name.clone(),
                        message: e,
                    });
                    continue;
                }
            };
            if manifest.name != dir_name {
                diags.push(PluginDiag {
                    package: dir_name.clone(),
                    message: format!("包目录名与 manifest.name `{}` 不一致", manifest.name),
                });
                continue;
            }
            let dll_path = dir.join(&manifest.dll);
            if !dll_path.exists() {
                diags.push(PluginDiag {
                    package: manifest.name.clone(),
                    message: format!("DLL 不存在: {}", manifest.dll),
                });
                continue;
            }
            if let Some(existing) = self.plugins.get(&manifest.name) {
                // 同一个包被重扫（「重新扫描解析器」）是常态：静默跳过。
                // 重扫只增不改 —— 已加载的 DLL 永不卸载（§3.2），换 DLL 要重启。
                if existing.dir == dir {
                    continue;
                }
                diags.push(PluginDiag {
                    package: manifest.name.clone(),
                    message: format!(
                        "重名插件包：已由 {} 注册，保留先扫描到的",
                        existing.dir.display()
                    ),
                });
                continue;
            }
            self.plugins.insert(
                manifest.name.clone(),
                PluginEntry {
                    manifest,
                    dll_path,
                    dir: dir.clone(),
                    enabled: true,
                    lang: OnceCell::new(),
                },
            );
        }
        diags
    }

    /// 按语法名取语言：插件包优先（同名覆盖 [`Self::register`] 注册的
    /// 项），DLL 惰性加载，失败返回 `None`。被禁用的包一律 `None`。
    pub fn get(&self, name: &str) -> Option<Language> {
        if let Some(entry) = self.plugins.get(name) {
            if !entry.enabled {
                return None;
            }
            let dll_path = entry.dll_path.clone();
            let entry_name = entry.manifest.entry.clone();
            return entry
                .lang
                .get_or_init(|| unsafe { dynlang::load_parser(&dll_path, &entry_name).ok() })
                .clone();
        }
        self.langs.get(name).cloned()
    }

    /// 这个语法愿不愿意接手 `bytes` 字节的文档：`manifest.max_file_bytes`
    /// 是**包自己的**上限，缺省（以及 `register` 注册的内置项）不设限。
    ///
    /// 边界取**闭区间**（`bytes <= max` 才附加） —— 用户写 `4194304` 想表达的
    /// 是"4 MB 以内没问题"。被禁用的包一律 `false`：反正 [`Self::get`] 也
    /// 拿不到语言。**不触发 DLL 加载**（只读 manifest），所以可以在 attach
    /// 之前放心调用。
    pub fn fits_file(&self, name: &str, bytes: u64) -> bool {
        match self.plugins.get(name) {
            Some(entry) if !entry.enabled => false,
            _ => self.max_file_bytes(name).is_none_or(|max| bytes <= max),
        }
    }

    /// 包声明的体积上限（字节）。`None` = 不设限，或这个名字根本没有 manifest
    /// （`register` 注册的内置项）。被禁用的包也当 `None` —— 它什么都不接手，
    /// 报个上限反而误导。**不触发 DLL 加载**。
    pub fn max_file_bytes(&self, name: &str) -> Option<u64> {
        self.plugins
            .get(name)
            .filter(|e| e.enabled)
            .and_then(|e| e.manifest.max_file_bytes)
    }

    /// 扩展名 → grammar 名：插件包 `manifest.extensions` 优先（按包名
    /// 字典序，保证确定性），其次 [`default_extension`] 兜底表。
    pub fn resolve_ext(&self, ext: &str) -> Option<String> {
        for (name, entry) in &self.plugins {
            if entry.enabled && entry.manifest.extensions.iter().any(|e| e == ext) {
                return Some(name.clone());
            }
        }
        default_extension(ext).map(|s| s.to_string())
    }

    /// 首行 → grammar 名：`manifest.first_lines` 的**前缀**匹配，包名字典序
    /// 先到先得（和 [`Self::resolve_ext`] 同一条规则）。给没有扩展名的脚本用：
    /// `#!/usr/bin/env python3` 这类文件，路径上一点线索都没有。
    ///
    /// 大小写敏感 —— shebang 本来就是；首行的 BOM 与行尾 `\r` 由调用方清掉。
    /// 空条目一律跳过（`first_lines = [""]` 不能匹配所有文件）。
    pub fn resolve_first_line(&self, line: &str) -> Option<String> {
        let line = line.trim_end();
        if line.is_empty() {
            return None;
        }
        for (name, entry) in &self.plugins {
            if entry.enabled
                && entry
                    .manifest
                    .first_lines
                    .iter()
                    .any(|p| !p.is_empty() && line.starts_with(p.as_str()))
            {
                return Some(name.clone());
            }
        }
        None
    }

    /// 文件名 → grammar 名：`manifest.filenames` 的**大小写不敏感**精确
    /// 匹配（`Dockerfile` / `Makefile` / `CMakeLists.txt` 这类没有可用
    /// 扩展名的文件）。插件包优先，`register` 注册的项没有文件名表。
    pub fn resolve_filename(&self, file: &str) -> Option<String> {
        for (name, entry) in &self.plugins {
            if entry.enabled
                && entry
                    .manifest
                    .filenames
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(file))
            {
                return Some(name.clone());
            }
        }
        None
    }

    /// 这个名字是否有 grammar（插件包或 `register` 注册的）。**不触发
    /// DLL 加载** —— 给「扩展名解析失败后把键当 grammar 名再试一次」用。
    pub fn has(&self, name: &str) -> bool {
        self.plugins.get(name).is_some_and(|e| e.enabled) || self.langs.contains_key(name)
    }

    /// 状态栏 / 「切换文件类型」要的显示名：`key` 既可能是 grammar 名
    /// （`rust`），也可能是扩展名（`rs`） —— `syntax_lang` 两种都存过。
    ///
    /// 认的是插件包的 `manifest.display`，也就是对话框列表里印的那个名字，
    /// 这样状态栏和列表永远一致。认不出来返回 `None`，由调用方退回内置
    /// 兜底表。**不触发 DLL 加载**：只读 manifest。
    pub fn display_name(&self, key: &str) -> Option<String> {
        let name = if self.plugins.contains_key(key) {
            key.to_string()
        } else {
            self.resolve_ext(key)?
        };
        self.plugins
            .get(&name)
            .filter(|e| e.enabled)
            .map(|e| e.manifest.display.clone())
    }

    /// 外部插件清单（UI/诊断列表用）。`queries` 每次现列目录 —
    /// 用户改完包内容不用重扫就能在报告里看到。
    pub fn plugin_list(&self) -> Vec<PluginInfo> {
        self.plugins
            .iter()
            .map(|(name, e)| PluginInfo {
                name: name.clone(),
                display: e.manifest.display.clone(),
                dll: e.dll_path.clone(),
                version: e.manifest.version.clone(),
                extensions: e.manifest.extensions.clone(),
                queries: Self::list_queries(&e.dir),
                enabled: e.enabled,
                // `OnceCell::get` 不触发加载：这里只报告当前状态。
                state: match e.lang.get() {
                    None => PluginState::Unloaded,
                    Some(Some(_)) => PluginState::Loaded,
                    Some(None) => PluginState::Failed,
                },
            })
            .collect()
    }

    /// 启用/禁用插件包，返回包里有没有这个名字。禁用**不卸载** DLL
    /// （§3.2 永不 FreeLibrary），只是不再被解析到；已打开的文档保留
    /// 自己的高亮器，新打开的文件才受影响。
    pub fn set_enabled(&mut self, name: &str, on: bool) -> bool {
        match self.plugins.get_mut(name) {
            Some(e) => {
                e.enabled = on;
                true
            }
            None => false,
        }
    }

    /// 把一个包从解析表里摘掉（「解析器下载」卸载用）：磁盘上的目录已经
    /// 删掉/改名，表里不留尸体。已加载进内存的 DLL 不卸载（§3.2 永不
    /// `FreeLibrary`），摘除只影响后续解析。返回表里有没有这个名字。
    pub fn remove(&mut self, name: &str) -> bool {
        self.plugins.remove(name).is_some()
    }

    /// 按禁用名单重设（启动时读 `[Syntax] Disabled`）：名单里的包禁用，
    /// 其余一律启用。
    pub fn apply_disabled(&mut self, disabled: &[String]) {
        for (name, e) in &mut self.plugins {
            e.enabled = !disabled.iter().any(|d| d == name);
        }
    }

    /// 当前被禁用的包名（写回 `[Syntax] Disabled` 用，字典序）。
    pub fn disabled_names(&self) -> Vec<String> {
        self.plugins
            .iter()
            .filter(|(_, e)| !e.enabled)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// `dir\queries\*.scm` 的文件名（去扩展名、排序）。
    fn list_queries(dir: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(dir.join("queries")) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("scm")))
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
            .collect();
        out.sort();
        out
    }

    /// 取插件包里的一个 query 文件（`"highlights"` / `"folds"` …），
    /// 对应 `queries\<file>.scm`。
    ///
    /// 一个包的 query 文本，按 `; inherits: a,b` 把父语言的同名 query
    /// 接在前面（Helix 运行时的分层约定）。父包没装就跳过那一份 ——
    /// 语法包按语言单独打包，不是每个父目录都有包。
    ///
    /// `Ok(None)` = 包里没有这个文件 —— 正常情况，调用方回落到 node
    /// kind 配色；`Err(_)` = 文件在但读不出来，值得报给用户。
    /// `register` 注册的 grammar 没有包目录，一律 `Ok(None)`。
    pub fn query_text(&self, name: &str, file: &str) -> Result<Option<String>, String> {
        let mut seen = Vec::new();
        self.query_text_inheriting(name, file, &mut seen)
    }

    /// [`Self::query_text`] 的递归体。`seen` 挡住 `a inherits b, b inherits a`
    /// 这类环 —— 真出现也只会少拼一份文本，不会转不出来。
    fn query_text_inheriting(
        &self,
        name: &str,
        file: &str,
        seen: &mut Vec<String>,
    ) -> Result<Option<String>, String> {
        if seen.iter().any(|n| n == name) {
            return Ok(None);
        }
        seen.push(name.to_string());
        let Some(entry) = self.plugins.get(name) else {
            return Ok(None);
        };
        let path = entry.dir.join("queries").join(format!("{file}.scm"));
        let own = match std::fs::read(&path) {
            Ok(bytes) => {
                String::from_utf8(bytes).map_err(|_| format!("{} 不是 UTF-8", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("读取 {} 失败: {e}", path.display())),
        };
        let mut out = String::new();
        for parent in query_inherits(&own) {
            let Some(entry) = self.plugins.get(&parent) else {
                continue;
            };
            if !entry.enabled {
                continue;
            }
            if let Some(text) = self.query_text_inheriting(&parent, file, seen)? {
                out.push_str(&text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
        out.push_str(&own);
        Ok(Some(out))
    }

    /// query 文件路径（不存在也返回，供诊断信息展示）。
    pub fn query_path(&self, name: &str, file: &str) -> Option<PathBuf> {
        self.plugins
            .get(name)
            .map(|e| e.dir.join("queries").join(format!("{file}.scm")))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.langs.keys().map(|s| s.as_str())
    }
}

/// 文件头 `; inherits: a,b` 的名单。约定写在 query 顶部的注释里，所以只看
/// 开头这一段：空行与注释跳过，遇到第一条规则就停。
///
/// 公开是给安装侧用的：装一个包时按同一份名单把父包也装上。
pub fn query_inherits(scm: &str) -> Vec<String> {
    for line in scm.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Some(comment) = t.strip_prefix(';') else {
            break;
        };
        if let Some(list) = comment.trim().strip_prefix("inherits:") {
            return list
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dll() -> Option<PathBuf> {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        for profile in ["debug", "release"] {
            let p = manifest
                .join("../../target")
                .join(profile)
                .join("fixture_json_dll.dll");
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    /// 发行版不带任何 grammar：没有 `parsers\` 时注册表是空的，
    /// 但扩展名兜底表仍把键映射到包名（装了包就能接上）。
    #[test]
    fn default_registry_ships_no_grammars() {
        let reg = LanguageRegistry::new();
        assert!(reg.get("json").is_none());
        assert!(reg.get("rust").is_none());
        assert_eq!(reg.resolve_ext("rs").as_deref(), Some("rust"));
        assert_eq!(reg.resolve_ext("json").as_deref(), Some("json"));
        // 没有包就没有 display：状态栏退回内置表，而不是编一个名字出来。
        assert_eq!(reg.display_name("rust"), None);
        assert_eq!(reg.display_name("rs"), None);
    }

    /// 状态栏/对话框共用的名字：grammar 名和扩展名两条路都要认，取的
    /// 都是包自己的 `manifest.display`。
    #[test]
    fn display_name_follows_the_package() {
        let Some(dll) = fixture_dll() else {
            eprintln!("skip: fixture dll 未构建");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "dg-syntax-display-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // 包名 `foo`、扩展名 `f`：两条路的答案必须一样，否则状态栏和
        // 「切换文件类型」会印出两个名字。
        let foo = root.join("foo");
        std::fs::create_dir_all(&foo).unwrap();
        std::fs::write(
            foo.join("manifest.toml"),
            "name = \"foo\"\ndisplay = \"Foo Lang\"\nextensions = [\"f\"]\nentry = \"tree_sitter_fixture_json\"\n",
        )
        .unwrap();
        std::fs::copy(&dll, foo.join("foo.dll")).unwrap();

        let mut reg = LanguageRegistry::new();
        reg.scan_plugins(&root);
        assert_eq!(reg.display_name("foo").as_deref(), Some("Foo Lang"), "grammar 名");
        assert_eq!(reg.display_name("f").as_deref(), Some("Foo Lang"), "扩展名");
        assert_eq!(reg.display_name("nope"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `; inherits: base`：父包的同名 query 排在前面，自己的接在后面
    /// （高亮「后出现覆盖先出现」，所以子语言仍是最后的赢家）；父包没装
    /// 就只剩自己的那份。
    #[test]
    fn query_text_pulls_in_the_inherited_query() {
        let Some(dll) = fixture_dll() else {
            eprintln!("skip: fixture dll 未构建");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "dg-syntax-inherit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        for (name, text) in [
            ("base", "(string) @string\n"),
            ("leaf", "; inherits: base\n(comment) @comment\n"),
        ] {
            let dir = root.join(name);
            std::fs::create_dir_all(dir.join("queries")).unwrap();
            std::fs::write(
                dir.join("manifest.toml"),
                format!(
                    "name = \"{name}\"\nextensions = [\"{name}\"]\nentry = \"tree_sitter_fixture_json\"\n"
                ),
            )
            .unwrap();
            std::fs::copy(&dll, dir.join(format!("{name}.dll"))).unwrap();
            std::fs::write(dir.join("queries/highlights.scm"), text).unwrap();
        }

        let mut reg = LanguageRegistry::new();
        assert!(reg.scan_plugins(&root).is_empty());
        assert_eq!(
            reg.query_text("leaf", "highlights").unwrap().as_deref(),
            Some("(string) @string\n; inherits: base\n(comment) @comment\n")
        );
        // 父包没有同名 query：只剩自己的。
        assert_eq!(reg.query_text("leaf", "folds").unwrap(), None);
        // 父包没装：静默降级，自己的照常返回。
        reg.remove("base");
        assert_eq!(
            reg.query_text("leaf", "highlights").unwrap().as_deref(),
            Some("; inherits: base\n(comment) @comment\n")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn default_extension_table_covers_the_old_match_arms() {
        assert_eq!(default_extension("rs"), Some("rust"));
        assert_eq!(default_extension("cc"), Some("cpp"));
        assert_eq!(default_extension("cxx"), Some("cpp"));
        assert_eq!(default_extension("mjs"), Some("javascript"));
        assert_eq!(default_extension("py"), Some("python"));
        assert_eq!(default_extension("json"), Some("json"));
        assert_eq!(default_extension("htm"), Some("html"));
        assert_eq!(default_extension("txt"), None);
    }

    /// 扫描一个临时目录：好包加载成功、缺 DLL/坏 manifest/名字不一致的
    /// 各自出诊断、外部包覆盖 `register` 的同名项。
    #[test]
    fn scan_plugins_registers_and_diagnoses() {
        let Some(dll) = fixture_dll() else {
            eprintln!("skip: fixture dll 未构建");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "dg-syntax-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // go：manifest + fixture DLL（借用 json 语法，包名与目录一致即可）。
        let go = root.join("go");
        std::fs::create_dir_all(&go).unwrap();
        std::fs::write(
            go.join("manifest.toml"),
            // DLL 是 fixture 的转发壳，导出名 tree_sitter_fixture_json，
            // 由 entry 指名 —— 顺带覆盖"entry 缺省名与实际导出不同"的路径。
            "name = \"go\"\ndisplay = \"Go\"\nextensions = [\"go\"]\nentry = \"tree_sitter_fixture_json\"\n",
        )
        .unwrap();
        std::fs::copy(&dll, go.join("go.dll")).unwrap();
        // go 包自带 query：`queries\highlights.scm` 随包发布。
        std::fs::create_dir_all(go.join("queries")).unwrap();
        std::fs::write(
            go.join("queries/highlights.scm"),
            "[(true)] @constant.builtin\n",
        )
        .unwrap();
        // json：带 filenames 的包（DLL 用 fixture 的导出名 tree_sitter_fixture_json）。
        let js = root.join("json");
        std::fs::create_dir_all(&js).unwrap();
        std::fs::write(
            js.join("manifest.toml"),
            "name = \"json\"\ndisplay = \"JSON(外部)\"\nextensions = [\"json\"]\nentry = \"tree_sitter_fixture_json\"\nfilenames = [\"Dockerfile\"]\n",
        )
        .unwrap();
        std::fs::copy(&dll, js.join("json.dll")).unwrap();
        // 坏包 1：DLL 不存在。
        let bad = root.join("broken");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(
            bad.join("manifest.toml"),
            "name = \"broken\"\nextensions = [\"brk\"]\n",
        )
        .unwrap();
        // 坏包 2：manifest 非法。
        let illegal = root.join("illegal");
        std::fs::create_dir_all(&illegal).unwrap();
        std::fs::write(illegal.join("manifest.toml"), "display = \"没有 name\"\n").unwrap();
        // 坏包 3：目录名与 name 不一致。
        let mismatch = root.join("notgo");
        std::fs::create_dir_all(&mismatch).unwrap();
        std::fs::write(mismatch.join("manifest.toml"), "name = \"go\"\n").unwrap();

        // 扫描前先塞一个同名项（rust grammar 冒充 go）：外部包必须盖掉它。
        let mut reg = LanguageRegistry::new();
        reg.register(
            "go",
            tree_sitter::Language::from(tree_sitter_rust::LANGUAGE),
        );
        let diags = reg.scan_plugins(&root);
        let packages: Vec<&str> = diags.iter().map(|d| d.package.as_str()).collect();
        assert_eq!(packages, vec!["broken", "illegal", "notgo"], "3 个坏包各出一条诊断");

        // 外部包按 manifest 扩展名解析；坏包不参与。
        assert_eq!(reg.resolve_ext("go").as_deref(), Some("go"));
        assert_eq!(reg.resolve_ext("brk"), None);
        // 文件名匹配（大小写不敏感），且与扩展名互不干扰。
        assert_eq!(reg.resolve_filename("Dockerfile").as_deref(), Some("json"));
        assert_eq!(reg.resolve_filename("dockerfile").as_deref(), Some("json"));
        assert_eq!(reg.resolve_filename("Dockerfile.dev"), None);
        // has() 不加载 DLL 也能答（扩展名键答 false；没装包的名字答 false）。
        assert!(reg.has("go"));
        assert!(!reg.has("rust"), "发行版不内置 grammar");
        assert!(!reg.has("rs"), "扩展名不是 grammar 名");
        let infos = reg.plugin_list();
        let names: Vec<&str> = infos.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["go", "json"]);
        assert_eq!(infos[1].display, "JSON(外部)");

        // get("go")：真·LoadLibrary 链路，加载的语言能解析；根节点是
        // json 的 `document` 而不是 register 那个 rust 的 `source_file`。
        let lang = reg.get("go").expect("go 包加载成功");
        let mut h = crate::Highlighter::new(lang);
        h.reset("{\"a\": 1}");
        assert!(h.kind_at(2).is_some());
        assert_eq!(
            h.tree().expect("tree").root_node().kind(),
            "document",
            "外部包覆盖 register() 注册的同名项"
        );

        // query 文件：有则取到、无则 Ok(None)（回落 kind 配色）。
        assert_eq!(
            reg.query_text("go", "highlights").unwrap().as_deref(),
            Some("[(true)] @constant.builtin\n")
        );
        assert_eq!(reg.query_text("go", "folds").unwrap(), None, "包里没有");
        assert_eq!(
            reg.query_text("nope", "highlights").unwrap(),
            None,
            "未知包"
        );

        // 重扫同一个目录：已注册的包静默跳过 —— 「重新扫描解析器」不该给
        // 每个包报一条"重名"（plan §3.2 重扫只增不改）。坏包仍会再报一次。
        let again = reg.scan_plugins(&root);
        assert!(
            again.iter().all(|d| !d.message.contains("重名")),
            "重扫不该给已注册的包报重名: {again:?}"
        );
        let again_pkgs: Vec<&str> = again.iter().map(|d| d.package.as_str()).collect();
        assert_eq!(again_pkgs, vec!["broken", "illegal", "notgo"]);

        // 另一个目录里的同名包才是真冲突。
        let other = root.join("other");
        std::fs::create_dir_all(other.join("go")).unwrap();
        std::fs::write(other.join("go/manifest.toml"), "name = \"go\"\n").unwrap();
        std::fs::copy(&dll, other.join("go/go.dll")).unwrap();
        let dup = reg.scan_plugins(&other);
        assert_eq!(dup.len(), 1, "{dup:?}");
        assert!(dup[0].message.contains("重名"), "{}", dup[0].message);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_missing_dir_is_not_an_error() {
        let mut reg = LanguageRegistry::new();
        let diags = reg.scan_plugins(Path::new("Z:/certainly/not/here"));
        assert!(diags.is_empty());
        assert!(reg.get("json").is_none(), "没装包就没有 grammar");
        assert_eq!(reg.resolve_ext("json").as_deref(), Some("json"), "兜底表照旧");
    }

    /// 「语法着色」里的勾选：禁用不卸载 DLL（§3.2），只是解析不到；
    /// `[Syntax] Disabled` 就是 `disabled_names()` 的往返。
    #[test]
    fn disabling_a_package_hides_it_without_unloading() {
        let Some(dll) = fixture_dll() else {
            eprintln!("skip: fixture dll 未构建");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "dg-syntax-disable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let go = root.join("go");
        std::fs::create_dir_all(&go).unwrap();
        std::fs::write(
            go.join("manifest.toml"),
            "name = \"go\"\ndisplay = \"Go\"\nextensions = [\"go\"]\nfilenames = [\"Dockerfile\"]\nentry = \"tree_sitter_fixture_json\"\n",
        )
        .unwrap();
        std::fs::copy(&dll, go.join("go.dll")).unwrap();

        let mut reg = LanguageRegistry::new();
        assert!(reg.scan_plugins(&root).is_empty());
        assert!(reg.get("go").is_some(), "先真的加载一次，让状态变 Loaded");

        assert!(reg.set_enabled("go", false));
        assert!(reg.get("go").is_none(), "禁用后不解析");
        assert!(!reg.has("go"), "has 也要说没有");
        assert_eq!(reg.resolve_ext("go"), None, "扩展名不再指向它");
        assert_eq!(reg.resolve_filename("Dockerfile"), None, "文件名同理");
        assert_eq!(reg.disabled_names(), vec!["go"]);

        let info = reg.plugin_list().into_iter().next().expect("一个包");
        assert!(!info.enabled);
        assert_eq!(info.extensions, vec!["go"]);
        assert_eq!(
            info.state,
            PluginState::Loaded,
            "禁用不卸载：DLL 还在进程里"
        );

        // 再启用：同一个 Language 还在缓存里，不用重新 LoadLibrary。
        assert!(reg.set_enabled("go", true));
        assert!(reg.get("go").is_some());
        assert!(reg.disabled_names().is_empty());
        assert_eq!(reg.resolve_ext("go").as_deref(), Some("go"));

        // 启动路径：整份禁用名单重设。
        reg.apply_disabled(&["go".to_string()]);
        assert!(reg.get("go").is_none());
        reg.apply_disabled(&[]);
        assert!(reg.get("go").is_some());

        assert!(!reg.set_enabled("nope", false), "没有这个包");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 首行嗅探：`manifest.first_lines` 是**前缀**，包名字典序先到先得；
    /// 空条目不能变成「匹配所有文件」，禁用包也不参与。
    #[test]
    fn the_first_line_sniffs_the_script_interpreter() {
        let Some(dll) = fixture_dll() else {
            eprintln!("skip: fixture dll 未构建");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "dg-syntax-shebang-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let dir = root.join("pyish");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.toml"),
            "name = \"pyish\"
first_lines = [\"#!/usr/bin/env python\", \"#!/usr/bin/python\"]
entry = \"tree_sitter_fixture_json\"
",
        )
        .unwrap();
        std::fs::copy(&dll, dir.join("pyish.dll")).unwrap();

        let mut reg = LanguageRegistry::new();
        assert!(reg.scan_plugins(&root).is_empty());
        assert_eq!(
            reg.resolve_first_line("#!/usr/bin/env python3").as_deref(),
            Some("pyish"),
            "前缀匹配，python3 也算 python"
        );
        assert_eq!(
            reg.resolve_first_line("#!/usr/bin/python").as_deref(),
            Some("pyish")
        );
        assert_eq!(reg.resolve_first_line("#!/bin/bash"), None, "别的解释器不认");
        assert_eq!(reg.resolve_first_line(""), None, "空行不匹配");
        assert_eq!(reg.resolve_first_line("   "), None);

        // 禁用之后不参与嗅探 —— 和 resolve_ext 同一条规则。
        assert!(reg.set_enabled("pyish", false));
        assert_eq!(reg.resolve_first_line("#!/usr/bin/env python3"), None);
        assert!(reg.set_enabled("pyish", true));
        assert_eq!(
            reg.resolve_first_line("#!/usr/bin/env python3").as_deref(),
            Some("pyish")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 体积上限：`manifest.max_file_bytes` 是包自己的事，闭区间，缺省不设限；
    /// 没有 manifest 的内置项（`register`）同样不设限。
    #[test]
    fn the_size_limit_comes_from_the_package() {
        let Some(dll) = fixture_dll() else {
            eprintln!("skip: fixture dll 未构建");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "dg-syntax-maxbytes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let dir = root.join("small");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.toml"),
            "name = \"small\"
extensions = [\"small\"]
max_file_bytes = 64
entry = \"tree_sitter_fixture_json\"
",
        )
        .unwrap();
        std::fs::copy(&dll, dir.join("small.dll")).unwrap();

        let mut reg = LanguageRegistry::new();
        assert!(reg.scan_plugins(&root).is_empty());
        assert!(reg.fits_file("small", 0), "空文件当然行");
        assert!(reg.fits_file("small", 64), "边界算在内");
        assert!(!reg.fits_file("small", 65), "超一个字节就不附加");
        assert!(!reg.fits_file("small", 1 << 30));
        // 没写 max_file_bytes 的包不设限；注册进来的内置项也没有 manifest。
        assert!(reg.fits_file("不存在的包", 1 << 40));
        let lang = reg.get("small").expect("fixture 语言");
        reg.register("builtin", lang);
        assert!(reg.fits_file("builtin", 1 << 40));

        // 禁用的包不接手任何文件 —— 与 get/resolve_* 一条规则。
        assert!(reg.set_enabled("small", false));
        assert!(!reg.fits_file("small", 1));
        assert!(reg.set_enabled("small", true));
        assert!(reg.fits_file("small", 1));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 两个包抢同一个扩展名（`.h` 既是 C 也是 C++）：**包名字典序，先到先得**。
    /// 规则要确定，否则「同一个文件今天高亮成 A、明天成 B」；想换就把包改名，
    /// 或者在对话框里禁掉先到的那个。`plugins` 是 `BTreeMap`，这条规则由它保证。
    #[test]
    fn two_packages_claiming_one_extension_go_to_the_first_name() {
        let Some(dll) = fixture_dll() else {
            eprintln!("skip: fixture dll 未构建");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "dg-syntax-conflict-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        for name in ["aaa", "zzz"] {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("manifest.toml"),
                format!(
                    "name = \"{name}\"
extensions = [\"h\"]
entry = \"tree_sitter_fixture_json\"
"
                ),
            )
            .unwrap();
            std::fs::copy(&dll, dir.join(format!("{name}.dll"))).unwrap();
        }
        let mut reg = LanguageRegistry::new();
        assert!(reg.scan_plugins(&root).is_empty());
        assert_eq!(
            reg.resolve_ext("h").as_deref(),
            Some("aaa"),
            "字典序第一个赢"
        );
        // 禁掉先到的，才轮到下一个 —— 不需要任何「覆盖表」。
        assert!(reg.set_enabled("aaa", false));
        assert_eq!(reg.resolve_ext("h").as_deref(), Some("zzz"));
        assert!(reg.set_enabled("aaa", true));
        assert_eq!(reg.resolve_ext("h").as_deref(), Some("aaa"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
