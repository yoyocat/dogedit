//! `--ui-doctor`：不开窗，把「这台机器上的脚本和主题到底哪里有问题」一次说完。
//!
//! 和 `--packages` 的分工：那个只回答「有哪些包、清单读不读得通」——它**不跑**
//! 脚本，所以「包装上了、但脚本第 12 行有个 nil」它看不见。这个跑。
//!
//! 和 `--ui-lint` 的分工：那个盯着**一个**文件的画法跑一遍，报每一格画出什么；
//! 这个看**全局**：包装不装得上、每一份主题的四个段有没有写不通的键。两个都
//! 不开窗，都能进 `diff`，都能当 CI 的一道闸。
//!
//! 为什么放在这里而不是 `win32/`：诊断不该只在 Windows 上能跑。它读的每一样
//! 东西（包、主题文件、`dg-skin` 的三张表）都是跨平台的。

use std::fmt::Write as _;
use std::path::PathBuf;

use dg_script::package;

/// 一次诊断的结果。
pub struct Report {
    /// 给终端看的全文，能直接 `diff`。
    pub text: String,
    /// 包装不上、主题文件读不出来。这些让退出码是 1。
    pub errors: usize,
    /// 主题里写不通的键、写了没人读的区域。只是说一声 —— `--strict` 才算错。
    pub warnings: usize,
}

/// 按默认的主题目录跑一遍。
pub fn run() -> Report {
    run_with(&crate::themes::dirs())
}

/// 按给定的目录跑一遍（测试用这个，不碰真实目录）。
pub fn run_with(theme_dirs: &[PathBuf]) -> Report {
    let mut out = String::new();
    let mut errors = 0usize;
    let mut warnings = 0usize;

    let scripts = check_scripts(&mut out, &mut errors);
    let themes = check_themes(&mut out, theme_dirs, &mut errors, &mut warnings);

    let _ = write!(out, "== 汇总 ==\n");
    if errors == 0 && warnings == 0 {
        let _ = writeln!(out, "没有问题。{scripts} · {themes}");
    } else {
        let _ = writeln!(out, "{errors} 处错误 · {warnings} 处提醒（{scripts} · {themes}）");
    }
    Report {
        text: out,
        errors,
        warnings,
    }
}

// ---------------------------------------------------------------------------
// 脚本
// ---------------------------------------------------------------------------

