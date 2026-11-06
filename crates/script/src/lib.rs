//! dg-script — this implementation's Lua script layer (milestones M1–M4): a sandboxed
//! engine plus a [`Bridge`] the shell implements, on every platform. Coordinates handed
//! to scripts are **1-based line numbers** and **1-based byte columns** into that line's
//! UTF-8 text (Lua strings are byte strings; `#` and `string.sub` line up with the
//! columns). The [`Bridge`] converts to the document's char columns with the helpers
//! below.
//!
//! One JSON file per script, with string values. One [`ScriptHost`] is a persistent
//! engine: chunks are [`load`]ed once (typically at startup, from `<exe>/scripts/*.lua`)
//! and their registrations survive; the shell replays them with [`trigger`] /
//! [`call_command`]. The bridge is *not* owned by the host — each entry point installs it
//! for the duration of the call (see [`BridgePtr`]), so the shell can pass a borrow of
//! its own state.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use mlua::{Function, IntoLuaMulti, Lua, MultiValue, StdLib, Table, UserData, UserDataMethods, Value};

pub mod paint;
pub mod package;
pub mod registry;
pub mod style;
mod budget;
mod jobs;
mod timers;

pub use package::{
    Origin, Package, PackageError, PackageManifest, Scanned, State, discover,
};
pub use registry::Counts;

pub use paint::{
    Canvas, Metrics, PaintCtx, PaintDoc, PaintInput, PaintOut, PaintRow, PaneSpec, Reads,
    ToolbarButton,
};

/// Script API version, surfaced as `App.version()`.
pub const API_VERSION: &str = "0.2";

/// (line, byte-col): 1-based line number, 1-based byte offset into
/// that line's UTF-8 text. Column `len+1` is past the line's last
/// byte; column 1 is its first.
pub type Pos = (usize, usize);

/// 文档的元信息：状态栏那几格要显示的东西，皮肤自己画的时候也得有。
///
/// 全是**只读**的快照值，一次调用取齐 —— 皮肤每帧要问，逐项过 bridge
/// 就是每帧十几次虚调用。
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct DocMeta {
    /// 编码名，原样（`UTF-8` / `GBK` / `windows-1252` …）。显示用的
    /// `CP 936` 这类词是外壳的事，引擎不翻译。
    pub encoding: String,
    pub bom: bool,
    /// `LF` / `CRLF` / `CR`；空文档答 `LF`。
    pub eol: String,
    /// 语法键（`rs` / `py` …），没挂语法就是空串。
    pub syntax: String,
    /// 文件在磁盘上的字节数；未命名或还没落盘是 0。
    pub bytes: u64,
    pub lines: usize,
    pub read_only: bool,
    pub modified: bool,
    /// 大文件模式（没有 tree-sitter、没有逐行修改状态）。
    pub large: bool,
}

/// 视口与光标：皮肤要按「现在看到哪儿」算几何时问它。
///
/// 行号 0 基，列是**字符**列 0 基 —— 和 `doc.meta().lines` 的 1 基行号
/// 不一样，这是给几何用的，不是给文档 API 用的。
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ViewInfo {
    pub top_line: usize,
    pub top_col: usize,
    /// 可见行数 / 列数（按当前字体和换行模式算出来的）。
    pub rows: usize,
    pub cols: usize,
    pub caret_line: usize,
    pub caret_col: usize,
}

/// The frame window's taskbar progress state. A platform-neutral name for
/// the Windows shell's `TBPFLAG`; a backend without a taskbar simply reports
/// `false` from [`Bridge::taskbar_progress`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskbarState {
    /// No bar on the button.
    None,
    /// The shell's own marquee, for work whose total is not known.
    Indeterminate,
    /// A green bar.
    Normal,
    /// A red bar.
    Error,
    /// A yellow bar.
    Paused,
}

impl TaskbarState {
    /// The spelling `App.taskbar` accepts. Case is folded, and the two
    /// everyday words a script is likely to reach for have aliases.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Self::None,
            "indeterminate" | "busy" => Self::Indeterminate,
            "normal" | "ok" => Self::Normal,
            "error" | "fail" => Self::Error,
            "paused" | "pause" => Self::Paused,
            _ => return None,
        })
    }

    /// The canonical spelling, for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Indeterminate => "indeterminate",
            Self::Normal => "normal",
            Self::Error => "error",
            Self::Paused => "paused",
        }
    }
}

/// What the shell does for a script. Every method takes script
/// coordinates (see [`Pos`]); the shell converts to document
/// coordinates and routes edits through its undo-capable path.
pub trait Bridge {
    fn alert(&mut self, msg: &str);
    fn log(&mut self, msg: &str);
    fn doc_name(&self) -> String;
    /// `None` for an untitled document.
    fn doc_path(&self) -> Option<String>;
    fn line_count(&self) -> usize;
    /// Text of a 1-based line, terminator excluded. `None` when the
    /// line is out of range.
    fn line_text(&self, line: usize) -> Option<String>;
    fn doc_text(&self) -> String;
    /// Text of a span, `None` when either endpoint is invalid.
    fn span_text(&self, s: Pos, e: Pos) -> Option<String>;
    /// Insert `text` at a position. `false` when the position is
    /// invalid or the buffer refused the edit (read-only).
    fn insert_at(&mut self, p: Pos, text: &str) -> bool;
    /// Replace `[s, e)` with `text`; an empty `text` deletes.
    fn replace_span(&mut self, s: Pos, e: Pos, text: &str) -> bool;
    fn sel_count(&self) -> usize;
    /// 0-based selection index.
    fn sel_range(&self, index: usize) -> Option<(Pos, Pos)>;
    fn sel_text(&self) -> String;
    fn sel_set(&mut self, s: Pos, e: Pos);
    /// M4: one-line input dialog. `None` cancels; the default
    /// implementation makes it a no-op for headless bridges.
    fn prompt(&mut self, title: &str, initial: &str) -> Option<String> {
        let _ = (title, initial);
        None
    }
    /// Runs a built-in command by name — the `cm_file_save` family, or the
    /// dotted spelling a skin doc uses (`file.save`). `false` when there is no
    /// such command or the current state refuses it.
    ///
    /// This is where `App.run` lands: the script queues the name, and
    /// [`ScriptHost::run_queue`] dispatches it once the callback has returned
    /// and the borrow on the shell's state has ended. Default does nothing, for
    /// headless bridges.
    fn run_command(&mut self, name: &str) -> bool {
        let _ = name;
        false
    }
    /// 文档元信息（编码 / EOL / 语法 / 大小 / 只读 / 修改位 / 大文件）。
    /// 默认全空，headless 桥不用管。
    fn doc_meta(&self) -> DocMeta {
        DocMeta::default()
    }
    /// 视口与光标。默认全 0。
    fn view_info(&self) -> ViewInfo {
        ViewInfo::default()
    }
    /// 滚到某一行某一列（0 基，列是字符列 —— 和 [`ViewInfo`] 一把尺子）。
    ///
    /// 这是 `App.view.scroll_to` 的落点，走待办队列：它改的是编辑状态，
    /// 而回调期间那份状态正被借走。默认不动，headless 桥够用。
    fn scroll_to(&mut self, line: usize, col: usize) -> bool {
        let _ = (line, col);
        false
    }
    /// 任务栏按钮的进度状态。`completed` / `total` 只在有刻度的三个状态里
    /// 有意义,`total` 必须大于 0。返回宿主是否真的设置了它:没有任务栏的
    /// 后端(headless、其它平台)恒为 `false`。默认什么都不做。
    fn taskbar_progress(&mut self, state: TaskbarState, completed: u64, total: u64) -> bool {
        let _ = (state, completed, total);
        false
    }
}

