//! dg-script 的引擎级测试:一个内存 [`Bridge`](dg_script::Bridge)
//! 模拟文档模型,验证 M1(alert/log)和 M2(doc/sel 编辑 API)
//! 的行为,包括 UTF-8 字节列语义。

use dg_script::{
    Bridge, DocMeta, Pos, ScriptHost, TaskbarState, ViewInfo, byte_col_to_char_col,
    char_col_to_byte_col,
};

/// 行数组的内存文档模型。编辑先把 (行, 字节列) 折算成拼接文本上
/// 的字符偏移,再拼接回去 —— 和真实 Bridge 一样的两步换算,所以
/// 测的字节列语义对两个实现都成立。
struct MockDoc {
    lines: Vec<String>,
    selection: Vec<(Pos, Pos)>,
    alerts: Vec<String>,
    logs: Vec<String>,
    /// 只读开关:所有编辑返回 false。
    read_only: bool,
    /// `Bridge::run_command` 收到的内置命令名,按调用顺序。
    commands_run: Vec<String>,
    /// `App.doc.meta()` 的答案,测试自己填。
    meta: DocMeta,
    /// `App.view.info()` 的答案。
    view: ViewInfo,
    /// `App.view.scroll_to` 收到的 (行, 列),按顺序。
    scrolls: Vec<(usize, usize)>,
    /// `App.taskbar` 收到的 (状态, 完成量, 总量),按顺序。
    taskbar_calls: Vec<(TaskbarState, u64, u64)>,
}

impl MockDoc {
    fn new(text: &str) -> Self {
        Self {
            lines: text.split('\n').map(str::to_string).collect(),
            selection: Vec::new(),
            alerts: Vec::new(),
            logs: Vec::new(),
            read_only: false,
            commands_run: Vec::new(),
            meta: DocMeta::default(),
            view: ViewInfo::default(),
            scrolls: Vec::new(),
            taskbar_calls: Vec::new(),
        }
    }

    fn joined(&self) -> String {
        self.lines.join("\n")
    }

    /// (行, 字节列) → 拼接文本上的字符偏移。
    fn flat(&self, p: Pos) -> Option<usize> {
        let (line, byte_col) = p;
        if line == 0 || line > self.lines.len() {
            return None;
        }
        let t = &self.lines[line - 1];
        let char_col = byte_col_to_char_col(t, byte_col);
        let base: usize = self.lines[..line - 1]
            .iter()
            .map(|l| l.chars().count() + 1)
            .sum();
        Some(base + char_col)
    }

