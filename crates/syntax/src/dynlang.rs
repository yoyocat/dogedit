//! 插件包 DLL 的动态加载层（parser_plugin_plan.md §3）。
//!
//! 铁律（§3.2 生命周期）：
//! 1. **永不 `FreeLibrary`** —— `Tree`/`Parser`/`Query` 持有 DLL 内的代码指针，
//! 任何时刻卸载都是 UB。加载成功的句柄全部收养进进程级 [`LIVED_LIBS`]，
//! 随进程退出释放。
//! 2. 调用方（registry）负责惰性：只在首次用到某语言时才进本模块。
//! 3. 失败分类成 [`PluginError`]，文案可直接进输出窗口。

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use libloading::Library;
use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

/// 进程级 DLL 收容所：加载过的解析器库在这里住到进程结束。
static LIVED_LIBS: OnceLock<Mutex<Vec<Library>>> = OnceLock::new();

/// 加载失败的原因分类。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginError {
    /// `LoadLibraryW` 失败（文件缺失、依赖 DLL 缺失等）。
    Io(String),
    /// 导出表里没有 `entry` 符号。
    SymbolMissing(String),
    /// `language()` 返回了空指针。
    NullLanguage,
    /// ABI 版本超出当前 tree-sitter 运行时的兼容区间。
    Abi {
        got: u32,
        supported: (u32, u32),
    },
    /// 结构体不合法或 `set_language` 拒绝（含冒烟探针失败）。
    NotParseable,
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "DLL 加载失败: {e}"),
            Self::SymbolMissing(entry) => write!(f, "缺少导出符号 {entry}"),
            Self::NullLanguage => write!(f, "语言函数返回了空指针"),
            Self::Abi { got, supported } => write!(
                f,
                "ABI 版本 {got} 超出兼容区间 [{}, {}]（DLL 与本程序 tree-sitter 版本不匹配，请换用对应 CLI 重新构建）",
                supported.0, supported.1
            ),
            Self::NotParseable => write!(f, "语言结构体不合法，无法解析"),
        }
    }
}

/// 加载一个解析器 DLL 并校验其语言结构体。
///
/// # Safety
///
/// `dll_path` 必须是可信来源的 tree-sitter 语言库（`entry` 指向
/// `TSLanguage *(*)(void)` 形态的导出函数）。加载的原生代码与宿主同权限运行。
pub unsafe fn load_parser(dll_path: &Path, entry: &str) -> Result<Language, PluginError> {
    // 绝对路径加载，不触碰 DLL 搜索顺序（防劫持）。
    // Library::new 会跑 DLL 初始化器，unsafe。
    let lib = unsafe { Library::new(dll_path) }.map_err(|e| PluginError::Io(e.to_string()))?;
    let sym: libloading::Symbol<unsafe extern "C" fn() -> *const ()> = unsafe {
        lib.get(entry.as_bytes())
    }
    .map_err(|_| PluginError::SymbolMissing(entry.to_string()))?;
    let fp = *sym;
    drop(sym);
    // 先自己调一次拿指针：`Language::from(LanguageFn)` 不查空，而后续
    // abi_version 会解引用 —— 空语言必须在包装前拦下。语言函数只返回
    // DLL 内静态数据的地址，重复调用是安全的。
    let raw: *const () = unsafe { fp() };
    adopt(lib);
    if raw.is_null() {
        return Err(PluginError::NullLanguage);
    }
    let lang = Language::from(unsafe { LanguageFn::from_raw(fp) });
    let min = tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION as u32;
    let max = tree_sitter::LANGUAGE_VERSION as u32;
    let abi = lang.abi_version() as u32;
    if abi < min || abi > max {
        return Err(PluginError::Abi {
            got: abi,
            supported: (min, max),
        });
    }
    if !lang.is_parseable() {
        return Err(PluginError::NotParseable);
    }
    // 冒烟探针：空文档解析一遍。拦得住加载即崩/字段错乱的低级坏包
    // （拦不住有越界 bug 的解析器 —— 进程内无法隔离，见 plan §7）。
    let mut probe = tree_sitter::Parser::new();
    probe
        .set_language(&lang)
        .map_err(|_| PluginError::NotParseable)?;
    let _ = probe.parse(b"", None);
    Ok(lang)
}

/// 把库句柄收养进进程级收容所（永不释放）。
fn adopt(lib: Library) {
    let libs = LIVED_LIBS.get_or_init(|| Mutex::new(Vec::new()));
    // 唯一一次所有权转移；之后没有任何代码路径会把它取出来。
    libs.lock().expect("lived libs poisoned").push(lib);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fixture DLL 未构建（`cargo test -p dg-syntax` 只编本 crate）时跳过。
    fn fixture_dll() -> Option<std::path::PathBuf> {
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

    #[test]
    fn fixture_dll_loads_and_parses() {
        let Some(dll) = fixture_dll() else {
            eprintln!("skip: fixture dll 未构建（cargo build -p fixture-json-dll）");
            return;
        };
        let lang = unsafe { load_parser(&dll, "tree_sitter_fixture_json") }.expect("loads");
        let mut h = crate::Highlighter::new(lang);
        h.reset("{\"a\": [1, 2]}");
        assert!(h.kind_at(2).is_some(), "DLL 加载的语言能解析");
    }

    #[test]
    fn missing_symbol_reports_symbol_missing() {
        let Some(dll) = fixture_dll() else {
            eprintln!("skip: fixture dll 未构建");
            return;
        };
        let err = unsafe { load_parser(&dll, "tree_sitter_nope") }.unwrap_err();
        assert_eq!(err, PluginError::SymbolMissing("tree_sitter_nope".into()));    }
}