/// 一个什么都不做的 [`Bridge`]：给 `--ui-lint` 这类**不开窗**的入口用。
///
/// 它存在的理由和 [`LogSink`] 一样：`skin.lua` 在装载时就会 `App.storage`、
/// `App.command`，lint 只想知道「画法产出什么」，不想造一台编辑器。`alert`
/// 和 `log` 落到 stderr —— 静默吞掉反而难查。
pub struct HeadlessBridge;

impl Bridge for HeadlessBridge {
    fn alert(&mut self, msg: &str) {
        eprintln!("[lua] alert: {msg}");
    }
    fn log(&mut self, msg: &str) {
        eprintln!("[lua] {msg}");
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
    fn line_text(&self, _line: usize) -> Option<String> {
        None
    }
    fn doc_text(&self) -> String {
        String::new()
    }
    fn span_text(&self, _s: Pos, _e: Pos) -> Option<String> {
        None
    }
    fn insert_at(&mut self, _p: Pos, _text: &str) -> bool {
        false
    }
    fn replace_span(&mut self, _s: Pos, _e: Pos, _text: &str) -> bool {
        false
    }
    fn sel_count(&self) -> usize {
        0
    }
    fn sel_range(&self, _index: usize) -> Option<(Pos, Pos)> {
        None
    }
    fn sel_text(&self) -> String {
        String::new()
    }
    fn sel_set(&mut self, _s: Pos, _e: Pos) {}
}

/// 没有 bridge 时日志往哪儿去（`print` 和 `App.log` 共用）。
///
/// 绘制回调里没有 bridge —— 那是重入，见 [`ScriptHost::paint`]。可绘制回调恰恰
/// 是最需要日志的地方：画不出来的时候，第一件事就是想打一行。所以这两条接口
/// 走同一条兜底链：**有 bridge 就交给 bridge**（外壳那边写 stderr 顺手
/// `OutputDebugString`），**没有就问这个 sink**，连 sink 都没装才直接写 stderr。
///
/// 外壳在 [`ScriptHost::new`] 之后调 [`ScriptHost::set_log_sink`] 装一次。
/// dg-script 自己不认识 `OutputDebugStringW`：这个 crate 还要给 cocoa / gtk
/// 后端用，不能挂 Windows API。
#[derive(Clone)]
pub struct LogSink(pub std::sync::Arc<dyn Fn(&str) + 'static>);

/// 1-based byte column → 0-based char column. A column that lands
/// inside a multi-byte char rounds down to that char's start; a
/// column past the end clamps to the line's length.
pub fn byte_col_to_char_col(line: &str, byte_col: usize) -> usize {
    let off = byte_col.saturating_sub(1).min(line.len());
    let mut boundary = off;
    while boundary > 0 && !line.is_char_boundary(boundary) {
        boundary -= 1;
    }
    line[..boundary].chars().count()
}

/// 0-based char column → 1-based byte column.
pub fn char_col_to_byte_col(line: &str, char_col: usize) -> usize {
    match line.char_indices().nth(char_col) {
        Some((i, _)) => i + 1,
        None => line.len() + 1,
    }
}

/// A script run failure: Lua error text with the chunk name folded
/// in by mlua (`name:line: message`).
#[derive(Debug)]
pub struct ScriptError(pub String);

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ScriptError {}

impl From<mlua::Error> for ScriptError {
    fn from(e: mlua::Error) -> Self {
        Self(e.to_string())
    }
}

/// The borrowed bridge, installed into the engine's app data for the
/// duration of each entry point ([`ScriptHost::load`],
/// [`ScriptHost::trigger`], [`ScriptHost::call_command`]).
///
/// SAFETY: the pointer is only dereferenced by callbacks while one
/// of those entry points is on the stack — the app data slot is
/// cleared before they return — so nothing dereferences it after the
/// borrow ends. The Lua state is single-threaded and never shared.
#[derive(Clone, Copy)]
struct BridgePtr(*mut dyn Bridge);

impl BridgePtr {
    fn get(self) -> &'static mut dyn Bridge {
        // SAFETY: see the type's comment; the 'static spelling stands
        // for "outlives the entry point that installed it".
        unsafe { &mut *self.0 }
    }
}

/// The app-data slot holding the bridge of the current entry point.
struct CurrentBridge(Option<BridgePtr>);

/// 当前正在跑的 chunk 名字。每条注册都记下它，重载时按名字清理 —— 否则
/// 同一个脚本改三次，`doc.saved` 就收到三份 handler。
struct ChunkName(RefCell<String>);

/// 包名与文件名之间的分隔符。块名来自文件名，而文件名里不可能有控制字符，
/// 所以 `U+0001` 不会撞上 —— 用它就不必给每条记录再加一个字段。
const PKG_SEP: char = '\u{1}';

/// 把「包 + 文件」拼成一条块名。没有包的（裸脚本、旧位置）就是文件名本身，
/// 和以前完全一样。
pub(crate) fn qualified(pkg: &str, chunk: &str) -> String {
    if pkg.is_empty() {
        chunk.to_string()
    } else {
        format!("{pkg}{PKG_SEP}{chunk}")
    }
}

/// 块名里的包名。裸脚本答空串。
pub(crate) fn pkg_of(chunk: &str) -> &str {
    match chunk.find(PKG_SEP) {
        Some(i) => &chunk[..i],
        None => "",
    }
}

/// 块名里给人看的那部分（去掉包前缀）。报错时用它，别让 `\u{1}` 漏到屏幕上。
pub fn chunk_label(chunk: &str) -> &str {
    match chunk.find(PKG_SEP) {
        Some(i) => &chunk[i + PKG_SEP.len_utf8()..],
        None => chunk,
    }
}

pub(crate) fn current_chunk(lua: &Lua) -> mlua::Result<String> {
    lua.app_data_ref::<ChunkName>()
        .map(|c| c.0.borrow().clone())
        .ok_or_else(|| mlua::Error::runtime("chunk name is not initialized"))
}

fn set_current_chunk(lua: &Lua, name: &str) {
    if let Some(c) = lua.app_data_ref::<ChunkName>() {
        let mut slot = c.0.borrow_mut();
        slot.clear();
        slot.push_str(name);
    }
}

/// 老名字 ↔ 新名字。`trigger` 两边都触发：改名不该让已经写好的脚本静默失效。
fn alias_of(event: &str) -> Option<&'static str> {
    Some(match event {
        "DocSaved" => "doc.saved",
        "doc.saved" => "DocSaved",
        "DocOpened" => "doc.opened",
        "doc.opened" => "DocOpened",
        "DocClosed" => "doc.closed",
        "doc.closed" => "DocClosed",
        "ThemeChanged" => "theme.changed",
        "theme.changed" => "ThemeChanged",
        "CaretMoved" => "caret.moved",
        "caret.moved" => "CaretMoved",
        "ViewScrolled" => "view.scrolled",
        "view.scrolled" => "ViewScrolled",
        _ => return None,
    })
}

/// 一条注册属于哪个 chunk。老格式（直接放函数）没有名字，算空串。
fn record_chunk(value: &Value) -> String {
    match value {
        Value::Table(t) => t.get::<Option<String>>("n").ok().flatten().unwrap_or_default(),
        _ => String::new(),
    }
}

/// 一条注册里的回调。老格式直接是函数，也认。
pub(crate) fn record_func(value: &Value) -> Option<Function> {
    match value {
        Value::Function(f) => Some(f.clone()),
        Value::Table(t) => t.get::<Option<Function>>("f").ok().flatten(),
        _ => None,
    }
}

/// The app-data cache behind `App.storage`.
#[derive(Default)]
struct StorageCache {
    dir: Option<PathBuf>,
    loaded: bool,
    tables: BTreeMap<String, BTreeMap<String, String>>,
}

impl StorageCache {
    fn save(&self) {
        let Some(dir) = &self.dir else { return };
        if let Ok(text) = serde_json::to_string(&self.tables) {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(dir.join("storage.json"), text);
        }
    }

