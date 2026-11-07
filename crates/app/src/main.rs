// 发行版是 Windows GUI 子系统：从资源管理器双击打开时，系统不会再给进程配
// 一个控制台窗口。dev/test 构建仍是控制台子系统，命令行诊断和测试输出照常
// 看得见。GUI 子系统只影响「系统要不要新建控制台」：从终端或管道启动时，
// 继承来的标准句柄仍然有效，--selftest / --packages 这些不开窗的开关照旧输出。
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // 开关分两种：不带值的（`--packages`）和带一个值的（`--ui-lint <file>`）。
    // 带值的那个值不能被当成要打开的文件 —— `dogedit --ui-lint ui.lua x.txt`
    // 里 `x.txt` 才是路径。解析放在 `Launch` 里，开窗和进程间转交用的是同一
    // 份结果。
    let launch = dg_app::launch::Launch::parse(&args);
    // 安全模式在读任何配置之前就要定下来：主题是开窗时挑的，脚本是开窗之
    // 前装的，晚一步就白开了。下面几个诊断开关不看它 —— 诊断本来就是
    // 「把有问题的那份跑一遍」。
    if launch.safe_mode {
        dg_app::safety::set(true);
    }
    if launch.selftest {
        let ok = dg_app::app::Editor::selftest();
        println!("selftest: {}", if ok { "OK" } else { "FAILED" });
        std::process::exit(if ok { 0 } else { 1 });
    }

    // `--packages` 不开窗：它只回答「这台机器上有哪些包、能不能装上、按什么
    // 顺序装」。
    if launch.packages {
        let scanned = dg_script::package::discover(&dg_script::package::roots());
        let loose: Vec<dg_script::package::Scanned> = dg_script::package::loose_roots()
            .iter()
            .flat_map(|root| dg_script::package::scan_loose(root))
            .collect();
        print!("{}", dg_script::package::report(&scanned, &loose));
        std::process::exit(0);
    }

    // `--ui-doctor` 也不开窗：它把包真装一遍（无窗口宿主）再把每一份主题的四个
    // 段过一遍，回答「这台机器上到底哪里有问题」。和 `--packages` 的分工：那个
    // 只看清单，不看脚本跑不跑得起来。
    if launch.doctor {
        let report = dg_app::doctor::run();
        print!("{}", report.text);
        // 提醒默认不算失败（主题文件是手写的，一条写不通不该让 CI 红）；
        // `--strict` 把它们也算上。
        let failed = report.errors > 0 || (launch.strict && report.warnings > 0);
        std::process::exit(if failed { 1 } else { 0 });
    }

    // `--ui-lint` 不开窗：它自己建一个无窗口的 Lua 宿主跑画法，报「这一格到底
    // 画出了什么」。收一个**脚本文件**，不需要包、不需要清单。
    if let Some(file) = &launch.lint_file {
        match dg_app::skin::lint(std::path::Path::new(file)) {
            Ok(report) => {
                print!("{}", report.text);
                std::process::exit(if report.errors > 0 { 1 } else { 0 });
            }
            Err(e) => {
                eprintln!("ui-lint: {e}");
                std::process::exit(2);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        std::process::exit(dg_app::win32::run(&launch));
    }
    #[cfg(target_os = "macos")]
    {
        std::process::exit(dg_app::cocoa::run(&launch));
    }
    #[cfg(target_os = "linux")]
    {
        std::process::exit(dg_app::gtk::run(&launch));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        eprintln!("no GUI backend for this platform");
        std::process::exit(1);
    }
}
