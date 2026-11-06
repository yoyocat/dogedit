//! 脚本定时器：`App.now` / `App.timer` / `App.cancel`。
//!
//! 沙箱里没有 `os`，脚本既拿不到时间也推迟不了工作 —— 动画、去抖、轮询全写不了。
//! 这一层把时钟和定时器交给宿主：`App.now()` 是宿主创建以来的毫秒数（单调，
//! 从 `Instant` 读，不受系统时间改动影响）；`App.timer` 只**登记**，真正的
//! 触发在 frame 的 `WM_TIMER` 里，所以回调永远不在 paint 期间跑。
//!
//! 登记带上块名（和 `App.on` 一样），`load` 重新加载同一块时连同它的定时器一起
//! 换掉 —— 否则改一次皮肤就多一组定时器，越改越忙。

use std::cell::RefCell;
use std::time::Instant;

use mlua::{Function, Lua};

/// 同时活着的定时器上限。一个皮肤用不到这么多，超了报错而不是静默吞掉。
pub(crate) const MAX_TIMERS: usize = 64;
/// 一次派发最多点几发；多出来的留到下一跳，别在一帧里跑干。
pub(crate) const MAX_FIRES_PER_PASS: usize = 64;
/// 周期下限。USER 定时器精度本来只有 ~15.6 ms，0 会变成每跳必发。
pub(crate) const MIN_PERIOD_MS: u64 = 1;
/// 周期上限（一天）。挡住 `App.timer(2^62, f)` 这种溢出。
pub(crate) const MAX_PERIOD_MS: u64 = 24 * 60 * 60 * 1000;

pub(crate) struct Timer {
    pub(crate) id: u32,
    /// 绝对到期时刻（宿主时钟毫秒）。
    pub(crate) due_ms: u64,
    /// 0 = 一次性。
    pub(crate) period_ms: u64,
    pub(crate) func: Function,
    /// 登记它的块名，重载时按这个清。
    pub(crate) chunk: String,
}

pub(crate) struct Timers {
    start: Instant,
    next_id: u32,
    list: Vec<Timer>,
    /// 因为超上限被拒的登记次数。
    pub(crate) dropped: usize,
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            next_id: 1,
            list: Vec::new(),
            dropped: 0,
        }
    }
}

impl Timers {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn add(&mut self, chunk: &str, ms: u64, repeat: bool, func: Function) -> mlua::Result<u32> {
        if self.list.len() >= MAX_TIMERS {
            self.dropped += 1;
            return Err(mlua::Error::runtime(format!(
                "too many timers (max {MAX_TIMERS})"
            )));
        }
        let step = ms.clamp(MIN_PERIOD_MS, MAX_PERIOD_MS);
        let id = self.next_id;
        // 0 留给「没有这个 id」，所以绕回时跳过。
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let due = self.now_ms().saturating_add(step);
        self.list.push(Timer {
            id,
            due_ms: due,
            period_ms: if repeat { step } else { 0 },
            func,
            chunk: chunk.to_string(),
        });
        Ok(id)
    }

    fn cancel(&mut self, id: u32) -> bool {
        let before = self.list.len();
        self.list.retain(|t| t.id != id);
        self.list.len() != before
    }

    fn next_due(&self) -> Option<u64> {
        self.list.iter().map(|t| t.due_ms).min()
    }

    /// 取走到期的。重复定时器**先推进下一次到期再发**：回调里取消自己、
    /// 或者回调报错，都不会让同一个定时器在下一跳里立刻又发一遍。
    fn take_due(&mut self, now: u64, cap: usize) -> Vec<(u32, Function)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.list.len() && out.len() < cap {
            if self.list[i].due_ms > now {
                i += 1;
                continue;
            }
            if self.list[i].period_ms == 0 {
                let (id, f) = (self.list[i].id, self.list[i].func.clone());
                self.list.remove(i);
                out.push((id, f));
                // 删掉后 i 位置上换了新元素，不能前移。
                continue;
            }
            let t = &mut self.list[i];
            let step = t.period_ms;
            // 卡了很久（比如拖动窗口）不要补发一串，直接对齐到现在之后。
            t.due_ms = if t.due_ms + step <= now {
                now + step
            } else {
                t.due_ms + step
            };
            out.push((t.id, t.func.clone()));
            i += 1;
        }
        out
    }

    fn purge_where(&mut self, pred: impl Fn(&str) -> bool) {
        self.list.retain(|t| !pred(&t.chunk));
    }
}

/// 借一次。借来的守卫活不过一次 Lua 调用，调用方只做纯内存动作。
fn with<R>(lua: &Lua, f: impl FnOnce(&mut Timers) -> R) -> Option<R> {
    let slot = lua.app_data_mut::<RefCell<Timers>>()?;
    Some(f(&mut slot.borrow_mut()))
}

/// 宿主时钟，毫秒。
pub(crate) fn now(lua: &Lua) -> u64 {
    with(lua, |t| t.now_ms()).unwrap_or(0)
}

/// 最早的到期时刻（绝对值），没有定时器就是 `None`。
pub(crate) fn next_due(lua: &Lua) -> Option<u64> {
    with(lua, |t| t.next_due()).flatten()
}

pub(crate) fn len(lua: &Lua) -> usize {
    with(lua, |t| t.list.len()).unwrap_or(0)
}

pub(crate) fn dropped(lua: &Lua) -> usize {
    with(lua, |t| t.dropped).unwrap_or(0)
}

pub(crate) fn add(
    lua: &Lua,
    chunk: &str,
    ms: u64,
    repeat: bool,
    func: Function,
) -> mlua::Result<u32> {
    with(lua, |t| t.add(chunk, ms, repeat, func))
        .unwrap_or_else(|| Err(mlua::Error::runtime("timer table is not initialized")))
}

pub(crate) fn cancel(lua: &Lua, id: u32) -> bool {
    with(lua, |t| t.cancel(id)).unwrap_or(false)
}

pub(crate) fn take_due(lua: &Lua, now: u64, cap: usize) -> Vec<(u32, Function)> {
    with(lua, |t| t.take_due(now, cap)).unwrap_or_default()
}

/// 按谓词清定时器。谓词收的是**块名**（可能带包前缀）。
pub(crate) fn purge_where(lua: &Lua, pred: &impl Fn(&str) -> bool) {
    let _ = with(lua, |t| t.purge_where(pred));
}