    fn table(&mut self, name: &str) -> &mut BTreeMap<String, String> {
        if !self.loaded {
            self.loaded = true;
            if let Some(dir) = self.dir.clone() {
                if let Ok(text) = std::fs::read_to_string(dir.join("storage.json")) {
                    if let Ok(map) =
                        serde_json::from_str::<BTreeMap<String, BTreeMap<String, String>>>(&text)
                    {
                        self.tables = map;
                    }
                }
            }
        }
        self.tables.entry(name.to_string()).or_default()
    }
}

/// The userdata `App.storage(name)` returns: this implementation's
/// `CPersistentStringTable` (string→string, persisted on every set).
struct StorageTable(String);

impl UserData for StorageTable {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("get", |lua, this, key: String| {
            let cache = lua
                .app_data_mut::<RefCell<StorageCache>>()
                .ok_or_else(no_storage)?;
            let _ = cache.borrow_mut().table(&this.0); // lazy-load the file
            Ok(cache
                .borrow()
                .tables
                .get(&this.0)
                .and_then(|t| t.get(&key))
                .cloned())
        });
        methods.add_method("set", |lua, this, (key, value): (String, String)| {
            let cache = lua
                .app_data_mut::<RefCell<StorageCache>>()
                .ok_or_else(no_storage)?;
            cache.borrow_mut().table(&this.0).insert(key, value);
            cache.borrow().save();
            Ok(())
        });
        methods.add_method("del", |lua, this, key: String| {
            let cache = lua
                .app_data_mut::<RefCell<StorageCache>>()
                .ok_or_else(no_storage)?;
            cache.borrow_mut().table(&this.0).remove(&key);
            cache.borrow().save();
            Ok(())
        });
        methods.add_method("keys", |lua, this, prefix: Option<String>| {
            let cache = lua
                .app_data_mut::<RefCell<StorageCache>>()
                .ok_or_else(no_storage)?;
            let _ = cache.borrow_mut().table(&this.0); // lazy-load the file
            let keys: Vec<String> = cache
                .borrow()
                .tables
                .get(&this.0)
                .map(|t| {
                    t.keys()
                        .filter(|k| prefix.as_ref().is_none_or(|p| k.starts_with(p)))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            Ok(keys)
        });
        methods.add_method("count", |lua, this, ()| {
            let cache = lua
                .app_data_mut::<RefCell<StorageCache>>()
                .ok_or_else(no_storage)?;
            let _ = cache.borrow_mut().table(&this.0); // lazy-load the file
            Ok(cache.borrow().tables.get(&this.0).map_or(0, |t| t.len()))
        });
    }
}

/// A parsed key gesture: `"Ctrl+Shift+T"` → `{ctrl, shift, alt, vk}`.
/// vk is a Win32-style virtual key code (letters `b'A'..=b'Z'`,
/// digits `b'0'..=b'9'`, `F1..F12`, or one of [`NAMED_KEYS`]); the
/// shell maps it to whatever its platform calls a key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gesture {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub vk: u32,
}

/// Keys that have no printable character. Values are Win32-style virtual
/// key codes; the shell matches `WM_KEYDOWN`'s `wParam` against them.
///
/// Script panes own the keyboard while focused, so arrow keys, Space and
/// Enter are the everyday way to drive one. Aliases (Esc/Escape,
/// PgUp/PageUp …) share a code and are folded case-insensitively.
pub const NAMED_KEYS: &[(&str, u32)] = &[
    ("left", 0x25),
    ("up", 0x26),
    ("right", 0x27),
    ("down", 0x28),
    ("space", 0x20),
    ("enter", 0x0D),
    ("return", 0x0D),
    ("esc", 0x1B),
    ("escape", 0x1B),
    ("tab", 0x09),
    ("backspace", 0x08),
    ("back", 0x08),
    ("insert", 0x2D),
    ("ins", 0x2D),
    ("delete", 0x2E),
    ("del", 0x2E),
    ("home", 0x24),
    ("end", 0x23),
    ("pageup", 0x21),
    ("pgup", 0x21),
    ("pagedown", 0x22),
    ("pgdn", 0x22),
];

/// What `App.keymap` prints when a gesture is not one it knows.
const GESTURE_HELP: &str = "want Ctrl/Shift/Alt + letter, digit, F1-F12, or one of \
     Up/Down/Left/Right/Space/Enter/Esc/Tab/Backspace/Insert/Delete/Home/End/PageUp/PageDown";

/// Parses a single-key gesture. Modifiers are the words
/// `Ctrl`/`Shift`/`Alt` in any combination, then one key: a letter, a
/// digit, `F1`..`F12`, or a name from [`NAMED_KEYS`]. Anything else is
/// not a script gesture.
pub fn parse_gesture(s: &str) -> Option<Gesture> {
    let mut g = Gesture { ctrl: false, shift: false, alt: false, vk: 0 };
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    for part in &parts[..parts.len() - 1] {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => g.ctrl = true,
            "shift" => g.shift = true,
            "alt" => g.alt = true,
            _ => return None,
        }
    }
    let key = parts[parts.len() - 1];
    let lower = key.to_ascii_lowercase();
    if let Some((_, vk)) = NAMED_KEYS.iter().find(|(name, _)| *name == lower) {
        g.vk = *vk;
        return Some(g);
    }
    let bytes = key.as_bytes();
    g.vk = match bytes {
        [b] if b.is_ascii_alphabetic() => b.to_ascii_uppercase() as u32,
        [b] if b.is_ascii_digit() => *b as u32,
        _ => {
            let n: u32 = lower.strip_prefix('f')?.parse().ok()?;
            if !(1..=12).contains(&n) {
                return None;
            }
            0x6F + n
        }
    };
    Some(g)
}

/// Registry keys the engine keeps its tables under — the Lua
/// registry, not the script `_ENV`, so scripts cannot reach them.
const REG_ENV: &str = "evscript.env";
const REG_EVENTS: &str = "evscript.events";
const REG_COMMANDS: &str = "evscript.commands";
const REG_KEYS: &str = "evscript.keys";

/// A persistent, sandboxed Lua engine plus the script registry.
pub struct ScriptHost {
    lua: Lua,
    /// 绘制结果缓存。`load` 换脚本时整表清掉 —— 回调换了，op 列表全都作废。
    paint_cache: std::cell::RefCell<paint::PaintCache>,
    /// 绘制档的文档快照，外壳每帧推一次（[`ScriptHost::set_paint_doc`]）。
    /// `ctx.doc` 读的就是它 —— 绘制回调没有 bridge，问不了 `App.doc.meta()`。
    paint_doc: std::cell::RefCell<Option<std::rc::Rc<paint::PaintDoc>>>,
    /// 指令预算的计数器（钩子在 `new` 里装好）。每个入口都 `arm` 一次 ——
    /// 沙箱挡不住 `while true do end`，这个挡得住。
    budget: budget::Meter,
    /// `--skin-debug`：每个绘制回调连跑两遍比 op。默认关 —— 打开就是每次绘制翻倍。
    paint_debug: std::cell::Cell<bool>,
    /// 已经报过的「paint 有副作用」的位置（`region#序号`）。一处只报一次。
    purity_seen: std::cell::RefCell<std::collections::HashSet<String>>,
    /// 一共报过几处。
    purity_count: std::cell::Cell<u64>,
    /// 有多少次绘制因为读了 `App.now()` 而没进缓存。命中率旁边要报它 ——
    /// 不然一个动画皮肤会把命中率压到个位数，看着像缓存坏了。
    paint_volatile: std::cell::Cell<u64>,
    /// 上一次绘制回调抛的错。绘制期没有返回值能带错误（`Option<PaintOut>` 里
    /// 的 `None` 意思是「没注册」），错误本来只往 stderr 打一行。`--ui-lint`
    /// 要在表里标出是哪一格挂了，所以顺手留一份，由 [`ScriptHost::take_paint_error`]
    /// 取走。
    paint_error: std::cell::RefCell<Option<String>>,
    /// 脏位：脚本改了外观（`App.ui.style`）。外壳在每个入口返回后取一次，
    /// 和 `bridge.edited` 同一个套路 —— 脚本里改状态，外壳统一收尾。
    dirty: std::rc::Rc<std::cell::Cell<u8>>,
    /// 已经往 stderr 打过的「区域 + 错误」。绘制跑在 `WM_PAINT` 里，一条坏画法
    /// 每帧都会抛 —— 不去重就是每秒几百行同样的栈。热重载时清空（皮肤改好了
    /// 再改坏，还得能看见）。
    paint_error_seen: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl Default for ScriptHost {
    fn default() -> Self {
        Self::new().expect("Lua::new_with with safe stdlib cannot fail")
    }
}

impl ScriptHost {
    /// Lua 堆的上限。一个每帧拼字符串的皮肤能把进程吃光，这条对脚本和皮肤
    /// 都值。超了 mlua 抛 `not enough memory`，被入口接住记日志。
    pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;

