//! dg-script 的 win32 宿主桥接(M1–M4)。
//!
//! [`EditorState::lua`] 持有一个持久 [`ScriptHost`];本模块把
//! [`Bridge`] 适配到活动文档,并提供全部入口:
//!
//! - [`init_host`]:启动时装载开机包(注册命令/事件/快捷键),storage.json
//! 落在 `<exe>/scripts`。写了 `launch` 的按需包**一条都不跑**,只进扩展菜单。
//! - [`toggle_launch`] / [`close_script_pane`]:按需包的生命周期 —— 点菜单装载,
//! 关掉它的窗格(或再点一次菜单)整包退出。
//! - [`event`] / [`command`] / [`menu_command_names`] / [`lookup_key`]:
//! M3 的回放入口。
//!
//! host 存在 `EditorState` 里(`Rc<ScriptHost>`)而 bridge 又要借
//! `&mut EditorState`,所以每个入口都走 [`with_host`]:克隆那个 `Rc`
//! (host 在堆上,借它不碰 `st`)、装上 bridge 执行,再按需刷新。
//!
//! 坐标换算遵循 dg-script 的约定(1 基行号、1 基字节列),在这层
//! 折算成文档的 0 基(行, 字符列)扁平偏移,再走 `Editor::apply_*`
//! —— 和键盘输入同一条 undo/布局/书签通道。

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, MessageBoxW, SetTimer, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
};

use dg_script::{Bridge, Pos, ScriptHost, TaskbarState, byte_col_to_char_col};

use super::prompt;
use super::window::EditorState;

/// 一行脚本日志的去处：stderr（启动时重定向到文件就看得见）加
/// `OutputDebugString`（挂在调试器上时看得见）。
///
/// 两条路都要，因为作者未必重定向了 stderr，也未必
/// 挂着调试器。`Bridge::log` 和绘制期的 sink 共用这一个函数 —— 同一个
/// `App.log` 在两种场合落到同一个地方，不会「点了按钮有日志、画的时候没有」。
pub(crate) fn log_line(msg: &str) {
    eprintln!("[lua] {msg}");
    let mut w: Vec<u16> = msg.encode_utf16().collect();
    w.push(0);
    unsafe {
        windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(PCWSTR(w.as_ptr()));
    }
}

pub(super) fn alert(hwnd: HWND, msg: &str, error: bool) {
    let mut text: Vec<u16> = msg.encode_utf16().collect();
    text.push(0);
    let mut title: Vec<u16> = "DogEdit Lua".encode_utf16().collect();
    title.push(0);
    unsafe {
        MessageBoxW(
            Some(hwnd),
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | if error { MB_ICONERROR } else { MB_ICONINFORMATION },
        );
    }
}

/// 用户脚本目录:`<exe目录>/scripts`(装载扫描和 storage.json)。
/// `--skin-debug`：每个绘制回调连跑两遍比 op。命令行开关或 `DOGEDIT_SKIN_DEBUG=1`。
///
/// 它让每次绘制翻倍，所以只在查「画法是不是纯函数」时开 —— 缓存的前提就是纯函数，
/// 有副作用的画法在「命中」和「重算」两条路上会给出不同结果，而这种 bug 平时看不出来。
fn skin_debug_on() -> bool {
    std::env::args_os().any(|a| a == "--skin-debug")
        || std::env::var_os("DOGEDIT_SKIN_DEBUG").is_some_and(|v| v != "0")
}

pub(super) fn scripts_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|d| d.join("scripts"))
}

/// 装载过的一个脚本：归属包、路径、装载那一刻的指纹。
///
/// 热重载按这张表逐条比 —— 不再有「那个 `skin.lua`」这个特例。包可以有好几个
/// 文件，改哪个都该生效。
pub(super) struct LoadedScript {
    pub pkg: String,
    pub path: std::path::PathBuf,
    pub stamp: Option<(u64, u64)>,
}

/// 「扩展」菜单里的一行按需启动包。行 id 是 `LAUNCH_CMD_BASE + 下标`，所以
/// 这份表的顺序就是菜单的顺序 —— 改表必须紧跟着重建菜单，不能只改一半。
#[derive(Clone, Debug)]
pub(super) struct LaunchEntry {
    pub id: String,
    pub label: String,
}

/// 重扫一遍盘，把「写了 `launch`、没被停用、没被同名覆盖」的包收成菜单表。
///
/// 三个地方调用：开机、刷新脚本菜单、改完启停名单。走的是和装载同一套
/// `scan_all`，所以「盘上有、菜单没有」这种分歧不会出现。
pub(super) fn refresh_launch_entries(st: &mut EditorState) {
    // 安全模式一条脚本都不跑；引擎没起来也一样。两种情况下挂一行点了没
    // 反应的菜单，比没有这一行更让人困惑。
    if crate::safety::on() || st.lua.is_none() {
        st.launch_rows.clear();
        return;
    }
    let off: std::collections::BTreeSet<String> =
        crate::settings::disabled_packages().into_iter().collect();
    let mut rows: Vec<LaunchEntry> = dg_script::package::scan_all()
        .into_iter()
        .filter_map(|s| s.result.ok())
        .filter(|p| {
            p.state != dg_script::State::Ignored && !off.contains(p.id()) && p.is_on_demand()
        })
        .map(|p| LaunchEntry {
            id: p.id().to_string(),
            label: p.menu_label().to_string(),
        })
        .collect();
    rows.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
    st.launch_rows = rows;
}

/// 「扩展」菜单该列哪些 `App.command`。
///
/// 按需包注册的命令是内部命令（方向键、暂停、重开走 `App.keymap` / `ui.on`），
/// 一个小游戏不该一个人占掉扩展菜单八行。只是不进菜单，`call_command` 照样能调。
pub(super) fn menu_command_names(st: &EditorState) -> Vec<String> {
    let hide: Vec<String> = st.launch_rows.iter().map(|e| e.id.clone()).collect();
    st.lua
        .as_ref()
        .map(|h| h.command_names_excluding(&hide))
        .unwrap_or_default()
}