/// 把能装的包装进一个无窗口的宿主跑一遍，报装不上的。
///
/// 判据是「跑完了没有」，不是「注册了几条」：一个包里所有脚本都跑通，才算这个
/// 包能上场。单文件失败**不带走整个包**（和运行时同一条规矩），所以这里也按
/// 文件报。
fn check_scripts(out: &mut String, errors: &mut usize) -> String {
    // 散装脚本要分开跑（各开一个宿主），所以先把它们从这份合表里挑出来。
    let all = package::scan_all();
    let loose: Vec<package::Scanned> = all
        .iter()
        .filter(|s| s.origin == package::Origin::Loose)
        .cloned()
        .collect();
    let scanned: Vec<package::Scanned> = all
        .into_iter()
        .filter(|s| s.origin != package::Origin::Loose)
        .collect();

    // 用户停用的包不跑、也不算问题：诊断要说的是「这台机器上什么坏了」，
    // 「你自己关掉的那个」不是坏。停用名单和运行时读同一处（`[Scripts] Disabled=`）。
    let off: std::collections::BTreeSet<String> = crate::settings::disabled_packages()
        .into_iter()
        .collect();

    let mut ok = Vec::new();
    let mut bad: Vec<(String, String)> = Vec::new();
    let mut ignored: Vec<String> = Vec::new();
    for item in &scanned {
        match &item.result {
            // 「同 id 后面的根胜」是正常结果，不是坏包：这份不装载是对的，
            // `--packages` 已经把它单列出来了。诊断不该因此报红、坏掉 CI。
            Ok(pkg) if pkg.state == package::State::Ignored => ignored.push(item.id.clone()),
            Ok(pkg) if !pkg.state.is_idle() => ok.push(pkg.clone()),
            Ok(pkg) => bad.push((item.id.clone(), format!("清单读通了但不上场：{:?}", pkg.state))),
            Err(why) => bad.push((item.id.clone(), why.clone())),
        }
    }
    let (ok, disabled, rejected) = package::sort_with(ok, package::HOST_VERSION, &off);
    for pkg in &rejected {
        bad.push((pkg.id().to_string(), format!("依赖排不开：{:?}", pkg.state)));
    }

    // 装载：一个宿主，按排序后的顺序，逐包逐文件。冲突（两个包抢同一个元素 id）
    // 就是在这一步以「后装载的那个失败」的形式冒出来的 —— 和运行时同一套规则。
    let mut loaded = 0usize;
    let mut failed: Vec<(String, Vec<String>)> = Vec::new();
    match dg_script::ScriptHost::new() {
        Err(e) => {
            *errors += 1;
            bad.push(("<host>".to_string(), e.0));
        }
        Ok(host) => {
            for pkg in &ok {
                let mut problems = Vec::new();
                for path in pkg.script_paths() {
                    if let Err(e) =
                        host.run_file_in(pkg.id(), &path, &mut dg_script::HeadlessBridge)
                    {
                        problems.push(e.to_string());
                    }
                }
                if problems.is_empty() {
                    loaded += 1;
                } else {
                    failed.push((pkg.id().to_string(), problems));
                }
            }
        }
    }

    // 散装脚本：没有清单，一个文件就是一个单元，各开一个宿主 —— 它们不该互相
    // 看见对方的注册，否则「两个散装脚本抢同一个元素 id」会被当成装载失败。
    let mut loose_ok = 0usize;
    let mut loose_bad: Vec<(String, String)> = Vec::new();
    for item in &loose {
        if off.contains(&item.id) {
            continue;
        }
        match &item.result {
            Ok(pkg) => {
                let mut problems = Vec::new();
                for path in pkg.script_paths() {
                    let run = || -> Result<(), String> {
                        let host = dg_script::ScriptHost::new().map_err(|e| e.0)?;
                        host.run_file(&path, &mut dg_script::HeadlessBridge)
                            .map_err(|e| e.0)
                    };
                    if let Err(e) = run() {
                        problems.push(e);
                    }
                }
                if problems.is_empty() {
                    loose_ok += 1;
                } else {
                    loose_bad.push((item.id.clone(), problems.join("；")));
                }
            }
            Err(why) => loose_bad.push((item.id.clone(), why.clone())),
        }
    }

    *errors += bad.len() + failed.iter().map(|(_, p)| p.len()).sum::<usize>() + loose_bad.len();

    let _ = writeln!(out, "== 脚本 ==");
    let total = scanned.len();
    // 「0 个跑通」在一个包都没装时是废话，在一个包被停用时是误导 —— 所以停用的
    // 个数写在括号里，说清那 0 是怎么来的。
    let off_note = if disabled.is_empty() {
        String::new()
    } else {
        format!("，{} 个已停用", disabled.len())
    };
    let ignored_note = if ignored.is_empty() {
        String::new()
    } else {
        format!("，{} 个被同名覆盖", ignored.len())
    };
    let _ = writeln!(
        out,
        "{total} 个包（{loaded} 个跑通{off_note}{ignored_note}）· {} 个散装脚本（{loose_ok} 个跑通）",
        loose.len()
    );
    // 停用的单独一行、不算错：它解释的是「这个包为什么没动静」，而不是「哪里坏了」。
    if !disabled.is_empty() {
        let names: Vec<&str> = disabled.iter().map(|p| p.id()).collect();
        let _ = writeln!(
            out,
            "  已停用（[Scripts] Disabled）：{}",
            names.join(", ")
        );
    }
    if !ignored.is_empty() {
        let _ = writeln!(
            out,
            "  同名被覆盖（后面的根胜，不装载）：{}",
            ignored.join(", ")
        );
    }
    // 按需包在这里照样真跑一遍（语法/装载检查），但开机不跑。写清楚，免得
    // 「doctor 说跑通了」被读成「开机就加载」。
    let on_demand: Vec<&package::Package> = ok.iter().filter(|p| p.is_on_demand()).collect();
    if !on_demand.is_empty() {
        let names: Vec<String> = on_demand
            .iter()
            .map(|p| format!("{}（{}）", p.id(), p.menu_label()))
            .collect();
        let _ = writeln!(
            out,
            "  按需启动（此处已校验，开机不装载）：{}",
            names.join(", ")
        );
    }
    for (id, why) in &bad {
        let _ = writeln!(out, "  {id}");
        push_problem(out, why);
    }
    for (id, problems) in &failed {
        let _ = writeln!(out, "  {id}");
        for p in problems {
            push_problem(out, p);
        }
    }
    for (id, why) in &loose_bad {
        let _ = writeln!(out, "  {id}（散装）");
        push_problem(out, why);
    }
    if bad.is_empty() && failed.is_empty() && loose_bad.is_empty() && loaded + loose_ok > 0 {
        let _ = writeln!(
            out,
            "  {}",
            if disabled.is_empty() {
                "（都装上了）"
            } else {
                "（其余都装上了）"
            }
        );
    }
    out.push('\n');
    format!("{total} 个包 · {} 个散装脚本", loose.len())
}

