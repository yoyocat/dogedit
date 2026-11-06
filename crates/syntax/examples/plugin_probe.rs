//! 插件包自检：给一个解析器包目录和一个源文件，打印
//! manifest → DLL 加载 → query 编译 → 高亮 runs / 折叠区 的全链路结果。
//!
//! cargo run -p dg-syntax --example plugin_probe -- <包目录> <源文件>
//!
//! 包目录必须是 `<根>/<name>/` 形式（目录名 == manifest.name），
//! 这里把它的父目录交给注册表扫描，等价于主程序扫 `parsers\`。

use dg_syntax::{Highlighter, LanguageRegistry, ThemeConfig};

fn main() {
    let pkg = std::env::args().nth(1).expect("用法: plugin_probe <包目录> <源文件>");
    let src = std::env::args().nth(2).expect("用法: plugin_probe <包目录> <源文件>");
    let pkg = std::path::PathBuf::from(pkg);
    let name = pkg
        .file_name()
        .and_then(|n| n.to_str())
        .expect("包目录名")
        .to_string();

    // 1) 扫描（manifest 校验 + 目录名一致性 + DLL 存在性）
    let mut reg = LanguageRegistry::new();
    let root = pkg.parent().expect("父目录").to_path_buf();
    let diags = reg.scan_plugins(&root);
    println!("包: {name}  根: {}", root.display());
    if diags.is_empty() {
        println!("诊断: 无");
    } else {
        for d in &diags {
            println!("诊断: [{}] {}", d.package, d.message);
        }
    }

    // 2) 惰性加载 DLL
    let Some(lang) = reg.get(&name) else {
        eprintln!("加载失败：{name} 未注册（见上面的诊断）");
        std::process::exit(1);
    };
    println!("DLL 加载: ok  abi={}", lang.abi_version());

    // 3) query
    let text = std::fs::read_to_string(&src).expect("读源文件");
    let mut h = Highlighter::new(lang);
    for file in ["highlights", "folds"] {
        match reg.query_text(&name, file) {
            Ok(None) => println!("query {file}: 包内没有（走 kind 启发式）"),
            Ok(Some(scm)) => {
                let r = if file == "folds" {
                    h.set_fold_query(&scm)
                } else {
                    h.set_query(&scm)
                };
                match r {
                    Ok(()) => println!("query {file}: 编译 ok（{} 字节）", scm.len()),
                    Err(e) => println!("query {file}: 编译失败 -> {e}"),
                }
            }
            Err(e) => println!("query {file}: 读取失败 -> {e}"),
        }
    }

    h.reset(&text);
    println!("解析: ok  tree={}", h.tree().is_some());

    // 4) 折叠区（字节区间 + 行区间）
    let nt = |s: usize, e: usize| text.as_bytes()[s..e].to_vec();
    let spans = h.fold_spans(&nt);
    println!("折叠区 {} 个", spans.len());
    for (s, e) in spans.iter().take(10) {
        println!(
            "  bytes {s:>6}..{e:<6} rows {}-{}",
            row_of(&text, *s),
            row_of(&text, *e)
        );
    }

    // 5) 高亮 runs（按行，颜色是 0xRRGGBB）
    let theme = ThemeConfig::default();
    let lines = text.split('\n').count();
    let mut byte = 0usize;
    println!("\n高亮（每行按 run 拆开，色值 0xRRGGBB）：");
    for line in 0..lines {
        let end = text[byte..]
            .find('\n')
            .map(|i| byte + i)
            .unwrap_or(text.len());
        let cl = |s: usize, e: usize| -> u32 {
            text[s.min(text.len())..e.min(text.len())]
                .chars()
                .count() as u32
        };
        let runs = h.highlight_range(byte, end, &theme, &cl, &nt);
        let mut col = byte;
        let mut parts: Vec<String> = Vec::new();
        for r in &runs {
            // run 是字符数，回推字节区间只为打印文本。
            let mut stop = col;
            let mut seen = 0u32;
            while stop < end && seen < r.len {
                stop += text[stop..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                seen += 1;
            }
            let snippet = text[col..stop].replace(['\n', '\r'], " ");
            parts.push(format!("#{:06X} x{:<3} {:?}", r.color, r.len, snippet));
            col = stop;
        }
        println!("{:>4} | {}", line + 1, parts.join("  "));
        byte = (end + 1).min(text.len());
        if line > 40 {
            println!("... (只打印前 40 行)");
            break;
        }
    }
}

fn row_of(text: &str, byte: usize) -> usize {
    text.as_bytes()[..byte.min(text.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
}