/// 启动装载：构建持久 host、设 storage 目录，然后把所有脚本跑一遍。
///
/// **包和散装脚本走同一条路**：`packages/<id>/` 里的有清单，`<exe>/scripts/*.lua`
/// 没有 —— `scan_loose` 给后者造一份只有 id 的清单，所以下游只认一种东西。
///
/// 顺序不是「扫到哪算哪」：`package::sort` 先按宿主版本筛一遍，再按 `requires`
/// 拓扑排，同层内按 `order`、最后按 id。
pub(super) fn init_host(st: &mut EditorState) {
    let hwnd = st.hwnd;
    let Some(mut host) = (|| ScriptHost::new().map_err(|e| e.0))().map_or_else(
        |e| {
            alert(hwnd, &e, true);
            None
        },
        Some,
    ) else {
        return;
    };
    // 绘制回调里没有 bridge，可那正是最想打日志的时候 —— 把同一条日志路
    // 装成 sink，`print` / `App.log` 在绘制期也进 stderr 和 OutputDebugString。
    host.set_log_sink(std::sync::Arc::new(log_line));
    if let Some(dir) = scripts_dir() {
        host.set_storage_dir(&dir);
    }
    // 安全模式：宿主照建（各处都按「有 host、表可能空」写的），一个文件都不跑。
    let problems = if crate::safety::on() {
        log_line("安全模式：脚本一条都不跑");
        Vec::new()
    } else {
        load_scripts(&mut host, st, hwnd)
    };
    for p in &problems {
        log_line(p);
    }
    // 一个弹窗，不是每个脚本一个：启动时连弹五个会把窗口挡死，而且坏脚本
    // 往往是一个语法错误被同一个文件里的每一段重复报。
    if !problems.is_empty() {
        let mut text = format!("{} 个脚本没能装上：\n\n", problems.len());
        for p in problems.iter().take(5) {
            text.push_str(p);
            text.push('\n');
        }
        if problems.len() > 5 {
            text.push_str(&format!("…另有 {} 个，见日志\n", problems.len() - 5));
        }
        alert(hwnd, &text, true);
    }
    if skin_debug_on() {
        host.set_paint_debug(true);
        eprintln!("[script] debug on: 每个绘制回调跑两遍比 op，一处报一次");
    }
    st.lua = Some(std::rc::Rc::new(host));
    // 按需包一行脚本都没跑，先把它们的菜单行收好 —— 命令 id 是「下标」，
    // 表和菜单必须是同一份，所以这份表也参与菜单重建。
    refresh_launch_entries(st);
    // 装载期改的外观/布局（`App.ui.style:metric`、`ui.layout:pane` 这类写在文件
    // 顶层的调用）不用在这里补一次：初始主题要到 `WM_CREATE` 才装，窗格也在那
    // 之后才 `sync_script_panes`，两样都已经在表里了。脏位只是清掉 —— 留着会让
    // 第一条命令白重排一次。
    if let Some(host) = &st.lua {
        host.take_dirty();
    }
}

/// 跑一遍所有能装的包，返回装不上的那些（一句话一条）。
///
/// 单个文件失败**不带走整个包**：后面的文件可能正是补它的那个，而且一个包里
/// 有十块窗格时，因为第四块的语法错就全不装是最糟的结果。
///
/// 用户停用的包（`setting.ini` 的 `[Scripts] Disabled=`）根本不进这条循环：
/// 它们既不排序也不装载，屏幕上等于「从未装过」。写了 `launch` 的按需包
/// 也一样不进循环 —— 它们只在扩展菜单里留一行，点它才走 [`launch_package`]。
fn load_scripts(host: &mut ScriptHost, st: &mut EditorState, hwnd: HWND) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();
    let mut packages = Vec::new();
    for s in dg_script::package::scan_all() {
        match s.result {
            // 被同名覆盖的那份不跑：管理器和 `--ui-doctor` 都是这个口径，
            // 装载少一份判断就会出现「盘上明明只有一条 snake，却跑了两遍」。
            Ok(p) if p.state != dg_script::State::Ignored => packages.push(p),
            Ok(_) => {}
            // 清单坏了只是这个包不上场：其余包照旧，用户还能用编辑器。
            Err(why) => problems.push(format!("{}: {why}", s.id)),
        }
    }
    let off: std::collections::BTreeSet<String> =
        crate::settings::disabled_packages().into_iter().collect();
    let (ok, disabled, bad) =
        dg_script::package::sort_with(packages, dg_script::package::HOST_VERSION, &off);
    for p in &disabled {
        log_line(&format!("{}: 已停用", p.id()));
    }
    for p in &bad {
        problems.push(format!("{}: {}", p.id(), state_reason(&p.state)));
    }
    // 写了 `launch` 的包开机**一条脚本都不跑**：它们只在扩展菜单里占一行，
    // 点那一行才走 [`launch_package`]。分开之后 `loaded_scripts` 里也就没有
    // 它们 —— 热重载不会盯一个还没启动的包，定时器也不会为它 arm。
    let (auto, on_demand): (Vec<_>, Vec<_>) = ok.into_iter().partition(|p| !p.is_on_demand());
    for p in &on_demand {
        log_line(&format!("{}: 按需启动（扩展菜单「{}」）", p.id(), p.menu_label()));
    }
    let mut loaded = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    for pkg in &auto {
        for path in pkg.script_paths() {
            // 指纹要在**装载前**记：装载期间用户可能正好在存盘，那一次改动
            // 应该由下一跳重跑，而不是被当成「装载时已经是这样了」。
            let stamp = stamp(&path);
            let mut bridge = EditorBridge {
                st,
                hwnd,
                edited: false,
            };
            match host.run_file_in(pkg.id(), &path, &mut bridge) {
                Ok(()) => loaded.push(LoadedScript {
                    pkg: pkg.id().to_string(),
                    path,
                    stamp,
                }),
                Err(e) => {
                    let why = e.to_string();
                    problems.push(format!("{}: {why}", path.display()));
                    // 管理器要的是「这个包为什么没在跑」，一个包记第一条就够：
                    // 十块窗格的包因为同一个语法错会报十遍，列出来也没人看。
                    if !failures.iter().any(|(id, _)| id == pkg.id()) {
                        failures.push((pkg.id().to_string(), first_line(&why)));
                    }
                }
            }
        }
    }
    st.loaded_scripts = loaded;
    st.script_failures = failures;
    problems
}