// ---------------------------------------------------------------------------
// 主题
// ---------------------------------------------------------------------------

/// 每一份主题文件的四个段都过一遍。
///
/// 检查的不是「能不能用」（用不了也照样打开，这是设计），而是「哪几个键写了
/// 没人读」——那才是最难查的一类：名字对、键也对，屏幕上就是没反应。
fn check_themes(
    out: &mut String,
    dirs: &[PathBuf],
    errors: &mut usize,
    warnings: &mut usize,
) -> String {
    // 外部文件按「第一次出现的赢」去重，和 `themeext::existing` 同一条口径。
    let mut external: Vec<(String, PathBuf)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
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
            if external.iter().any(|(n, _)| n.eq_ignore_ascii_case(stem)) {
                continue;
            }
            external.push((stem.to_string(), path));
        }
    }

    let mut lines: Vec<String> = Vec::new();
    let mut covered = 0usize;
    let mut checked = 0usize;
    // 内置的：叠上同名外部文件（有就叠）。
    for (name, _) in crate::themes::BUNDLED_THEMES {
        let over = external
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, p)| p.clone());
        checked += 1;
        let (found, err) = check_one(name, over.as_deref());
        if over.is_some() {
            covered += 1;
        }
        match err {
            Some(e) => {
                *errors += 1;
                lines.push(format!("  {name}\n    ! {e}"));
            }
            None => {
                *warnings += found.len();
                if !found.is_empty() {
                    lines.push(one_theme(name, over.as_deref(), &found));
                }
            }
        }
    }
    // 只剩外部文件的：用户从零写的主题。
    for (name, path) in &external {
        if crate::themes::BUNDLED_THEMES
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            continue;
        }
        checked += 1;
        let (found, err) = check_one(name, Some(path));
        match err {
            Some(e) => {
                *errors += 1;
                lines.push(format!("  {name}\n    ! {e}"));
            }
            None => {
                *warnings += found.len();
                if !found.is_empty() {
                    lines.push(one_theme(name, Some(path), &found));
                }
            }
        }
    }

    let _ = writeln!(out, "== 主题 ==");
    let _ = writeln!(out, "{checked} 个主题（{covered} 个有外部覆盖）");
    if lines.is_empty() {
        let _ = writeln!(out, "  （四个段都没有写不通的键）");
    }
    for line in &lines {
        let _ = writeln!(out, "{line}");
    }
    out.push('\n');
    format!("{checked} 个主题")
}

fn one_theme(name: &str, path: Option<&std::path::Path>, found: &[String]) -> String {
    let head = match path {
        Some(p) => format!("  {name}  + {}", p.display()),
        None => format!("  {name}"),
    };
    let body = found
        .iter()
        .map(|f| format!("    ! {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{head}\n{body}")
}

/// 一条问题，`!` 只打在第一行上。
///
/// Lua 的报错自带 traceback（第二行起是调用栈），一律顶格 `!` 会让整段看起来
/// 像好几条互不相干的问题。
fn push_problem(out: &mut String, text: &str) {
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            let _ = writeln!(out, "    ! {line}");
        } else {
            let _ = writeln!(out, "      {}", line.trim_start());
        }
    }
}