    pub fn new() -> Result<Self, ScriptError> {
        // No os/io/package/debug: the only I/O a script gets is what
        // the bridge hands it. coroutine/string/math/table/utf8 keep
        // ordinary scripting (patterns, format, sort) working.
        let libs = StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8;
        let lua = Lua::new_with(libs, Default::default())?;
        lua.set_memory_limit(Self::MEMORY_LIMIT)?;
        let host = Self {
            lua,
            paint_cache: std::cell::RefCell::default(),
            paint_doc: std::cell::RefCell::default(),
            budget: budget::Meter::default(),
            paint_debug: std::cell::Cell::new(false),
            purity_seen: std::cell::RefCell::default(),
            purity_count: std::cell::Cell::new(0),
            paint_volatile: std::cell::Cell::new(0),
            paint_error: std::cell::RefCell::default(),
            paint_error_seen: std::cell::RefCell::default(),
            dirty: std::rc::Rc::new(std::cell::Cell::new(0)),
        };
        host.budget.install(&host.lua)?;
        host.build_api()?;
        Ok(host)
    }

    /// 打开绘制纯度检查（`--skin-debug`）。默认关：它让每次绘制翻倍。
    pub fn set_paint_debug(&self, on: bool) {
        self.paint_debug.set(on);
    }

    /// 到目前为止报过几处「paint 有副作用」。`--skin-debug` 退出时打一行。
    pub fn paint_impurity_count(&self) -> u64 {
        self.purity_count.get()
    }

    /// 有多少次绘制读了 `App.now()` 而没进缓存。`--skin-debug` 的总账要报它。
    pub fn paint_volatile_count(&self) -> u64 {
        self.paint_volatile.get()
    }

    /// 用了多少 / 上限，字节。给 `--skin-debug` 和测试看。
    pub fn memory(&self) -> (usize, usize) {
        (self.lua.used_memory(), Self::MEMORY_LIMIT)
    }

    /// 还排着几件待办。
    pub fn pending_jobs(&self) -> usize {
        jobs::pending(&self.lua)
    }

    /// 宿主时钟：从 `new()` 算起的毫秒数，单调。`App.now()` 读的就是它。
    pub fn now_ms(&self) -> u64 {
        timers::now(&self.lua)
    }

    /// 最早的定时器还有多久到期（绝对时刻，同一把时钟）。`None` = 没有定时器，
    /// 宿主据此决定要不要装 `WM_TIMER`。
    pub fn next_timer_due(&self) -> Option<u64> {
        timers::next_due(&self.lua)
    }

    /// 活着的定时器数量，给 `--skin-debug` 看。
    pub fn timer_count(&self) -> usize {
        timers::len(&self.lua)
    }

    /// 因为超过 [`timers::MAX_TIMERS`] 被拒的登记次数。
    pub fn dropped_timers(&self) -> usize {
        timers::dropped(&self.lua)
    }

    /// 点掉所有到期的定时器，返回真跑了几发。
    ///
    /// 报错的定时器**当场注销**：一个每 100 ms 报错的皮肤会刷满日志，而它
    /// 本来也不会自己好起来。重复定时器的下一次到期在调用前就推进了，所以
    /// 回调里 `App.cancel(自己的 id)` 一定生效。
    pub fn run_timers(&self, bridge: &mut dyn Bridge) -> usize {
        let due = timers::take_due(&self.lua, timers::now(&self.lua), timers::MAX_FIRES_PER_PASS);
        let mut ran = 0;
        for (id, f) in due {
            // 日志必须在 bridge 还装着的**里面**记（和 `fire` 一样）：
            // `with_bridge` 一返回 CurrentBridge 就是 None，`log_via_bridge`
            // 只能退回 stderr。
            let r = self.with_bridge(bridge, || {
                let _budget = self.arm(budget::SCRIPT);
                if let Err(e) = f.call::<()>(()) {
                    log_via_bridge(&self.lua, &format!("timer {id}: {e}"));
                    return Err(e);
                }
                Ok(())
            });
            match r {
                Ok(()) => ran += 1,
                Err(_) => {
                    timers::cancel(&self.lua, id);
                }
            }
        }
        ran
    }

    /// 因为超上限被丢掉的件数。一直是 0 才正常。
    pub fn dropped_jobs(&self) -> usize {
        jobs::dropped(&self.lua)
    }

    /// Where `App.storage` persists (`storage.json` inside).
    pub fn set_storage_dir(&mut self, dir: impl Into<PathBuf>) {
        let mut cache = StorageCache::default();
        cache.dir = Some(dir.into());
        self.lua.set_app_data(RefCell::new(cache));
    }

    /// 取走脏位（取完清零）。外壳在**每个**入口返回后调一次，和 `bridge.edited`
    /// 同一个套路：脚本里改状态，外壳统一收尾。
    pub fn take_dirty(&self) -> u8 {
        let bits = self.dirty.get();
        self.dirty.set(0);
        bits
    }

    /// 运行时外观覆盖（`App.ui.style`）：颜色 / 度量 / 形状 / 字体四段。
    ///
    /// 外壳合成外观时**最后一步**盖上去 —— 出厂值 → `themes/<name>.ini` →
    /// 这一层，只有三层。回一份克隆而不是借用：外壳要在装主题的过程中同时动
    /// `self.docs`，借着一份 `RefCell` 的守卫会把那条路堵死。
    pub fn style_overrides(&self) -> style::Overrides {
        style::overrides_of(&self.lua)
    }

    /// 记下当前主题名 —— `App.theme.name()` 读它。
    pub fn set_theme_name(&self, name: &str) {
        let _ = style::set_theme_name(&self.lua, name);
    }

    /// 绘制期日志的落点（[`LogSink`]）。装一次，之后 `print` 和 `App.log`
    /// 在没有 bridge 的时候都走它。
    pub fn set_log_sink(&self, sink: std::sync::Arc<dyn Fn(&str) + 'static>) {
        self.lua.set_app_data(LogSink(sink));
    }

    fn registry(&self, key: &str) -> Table {
        self.lua
            .named_registry_value::<Table>(key)
            .expect("registry tables installed in build_api")
    }