/// 一个包为什么没上场。`sort` 只往 `Rejected` / `Failed` 里写原因。
fn state_reason(state: &dg_script::State) -> &str {
    match state {
        dg_script::State::Rejected(why) | dg_script::State::Failed(why) => why.as_str(),
        dg_script::State::Disabled => "已停用",
        _ => "跳过",
    }
}

/// Lua 报错的第一行。管理器那一列只有一格宽，traceback 留给日志。
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).to_string()
}

// -----------------------------------------------------------------
// 按需启动：点菜单装载，关窗格（或再点一次菜单）整包退出
// -----------------------------------------------------------------

/// 从盘上找出还能启动的那一份。同一个 id 可能在几个根里都有，`Ignored`
/// 那份不能要 —— 它是被覆盖的旧版。
fn find_launch_package(id: &str) -> Option<dg_script::Package> {
    let off: std::collections::BTreeSet<String> =
        crate::settings::disabled_packages().into_iter().collect();
    dg_script::package::scan_all()
        .into_iter()
        .filter_map(|s| s.result.ok())
        .find(|p| {
            p.id() == id
                && p.state != dg_script::State::Ignored
                && !off.contains(p.id())
                && p.is_on_demand()
        })
}

/// 点一下「贪吃蛇」：把这个包的文件跑一遍。
///
/// 走 [`with_host`] 而不是直接 `run_file_in`：脚本顶层那句 `ui.layout:pane`
/// 置的布局脏位要在这里变成真窗口，心跳定时器要在这里 arm，`App.run` 排的
/// 队要在这里派发。少了这一层，游戏会「装载成功但看不见」。
fn launch_package(st: &mut EditorState, hwnd: HWND, id: &str) {
    let Some(pkg) = find_launch_package(id) else {
        log_line(&format!("{id}: 找不到这个按需包（可能被停用或从盘上删了）"));
        return;
    };
    let paths = pkg.script_paths();
    let stamps: Vec<_> = paths.iter().map(|p| stamp(p)).collect();
    let done = with_host(st, hwnd, |host, bridge| {
        let mut ok: Vec<(std::path::PathBuf, Option<(u64, u64)>)> = Vec::new();
        for (path, stamp) in paths.iter().zip(&stamps) {
            match host.run_file_in(id, path, bridge) {
                Ok(()) => ok.push((path.clone(), *stamp)),
                Err(e) => {
                    log_line(&format!("{}: {e}", path.display()));
                    // 同包后半段可能依赖前半段建的表，别带着半份状态上场。
                    break;
                }
            }
        }
        ok
    });
    let Some(done) = done else { return };
    if done.is_empty() {
        st.script_failures.retain(|(p, _)| p != id);
        st.script_failures
            .push((id.to_string(), "装载失败，见脚本日志".to_string()));
        return;
    }
    for (path, stamp) in done {
        st.loaded_scripts.push(LoadedScript {
            pkg: id.to_string(),
            path,
            stamp,
        });
    }
    st.script_failures.retain(|(p, _)| p != id);
    log_line(&format!("{id}: 已启动"));
}

/// 整包退出。这是「关掉蛇之后不占内存」的落点：
///
/// - `purge_package` 摘掉十项注册、定时器、待办队列、画布缓存，并跑满两轮
///   Lua GC，把闭包和它们攥住的表真的还回去；
/// - `apply_layout_changes` 把窗格声明的投影（真 HWND）一并拆掉；
/// - 心跳定时器重算一次，没了就 `KillTimer`；
/// - 热重载表里也不留它的文件。
pub(super) fn unload_package(st: &mut EditorState, hwnd: HWND, pkg: &str) {
    let Some(host) = st.lua.clone() else {
        return;
    };
    let before = host.memory().0 / 1024;
    let removed = host.purge_package(pkg);
    let after = host.memory().0 / 1024;
    st.loaded_scripts.retain(|e| e.pkg != pkg);
    st.script_failures.retain(|(id, _)| id != pkg);
    log_line(&format!(
        "{pkg}: 已退出，清掉 {} 条注册，Lua 堆 {before}→{after} KiB",
        removed.total()
    ));
    // 窗格是注册表的投影：注册没了，窗口也得跟着拆。只把窗格移出停靠栏
    // 是不够的 —— 那块 HWND 还在，下次点启动会被同名声明复用。
    apply_layout_changes(st, hwnd);
    // 定时器可能刚刚被整批清掉；把「已 arm 到的时刻」作废，强制重算。
    st.script_timer_due = None;
    arm_timers(st, hwnd);
}