/// 一份主题的提醒清单。`Err` 是「读不出来」这类硬错。
///
/// 三个来源各查各的：`[Metrics]` 走 `Metrics::apply_theme`、`[Shape]` 走
/// `Recipe::from_theme_file`、`[Fonts]` 走 `Fonts::from_theme_file` —— 和真正
/// 装载时**同一套代码**，所以诊断说「没问题」就等于屏幕上真的没问题。
fn check_one(
    name: &str,
    external_path: Option<&std::path::Path>,
) -> (Vec<String>, Option<String>) {
    let over = match external_path {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(text) => Some(text),
            // 只认 UTF-8：读的那一头也只认 UTF-8，写一个读不回来的编码等于把主题改坏。
            Err(e) => return (Vec::new(), Some(format!("{}: {e}", p.display()))),
        },
        None => None,
    };
    let Some((_, file)) = crate::themes::compose(name, over.as_deref()) else {
        return (Vec::new(), Some("既没有内置副本也没有外部文件".to_string()));
    };
    let mut found: Vec<String> = Vec::new();

    let mut metrics = dg_skin::Metrics::new();
    let m = metrics.apply_theme(&file);
    for key in &m.unknown {
        found.push(format!("[Metrics] {key}：不认识这个度量名"));
    }
    for (key, written, stored) in &m.clamped {
        found.push(format!("[Metrics] {key}={written} 越界，按 {stored} 生效"));
    }

    let (_, shape_problems) = dg_skin::Recipe::from_theme_file(&file);
    // `from_theme_file` 的每条问题自己就带 `[Shape] ` 前缀，别再加一遍。
    found.extend(shape_problems);
    for region in dg_skin::unwired_regions(&file) {
        found.push(format!(
            "[Shape] {region}：这个区域还没接绘制，写在那儿的键不生效"
        ));
    }

    let (fonts, font_problems) = dg_skin::Fonts::from_theme_file(&file);
    found.extend(font_problems);
    // `Fonts::set` 的夹取是**静默**的（只有「写不成数字」才进 problems），所以
    // 越界要自己比一遍：写 `mono.size=99` 存 48，和 `[Metrics]` 一样得说一声。
    for (key, value) in &file.fonts {
        let Some((name, "size")) = key.rsplit_once('.') else {
            continue;
        };
        let Some(token) = dg_skin::FontToken::parse(name) else {
            continue;
        };
        let (Some(spec), Ok(written)) = (fonts.get(token), value.trim().parse::<f32>()) else {
            continue;
        };
        if (written - spec.size).abs() > f32::EPSILON {
            found.push(format!("[Fonts] {key}={written} 越界，按 {} 生效", spec.size));
        }
    }

    // `[Theme]` 的键有两种写法：新 token 名（`editor.caret`，带点）和老 CamelCase
    // （`Caret`，不带点）。老写法是 `apply_theme_file` 里九十来条 `file.bg("…")`，
    // 没有一张可以对着查的表 —— 所以这里只判**带点的**：带点的只可能是 token 名，
    // `token()` 认不出就是写错了。老写法写错（`Carett`）查不出来，但它也不会
    // 造成「看起来生效了其实没有」的错觉之外的问题：两种写法并存时 token 赢，
    // 新写的东西应该用新写法。
    let probe = dg_render::theme::Theme::default();
    for key in file.colors.keys() {
        if key.contains('.') && probe.token(key).is_none() {
            found.push(format!("[Theme] {key}：不认识这个 token（跳过）"));
        }
    }
    (found, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份干净的主题一条都不报 —— 诊断说「没问题」得是真的没问题。
    #[test]
    fn a_clean_theme_says_nothing() {
        let (found, err) = check_one("DogEdit_Dark", None);
        assert!(err.is_none());
        assert!(found.is_empty(), "{found:?}");
    }

    /// 四种写不通的键各报一条，而且说清是哪个段、哪个键。
    #[test]
    fn every_unreadable_key_is_named() {
        let dir = std::env::temp_dir().join("dg-doctor-keys");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("Probe.ini");
        std::fs::write(
            &path,
            "[Theme]\neditor.caret=#FF0000\neditor.carett=#00FF00\n\
             [Metrics]\nstatus.height=500\n\
             [Shape]\neditor.gap=2\ntab.pad_x=9\n\
             [Fonts]\nmono.size=99\n",
        )
        .expect("seed");
        let (found, err) = check_one("Probe", Some(&path));
        let _ = std::fs::remove_file(&path);
        assert!(err.is_none());
        let text = found.join("\n");
        assert!(text.contains("[Theme] editor.carett"), "未知 token：{text}");
        assert!(text.contains("[Metrics] status.height=500"), "越界：{text}");
        assert!(text.contains("[Shape] editor"), "没接绘制的区域：{text}");
        assert!(text.contains("[Shape] tab.pad_x"), "度量写进 [Shape]：{text}");
        assert!(text.contains("[Fonts] mono.size"), "字号越界：{text}");
        assert!(!text.contains("editor.caret="), "写对的键不该出现：{text}");
    }

    /// 外部文件读不出来是**硬错**，不是提醒 —— 那时候屏幕上是内置那份，用户
    /// 改的那一版根本没参与。
    #[test]
    fn an_unreadable_theme_file_is_an_error() {
        let dir = std::env::temp_dir().join("dg-doctor-utf16");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("Probe.ini");
        std::fs::write(&path, [0xFF, 0xFE, 0x41, 0x00]).expect("seed");
        let (_, err) = check_one("Probe", Some(&path));
        assert!(err.is_some(), "非 UTF-8 要报错");
        let _ = std::fs::remove_file(&path);
    }
}
