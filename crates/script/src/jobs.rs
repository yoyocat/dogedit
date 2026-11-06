//! 待办队列：回调里 `App.run` / `App.defer` 只入队，入口返回前统一派发。
//!
//! 为什么必须有它：回调执行期间 `EditorState` 已经被 bridge 借走，回调里再去
//! 触发一个内置命令就是重入 —— 轻则改到一半的文档，重则 abort（窗口过程里的
//! panic 不能 unwind）。所以这两个接口**只入队**，由
//! [`crate::ScriptHost::run_queue`] 在「bridge 还活着、回调已经返回」的那个缝隙
//! 里派发。
//!
//! 队列是**进程级一份**，放在 Lua 的 app data 里，所以嵌套调用（命令里再
//! `App.run`）自然接到队尾，按调用顺序跑。
//!
//! 上限有两道：入队时挡住（`while true do App.run(...) end` 一次能灌无数条），
//! 派发时也挡住（命令 A 跑命令 B 再跑命令 A 是另一种死循环）。

use std::cell::RefCell;
use std::collections::VecDeque;

use mlua::{Function, Lua};

/// 一次派发最多跑这么多件。
pub(crate) const MAX_JOBS: usize = 256;

/// 队列里的一件事。
pub(crate) enum Job {
    /// `App.run("file.save")`。
    Command(String),
    /// `App.defer(function() ... end)`。
    Func(Function),
    /// `App.view.scroll_to(line, col)`。写操作必须排队：它改的是
    /// `EditorState`，而回调期间那份状态正被借走。
    Scroll { line: usize, col: usize },
}

#[derive(Default)]
pub(crate) struct Jobs {
    /// `(块名, 工作)`。块名和注册表里那条记录同一个口径（可能带包前缀），
    /// 卸载包时按它把这个包排的队清掉 —— `Job::Func` 手里攥着一个 Lua 闭包，
    /// 不清就收不回去。
    queue: VecDeque<(String, Job)>,
    /// 因为超上限被丢掉的件数。测试和 `--skin-debug` 读它。
    pub(crate) dropped: usize,
}

impl Jobs {
    fn push(&mut self, chunk: String, job: Job) {
        if self.queue.len() >= MAX_JOBS {
            self.dropped += 1;
            return;
        }
        self.queue.push_back((chunk, job));
    }

    pub(crate) fn pop(&mut self) -> Option<Job> {
        self.queue.pop_front().map(|(_, job)| job)
    }

    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }

    pub(crate) fn clear(&mut self) {
        self.queue.clear();
    }

    /// 丢掉某个块排的、还没派发的工作。
    pub(crate) fn purge_where(&mut self, pred: impl Fn(&str) -> bool) {
        self.queue.retain(|(chunk, _)| !pred(chunk));
    }
}

/// 借一次队列。`build_api` 里 `set_app_data` 过一次；拿不到就是编程错误。
///
/// 借来的 `RefMut` 在闭包里活不过一次 Lua 调用 —— 调用方只做入队/出队这种
/// 纯内存动作，真正的派发在闭包外面。
fn with<R>(lua: &Lua, f: impl FnOnce(&mut Jobs) -> R) -> Option<R> {
    let slot = lua.app_data_mut::<RefCell<Jobs>>()?;
    Some(f(&mut slot.borrow_mut()))
}

fn must<R>(lua: &Lua, f: impl FnOnce(&mut Jobs) -> R) -> mlua::Result<R> {
    with(lua, f).ok_or_else(|| mlua::Error::runtime("job queue is not initialized"))
}

/// 谁排的这件工作。拿不到块名（宿主自己调的）当裸脚本，别让它把入队挡住。
fn chunk_of(lua: &Lua) -> String {
    crate::current_chunk(lua).unwrap_or_default()
}

pub(crate) fn push_command(lua: &Lua, name: String) -> mlua::Result<()> {
    let chunk = chunk_of(lua);
    must(lua, |j| j.push(chunk, Job::Command(name)))
}

pub(crate) fn push_func(lua: &Lua, f: Function) -> mlua::Result<()> {
    let chunk = chunk_of(lua);
    must(lua, |j| j.push(chunk, Job::Func(f)))
}

pub(crate) fn push_scroll(lua: &Lua, line: usize, col: usize) -> mlua::Result<()> {
    let chunk = chunk_of(lua);
    must(lua, |j| j.push(chunk, Job::Scroll { line, col }))
}

/// 卸载包 / 热重载一块脚本时，把它还排着的队清掉。
pub(crate) fn purge_where(lua: &Lua, pred: &impl Fn(&str) -> bool) {
    let _ = with(lua, |j| j.purge_where(pred));
}

/// 取一件出来。返回时借已经还了，所以调用方可以安全地跑 Lua。
pub(crate) fn pop(lua: &Lua) -> Option<Job> {
    with(lua, |j| j.pop()).flatten()
}

/// 还排着几件。
pub(crate) fn pending(lua: &Lua) -> usize {
    with(lua, |j| j.len()).unwrap_or(0)
}

/// 丢掉还没跑的（到上限时清场，避免下一帧接着跑同样的死循环）。
pub(crate) fn discard(lua: &Lua) -> usize {
    with(lua, |j| {
        let n = j.len();
        j.clear();
        n
    })
    .unwrap_or(0)
}

/// 因为超上限被丢掉的件数。
pub(crate) fn dropped(lua: &Lua) -> usize {
    with(lua, |j| j.dropped).unwrap_or(0)
}
