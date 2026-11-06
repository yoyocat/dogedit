//! 批量试编译。两种输入，自动识别：
//!
//! - **runtime 目录**（Helix 布局，含 `grammars/` + `queries/`）：把每个
//! `queries/<lang>/*.scm` 配 `grammars/<lang>.dll` 走一遍加载 +
//! `Query::new`，报告哪些组合编不过。
//! - **parsers 目录**（本项目的插件包布局，含 `<name>/manifest.toml`）：
//! 走真正的 `LanguageRegistry::scan_plugins`，逐包加载 DLL 并编译它的
//! query —— 装完一批包后确认"哪些包有问题"就用这个。
//!
//! cargo run --release -p dg-syntax --example query_sweep -- <目录>
//!
//! 只做验证，不产生任何文件。

use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("用法: query_sweep <目录>"));
    if root.join("grammars").is_dir() {
        sweep_runtime(&root);
    } else if root.is_dir() {
        sweep_parsers(&root);
    } else {
        eprintln!("{} 不存在", root.display());
        std::process::exit(2);
    }
}

/// 插件包布局：`<root>/<name>/{manifest.toml, <name>.dll, queries/}`。
/// 直接走注册表，所以 manifest 校验、目录名一致性、惰性加载全都在测。
fn sweep_parsers(root: &Path) {
    let mut reg = dg_syntax::LanguageRegistry::new();
    let diags = reg.scan_plugins(root);
    let plugins = reg.plugin_list();
    println!("包 {} 个", plugins.len());
    if !diags.is_empty() {
        println!("扫描诊断 {} 条：", diags.len());
        for d in &diags {
            println!("  [{}] {}", d.package, d.message);
        }
    }
    let (mut bad, mut nq) = (0, 0);
    for p in &plugins {
        let Some(lang) = reg.get(&p.name) else {
            println!("  {}: DLL 加载失败", p.name);
            bad += 1;
            continue;
        };
        let mut notes = Vec::new();
        for f in ["highlights", "folds"] {
            match reg.query_text(&p.name, f) {
                Ok(None) => {}
                Ok(Some(scm)) => {
                    nq += 1;
                    if let Err(e) = tree_sitter::Query::new(&lang, &scm) {
                        notes.push(format!("{f}.scm {}", e.to_string().lines().next().unwrap_or("")));
                    }
                }
                Err(e) => notes.push(format!("{f}.scm 读取失败: {e}")),
            }
        }
        if !notes.is_empty() {
            println!("  {}: {}", p.name, notes.join("; "));
            bad += 1;
        }
    }
    println!("query 文件 {nq} 个；有问题的包 {bad} 个");
}

fn sweep_runtime(root: &Path) {
    let grammars = root.join("grammars");
    let queries = root.join("queries");

    let mut langs: Vec<String> = std::fs::read_dir(&queries)
        .expect("queries 目录")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect();
    langs.sort();

    let (mut ok, mut no_dll, mut bad_load, mut bad_q, mut total_q) = (0, 0, 0, 0, 0);
    let mut failures: Vec<String> = Vec::new();

    for lang in &langs {
        let dll = find_dll(&grammars, lang);
        let Some(dll) = dll else {
            no_dll += 1;
            failures.push(format!("{lang}: 没有 DLL"));
            continue;
        };
        // Helix 的导出约定：`-` → `_`
        let entry = format!("tree_sitter_{}", lang.replace('-', "_"));
        let language = match unsafe { dg_syntax::load_parser(&dll, &entry) } {
            Ok(l) => l,
            Err(e) => {
                bad_load += 1;
                failures.push(format!("{lang}: 加载失败 {e}"));
                continue;
            }
        };
        let mut lang_ok = true;
        for f in ["highlights", "folds", "locals", "indents", "textobjects", "injections", "tags"] {
            let p = queries.join(lang).join(format!("{f}.scm"));
            let Ok(scm) = std::fs::read_to_string(&p) else {
                continue;
            };
            total_q += 1;
            if let Err(e) = tree_sitter::Query::new(&language, &scm) {
                lang_ok = false;
                bad_q += 1;
                failures.push(format!("{lang}/{f}.scm: {}", first_line(&e.to_string())));
            }
        }
        if lang_ok {
            ok += 1;
        }
    }

    println!("语言 {} 个，query 文件 {total_q} 个", langs.len());
    println!("全通过 {ok}；缺 DLL {no_dll}；DLL 加载失败 {bad_load}；query 编译失败 {bad_q}");
    if !failures.is_empty() {
        println!("\n明细：");
        for f in failures.iter().take(400) {
            println!("  {f}");
        }
        if failures.len() > 400 {
            println!("  ... 还有 {} 条", failures.len() - 400);
        }
    }
}

/// `grammars/<lang>.dll`，大小写不敏感（Windows 上文件名大小写不定）。
fn find_dll(grammars: &Path, lang: &str) -> Option<PathBuf> {
    let direct = grammars.join(format!("{lang}.dll"));
    if direct.exists() {
        return Some(direct);
    }
    std::fs::read_dir(grammars)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(lang))
                && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll"))
        })
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or(s).to_string()
}