    fn build_api(&self) -> Result<(), ScriptError> {
        // The bridge slot every callback reads, and the storage
        // cache every storage method reads.
        self.lua.set_app_data(CurrentBridge(None));
        self.lua.set_app_data(RefCell::new(StorageCache::default()));
        self.lua.set_app_data(RefCell::new(jobs::Jobs::default()));
        self.lua.set_app_data(RefCell::new(timers::Timers::default()));
        self.lua.set_app_data(ChunkName(RefCell::new(String::new())));
        style::install(&self.lua, std::rc::Rc::clone(&self.dirty))?;
        self.lua.set_named_registry_value(REG_EVENTS, self.lua.create_table()?)?;
        self.lua.set_named_registry_value(REG_COMMANDS, self.lua.create_table()?)?;
        self.lua.set_named_registry_value(REG_KEYS, self.lua.create_table()?)?;

        // The chunk _ENV: reads fall through to the real globals
        // (string/table/math), writes stay local to scripts.
        let env = self.lua.create_table()?;
        let mt = self.lua.create_table()?;
        mt.set("__index", self.lua.globals())?;
        env.set_metatable(Some(mt))?;

        // Bodies call b0 (a module-level fn, so callbacks stay
        // 'static); this only seals the Ok(...).
        macro_rules! bridged {
            ($lua:ident, $body:expr) => {{ let _ = &$lua; Ok($body) }};
        }

        let app = self.lua.create_table()?;
        app.set("version", self.lua.create_function(|_, ()| Ok(API_VERSION))?)?;
        app.set(
            "alert",
            self.lua.create_function(|lua, msg: String| {
                bridged!(lua, b0(lua)?.get().alert(&msg))
            })?,
        )?;
        app.set(
            // `log` 走 `log_via_bridge` 而不是 `bridged!`：绘制回调里没有
            // bridge，而那正是最想打日志的地方。以前这里会抛 `no bridge`，
            // 整个回调中止、一条 op 都不上屏 —— 症状是「皮肤什么都没画」，
            // 比不打日志难查得多。现在绘制期照样能打（落点是 [`LogSink`]）。
            "log",
            self.lua.create_function(|lua, msg: String| {
                log_via_bridge(lua, &msg);
                Ok(())
            })?,
        )?;
        app.set(
            "prompt",
            self.lua.create_function(
                |lua, (title, initial): (String, Option<String>)| {
                    bridged!(lua, b0(lua)?.get().prompt(&title, initial.as_deref().unwrap_or("")))
                },
            )?,
        )?;

        let doc = self.lua.create_table()?;
        doc.set(
            "name",
            self.lua.create_function(|lua, ()| bridged!(lua, b0(lua)?.get().doc_name()))?,
        )?;
        doc.set(
            "path",
            self.lua.create_function(|lua, ()| bridged!(lua, b0(lua)?.get().doc_path()))?,
        )?;
        doc.set(
            "line_count",
            self.lua
                .create_function(|lua, ()| bridged!(lua, b0(lua)?.get().line_count()))?,
        )?;
        doc.set(
            "line",
            self.lua
                .create_function(|lua, line: usize| bridged!(lua, b0(lua)?.get().line_text(line)))?,
        )?;
        doc.set(
            "line_len",
            self.lua.create_function(|lua, line: usize| {
                bridged!(lua, b0(lua)?.get().line_text(line).map(|t| t.len()))
            })?,
        )?;
        doc.set(
            "text",
            self.lua.create_function(|lua, ()| bridged!(lua, b0(lua)?.get().doc_text()))?,
        )?;
        doc.set(
            "span_text",
            self.lua.create_function(
                |lua, (s1, c1, e1, c2): (usize, usize, usize, usize)| {
                    bridged!(lua, b0(lua)?.get().span_text((s1, c1), (e1, c2)))
                },
            )?,
        )?;
        doc.set(
            "insert",
            self.lua.create_function(
                |lua, (line, col, text): (usize, usize, String)| {
                    bridged!(lua, b0(lua)?.get().insert_at((line, col), &text))
                },
            )?,
        )?;
        doc.set(
            "replace",
            self.lua.create_function(
                |lua, (s1, c1, e1, c2, text): (usize, usize, usize, usize, String)| {
                    bridged!(lua, b0(lua)?.get().replace_span((s1, c1), (e1, c2), &text))
                },
            )?,
        )?;
        doc.set(
            "delete",
            self.lua.create_function(
                |lua, (s1, c1, e1, c2): (usize, usize, usize, usize)| {
                    bridged!(lua, b0(lua)?.get().replace_span((s1, c1), (e1, c2), ""))
                },
            )?,
        )?;
        // 元信息：一张**复用**的表（和 `ctx` 一样，别存下来比旧值）。
        // 状态栏每帧每个格子都要问，逐次建表就是每帧十几次分配。
        let meta = self.lua.create_table()?;
        let mt = meta.clone();
        doc.set(
            "meta",
            self.lua.create_function(move |lua, ()| {
                let m = b0(lua)?.get().doc_meta();
                mt.set("encoding", m.encoding)?;
                mt.set("bom", m.bom)?;
                mt.set("eol", m.eol)?;
                mt.set("syntax", m.syntax)?;
                mt.set("bytes", m.bytes)?;
                mt.set("lines", m.lines)?;
                mt.set("read_only", m.read_only)?;
                mt.set("modified", m.modified)?;
                mt.set("large", m.large)?;
                Ok(mt.clone())
            })?,
        )?;
        app.set("doc", doc)?;

        // 视口：同样是复用的一张表。
        let view = self.lua.create_table()?;
        let vt = view.clone();
        view.set(
            "info",
            self.lua.create_function(move |lua, ()| {
                let v = b0(lua)?.get().view_info();
                vt.set("top_line", v.top_line)?;
                vt.set("top_col", v.top_col)?;
                vt.set("rows", v.rows)?;
                vt.set("cols", v.cols)?;
                vt.set("caret_line", v.caret_line)?;
                vt.set("caret_col", v.caret_col)?;
                Ok(vt.clone())
            })?,
        )?;
        view.set(
            "scroll_to",
            self.lua.create_function(|lua, (line, col): (usize, Option<usize>)| {
                jobs::push_scroll(lua, line, col.unwrap_or(0))
            })?,
        )?;
        app.set("view", view)?;

        let sel = self.lua.create_table()?;
        sel.set(
            "count",
            self.lua.create_function(|lua, ()| bridged!(lua, b0(lua)?.get().sel_count()))?,
        )?;
        sel.set(
            "get",
            self.lua.create_function(|lua, index: Option<usize>| {
                bridged!(lua, {
                    match b0(lua)?.get().sel_range(index.unwrap_or(0)) {
                        Some(((s1, c1), (e1, c2))) => (s1, c1, e1, c2).into_lua_multi(lua)?,
                        None => MultiValue::new(),
                    }
                })
            })?,
        )?;
        sel.set(
            "text",
            self.lua.create_function(|lua, ()| bridged!(lua, b0(lua)?.get().sel_text()))?,
        )?;
        sel.set(
            "set",
            self.lua.create_function(
                |lua, (s1, c1, e1, c2): (usize, usize, usize, usize)| {
                    bridged!(lua, b0(lua)?.get().sel_set((s1, c1), (e1, c2)))
                },
            )?,
        )?;
        app.set("sel", sel)?;

        // M3: registrations — writes into the hidden registry tables,
        // no bridge needed. Each record carries the chunk it came from so a
        // reload can drop exactly that chunk's registrations.
        app.set(
            "on",
            self.lua.create_function(|lua, (event, f): (String, Function)| {
                let events = lua.named_registry_value::<Table>(REG_EVENTS)?;
                let list: Table = match events.get(event.as_str())? {
                    Value::Nil => {
                        let t = lua.create_table()?;
                        events.set(event.as_str(), t.clone())?;
                        t
                    }
                    Value::Table(t) => t,
                    _ => return Err(mlua::Error::runtime("event registry corrupted")),
                };
                let record = lua.create_table()?;
                record.set("n", current_chunk(lua)?)?;
                record.set("f", f)?;
                list.push(record)?;
                Ok(())
            })?,
        )?;
        app.set(
            "command",
            self.lua.create_function(|lua, (name, f): (String, Function)| {
                let commands = lua.named_registry_value::<Table>(REG_COMMANDS)?;
                let record = lua.create_table()?;
                record.set("n", current_chunk(lua)?)?;
                record.set("f", f)?;
                commands.set(name, record)?;
                Ok(())
            })?,
        )?;
        app.set(
            "keymap",
            self.lua.create_function(|lua, (gesture, name): (String, String)| {
                if parse_gesture(&gesture).is_none() {
                    return Err(mlua::Error::runtime(format!(
                        "bad gesture {gesture:?}: {GESTURE_HELP}"
                    )));
                }
                let keys = lua.named_registry_value::<Table>(REG_KEYS)?;
                let record = lua.create_table()?;
                record.set("n", current_chunk(lua)?)?;
                record.set("c", name)?;
                keys.set(gesture, record)?;
                Ok(())
            })?,
        )?;

        // M2: 待办队列。回调里只入队 —— 真正跑在 `run_queue` 里，那时回调已经
        // 返回、`EditorState` 的借用也结束了。见 `jobs` 模块的说明。
        app.set(
            "run",
            self.lua.create_function(|lua, name: String| {
                if name.is_empty() || name.len() > 128 {
                    return Err(mlua::Error::runtime("command name must be 1..128 chars"));
                }
                jobs::push_command(lua, name)
            })?,
        )?;
        app.set(
            "defer",
            self.lua.create_function(|lua, f: Function| jobs::push_func(lua, f))?,
        )?;

        // 还排着几件待办。给脚本自己的诊断用（`--skin-debug` 也读它）。
        app.set(
            "pending",
            self.lua.create_function(|lua, ()| Ok(jobs::pending(lua)))?,
        )?;

        // 时间与定时器：沙箱没有 `os`，这一对是脚本能拿到的唯一时钟。
        app.set(
            "now",
            self.lua.create_function(|lua, ()| {
                // 在绘制回调里读时钟 = 这份画法每毫秒都不一样，告诉 `paint`
                // 别缓存它（否则那条靠心跳扫的线会冻住）。
                let _ = lua.set_named_registry_value(paint::REG_VOLATILE, true);
                Ok(timers::now(lua))
            })?,
        )?;
        app.set(
            "timer",
            self.lua.create_function(
                |lua, (ms, f, repeat): (u64, Function, Option<bool>)| {
                    if ms == 0 || ms > timers::MAX_PERIOD_MS {
                        return Err(mlua::Error::runtime(format!(
                            "interval must be 1..={} ms",
                            timers::MAX_PERIOD_MS
                        )));
                    }
                    let chunk = current_chunk(lua)?;
                    timers::add(lua, &chunk, ms, repeat.unwrap_or(false), f)
                },
            )?,
        )?;
        app.set(
            "cancel",
            self.lua.create_function(|lua, id: u32| Ok(timers::cancel(lua, id)))?,
        )?;

        // 任务栏进度:和外部工具完成时用的是同一块状态,脚本可以自己驱动
        // (启动置 indeterminate,跑的时候报刻度,结束 normal / error,完事 none)。
        // 意图档 API:绘制回调里没有 bridge,见 `b0`。
        app.set(
            "taskbar",
            self.lua.create_function(
                |lua, (state, completed, total): (String, Option<u64>, Option<u64>)| {
                    let state = TaskbarState::parse(&state).ok_or_else(|| {
                        mlua::Error::runtime(
                            "taskbar state must be none, indeterminate, normal, error or paused",
                        )
                    })?;
                    // Omitted values mean a full bar, which is what a
                    // completion notice wants: `App.taskbar("error")`.
                    let completed = completed.unwrap_or(1);
                    let total = total.unwrap_or(1);
                    if total == 0 {
                        return Err(mlua::Error::runtime("taskbar total must be > 0"));
                    }
                    bridged!(
                        lua,
                        b0(lua)?
                            .get()
                            .taskbar_progress(state, completed.min(total), total)
                    )
                },
            )?,
        )?;

        // M4: the persistent string table.
        app.set(
            "storage",
            self.lua.create_function(|_, name: String| {
                if name.is_empty() || name.len() > 64 {
                    return Err(mlua::Error::runtime("storage name must be 1..64 chars"));
                }
                Ok(StorageTable(name))
            })?,
        )?;

        // 皮肤的画布/上下文都挂在 `App.ui` 上，建一次就复用。
        self.build_ui_api(&app)?;
        env.set("App", app)?;
        // A GUI app's stdout is a void; scripts' print goes to the
        // log with everything else.
        //
        // 走 `log_via_bridge` 而不是 `bridged!`：**绘制回调里没有 bridge**
        // （那是重入，见 `paint`），而 `print` 恰恰是画不出来时第一个想用的
        // 东西。以前这里会抛 `no bridge`，整个回调中止、一条 op 都不上屏 ——
        // 症状是「皮肤什么都没画」，比不打日志难查得多。现在没有 bridge 就
        // 直接写 stderr，消息照样到得了脚本日志里。
        env.set(
            "print",
            self.lua.create_function(|lua, msg: String| {
                log_via_bridge(lua, &msg);
                Ok(())
            })?,
        )?;

        self.lua.set_named_registry_value(REG_ENV, env)?;
        Ok(())
    }