/// 菜单行是个开关：没跑就启动，跑着就退出。
pub(super) fn toggle_launch(st: &mut EditorState, hwnd: HWND, id: &str) {
    if crate::safety::on() {
        log_line("安全模式：不启动按需脚本");
        return;
    }
    if st.lua.is_none() {
        log_line("脚本引擎没起来，启动不了按需包（见启动日志）");
        return;
    }
    if st.loaded_scripts.iter().any(|e| e.pkg == id) {
        unload_package(st, hwnd, id);
    } else {
        launch_package(st, hwnd, id);
    }
    // 两种方向都要重排：行文字在「启动 / 关闭」之间换，命令行也会跟着
    // 多出来或消失。菜单行本身也是这一刻重建的。
    st.macro_reload();
}

/// ✕ 关掉一块脚本窗格。按需包的**最后一块**窗格 = 用户玩完了，整包退出。
/// 返回 `true` 表示窗格已经被拆掉，调用方不要再走「只移出停靠栏」那条路。
pub(super) fn close_script_pane(st: &mut EditorState, hwnd: HWND, pane_id: &str) -> bool {
    let Some(host) = st.lua.clone() else {
        return false;
    };
    let Some(pkg) = host.sole_pane_package(pane_id) else {
        return false;
    };
    // 只有清单里写了 `launch` 的包有「关窗 = 退出」这个约定；开机装载的包
    // 关一块窗格只是把面板收起来，脚本还得继续干活。
    if !st.launch_rows.iter().any(|e| e.id == pkg) {
        return false;
    }
    unload_package(st, hwnd, &pkg);
    st.macro_reload();
    true
}

// -----------------------------------------------------------------
// 管理器：列出所有包，改启停
// -----------------------------------------------------------------

/// 管理器表里的全部行，外加「清单读不出来」的个数。
///
/// 走的是和装载**同一套**判断（`scan_all` + `sort_with`），所以「管理器说能跑、
/// 程序不跑」这种分歧不会出现。区别只有一处：清单读不出来的包不进表（它们的
/// 勾选框没有意义 —— 装不装都跑不了），只报个数，细节在 `--ui-doctor`。
pub(super) fn package_rows(st: &EditorState) -> (Vec<super::pkgdlg::Row>, usize) {
    let off: std::collections::BTreeSet<String> =
        crate::settings::disabled_packages().into_iter().collect();
    let mut ok = Vec::new();
    let mut broken = 0usize;
    for s in dg_script::package::scan_all() {
        match s.result {
            // 被同名覆盖的那一份不单列：同一个 id 一行就够，生效的那份才是它。
            Ok(pkg) if pkg.state != dg_script::State::Ignored => ok.push(pkg),
            Ok(_) => {}
            Err(_) => broken += 1,
        }
    }
    let (live, disabled, bad) =
        dg_script::package::sort_with(ok, dg_script::package::HOST_VERSION, &off);
    let mut rows: Vec<super::pkgdlg::Row> = Vec::new();
    for (pkg, enabled) in live
        .iter()
        .map(|p| (p, true))
        .chain(disabled.iter().map(|p| (p, false)))
        .chain(bad.iter().map(|p| (p, !off.contains(p.id()))))
    {
        let failure = st
            .script_failures
            .iter()
            .find(|(id, _)| id == pkg.id())
            .map(|(_, why)| why.clone());
        let note = match (enabled, &pkg.state, failure) {
            (false, _, _) => "已停用".to_string(),
            (true, _, Some(why)) => format!("装载失败：{why}"),
            (true, dg_script::State::Rejected(why), None) => why.clone(),
            (true, dg_script::State::Failed(why), None) => why.clone(),
            // 按需包「已启用」只代表菜单里有它，别让人以为开机已经跑过了。
            _ if pkg.is_on_demand() => {
                if st.loaded_scripts.iter().any(|e| e.pkg == pkg.id()) {
                    "运行中（按需启动）".to_string()
                } else {
                    "按需启动（点扩展菜单）".to_string()
                }
            }
            _ => "已启用".to_string(),
        };
        rows.push(super::pkgdlg::Row {
            id: pkg.id().to_string(),
            version: pkg.manifest.version.clone(),
            origin: origin_text(pkg.origin).to_string(),
            note,
            scripts: pkg.manifest.scripts.clone(),
            dir: pkg.dir.display().to_string(),
            enabled,
        });
    }
    // 表的顺序按 id 稳定：三份（启用/停用/被拒）各有各的排序规则，
    // 混在一起按 id 排才读得顺。
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    (rows, broken)
}

fn origin_text(origin: dg_script::package::Origin) -> &'static str {
    use dg_script::package::Origin;
    match origin {
        Origin::Bundled => "随包",
        Origin::User => "用户",
        Origin::Development => "开发",
        Origin::Loose => "散装",
    }
}

