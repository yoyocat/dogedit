//! 注册表：谁登记了什么，以及怎么把一个包登记的东西全拿回去。
//!
//! 脚本能往九张表里塞东西：绘制回调、窗格、状态段、工具条按钮、按下回调、
//! 悬停提示、事件、命令、快捷键，外加定时器。每张表原本只记「块名」（文件名），
//! 于是「拆掉一个包」只能靠逐个文件猜 —— 一个包有四个面文件，就漏三个。
//!
//! 块名现在是**带包前缀**的（见 `crate::qualified`），所以归属不需要新的字段：
//! `pkg_of(chunk)` 就是答案。这个模块只做两件事 —— 数一数，和按包清。
//!
//! ## 为什么数得清很重要
//!
//! 「禁用一个包」的验收线是「和从来没装过一样」。眼睛看不出这件事，得靠数字：
//! 装载前 [`count`] 一次，卸载后再 [`count`] 一次，十项全等才算干净。管理器
//! 里的「这个包占了什么」也是同一份数据。

use std::collections::BTreeMap;

use mlua::{Lua, Table, Value};

use crate::paint::{REG_HINTS, REG_PAINTS, REG_PANES, REG_PRESS, REG_STATUS, REG_TOOLBAR};
use crate::{REG_COMMANDS, REG_EVENTS, REG_KEYS};

/// 九张表各有多少条，加定时器。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub paints: usize,
    pub panes: usize,
    pub status: usize,
    pub toolbar: usize,
    pub press: usize,
    pub hints: usize,
    pub events: usize,
    pub commands: usize,
    pub keys: usize,
    pub timers: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.paints
            + self.panes
            + self.status
            + self.toolbar
            + self.press
            + self.hints
            + self.events
            + self.commands
            + self.keys
            + self.timers
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// 逐项相减，负数夹到 0。用来算「这一次清掉了多少」。
    pub fn removed(before: Counts, after: Counts) -> Counts {
        Counts {
            paints: before.paints.saturating_sub(after.paints),
            panes: before.panes.saturating_sub(after.panes),
            status: before.status.saturating_sub(after.status),
            toolbar: before.toolbar.saturating_sub(after.toolbar),
            press: before.press.saturating_sub(after.press),
            hints: before.hints.saturating_sub(after.hints),
            events: before.events.saturating_sub(after.events),
            commands: before.commands.saturating_sub(after.commands),
            keys: before.keys.saturating_sub(after.keys),
            timers: before.timers.saturating_sub(after.timers),
        }
    }

    /// `paints=3 panes=1 …`，只列非零项。日志和 `--packages` 用它。
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (name, n) in [
            ("paints", self.paints),
            ("panes", self.panes),
            ("status", self.status),
            ("toolbar", self.toolbar),
            ("press", self.press),
            ("hints", self.hints),
            ("events", self.events),
            ("commands", self.commands),
            ("keys", self.keys),
            ("timers", self.timers),
        ] {
            if n > 0 {
                parts.push(format!("{name}={n}"));
            }
        }
        if parts.is_empty() {
            "（空）".to_string()
        } else {
            parts.join(" ")
        }
    }
}

/// 现在一共登记了多少条。
pub fn count(lua: &Lua) -> Counts {
    let mut c = Counts::default();
    each_single(lua, REG_PAINTS, |_| c.paints += 1);
    each_list(lua, REG_PANES, |_| c.panes += 1);
    each_list(lua, REG_STATUS, |_| c.status += 1);
    each_list(lua, REG_TOOLBAR, |_| c.toolbar += 1);
    each_single(lua, REG_PRESS, |_| c.press += 1);
    each_single(lua, REG_HINTS, |_| c.hints += 1);
    each_event(lua, |_| c.events += 1);
    each_single(lua, REG_COMMANDS, |_| c.commands += 1);
    each_single(lua, REG_KEYS, |_| c.keys += 1);
    c.timers = crate::timers::len(lua);
    c
}