    /// Installs `bridge` for the duration of `f`.
    fn with_bridge<R>(
        &self,
        bridge: &mut dyn Bridge,
        f: impl FnOnce() -> mlua::Result<R>,
    ) -> Result<R, ScriptError> {
        // SAFETY: the pointer's validity is this frame (see
        // BridgePtr); a fat pointer's layout carries no trait-object
        // lifetime.
        let ptr = unsafe {
            let p: *mut (dyn Bridge + '_) = bridge;
            std::mem::transmute::<*mut (dyn Bridge + '_), *mut (dyn Bridge + 'static)>(p)
        };
        self.lua.set_app_data(CurrentBridge(Some(BridgePtr(ptr))));
        let r = f();
        self.lua.set_app_data(CurrentBridge(None));
        r.map_err(ScriptError::from)
    }

    /// Loads (runs the top level of) a script chunk. Registrations
    /// made by the chunk persist in this host.
    ///
    /// **Reloading the same name replaces that chunk's registrations** rather
    /// than adding to them: `purge` drops everything the previous run of this
    /// name registered, then the chunk runs again. A skin edited three times
    /// still gets one `doc.saved` per save.
    ///
    /// **先编译、再清旧的**：热重载时正在编辑的 `skin.lua` 十次有九次是语法错的，
    /// 先 `purge` 就会把还在工作的那一份一起清掉 —— 屏幕上立刻退回内置外观，而
    /// 你只是想改个颜色。编译失败时旧的那一套原封不动。
    pub fn load(
        &self,
        name: &str,
        source: &str,
        bridge: &mut dyn Bridge,
    ) -> Result<(), ScriptError> {
        self.load_in("", name, source, bridge)
    }

    /// 同上，但这块脚本属于包 `pkg`。
    ///
    /// 归属只影响**清理**：同名的块照样整块替换，但 [`Self::purge_package`]
    /// 能把一个包所有面文件登记的东西一次拿干净。裸脚本（`<exe>/scripts/*.lua`）
    /// 和旧式皮肤传空串，行为与以前一模一样。
    pub fn load_in(
        &self,
        pkg: &str,
        name: &str,
        source: &str,
        bridge: &mut dyn Bridge,
    ) -> Result<(), ScriptError> {
        let env = self
            .lua
            .named_registry_value::<Table>(REG_ENV)
            .expect("env installed in build_api");
        let function = self
            .lua
            .load(source)
            .set_name(name)
            .set_environment(env)
            .into_function()?;
        let chunk = qualified(pkg, name);
        self.purge(&chunk);
        // 回调表换了，缓存里的 op 列表全都作废 —— 留着就是拿旧脚本的画法画新脚本。
        self.paint_cache.borrow_mut().clear();
        set_current_chunk(&self.lua, &chunk);
        // 皮肤文件本身也是脚本：一个 `while true do end` 写在顶层同样能把 UI
        // 线程卡死，所以装载也装预算。
        let _budget = self.arm(budget::SCRIPT);
        let r = self.with_bridge(bridge, || function.call(()));
        set_current_chunk(&self.lua, "");
        r
    }

    /// 给这次调用装一份指令预算，守卫活到作用域结束。见 [`budget`]。
    ///
    /// 调用点一律写 `let _budget = ...` —— 写成 `let _ =` 会当场析构，预算白装。
    fn arm(&self, instructions: u32) -> budget::Armed {
        self.budget.arm(instructions)
    }

    /// 上一次入口调用实际烧掉多少条指令（粗到 1000）。给测试和 `--skin-debug`。
    pub fn budget_used(&self) -> u32 {
        self.budget.used()
    }

    /// Drops every registration `chunk` left behind. Empty names are ignored —
    /// a host used without `load` has nothing to purge.
    fn purge(&self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        self.purge_where(&|c| c == chunk);
    }

    /// 按谓词清。`purge` 精确匹配一条块名，[`Self::purge_package`] 匹配整个包。
    ///
    /// 谓词收的是**块名**（可能带包前缀），调用方自己决定怎么比。
    fn purge_where(&self, pred: &impl Fn(&str) -> bool) {
        // 换了脚本就重新给每个错误一次露脸的机会 —— 否则「改坏 → 改好 → 又改坏
        // 成同一个样子」第二次就一声不响了。
        self.paint_error_seen.borrow_mut().clear();
        // Events: each key holds an array of `{n, f}` records.
        let events = self.registry(REG_EVENTS);
        let mut names: Vec<String> = Vec::new();
        for kv in events.clone().pairs::<String, Value>() {
            if let Ok((name, _)) = kv {
                names.push(name);
            }
        }
        for name in names {
            let Ok(Value::Table(list)) = events.get::<Value>(name.as_str()) else {
                continue;
            };
            let mut keep: Vec<Value> = Vec::new();
            for rec in list.clone().sequence_values::<Value>() {
                let Ok(rec) = rec else { continue };
                if !pred(&record_chunk(&rec)) {
                    keep.push(rec);
                }
            }
            let _ = list.clear();
            for rec in keep {
                let _ = list.push(rec);
            }
        }
        // Commands, keymaps, 画法, 按下回调 and 悬停声明: one record per key.
        //
        // `REG_PAINTS` 以前不在这张表里 —— 那时画法存的是裸函数，没有块名，
        // 想清也清不掉；改完的 `skin.lua` 里删掉一条 `s:paint`，旧回调会一直
        // 留在表里。现在画法也带块名了，跟着一起清。
        for table in [
            REG_COMMANDS,
            REG_KEYS,
            crate::paint::REG_PAINTS,
            crate::paint::REG_PRESS,
            crate::paint::REG_HINTS,
        ] {
            let reg = self.registry(table);
            let mut keys: Vec<String> = Vec::new();
            for kv in reg.clone().pairs::<String, Value>() {
                if let Ok((key, value)) = kv
                    && pred(&record_chunk(&value))
                {
                    keys.push(key);
                }
            }
            for key in keys {
                let _ = reg.set(key, Value::Nil);
            }
        }
        // Timers: 不按块清的话，改一次皮肤就多一组定时器，越改越忙。
        timers::purge_where(&self.lua, pred);
        // 待办队列: `Job::Func` 攥着这个块的 Lua 闭包，重载/卸载时不摘掉，
        // 旧闭包既可能还跑一发，也让 GC 收不走那一整片状态。
        jobs::purge_where(&self.lua, pred);
        // 脚本窗格 / 状态栏段：数组，一条声明一项。不清的话重载一次就多一块
        // 窗格、多一格状态栏。`s:status` 本身是整条替换，但改完的 `skin.lua` 里
        // 那一行要是删了，旧的那些只能靠这里清掉。
        for table in [
            crate::paint::REG_PANES,
            crate::paint::REG_STATUS,
            crate::paint::REG_TOOLBAR,
        ] {
            if let Ok(list) = self.lua.named_registry_value::<Table>(table) {
                let mut keep: Vec<Value> = Vec::new();
                for rec in list.clone().sequence_values::<Value>() {
                    let Ok(rec) = rec else { continue };
                    if !pred(&record_chunk(&rec)) {
                        keep.push(rec);
                    }
                }
                let _ = list.clear();
                for rec in keep {
                    let _ = list.push(rec);
                }
            }
        }
    }

    /// 把一个包登记的东西全拿回去，返回清掉了多少条。
    ///
    /// 「禁用一个包」/「关掉按需包的窗格」的验收线都是「和从来没装过一样」：
    /// 靠 [`registry::count`] 前后各数一次，再加上一次 Lua 全量 GC，把闭包、
    /// 表、画布 op 这些**看得见的注册之外的内存**也还回去。
    pub fn purge_package(&self, pkg: &str) -> registry::Counts {
        // 空包名是裸脚本的归属，不是「一个包」。整片清掉会把用户的
        // `scripts/*.lua` 一起带走。
        if pkg.is_empty() {
            return registry::Counts::default();
        }
        let before = registry::count(&self.lua);
        self.purge_where(&|chunk| pkg_of(chunk) == pkg);
        // 画法可能已经换了，缓存里的 op 列表跟着作废。
        self.paint_cache.borrow_mut().clear();
        // 注册表摘掉之后，Lua 对象只是「不可达」，还没真的还内存。跑满两轮
        // 全量 GC：第一轮收完当前周期，第二轮把带 __gc 的对象也收干净。
        let _ = self.lua.gc_collect();
        let _ = self.lua.gc_collect();
        registry::Counts::removed(before, registry::count(&self.lua))
    }

    /// 现在一共登记了多少条。
    pub fn counts(&self) -> registry::Counts {
        registry::count(&self.lua)
    }

    /// Runs every handler registered for `event`, in registration
    /// order. A failing handler is logged through the bridge and the
    /// walk continues — one bad handler must not take down the rest
    /// or the shell.
    ///
    /// 老名字与新名字互为别名，两边都触发（见 [`alias_of`]）。
    pub fn trigger(&self, event: &str, bridge: &mut dyn Bridge) {
        let _ = self.with_bridge(bridge, || {
            self.fire(event)?;
            if let Some(alias) = alias_of(event) {
                self.fire(alias)?;
            }
            Ok(())
        });
    }

    /// 一个事件名的 handler 全跑一遍。
    fn fire(&self, event: &str) -> mlua::Result<()> {
        let events = self.registry(REG_EVENTS);
        let Value::Table(list) = events.get::<Value>(event)? else {
            return Ok(());
        };
        for rec in list.sequence_values::<Value>() {
            let Some(f) = record_func(&rec?) else {
                continue;
            };
            let _budget = self.arm(budget::SCRIPT);
            if let Err(e) = f.call::<()>(()) {
                log_via_bridge(&self.lua, &format!("{event} handler: {e}"));
            }
        }
        Ok(())
    }

    /// Calls a registered command. `false` when there is no such
    /// command or it failed; a failure is logged through the bridge
    /// while it is still installed, not propagated.
    ///
    /// 没有同名脚本命令时落到宿主的内置命令表（[`Bridge::run_command`]）——
    /// `App.run("file.save")` 走的就是这条。
    pub fn call_command(&self, name: &str, bridge: &mut dyn Bridge) -> bool {
        let commands = self.registry(REG_COMMANDS);
        let Some(f) = commands.get::<Value>(name).ok().and_then(|v| record_func(&v)) else {
            return bridge.run_command(name);
        };
        matches!(
            self.with_bridge(bridge, || {
                let _budget = self.arm(budget::SCRIPT);
                match f.call::<()>(()) {
                    Ok(()) => Ok(Some(true)),
                    Err(e) => {
                        // Still inside with_bridge, so this log
                        // goes through the installed bridge.
                        log_via_bridge(&self.lua, &format!("command {name}: {e}"));
                        Ok(Some(false))
                    }
                }
            }),
            Ok(Some(true))
        )
    }

    /// 派发待办队列：`App.run` 排的命令、`App.defer` 排的闭包。
    ///
    /// 宿主在**回调返回之后、bridge 还在**的那一刻调它 —— 那时
    /// `EditorState` 的借用已经结束，内置命令可以安全地跑。返回真跑了几件。
    ///
    /// 两道上限都在 [`jobs`]：入队时挡（一次调用灌进无数条），派发时也挡
    /// （命令 A 跑 B 再跑 A 是另一种死循环）。到上限还没清空就整队丢掉，
    /// 免得下一帧接着跑同样的循环。
    pub fn run_queue(&self, bridge: &mut dyn Bridge) -> usize {
        let mut ran = 0;
        while ran < jobs::MAX_JOBS {
            let Some(job) = jobs::pop(&self.lua) else { break };
            match job {
                jobs::Job::Command(name) => {
                    let ok = self.call_command(&name, bridge);
                    if !ok {
                        log_via_bridge(&self.lua, &format!("App.run: no command {name:?}"));
                    }
                }
                jobs::Job::Func(f) => {
                    let _budget = self.arm(budget::SCRIPT);
                    let _ = self.with_bridge(bridge, || f.call::<()>(()));
                }
                jobs::Job::Scroll { line, col } => {
                    bridge.scroll_to(line, col);
                }
            }
            ran += 1;
        }
        if ran >= jobs::MAX_JOBS {
            log_via_bridge(
                &self.lua,
                &format!("App.run: {MAX} jobs in one turn, the rest were dropped", MAX = jobs::MAX_JOBS),
            );
            jobs::discard(&self.lua);
        }
        ran
    }

    /// Registered command names, sorted — for menu rows and dispatch
    /// tables, which want a stable order.
    pub fn command_names(&self) -> Vec<String> {
        self.command_names_excluding(&[])
    }

    /// 同上，但把 `hide` 里那些包注册的命令排除掉。
    ///
    /// 按需启动的小程序（游戏、向导）用 `App.command` 注册的是**内部命令**：
    /// 方向键、暂停、重开走 `App.keymap` / `ui.on`，本来就不该一个人占扩展菜单
    /// 八行。排除只是不进菜单，`call_command` 照样能调它们。
    pub fn command_names_excluding(&self, hide: &[String]) -> Vec<String> {
        let commands = self.registry(REG_COMMANDS);
        let mut names = Vec::new();
        for kv in commands.pairs::<String, Value>() {
            let Ok((name, value)) = kv else { continue };
            let chunk = record_chunk(&value);
            let owner = pkg_of(&chunk);
            if !owner.is_empty() && hide.iter().any(|p| p == owner) {
                continue;
            }
            names.push(name);
        }
        names.sort();
        names
    }

    /// 某块脚本窗格是不是它所属包留下的**最后一块**；是就返回那个包 id。
    ///
    /// 按需包关掉最后一块窗格 = 用户玩完了，整包该卸载。包里还有别的窗格时
    /// 返回 `None`：关一块不该把另一块一起带走。裸脚本（块名没有包前缀）也
    /// 返回 `None` —— 它们没有「整包卸载」这件事。
    pub fn sole_pane_package(&self, pane_id: &str) -> Option<String> {
        let list = self
            .lua
            .named_registry_value::<Table>(crate::paint::REG_PANES)
            .ok()?;
        let mut records: Vec<(String, String)> = Vec::new();
        for rec in list.sequence_values::<Value>() {
            let Ok(Value::Table(rec)) = rec else { continue };
            let pkg = pkg_of(&record_chunk(&Value::Table(rec.clone()))).to_string();
            if pkg.is_empty() {
                continue;
            }
            let id = rec
                .get::<Option<String>>("id")
                .ok()
                .flatten()
                .unwrap_or_default();
            records.push((id, pkg));
        }
        let owner = records
            .iter()
            .find(|(id, _)| id == pane_id)
            .map(|(_, pkg)| pkg.clone())?;
        let siblings = records.iter().filter(|(_, pkg)| *pkg == owner).count();
        (siblings == 1).then_some(owner)
    }

    /// All registered key bindings (gesture, command name).
    pub fn key_bindings(&self) -> Vec<(Gesture, String)> {
        let keys = self.registry(REG_KEYS);
        let mut out = Vec::new();
        for kv in keys.pairs::<String, Value>() {
            let Ok((gesture, value)) = kv else { continue };
            let name = match &value {
                Value::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
                Value::Table(t) => t.get::<Option<String>>("c").ok().flatten().unwrap_or_default(),
                _ => continue,
            };
            if let Some(g) = parse_gesture(&gesture)
                && !name.is_empty()
            {
                out.push((g, name));
            }
        }
        out
    }

    /// The command a gesture is bound to, if any.
    pub fn lookup_key(&self, ctrl: bool, shift: bool, alt: bool, vk: u32) -> Option<String> {
        self.key_bindings().into_iter().find_map(|(g, name)| {
            (g.ctrl == ctrl && g.shift == shift && g.alt == alt && g.vk == vk).then_some(name)
        })
    }

    /// One-shot convenience from M1/M2: load a chunk (on this host —
    /// its registrations persist like any other load).
    pub fn run(&self, name: &str, source: &str, bridge: &mut dyn Bridge) -> Result<(), ScriptError> {
        self.load(name, source, bridge)
    }

    pub fn run_file(
        &self,
        path: &std::path::Path,
        bridge: &mut dyn Bridge,
    ) -> Result<(), ScriptError> {
        self.run_file_in("", path, bridge)
    }

    /// 同上，但这份文件属于包 `pkg`。皮肤目录里的 `skin.lua` 走这条 ——
    /// 它没有清单，包名就用目录名（和 `--packages` 扫出来的旧式 id 同一个词）。
    pub fn run_file_in(
        &self,
        pkg: &str,
        path: &std::path::Path,
        bridge: &mut dyn Bridge,
    ) -> Result<(), ScriptError> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| ScriptError(format!("{}: {e}", path.display())))?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("script");
        self.load_in(pkg, name, &source, bridge)
    }
}