/// 把新的停用名单落盘并**当场**生效：停用的包把注册全拿回去，启用的包重新跑。
///
/// 光写 `setting.ini` 就得重启才看得见，而管理器是「我点一下就该有反应」的地方。
/// 停用走 `purge_package`（十项注册一次清干净，`Counts::total` 报个数），
/// 启用就是把它的文件再跑一遍 —— 和热重载同一条路，所以不会重复注册。
///
/// 窗格也在清理范围内：`apply_layout_changes` → `sync_script_panes` 会建/拆
/// 真 HWND，所以停用一个带窗格的包之后那块会当场消失，不用重开进程。
///
/// **按需包是例外**：启用它不会顺手把脚本跑起来（那正是 `launch` 的意思），
/// 只是让扩展菜单里那一行回来；要玩得用户自己点。
pub(super) fn apply_disabled(st: &mut EditorState, hwnd: HWND, disabled: &[String]) {
    let before = crate::settings::disabled_packages();
    if before.as_slice() == disabled {
        return;
    }
    crate::settings::set_disabled_packages(disabled);
    // 安全模式下脚本一条都没跑，这里也就没有什么可清、可补的 —— 名单照写
    // （那正是用户进来要做的事：把闯祸的包关掉），装载留给下一次正常启动。
    if crate::safety::on() {
        log_line("安全模式：停用名单已写入，装载留到下次启动");
        return;
    }
    let Some(host) = st.lua.clone() else {
        return;
    };
    let off: std::collections::BTreeSet<String> = disabled.iter().cloned().collect();

    // 停用：**看注册表里有没有它的东西**，不看 setting.ini 变了什么。
    // 名单的差集不是判据 —— 第一次停用时 `before` 是空的（文件里根本没有
    // `[Scripts]` 这一行），按差集算就一条都清不掉，屏幕上什么都不会变。
    for id in disabled {
        if !st.loaded_scripts.iter().any(|e| &e.pkg == id) {
            continue;
        }
        let removed = host.purge_package(id);
        st.loaded_scripts.retain(|e| &e.pkg != id);
        log_line(&format!("{id}: 已停用，清掉 {} 条注册", removed.total()));
    }

    // 启用：盘上有、现在没在跑、也不在停用名单里的，跑一遍。
    // 按依赖顺序排，所以「启用的包依赖另一个刚启用的包」也是对的。
    let mut candidates = Vec::new();
    for s in dg_script::package::scan_all() {
        if let Ok(pkg) = s.result
            && pkg.state != dg_script::State::Ignored
            && !off.contains(pkg.id())
            && !pkg.is_on_demand()
            && !st.loaded_scripts.iter().any(|e| e.pkg == pkg.id())
        {
            candidates.push(pkg);
        }
    }
    if !candidates.is_empty() {
        let (live, _, _) =
            dg_script::package::sort_with(candidates, dg_script::package::HOST_VERSION, &off);
        for pkg in &live {
            for path in pkg.script_paths() {
                let stamp = stamp(&path);
                let mut bridge = EditorBridge {
                    st,
                    hwnd,
                    edited: false,
                };
                match host.run_file_in(pkg.id(), &path, &mut bridge) {
                    Ok(()) => st.loaded_scripts.push(LoadedScript {
                        pkg: pkg.id().to_string(),
                        path,
                        stamp,
                    }),
                    Err(e) => log_line(&format!("{}: {e}", path.display())),
                }
            }
            log_line(&format!("{}: 已启用", pkg.id()));
        }
    }
    // 窗格/状态段/工具栏都是注册表的投影：注册变了就得重建一遍，否则屏幕上
    // 还留着上一个包画的东西。
    apply_layout_changes(st, hwnd);
    // 停用名单变了，按需包的菜单行跟着增删；命令行也因为 purge 变了。
    refresh_launch_entries(st);
    st.macro_reload();
    arm_timers(st, hwnd);
}

// -----------------------------------------------------------------
// 新建脚本向导：落盘 + 当场装载
// -----------------------------------------------------------------