    fn flat_span(&self, s: Pos, e: Pos) -> Option<(usize, usize)> {
        let a = self.flat(s)?;
        let b = self.flat(e)?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    fn apply_flat(&mut self, start: usize, end: usize, text: &str) {
        let mut joined = self.joined();
        let b = |off: usize| -> usize {
            joined.char_indices().nth(off).map_or(joined.len(), |(i, _)| i)
        };
        let (bs, be) = (b(start), b(end));
        joined.replace_range(bs..be, text);
        self.lines = joined.split('\n').map(str::to_string).collect();
    }
}

impl Bridge for MockDoc {
    fn doc_meta(&self) -> DocMeta {
        self.meta.clone()
    }
    fn view_info(&self) -> ViewInfo {
        self.view
    }
    fn scroll_to(&mut self, line: usize, col: usize) -> bool {
        self.scrolls.push((line, col));
        true
    }
    fn taskbar_progress(&mut self, state: TaskbarState, completed: u64, total: u64) -> bool {
        self.taskbar_calls.push((state, completed, total));
        true
    }
    fn alert(&mut self, msg: &str) {
        self.alerts.push(msg.to_string());
    }
    fn log(&mut self, msg: &str) {
        self.logs.push(msg.to_string());
    }
    fn doc_name(&self) -> String {
        "mock.txt".into()
    }
    fn doc_path(&self) -> Option<String> {
        Some(r"C:\mock\mock.txt".into())
    }
    fn line_count(&self) -> usize {
        self.lines.len()
    }
    fn line_text(&self, line: usize) -> Option<String> {
        self.lines.get(line.checked_sub(1)?).cloned()
    }
    fn doc_text(&self) -> String {
        self.joined()
    }
    fn span_text(&self, s: Pos, e: Pos) -> Option<String> {
        let (a, b) = self.flat_span(s, e)?;
        let joined = self.joined();
        let bo = |off: usize| -> usize {
            joined.char_indices().nth(off).map_or(joined.len(), |(i, _)| i)
        };
        Some(joined[bo(a)..bo(b)].to_string())
    }
    fn insert_at(&mut self, p: Pos, text: &str) -> bool {
        if self.read_only {
            return false;
        }
        match self.flat(p) {
            Some(off) => {
                self.apply_flat(off, off, text);
                true
            }
            None => false,
        }
    }
    fn replace_span(&mut self, s: Pos, e: Pos, text: &str) -> bool {
        if self.read_only {
            return false;
        }
        match self.flat_span(s, e) {
            Some((a, b)) => {
                self.apply_flat(a, b, text);
                true
            }
            None => false,
        }
    }
    fn sel_count(&self) -> usize {
        self.selection.len()
    }
    fn sel_range(&self, index: usize) -> Option<(Pos, Pos)> {
        self.selection.get(index).copied()
    }
    fn sel_text(&self) -> String {
        match self.selection.first() {
            Some(&(s, e)) => self.span_text(s, e).unwrap_or_default(),
            None => String::new(),
        }
    }
    fn sel_set(&mut self, s: Pos, e: Pos) {
        self.selection = vec![(s, e)];
    }
    fn run_command(&mut self, name: &str) -> bool {
        // 只有"内置命令"这个命名空间里的名字才算数 —— 真实的宿主也是这么做的
        // (`Command::parse` 认不出就返回 false)。
        let known = name.starts_with("file.") || name.starts_with("edit.") || name.starts_with("cm_");
        if known {
            self.commands_run.push(name.to_string());
        }
        known
    }
}

fn run(src: &str, bridge: &mut MockDoc) -> Result<(), String> {
    ScriptHost::new()
        .and_then(|h| h.run("test.lua", src, bridge))
        .map_err(|e| e.0)
}

#[test]
fn m3_events_fire_in_order_and_survive_errors() {
    let mut m = MockDoc::new("");
    let host = ScriptHost::new().unwrap();
    host.load(
        "test.lua",
        r#"
        App.on("DocSaved", function() App.log("first") end)
        App.on("DocSaved", function() App.doc.nope() end)  -- fails
        App.on("DocSaved", function() App.log("third") end)
        "#,
        &mut m,
    )
    .unwrap();
    host.trigger("DocSaved", &mut m);
    // 第三个 handler 在第二个失败后仍然执行;失败被记入日志。
    assert!(m.logs.iter().any(|l| l == "first"));
    assert!(m.logs.iter().any(|l| l == "third"));
    assert!(m
        .logs
        .iter()
        .any(|l| l.starts_with("DocSaved handler:")));
    // 未注册的事件:无操作。
    host.trigger("Nope", &mut m);
    assert_eq!(m.alerts.len(), 0);
}

#[test]
fn m3_commands_register_and_call() {
    let mut m = MockDoc::new("x");
    let host = ScriptHost::new().unwrap();
    host.load(
        "test.lua",
        r#"
        App.command("stamp", function() App.doc.insert(1, 1, "M3") end)
        App.command("boom", function() error("kaboom") end)
        "#,
        &mut m,
    )
    .unwrap();
    assert_eq!(host.command_names(), ["boom", "stamp"]);
    assert!(host.call_command("stamp", &mut m));
    assert_eq!(m.lines, ["M3x"]);
    // 失败的命令不向上抛,记日志并返回 false。
    assert!(!host.call_command("boom", &mut m));
    assert!(m.logs.iter().any(|l| l.contains("kaboom")));
    assert!(!host.call_command("missing", &mut m));
}

/// 命令里死循环也一样被掐掉：`call_command` 返回 false，日志里说清楚是预算。
/// 这一档的预算比绘制档宽得多（20 000 000 条指令），但仍然是有限的。
#[test]
fn a_runaway_command_is_cut_off() {
    let mut m = MockDoc::new("x");
    let host = ScriptHost::new().unwrap();
    host.load(
        "test.lua",
        r#"
        App.command("spin", function() while true do end end)
        "#,
        &mut m,
    )
    .unwrap();
    assert!(!host.call_command("spin", &mut m), "a runaway command fails");
    assert!(
        m.logs.iter().any(|l| l.contains("instruction budget")),
        "the log says why: {:?}",
        m.logs
    );
    // 预算按次算：掐掉一次之后，下一条命令照样能跑。
    host.load("test2.lua", r#"App.command("ok", function() App.doc.insert(1, 1, "!") end)"#, &mut m)
        .unwrap();
    assert!(host.call_command("ok", &mut m));
    assert_eq!(m.lines, ["!x"]);
}

#[test]
fn m3_gesture_parse_and_lookup() {
    use dg_script::parse_gesture;
    let g = parse_gesture("Ctrl+Shift+T").unwrap();
    assert!(g.ctrl && g.shift && !g.alt && g.vk == b'T' as u32);
    assert_eq!(parse_gesture("alt+f4").unwrap().vk, 0x73);
    assert_eq!(parse_gesture("9").unwrap().vk, b'9' as u32);
    assert!(parse_gesture("C+K,R").is_none());
    assert!(parse_gesture("").is_none());
    assert!(parse_gesture("Ctrl+Foo").is_none());
    assert!(parse_gesture("Ctrl+F13").is_none());
    // 具名键：脚本窗格做交互要用方向键和空格，字母/数字表不够用。
    assert_eq!(parse_gesture("up").unwrap().vk, 0x26);
    assert_eq!(parse_gesture("Ctrl+Shift+Left").unwrap().vk, 0x25);
    assert_eq!(parse_gesture("SPACE").unwrap().vk, 0x20);
    assert_eq!(parse_gesture("Esc").unwrap().vk, 0x1B);
    assert_eq!(parse_gesture("escape").unwrap().vk, 0x1B);
    assert_eq!(parse_gesture("PgDn").unwrap().vk, 0x22);
    assert!(parse_gesture("Alt+Up").unwrap().alt);
    assert!(parse_gesture("Upwards").is_none());

    let mut m = MockDoc::new("");
    let host = ScriptHost::new().unwrap();
    host.load(
        "test.lua",
        r#"
        App.keymap("Ctrl+Shift+T", "stamp")
        App.command("stamp", function() App.log("via key") end)
        "#,
        &mut m,
    )
    .unwrap();
    assert_eq!(host.lookup_key(true, true, false, b'T' as u32).as_deref(), Some("stamp"));
    assert_eq!(host.lookup_key(false, true, false, b'T' as u32), None);
    host.call_command("stamp", &mut m);
    assert_eq!(m.logs, ["via key"]);
    // 非法手势在注册时就报错。
    let mut m2 = MockDoc::new("");
    let h2 = ScriptHost::new().unwrap();
    assert!(h2.load("bad.lua", r#"App.keymap("C+K,R", "x")"#, &mut m2).is_err());
}

#[test]
fn m4_storage_persists_across_hosts() {
    let dir = std::env::temp_dir().join(format!("evscript-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut m = MockDoc::new("");
    let mut host = ScriptHost::new().unwrap();
    host.set_storage_dir(&dir);
    host.load(
        "test.lua",
        r#"
        local t = App.storage("prefs")
        t:set("last_doc", "mock.txt")
        t:set("k1", "v1")
        "#,
        &mut m,
    )
    .unwrap();

    // 新引擎、同目录:set 的内容还在,prefix 枚举与计数可用,del 生效。
    let mut m2 = MockDoc::new("");
    let mut host2 = ScriptHost::new().unwrap();
    host2.set_storage_dir(&dir);
    host2.load(
        "test2.lua",
        r#"
        local t = App.storage("prefs")
        assert(t:get("last_doc") == "mock.txt")
        assert(t:count() == 2)
        assert(t:keys("k")[1] == "k1")
        t:del("k1")
        assert(t:count() == 1)
        "#,
        &mut m2,
    )
    .unwrap();
    let json = std::fs::read_to_string(dir.join("storage.json")).unwrap();
    assert!(json.contains(r#""prefs":{"last_doc":"mock.txt"}"#), "{json}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn m4_prompt_routes_through_bridge() {
    let mut m = MockDoc::new("");
    let host = ScriptHost::new().unwrap();
    host.load(
        "test.lua",
        r#"
        name = App.prompt("your name?", "default")
        "#,
        &mut m,
    )
    .unwrap();
    // MockDoc 不实现 prompt,默认无操作 → nil。
    host.trigger("__noop", &mut m);
}

#[test]
fn registry_hidden_from_scripts() {
    let mut m = MockDoc::new("");
    let host = ScriptHost::new().unwrap();
    host.load(
        "test.lua",
        r#"
        assert(__events == nil and __commands == nil and __keys == nil)
        "#,
        &mut m,
    )
    .unwrap();
}

#[test]
fn load_failure_leaves_host_usable() {
    let mut m = MockDoc::new("");
    let host = ScriptHost::new().unwrap();
    assert!(host.load("bad.lua", "App.doc.nope()", &mut m).is_err());
    // 引擎还活着:后续 load 照常。
    host.load("good.lua", "App.log('still alive')", &mut m).unwrap();
    assert_eq!(m.logs.last().map(String::as_str), Some("still alive"));
}

/// 随包示例脚本（文档 script_api 的入门两例），引擎级测试直接拿它们当素材。
const HELLO_LUA: &str = include_str!("scripts/hello.lua");
const TRIM_TRAILING_LUA: &str = include_str!("scripts/trim_trailing.lua");

#[test]
fn m1_hello_runs() {
    let mut m = MockDoc::new("one\ntwo\n");
    run(HELLO_LUA, &mut m).unwrap();
    assert_eq!(m.alerts.len(), 1);
    assert!(m.alerts[0].contains("hello from dg-script"));
    assert!(m.alerts[0].contains("Lua 5"));
    assert_eq!(m.logs.last().map(String::as_str), Some("hello.lua finished"));
}

#[test]
fn m2_trim_trailing() {
    let mut m = MockDoc::new("foo  \nbar\n  baz\t\n");
    run(TRIM_TRAILING_LUA, &mut m).unwrap();
    assert_eq!(m.lines, ["foo", "bar", "  baz", ""]);
    assert_eq!(m.logs.len(), 1);
}

#[test]
fn m2_trim_trailing_multibyte() {
    // 字节列必须换算成字符列:你好 占 6 字节,行尾两个空格在字节 7..8。
    let mut m = MockDoc::new("你好  \nx\t\n");
    run(TRIM_TRAILING_LUA, &mut m).unwrap();
    assert_eq!(m.lines, ["你好", "x", ""]);
}

#[test]
fn m2_whitespace_only_line_trims_to_empty() {
    let mut m = MockDoc::new("  \nkeep\n");
    run(TRIM_TRAILING_LUA, &mut m).unwrap();
    assert_eq!(m.lines, ["", "keep", ""]);
}

#[test]
fn m2_doc_reads_and_spans() {
    let mut m = MockDoc::new("hello 世界\nsecond");
    run(
        r#"
        assert(App.doc.name() == "mock.txt")
        assert(App.doc.path() ~= nil)
        assert(App.doc.line_count() == 2)
        assert(App.doc.line(1) == "hello 世界")
        assert(App.doc.line(3) == nil)
        assert(App.doc.line_len(1) == #"hello 世界")
        assert(App.doc.text() == "hello 世界\nsecond")
        assert(App.doc.span_text(1, 7, 2, 3) == "世界\nse")
        assert(App.version() ~= nil)
        "#,
        &mut m,
    )
    .unwrap();
}

#[test]
fn m2_insert_and_delete() {
    let mut m = MockDoc::new("ab\ncd");
    run(
        r#"
        assert(App.doc.insert(1, 2, "X") == true)
        assert(App.doc.insert(0, 1, "Y") == false)
        assert(App.doc.line(1) == "aXb")
        App.doc.delete(2, 1, 2, 3)
        "#,
        &mut m,
    )
    .unwrap();
    assert_eq!(m.lines, ["aXb", ""]);
}

#[test]
fn m2_selection_roundtrip() {
    let mut m = MockDoc::new("abc\ndefg");
    run(
        r#"
        assert(App.sel.count() == 0)
        App.sel.set(1, 2, 2, 3)
        assert(App.sel.count() == 1)
        local s1, c1, e1, c2 = App.sel.get()
        assert(s1 == 1 and c1 == 2 and e1 == 2 and c2 == 3)
        assert(App.sel.text() == "bc\nde")
        "#,
        &mut m,
    )
    .unwrap();
}

#[test]
fn sandbox_hides_io_os() {
    let mut m = MockDoc::new("");
    let err = run("return os.time()", &mut m).unwrap_err();
    assert!(err.contains("test.lua"), "error names the chunk: {err}");
    let mut m = MockDoc::new("");
    assert!(run("io.write('x')", &mut m).is_err());
    let mut m = MockDoc::new("");
    // 标准库还在(通过 _ENV 的 __index 回落到 globals)。
    run(r#"assert(("a b"):find(" ") == 2)"#, &mut m).unwrap();
}

#[test]
fn runtime_error_reports_chunk_and_line() {
    let mut m = MockDoc::new("");
    let err = run("App.doc.nope()", &mut m).unwrap_err();
    assert!(err.contains("test.lua"), "error text: {err}");
}

#[test]
fn print_routes_to_log() {
    let mut m = MockDoc::new("");
    run("print(\"routed\")", &mut m).unwrap();
    assert_eq!(m.logs, ["routed"]);
}

#[test]
fn column_conversions() {
    let line = "你好x";
    // 字节列 1 → 字符 0;字节列 4(好 的开头)→ 字符 1;
    // 字节列 7(x 的开头)→ 字符 2;字节列 99 → 行尾 3。
    assert_eq!(byte_col_to_char_col(line, 1), 0);
    assert_eq!(byte_col_to_char_col(line, 4), 1);
    assert_eq!(byte_col_to_char_col(line, 7), 2);
    assert_eq!(byte_col_to_char_col(line, 99), 3);
    // 字符 0 → 字节 1;字符 2(x)→ 字节 7;越界 → len+1。
    assert_eq!(char_col_to_byte_col(line, 0), 1);
    assert_eq!(char_col_to_byte_col(line, 2), 7);
    assert_eq!(char_col_to_byte_col(line, 9), line.len() + 1);
}

// ---------------------------------------------------------------- M2 引擎扩容

/// `App.run` 只入队：回调返回前一件都不跑，`run_queue` 按入队顺序派发。
#[test]
fn m5_app_run_is_queued_until_the_entry_point_returns() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        App.command("a", function() print("a") end)
        App.command("b", function() print("b") end)
        App.on("go", function()
            print("before")
            App.run("a")
            App.run("b")
            print("after")
        end)
        "#,
        &mut m,
    )
    .unwrap();
    h.trigger("go", &mut m);
    assert_eq!(m.logs, ["before", "after"], "queued, not run inline");
    assert_eq!(h.pending_jobs(), 2);

    assert_eq!(h.run_queue(&mut m), 2);
    assert_eq!(m.logs, ["before", "after", "a", "b"]);
    assert_eq!(h.pending_jobs(), 0);
}

/// 命令里再 `App.run`：接到队尾，按调用顺序继续跑，不递归。
#[test]
fn m5_a_command_that_runs_another_appends_to_the_queue() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        App.command("a", function() print("a"); App.run("b") end)
        App.command("b", function() print("b") end)
        App.on("go", function() App.run("a") end)
        "#,
        &mut m,
    )
    .unwrap();
    h.trigger("go", &mut m);
    h.run_queue(&mut m);
    assert_eq!(m.logs, ["a", "b"]);
}

/// 自己调自己：到上限就整队丢掉，下一帧不会接着跑同样的死循环。
#[test]
fn m5_a_self_running_command_is_capped_and_the_queue_is_cleared() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        App.command("loop", function() App.run("loop") end)
        App.on("go", function() App.run("loop") end)
        "#,
        &mut m,
    )
    .unwrap();
    h.trigger("go", &mut m);
    assert_eq!(h.run_queue(&mut m), 256);
    assert_eq!(h.pending_jobs(), 0, "the rest was dropped, not deferred");
    assert_eq!(h.dropped_jobs(), 0, "the entry cap never had to bite");
}

/// `App.defer` 排的是闭包，跑在入口返回之后，捕获的变量还在。
#[test]
fn m5_app_defer_runs_after_the_entry_point() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        App.on("go", function()
            local n = 41
            App.defer(function() print("deferred " .. (n + 1)) end)
            print("now")
        end)
        "#,
        &mut m,
    )
    .unwrap();
    h.trigger("go", &mut m);
    assert_eq!(m.logs, ["now"]);
    h.run_queue(&mut m);
    assert_eq!(m.logs, ["now", "deferred 42"]);
}

/// 同一个 chunk 重载三次，`doc.saved` 仍然只收到一次 —— 注册按 chunk 名替换。
#[test]
fn m5_reloading_a_chunk_replaces_its_registrations() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    for _ in 0..3 {
        h.load(
            "t.lua",
            r#"App.on("doc.saved", function() print("saved") end)"#,
            &mut m,
        )
        .unwrap();
    }
    h.trigger("doc.saved", &mut m);
    assert_eq!(m.logs, ["saved"]);
}

/// 不同的 chunk 各有各的注册，重载一个不动另一个。
#[test]
fn m5_two_chunks_keep_their_own_handlers() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load("a.lua", r#"App.on("go", function() print("a") end)"#, &mut m)
        .unwrap();
    h.load("b.lua", r#"App.on("go", function() print("b") end)"#, &mut m)
        .unwrap();
    h.trigger("go", &mut m);
    assert_eq!(m.logs, ["a", "b"]);

    // 重载 a 只换 a 那一份。顺序按"最近一次装载"排 —— 先清掉旧的，再追加，
    // 所以 a 挪到了 b 后面。handler 之间不该有顺序依赖。
    m.logs.clear();
    h.load("a.lua", r#"App.on("go", function() print("a2") end)"#, &mut m)
        .unwrap();
    h.trigger("go", &mut m);
    assert_eq!(m.logs, ["b", "a2"]);
}

/// 重载还要清掉命令和快捷键，否则旧命令会一直挂着。
#[test]
fn m5_reloading_also_drops_commands_and_keymaps() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        App.command("old", function() print("old") end)
        App.keymap("Ctrl+K", "old")
        "#,
        &mut m,
    )
    .unwrap();
    assert_eq!(h.command_names(), ["old"]);
    assert_eq!(h.key_bindings().len(), 1);

    h.load(
        "t.lua",
        r#"App.command("new", function() print("new") end)"#,
        &mut m,
    )
    .unwrap();
    assert_eq!(h.command_names(), ["new"]);
    assert!(h.key_bindings().is_empty(), "the old binding is gone");
}

/// 没有同名脚本命令时落到宿主的**内置**命令表 —— `App.run("file.save")`。
#[test]
fn m5_app_run_falls_back_to_the_shell_commands() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        App.on("go", function()
            App.run("file.save")
            App.run("edit.undo")
        end)
        "#,
        &mut m,
    )
    .unwrap();
    h.trigger("go", &mut m);
    h.run_queue(&mut m);
    assert_eq!(m.commands_run, ["file.save", "edit.undo"]);
}

