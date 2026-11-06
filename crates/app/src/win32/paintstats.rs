//! 绘制计时（`--paint-stats`）。
//!
//! 要证明「脚本绘制不会把界面拖慢」，先得量出现在有多快 —— 整棵树此前没有任何
//! 绘制计时。这个模块只在命令行带 `--paint-stats`（或环境变量
//! `DOGEDIT_PAINT_STATS=1`）时工作；其余时候 `begin()` 只是一次原子读然后返回
//! `None`，代价几个纳秒。
//!
//! 计的是**我们自己的绘制代码**：`BeginPaint`/`EndPaint`、`BitBlt`、
//! `UpdateLayeredWindow` 之外的那一段。窗口创建、消息分发、控件的命中测试都不在内。
//! 每 5 秒往 stderr 打一段窗口统计，进程退出时再打一次全程累计 —— GUI 进程的
//! stderr 要用 `Start-Process -RedirectStandardError` 才抓得到。

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// 计时的区域。值就是数组下标，加变体要同步 [`NAMES`] 与 [`REGIONS`]。
#[derive(Clone, Copy)]
pub(super) enum Region {
    Frame = 0,
    TabStrip,
    DockTabs,
    StatusBar,
    MenuRow,
    Toolbar,
    Overlay,
    /// 一块脚本窗格。单列一格是因为它最贵：一次整块 Lua 回调 + 几十条 op 回放
    /// + 一次 `BitBlt`，而且滚轮每动一格就要重来一遍。
    Pane,
}

const REGIONS: usize = 8;

const NAMES: [&str; REGIONS] = [
    "frame",
    "tabstrip",
    "docktabs",
    "statusbar",
    "menu.row",
    "toolbar",
    "overlay",
    "pane",
];

/// 一次统计窗口的长度：5 秒。
const WINDOW_NS: u64 = 5_000_000_000;

/// 一个区域的计数器。窗口内的三项每次报告后清零，`total_*` 只增不减。
struct Slot {
    calls: AtomicU64,
    nanos: AtomicU64,
    max: AtomicU64,
    total_calls: AtomicU64,
    total_nanos: AtomicU64,
}

impl Slot {
    const fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
            max: AtomicU64::new(0),
            total_calls: AtomicU64::new(0),
            total_nanos: AtomicU64::new(0),
        }
    }
}

static ON: AtomicBool = AtomicBool::new(false);
static EPOCH: OnceLock<Instant> = OnceLock::new();
static WINDOW_START_NS: AtomicU64 = AtomicU64::new(0);
static SLOTS: [Slot; REGIONS] = [const { Slot::new() }; REGIONS];

/// 读命令行与环境变量，决定要不要计时。启动时调一次。
pub(super) fn init_from_args() {
    let flag = std::env::args_os().any(|a| a == "--paint-stats")
        || std::env::var_os("DOGEDIT_PAINT_STATS").is_some_and(|v| v != "0");
    if !flag {
        return;
    }
    let _ = EPOCH.get_or_init(Instant::now);
    WINDOW_START_NS.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    eprintln!("[paint] stats on: 5 s windows, stderr");
}

/// 一次绘制的计时器；`Drop` 时记账。没开统计时是 `None`，调用点不分支。
#[must_use]
pub(super) struct Guard {
    slot: usize,
    t0: Instant,
}

/// 开始计一个区域。`region` 的值必须落在 [`REGIONS`] 内。
pub(super) fn begin(region: Region) -> Option<Guard> {
    if !ON.load(Ordering::Relaxed) {
        return None;
    }
    Some(Guard {
        slot: region as usize,
        t0: Instant::now(),
    })
}

impl Drop for Guard {
    fn drop(&mut self) {
        let dt = self.t0.elapsed().as_nanos() as u64;
        let Some(slot) = SLOTS.get(self.slot) else {
            return;
        };
        slot.calls.fetch_add(1, Ordering::Relaxed);
        slot.nanos.fetch_add(dt, Ordering::Relaxed);
        slot.max.fetch_max(dt, Ordering::Relaxed);
        slot.total_calls.fetch_add(1, Ordering::Relaxed);
        slot.total_nanos.fetch_add(dt, Ordering::Relaxed);
        report_if_due();
    }
}

/// 满一个窗口就打印并清零。判断放在 `Drop` 里，省掉一个定时器 —— 绘制本来就会
/// 不断发生，没有绘制时也不需要统计。
fn report_if_due() {
    let Some(epoch) = EPOCH.get() else {
        return;
    };
    let now = epoch.elapsed().as_nanos() as u64;
    let start = WINDOW_START_NS.load(Ordering::Relaxed);
    if now.saturating_sub(start) < WINDOW_NS {
        return;
    }
    if WINDOW_START_NS
        .compare_exchange(start, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    print_window((now - start) as f64 / 1e9);
}

/// 一段窗口统计。整段一次 `eprint!`，免得和脚本日志交错。
fn print_window(secs: f64) {
    let mut out = format!("[paint] window {secs:.2}s\n");
    for i in 0..REGIONS {
        let slot = &SLOTS[i];
        let calls = slot.calls.swap(0, Ordering::Relaxed);
        let nanos = slot.nanos.swap(0, Ordering::Relaxed);
        let max = slot.max.swap(0, Ordering::Relaxed);
        if calls == 0 {
            continue;
        }
        out.push_str(&format!(
            "[paint]   {:<9} {calls:>5} calls {:>9.3} ms  avg {:>6} us  max {:>6} us\n",
            NAMES[i],
            nanos as f64 / 1e6,
            nanos / calls / 1_000,
            max / 1_000,
        ));
    }
    eprint!("{out}");
}

/// 全程累计，退出时打一次。
pub(super) fn dump() {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let mut out = String::from("[paint] lifetime\n");
    for i in 0..REGIONS {
        let slot = &SLOTS[i];
        let calls = slot.total_calls.load(Ordering::Relaxed);
        if calls == 0 {
            continue;
        }
        let nanos = slot.total_nanos.load(Ordering::Relaxed);
        out.push_str(&format!(
            "[paint]   {:<9} {calls:>6} calls {:>10.3} ms  avg {:>6} us\n",
            NAMES[i],
            nanos as f64 / 1e6,
            nanos / calls / 1_000,
        ));
    }
    eprint!("{out}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名字表与变体一一对应：少一个名字，报告里那一行就串位。
    #[test]
    fn every_region_has_a_name() {
        assert_eq!(NAMES.len(), REGIONS);
        assert_eq!(Region::Pane as usize, REGIONS - 1);
        assert_eq!(NAMES[Region::Pane as usize], "pane");
    }

    /// 没开统计时 `begin` 不取时间、不记账。
    #[test]
    fn begin_is_free_when_off() {
        ON.store(false, Ordering::Relaxed);
        assert!(begin(Region::Frame).is_none());
        assert_eq!(SLOTS[Region::Frame as usize].total_calls.load(Ordering::Relaxed), 0);
    }
}