/// 按包分组数一遍。空串那一组是裸脚本（`<exe>/scripts/*.lua`、旧式皮肤）。
pub fn owners(lua: &Lua) -> BTreeMap<String, Counts> {
    let mut out: BTreeMap<String, Counts> = BTreeMap::new();
    each_single(lua, REG_PAINTS, |chunk| bump(&mut out, chunk).paints += 1);
    each_list(lua, REG_PANES, |chunk| bump(&mut out, chunk).panes += 1);
    each_list(lua, REG_STATUS, |chunk| bump(&mut out, chunk).status += 1);
    each_list(lua, REG_TOOLBAR, |chunk| bump(&mut out, chunk).toolbar += 1);
    each_single(lua, REG_PRESS, |chunk| bump(&mut out, chunk).press += 1);
    each_single(lua, REG_HINTS, |chunk| bump(&mut out, chunk).hints += 1);
    each_event(lua, |chunk| bump(&mut out, chunk).events += 1);
    each_single(lua, REG_COMMANDS, |chunk| bump(&mut out, chunk).commands += 1);
    each_single(lua, REG_KEYS, |chunk| bump(&mut out, chunk).keys += 1);
    out
}

fn bump<'a>(out: &'a mut BTreeMap<String, Counts>, chunk: &str) -> &'a mut Counts {
    out.entry(crate::pkg_of(chunk).to_string()).or_default()
}

/// 哈希表：一条记录一个键（命令、键、绘制、悬停、按下）。
fn each_single(lua: &Lua, key: &str, mut f: impl FnMut(&str)) {
    let Ok(t) = lua.named_registry_value::<Table>(key) else {
        return;
    };
    for (_, v) in t.pairs::<Value, Value>().flatten() {
        f(&crate::record_chunk(&v));
    }
}

/// 数组表：一条声明一项（窗格、状态段、工具条按钮）。
fn each_list(lua: &Lua, key: &str, mut f: impl FnMut(&str)) {
    let Ok(t) = lua.named_registry_value::<Table>(key) else {
        return;
    };
    for v in t.sequence_values::<Value>().flatten() {
        f(&crate::record_chunk(&v));
    }
}