/// 脚本命令优先于内置命令：同名时跑脚本那个。
#[test]
fn m5_a_script_command_shadows_a_builtin_one() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        App.command("file.save", function() print("mine") end)
        App.on("go", function() App.run("file.save") end)
        "#,
        &mut m,
    )
    .unwrap();
    h.trigger("go", &mut m);
    h.run_queue(&mut m);
    assert_eq!(m.logs, ["mine"]);
    assert!(m.commands_run.is_empty(), "the built-in one never ran");
}

/// 事件名新旧互为别名：写哪个都能收到。
#[test]
fn m5_event_names_alias_both_ways() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"App.on("doc.saved", function() print("new") end)"#,
        &mut m,
    )
    .unwrap();
    h.trigger("DocSaved", &mut m);
    assert_eq!(m.logs, ["new"]);

    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"App.on("DocOpened", function() print("old") end)"#,
        &mut m,
    )
    .unwrap();
    h.trigger("doc.opened", &mut m);
    assert_eq!(m.logs, ["old"]);
}

/// 一个事件名没有别名时只触发一次。
#[test]
fn m5_an_unknown_event_name_fires_once() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"App.on("ui.status.pane.click", function() print("x") end)"#,
        &mut m,
    )
    .unwrap();
    h.trigger("ui.status.pane.click", &mut m);
    assert_eq!(m.logs, ["x"]);
}