/// 把向导交出的 [`Plan`](super::newscript::Plan) 落到磁盘，再当场装载一遍。
///
/// 装载走 [`load_scripts`] 同一条路：`run_file_in` 进持久 host，成功的文件
/// 进 `loaded_scripts`（热重载的定时器盯的就是这张表），失败的那条进
/// `script_failures`（管理器状态列的原料）。**文件不清掉** —— 装载失败
/// 多半是一个语法错，主文件马上会在编辑器里打开，改完重启或重开就能跑。
pub(super) fn create_script(
    st: &mut EditorState,
    hwnd: HWND,
    plan: &super::newscript::Plan,
) -> Result<(), String> {
    for (path, content) in &plan.files {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("建不了目录 {}：{e}", dir.display()))?;
        }
        std::fs::write(path, content).map_err(|e| format!("写不了 {}：{e}", path.display()))?;
    }
    // 安全模式：文件照写（那是用户要的产物），装载等下一次正常启动。
    if crate::safety::on() {
        log_line("安全模式：脚本文件已写好，装载留到下次启动");
        return Ok(());
    }
    let Some(host) = st.lua.clone() else {
        return Err("脚本引擎没有运行（启动日志里有原因）。文件已写好，重启后装载。".to_string());
    };
    for (path, _) in plan
        .files
        .iter()
        .filter(|(p, _)| p.extension().is_some_and(|e| e == "lua"))
    {
        let stamp = stamp(path);
        let mut bridge = EditorBridge {
            st,
            hwnd,
            edited: false,
        };
        match host.run_file_in(&plan.pkg_id, path, &mut bridge) {
            Ok(()) => {
                st.loaded_scripts.push(LoadedScript {
                    pkg: plan.pkg_id.clone(),
                    path: path.clone(),
                    stamp,
                });
                log_line(&format!("{}: 已装载", path.display()));
            }
            Err(e) => {
                let why = e.to_string();
                st.script_failures.push((plan.pkg_id.clone(), first_line(&why)));
                return Err(format!(
                    "{} 装载失败：{why}（文件已写好，改完重启装载）",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

/// 关窗时打一行总账：几处有副作用的画法、缓存命中率多少。
///
/// 放在 `WM_DESTROY` 里（`EditorState` 还在），不能放到 `run` 的消息循环之后 ——
/// 那时候状态已经跟着窗口一起没了。
pub(super) fn dump_debug(st: &EditorState) {
    if !skin_debug_on() {
        return;
    }
    let Some(host) = st.lua.as_ref() else {
        return;
    };
    let (hits, misses) = host.paint_stats();
    let total = hits + misses;
    let rate = if total == 0 {
        0.0
    } else {
        hits as f64 * 100.0 / total as f64
    };
    let volatile = host.paint_volatile_count();
    eprintln!(
        "[script] debug: 有副作用的画法 {} 处；绘制缓存命中 {hits}/{total}（{rate:.1}%），\
         另有 {volatile} 次读了 App.now() 不进缓存",
        host.paint_impurity_count()
    );
}

// -----------------------------------------------------------------
// 热重载：装载过的脚本改一下就重跑
// -----------------------------------------------------------------

/// 一个文件「改过没有」的指纹：`(修改时间毫秒, 字节数)`。
///
/// 只比时间戳不够 —— 同一个时间粒度里连存两版（编辑器里 Ctrl+S 很快）会漏；
/// 加上长度就够了，而且比读整份文件便宜得多。
pub(super) fn stamp(path: &std::path::Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let ms = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some((ms, meta.len()))
}

/// 重跑一个脚本。返回 `true` 表示真的换上了新的一版。
///
/// 失败**不弹窗**：热重载是边改边看，弹窗会把窗口抢走；一行 stderr 就够。
/// `load` 先编译再 `purge`，所以语法错的这一版不会把还在工作的那份清掉 ——
/// 屏幕上保持上一次成功的样子。
pub(super) fn reload_script(
    st: &mut EditorState,
    hwnd: HWND,
    pkg: &str,
    path: &std::path::Path,
) -> bool {
    let Some(host) = st.lua.clone() else {
        return false;
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("script")
        .to_string();
    let mut bridge = EditorBridge {
        st,
        hwnd,
        edited: false,
    };
    match host.run_file_in(pkg, path, &mut bridge) {
        Ok(()) => {
            eprintln!("[script] reloaded {name}");
            true
        }
        Err(e) => {
            eprintln!("[script] {name}: {e}");
            false
        }
    }
}

/// 有脚本改过就重跑那一个，然后把每一条自绘带重画一遍。
///
/// 从「盯着一个 `skin.lua`」改成「盯着装载过的每一个文件」：包可以有好几个
/// 文件，改哪个都该生效。`WM_TIMER` 一跳做 N 次 `stat`（每次几百微秒），
/// 不值得上目录监视线程。
pub(super) fn skin_tick(st: &mut EditorState, hwnd: HWND) {
    let mut changed: Vec<(String, std::path::PathBuf)> = Vec::new();
    for entry in &mut st.loaded_scripts {
        let now = stamp(&entry.path);
        if now == entry.stamp {
            continue;
        }
        // 指纹读不到（文件正被写、或刚被换掉）就先记着，下一跳拿到稳定的再说。
        // 指纹变了就重跑：`load` 先编译再 `purge`，所以语法错的这一版不会把
        // 还在工作的那份清掉，屏幕上保持上一版成功的样子。
        entry.stamp = now;
        if now.is_some() {
            changed.push((entry.pkg.clone(), entry.path.clone()));
        }
    }
    if changed.is_empty() {
        return;
    }
    let mut any = false;
    for (pkg, path) in &changed {
        any |= reload_script(st, hwnd, pkg, path);
    }
    if !any {
        return;
    }
    apply_layout_changes(st, hwnd);
}

/// 布局表变过（声明期、热重载、或者运行时 `ui.layout:update` / `:remove`）：
/// 把窗口对齐到新的声明上，再重画各条自绘带。
///
/// 三样东西各走各的路，缺一样就有一半不生效：
///
/// - **窗格是窗口**：`sync_script_panes` 建/拆/换边，新窗格要等布局给它矩形才
///   露得出来。
/// - **状态栏是数据**：`reload_row` 重取声明（`SB_SETPARTS` 得跟着改，否则加了
///   一段也没地方放），然后按新表重排一次。
/// - **工具栏是按钮表的一部分**：增删了要重建控件才会动，多一格就是多一格宽。
pub(super) fn apply_layout_changes(st: &mut EditorState, hwnd: HWND) {
    st.sync_script_panes(hwnd);
    if st.panels.status_win.reload_row() {
        st.refresh_status_bar();
    }
    st.reload_script_toolbar(hwnd);
    st.repaint_all_chrome(hwnd);
}

/// 公共骨架:克隆 host、bridge 装上执行;
/// 若脚本改了文档,补上与键盘编辑相同的收尾(夹紧光标 + 统一刷新)。
fn with_host<R>(
    st: &mut EditorState,
    hwnd: HWND,
    f: impl FnOnce(&ScriptHost, &mut EditorBridge<'_>) -> R,
) -> Option<R> {
    // `Rc` 克隆而不是 take/put-back：host 在堆上，借它不影响 `st`，
    // 所以 `bridge` 拿 `&mut st` 和 `host` 同时存在是安全的。
    let host = st.lua.clone()?;
    let (r, edited) = {
        let mut bridge = EditorBridge { st, hwnd, edited: false };
        let r = f(&host, &mut bridge);
        // 回调里 `App.run` / `App.defer` 排的队在这里派发：回调已经返回，
        // `EditorState` 的借用还没结束 —— 正是内置命令可以安全跑的那个缝隙。
        host.run_queue(&mut bridge);
        (r, bridge.edited)
    };
    // 外观改过（`App.ui.style:set` / `:reset`）：重走一遍「装主题」那条路。
    // 只把覆盖再盖一遍不够 —— `reset` 要把主题给的那个值拿回来，而那个值只有
    // 主题文件知道。脏位在这里取：每个入口都经过这个函数。
    let dirty = host.take_dirty();
    if dirty & dg_script::style::DIRTY_APPEARANCE != 0 {
        st.apply_style_overrides(hwnd);
    }
    // 布局改过（`ui.layout:update` / `:remove`，或者回调里第一次声明了元素）：
    // 建/拆窗口、重取状态段、重建工具栏。和热重载走同一条路 —— 两边各写一份
    // 迟早会分叉。
    if dirty & dg_script::paint::DIRTY_LAYOUT != 0 {
        apply_layout_changes(st, hwnd);
    }
    // 脚本改了自己内部的状态（`App.ui.repaint`）：先按名字丢绘制缓存，再作废
    // 对应的窗口。窗格是独立子窗口，父窗口的 invalidate 管不到，得单独点名。
    if dirty & dg_script::paint::DIRTY_REPAINT != 0 {
        let regions = host.take_repaint_regions();
        repaint_script_surfaces(st, hwnd, &regions);
    }
    if edited {
        let ed = st.script_active_mut();
        let last = ed.doc.line_count().saturating_sub(1);
        ed.caret.0 = ed.caret.0.min(last);
        ed.caret.1 = ed.caret.1.min(ed.doc.line_len(ed.caret.0));
        st.after_undo_redo(hwnd);
    }
    // 回调里可能刚登记/取消了一个定时器：出这里之前把表重新装对。
    arm_timers(st, hwnd);
    Some(r)
}

/// 按最早的到期时刻装 `WM_TIMER`（或在不剩定时器时拆掉）。
///
/// `script_timer_due` 记住**已经 arm 到的那个绝对时刻**：没变就什么都不做，
/// 所以每个事件入口都调一次也不会把倒计时反复推后。
pub(super) fn arm_timers(st: &mut EditorState, hwnd: HWND) {
    let want = st.lua.as_ref().and_then(|h| h.next_timer_due());
    if want == st.script_timer_due {
        return;
    }
    match want {
        None => {
            if st.script_timer_due.take().is_some() {
                unsafe {
                    let _ = KillTimer(Some(hwnd), super::window::SCRIPT_TIMER_ID);
                }
            }
        }
        Some(due) => {
            let now = st.lua.as_ref().map(|h| h.now_ms()).unwrap_or(0);
            // USER 定时器精度 ~15.6 ms，1 ms 是「尽快」而不是忙等。
            let ms = due.saturating_sub(now).clamp(1, 60_000) as u32;
            unsafe {
                let _ = SetTimer(Some(hwnd), super::window::SCRIPT_TIMER_ID, ms, None);
            }
            st.script_timer_due = Some(due);
        }
    }
}

/// `App.ui.repaint(region)` 的收尾：点名 `pane.<id>` 时**只**作废那一块窗格。
///
/// 一个每 100 ms 推进的小游戏不该让工具栏那四十几格、状态栏和页签条陪着
/// 重画。没点名、点了别的区域、或者窗格名字认不出来，就退回「各条自绘带 +
/// 所有脚本窗格」全作废 —— 宁可多画一帧，也别让脚本改了东西却看不见。
fn repaint_script_surfaces(st: &mut EditorState, hwnd: HWND, regions: &[String]) {
    let mut full = regions.is_empty();
    for region in regions {
        let lower = region.to_ascii_lowercase();
        match lower.strip_prefix("pane.") {
            Some(id) if st.panels.invalidate_script_pane(id) => {}
            _ => full = true,
        }
    }
    if full {
        st.repaint_all_chrome(hwnd);
        st.panels.repaint_script_panes();
    }
}

/// `WM_TIMER` 的落点：点掉到期的定时器，再按新的最早到期重装。
pub(super) fn run_timers(st: &mut EditorState, hwnd: HWND) {
    // 这一跳已经烧掉了，让 `arm_timers` 无条件重算。
    st.script_timer_due = None;
    let ran = with_host(st, hwnd, |host, bridge| host.run_timers(bridge)).unwrap_or(0);
    if ran > 0 {
        // 皮肤定时器基本都是动画：跳过一次就重画一帧，脚本不必再请求。
        //
        // 状态栏和脚本窗格都是**独立子窗口**，invalidate 父窗口不会让它们重画
        // （父窗口带 WS_CLIPCHILDREN，子窗口自己那份表面得自己作废）。少了窗格
        // 这一下，动画窗格只在鼠标进出或改窗口大小时才动 —— 时钟会一直停着。
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            st.panels.status_win.invalidate();
        }
        st.panels.repaint_script_panes();
    }
}

/// 把一次点击交给皮肤（`s:on(region, "press"/"rclick", fn)`）。
///
/// 返回 true = 脚本接管了，调用方跳过内置动作。点击发生在 `WM_LBUTTONDOWN`
/// 里，`EditorState` 没被借走，所以这条路上 handler 能 `print`、能 `App.run`。
pub(super) fn press(
    st: &mut EditorState,
    hwnd: HWND,
    region: &str,
    event: &str,
    input: &dg_script::PaintInput,
) -> bool {
    with_host(st, hwnd, |host, bridge| {
        host.press(region, event, input, bridge)
    })
    .flatten()
    .unwrap_or(false)
}

/// 触发一个事件(DocSaved / DocOpened)。handler 失败由引擎记日志。
pub(super) fn event(st: &mut EditorState, hwnd: HWND, name: &str) {
    let _ = with_host(st, hwnd, |host, bridge| host.trigger(name, bridge));
}

/// 调用一个 `App.command` 注册的命令;返回命令是否存在且成功。
pub(super) fn command(st: &mut EditorState, hwnd: HWND, name: &str) -> bool {
    with_host(st, hwnd, |host, bridge| host.call_command(name, bridge)).unwrap_or(false)
}

/// 手势 → 命令名(只读入口)。
pub(super) fn lookup_key(
    st: &EditorState,
    ctrl: bool,
    shift: bool,
    alt: bool,
    vk: u32,
) -> Option<String> {
    st.lua.as_ref().and_then(|h| h.lookup_key(ctrl, shift, alt, vk))
}

struct EditorBridge<'a> {
    st: &'a mut EditorState,
    hwnd: HWND,
    edited: bool,
}

impl EditorBridge<'_> {
    /// 1 基行号 → 行文本;`None` 当行号越界。
    fn line_text_1(&self, line: usize) -> Option<String> {
        let ed = self.st.script_active();
        if line == 0 || line > ed.doc.line_count() {
            return None;
        }
        let mut out = String::new();
        let _ = ed.doc.line_text(line - 1, &mut out);
        Some(out)
    }

    /// (行, 字节列) → 文档 (行, 字符列);行号越界返回 `None`。
    fn to_pos(&self, (line, byte_col): Pos) -> Option<dg_core::linedoc::Pos> {
        let t = self.line_text_1(line)?;
        let doc_line = line - 1;
        let char_col = byte_col_to_char_col(&t, byte_col);
        Some(dg_core::linedoc::Pos {
            line: doc_line,
            col: char_col,
        })
    }

    /// (行, 字节列) → 文档 (行, 字符列),越界位置夹紧到文档内。
    fn to_doc_pos(&self, (line, byte_col): Pos) -> (usize, usize) {
        let ed = self.st.script_active();
        let line = line
            .saturating_sub(1)
            .min(ed.doc.line_count().saturating_sub(1));
        let mut t = String::new();
        let _ = ed.doc.line_text(line, &mut t);
        let col = byte_col_to_char_col(&t, byte_col).min(ed.doc.line_len(line));
        (line, col)
    }
}

impl Bridge for EditorBridge<'_> {
    /// `App.run("file.save")` 的落点。两种写法都认：内置命令表里的
    /// `cm_file_save`，和皮肤文档里的点分写法 `file.save`。
    fn run_command(&mut self, name: &str) -> bool {
        let cm = if name.starts_with("cm_") {
            name.to_string()
        } else {
            format!("cm_{}", name.replace('.', "_"))
        };
        let Some(cmd) = crate::keys::Command::parse(&cm) else {
            return false;
        };
        let hwnd = self.hwnd;
        let page = self.st.editor_visible_rows();
        let ok = self.st.run_command(hwnd, cmd, page);
        if ok {
            // 和菜单那条路一样：命令自己不一定重画，光标挪了得让它显示出来。
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            }
        }
        ok
    }
    /// `App.view.scroll_to`：滚到某一行某一列。列是**字符**列（和
    /// `App.view.info()` 一把尺子），行是 0 基的视觉首行。
    fn scroll_to(&mut self, line: usize, col: usize) -> bool {
        let ed = self.st.script_active_mut();
        let last = ed.doc.line_count().saturating_sub(1);
        ed.first_row = line.min(last);
        ed.h_scroll = col.min(i32::MAX as usize) as i32;
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.hwnd), None, false);
        }
        true
    }
    fn doc_meta(&self) -> dg_script::DocMeta {
        self.st.script_doc_meta()
    }
    fn view_info(&self) -> dg_script::ViewInfo {
        self.st.script_view_info()
    }
    fn alert(&mut self, msg: &str) {
        alert(self.hwnd, msg, false);
    }
    fn log(&mut self, msg: &str) {
        log_line(msg);
    }
    fn doc_name(&self) -> String {
        self.st.script_active().display_name()
    }
    fn doc_path(&self) -> Option<String> {
        self.st.script_active().path.clone()
    }
    fn line_count(&self) -> usize {
        self.st.script_active().doc.line_count()
    }
    fn line_text(&self, line: usize) -> Option<String> {
        self.line_text_1(line)
    }
    fn doc_text(&self) -> String {
        self.st.script_active().doc.to_text()
    }
    fn span_text(&self, s: Pos, e: Pos) -> Option<String> {
        let a = self.to_pos(s)?;
        let b = self.to_pos(e)?;
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        Some(self.st.script_active().doc.range_text(a, b))
    }
    fn insert_at(&mut self, p: Pos, text: &str) -> bool {
        let Some(pos) = self.to_pos(p) else {
            return false;
        };
        let ok = self.st.script_active_mut().apply_insert(pos, text);
        self.edited |= ok;
        ok
    }
    fn replace_span(&mut self, s: Pos, e: Pos, text: &str) -> bool {
        let (Some(a), Some(b)) = (self.to_pos(s), self.to_pos(e)) else {
            return false;
        };
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let ok = self.st.script_active_mut().apply_replace(a, b, text);
        self.edited |= ok;
        ok
    }
    fn sel_count(&self) -> usize {
        self.st.script_active().selection.ranges.len()
    }
    fn sel_range(&self, index: usize) -> Option<(Pos, Pos)> {
        let ed = self.st.script_active();
        let r = ed.selection.ranges.get(index)?;
        let conv = |(line, col): (usize, usize)| -> Pos {
            let mut t = String::new();
            let _ = ed.doc.line_text(line, &mut t);
            (line + 1, dg_script::char_col_to_byte_col(&t, col))
        };
        Some((conv(r.start), conv(r.end)))
    }
    fn sel_text(&self) -> String {
        let ed = self.st.script_active();
        let Some(r) = ed.selection.ranges.first() else {
            return String::new();
        };
        let (a, b) = (r.start, r.end);
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        ed.doc.range_text(
            dg_core::linedoc::Pos { line: a.0, col: a.1 },
            dg_core::linedoc::Pos { line: b.0, col: b.1 },
        )
    }
    fn sel_set(&mut self, s: Pos, e: Pos) {
        let (start, end) = (self.to_doc_pos(s), self.to_doc_pos(e));
        let ed = self.st.script_active_mut();
        let r = dg_core::Range { start, end }.normalized();
        ed.caret = r.end;
        ed.anchor = None;
        ed.selection.clear();
        ed.selection.add(r);
    }
    fn prompt(&mut self, title: &str, initial: &str) -> Option<String> {
        prompt::prompt(self.hwnd, title, title, initial)
    }
    /// `App.taskbar` 的落点:和外部工具完成时用的是同一个 taskbar 对象与
    /// 同一套状态,只是这条路由脚本自己负责摘除。
    fn taskbar_progress(&mut self, state: TaskbarState, completed: u64, total: u64) -> bool {
        super::notify::set_progress(self.hwnd, state, completed, total)
    }
}
