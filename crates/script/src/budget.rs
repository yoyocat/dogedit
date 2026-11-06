//! 指令预算：一次脚本调用最多能烧多少条 Lua VM 指令。
//!
//! 沙箱挡得住 `io`/`os`/`package`，挡不住 `while true do end` —— 一个画错的皮肤
//! 足以让 UI 线程再也回不到消息循环，进程看着像死了。Lua 的**计数钩子**是唯一能
//! 从这儿抽身的东西：它每 N 条指令回调一次，回调返回 `Err` 就变成一次普通的 Lua
//! 错误，`f.call()` 照常接住、记日志、交回内置画法。
//!
//! 为什么是「指令数」而不是「毫秒」：指令数是确定的，同一段脚本每次烧掉的格数
//! 一样，测试能断言；读时钟要 `App.now()`，那个一动缓存就废（见 `paint`）。
//!
//! 代价：钩子每 [`STEP`] 条指令响一次，`Continue` 那条路上不分配、不碰 Lua，
//! 一次绘制回调（几千条指令）通常一次都响不到。

use std::cell::Cell;
use std::rc::Rc;

use mlua::{HookTriggers, Lua, VmState};

/// 钩子每这么多条指令响一次。1000 是「够粗，量级上不亏」—— 一个跑满 20 万条
/// 指令的绘制回调只响 200 次。
pub(crate) const STEP: u32 = 1_000;

/// 一次绘制回调的预算（20 万条指令）。实测例子里那套状态栏画法一次不到 1 万条，
/// 留了二十倍余量；真跑满也就几毫秒，够它把 8 ms 的帧预算撞出来。
pub(crate) const PAINT: u32 = 200_000;

/// 一次事件/命令/定时器回调的预算（2000 万条指令）。这一档脚本是「干活」的 ——
/// 扫一个几十万行的文件、拼一张大表都算正常。真死循环也就在一两秒内被掐掉，
/// 而不是永远。
pub(crate) const SCRIPT: u32 = 20_000_000;

/// 计数器和它的钩子。`ScriptHost` 持一个，[`Meter::arm`] 出来的守卫负责恢复。
#[derive(Clone, Default)]
pub(crate) struct Meter {
    /// 还剩多少条指令。
    left: Rc<Cell<u32>>,
    /// 这次调用开始时的预算，以及期间见过的最低水位 —— 「实际烧了多少」由两者
    /// 相减得出。守卫析构后 `left` 已经复原，只有这两个还记得。
    armed: Rc<Cell<u32>>,
    low: Rc<Cell<u32>>,
}

impl Meter {
    /// 装上钩子。整条 VM 只装一次，之后靠 [`arm`](Self::arm) 换预算。
    pub fn install(&self, lua: &Lua) -> mlua::Result<()> {
        self.left.set(SCRIPT);
        self.armed.set(SCRIPT);
        self.low.set(SCRIPT);
        let left = Rc::clone(&self.left);
        let low = Rc::clone(&self.low);
        let armed = Rc::clone(&self.armed);
        lua.set_hook(
            HookTriggers::default().every_nth_instruction(STEP),
            move |_, _| {
                let now = left.get();
                if now <= STEP {
                    // 扣光：抛出去。Lua 会把它当成一次普通错误往上抛。
                    left.set(0);
                    low.set(0);
                    return Err(mlua::Error::RuntimeError(format!(
                        "script exceeded its instruction budget ({} instructions)",
                        armed.get()
                    )));
                }
                let next = now - STEP;
                left.set(next);
                if next < low.get() {
                    low.set(next);
                }
                Ok(VmState::Continue)
            },
        )
    }

    /// 给接下来的这次调用装一份预算，守卫析构时恢复上一个值 —— `run_queue` 里
    /// 跑命令、命令里再 `App.run`，各算各的。
    pub fn arm(&self, instructions: u32) -> Armed {
        let prev = self.left.replace(instructions);
        self.armed.set(instructions);
        self.low.set(instructions);
        Armed {
            meter: self.clone(),
            prev,
        }
    }

    /// 上一次 [`arm`](Self::arm) 之后实际烧掉多少条指令（粗到 [`STEP`]）。
    pub fn used(&self) -> u32 {
        self.armed.get().saturating_sub(self.low.get())
    }
}

/// 一次「装了预算」的租约。忘了绑名字（`let _ = host.arm(..)`）会当场析构，
/// 预算等于没装 —— 所以调用点一律 `let _budget = ...`。
pub(crate) struct Armed {
    meter: Meter,
    prev: u32,
}

impl Drop for Armed {
    fn drop(&mut self) {
        self.meter.left.set(self.prev);
    }
}