/// 内存上限装上了，超了报错而不是把进程吃光。
#[test]
fn m5_the_memory_limit_is_enforced() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    let (used, limit) = h.memory();
    assert_eq!(limit, ScriptHost::MEMORY_LIMIT);
    assert!(used > 0 && used < limit, "used {used} of {limit}");

    let err = h
        .load(
            "hog.lua",
            r#"
            local acc = {}
            local chunk = string.rep("x", 1024 * 1024)
            for i = 1, 200 do acc[i] = chunk .. i end
            "#,
            &mut m,
        )
        .unwrap_err();
    assert!(
        err.0.to_lowercase().contains("memory"),
        "expected a memory error, got {err}"
    );
    // host 还能继续用。
    h.load("ok.lua", r#"print("alive")"#, &mut m).unwrap();
    assert_eq!(m.logs.last().map(String::as_str), Some("alive"));
}

// ---- M2b: 时钟与定时器 ----

/// `App.now()` 是宿主时钟：单调、毫秒、从 0 附近起步（沙箱里没有 `os`，
/// 这是脚本能拿到的唯一时间）。
#[test]
fn m5_app_now_is_a_monotonic_millisecond_clock() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        local a = App.now()
        local b = App.now()
        print(a .. " " .. b)
        print(type(a))
        print(tostring(b >= a))
        "#,
        &mut m,
    )
    .unwrap();
    let parts: Vec<&str> = m.logs[0].split(' ').collect();
    let a: u64 = parts[0].parse().expect("App.now() should be a whole number");
    let b: u64 = parts[1].parse().unwrap();
    assert!(a <= b, "monotonic: {a} then {b}");
    assert!(b < 5_000, "the clock starts near zero, got {b} ms");
    assert_eq!(m.logs[1], "number");
    assert_eq!(m.logs[2], "true");
}