/// 事件表是「名字 → 记录数组」，要进两层。
fn each_event(lua: &Lua, mut f: impl FnMut(&str)) {
    let Ok(t) = lua.named_registry_value::<Table>(REG_EVENTS) else {
        return;
    };
    for (_, v) in t.pairs::<Value, Value>().flatten() {
        let Value::Table(list) = v else {
            continue;
        };
        for rec in list.sequence_values::<Value>().flatten() {
            f(&crate::record_chunk(&rec));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ScriptHost;

    fn load(host: &ScriptHost, pkg: &str, name: &str, src: &str) {
        let mut bridge = crate::HeadlessBridge;
        host.load_in(pkg, name, src, &mut bridge)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }

    #[test]
    fn count_sees_every_table() {
        let host = ScriptHost::new().expect("host");
        assert!(count(&host.lua).is_empty(), "a fresh host has nothing");
        load(
            &host,
            "acme.clock",
            "paint.lua",
            r#"
App.ui.paint:on("status.pane", function(c, ctx) end)
App.ui.paint:hint("tab", { cursor = "hand" })
App.on("doc.saved", function() end)
App.command("acme.tick", function() end)
App.keymap("Ctrl+F9", "acme.tick")
App.timer(50, function() end)
"#,
        );
        let c = count(&host.lua);
        assert_eq!(c.paints, 1, "{c:?}");
        assert_eq!(c.hints, 1, "{c:?}");
        assert_eq!(c.events, 1, "{c:?}");
        assert_eq!(c.commands, 1, "{c:?}");
        assert_eq!(c.keys, 1, "{c:?}");
        assert_eq!(c.timers, 1, "{c:?}");
        assert_eq!(c.total(), 6);
    }

    #[test]
    fn owners_groups_by_package() {
        let host = ScriptHost::new().expect("host");
        load(
            &host,
            "acme.clock",
            "paint.lua",
            r#"App.command("acme.a", function() end)"#,
        );
        load(
            &host,
            "acme.clock",
            "behavior.lua",
            r#"App.command("acme.b", function() end)"#,
        );
        load(&host, "", "loose.lua", r#"App.command("loose", function() end)"#);
        let map = owners(&host.lua);
        assert_eq!(map.get("acme.clock").map(|c| c.commands), Some(2));
        assert_eq!(map.get("").map(|c| c.commands), Some(1));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn purge_package_removes_all_four_facets_and_leaves_others() {
        let host = ScriptHost::new().expect("host");
        load(
            &host,
            "acme.clock",
            "paint.lua",
            r#"
App.ui.paint:on("status.pane", function(c, ctx) end)
App.ui.paint:hint("tab", { cursor = "hand" })
App.ui.layout:status({ { id = "clock", width = 60 } })
App.ui.layout:toolbar({ icon = 44, command = "acme.tick" })
App.ui.on("tab", "press", function() return false end)
App.on("doc.saved", function() end)
App.command("acme.tick", function() end)
App.keymap("Ctrl+F9", "acme.tick")
App.timer(50, function() end)
"#,
        );
        load(&host, "", "loose.lua", r#"App.command("loose", function() end)"#);
        let before = count(&host.lua);
        assert_eq!(before.commands, 2);

        let removed = host.purge_package("acme.clock");
        assert_eq!(removed.paints, 1, "{removed:?}");
        assert_eq!(removed.panes, 0);
        assert_eq!(removed.status, 1);
        assert_eq!(removed.toolbar, 1);
        assert_eq!(removed.press, 1);
        assert_eq!(removed.hints, 1);
        assert_eq!(removed.events, 1);
        assert_eq!(removed.commands, 1);
        assert_eq!(removed.keys, 1);
        assert_eq!(removed.timers, 1);

        // 裸脚本那份还在：拆包不该误伤别人的登记。
        let after = count(&host.lua);
        assert_eq!(after.commands, 1);
        assert!(after.paints == 0 && after.hints == 0 && after.timers == 0);
        assert_eq!(after.total(), 1);
    }

    #[test]
    fn reloading_a_package_twice_does_not_grow() {
        let host = ScriptHost::new().expect("host");
        let paint = r#"App.ui.paint:on("status.pane", function(c, ctx) end)"#;
        let behavior = r#"
App.command("acme.tick", function() end)
App.keymap("Ctrl+F9", "acme.tick")
"#;
        // 热重载三次：同一个包的两个面文件各跑三遍，条数必须回到同一组。
        for _ in 0..3 {
            load(&host, "acme.clock", "paint.lua", paint);
            load(&host, "acme.clock", "behavior.lua", behavior);
        }
        let c = count(&host.lua);
        assert_eq!(c.paints, 1, "{c:?}");
        assert_eq!(c.commands, 1, "{c:?}");
        assert_eq!(c.keys, 1, "{c:?}");
        assert_eq!(c.total(), 3, "{c:?}");
    }

    #[test]
    fn purge_package_twice_is_a_no_op() {
        let host = ScriptHost::new().expect("host");
        load(
            &host,
            "acme.clock",
            "paint.lua",
            r#"App.command("acme.tick", function() end)"#,
        );
        assert_eq!(host.purge_package("acme.clock").commands, 1);
        assert!(host.purge_package("acme.clock").is_empty());
        assert!(count(&host.lua).is_empty());
    }

    /// 「关掉游戏之后不占内存」的硬指标：注册表清干净之外，Lua 堆也得真的
    /// 降回去。这里让脚本攥住一张 ~1 MB 的表，卸载后看 `memory().0`。
    #[test]
    fn purge_package_returns_the_lua_heap_not_just_the_registry() {
        let host = ScriptHost::new().expect("host");
        let base = host.memory().0;
        load(
            &host,
            "demo.snake",
            "snake.lua",
            r#"
local data = {}
for i = 1, 20000 do data[i] = string.rep("x", 48) end
App.command("demo.tick", function() return #data end)
App.timer(100, function() return #data end, true)
"#,
        );
        let loaded = host.memory().0;
        assert!(loaded > base + 500_000, "测试得先真的占住内存：{base} → {loaded}");

        let removed = host.purge_package("demo.snake");
        assert_eq!(removed.commands, 1);
        assert_eq!(removed.timers, 1);
        assert!(count(&host.lua).is_empty(), "注册一条都不该剩");

        let after = host.memory().0;
        assert!(
            after < loaded - 400_000,
            "卸载后 Lua 堆没还回来：{loaded} → {after}"
        );
    }

    /// 待办队列里的 `App.run` 也归这个包：卸载时一并摘掉，别在下次入口
    /// 让一个已经退出的脚本再跑一发。
    #[test]
    fn purge_package_drops_its_queued_jobs_but_not_others() {
        let host = ScriptHost::new().expect("host");
        load(&host, "demo.a", "a.lua", r#"App.run("file.save")"#);
        load(&host, "demo.b", "b.lua", r#"App.run("file.open")"#);
        assert_eq!(host.pending_jobs(), 2);
        host.purge_package("demo.a");
        assert_eq!(host.pending_jobs(), 1, "只该留 demo.b 那一件");
        let mut bridge = crate::HeadlessBridge;
        assert_eq!(host.run_queue(&mut bridge), 1);
    }

    #[test]
    fn command_names_excluding_hides_only_the_named_packages() {
        let host = ScriptHost::new().expect("host");
        load(
            &host,
            "demo.snake",
            "s.lua",
            r#"App.command("snake.up", function() end) App.command("snake.restart", function() end)"#,
        );
        load(
            &host,
            "demo.clock",
            "c.lua",
            r#"App.command("clock.tick", function() end)"#,
        );
        load(&host, "", "loose.lua", r#"App.command("loose.go", function() end)"#);
        assert_eq!(host.command_names().len(), 4);

        let hidden = ["demo.snake".to_string()];
        assert_eq!(
            host.command_names_excluding(&hidden),
            vec!["clock.tick", "loose.go"],
            "裸脚本没有包名，不该被任何包名单误伤"
        );
        // 排除只影响菜单，命令本身还在。
        assert_eq!(host.command_names().len(), 4);
    }

    #[test]
    fn sole_pane_package_is_some_only_for_a_packages_last_pane() {
        let host = ScriptHost::new().expect("host");
        load(
            &host,
            "demo.game",
            "board.lua",
            r#"App.ui.layout:pane{ id = "board", side = "right" }"#,
        );
        load(
            &host,
            "demo.other",
            "o.lua",
            r#"App.ui.layout:pane{ id = "other", side = "left" }"#,
        );
        load(
            &host,
            "",
            "loose.lua",
            r#"App.ui.layout:pane{ id = "loose", side = "left" }"#,
        );
        assert_eq!(host.sole_pane_package("board").as_deref(), Some("demo.game"));
        assert_eq!(host.sole_pane_package("other").as_deref(), Some("demo.other"));
        assert_eq!(host.sole_pane_package("loose"), None, "裸脚本没有整包卸载");
        assert_eq!(host.sole_pane_package("missing"), None);

        // 同一个包又长了一块：关 board 不该把 hud 一起带走。
        load(
            &host,
            "demo.game",
            "hud.lua",
            r#"App.ui.layout:pane{ id = "hud", side = "right" }"#,
        );
        assert_eq!(host.sole_pane_package("board"), None);
        assert_eq!(host.sole_pane_package("hud"), None);

        host.purge_package("demo.game");
        assert_eq!(host.sole_pane_package("board"), None, "卸载后窗格声明也没了");
    }
}