/// Logs through the bridge of the current entry point (errors during
/// `trigger`/`call_command` must not unwind); outside one it asks the host's
/// [`LogSink`], and with neither it writes stderr.
fn log_via_bridge(lua: &Lua, msg: &str) {
    if let Some(mut slot) = lua.app_data_mut::<CurrentBridge>() {
        if let Some(b) = slot.0.take() {
            b.get().log(msg);
            slot.0 = Some(b);
            return;
        }
    }
    match lua.app_data_ref::<LogSink>() {
        Some(sink) => (sink.0)(msg),
        None => eprintln!("[lua] {msg}"),
    }
}

/// Takes the bridge of the current entry point, puts it right back
/// (later callbacks in the same entry point need it too), and hands
/// the caller its copy. Called outside any entry point the take
/// fails loudly.
fn no_storage() -> mlua::Error {
    mlua::Error::runtime("storage is not initialized")
}

fn b0(lua: &Lua) -> Result<BridgePtr, mlua::Error> {
    let b = lua
        .app_data_mut::<CurrentBridge>()
        .and_then(|mut slot| slot.0.take())
        .ok_or_else(|| mlua::Error::runtime("no bridge: called outside a host entry point"))?;
    lua.set_app_data(CurrentBridge(Some(b)));
    Ok(b)
}