/// 一次性定时器：到点前不跑，到点跑一次，之后连 `next_timer_due` 都没有了。
#[test]
fn m5_a_one_shot_timer_fires_exactly_once() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"App.timer(20, function() print("tick") end)"#,
        &mut m,
    )
    .unwrap();
    assert_eq!(h.timer_count(), 1);
    assert_eq!(h.run_timers(&mut m), 0, "not due yet");
    assert!(h.next_timer_due().is_some());

    std::thread::sleep(std::time::Duration::from_millis(40));
    assert_eq!(h.run_timers(&mut m), 1);
    assert_eq!(m.logs, ["tick"]);
    assert_eq!(h.timer_count(), 0);
    assert!(h.next_timer_due().is_none());
    assert_eq!(h.run_timers(&mut m), 0, "nothing left to fire");
}

/// 重复定时器每轮到点就发；回调里 `App.cancel` 自己一定生效（下一次到期在
/// 调用前就推进了，所以不会在回调返回后又补一发）。
#[test]
fn m5_a_repeating_timer_keeps_firing_until_cancelled() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        local n = 0
        id = App.timer(10, function()
            n = n + 1
            print("tick " .. n)
            if n == 3 then App.cancel(id) end
        end, true)
        print("registered " .. tostring(id > 0))
        "#,
        &mut m,
    )
    .unwrap();
    assert_eq!(m.logs, ["registered true"]);
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.run_timers(&mut m);
    }
    assert_eq!(
        m.logs,
        ["registered true", "tick 1", "tick 2", "tick 3"],
        "the cancel in tick 3 must stop the rest"
    );
    assert_eq!(h.timer_count(), 0);
}

/// 报错的定时器当场注销，而且**只记一行** —— 一个每跳都报错的皮肤不该刷满
/// 日志，它也不会自己好起来。
#[test]
fn m5_a_failing_timer_is_dropped_after_one_error() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"App.timer(5, function() error("boom") end, true)"#,
        &mut m,
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(h.run_timers(&mut m), 0, "a failed fire does not count as run");
    assert_eq!(h.timer_count(), 0, "dropped, not retried");
    let boom: Vec<&String> = m.logs.iter().filter(|l| l.contains("boom")).collect();
    assert_eq!(boom.len(), 1, "logged once, got {:?}", m.logs);
    // 再等一轮也不会有第二行。
    std::thread::sleep(std::time::Duration::from_millis(20));
    h.run_timers(&mut m);
    assert_eq!(m.logs.len(), 1);
}

/// 重载同一个 chunk 会连同它的定时器一起换掉：改三次皮肤不该攒下三组定时器。
#[test]
fn m5_reloading_a_chunk_drops_its_timers() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    for _ in 0..3 {
        h.load(
            "t.lua",
            r#"App.timer(60000, function() print("never") end, true)"#,
            &mut m,
        )
        .unwrap();
        assert_eq!(h.timer_count(), 1, "each reload replaces, not appends");
    }
}

/// 参数挡住明显的错：0 或超过一天的间隔报错，而且不留下定时器。
#[test]
fn m5_timer_rejects_a_bad_interval() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    let e = h
        .load("t.lua", r#"App.timer(0, function() end)"#, &mut m)
        .unwrap_err();
    assert!(format!("{e}").contains("interval"), "{e}");
    let e = h
        .load("t.lua", r#"App.timer(86400001, function() end)"#, &mut m)
        .unwrap_err();
    assert!(format!("{e}").contains("interval"), "{e}");
    assert_eq!(h.timer_count(), 0);
}

/// `App.cancel` 对未知 id 只是 false，不报错。
#[test]
fn m5_cancel_of_an_unknown_id_is_false() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        print(tostring(App.cancel(9999)))
        local id = App.timer(60000, function() end)
        print(tostring(App.cancel(id)))
        print(tostring(App.cancel(id)))
        "#,
        &mut m,
    )
    .unwrap();
    assert_eq!(m.logs, ["false", "true", "false"]);
    assert_eq!(h.timer_count(), 0);
}

// ---- M2b: 元信息与视口 ----

/// `App.doc.meta()` 把 bridge 给的九个字段原样摊开；`App.view.info()` 六个。
/// 两张表都是**复用**的：再问一次读到的是新值，不是第一次的快照。
#[test]
fn m5_doc_meta_and_view_info_come_from_the_bridge() {
    let mut m = MockDoc::new("a
b
c");
    m.meta = DocMeta {
        encoding: "GBK".into(),
        bom: false,
        eol: "CRLF".into(),
        syntax: "rs".into(),
        bytes: 1234,
        lines: 3,
        read_only: true,
        modified: true,
        large: false,
    };
    m.view = ViewInfo {
        top_line: 7,
        top_col: 2,
        rows: 40,
        cols: 120,
        caret_line: 9,
        caret_col: 4,
    };
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        local d = App.doc.meta()
        print(d.encoding .. "|" .. tostring(d.bom) .. "|" .. d.eol .. "|" .. d.syntax)
        print(d.bytes .. "|" .. d.lines .. "|" .. tostring(d.read_only)
              .. "|" .. tostring(d.modified) .. "|" .. tostring(d.large))
        local v = App.view.info()
        print(v.top_line .. "|" .. v.top_col .. "|" .. v.rows .. "|" .. v.cols
              .. "|" .. v.caret_line .. "|" .. v.caret_col)
        -- 同一个表：再取一次是同一份引用，读到的却是新值。
        local same = (App.doc.meta() == d)
        print("same table: " .. tostring(same))
        "#,
        &mut m,
    )
    .unwrap();
    assert_eq!(m.logs[0], "GBK|false|CRLF|rs");
    assert_eq!(m.logs[1], "1234|3|true|true|false");
    assert_eq!(m.logs[2], "7|2|40|120|9|4");
    assert_eq!(m.logs[3], "same table: true");
}

/// 默认桥（不实现这两个方法）也要能跑：全空、全 0，不 panic。
#[test]
fn m5_a_headless_bridge_answers_empty_meta_and_view() {
    struct Bare;
    impl Bridge for Bare {
        fn alert(&mut self, _: &str) {}
        fn log(&mut self, msg: &str) {
            println!("{msg}");
        }
        fn doc_name(&self) -> String {
            String::new()
        }
        fn doc_path(&self) -> Option<String> {
            None
        }
        fn line_count(&self) -> usize {
            0
        }
        fn line_text(&self, _: usize) -> Option<String> {
            None
        }
        fn doc_text(&self) -> String {
            String::new()
        }
        fn span_text(&self, _: Pos, _: Pos) -> Option<String> {
            None
        }
        fn insert_at(&mut self, _: Pos, _: &str) -> bool {
            false
        }
        fn replace_span(&mut self, _: Pos, _: Pos, _: &str) -> bool {
            false
        }
        fn sel_count(&self) -> usize {
            0
        }
        fn sel_range(&self, _: usize) -> Option<(Pos, Pos)> {
            None
        }
        fn sel_text(&self) -> String {
            String::new()
        }
        fn sel_set(&mut self, _: Pos, _: Pos) {}
    }
    let mut b = Bare;
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        local d = App.doc.meta()
        local v = App.view.info()
        assert(d.encoding == "" and d.eol == "" and d.syntax == "")
        assert(d.bytes == 0 and d.lines == 0 and d.bom == false)
        assert(v.top_line == 0 and v.rows == 0 and v.caret_col == 0)
        "#,
        &mut b,
    )
    .unwrap();
}

// ---- M2b: 点击回调（s:on） ----

fn tab_input() -> dg_script::PaintInput {
    dg_script::PaintInput {
        rect: (10.0, 20.0, 100.0, 30.0),
        thumb: (10.0, 20.0, 100.0, 30.0),
        bar: (0.0, 0.0, 0.0, 0.0),
        scroll: 0.0,
        state: dg_skin::State::ACTIVE.bits(),
        part: "tab".into(),
        text: "main.rs".into(),
        label: String::new(),
        value: String::new(),
        dpi: 96,
        index: 0,
    }
}

/// 注册、触发、返回值三态：`true` 接管，`false`/`nil` 让位。
#[test]
fn m5_a_press_handler_sees_the_cell_and_can_take_it_over() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        local ui = App.ui
        ui.on("tab", "press", function(ctx)
            print("press " .. ctx.part .. " " .. ctx.text .. " active=" .. tostring(ctx.active))
            return ctx.text == "main.rs"
        end)
        "#,
        &mut m,
    )
    .unwrap();
    assert!(h.has_press("tab", "press"));
    assert!(!h.has_press("tab", "rclick"));
    assert_eq!(h.press("tab", "press", &tab_input(), &mut m), Some(true));
    assert_eq!(m.logs, ["press tab main.rs active=true"]);

    // 换一格标题：同一个回调返回 false，内置动作该照做。
    let mut other = tab_input();
    other.text = "other.rs".into();
    other.state = 0;
    assert_eq!(h.press("tab", "press", &other, &mut m), Some(false));
    assert_eq!(m.logs[1], "press tab other.rs active=false");
}

/// 没注册回调时返回 `None`（调用方据此完全不打扰脚本）。
#[test]
fn m5_no_press_handler_answers_none() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load("t.lua", "local ui = App.ui", &mut m).unwrap();
    assert!(!h.has_press("tab", "press"));
    assert_eq!(h.press("tab", "press", &tab_input(), &mut m), None);
}

/// 回调报错按「没接管」处理：一个画错的皮肤不该让页签点不动。
#[test]
fn m5_a_failing_press_handler_does_not_swallow_the_click() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"App.ui.on("tab", "press", function() error("boom") end)"#,
        &mut m,
    )
    .unwrap();
    assert_eq!(h.press("tab", "press", &tab_input(), &mut m), Some(false));
}

/// 事件名写错当场报错，不静默收下。
#[test]
fn m5_a_bad_event_name_is_rejected() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    let e = h
        .load(
            "t.lua",
            r#"App.ui.on("tab", "click", function() end)"#,
            &mut m,
        )
        .unwrap_err();
    assert!(format!("{e}").contains("press"), "{e}");
    assert!(!h.has_press("tab", "press"));
}

/// 重载换掉回调、`unon` 也能撤：改一次皮肤不该留下两套点击处理。
#[test]
fn m5_reloading_or_unon_drops_the_press_handler() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    for _ in 0..3 {
        h.load(
            "t.lua",
            r#"App.ui.on("tab", "press", function() return true end)"#,
            &mut m,
        )
        .unwrap();
        assert!(h.has_press("tab", "press"));
    }
    h.load(
        "t.lua",
        r#"
        local ui = App.ui
        ui.on("tab", "press", function() return true end)
        ui.off("tab", "press")
        "#,
        &mut m,
    )
    .unwrap();
    assert!(!h.has_press("tab", "press"));
    assert_eq!(h.press("tab", "press", &tab_input(), &mut m), None);
}

/// `App.view.scroll_to` 只排队：回调里调完视口还没动，派发时才滚。
/// 这是写操作的统一规矩 —— 回调期间 `EditorState` 正被借走。
#[test]
fn m5_view_scroll_to_is_queued_until_the_entry_point_returns() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        App.on("go", function()
            App.view.scroll_to(42, 7)
            App.view.scroll_to(3)
            print("queued " .. App.pending())
        end)
        "#,
        &mut m,
    )
    .unwrap();
    h.trigger("go", &mut m);
    assert!(m.scrolls.is_empty(), "回调里不该动视口");
    assert_eq!(h.pending_jobs(), 2);
    h.run_queue(&mut m);
    assert_eq!(m.scrolls, [(42, 7), (3, 0)], "列省略时按 0 算");
    assert_eq!(h.pending_jobs(), 0);
}

/// 任务栏状态的五种写法都能解析,别名和大小写不挑剔;规范名用于报错。
#[test]
fn taskbar_state_names_round_trip() {
    for (name, want) in [
        ("none", TaskbarState::None),
        ("OFF", TaskbarState::None),
        ("indeterminate", TaskbarState::Indeterminate),
        ("busy", TaskbarState::Indeterminate),
        ("normal", TaskbarState::Normal),
        ("ok", TaskbarState::Normal),
        ("error", TaskbarState::Error),
        ("fail", TaskbarState::Error),
        ("paused", TaskbarState::Paused),
        (" pause ", TaskbarState::Paused),
    ] {
        assert_eq!(TaskbarState::parse(name), Some(want), "{name}");
    }
    for name in ["", "done", "progress", "2"] {
        assert_eq!(TaskbarState::parse(name), None, "{name}");
    }
    assert_eq!(TaskbarState::Indeterminate.name(), "indeterminate");
    assert_eq!(TaskbarState::None.name(), "none");
}

/// `App.taskbar` 直接走 bridge:省略刻度是满格,超出的完成量夹到总量。
#[test]
fn app_taskbar_drives_the_bridge() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        App.on("go", function()
            App.taskbar("busy")
            App.taskbar("normal", 3, 10)
            App.taskbar("ERROR")
            App.taskbar("paused", 99, 4)
            App.taskbar("none")
        end)
        "#,
        &mut m,
    )
    .unwrap();
    h.trigger("go", &mut m);
    assert_eq!(
        m.taskbar_calls,
        [
            (TaskbarState::Indeterminate, 1, 1),
            (TaskbarState::Normal, 3, 10),
            (TaskbarState::Error, 1, 1),
            (TaskbarState::Paused, 4, 4),
            (TaskbarState::None, 1, 1),
        ]
    );
}

#[test]
fn app_taskbar_rejects_an_unknown_state_and_a_zero_total() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    assert!(h.load("bad.lua", r#"App.taskbar("done")"#, &mut m).is_err());
    assert!(h
        .load("zero.lua", r#"App.taskbar("normal", 1, 0)"#, &mut m)
        .is_err());
    assert!(m.taskbar_calls.is_empty());
}

// ---- App.ui.repaint：脚本改了自己的内部状态，通知宿主重画一帧 ----

#[test]
fn app_ui_repaint_marks_dirty_and_names_the_region() {
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load(
        "t.lua",
        r#"
        App.command("touch", function()
            App.ui.repaint("pane.clock")
            App.ui.repaint("pane.clock")
        end)
        App.command("touch_all", function() App.ui.repaint() end)
        "#,
        &mut m,
    )
    .unwrap();
    let _ = h.take_dirty();
    assert!(h.call_command("touch", &mut m));
    assert_ne!(
        h.take_dirty() & dg_script::paint::DIRTY_REPAINT,
        0,
        "重画请求要出现在脏位里"
    );
    assert_eq!(
        h.take_repaint_regions(),
        vec!["pane.clock".to_string(), "pane.clock".to_string()]
    );
    assert!(h.take_repaint_regions().is_empty(), "取过一次就该清空");

    assert!(h.call_command("touch_all", &mut m));
    assert_ne!(h.take_dirty() & dg_script::paint::DIRTY_REPAINT, 0);
    // 不给名字也记一条空串：外壳据此知道「有人请求了全量重画」。
    assert_eq!(h.take_repaint_regions(), vec![String::new()]);
}

// ---- 随仓库附带的贪吃蛇示例：装载、画法、按键、一整局 ----

/// `snake.state` 打的那一行里取一个 `key=数字`，随机食物的位置不影响断言。
fn snake_number(line: &str, key: &str) -> i32 {
    line.split_whitespace()
        .find_map(|word| word.strip_prefix(key)?.parse().ok())
        .unwrap_or(-1)
}

#[test]
fn snake_demo_loads_draws_and_plays_a_round() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/snake/snake.lua"
    ))
    .expect("the snake demo ships with the repo");
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    h.load("snake.lua", &source, &mut m).unwrap();

    // 方向键 / 空格 / 裸字母共用一张命令表。
    assert_eq!(
        h.lookup_key(false, false, false, 0x26).as_deref(),
        Some("snake.up")
    );
    assert_eq!(
        h.lookup_key(false, false, false, 0x20).as_deref(),
        Some("snake.toggle")
    );
    assert_eq!(
        h.lookup_key(false, false, false, b'R' as u32).as_deref(),
        Some("snake.restart")
    );

    let input = dg_script::PaintInput {
        rect: (0.0, 0.0, 300.0, 600.0),
        thumb: (0.0, 0.0, 300.0, 600.0),
        bar: (0.0, 0.0, 0.0, 0.0),
        scroll: 0.0,
        state: 0,
        part: "snake".into(),
        text: String::new(),
        label: String::new(),
        value: String::new(),
        dpi: 96,
        index: 0,
    };
    let out = h
        .paint("pane.snake", &input)
        .expect("the demo registers a paint callback");
    assert!(out.handled);
    assert!(
        out.ops.len() > 30,
        "棋盘、格线、蛇和 HUD 都该画出来：{} 条 op",
        out.ops.len()
    );

    h.call_command("snake.state", &mut m);
    assert!(m.logs.last().unwrap().contains("phase=ready score=0"));

    // 开局向右：第 7 步吃到 (14,9) 那颗食物，长度 3 → 4、分数 +10。
    assert!(h.call_command("snake.toggle", &mut m));
    for _ in 0..7 {
        assert!(h.call_command("snake.step", &mut m));
    }
    h.call_command("snake.state", &mut m);
    let line = m.logs.last().unwrap().clone();
    assert!(line.contains("phase=run"), "{line}");
    assert!(line.contains("score=10"), "{line}");
    assert!(line.contains("len=4"), "{line}");
    assert!(line.contains("head=14,9"), "{line}");

    // 一直向右撞墙；结束后 step 不再推进。
    for _ in 0..20 {
        h.call_command("snake.step", &mut m);
    }
    h.call_command("snake.state", &mut m);
    let line = m.logs.last().unwrap().clone();
    assert!(line.contains("phase=over"), "{line}");
    assert!(line.contains("head=20,9"), "{line}");
    assert!(snake_number(&line, "score=") >= 10, "{line}");
    assert!(snake_number(&line, "best=") >= 10, "{line}");

    // 重开把分数和长度收回去，方向键还能改朝向。
    assert!(h.call_command("snake.restart", &mut m));
    assert!(h.call_command("snake.up", &mut m));
    assert!(h.call_command("snake.step", &mut m));
    h.call_command("snake.state", &mut m);
    let line = m.logs.last().unwrap();
    assert!(line.contains("phase=run"), "{line}");
    assert_eq!(snake_number(line, "score="), 0, "{line}");
    assert_eq!(snake_number(line, "len="), 3, "{line}");
    assert!(line.contains("head=7,8"), "{line}");
}

/// 贪吃蛇是「点了菜单才跑」的包：清单里必须有 `launch`，否则开机就会把
/// 八条内部命令全挂进扩展菜单 —— 那正是这个生命周期要解决的问题。
#[test]
fn snake_manifest_declares_on_demand_launch() {
    let dir = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/snake"
    ));
    let text = std::fs::read_to_string(dir.join("package.toml")).expect("snake manifest");
    let m = dg_script::package::parse_manifest(&text, dir).expect("snake manifest parses");
    assert_eq!(m.id, "snake");
    assert!(m.is_on_demand(), "snake 必须写 launch = \"…\"");
    assert!(!m.menu_label().trim().is_empty());

    // 另一个示例包没写 launch，保持开机装载，别被一起改坏。
    let paint = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/luapaint"
    ));
    let text = std::fs::read_to_string(paint.join("package.toml")).expect("luapaint manifest");
    let m = dg_script::package::parse_manifest(&text, paint).expect("luapaint manifest parses");
    assert!(!m.is_on_demand());
}

/// 「关掉游戏之后不占内存」在真示例上走一遍：装载 → 玩一发 → 整包退出。
/// 命令、按键、窗格声明、定时器、画布缓存一样都不该剩，Lua 堆要降回去。
#[test]
fn snake_demo_unloads_completely_when_the_game_closes() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/snake/snake.lua"
    ))
    .expect("the snake demo ships with the repo");
    let mut m = MockDoc::new("");
    let h = ScriptHost::new().unwrap();
    let base = h.memory().0;

    // 用 `load_in` 而不是 `load`：带上包名，卸载才有归属可依。
    h.load_in("snake", "snake.lua", &source, &mut m).unwrap();
    assert_eq!(h.sole_pane_package("snake").as_deref(), Some("snake"));
    assert_eq!(h.command_names().len(), 8, "菜单里本来会有八行 snake.*");
    assert_eq!(
        h.command_names_excluding(&["snake".to_string()]).len(),
        0,
        "按需包的内部命令不该进扩展菜单"
    );
    assert!(h.lookup_key(false, false, false, 0x26).is_some());
    assert_eq!(h.panes().len(), 1);

    // 真玩一发，让计时器/画法缓存里都填上东西。
    assert!(h.call_command("snake.toggle", &mut m));
    assert!(h.call_command("snake.step", &mut m));
    let input = dg_script::PaintInput {
        rect: (0.0, 0.0, 300.0, 600.0),
        thumb: (0.0, 0.0, 300.0, 600.0),
        bar: (0.0, 0.0, 0.0, 0.0),
        scroll: 0.0,
        state: 0,
        part: "snake".into(),
        text: String::new(),
        label: String::new(),
        value: String::new(),
        dpi: 96,
        index: 0,
    };
    assert!(h.paint("pane.snake", &input).is_some());
    let loaded = h.memory().0;
    assert!(loaded > base);

    let removed = h.purge_package("snake");
    assert_eq!(removed.commands, 8, "{removed:?}");
    assert_eq!(removed.keys, 11, "{removed:?}");
    assert_eq!(removed.panes, 1, "{removed:?}");
    assert_eq!(removed.timers, 1, "{removed:?}");
    assert_eq!(removed.paints, 1, "{removed:?}");
    assert!(h.command_names().is_empty());
    assert!(h.panes().is_empty());
    assert!(h.paint("pane.snake", &input).is_none(), "画法缓存也该作废");
    assert!(h.lookup_key(false, false, false, 0x26).is_none());
    assert_eq!(h.sole_pane_package("snake"), None);
    assert!(h.counts().is_empty(), "一条注册都不该剩：{:?}", h.counts());
    assert!(
        h.memory().0 < loaded,
        "退出后 Lua 堆不该还涨着：{loaded} → {}",
        h.memory().0
    );
}
