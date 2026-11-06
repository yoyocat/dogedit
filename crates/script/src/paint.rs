//! 绘制档：Lua 在 `WM_PAINT` 里画东西。
//!
//! 这一档**没有 bridge**。回调拿到的只有两样东西：一块画布 [`Canvas`] 和一份
//! 只读上下文 [`PaintCtx`]。它读不到文档、拿不到 `&mut EditorState`、也跑不了
//! 命令 —— 这正是「绘制期不会重入」的保证：不是靠约定，是靠没给工具。
//!
//! 回调的返回值是「这一块我画了没有」：
//!
//! - `true`：内置画法让位，这一块完全由脚本负责；
//! - `false` / `nil`：交回内置画法（皮肤只想改某一格时这么写）；
//! - 抛错：记一条日志，交回内置画法。**一个坏皮肤不该让窗口白屏**。
//!
//! 画布收集的是 [`UiOp`]，不是 GDI 调用。回放层在 `dg-app` 里（它需要 `HDC`），
//! 所以这里能脱离窗口逐条断言：`c:fill(...)` 之后 ops 里就是一条 `Fill`。
//!
//! **画布和上下文都是复用的**：`build_ui_api` 建一块，之后每次绘制只清空/改写
//! 字段。所以稳定状态下一次绘制不新建 userdata、不新建表、不动字符串容量 ——
//! 唯一的新分配是 `PaintOut` 里那份 ops 拷贝，而它是**按需**的（调用方缓存起来
//! 之后就不再每帧问）。

use std::cell::RefCell;

use dg_render::{Color, Rect};
use dg_skin::{
    Align, Dir, Fit, FontId, ImageId, OpList, Point, PointsId, TextId, TextSource, UiOp, VAlign,
};
use mlua::{
    AnyUserData, FromLua, Function, Lua, Table, UserData, UserDataFields, UserDataMethods, Value,
};

use crate::{Bridge, ScriptError, ScriptHost};

/// 注册表：区域名 → 回调。
pub(crate) const REG_PAINTS: &str = "dogedit.paints";
/// 注册表：`ui.layout:pane{...}` 声明过的脚本窗格，**数组**（声明顺序就是页签顺序）。
pub const REG_PANES: &str = "dogedit.panes";
/// 注册表：`ui.layout:status{...}` 声明过的状态栏段，**数组**（声明顺序就是左右顺序）。
///
/// 和 `REG_PANES` 一样「一次声明就是一整条」：再调一次 `ui.layout:status` 是**整条替换**，
/// 不是往后追加 —— 热重载重跑整个 `skin.lua`，追加会越跑越长。
pub const REG_STATUS: &str = "dogedit.status";
/// 注册表：`ui.layout:toolbar{...}` 声明过的工具栏按钮，**数组**（声明顺序就是插入顺序）。
///
/// 和上面两张表相反，这里是**追加**：工具栏那四十几个按钮是用户自己的
/// （`[ToolBar]` 里存着他的顺序），不是皮肤的地盘，整条替换会把他的排布抹掉。
/// 热重载按 chunk 清（`ScriptHost::purge`），所以重跑一遍还是那几个。
pub const REG_TOOLBAR: &str = "dogedit.toolbar";
/// 注册表：token 名 → `"#RRGGBB"`。**一张表用到底**，换主题时清空重灌 ——
/// 因为 `ctx.theme` 指向的就是它。
pub(crate) const REG_PALETTE: &str = "dogedit.palette";
/// 注册表：唯一那块画布。
const REG_CANVAS: &str = "dogedit.canvas";
/// 注册表：唯一那个上下文。
const REG_CTX: &str = "dogedit.paintctx";
/// 「这一帧读了时钟」的标记。`App.now()` 会把它置位，`paint` 读完就清 —— 读了
/// 时间的画法每毫秒都不一样，缓存它等于把动画冻住。
pub(crate) const REG_VOLATILE: &str = "dogedit.paint.volatile";
/// `ui.on(region, event, fn)` 的回调：键是 `"tab:press"` 这种，值是
/// `{n = 块名, f = 函数}` —— 和 `App.on` 一套记录，`load` 重载时同样被换掉。
pub(crate) const REG_PRESS: &str = "dogedit.press";
/// `ui.paint:hint(region, spec)` 的声明：键是区域名，值是
/// `{n = 块名, cursor = "...", tip = "..."}`。
///
/// 和 `REG_PRESS` 不同，这里的值**不是函数**：悬停每秒问几十次，为两个常量进一次
/// Lua 不值当，而且回调能写出死循环，指针卡住比画错更难受。见 [`dg_skin::HintSpec`]。
pub(crate) const REG_HINTS: &str = "dogedit.hints";

/// 回调这一次读了 `ctx` 里哪几样「缓存键放不下」的东西。
///
/// 缓存键是整数（区域/部件/状态/尺寸/DPI），但 `thumb` 和三个文字字段会独立地
/// 变：滑块在滚、状态栏的行列号每帧都变。把它们塞进键里，滚动条和状态栏就永远
/// 命中不了 —— 那正是 `@text` 占位符要解决的事。
///
/// 所以换个方向：**读过的才比对**。回调没读 `ctx.text`，op 列表就不可能是文字的
/// 函数，那份文字换了也照样命中；推荐写法 `c:text(r, "@text", …)` 正好不读。
/// 读了就老老实实比字符串 —— 正确性由比对兜住，不靠「皮肤作者不会按文字分支」
/// 这种假设。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Reads(u8);

impl Reads {
    pub const PART: u8 = 1 << 0;
    pub const TEXT: u8 = 1 << 1;
    pub const LABEL: u8 = 1 << 2;
    pub const VALUE: u8 = 1 << 3;
    pub const THUMB: u8 = 1 << 4;
    /// 读过 `ctx.doc`。快照每帧由外壳推一次，改一行代码就换一份，所以它和
    /// 前面五样一样「独立于缓存键变化」—— 没读它的画法不受存盘/改名影响。
    pub const DOC: u8 = 1 << 5;
    /// 读过 `ctx.scroll`。滚动位置每滚一格就变，但**没读它的画法不该因此重算** ——
    /// 这也是它不进缓存键的原因（键里放不下第二份浮点状态，而且没必要）。
    pub const SCROLL: u8 = 1 << 6;
    /// 读过 `ctx.bar`。轨道宽高只在窗格尺寸变的时候变，那是 `rect`，本来就在键里；
    /// 这一位是为了「同一块窗格换 bar 宽度」这种热重载后的边界。
    pub const BAR: u8 = 1 << 7;

    /// 读没读过这一样。
    pub const fn has(self, bit: u8) -> bool {
        self.0 & bit != 0
    }

    fn add(&mut self, bit: u8) {
        self.0 |= bit;
    }
}

/// 绘制结果缓存。
///
/// 键是 [`dg_skin::CacheKey`]（20 字节、`Copy`、散列是五次乘法）；键里放不下的
/// 那几样由条目自己比对，见 [`Reads`]。上限一到就整表清空 —— 活的工作集是
/// 「一个区域里的部件数」，几十条，离上限很远，所以清空很少发生。
#[derive(Default)]
pub(crate) struct PaintCache {
    map: std::collections::HashMap<dg_skin::CacheKey, Cached>,
    hits: u64,
    misses: u64,
}

/// 缓存上限。42 格的工具栏 + 9 格状态栏 + 一屏页签，也就一百出头。
const PAINT_CACHE_CAP: usize = 512;

/// 一份算好的绘制结果，连带算出它时用的那些「键里放不下」的输入。
struct Cached {
    /// 算这份 op 时读了哪几样。比对只看这几样。
    reads: Reads,
    /// 这一块的矩形（含左上角）。**无条件比对**：页签条和状态栏的 op 用的是
    /// 客户区绝对坐标，两块尺寸相同、状态相同的格子换个位置画法就不同了。
    rect: (f32, f32, f32, f32),
    part: String,
    text: String,
    label: String,
    value: String,
    thumb: (f32, f32, f32, f32),
    bar: (f32, f32, f32, f32),
    scroll: f32,
    /// 算这份 op 时的文档快照。只有读过 `ctx.doc` 的条目才比对它。
    doc: Option<std::rc::Rc<PaintDoc>>,
    out: std::rc::Rc<PaintOut>,
}

impl Cached {
    /// 这份结果还能不能用。只比回调真读过的东西 —— 没读过的不可能影响 op 列表。
    fn matches(&self, input: &PaintInput, doc: Option<&PaintDoc>) -> bool {
        let reads = self.reads;
        self.rect == input.rect
            && (!reads.has(Reads::PART) || self.part == input.part)
            && (!reads.has(Reads::TEXT) || self.text == input.text)
            && (!reads.has(Reads::LABEL) || self.label == input.label)
            && (!reads.has(Reads::VALUE) || self.value == input.value)
            && (!reads.has(Reads::THUMB) || self.thumb == input.thumb)
            && (!reads.has(Reads::BAR) || self.bar == input.bar)
            && (!reads.has(Reads::SCROLL) || self.scroll == input.scroll)
            && (!reads.has(Reads::DOC) || self.doc.as_deref() == doc)
    }
}

impl PaintCache {
    /// 查。命中返回那份结果的引用（`Rc` 克隆，不复制 op 列表）。
    fn get(
        &mut self,
        key: &dg_skin::CacheKey,
        input: &PaintInput,
        doc: Option<&PaintDoc>,
    ) -> Option<std::rc::Rc<PaintOut>> {
        match self.map.get(key) {
            Some(entry) if entry.matches(input, doc) => {
                self.hits += 1;
                Some(std::rc::Rc::clone(&entry.out))
            }
            _ => {
                self.misses += 1;
                None
            }
        }
    }

    fn put(
        &mut self,
        key: dg_skin::CacheKey,
        input: &PaintInput,
        doc: Option<&PaintDoc>,
        out: &PaintOut,
        shared: std::rc::Rc<PaintOut>,
    ) {
        if self.map.len() >= PAINT_CACHE_CAP && !self.map.contains_key(&key) {
            self.map.clear();
        }
        self.map.insert(
            key,
            Cached {
                reads: out.reads,
                rect: input.rect,
                part: input.part.clone(),
                text: input.text.clone(),
                label: input.label.clone(),
                value: input.value.clone(),
                thumb: input.thumb,
                bar: input.bar,
                scroll: input.scroll,
                doc: doc.map(|d| std::rc::Rc::new(d.clone())),
                out: shared,
            },
        );
    }

    /// 换脚本了：所有 op 列表都可能变，一条都不留。
    pub(crate) fn clear(&mut self) {
        self.map.clear();
    }

    /// 丢掉一个区域的全部条目。
    ///
    /// 点击之后调：handler 可能改了画法要用的状态（计数器、选中项、展开位），
    /// 而宿主看不见那些变量 —— 缓存的前提「输入没变则 op 不变」在点击这一侧
    /// 不成立。一个区域的条目最多几十条，一次点击丢一次不值得计较。
    pub(crate) fn forget_region(&mut self, region: dg_skin::Region) {
        let id = region as u32;
        self.map.retain(|k, _| k.region() != id);
    }

    /// (命中, 未命中)。给测试和 `--skin-debug` 看命中率。
    pub(crate) fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

/// 部件名 → 缓存键里的 8 位。撞了不致命：真读过 `ctx.part` 的皮肤会按字符串
/// 比出来、当未命中处理（只慢，不会错）。
fn part_bits(part: &str) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for b in part.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    ((h >> 16) ^ h) as u16
}

/// 绘制档的文档快照：`ctx.doc` 拿到的那张表。
///
/// 外壳每帧推一次（[`ScriptHost::set_paint_doc`]）—— 绘制回调没有 bridge，问不了
/// `App.doc.meta()`，所以只能把答案提前摆好。快照**只在内容变了的时候换**，
/// 缓存里读过 `ctx.doc` 的条目才跟着失效（见 [`Reads::DOC`]）。
///
/// 比 [`DocMeta`](crate::DocMeta) **少一个 `bytes`**，是故意的：算总字节要走字节
/// 索引，那东西一改就整表 O(N) 重建，放进每帧的拍快照等于给打字加一道全量扫描
/// （`crates/core/src/index.rs` 的 `ByteIndex::ensure`）。要总字节用 Intent 档的
/// `App.doc.meta()`，那是「问一次算一次」，不是每帧。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaintDoc {
    /// 编码名，原样（`UTF-8` / `GBK` …），和 `DocMeta::encoding` 同一个词。
    pub encoding: String,
    pub bom: bool,
    /// `LF` / `CRLF` / `CR`。
    pub eol: String,
    /// 语法键（`rs` / `py` …），没挂语法是空串。
    pub syntax: String,
    /// 总行数，和 `DocMeta::lines` 同一个值。
    pub lines: usize,
    pub read_only: bool,
    pub modified: bool,
    /// 大文件模式。
    pub large: bool,
}

/// 编辑器字体的度量：`ctx.metrics`。
///
/// 为什么给这几样：皮肤要**对齐文字**就绕不开「一个字多宽、一行多高」。内置
/// 行号栏是靠 `x_width` 右对齐的（`x + width - 3 - 位数 × x_width`），皮肤
/// 想画出一模一样的列，就得有同一个数 —— 让它自己量一遍字体会和编辑器差半个
/// 像素，一列数字立刻看着歪。
///
/// 全是**这一帧已经算好的**值，取值不额外测量。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    /// 半角字符宽度（等宽字体的单元格宽）。
    pub x_width: f32,
    /// 全角字符宽度。
    pub cjk_width: f32,
    /// 字体的行高（字面本身，不含行距）。
    pub font_height: f32,
    /// 一行占的高度（含行距）—— 折行带里每段就是这个步长。
    pub line_height: f32,
    /// 行号栏的总宽（`theme.gutter_x`）：编辑区里正文从这条线右边开始。
    pub gutter_x: f32,
    /// 行号那一格的左边（书签条、诊断条在它左边，可能宽度为 0）。
    pub line_number_x: f32,
    /// 行号那一格的宽度。内置行号栏是「右边留 3 px 再右对齐」，皮肤想和它对齐
    /// 就用 `line_number_x + line_number_w - 3 - 位数 × x_width`。
    ///
    /// 单给 `gutter_x` 不够：那是**整列**的宽，行号只占其中一格，右边还有折叠、
    /// 图标、边界线。照着 `gutter_x` 右对齐会把数字推到正文边上。
    pub line_number_w: f32,
}

/// `ui.layout:pane{...}` 声明的一个脚本窗格：一块挂在停靠栏里的、皮肤自己画的窗口。
///
/// **只声明结构，不带绘制**：画法是 `ui.paint:on("pane.<id>", fn)`，和别的区域一样
/// 走缓存、走「paint 是纯函数」那条规矩。合成一张表看着方便，代价是
/// 绘制回调混进声明期，重载和缓存都会变得说不清。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaneSpec {
    /// 唯一名。也是绘制区域名（`pane.<id>`）和 `ctx.part`。
    pub id: String,
    /// `left` / `right` / `top` / `bottom`。
    pub side: String,
    /// 页签标题。没写就是 `id`。
    pub title: String,
    /// 页签图标：工具栏精灵格号，0 = 不给图标。
    pub icon: i32,
    /// 这一侧的厚度（DIP），0 = 用停靠栏自己的记忆值。
    pub size: f32,
    /// 竖直滚动条的宽度（DIP）。0 = 不要滚动条（滚轮仍然可用）。
    ///
    /// 声明的是**轨道占多宽**，不是「画不画」—— 轨道和滑块的几何由宿主算，
    /// 皮肤填色就行。内容高不是在这里声明的：它在 `s:paint` 里 `return 数字`，
    /// 因为内容高是画的时候才知道的（时钟、日志、列表都会变）。
    pub bar: f32,
}

impl PaneSpec {
    /// 这个窗格的绘制区域名。
    pub fn region(&self) -> String {
        format!("pane.{}", self.id)
    }
}

/// 一个 `ui.layout:toolbar` 按钮，已经过声明期校验。
///
/// 宿主只认这四样：画哪一格图、提示写什么、点下去跑哪个命令、插在谁后面。
/// **没有 paint**：按钮的画法仍走 `ui.paint:on("toolbar.button", fn)`，和其它区域
/// 一样 —— 声明管结构，画法管像素。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolbarButton {
    /// 工具栏精灵格号（和 `ui.layout:pane` 的 `icon`、`ui.layout:status` 的图标同一张图）。
    pub icon: i32,
    /// 悬停提示。没写就用命令名 —— 空提示等于没提示。
    pub tip: String,
    /// 点下去跑的命令名。脚本命令优先，没有就落内置命令表。
    pub command: String,
    /// 插在这个命令 id 的那一格后面；`0` = 末尾。
    pub after: u32,
}

/// `ctx.rows` 里的一行：编辑区一个**可见逻辑行**（折行的那几段算一行）。
///
/// 为什么按逻辑行而不是按折行段：gutter 的七个组件也是按逻辑行画的，一格
/// 折行带就是一行；按段给的话皮肤得自己再拼回去，而它拿不到「这一段是第几段」
/// 之外的信息。真要逐段处理，用 `y + i * (h / rows)` 自己切。
///
/// 字段全是**这一帧已经算好的**值 —— 建这张表不额外扫描文档。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaintRow {
    /// 逻辑行号，0 基。
    pub line: u32,
    /// 这一行整条折行带的顶边，相对编辑区客户区左上角，DIP。
    pub y: f32,
    /// 整条折行带的高度（`rows × 行高`），DIP。
    pub h: f32,
    /// 折了几段。没折行是 1。
    pub rows: u32,
    /// 行号列上显示的那个号，**从 1 起**（折叠之后的行号，和眼睛看到的一致）。
    pub display: u32,
    /// 光标在这一行。
    pub active: bool,
    /// 这一行是折叠头（`fold_start`）且被折起来了。
    pub folded: bool,
    /// 这一行能折但没折。
    pub foldable: bool,
    /// 有书签。
    pub bookmark: bool,
    /// 行注记图标（0 = 没有）。
    pub icon: u8,
    /// 行修改状态：0 没改过 / 1 改过没存 / 2 存过了。
    pub state: u8,
    /// 上一行没有行尾符 —— 行号栏画 `~` 而不是数字（老式编辑器那个「这行是
    /// 上一行接下来的」记号）。
    pub tilde: bool,
    /// 这一行的行尾符，内部编码：0 没有（末行）/ 1 CRLF / 2 LF / 3 CR。
    /// 交给脚本时转成字符串（`""` / `"CRLF"` / `"LF"` / `"CR"`），和
    /// `ctx.doc.eol` 同一套词 —— 数字编码是存储细节，不该漏给皮肤。
    pub eol: u8,
}

/// 行尾符的内部编码 → 脚本看见的词。0 是「没有行尾符」（末行），给空串。
fn eol_name(kind: u8) -> &'static str {
    match kind {
        1 => "CRLF",
        2 => "LF",
        3 => "CR",
        _ => "",
    }
}

/// 一次绘制请求：内置画法算好的几何与文字，交给脚本。
#[derive(Clone, Debug, Default)]
pub struct PaintInput {
    /// 这一块的矩形（DIP 与设备像素同值：调用方已经按 DPI 算好）。
    pub rect: (f32, f32, f32, f32),
    /// 块里那个「会动的部件」的矩形，同样在 DIP 里。滚动条是滑块，其它区域
    /// 没有部件，等于 `rect` —— 脚本一律按「有就用、没有就和 rect 一样」写，
    /// 不必先判断区域名。
    pub thumb: (f32, f32, f32, f32),
    /// 竖直滚动条的整条轨道（DIP）。**没有滚动条时是空矩形（`w == 0`）**，所以
    /// 皮肤写 `if ctx.bar.w > 0 then … end` 一处判断就够，不必和 `rect` 比。
    ///
    /// 轨道和滑块都由宿主算：拖拽、翻页、夹取都要按同一份几何来，只有宿主
    /// 知道内容多高、视口多高。皮肤只负责填色（`c:fill(ctx.bar, …)` /
    /// `c:fill(ctx.thumb, …)`）。
    pub bar: (f32, f32, f32, f32),
    /// 竖直滚动位置（DIP），已经夹进 `[0, 内容高 - 视口高]`。
    ///
    /// **由皮肤自己减**，回放层不替它平移。理由和别处一样：`ctx` 说状态，皮肤
    /// 说像素。好处是固定表头免费 —— 那一行不加 `ctx.scroll` 就是了，而「整块
    /// 平移」的写法做不到这件事。别的区域恒 0。
    pub scroll: f32,
    /// 9 个状态位，见 [`dg_skin::State`]。
    pub state: u16,
    /// 部件名（`"Encoding"`、`"LineCol"`……），皮肤按它分支。
    pub part: String,
    /// `@text`。
    pub text: String,
    /// `@label`。
    pub label: String,
    /// `@value`。
    pub value: String,
    /// 该区域解析到的 DPI，脚本算尺寸时用得上。
    pub dpi: i32,
    /// 这一块在本区域里的身份，**同一区域内必须唯一**。**只进缓存键，不暴露
    /// 给脚本** —— 部件名哈希只有 8 位，42 格工具栏会撞（实测 42 个命令名撞出
    /// 3 对），序号是天然的区分。状态格用字段序号、页签用页签序号、工具栏用
    /// 命令 id（比按钮位置稳：用户改工具栏顺序时 id 不变）。
    pub index: u16,
}

/// 一次绘制的结果。
#[derive(Clone, Debug, Default)]
pub struct PaintOut {
    /// 脚本发出来的绘制指令。
    pub ops: Vec<UiOp>,
    /// 指令里引用的字面量，下标就是 [`TextId`]。
    pub strings: Vec<String>,
    /// 指令里引用的字体名，下标就是 [`FontId`]。空表示「用这个区域自己的字体」。
    pub fonts: Vec<String>,
    /// 指令里引用的点集，下标就是 [`PointsId`]。
    pub points: Vec<Point>,
    /// 回调说了「我画了」。
    pub handled: bool,
    /// 内容总高（DIP）。**只有 `s:paint` 的回调返回数字时才有值**。
    ///
    /// 数字在 Lua 里本来就算真值、本来就等于「我画了」，所以拿它兼职报内容高度
    /// 是纯扩展：老皮肤写 `return true` 一字不改，写 `return 1200` 就多告诉宿主
    /// 一件事。宿主拿它算滑块长度和最大滚动量（`skinpane`）。
    pub content: Option<f32>,
    /// 回调读了 `ctx` 里哪几样键放不下的东西。缓存靠它决定要比对什么。
    pub reads: Reads,
}

impl PaintOut {
    /// 两次跑出来的第一处不同，`None` 表示画出来的东西一模一样。
    ///
    /// 只比**画出来的**：`reads` 是缓存记账，不是输出，两遍之间本来就可能不一样
    /// （第一遍读过什么就记什么，第二遍读到的是同一批，但顺序/时机不必相同）。
    pub fn describe_diff(&self, other: &PaintOut) -> Option<String> {
        if self.handled != other.handled {
            return Some(format!(
                "返回值 {} vs {}",
                self.handled, other.handled
            ));
        }
        if self.content != other.content {
            return Some(format!(
                "内容高 {:?} vs {:?}",
                self.content, other.content
            ));
        }
        let n = self.ops.len().max(other.ops.len());
        for i in 0..n {
            match (self.ops.get(i), other.ops.get(i)) {
                (Some(a), Some(b)) if a == b => {}
                (Some(a), None) => return Some(format!("第 {i} 条 op 只有第一遍有：{a:?}")),
                (None, Some(b)) => return Some(format!("第 {i} 条 op 只有第二遍有：{b:?}")),
                (Some(a), Some(b)) => return Some(format!("第 {i} 条 op：{a:?} → {b:?}")),
                (None, None) => {}
            }
        }
        if self.strings != other.strings {
            return Some(format!(
                "字面量表 {} 条 vs {} 条",
                self.strings.len(),
                other.strings.len()
            ));
        }
        if self.fonts != other.fonts {
            return Some(format!(
                "字体表 {} 条 vs {} 条",
                self.fonts.len(),
                other.fonts.len()
            ));
        }
        if self.points != other.points {
            return Some(format!(
                "点集 {} 段 vs {} 段",
                self.points.len(),
                other.points.len()
            ));
        }
        None
    }

    /// 交给回放层用的紧凑列表。
    pub fn compacted(&self) -> OpList {
        let mut list = OpList::with_capacity(self.ops.len());
        for op in &self.ops {
            list.push(*op);
        }
        list.compact();
        list
    }
}

/// 画布：脚本往它上面加指令。
///
/// 全程只有一块（在 Lua 注册表里），每次绘制前清空。所以脚本没法「接着上一帧
/// 画」，也就没有跨帧残留这种坑。
pub struct Canvas {
    ops: RefCell<Vec<UiOp>>,
    strings: RefCell<Vec<String>>,
    fonts: RefCell<Vec<String>>,
    points: RefCell<Vec<Point>>,
}

impl Canvas {
    fn new() -> Self {
        Self {
            ops: RefCell::new(Vec::with_capacity(32)),
            strings: RefCell::new(Vec::new()),
            fonts: RefCell::new(Vec::new()),
            points: RefCell::new(Vec::new()),
        }
    }

    fn reset(&self) {
        self.ops.borrow_mut().clear();
        self.strings.borrow_mut().clear();
        self.fonts.borrow_mut().clear();
        self.points.borrow_mut().clear();
    }

    fn push(&self, op: UiOp) {
        self.ops.borrow_mut().push(op);
    }

    /// 字面量进表，返回下标。同一个词复用同一个下标，省得一张表里十份 "OK"。
    fn intern_text(&self, text: &str) -> TextId {
        let mut list = self.strings.borrow_mut();
        if let Some(i) = list.iter().position(|s| s == text) {
            return TextId(i as u16);
        }
        let id = TextId(list.len() as u16);
        list.push(text.to_string());
        id
    }

    fn intern_font(&self, name: &str) -> FontId {
        let mut list = self.fonts.borrow_mut();
        if let Some(i) = list.iter().position(|f| f == name) {
            return FontId(i as u16);
        }
        let id = FontId(list.len() as u16);
        list.push(name.to_string());
        id
    }

    fn intern_points(&self, pts: &[Point]) -> PointsId {
        let mut list = self.points.borrow_mut();
        let start = list.len() as u16;
        list.extend_from_slice(pts);
        PointsId {
            start,
            len: pts.len() as u16,
        }
    }

    fn snapshot(&self, handled: bool) -> PaintOut {
        PaintOut {
            ops: self.ops.borrow().clone(),
            strings: self.strings.borrow().clone(),
            fonts: self.fonts.borrow().clone(),
            points: self.points.borrow().clone(),
            handled,
            content: None,
            reads: Reads::default(),
        }
    }
}

impl UserData for Canvas {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // c:fill(rect, "#RRGGBB")
        methods.add_method("fill", |_, this, (rect, color): (RectUd, String)| {
            let color = parse_color(&color)?;
            this.push(UiOp::Fill {
                rect: rect.0,
                color,
            });
            Ok(())
        });

        // c:rect(rect, "#RRGGBB"[, width])
        methods.add_method(
            "rect",
            |_, this, (rect, color, width): (RectUd, String, Option<f32>)| {
                let color = parse_color(&color)?;
                this.push(UiOp::Rect {
                    rect: rect.0,
                    color,
                    width: width.unwrap_or(1.0),
                });
                Ok(())
            },
        );

        // c:round(rect, radius[, fill[, stroke[, stroke_w]]])
        methods.add_method(
            "round",
            |_,
             this,
             (rect, radius, fill, stroke, stroke_w): (
                RectUd,
                f32,
                Option<String>,
                Option<String>,
                Option<f32>,
            )| {
                let fill = match fill {
                    Some(c) => Some(parse_color(&c)?),
                    None => None,
                };
                let stroke = match stroke {
                    Some(c) => Some(parse_color(&c)?),
                    None => None,
                };
                this.push(UiOp::Round {
                    rect: rect.0,
                    radius,
                    fill,
                    stroke,
                    stroke_w: stroke_w.unwrap_or(1.0),
                });
                Ok(())
            },
        );

        // c:line(x1, y1, x2, y2, "#RRGGBB"[, width])
        methods.add_method(
            "line",
            |_,
             this,
             (x1, y1, x2, y2, color, width): (f32, f32, f32, f32, String, Option<f32>)| {
                let color = parse_color(&color)?;
                this.push(UiOp::Line {
                    a: Point::new(x1, y1),
                    b: Point::new(x2, y2),
                    color,
                    width: width.unwrap_or(1.0),
                });
                Ok(())
            },
        );

        // c:poly({ {x=,y=}, ... }, "#RRGGBB"[, width[, closed]])
        // 点也可以是 {x, y} 两个数，两种写法都收。
        methods.add_method(
            "poly",
            |_,
             this,
             (points, color, width, closed): (Table, String, Option<f32>, Option<bool>)| {
                let color = parse_color(&color)?;
                let mut pts = Vec::with_capacity(points.raw_len());
                for v in points.sequence_values::<Value>() {
                    pts.push(point_of(&v?)?);
                }
                let id = this.intern_points(&pts);
                this.push(UiOp::Poly {
                    points: id,
                    color,
                    width: width.unwrap_or(1.0),
                    closed: closed.unwrap_or(false),
                });
                Ok(())
            },
        );

        // c:circle(cx, cy, r[, fill[, stroke[, stroke_w]]])
        methods.add_method(
            "circle",
            |_,
             this,
             (cx, cy, r, fill, stroke, stroke_w): (
                f32,
                f32,
                f32,
                Option<String>,
                Option<String>,
                Option<f32>,
            )| {
                let fill = match fill {
                    Some(c) => Some(parse_color(&c)?),
                    None => None,
                };
                let stroke = match stroke {
                    Some(c) => Some(parse_color(&c)?),
                    None => None,
                };
                this.push(UiOp::Circle {
                    center: Point::new(cx, cy),
                    r,
                    fill,
                    stroke,
                    stroke_w: stroke_w.unwrap_or(1.0),
                });
                Ok(())
            },
        );

        // c:gradient(rect, from, to[, dir])  dir = "v"（默认）| "h"
        methods.add_method(
            "gradient",
            |_, this, (rect, from, to, dir): (RectUd, String, String, Option<String>)| {
                let from = parse_color(&from)?;
                let to = parse_color(&to)?;
                let dir = match dir.as_deref() {
                    Some("h") | Some("H") => Dir::H,
                    _ => Dir::V,
                };
                this.push(UiOp::Gradient {
                    rect: rect.0,
                    from,
                    to,
                    dir,
                });
                Ok(())
            },
        );

        // c:shadow(rect[, radius[, "#RRGGBB"[, blur]]])
        methods.add_method(
            "shadow",
            |_,
             this,
             (rect, radius, color, blur): (RectUd, Option<f32>, Option<String>, Option<f32>)| {
                let color = match color {
                    Some(c) => parse_color(&c)?,
                    None => Color::rgba(0, 0, 0, 96),
                };
                this.push(UiOp::Shadow {
                    rect: rect.0,
                    radius: radius.unwrap_or(0.0),
                    color,
                    blur: blur.unwrap_or(4.0),
                });
                Ok(())
            },
        );

        // c:text(rect, source, color[, opts])
        //   opts = { font=, align=, valign=, ellipsis=, clip= }
        methods.add_method(
            "text",
            |_, this, (rect, source, color, opts): (RectUd, String, String, Option<Table>)| {
                let color = parse_color(&color)?;
                let mut op = UiOp::Text {
                    rect: rect.0,
                    text: text_source(this, &source),
                    font: FontId(0),
                    color,
                    align: Align::Start,
                    valign: VAlign::Middle,
                    clip: None,
                    ellipsis: false,
                };
                if let Some(t) = opts {
                    if let UiOp::Text {
                        font,
                        align,
                        valign,
                        clip,
                        ellipsis,
                        ..
                    } = &mut op
                    {
                        if let Some(name) = t.get::<Option<String>>("font")? {
                            *font = this.intern_font(&name);
                        }
                        *align = match t.get::<Option<String>>("align")?.as_deref() {
                            Some("center") => Align::Center,
                            Some("right") | Some("end") => Align::End,
                            _ => Align::Start,
                        };
                        *valign = match t.get::<Option<String>>("valign")?.as_deref() {
                            Some("top") => VAlign::Top,
                            Some("bottom") => VAlign::Bottom,
                            _ => VAlign::Middle,
                        };
                        *ellipsis = t.get::<Option<bool>>("ellipsis")?.unwrap_or(false);
                        *clip = match t.get::<Option<RectUd>>("clip")? {
                            Some(r) => Some(r.0),
                            None => None,
                        };
                    }
                }
                this.push(op);
                Ok(())
            },
        );

        // c:image(rect, index, fit[, tint])
        methods.add_method(
            "image",
            |_, this, (rect, index, fit, tint): (RectUd, u16, Option<String>, Option<String>)| {
                let fit = match fit.as_deref() {
                    Some("stretch") => Fit::Stretch,
                    Some("tile") => Fit::Tile,
                    _ => Fit::Center,
                };
                let tint = match tint {
                    Some(c) => Some(parse_color(&c)?),
                    None => None,
                };
                this.push(UiOp::Image {
                    rect: rect.0,
                    src: ImageId(index),
                    tint,
                    fit,
                });
                Ok(())
            },
        );

        // c:clip(rect) / c:unclip()
        methods.add_method("clip", |_, this, rect: RectUd| {
            this.push(UiOp::Clip { rect: rect.0 });
            Ok(())
        });
        methods.add_method("unclip", |_, this, ()| {
            this.push(UiOp::Unclip);
            Ok(())
        });

        // c:spacer() —— 配方里的占位，回放时忽略；脚本用它做「分界」。
        methods.add_method("spacer", |_, this, ()| {
            this.push(UiOp::Spacer);
            Ok(())
        });

        // 指令条数，脚本自检用（比如「我到底画了几条」）。
        methods.add_method("count", |_, this, ()| Ok(this.ops.borrow().len()));
    }
}

/// 矩形，给脚本用的 `{x, y, w, h}`，带 `inset`/`outset`。
///
/// 用 userdata 而不是普通表：`ctx.rect:inset(4, 2)` 要能直接链下去，而表的
/// `__index` 每次都要查一次元表；userdata 的字段是编译期就定好的。
#[derive(Clone, Copy)]
pub struct RectUd(Rect);

impl UserData for RectUd {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("x", |_, this| Ok(this.0.x));
        fields.add_field_method_get("y", |_, this| Ok(this.0.y));
        fields.add_field_method_get("w", |_, this| Ok(this.0.w));
        fields.add_field_method_get("h", |_, this| Ok(this.0.h));
        fields.add_field_method_get("right", |_, this| Ok(this.0.x + this.0.w));
        fields.add_field_method_get("bottom", |_, this| Ok(this.0.y + this.0.h));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // r:inset(dx[, dy])
        methods.add_method("inset", |_, this, (dx, dy): (f32, Option<f32>)| {
            let dy = dy.unwrap_or(dx);
            Ok(RectUd(Rect::new(
                this.0.x + dx,
                this.0.y + dy,
                (this.0.w - 2.0 * dx).max(0.0),
                (this.0.h - 2.0 * dy).max(0.0),
            )))
        });
        methods.add_method("outset", |_, this, (dx, dy): (f32, Option<f32>)| {
            let dy = dy.unwrap_or(dx);
            Ok(RectUd(Rect::new(
                this.0.x - dx,
                this.0.y - dy,
                this.0.w + 2.0 * dx,
                this.0.h + 2.0 * dy,
            )))
        });
        // r:with_width(w) / r:with_height(h)：只改一个方向。
        methods.add_method("with_width", |_, this, w: f32| {
            Ok(RectUd(Rect::new(this.0.x, this.0.y, w, this.0.h)))
        });
        methods.add_method("with_height", |_, this, h: f32| {
            Ok(RectUd(Rect::new(this.0.x, this.0.y, this.0.w, h)))
        });
        // r:move(dx, dy)
        methods.add_method("move", |_, this, (dx, dy): (f32, f32)| {
            Ok(RectUd(Rect::new(
                this.0.x + dx,
                this.0.y + dy,
                this.0.w,
                this.0.h,
            )))
        });
        methods.add_method("to_string", |_, this, ()| {
            Ok(format!(
                "{},{},{},{}",
                this.0.x, this.0.y, this.0.w, this.0.h
            ))
        });
    }
}

// mlua 给 `T: UserData` 兜了 `IntoLua`，但**没有**兜 `FromLua`（它不知道要不要
// `Clone`），所以这里只补 `FromLua`。有了它，`c:fill(ctx.rect, ...)` 的签名就能
// 直接写 `RectUd`。
impl FromLua for RectUd {
    fn from_lua(value: Value, _: &Lua) -> mlua::Result<Self> {
        match value {
            Value::UserData(ud) => Ok(*ud.borrow::<RectUd>()?),
            // 字面表 `{ x = 0, y = 40, w = 8, h = 20 }`。`editor.overlay` 要它 ——
            // 那里的矩形是从行表算出来的，不是内置画法给的那一块，
            // `inset`/`with_width` 拼不出来。四个字段都可以省，省了当 0。
            Value::Table(t) => {
                let field = |name: &str| t.get::<Option<f32>>(name).map(|v| v.unwrap_or(0.0));
                Ok(RectUd(Rect::new(
                    field("x")?,
                    field("y")?,
                    field("w")?,
                    field("h")?,
                )))
            }
            other => Err(mlua::Error::FromLuaConversionError {
                from: other.type_name(),
                to: "Rect".to_string(),
                message: Some(
                    "要一个矩形：ctx.rect、ctx.rect:inset(...)，或 { x=, y=, w=, h= }".to_string(),
                ),
            }),
        }
    }
}

pub(crate) fn parse_color(text: &str) -> mlua::Result<Color> {
    dg_render::parse_hex_color(text)
        .ok_or_else(|| mlua::Error::RuntimeError(format!("颜色 {text:?} 不是 #RRGGBB")))
}

/// `"@text"` / `"@label"` / `"@value"` 是占位符，其余是字面量。
fn text_source(canvas: &Canvas, text: &str) -> TextSource {
    match text {
        "@text" => TextSource::Text,
        "@label" => TextSource::Label,
        "@value" => TextSource::Value,
        literal => TextSource::Literal(canvas.intern_text(literal)),
    }
}

/// `{x=1,y=2}` 和 `{1,2}` 都收。
fn point_of(value: &Value) -> mlua::Result<Point> {
    let Value::Table(t) = value else {
        return Err(mlua::Error::RuntimeError(
            "点要写成 {x=,y=} 或 {x,y}".to_string(),
        ));
    };
    let x: f32 = match t.get::<Option<f32>>(1)? {
        Some(v) => v,
        None => t.get("x")?,
    };
    let y: f32 = match t.get::<Option<f32>>(2)? {
        Some(v) => v,
        None => t.get("y")?,
    };
    Ok(Point::new(x, y))
}

/// 绘制上下文：内置画法算好的几何、状态与文字。
///
/// 全程只有一个，每次绘制改写字段（字符串 `clear` + `push_str`，不重新分配）。
pub struct PaintCtx {
    rect: Rect,
    thumb: Rect,
    bar: Rect,
    scroll: f32,
    state: u16,
    part: String,
    text: String,
    label: String,
    value: String,
    dpi: i32,
    /// `ctx.theme["status.bg"]` —— 指向注册表里那张调色板，表本身不复制。
    theme: Table,
    /// 这一帧的文档快照。外壳每帧推一次，`paint` 时按 `Rc` 挂上来（不复制）。
    doc: Option<std::rc::Rc<PaintDoc>>,
    /// 这一帧的可见行表。只有 `editor.overlay` / `editor.gutter` 挂，别的时候是空的。
    rows: Option<std::rc::Rc<Vec<PaintRow>>>,
    /// 编辑器字体的度量。同样只有编辑区那两个区域挂。
    metrics: Option<Metrics>,
    /// 这一帧回调读了哪几样。`set` 时清零，字段 getter 里置位 —— 缓存靠它
    /// 决定比对什么（见 [`Reads`]）。
    reads: std::cell::Cell<Reads>,
}

impl PaintCtx {
    /// 记一笔「这一帧读过它」。字段 getter 是 `&PaintCtx`，所以走 `Cell`。
    fn note(&self, bit: u8) {
        let mut reads = self.reads.get();
        reads.add(bit);
        self.reads.set(reads);
    }

    fn new(theme: Table) -> Self {
        Self {
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            thumb: Rect::new(0.0, 0.0, 0.0, 0.0),
            bar: Rect::new(0.0, 0.0, 0.0, 0.0),
            scroll: 0.0,
            state: 0,
            part: String::new(),
            text: String::new(),
            label: String::new(),
            value: String::new(),
            dpi: 96,
            theme,
            doc: None,
            rows: None,
            metrics: None,
            reads: std::cell::Cell::new(Reads::default()),
        }
    }

    fn set(&mut self, input: &PaintInput) {
        self.reads.set(Reads::default());
        self.rect = Rect::new(input.rect.0, input.rect.1, input.rect.2, input.rect.3);
        self.thumb = Rect::new(
            input.thumb.0,
            input.thumb.1,
            input.thumb.2,
            input.thumb.3,
        );
        self.bar = Rect::new(input.bar.0, input.bar.1, input.bar.2, input.bar.3);
        self.scroll = input.scroll;
        self.state = input.state;
        self.dpi = input.dpi;
        for (dst, src) in [
            (&mut self.part, &input.part),
            (&mut self.text, &input.text),
            (&mut self.label, &input.label),
            (&mut self.value, &input.value),
        ] {
            dst.clear();
            dst.push_str(src);
        }
    }

    /// 挂上这一帧的文档快照。`Rc` 克隆，不复制里面那几个字符串。
    fn set_doc(&mut self, doc: Option<std::rc::Rc<PaintDoc>>) {
        self.doc = doc;
    }

    /// 挂上这一帧的可见行表。`None` 清空 —— 别的区域不该看见上一帧的行。
    fn set_rows(&mut self, rows: Option<std::rc::Rc<Vec<PaintRow>>>) {
        self.rows = rows;
    }

    /// 挂上编辑器字体度量。和行表同进同出。
    fn set_metrics(&mut self, metrics: Option<Metrics>) {
        self.metrics = metrics;
    }
}

impl UserData for PaintCtx {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("rect", |_, this| Ok(RectUd(this.rect)));
        // `ctx.w` / `ctx.h`：`ctx.rect` 的宽高。看着和 `ctx.rect.w` 重复，但
        // 「铺满整块」是皮肤里最常写的一行（`c:fill({x=0,y=r.y,w=ctx.w,h=r.h})`），
        // 每次都要绕一层 `rect` 太别扭；计划 §4.4 的字段表也是按顶层列的。
        // 跟着 `rect` 走，所以不记 `reads` —— `rect` 本身总在缓存键里。
        fields.add_field_method_get("w", |_, this| Ok(this.rect.w));
        fields.add_field_method_get("h", |_, this| Ok(this.rect.h));
        // 下面这几样会独立于缓存键变化，所以读到哪样就记哪样。
        fields.add_field_method_get("thumb", |_, this| {
            this.note(Reads::THUMB);
            Ok(RectUd(this.thumb))
        });
        // `ctx.bar` / `ctx.scroll`：脚本窗格的滚动条。别的区域 `bar == rect`、
        // `scroll == 0`，所以「有就画、没有就不画」写成 `if ctx.bar.w > 0` 一处
        // 判断就够。
        fields.add_field_method_get("bar", |_, this| {
            this.note(Reads::BAR);
            Ok(RectUd(this.bar))
        });
        fields.add_field_method_get("scroll", |_, this| {
            this.note(Reads::SCROLL);
            Ok(this.scroll)
        });
        fields.add_field_method_get("part", |_, this| {
            this.note(Reads::PART);
            Ok(this.part.clone())
        });
        fields.add_field_method_get("text", |_, this| {
            this.note(Reads::TEXT);
            Ok(this.text.clone())
        });
        fields.add_field_method_get("label", |_, this| {
            this.note(Reads::LABEL);
            Ok(this.label.clone())
        });
        fields.add_field_method_get("value", |_, this| {
            this.note(Reads::VALUE);
            Ok(this.value.clone())
        });
        fields.add_field_method_get("dpi", |_, this| Ok(this.dpi));
        fields.add_field_method_get("theme", |_, this| Ok(this.theme.clone()));
        // `ctx.doc`：文档快照。**每读一次建一张表** —— 一次回调里一般只
        // `local d = ctx.doc` 一次，之后读字段；真在循环里反复读就是自己
        // 给自己加分配。没有快照（测试、第一帧之前）给一张空表，读字段得 nil
        // 而不是报错，皮肤写 `if ctx.doc.modified then` 不用先判空。
        fields.add_field_method_get("doc", |lua, this| {
            this.note(Reads::DOC);
            let t = lua.create_table()?;
            if let Some(d) = &this.doc {
                t.set("encoding", d.encoding.as_str())?;
                t.set("bom", d.bom)?;
                t.set("eol", d.eol.as_str())?;
                t.set("syntax", d.syntax.as_str())?;
                t.set("lines", d.lines)?;
                t.set("read_only", d.read_only)?;
                t.set("modified", d.modified)?;
                t.set("large", d.large)?;
            }
            Ok(t)
        });
        // `ctx.rows`：可见行表，只有 `editor.overlay` 有。**数组从 1 起**，
        // 和 Lua 的习惯一致；每读一次整张重建，所以皮肤该 `local rows = ctx.rows`
        // 拿一次，别在循环条件里反复读。
        //
        // 这一档不进缓存（见 [`ScriptHost::paint_rows`]），所以这里不记 `reads`。
        fields.add_field_method_get("rows", |lua, this| {
            let t = lua.create_table()?;
            let Some(rows) = &this.rows else {
                return Ok(t);
            };
            for (i, r) in rows.iter().enumerate() {
                let row = lua.create_table()?;
                row.set("line", r.line)?;
                row.set("y", r.y)?;
                row.set("h", r.h)?;
                row.set("rows", r.rows)?;
                row.set("display", r.display)?;
                row.set("active", r.active)?;
                row.set("folded", r.folded)?;
                row.set("foldable", r.foldable)?;
                row.set("bookmark", r.bookmark)?;
                row.set("icon", r.icon)?;
                row.set("state", r.state)?;
                row.set("tilde", r.tilde)?;
                row.set("eol", eol_name(r.eol))?;
                t.set(i + 1, row)?;
            }
            Ok(t)
        });
        // `ctx.metrics`：编辑器字体的度量。和 `ctx.rows` 一样只有编辑区那两个
        // 区域挂得上 —— 别的区域要它得先把度量从各自的内置画法里递出来。
        //
        // 没有度量时给一张空表（读字段得 nil），和 `ctx.doc` 同一个态度：
        // `if ctx.metrics.x_width then` 不用先判空。也不记 `reads` —— 度量跟着
        // 字体走，换字体本来就整屏重画。
        fields.add_field_method_get("metrics", |lua, this| {
            let t = lua.create_table()?;
            if let Some(m) = this.metrics {
                t.set("x_width", m.x_width)?;
                t.set("cjk_width", m.cjk_width)?;
                t.set("font_height", m.font_height)?;
                t.set("line_height", m.line_height)?;
                t.set("gutter_x", m.gutter_x)?;
                t.set("line_number_x", m.line_number_x)?;
                t.set("line_number_w", m.line_number_w)?;
            }
            Ok(t)
        });
        // `ctx.hot` / `ctx.active` / `ctx.pressed` …… 9 个布尔字段，名字与
        // `dg_skin::State::NAMES` 同序。用 9 个字段而不是一张表：字段访问不分配，
        // 一张表每帧建一次就够呛了。
        for (i, name) in dg_skin::State::NAMES.iter().enumerate() {
            let mask = 1u16 << i;
            fields.add_field_method_get(*name, move |_, this| Ok(this.state & mask != 0));
        }
    }
}

/// `App.ui.paint`：接管某个区域的画法。
///
/// 只管**像素**：画法、指针、提示。元素叫什么、长在哪是 [`LayoutApi`] 的事 ——
/// 这两样以前挂在同一个对象上，于是「只把页签圆角改掉」也得先读完整的布局文档。
struct PaintApi;

impl UserData for PaintApi {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // ui.paint:on("status.pane", function(c, ctx) ... end)
        //
        // 区域名当场校验：写错了应该在这一行报错，而不是「这条画法怎么永远
        // 不生效」。名字还会决定进不进缓存 —— 认不出来的名字以前是**静默不缓存**
        // （每帧重跑一遍 Lua，看起来能画、其实一直在白算），比画错难查得多。
        methods.add_method(
            "on",
            |lua, _, (region, callback): (String, Function)| {
                dg_skin::Region::parse(&region)
                    .map_err(|e| mlua::Error::runtime(e.to_string()))?;
                // 存成 `{ n = 块名, f = 回调 }` 而不是裸函数：注册表要能回答
                // 「这条画法是谁的」，拆包时按归属清掉。读的一侧走
                // `record_func`，两种形状都认。
                let record = lua.create_table()?;
                record.set("n", crate::current_chunk(lua)?)?;
                record.set("f", callback)?;
                paints(lua)?.set(region, record)?;
                Ok(())
            },
        );
        // ui.paint:off("status.pane")
        methods.add_method("off", |lua, _, region: String| {
            paints(lua)?.set(region, Value::Nil)?;
            Ok(())
        });
        // ui.paint:list() → 已经注册了回调的区域名（排过序）。
        methods.add_method("list", |lua, _, ()| {
            let paints = paints(lua)?;
            let mut names: Vec<String> = Vec::new();
            for pair in paints.pairs::<String, Value>() {
                let (name, _) = pair?;
                names.push(name);
            }
            names.sort();
            Ok(names)
        });
        // ui.paint:hint("toolbar.button", { cursor = "hand", tip = "@label" })
        //
        // 区域名和指针名都**当场**校验：写错了应该在这一行报错，而不是等到把鼠标
        // 移上去才发现「指针怎么不动」。名字表里没有的一律拒绝，不退回默认值。
        methods.add_method(
            "hint",
            |lua, _, (region, spec): (String, Table)| {
                // 查表用规范名：精确区域归一成小写词表名，`pane.<id>` 保留整个
                // 名字（两块窗格是两条提示，不能都存成 "custom"）。
                let key = dg_skin::Region::key(&region)
                    .map_err(|e| mlua::Error::runtime(e.to_string()))?;
                let region = dg_skin::Region::parse(&region)
                    .map_err(|e| mlua::Error::runtime(e.to_string()))?;
                let record = lua.create_table()?;
                record.set("n", crate::current_chunk(lua)?)?;
                if let Ok(Value::String(s)) = spec.get::<Value>("cursor") {
                    let cursor = dg_skin::Cursor::parse(&s.to_str()?)
                        .map_err(|e| mlua::Error::runtime(e.to_string()))?;
                    record.set("cursor", cursor.as_str())?;
                }
                if let Ok(Value::String(s)) = spec.get::<Value>("tip") {
                    // 提示只有本来没有提示的区域收。工具栏问 `cmdtext`，状态栏在
                    // 文字放不下时自己弹 —— 那两处再挂一条就是两个框叠在一起。
                    if !region.takes_skin_tip() {
                        return Err(mlua::Error::runtime(format!(
                            "{} does not take a tip: its tip belongs to the control \
                             (the toolbar asks cmdtext, the status bar shows one when \
                             the text does not fit). Only {} take a tip.",
                            region.as_str(),
                            "tab / dock.tab / pane.<id>"
                        )));
                    }
                    record.set("tip", s)?;
                }
                hints(lua)?.set(key, record)?;
                Ok(())
            },
        );
        // ui.paint:unhint("status.pane")
        methods.add_method("unhint", |lua, _, region: String| {
            let key = dg_skin::Region::key(&region)
                .map_err(|e| mlua::Error::runtime(e.to_string()))?;
            hints(lua)?.set(key, Value::Nil)?;
            Ok(())
        });
    }
}

/// `App.ui.layout`：声明有哪些元素、放在哪里、点了走哪个命令。
///
/// 这里**没有颜色、没有字体、没有回调**。外观按区域名在主题文件里寻址
/// （`pane.<id>.bg`），行为在 `App.command` 里注册 —— 元素和外观之间没有
/// 任何指针，所以换主题不用改一行布局。
struct LayoutApi;

impl UserData for LayoutApi {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // ui.layout:pane{ id = "clock", side = "left", title = "时钟", icon = 1325, size = 180 }
        //
        // 声明一块停靠窗格。**画法另给**：`ui.paint:on("pane.clock", fn)`。同名再声明
        // 一次是覆盖（热重载重跑整个文件时不会叠出两块）。
        methods.add_method("pane", |lua, _, spec: Table| {
            let record = pane_record(lua, &spec)?;
            let id: String = record.get("id")?;
            let list = panes_reg(lua)?;
            // 同名覆盖：重载时整个文件重跑一遍，不能越跑越多。
            if let Some((i, _)) = find_in_list(&list, "id", &id)? {
                list.set(i, record)?;
            } else {
                list.set(list.raw_len() + 1, record)?;
            }
            mark(lua, DIRTY_LAYOUT);
            Ok(())
        });
        // ui.layout:status{ { id = "pos", text = "%l:%c" }, { id = "msg", width = "fill" } }
        //
        // 声明整条状态栏，从左到右。**不带参数调一次就是读**：返回当前声明（没
        // 声明过就是空表），改完再传回去 —— 这样「在内置那条后面加一格时钟」
        // 不用把十三个字段抄一遍。
        //
        // 带参数是**整条替换**，不是追加：热重载重跑整个文件，追加会越跑越长。
        // 传空表 `{}` = 一条都不要（整条只剩底色）。
        methods.add_method("status", |lua, _, list: Option<Table>| {
            let reg = status_reg(lua)?;
            let Some(list) = list else {
                return Ok(Value::Table(status_public(lua, &reg)?));
            };
            let mut seen: Vec<String> = Vec::new();
            let mut fresh: Vec<Table> = Vec::new();
            for entry in list.sequence_values::<Table>() {
                let entry = entry?;
                let record = status_record(lua, &entry)?;
                let id: String = record.get("id")?;
                if !id.is_empty() {
                    if seen.contains(&id) {
                        return Err(mlua::Error::runtime(format!(
                            "ui.layout:status 里 {id:?} 出现了两次：段名同时是绘制区域名 \
                             status.{id}，两块用同一个名字会共用一份缓存"
                        )));
                    }
                    seen.push(id);
                }
                fresh.push(record);
            }
            let _ = reg.clear();
            for record in fresh {
                reg.set(reg.raw_len() + 1, record)?;
            }
            mark(lua, DIRTY_LAYOUT);
            Ok(Value::Nil)
        });
        // ui.layout:defaults("status") → 内置那十三个字段，可以 table.insert 完再传回去。
        //
        // 单独一个函数而不是 `ui.layout:status{ inherit = true, ... }`：那张表同时是数组和
        // 哈希，`#` 和 `ipairs` 都不可靠，而且读回来的时候 `inherit` 该不该留着
        // 说不清。分开就没有歧义。
        methods.add_method("defaults", |lua, _, what: Option<String>| {
            // 目前只有状态栏有「出厂那一套」。写成别的东西要当场报错 ——
            // 静默返回一份状态栏会让人以为工具栏也能整条替换。
            match what.as_deref() {
                None | Some("status") => {}
                Some(other) => {
                    return Err(mlua::Error::runtime(format!(
                        "ui.layout:defaults 只认 \"status\"，写的是 {other:?}"
                    )));
                }
            }
            let out = lua.create_table()?;
            for (i, seg) in dg_skin::statusline::default_segments().iter().enumerate() {
                let entry = lua.create_table()?;
                entry.set("id", seg.id.clone())?;
                match seg.width {
                    dg_skin::Width::Fill => entry.set("width", "fill")?,
                    dg_skin::Width::Fixed(dip) => entry.set("width", dip)?,
                    dg_skin::Width::Range { min, max } => {
                        let w = lua.create_table()?;
                        w.set("min", min)?;
                        w.set("max", max)?;
                        entry.set("width", w)?;
                    }
                    dg_skin::Width::Auto => {}
                }
                entry.set("priority", seg.priority)?;
                out.set(i + 1, entry)?;
            }
            Ok(out)
        });
        // ui.layout:toolbar{ icon = 44, tip = "计时", command = "skin.timer" }
        //
        // **追加**一个按钮，不是整条替换 —— 理由见 `REG_TOOLBAR`。不带参数调一次
        // 是读：返回已经声明过的那几个，方便先读出来再整批改。
        methods.add_method("toolbar", |lua, _, spec: Option<Table>| {
            let reg = toolbar_reg(lua)?;
            let Some(spec) = spec else {
                return Ok(Value::Table(toolbar_public(lua, &reg)?));
            };
            let record = toolbar_record(lua, &spec)?;
            reg.set(reg.raw_len() + 1, record)?;
            mark(lua, DIRTY_LAYOUT);
            Ok(Value::Nil)
        });
        // ui.layout:update("status.clock", { text = "已保存" })
        //
        // 改一个**已经声明过**的元素，不用把整条声明重写一遍。这是「推」那一半
        // —— 事件来了才写，不轮询。改的字段走和声明期同一套校验。
        methods.add_method("update", |lua, _, (name, patch): (String, Table)| {
            let (kind, key) = split_element(&name)?;
            match kind {
                "pane" => {
                    let reg = panes_reg(lua)?;
                    let (_, rec) = find_in_list(&reg, "id", key)?.ok_or_else(|| not_found(kind, key))?;
                    let spec = pane_public(lua, &rec)?;
                    overlay(kind, &spec, &patch)?;
                    copy_fields(&pane_record(lua, &spec)?, &rec)?;
                }
                "status" => {
                    let reg = status_reg(lua)?;
                    let (_, rec) = find_in_list(&reg, "id", key)?.ok_or_else(|| not_found(kind, key))?;
                    let spec = status_entry_public(lua, &rec)?;
                    overlay(kind, &spec, &patch)?;
                    copy_fields(&status_record(lua, &spec)?, &rec)?;
                    // `text` 和 `update` 是同一格的两条路：写了静态文字就把拉取
                    // 函数摘掉，否则屏幕上还是函数算出来的那个，推了等于没推。
                    //
                    // 摘在 `rec` 上而不是 `spec` 上：表里把键设成 nil 等于「没有
                    // 这个键」，`copy_fields` 遍历不到它，也就摘不掉旧的那个。
                    if patch.contains_key("text")? {
                        rec.set("update", Value::Nil)?;
                    } else if patch.contains_key("update")? {
                        rec.set("text", Value::Nil)?;
                    }
                }
                _ => {
                    let reg = toolbar_reg(lua)?;
                    let (_, rec) = find_in_list(&reg, "command", key)?
                        .ok_or_else(|| not_found(kind, key))?;
                    let spec = lua.create_table()?;
                    for field in ["icon", "tip", "command", "after"] {
                        spec.set(field, rec.get::<Value>(field)?)?;
                    }
                    overlay(kind, &spec, &patch)?;
                    copy_fields(&toolbar_record(lua, &spec)?, &rec)?;
                }
            }
            mark(lua, DIRTY_LAYOUT);
            Ok(())
        });
        // ui.layout:remove("pane.clock") —— 拆掉一个元素。
        //
        // 窗格会真的销毁（外壳重排时把不在表里的那块 `destroy` 掉），状态段和
        // 工具栏按钮下一次重排就消失。
        methods.add_method("remove", |lua, _, name: String| {
            let (kind, key) = split_element(&name)?;
            let (reg, field) = match kind {
                "pane" => (panes_reg(lua)?, "id"),
                "status" => (status_reg(lua)?, "id"),
                _ => (toolbar_reg(lua)?, "command"),
            };
            let Some((at, _)) = find_in_list(&reg, field, key)? else {
                return Err(not_found(kind, key));
            };
            // 数组表要压实：中间留一个空位，`raw_len` 之后的项会被漏读。
            let n = reg.raw_len();
            for i in at + 1..=n {
                reg.set(i - 1, reg.get::<Value>(i)?)?;
            }
            reg.set(n, Value::Nil)?;
            mark(lua, DIRTY_LAYOUT);
            Ok(())
        });
        // ui.layout:list() → { ["pane.clock"] = {…}, ["status.clock"] = {…}, … }
        //
        // 键就是 `update` / `remove` 收的那个名字，所以「列出我建了什么」和
        // 「改一个」用的是同一套地址。给调试和诊断页用。
        methods.add_method("list", |lua, _, ()| {
            let out = lua.create_table()?;
            let panes = panes_reg(lua)?;
            for i in 1..=panes.raw_len() {
                let Ok(rec) = panes.get::<Table>(i) else {
                    continue;
                };
                let spec = pane_public(lua, &rec)?;
                let id: String = spec.get("id")?;
                out.set(format!("pane.{id}"), spec)?;
            }
            let status = status_reg(lua)?;
            for i in 1..=status.raw_len() {
                let Ok(rec) = status.get::<Table>(i) else {
                    continue;
                };
                let spec = status_entry_public(lua, &rec)?;
                let id: String = rec.get::<Option<String>>("id")?.unwrap_or_default();
                if !id.is_empty() {
                    out.set(format!("status.{id}"), spec)?;
                }
            }
            let toolbar = toolbar_reg(lua)?;
            for i in 1..=toolbar.raw_len() {
                let Ok(rec) = toolbar.get::<Table>(i) else {
                    continue;
                };
                let command: String = rec.get::<Option<String>>("command")?.unwrap_or_default();
                let entry = lua.create_table()?;
                entry.set("icon", rec.get::<Option<i32>>("icon")?.unwrap_or(0))?;
                entry.set("tip", rec.get::<Option<String>>("tip")?.unwrap_or_default())?;
                entry.set("command", command.clone())?;
                out.set(format!("toolbar.{command}"), entry)?;
            }
            Ok(out)
        });
    }
}

/// `App.ui.on(region, event, fn)`：这一区被点了。返回 true = 接管，
/// 内置动作（切页签、弹菜单）不再执行；false/nil = 让内置的照做。
///
/// 挂在 `App.ui` 自己身上而不是某个子表上：它既不是画法也不是布局，
/// 是**意图** —— 回调里没有 bridge，想做事就返回一个命令名。
fn ui_on(
    lua: &Lua,
    (region, event, callback): (String, String, Function),
) -> mlua::Result<()> {
    let key = press_key(&region, &event)?;
    let record = lua.create_table()?;
    record.set("n", crate::current_chunk(lua)?)?;
    record.set("f", callback)?;
    press_reg(lua)?.set(key, record)?;
    Ok(())
}

/// `App.ui.off(region, event)`：撤掉一条。
fn ui_off(lua: &Lua, (region, event): (String, String)) -> mlua::Result<()> {
    let key = press_key(&region, &event)?;
    press_reg(lua)?.set(key, Value::Nil)?;
    Ok(())
}

fn paints(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value::<Table>(REG_PAINTS)
}

fn panes_reg(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value::<Table>(REG_PANES)
}

fn status_reg(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value::<Table>(REG_STATUS)
}

fn toolbar_reg(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value::<Table>(REG_TOOLBAR)
}

/// 脏位：脚本改了布局（`App.ui.layout:*`）。
///
/// 和 `style::DIRTY_APPEARANCE` 是**同一组位**，别撞号 —— 外壳一次取走、按位
/// 分派。布局脏了要做的事和外观不同：前者要建/拆窗口，后者只重画。
pub const DIRTY_LAYOUT: u8 = 2;

/// 脏位：脚本改了自己内部的状态，请求重画（`App.ui.repaint`）。
///
/// 和 [`DIRTY_LAYOUT`] 一样走「回调里记、外壳收尾」：绘制结果有缓存，而脚本
/// 存在 Lua upvalue 里的变量宿主看不见。这一位只管重画，不改主题、不动布局。
pub const DIRTY_REPAINT: u8 = 4;

/// `App.ui.repaint(region)` 记下的区域名。回调期间只往里塞，外壳在入口返回后
/// 取走并逐条丢缓存（`take_repaint_regions`）。
#[derive(Default)]
struct RepaintRequests(RefCell<Vec<String>>);

fn mark(lua: &Lua, bits: u8) {
    crate::style::mark(lua, bits);
}

/// `App.ui.repaint([region])`：脚本改了外部状态后，请宿主重画一帧。
///
/// 区域名可以省略（只重画，不指定缓存）；给了就顺手丢这一块的绘制缓存 ——
/// 否则下一次重画还是从缓存里拿出旧的那份 op。真正的窗口作废在外壳那一侧：
/// 这里只需要一个脏位，因为绘制期间没有 bridge、也不该碰窗口。
fn ui_repaint(lua: &Lua, region: Option<String>) -> mlua::Result<()> {
    if let Some(slot) = lua.app_data_ref::<RepaintRequests>() {
        // 不给名字也记一条空串：外壳要据此知道「这次有人请求重画」，而
        // `forget_region("")` 认不出名字就安静地什么都不做。
        slot.0.borrow_mut().push(region.unwrap_or_default());
    }
    mark(lua, DIRTY_REPAINT);
    Ok(())
}

/// 把一条 `ui.layout:pane` 的声明校验完、归一成注册表里那条记录。
///
/// 抽出来是因为运行时改（`ui.layout:update`）要走**同一套**校验：两处各写一份
/// 迟早会分叉，而分叉的表现是「声明时报错、update 时静默」。
fn pane_record(lua: &Lua, spec: &Table) -> mlua::Result<Table> {
    let id: String = spec.get("id")?;
    if id.trim().is_empty() {
        return Err(mlua::Error::runtime("ui.layout:pane 要一个非空的 id"));
    }
    // 侧边写错当场报错并列出四个 —— 猜一个默认值会让窗格长在看不见的地方。
    let side: String = spec
        .get::<Option<String>>("side")?
        .unwrap_or_else(|| "bottom".to_string());
    if !matches!(side.as_str(), "left" | "right" | "top" | "bottom") {
        return Err(mlua::Error::runtime(format!(
            "ui.layout:pane 的 side 只能是 left / right / top / bottom，写的是 {side:?}"
        )));
    }
    let title: String = spec
        .get::<Option<String>>("title")?
        .unwrap_or_else(|| id.clone());
    let record = lua.create_table()?;
    record.set("n", crate::current_chunk(lua)?)?;
    record.set("id", id)?;
    record.set("side", side)?;
    record.set("title", title)?;
    record.set("icon", spec.get::<Option<i32>>("icon")?.unwrap_or(0))?;
    record.set("size", spec.get::<Option<f32>>("size")?.unwrap_or(0.0))?;
    // 轨道宽。负数没有意义，夹到 0（= 不要轨道，滚轮照样能滚）。
    record.set(
        "bar",
        spec.get::<Option<f32>>("bar")?.unwrap_or(0.0).max(0.0),
    )?;
    Ok(record)
}

/// 注册表里那条记录 → 一条公开的窗格声明（不带 `n`）。
fn pane_public(lua: &Lua, rec: &Table) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    out.set("id", rec.get::<Option<String>>("id")?.unwrap_or_default())?;
    out.set("side", rec.get::<Option<String>>("side")?.unwrap_or_default())?;
    out.set("title", rec.get::<Option<String>>("title")?.unwrap_or_default())?;
    out.set("icon", rec.get::<Option<i32>>("icon")?.unwrap_or(0))?;
    out.set("size", rec.get::<Option<f32>>("size")?.unwrap_or(0.0))?;
    out.set("bar", rec.get::<Option<f32>>("bar")?.unwrap_or(0.0))?;
    Ok(out)
}

/// 在一条声明列表里按某个字段找那一项。`(下标, 记录)`。
fn find_in_list(reg: &Table, field: &str, want: &str) -> mlua::Result<Option<(usize, Table)>> {
    for i in 1..=reg.raw_len() {
        let Ok(rec) = reg.get::<Table>(i) else {
            continue;
        };
        if rec.get::<Option<String>>(field)?.as_deref() == Some(want) {
            return Ok(Some((i, rec)));
        }
    }
    Ok(None)
}

/// 把 `src` 的字段抄进 `dst`，**跳过 `n`**。
///
/// `n` 是「这条声明属于哪个脚本块」，热重载按它清。运行时 `update` 是把新字段
/// 抄回**原来那条记录**，块名必须留着 —— 抄成 update 调用者的块名，声明方一
/// 重载就清不掉它，于是越跑越多。
fn copy_fields(src: &Table, dst: &Table) -> mlua::Result<()> {
    for pair in src.pairs::<String, Value>() {
        let (key, value) = pair?;
        if key != "n" {
            dst.set(key, value)?;
        }
    }
    Ok(())
}

/// `"pane.clock"` → `("pane", "clock")`。按**第一个**点切：元素 id 自己可以带点
/// （`acme.clock`），所以只能切第一个。
fn split_element(name: &str) -> mlua::Result<(&str, &str)> {
    let Some((kind, key)) = name.split_once('.') else {
        return Err(mlua::Error::runtime(format!(
            "ui.layout: {name:?} 要写成「种类.名字」，比如 pane.clock / status.clock / \
             toolbar.file.save；种类只有 pane / status / toolbar"
        )));
    };
    if !matches!(kind, "pane" | "status" | "toolbar") {
        return Err(mlua::Error::runtime(format!(
            "ui.layout: {kind:?} 不是元素的种类；只有 pane / status / toolbar"
        )));
    }
    if key.is_empty() {
        return Err(mlua::Error::runtime(format!(
            "ui.layout: {name:?} 少了名字"
        )));
    }
    Ok((kind, key))
}

/// 运行时能改哪些字段。**只列外壳真的会读的** —— 收下一个没人读的字段，等于
/// 「改了没反应」，那正是这套接口最想避免的事。
fn patch_keys(kind: &str) -> &'static [&'static str] {
    match kind {
        "pane" => &["side", "title", "icon", "size", "bar"],
        "status" => &["text", "update", "width", "priority", "click"],
        // 命令名是身份，`after` 是插入位置 —— 改它等于换一个按钮，用 remove + 重声明。
        "toolbar" => &["icon", "tip"],
        _ => &[],
    }
}

fn overlay(kind: &str, dst: &Table, patch: &Table) -> mlua::Result<()> {
    let allowed = patch_keys(kind);
    for pair in patch.pairs::<String, Value>() {
        let (key, value) = pair?;
        if !allowed.contains(&key.as_str()) {
            return Err(mlua::Error::runtime(format!(
                "ui.layout:update 改不了 {kind} 的 {key:?}；能改的有 {}",
                allowed.join(" ")
            )));
        }
        dst.set(key, value)?;
    }
    Ok(())
}

fn not_found(kind: &str, key: &str) -> mlua::Error {
    mlua::Error::runtime(format!(
        "ui.layout: 没有 {kind}.{key} 这个元素 —— 先声明它，再改它"
    ))
}

/// 把一条 `ui.layout:toolbar` 的按钮声明校验完、归一成注册表里那条记录。
///
/// 校验放在声明期：`command` 空着就是「点下去什么也不发生」，这种按钮画出来
/// 只会让人以为坏了，所以当场报错。`after` 写错命令号不报错 —— 那张表是用户的
/// `[ToolBar]`，皮肤不该知道里面每个号叫什么，找不到就退回末尾。
fn toolbar_record(lua: &Lua, spec: &Table) -> mlua::Result<Table> {
    let command = spec
        .get::<Option<String>>("command")?
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .ok_or_else(|| {
            mlua::Error::runtime("ui.layout:toolbar 少 command：按钮点下去要有个命令名")
        })?;
    let icon = match spec.get::<Value>("icon")? {
        Value::Nil => 0,
        Value::Integer(n) => n.clamp(0, i32::MAX as i64) as i32,
        Value::Number(n) => n.max(0.0) as i32,
        other => {
            return Err(mlua::Error::runtime(format!(
                "ui.layout:toolbar 的 icon 要精灵格号（数字），给的是 {}",
                other.type_name()
            )));
        }
    };
    let after = match spec.get::<Value>("after")? {
        Value::Nil => 0u32,
        Value::Integer(n) if n >= 0 => n as u32,
        Value::Number(n) if n >= 0.0 => n as u32,
        other => {
            return Err(mlua::Error::runtime(format!(
                "ui.layout:toolbar 的 after 要一个命令号（插在那一格后面，0 = 末尾），给的是 {}",
                other.type_name()
            )));
        }
    };
    let tip = spec.get::<Option<String>>("tip")?.unwrap_or_default();
    let record = lua.create_table()?;
    record.set("n", crate::current_chunk(lua)?)?;
    record.set("icon", icon)?;
    // 空提示等于没提示：退回命令名，至少能看出这一格是干什么的。
    record.set("tip", if tip.trim().is_empty() { command.clone() } else { tip })?;
    record.set("command", command)?;
    record.set("after", after)?;
    Ok(record)
}

/// 注册表里那条记录 → 一条公开的声明表（不带 `n`）。
fn toolbar_public(lua: &Lua, reg: &Table) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for i in 1..=reg.raw_len() {
        let Ok(rec) = reg.get::<Table>(i) else {
            continue;
        };
        let entry = lua.create_table()?;
        entry.set("icon", rec.get::<Option<i32>>("icon")?.unwrap_or(0))?;
        entry.set("tip", rec.get::<Option<String>>("tip")?.unwrap_or_default())?;
        entry.set("command", rec.get::<Option<String>>("command")?.unwrap_or_default())?;
        let after = rec.get::<Option<u32>>("after")?.unwrap_or(0);
        if after != 0 {
            entry.set("after", after)?;
        }
        out.set(i, entry)?;
    }
    Ok(out)
}

/// 把一条 `ui.layout:status` 的段声明校验完、归一成注册表里那条记录。
///
/// **校验放在声明期**：段名同时是绘制区域名（`status.<id>`），写错了要当场报错，
/// 而不是等到「这一格怎么永远不生效」。宽度写法只有四种，写别的也当场报错 ——
/// 猜一个默认值会让皮肤以为设了、其实没设。
fn status_record(lua: &Lua, entry: &Table) -> mlua::Result<Table> {
    let record = lua.create_table()?;
    record.set("n", crate::current_chunk(lua)?)?;
    let id = entry
        .get::<Option<String>>("id")?
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_default();
    // 段名要能拼成 `status.<id>`，合法性借区域名那条规矩（ascii 字母数字/_/-，
    // 不超过 40 字节）—— 两处各写一份校验迟早会分叉。
    if !id.is_empty() {
        dg_skin::Region::parse(&format!("status.{id}"))
            .map_err(|e| mlua::Error::runtime(format!("ui.layout:status 的 id {id:?}: {e}")))?;
    }
    // 宽度：`"auto"`（默认）/ `"fill"` / 数字（固定 DIP）/ `{ min =, max = }`。
    let (wkind, wmin, wmax) = match entry.get::<Value>("width")? {
        Value::Nil => ("auto", 0.0f32, 0.0f32),
        Value::String(s) => match s.to_str()?.as_ref() {
            "auto" => ("auto", 0.0, 0.0),
            "fill" => ("fill", 0.0, 0.0),
            other => {
                return Err(mlua::Error::runtime(format!(
                    "ui.layout:status 的 width 字符串只有 \"auto\" 和 \"fill\"，写的是 {other:?}"
                )));
            }
        },
        Value::Number(n) => ("fixed", n as f32, 0.0),
        // Lua 5.4 的整数和浮点是两个类型：`width = 60` 给的是 Integer。
        Value::Integer(n) => ("fixed", n as f32, 0.0),
        Value::Table(t) => {
            // 两个都可省：`{ max = 104 }` = 「按文字量，最多 104」，这是最常见
            // 的写法。省掉 `max` 是「没有上限」，省掉 `min` 是「没有下限」。
            let min: f32 = t.get::<Option<f32>>("min")?.unwrap_or(0.0);
            let max: f32 = t.get::<Option<f32>>("max")?.unwrap_or(0.0);
            if min < 0.0 || max < 0.0 {
                return Err(mlua::Error::runtime(
                    "ui.layout:status 的 width.min / width.max 不能是负数",
                ));
            }
            ("range", min, max)
        }
        other => {
            return Err(mlua::Error::runtime(format!(
                "ui.layout:status 的 width 要一个数字、\"auto\"、\"fill\" 或 {{min=,max=}}，\
                 给的是 {}",
                other.type_name()
            )));
        }
    };
    // 没 id 的段只能是 fill：那是「把剩下的吃掉」的那一格，它没有内容可画。
    if id.is_empty() && wkind != "fill" {
        return Err(mlua::Error::runtime(
            "ui.layout:status 的段要么有 id（有 id 才有绘制区域名 status.<id>），\
             要么 width = \"fill\"（那格只负责把剩下的宽度吃掉）",
        ));
    }
    let priority = entry.get::<Option<i64>>("priority")?.unwrap_or(50);
    if !(0..=255).contains(&priority) {
        return Err(mlua::Error::runtime(
            "ui.layout:status 的 priority 要 0..255：数字越大越重要，窗口窄了先丢小的",
        ));
    }
    let update = entry.get::<Value>("update")?;
    if !matches!(update, Value::Nil | Value::Function(_)) {
        return Err(mlua::Error::runtime(
            "ui.layout:status 的 update 要一个函数：update(ctx) 返回这一格的文字",
        ));
    }
    record.set("id", id)?;
    record.set("text", entry.get::<Option<String>>("text")?.unwrap_or_default())?;
    record.set("wkind", wkind)?;
    record.set("wmin", wmin)?;
    record.set("wmax", wmax)?;
    record.set("priority", priority)?;
    record.set("click", entry.get::<Option<String>>("click")?.unwrap_or_default())?;
    record.set("update", update)?;
    Ok(record)
}

/// 注册表里那条记录 → 一条公开的声明表（不带 `n`，宽度还原成调用方写的形状）。
fn status_public(lua: &Lua, reg: &Table) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for i in 1..=reg.raw_len() {
        let Ok(rec) = reg.get::<Table>(i) else {
            continue;
        };
        out.set(i, status_entry_public(lua, &rec)?)?;
    }
    Ok(out)
}

/// 一条状态段的记录 → 公开的声明表。
///
/// 和 [`status_public`] 拆开是因为运行时那条路（`update` / `list`）一次只处理
/// 一格：整条转一遍再挑，等于把「改一格」变成「抄十三个」。
fn status_entry_public(lua: &Lua, rec: &Table) -> mlua::Result<Table> {
    let entry = lua.create_table()?;
    let id = rec.get::<Option<String>>("id")?.unwrap_or_default();
    if !id.is_empty() {
        entry.set("id", id)?;
    }
    let text = rec.get::<Option<String>>("text")?.unwrap_or_default();
    if !text.is_empty() {
        entry.set("text", text)?;
    }
    let wkind = rec.get::<Option<String>>("wkind")?.unwrap_or_default();
    let min = rec.get::<Option<f32>>("wmin")?.unwrap_or(0.0);
    let max = rec.get::<Option<f32>>("wmax")?.unwrap_or(0.0);
    match wkind.as_str() {
        "fill" => entry.set("width", "fill")?,
        "fixed" => entry.set("width", min)?,
        "range" => {
            let w = lua.create_table()?;
            w.set("min", min)?;
            w.set("max", max)?;
            entry.set("width", w)?;
        }
        _ => {}
    }
    entry.set("priority", rec.get::<Option<i64>>("priority")?.unwrap_or(50))?;
    let click = rec.get::<Option<String>>("click")?.unwrap_or_default();
    if !click.is_empty() {
        entry.set("click", click)?;
    }
    if let Ok(Value::Function(f)) = rec.get::<Value>("update") {
        entry.set("update", f)?;
    }
    Ok(entry)
}

fn press_reg(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value::<Table>(REG_PRESS)
}

fn hints(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value::<Table>(REG_HINTS)
}

/// 事件名只有两个：左键 `press`、右键 `rclick`。写错当场报错并列出合法的 ——
/// 和样式键一条规矩，不猜。
fn press_key(region: &str, event: &str) -> mlua::Result<String> {
    match event {
        "press" | "rclick" => Ok(format!("{region}:{event}")),
        other => Err(mlua::Error::runtime(format!(
            "bad event {other:?}: want \"press\" or \"rclick\""
        ))),
    }
}

impl ScriptHost {
    /// 装 `App.ui`。由 `build_api` 调一次。
    pub(crate) fn build_ui_api(&self, app: &Table) -> Result<(), ScriptError> {
        self.lua
            .set_named_registry_value(REG_PAINTS, self.lua.create_table()?)?;
        self.lua
            .set_named_registry_value(REG_PANES, self.lua.create_table()?)?;
        self.lua
            .set_named_registry_value(REG_STATUS, self.lua.create_table()?)?;
        self.lua
            .set_named_registry_value(REG_TOOLBAR, self.lua.create_table()?)?;
        self.lua
            .set_named_registry_value(REG_PRESS, self.lua.create_table()?)?;
        self.lua
            .set_named_registry_value(REG_HINTS, self.lua.create_table()?)?;
        self.lua
            .set_named_registry_value(REG_CANVAS, self.lua.create_userdata(Canvas::new())?)?;

        let palette = self.lua.create_table()?;
        self.lua
            .set_named_registry_value(REG_PALETTE, palette.clone())?;
        let ctx = PaintCtx::new(palette);
        self.lua
            .set_named_registry_value(REG_CTX, self.lua.create_userdata(ctx)?)?;
        self.lua.set_app_data(RepaintRequests::default());

        let ui = self.lua.create_table()?;
        // 四张表，各管一件事。**没有 `App.ui.skin()`** —— 它以前把画法和布局
        // 装在一个对象上，于是「只改页签圆角」的脚本也得知道 `pane` 怎么声明。
        // 现在想要哪个就取哪个，写错了是 `attempt to index a nil value`，
        // 一眼能看出用错了门。
        ui.set("paint", self.lua.create_userdata(PaintApi)?)?;
        ui.set("layout", self.lua.create_userdata(LayoutApi)?)?;
        ui.set("on", self.lua.create_function(ui_on)?)?;
        ui.set("off", self.lua.create_function(ui_off)?)?;
        // 第五个成员是函数，不是表：前四个管「声明什么」，它管「改完了，重画」。
        ui.set("repaint", self.lua.create_function(ui_repaint)?)?;
        ui.set("style", self.lua.create_userdata(crate::style::StyleApi)?)?;
        app.set("ui", ui)?;
        // `App.theme` 是只读的当前主题。放在 `app` 上而不是 `ui` 下：
        // 「现在用的是哪套颜色」不是 UI 的事，命令和事件回调里也要问。
        app.set("theme", crate::style::theme_table(&self.lua)?)?;

        // 先灌一份出厂主题：皮肤不调 set_palette 时 `ctx.theme[...]` 也不该是空的。
        self.set_palette(&dg_render::Theme::default());
        Ok(())
    }

    /// 皮肤声明过的脚本窗格，按声明顺序。
    ///
    /// 外壳在皮肤装载之后调一次，用来自建窗口、上停靠栏。**窗格是「皮肤装载
    /// 时存在的东西」**：热重载只重跑 `skin.lua`，这里返回的新列表不会去
    /// 增删已有窗口 —— 加/删窗格要重开进程。只改 `ui.paint:on("pane.x")` 是热的。
    pub fn panes(&self) -> Vec<PaneSpec> {
        let Ok(list) = panes_reg(&self.lua) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 1..=list.raw_len() {
            let Ok(rec) = list.get::<Table>(i) else {
                continue;
            };
            let Ok(id) = rec.get::<String>("id") else {
                continue;
            };
            out.push(PaneSpec {
                id,
                side: rec
                    .get::<Option<String>>("side")
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "bottom".to_string()),
                title: rec.get::<Option<String>>("title").ok().flatten().unwrap_or_default(),
                icon: rec.get::<Option<i32>>("icon").ok().flatten().unwrap_or(0),
                size: rec.get::<Option<f32>>("size").ok().flatten().unwrap_or(0.0),
                // 负数没有意义，夹到 0；上限不设 —— 皮肤想用 60 DIP 的宽条也随它。
                bar: rec
                    .get::<Option<f32>>("bar")
                    .ok()
                    .flatten()
                    .unwrap_or(0.0)
                    .max(0.0),
            });
        }
        out
    }

    /// 皮肤声明过的状态栏段，按声明顺序（= 从左到右）。
    ///
    /// 空 = 没接管，内置那十三个字段照旧。非空 = **整条由皮肤说了算**：内置字段
    /// 一个都不画，除非皮肤用 `ui.layout:status()` 把它们读出来再加回去。
    ///
    /// 只做表读，**不进 Lua**（`update` 是另一条路，见 [`Self::status_text`]）：
    /// 这个方法在每次排版时调，一次调用要能扛住每帧。
    pub fn status(&self) -> Vec<dg_skin::Segment> {
        let Ok(reg) = status_reg(&self.lua) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 1..=reg.raw_len() {
            let Ok(rec) = reg.get::<Table>(i) else {
                continue;
            };
            let f32_of = |key: &str| rec.get::<Option<f32>>(key).ok().flatten().unwrap_or(0.0);
            let width = match rec.get::<Option<String>>("wkind").ok().flatten().as_deref() {
                Some("fill") => dg_skin::Width::Fill,
                Some("fixed") => dg_skin::Width::Fixed(f32_of("wmin")),
                Some("range") => dg_skin::Width::Range {
                    min: f32_of("wmin"),
                    max: f32_of("wmax"),
                },
                _ => dg_skin::Width::Auto,
            };
            out.push(dg_skin::Segment {
                id: rec.get::<Option<String>>("id").ok().flatten().unwrap_or_default(),
                template: rec.get::<Option<String>>("text").ok().flatten().unwrap_or_default(),
                width,
                priority: rec.get::<Option<i64>>("priority").ok().flatten().unwrap_or(50) as u8,
                click: rec.get::<Option<String>>("click").ok().flatten().unwrap_or_default(),
                dynamic: matches!(rec.get::<Value>("update"), Ok(Value::Function(_))),
            });
        }
        out
    }

    /// `ui.layout:toolbar` 声明的按钮，按声明顺序。
    ///
    /// 宿主拿到的是纯数据，不是 Lua 值 —— 和 `status()` 一样，读表不跑 Lua。
    pub fn toolbar_buttons(&self) -> Vec<ToolbarButton> {
        let Ok(reg) = toolbar_reg(&self.lua) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 1..=reg.raw_len() {
            let Ok(rec) = reg.get::<Table>(i) else {
                continue;
            };
            let Ok(command) = rec.get::<String>("command") else {
                continue;
            };
            out.push(ToolbarButton {
                icon: rec.get::<Option<i32>>("icon").ok().flatten().unwrap_or(0),
                tip: rec.get::<Option<String>>("tip").ok().flatten().unwrap_or_default(),
                command,
                after: rec.get::<Option<u32>>("after").ok().flatten().unwrap_or(0),
            });
        }
        out
    }

    /// 跑一段的 `update` 回调，拿到它的文字。
    ///
    /// **这是绘制档的兄弟，不是 Intent 档**：不装 bridge，拿到的 `ctx` 和
    /// `s:paint` 一模一样（`ctx.doc`、`App.now()`、`ctx.theme`），所以它碰不到
    /// `EditorState` —— 它跑在量宽度那条路上，那会儿编辑器正被画着。要读文档就
    /// 读 `ctx.doc` 快照，和画法一个待遇。
    ///
    /// `ctx.w` / `ctx.h` 在这一档是 0：文字决定宽度，宽度这会儿还不知道。
    ///
    /// 没有 `update`、回调报错、返回值不是字符串 —— 一律 `None`，调用方退回模板、
    /// 再不行退回内置那一格的文字。**一个坏回调不该让整条状态栏空掉。**
    pub fn status_text(&self, id: &str) -> Option<String> {
        let reg = status_reg(&self.lua).ok()?;
        let mut func = None;
        for i in 1..=reg.raw_len() {
            let Ok(rec) = reg.get::<Table>(i) else {
                continue;
            };
            if rec.get::<Option<String>>("id").ok().flatten().as_deref() != Some(id) {
                continue;
            }
            if let Ok(Value::Function(f)) = rec.get::<Value>("update") {
                func = Some(f);
            }
            break;
        }
        let func = func?;
        let ctx = self.lua.named_registry_value::<AnyUserData>(REG_CTX).ok()?;
        {
            let mut c = ctx.borrow_mut::<PaintCtx>().ok()?;
            c.set(&PaintInput {
                part: id.to_string(),
                ..PaintInput::default()
            });
            c.set_doc(self.paint_doc.borrow().clone());
            // 行表和度量是编辑区专用的：这里必须清，否则同一个 `PaintCtx` 里
            // 会留着上一帧 `editor.overlay` 挂上去的那份。
            c.set_rows(None);
            c.set_metrics(None);
        }
        let _ = self.lua.set_named_registry_value(REG_VOLATILE, false);
        let _budget = self.arm(crate::budget::PAINT);
        match func.call::<Value>(&ctx) {
            Ok(Value::String(s)) => s.to_str().ok().map(|s| s.to_string()),
            Ok(Value::Number(n)) => Some(n.to_string()),
            Ok(Value::Integer(n)) => Some(n.to_string()),
            Ok(Value::Nil) => Some(String::new()),
            Ok(_) => None,
            Err(e) => {
                eprintln!("[script] status.{id} update: {e}");
                None
            }
        }
    }

    /// 把当前主题的 89 个 token 灌进 `ctx.theme`。
    ///
    /// 只在主题变化时调一次：绘制路径上不能每帧建 89 个字符串。表是**同一张**，
    /// 所以已经拿在手里的 `ctx.theme` 也跟着变。
    pub fn set_palette(&self, theme: &dg_render::Theme) {
        let Ok(table) = self.lua.named_registry_value::<Table>(REG_PALETTE) else {
            return;
        };
        let _ = table.clear();
        for name in dg_render::tokens::TOKEN_NAMES {
            if let Some(color) = theme.token(name) {
                let _ = table.set(
                    *name,
                    format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b),
                );
            }
        }
    }

    /// 这个区域的这个事件有脚本回调吗？
    pub fn has_press(&self, region: &str, event: &str) -> bool {
        let Ok(key) = press_key(region, event) else {
            return false;
        };
        match press_reg(&self.lua) {
            Ok(t) => matches!(t.get::<Value>(key), Ok(Value::Table(_))),
            Err(_) => false,
        }
    }

    /// 把一次点击交给脚本。`None` = 没注册回调；`Some(true)` = 脚本接管了，
    /// 内置动作别做；`Some(false)` = 脚本看过了但让内置的照做。
    ///
    /// **点击是 Intent 档，不是绘制档**：它发生在 `WM_LBUTTONDOWN` 里，那会儿
    /// `EditorState` 没被借走，所以这里装得上 bridge —— handler 里 `print`、
    /// `App.run`、`App.doc.*` 都能用。绘制期没有这份待遇，那是重入。
    ///
    /// 报错按「没接管」处理并记一行 —— 一个画错的皮肤不该让页签点不动。
    pub fn press(
        &self,
        region: &str,
        event: &str,
        input: &PaintInput,
        bridge: &mut dyn Bridge,
    ) -> Option<bool> {
        let key = press_key(region, event).ok()?;
        let reg = press_reg(&self.lua).ok()?;
        let record = reg.get::<Value>(key).ok()?;
        let f = crate::record_func(&record)?;
        let ctx = self.lua.named_registry_value::<AnyUserData>(REG_CTX).ok()?;
        let doc = self.paint_doc.borrow().clone();
        {
            let mut c = ctx.borrow_mut::<PaintCtx>().ok()?;
            c.set(input);
            c.set_doc(doc);
            // 同 `run_paint`：行表/度量只属于编辑区的绘制档，点击档一律清空。
            c.set_rows(None);
            c.set_metrics(None);
        }
        // 点击回调是「干活」那一档：可能去开文件、跑命令，预算按脚本档给。
        let _budget = self.arm(crate::budget::SCRIPT);
        // 日志在 bridge 还装着的闭包里记（`with_bridge` 一返回就没了）。
        let r = self.with_bridge(bridge, || f.call::<Value>(&ctx));
        // handler 可能改了自己那份画法要用的状态（计数器、选中项、展开位），
        // 而那些变量宿主看不见 —— 缓存没法知道该失效哪一条，只能整区域丢掉。
        // 只在真有 handler 的时候丢：内置区域（工具栏、状态栏、页签条）点一下
        // 通常只改状态位，键自己就变了，不必陪着重算几十格。
        self.forget_region(region);
        Some(match r {
            Ok(Value::Boolean(b)) => b,
            Ok(Value::Nil) => false,
            Ok(_) => true,
            Err(e) => {
                crate::log_via_bridge(&self.lua, &format!("{region} {event}: {e}"));
                false
            }
        })
    }

    /// 这个区域有脚本画法吗？
    pub fn has_paint(&self, region: &str) -> bool {
        match paints(&self.lua) {
            Ok(t) => t
                .get::<Value>(region)
                .map(|v| crate::record_func(&v).is_some())
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    /// 声明过画法的区域名，排好序。`--ui-lint` 的清单就是它。
    pub fn paint_regions(&self) -> Vec<String> {
        let Ok(t) = paints(&self.lua) else {
            return Vec::new();
        };
        let mut out: Vec<String> = t
            .pairs::<Value, Value>()
            .flatten()
            .filter_map(|(k, v)| match k {
                Value::String(s) if crate::record_func(&v).is_some() => {
                    s.to_str().ok().map(|s| s.to_string())
                }
                _ => None,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// 上一次 [`Self::paint`] 里回调抛的错。`paint` 每次开头清空。
    ///
    /// 绘制期没有 bridge 也没有返回值能带错误 —— 错误只往 stderr 打一行，
    /// 而 `--ui-lint` 要在表里标出是哪一格挂了，所以顺手留一份。
    pub fn take_paint_error(&self) -> Option<String> {
        self.paint_error.borrow_mut().take()
    }

    /// 记一条绘制错误：留一份给 [`Self::take_paint_error`]，stderr **只打一次**。
    ///
    /// 去重不是洁癖：绘制跑在 `WM_PAINT` 里，一条坏画法每帧都抛 —— 不去重就是
    /// 每秒几百行同样的栈，`--ui-lint` 跑六档状态也会把同一处打六遍。
    fn note_paint_error(&self, region: &str, msg: String) {
        let key = format!("{region}\u{1}{msg}");
        if self.paint_error_seen.borrow_mut().insert(key) {
            eprintln!("[script] {region} paint: {msg}");
        }
        *self.paint_error.borrow_mut() = Some(msg);
    }

    /// 这个区域的悬停声明。没写过答 `None`（指针和提示都归内置）。
    ///
    /// 只做一次表读 + 最多两个字符串拷贝，**不进 Lua、不装预算** —— 它跑在
    /// `WM_MOUSEMOVE` 的命中测试之后，换一格问一次，人手的频率。
    pub fn hint(&self, region: &str) -> Option<dg_skin::HintSpec> {
        let hints = hints(&self.lua).ok()?;
        let Value::Table(record) = hints.get::<Value>(region).ok()? else {
            return None;
        };
        let cursor = match record.get::<Value>("cursor") {
            Ok(Value::String(s)) => dg_skin::Cursor::parse(&s.to_str().ok()?).ok(),
            _ => None,
        };
        let tip = match record.get::<Value>("tip") {
            Ok(Value::String(s)) => s.to_str().ok().map(|s| s.to_string()),
            _ => None,
        };
        let spec = dg_skin::HintSpec { cursor, tip };
        (!spec.is_empty()).then_some(spec)
    }

    /// 有没有哪个区域声明过悬停。外壳用它决定要不要为指针那条路铺代码 ——
    /// 没有声明就一次都不问。
    ///
    /// 注意不能数 `raw_len()`：这张表是**字符串键**，数组部分永远是空的。
    pub fn has_hints(&self) -> bool {
        match hints(&self.lua) {
            Ok(t) => t.pairs::<Value, Value>().next().is_some(),
            Err(_) => false,
        }
    }

    /// 跑一次绘制回调。`None` 表示没有注册，交回内置画法。
    ///
    /// 整个调用**不装 bridge**：回调里 `App.doc.*` 一律拿到「no bridge」的报错，
    /// 被这里接住并转成「没画」。
    ///
    /// 结果按 [`dg_skin::CacheKey`] 缓存：命中时不进 Lua，直接还上一份 op 列表
    /// （`Rc` 克隆）。键里放不下的那几样由条目比对，见 [`Reads`]。
    pub fn paint(&self, region: &str, input: &PaintInput) -> Option<std::rc::Rc<PaintOut>> {
        self.paint_error.borrow_mut().take();
        let paints = paints(&self.lua).ok()?;
        let callback = crate::record_func(&paints.get::<Value>(region).ok()?)?;
        let doc = self.paint_doc.borrow().clone();
        let key = paint_key(region, input);
        if let Some(key) = key {
            if let Some(hit) = self
                .paint_cache
                .borrow_mut()
                .get(&key, input, doc.as_deref())
            {
                return Some(hit);
            }
        }
        let (out, volatile) = self.run_paint(region, &callback, input, &doc)?;
        if volatile {
            self.paint_volatile.set(self.paint_volatile.get() + 1);
        }
        // `--skin-debug`：同一份输入再跑一遍，op 必须逐条相同。缓存的前提就是
        // 纯函数 —— 有副作用的画法在「命中」和「重算」两条路上给出不同结果。
        // 读过 `App.now()` 的画法不比：它本来就每毫秒都不一样。
        if self.paint_debug.get() && !volatile {
            self.report_impurity(region, &callback, input, &doc, &out);
        }
        let shared = std::rc::Rc::new(out);
        if let (Some(key), false) = (key, volatile) {
            self.paint_cache
                .borrow_mut()
                .put(key, input, doc.as_deref(), &shared, std::rc::Rc::clone(&shared));
        }
        Some(shared)
    }

    /// `editor.overlay` / `editor.gutter` 专用：一次调用，拿到整屏行表。
    ///
    /// 和 [`Self::paint`] 分成两个入口，而不是给 [`PaintInput`] 加字段，是因为
    /// 这一档**永远不进缓存**：输入是可见行表，滚动一格、改一个字、动一下光标
    /// 全都变。要缓存就得每帧把四十行逐字段比一遍 —— 和直接跑一遍一样贵，还多
    /// 一份状态要维护。所以这里不查也不存，`CacheKey` 那条路根本不过。
    ///
    /// 代价是每帧一次 Lua 调用。`ctx.rows` 是懒建的（回调不读就一张表都不建），
    /// 读的时候整张重建 —— 皮肤该 `local rows = ctx.rows` 拿一次。
    ///
    /// `metrics` 只有编辑区给（见 [`Metrics`]）：皮肤要对齐文字就得知道一个字
    /// 多宽，自己量一遍会和编辑器差半像素。
    pub fn paint_rows(
        &self,
        region: &str,
        input: &PaintInput,
        rows: &[PaintRow],
        metrics: Option<Metrics>,
    ) -> Option<std::rc::Rc<PaintOut>> {
        self.paint_error.borrow_mut().take();
        let paints = paints(&self.lua).ok()?;
        let callback = crate::record_func(&paints.get::<Value>(region).ok()?)?;
        let doc = self.paint_doc.borrow().clone();
        let rows = std::rc::Rc::new(rows.to_vec());
        let (out, volatile) =
            self.run_paint_rows(region, &callback, input, &doc, Some(&rows), metrics)?;
        if volatile {
            self.paint_volatile.set(self.paint_volatile.get() + 1);
        }
        Some(std::rc::Rc::new(out))
    }

    /// 跑一次带行表的回调。和 [`Self::run_paint`] 是同一条路，只多了 `rows`。
    fn run_paint_rows(
        &self,
        region: &str,
        callback: &mlua::Function,
        input: &PaintInput,
        doc: &Option<std::rc::Rc<PaintDoc>>,
        rows: Option<&std::rc::Rc<Vec<PaintRow>>>,
        metrics: Option<Metrics>,
    ) -> Option<(PaintOut, bool)> {
        let canvas = self.lua.named_registry_value::<AnyUserData>(REG_CANVAS).ok()?;
        let ctx = self.lua.named_registry_value::<AnyUserData>(REG_CTX).ok()?;
        canvas.borrow::<Canvas>().ok()?.reset();
        {
            let mut c = ctx.borrow_mut::<PaintCtx>().ok()?;
            c.set(input);
            c.set_doc(doc.clone());
            c.set_rows(rows.cloned());
            c.set_metrics(metrics);
        }
        let _ = self.lua.set_named_registry_value(REG_VOLATILE, false);
        let _budget = self.arm(crate::budget::PAINT);
        let handled = match callback.call::<Value>((&canvas, &ctx)) {
            Ok(v) => match v {
                Value::Boolean(b) => b,
                Value::Nil => false,
                _ => true,
            },
            Err(e) => {
                let msg = format!("{e}");
                self.note_paint_error(region, msg);
                false
            }
        };
        let handled = handled && self.budget_used() < crate::budget::PAINT;
        let volatile = self
            .lua
            .named_registry_value::<bool>(REG_VOLATILE)
            .unwrap_or(false);
        let mut out = canvas.borrow::<Canvas>().ok()?.snapshot(handled);
        out.reads = ctx.borrow::<PaintCtx>().ok()?.reads.get();
        Some((out, volatile))
    }

    /// 跑一次回调、收一份 op。`paint` 和纯度检查的第二遍走的是同一条路 ——
    /// 两遍之间唯一的差别是 canvas 被 `reset` 过。
    fn run_paint(
        &self,
        region: &str,
        callback: &mlua::Function,
        input: &PaintInput,
        doc: &Option<std::rc::Rc<PaintDoc>>,
    ) -> Option<(PaintOut, bool)> {
        let canvas = self.lua.named_registry_value::<AnyUserData>(REG_CANVAS).ok()?;
        let ctx = self.lua.named_registry_value::<AnyUserData>(REG_CTX).ok()?;
        canvas.borrow::<Canvas>().ok()?.reset();
        {
            let mut c = ctx.borrow_mut::<PaintCtx>().ok()?;
            c.set(input);
            c.set_doc(doc.clone());
            // 行表和度量是编辑区专用的：普通区域**必须**在这里清掉，否则上一帧
            // `editor.overlay` 挂上去的那份会留在同一个 `PaintCtx` 里，状态栏的
            // 皮肤就读到了编辑区的行（`set_rows` 的注释说的就是这件事，但当时
            // 只写了 setter、没有清）。
            c.set_rows(None);
            c.set_metrics(None);
        }
        let _ = self.lua.set_named_registry_value(REG_VOLATILE, false);
        // 绘制这一档的预算最紧：它跑在 `WM_PAINT` 里，卡住就是整窗不刷新。
        let _budget = self.arm(crate::budget::PAINT);
        let mut content = None;
        let handled = match callback.call::<Value>((&canvas, &ctx)) {
            Ok(v) => match v {
                Value::Boolean(b) => b,
                Value::Nil => false,
                // 数字 = 「我画了，而且我的内容有 N DIP 高」。见 `PaintOut::content`。
                // 整数和浮点都要接：Lua 5.4 的 `1200` 是 `Integer`，`1200.5` 才是
                // `Number`，只认一个会让 `return 1200` 静默变成「没报内容高」。
                Value::Number(n) => {
                    content = Some((n as f32).max(0.0));
                    true
                }
                Value::Integer(n) => {
                    content = Some((n as f32).max(0.0));
                    true
                }
                _ => true,
            },
            Err(e) => {
                // 绘制期没有 bridge 可记日志，走 stderr —— 一行，别刷屏。
                let msg = format!("{e}");
                self.note_paint_error(region, msg);
                false
            }
        };
        // 钩子抛的错脚本能自己 `pcall` 吞掉，然后照样返回 true —— 那就再兜一次：
        // 烧满预算的这次调用一律按「没画」算。（`pcall` 能接住意味着预算拦的是
        // 手滑写的死循环，拦不住存心绕的脚本；这一行至少让它绕不出结果。）
        let handled = handled && self.budget_used() < crate::budget::PAINT;
        // 回调里读了 `App.now()`：这份 op 只在那一毫秒成立，不进缓存（也不查 ——
        // 它从来没被存过）。例子里那条靠心跳扫来扫去的细线就是这一档。
        let volatile = self
            .lua
            .named_registry_value::<bool>(REG_VOLATILE)
            .unwrap_or(false);
        let mut out = canvas.borrow::<Canvas>().ok()?.snapshot(handled);
        out.reads = ctx.borrow::<PaintCtx>().ok()?.reads.get();
        // 没画就没有内容高 —— 一个「交回内置」的返回值不该改宿主的滚动量。
        out.content = if handled { content } else { None };
        Some((out, volatile))
    }

    /// `--skin-debug` 的第二遍：同一份输入再跑一次，比画出来的东西。
    ///
    /// **一处只报一次** —— 绘制跑在 `WM_PAINT` 里，每帧一行就是刷屏；带上第一处
    /// 不同的位置，改起来不用猜。
    fn report_impurity(
        &self,
        region: &str,
        callback: &mlua::Function,
        input: &PaintInput,
        doc: &Option<std::rc::Rc<PaintDoc>>,
        first: &PaintOut,
    ) {
        let Some((second, _)) = self.run_paint(region, callback, input, doc) else {
            return;
        };
        let Some(diff) = first.describe_diff(&second) else {
            return;
        };
        let site = format!("{region}#{}", input.index);
        if !self.purity_seen.borrow_mut().insert(site) {
            return;
        }
        self.purity_count.set(self.purity_count.get() + 1);
        eprintln!(
            "[script] paint 有副作用：{region} 第 {} 格（{}）两遍跑出来不一样 —— {diff}。\
             缓存的前提是纯函数，副作用请放到 update 里。",
            input.index, input.part
        );
    }

    /// 推一份绘制档的文档快照。
    ///
    /// 外壳每帧调一次（`window.rs` 的 `paint()` 开头）。**内容没变就什么都不做** ——
    /// 换掉 `Rc` 会让读过 `ctx.doc` 的缓存条目全部对不上，等于每帧白扔一遍。
    /// 所以这里比一遍内容，只有真变了才换。
    pub fn set_paint_doc(&self, doc: PaintDoc) {
        let mut slot = self.paint_doc.borrow_mut();
        if slot.as_deref() == Some(&doc) {
            return;
        }
        *slot = Some(std::rc::Rc::new(doc));
    }

    /// 撤掉快照（换皮肤、关文档时用；之后 `ctx.doc` 是一张空表）。
    pub fn clear_paint_doc(&self) {
        self.paint_doc.borrow_mut().take();
    }

    /// 绘制缓存的 (命中, 未命中)。
    pub fn paint_stats(&self) -> (u64, u64) {
        self.paint_cache.borrow().stats()
    }

    /// 丢掉一个区域的绘制缓存。
    ///
    /// 宿主在**点击**之后调：`press` 的 handler 可能改了画法要用的状态，而那些
    /// 变量宿主看不到，缓存没法知道该失效哪一条。区域名认不出来就什么都不做。
    pub fn forget_region(&self, region: &str) {
        let Ok(id) = dg_skin::Region::parse(region) else {
            return;
        };
        self.paint_cache.borrow_mut().forget_region(id);
    }

    /// 取走 `App.ui.repaint(region)` 攒下的区域名，逐条丢掉缓存。
    ///
    /// 外壳在处理 [`DIRTY_REPAINT`] 时调一次：一个入口里连调十次 `repaint` 也
    /// 只重画一帧，但每一块点名的区域都得作废。返回那些区域名（不带名字的
    /// 请求是空串），外壳据此决定作废哪些窗口。
    pub fn take_repaint_regions(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(slot) = self.lua.app_data_ref::<RepaintRequests>() {
            std::mem::swap(&mut *slot.0.borrow_mut(), &mut names);
        }
        for region in &names {
            self.forget_region(region);
        }
        names
    }
}

/// 一次绘制请求的缓存键。区域名认不出来（皮肤写错了）就不缓存 —— 让它每次
/// 都跑一遍，错的地方照旧报错，别让缓存把错误也一起记住。
fn paint_key(region: &str, input: &PaintInput) -> Option<dg_skin::CacheKey> {
    let region = dg_skin::Region::parse(region).ok()?;
    Some(
        dg_skin::CacheKey::new(region, part_bits(&input.part))
            .state(dg_skin::State::from_bits(input.state))
            // op 的坐标是块内的，所以尺寸就是 rect 的宽高。
            .size(input.rect.2, input.rect.3)
            .dpi(input.dpi.clamp(0, 255) as u16)
            // 序号把同区域里的每一块分开，部件名的 8 位哈希只当补充。
            .index(input.index, 0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> ScriptHost {
        ScriptHost::new().expect("lua")
    }

    fn input() -> PaintInput {
        PaintInput {
            rect: (10.0, 20.0, 100.0, 30.0),
            thumb: (10.0, 20.0, 100.0, 30.0),
            state: 0,
            part: "Encoding".into(),
            text: "UTF-8".into(),
            label: "编码".into(),
            value: "65001".into(),
            dpi: 96,
            index: 0,
            bar: (0.0, 0.0, 0.0, 0.0),
            scroll: 0.0,
        }
    }

    /// 没有注册回调时，`paint` 直接说「没有」。
    #[test]
    fn an_unregistered_region_returns_none() {
        let h = host();
        assert!(!h.has_paint("status.pane"));
        assert!(h.paint("status.pane", &input()).is_none());
    }

    /// 注册之后回调真的跑，画出来的是一条 `Fill`。
    #[test]
    fn a_registered_callback_runs_and_emits_ops() {
        let h = host();
        h.load(
            "t",
            r##"
            local ui = App.ui
            ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, "#112233")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        assert!(h.has_paint("status.pane"));
        let out = h.paint("status.pane", &input()).expect("runs");
        assert!(out.handled);
        assert_eq!(out.ops.len(), 1);
        match out.ops[0] {
            UiOp::Fill { rect, color } => {
                assert_eq!(rect, Rect::new(10.0, 20.0, 100.0, 30.0));
                assert_eq!(color, Color::rgb(0x11, 0x22, 0x33));
            }
            other => panic!("{other:?}"),
        }
    }

    /// 返回 false 表示「交回内置画法」，但已经发出来的指令仍然交回去 ——
    /// 由调用方决定要不要用。`handled` 才是那个开关。
    #[test]
    fn returning_false_means_hand_it_back() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("tab", function(c, ctx) return false end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("tab", &input()).expect("runs");
        assert!(!out.handled);
        assert!(out.ops.is_empty());
    }

    /// 绘制期打日志不该把回调掐掉。
    ///
    /// `App.log` 以前要 bridge，而绘制回调里没有（重入）—— 于是它抛
    /// `no bridge`，整个回调中止、一条 op 都不上屏，症状是「皮肤什么都没画」。
    /// 现在它和 `print` 走同一条兜底链：bridge → [`crate::LogSink`] → stderr。
    #[test]
    fn logging_from_paint_does_not_abort_the_callback() {
        let h = host();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        h.set_log_sink(std::sync::Arc::new(move |m: &str| {
            sink.lock().expect("lock").push(m.to_string());
        }));
        h.load(
            "t",
            r##"
            App.ui.paint:on("tab", function(c, ctx)
                print("from print")
                App.log("from log")
                c:fill(ctx.rect, "#FF0000")
                return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("tab", &input()).expect("runs");
        assert!(out.handled, "logging must not turn the paint into a refusal");
        assert!(!out.ops.is_empty(), "logging must not eat the ops");
        assert_eq!(
            seen.lock().expect("lock").as_slice(),
            ["from print", "from log"],
            "both go to the sink, in call order"
        );
    }

    /// 返回数字 = 「我画了，而且内容有 N DIP 高」。数字本来就是真值，所以这是
    /// 纯扩展；`ctx` 里的 `bar`/`scroll` 就是配套的那两样。
    #[test]
    fn a_number_return_declares_the_content_height() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("pane.clock", function(c, ctx)
              c:fill(ctx.rect, "#101010")
              return 1200
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("pane.clock", &input()).expect("runs");
        assert!(out.handled);
        assert_eq!(out.content, Some(1200.0));
    }

    /// 布尔真值只说「画了」，不说内容多高 —— 不报就是不滚。
    #[test]
    fn returning_true_declares_no_content_height() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("pane.clock", function(c, ctx) return true end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        assert_eq!(h.paint("pane.clock", &input()).expect("runs").content, None);
    }

    /// 读了 `ctx.scroll` 的画法：滚动就得重算，否则滑块在动、内容不动。
    #[test]
    fn a_paint_that_reads_scroll_is_recomputed_when_it_moves() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("pane.log", function(c, ctx)
              c:fill({ x = 0, y = -ctx.scroll, w = 10, h = 10 }, "#FFFFFF")
              return 999
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let mut top = input();
        top.rect = (0.0, 0.0, 200.0, 200.0);
        top.bar = (190.0, 0.0, 10.0, 200.0);
        let a = h.paint("pane.log", &top).expect("runs");
        let mut scrolled = top.clone();
        scrolled.scroll = 40.0;
        let b = h.paint("pane.log", &scrolled).expect("runs");
        assert_ne!(a.ops, b.ops, "读了 scroll 的画法必须重算");
    }

    /// 没读 `ctx.scroll` 的画法：滚动不该让它重算，拿回来的是同一份结果。
    #[test]
    fn a_paint_that_ignores_scroll_keeps_its_cached_ops() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("pane.log", function(c, ctx)
              c:fill(ctx.rect, "#101010")
              return 999
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let mut top = input();
        top.rect = (0.0, 0.0, 200.0, 200.0);
        let a = h.paint("pane.log", &top).expect("runs");
        let mut scrolled = top.clone();
        scrolled.scroll = 40.0;
        let b = h.paint("pane.log", &scrolled).expect("runs");
        assert!(std::rc::Rc::ptr_eq(&a, &b), "没读 scroll 就该命中缓存");
    }

    /// 占位符与字面量：`@text` 走 ctx，`"OK"` 进字符串表且只进一次。
    #[test]
    fn text_placeholders_and_literals() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:text(ctx.rect, "@text", "#FFFFFF", { align = "center" })
              c:text(ctx.rect, "OK", "#FFFFFF")
              c:text(ctx.rect, "OK", "#FFFFFF")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("runs");
        assert_eq!(out.ops.len(), 3);
        assert_eq!(out.strings, vec!["OK".to_string()], "interned once");
        match &out.ops[0] {
            UiOp::Text { text, align, .. } => {
                assert_eq!(*text, TextSource::Text);
                assert_eq!(*align, Align::Center);
            }
            other => panic!("{other:?}"),
        }
        match &out.ops[1] {
            UiOp::Text { text, .. } => assert_eq!(*text, TextSource::Literal(TextId(0))),
            other => panic!("{other:?}"),
        }
    }

    /// 字体名也进表，`opts.font` 变成 `FontId`。
    #[test]
    fn a_font_name_becomes_an_id() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:text(ctx.rect, "A", "#FFFFFF", { font = "Consolas", ellipsis = true })
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("runs");
        assert_eq!(out.fonts, vec!["Consolas".to_string()]);
        match &out.ops[0] {
            UiOp::Text {
                font, ellipsis, ..
            } => {
                assert_eq!(*font, FontId(0));
                assert!(*ellipsis);
            }
            other => panic!("{other:?}"),
        }
    }

    /// `ctx.rect:inset` 真的缩了，链式写法可用。
    #[test]
    fn rect_inset_shrinks_in_place() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect:inset(4, 2), "#000000")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("runs");
        match out.ops[0] {
            UiOp::Fill { rect, .. } => {
                assert_eq!(rect, Rect::new(14.0, 22.0, 92.0, 26.0));
            }
            other => panic!("{other:?}"),
        }
    }

    /// 状态位从 ctx 里读得到，而且是布尔字段（不建表）。
    #[test]
    fn the_state_bits_are_boolean_fields() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              if ctx.hot and not ctx.active then
                c:fill(ctx.rect, "#00FF00")
              end
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let mut hot = input();
        hot.state = dg_skin::State::HOT.bits();
        let out = h.paint("status.pane", &hot).expect("runs");
        assert_eq!(out.ops.len(), 1);

        // 同时置 hot 与 active 就不该画了。
        let mut both = input();
        both.state = dg_skin::State::HOT.bits() | dg_skin::State::ACTIVE.bits();
        assert!(h.paint("status.pane", &both).expect("runs").ops.is_empty());
    }

    /// `ctx.thumb` 是块里那个「会动的部件」：滚动条是滑块，别处等于 `rect`。
    /// 它和 `rect` 各走各的字段，脚本不必按区域名分支。
    #[test]
    fn the_thumb_is_its_own_rect() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, "#101010")
              c:round(ctx.thumb, 3, "#FF0000")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let mut input = input();
        input.thumb = (12.0, 40.0, 4.0, 60.0);
        let out = h.paint("status.pane", &input).expect("runs");
        assert_eq!(out.ops.len(), 2);
        match out.ops[1] {
            UiOp::Round { rect, .. } => assert_eq!(rect, Rect::new(12.0, 40.0, 4.0, 60.0)),
            other => panic!("{other:?}"),
        }
    }

    /// 回调里抛错：不崩、不抛给调用方，按「没画」处理。
    #[test]
    fn a_failing_callback_falls_back() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx) error("boom") end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("still returns");
        assert!(!out.handled);
        assert!(out.ops.is_empty());
    }

    /// 绘制回调里拿不到 bridge：`App.doc` 之类必须报错，而不是悄悄改文档。
    #[test]
    fn the_paint_tier_has_no_bridge() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              local n = App.doc.line_count()   -- 没有 bridge，应当失败
              c:fill(ctx.rect, "#000000")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("returns");
        assert!(!out.handled, "the callback died, so it did not handle the part");
        assert!(out.ops.is_empty(), "nothing was drawn");
    }

    /// 调色板灌进去之后，`ctx.theme["status.bg"]` 拿得到颜色。
    #[test]
    fn the_palette_is_reachable_from_ctx() {
        let h = host();
        let mut theme = dg_render::Theme::default();
        theme.statusbar_bg = Color::rgb(0x12, 0x34, 0x56);
        h.set_palette(&theme);
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, ctx.theme["status.bg"])
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("runs");
        match out.ops[0] {
            UiOp::Fill { color, .. } => assert_eq!(color, Color::rgb(0x12, 0x34, 0x56)),
            other => panic!("{other:?}"),
        }
    }

    /// 换主题是**灌同一张表**，已经注册的回调不用重新注册。
    #[test]
    fn changing_the_palette_keeps_the_same_table() {
        let h = host();
        h.load(
            "t",
            r##"
            local seen = App.ui.paint
            seen:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, ctx.theme["status.bg"])
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let mut theme = dg_render::Theme::default();
        theme.statusbar_bg = Color::rgb(1, 2, 3);
        h.set_palette(&theme);
        let out = h.paint("status.pane", &input()).expect("runs");
        match out.ops[0] {
            UiOp::Fill { color, .. } => assert_eq!(color, Color::rgb(1, 2, 3)),
            other => panic!("{other:?}"),
        }
    }

    /// 两次绘制之间画布清空：上一帧的指令不会漏到下一帧。
    #[test]
    fn the_canvas_is_cleared_between_frames() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              if ctx.hot then c:fill(ctx.rect, "#111111") end
              c:fill(ctx.rect, "#222222")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let mut hot = input();
        hot.state = dg_skin::State::HOT.bits();
        assert_eq!(h.paint("status.pane", &hot).expect("runs").ops.len(), 2);
        // 同样的回调、非 hot：只剩一条。
        assert_eq!(h.paint("status.pane", &input()).expect("runs").ops.len(), 1);
    }

    /// `poly` 的点集进点表，`PointsId` 能切回来。
    #[test]
    fn poly_collects_its_points() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:poly({ {x=0,y=0}, {10,0}, {10,10} }, "#FFFFFF", 1, true)
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("runs");
        assert_eq!(out.points.len(), 3);
        match out.ops[0] {
            UiOp::Poly { points, closed, .. } => {
                assert!(closed);
                let slice = points.slice(&out.points).expect("in range");
                assert_eq!(slice[1], Point::new(10.0, 0.0));
            }
            other => panic!("{other:?}"),
        }
    }

    /// `ui.paint:list()` 列出已注册的区域，`unpaint` 之后就不在了。
    ///
    /// 断言写在脚本里：`App` 只在这个沙箱 `_ENV` 里，测试自己 `eval` 一段
    /// 是看不到它的。
    #[test]
    fn regions_are_listed_and_can_be_removed() {
        let h = host();
        h.load(
            "t",
            r##"
            local ui = App.ui
            ui.paint:on("status.pane", function() return true end)
            ui.paint:on("tab", function() return true end)
            local got = table.concat(ui.paint:list(), ",")
            assert(got == "status.pane,tab", "sorted regions, got " .. got)
            ui.paint:off("tab")
            local after = table.concat(ui.paint:list(), ",")
            assert(after == "status.pane", "after unpaint, got " .. after)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        assert!(h.has_paint("status.pane"));
        assert!(!h.has_paint("tab"));
    }

    /// 同一个格子画第二遍不进 Lua：缓存键一模一样，直接还上一份 op 列表。
    /// 回调里那个计数器是判据 —— 真跑过就会是 2。
    #[test]
    fn the_second_paint_of_the_same_cell_does_not_enter_lua() {
        let h = host();
        h.load(
            "t",
            r##"
            runs = 0
            App.ui.paint:on("status.pane", function(c, ctx)
              runs = runs + 1
              c:fill(ctx.rect, "#112233")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let first = h.paint("status.pane", &input()).expect("runs");
        let (hits, misses) = h.paint_stats();
        assert_eq!((hits, misses), (0, 1), "first paint is a miss");
        let second = h.paint("status.pane", &input()).expect("runs");
        assert_eq!(h.paint_stats(), (1, 1), "second paint hits");
        assert_eq!(second.ops.len(), first.ops.len());
        // 同一个 `Rc`：命中是克隆指针，不是复制 op 列表。
        assert!(std::rc::Rc::ptr_eq(&first, &second));
    }

    /// `--skin-debug` 默认关：回调只跑一遍。
    #[test]
    fn the_purity_check_is_off_by_default() {
        let h = host();
        h.load(
            "t",
            r##"
            runs = 0
            App.ui.paint:on("status.pane", function(c, ctx)
              runs = runs + 1
              c:fill(ctx.rect, "#112233")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        h.paint("status.pane", &input()).expect("runs");
        assert_eq!(h.paint_impurity_count(), 0);
    }

    /// 纯画法跑两遍，op 一模一样 —— 一处都不报。
    #[test]
    fn a_pure_paint_runs_twice_and_reports_nothing() {
        let h = host();
        h.load(
            "t",
            r##"
            runs = 0
            App.ui.paint:on("status.pane", function(c, ctx)
              runs = runs + 1
              c:fill(ctx.rect, "#112233")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        h.set_paint_debug(true);
        h.paint("status.pane", &input()).expect("runs");
        assert_eq!(h.paint_impurity_count(), 0, "a pure paint reports nothing");
    }

    /// 画法读了会变的东西（这里是自增计数）→ 两遍不一样 → 报一处。
    ///
    /// 同一个位置**只报一次**：换一个状态位让缓存不命中、再跑一遍，计数还是 1。
    #[test]
    fn an_impure_paint_is_reported_once_per_cell() {
        let h = host();
        h.load(
            "t",
            r##"
            runs = 0
            App.ui.paint:on("status.pane", function(c, ctx)
              runs = runs + 1
              -- 每跑一次换一个颜色：这是最典型的「paint 里有副作用」。
              c:fill(ctx.rect, runs % 2 == 0 and "#00FF00" or "#112233")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        h.set_paint_debug(true);
        h.paint("status.pane", &input()).expect("runs");
        assert_eq!(h.paint_impurity_count(), 1, "the second run differed");
        // 换一位状态 → 新键 → 真的再跑一遍，但位置没变，不重复报。
        let mut hot = input();
        hot.state = dg_skin::State::HOT.bits();
        h.paint("status.pane", &hot).expect("runs");
        assert_eq!(h.paint_impurity_count(), 1, "one report per cell, not per run");
    }

    /// 读过 `App.now()` 的画法不比：它本来就每毫秒都不一样，报了也是噪音。
    #[test]
    fn a_volatile_paint_is_not_called_impure() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              local t = App.now()
              c:fill(ctx.rect, "#112233")
              c:fill(ctx.rect:with_width(t % 10), "#445566")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        h.set_paint_debug(true);
        h.paint("status.pane", &input()).expect("runs");
        assert_eq!(h.paint_impurity_count(), 0, "a clock-driven paint is exempt");
    }

    /// 状态位变了就得重画 —— 键里有 state。
    #[test]
    fn changing_a_state_bit_misses() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              if ctx.hot then c:fill(ctx.rect, "#00FF00") end
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let cold = input();
        let mut hot = input();
        hot.state = dg_skin::State::HOT.bits();
        h.paint("status.pane", &cold).expect("runs");
        assert_eq!(h.paint("status.pane", &hot).expect("runs").ops.len(), 1);
        assert_eq!(h.paint_stats(), (0, 2), "a different state is a different key");
        // 换回来：cold 那份还在，命中。
        assert!(h.paint("status.pane", &cold).expect("runs").ops.is_empty());
        assert_eq!(h.paint_stats(), (1, 2));
    }

    fn row(line: u32, y: f32, state: u8) -> PaintRow {
        PaintRow {
            line,
            y,
            h: 20.0,
            rows: 1,
            display: line + 1,
            active: line == 3,
            folded: false,
            foldable: line % 2 == 0,
            bookmark: line == 1,
            icon: if line == 2 { 4 } else { 0 },
            state,
            tilde: line == 5,
            eol: match line % 4 {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => 3,
            },
        }
    }

    /// 一组好认的度量：断言里直接对数字。
    fn metrics() -> Metrics {
        Metrics {
            x_width: 8.0,
            cjk_width: 16.0,
            font_height: 16.0,
            line_height: 20.0,
            gutter_x: 48.0,
            line_number_x: 6.0,
            line_number_w: 30.0,
        }
    }

    /// `ctx.rows` 把行表原样交给回调：条数、几何、每个标记位都到得了。
    #[test]
    fn the_row_table_reaches_the_overlay() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("editor.overlay", function(c, ctx)
              local rows = ctx.rows
              -- 把「有几行」画成一条宽条，测试从 op 的宽度读它
              c:fill(ctx.rect:with_width(#rows * 10), "#101010")
              for _, r in ipairs(rows) do
                if r.active then
                  c:fill({ x = 0, y = r.y, w = 8, h = r.h }, "#00FF00")
                elseif r.bookmark then
                  c:fill({ x = 0, y = r.y, w = 8, h = r.h }, "#0000FF")
                elseif r.state == 1 then
                  c:fill({ x = 0, y = r.y, w = 8, h = r.h }, "#FF0000")
                end
              end
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let rows = vec![row(0, 0.0, 1), row(1, 20.0, 0), row(2, 40.0, 0), row(3, 60.0, 0)];
        let out = h
            .paint_rows("editor.overlay", &input(), &rows, Some(metrics()))
            .expect("runs");
        // 1 条宽度 + 三条标记（第 0 行 state=1、第 1 行书签、第 3 行光标）
        assert_eq!(out.ops.len(), 4, "{:?}", out.ops);
        let UiOp::Fill { rect, .. } = out.ops[0] else {
            panic!("{:?}", out.ops[0]);
        };
        assert_eq!(rect.w, 40.0, "four rows went through");
        let colours: Vec<Color> = out.ops[1..]
            .iter()
            .map(|op| match op {
                UiOp::Fill { color, .. } => *color,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            colours,
            vec![
                Color::rgb(0xFF, 0x00, 0x00),
                Color::rgb(0x00, 0x00, 0xFF),
                Color::rgb(0x00, 0xFF, 0x00),
            ],
            "state / bookmark / active each win their own row"
        );
    }

    /// `editor.overlay` **不进缓存**：同样的输入连跑两遍，缓存一次都没被问过。
    ///
    /// 这是它和别的区域最本质的差别 —— 输入是可见行表，滚动一格就全变，比对
    /// 四十行和直接跑一遍一样贵。
    #[test]
    fn the_overlay_never_touches_the_cache() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("editor.overlay", function(c, ctx)
              c:fill(ctx.rect, "#101010")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let rows = vec![row(0, 0.0, 0)];
        for _ in 0..3 {
            assert_eq!(
                h.paint_rows("editor.overlay", &input(), &rows, Some(metrics()))
                    .expect("runs")
                    .ops
                    .len(),
                1
            );
        }
        assert_eq!(h.paint_stats(), (0, 0), "neither looked up nor stored");
    }

    /// 别的区域没有行表：`ctx.rows` 是空数组（`#rows == 0`），读字段不会报错。
    #[test]
    fn other_regions_see_an_empty_row_table() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect:with_width(#ctx.rows * 10), "#101010")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("runs");
        let UiOp::Fill { rect, .. } = out.ops[0] else {
            panic!("{:?}", out.ops[0]);
        };
        assert_eq!(rect.w, 0.0);
    }

    /// 绘制回调里 `print` **不能**把这一帧打断。
    ///
    /// 绘制期没有 bridge（重入），`print` 曾经抛 `no bridge`，整个回调中止 ——
    /// 症状是「皮肤什么都没画」，而 `print` 恰恰是画不出来时第一个想用的东西。
    /// 现在没有 bridge 就退到 stderr，op 照画。
    #[test]
    fn print_does_not_abort_a_paint_callback() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              print("debug: drawing one bar")
              c:fill(ctx.rect, "#101010")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("runs");
        assert_eq!(out.ops.len(), 1, "print 之后那条 fill 还在");
    }

    /// `ctx.w` / `ctx.h` 就是 `ctx.rect` 的宽高，顶层可读。
    ///
    /// 曾经只有 `ctx.rect.w`：皮肤照计划书写 `w = ctx.w` 时，mlua 抛
    /// 「unknown field」，整个回调中止、一条 op 都没有 —— 界面上是「什么都没画」，
    /// 比画错还难查。这条测试把两个字段钉在 `rect` 上。
    #[test]
    fn w_and_h_mirror_the_rect() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill({ x = 0, y = 0, w = ctx.w, h = ctx.h }, "#101010")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("runs");
        let UiOp::Fill { rect, .. } = out.ops[0] else {
            panic!("{:?}", out.ops[0]);
        };
        assert_eq!((rect.w, rect.h), (100.0, 30.0), "input() 的 rect 宽高");
    }

    /// `ctx.metrics` 给的是编辑器自己那份度量，不是脚本再量一遍。
    #[test]
    fn the_editor_metrics_reach_the_overlay() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("editor.overlay", function(c, ctx)
              local m = ctx.metrics
              c:fill({ x = 0, y = 0, w = m.x_width, h = m.line_height }, "#101010")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h
            .paint_rows(
                "editor.overlay",
                &input(),
                &[row(0, 0.0, 0)],
                Some(metrics()),
            )
            .expect("runs");
        let UiOp::Fill { rect, .. } = out.ops[0] else {
            panic!("{:?}", out.ops[0]);
        };
        assert_eq!((rect.w, rect.h), (8.0, 20.0));
    }

    /// 别的区域没有度量：`ctx.metrics` 是空表，读字段得 nil 而不是报错。
    #[test]
    fn other_regions_see_empty_metrics() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect:with_width(ctx.metrics.x_width or 0), "#101010")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("runs");
        let UiOp::Fill { rect, .. } = out.ops[0] else {
            panic!("{:?}", out.ops[0]);
        };
        assert_eq!(rect.w, 0.0, "no metrics, no width");
    }

    /// **行表不会漏到别的区域**：`PaintCtx` 是共用的一个，画完编辑区不擦掉的话，
    /// 下一个区域的皮肤就读到编辑区的行。`set_rows` 的注释一直这么说，但
    /// `run_paint` 当时只写了 setter、没有清 —— 这条测试守的就是那一句。
    #[test]
    fn rows_do_not_leak_into_the_next_region() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("editor.overlay", function(c, ctx)
              c:fill(ctx.rect, "#101010")
              return true
            end)
            App.ui.paint:on("status.pane", function(c, ctx)
              -- 有行就画宽 99，没有画宽 0
              c:fill(ctx.rect:with_width(#ctx.rows * 99), "#202020")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        h.paint_rows(
            "editor.overlay",
            &input(),
            &[row(0, 0.0, 0), row(1, 20.0, 0)],
            Some(metrics()),
        )
        .expect("overlay runs");
        let out = h.paint("status.pane", &input()).expect("runs");
        let UiOp::Fill { rect, .. } = out.ops[0] else {
            panic!("{:?}", out.ops[0]);
        };
        assert_eq!(rect.w, 0.0, "the status bar saw no rows");
    }

    /// 行表的 `tilde` / `eol`：内置行号栏靠这两样画 `~` 和续行标记。
    #[test]
    fn the_row_carries_tilde_and_eol() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("editor.overlay", function(c, ctx)
              for _, r in ipairs(ctx.rows) do
                -- 宽 = tilde 记 100 + 行尾符名字的长度 × 10
                local w = (r.tilde and 100 or 0) + #r.eol * 10
                c:fill({ x = 0, y = r.y, w = w, h = r.h }, "#101010")
              end
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        // line 5 → tilde（row() 只在 5 上置位）、eol = 5 % 4 = 1 = CRLF（4 个字）
        let out = h
            .paint_rows("editor.overlay", &input(), &[row(5, 0.0, 0)], Some(metrics()))
            .expect("runs");
        let UiOp::Fill { rect, .. } = out.ops[0] else {
            panic!("{:?}", out.ops[0]);
        };
        assert_eq!(rect.w, 140.0, "tilde + 'CRLF'");
    }

    /// `ui.layout:pane{...}` 进注册表：默认值填好、同名覆盖、侧边写错当场报错。
    #[test]
    fn panes_register_with_defaults_and_overwrite_by_id() {
        let h = host();
        h.load(
            "a",
            r##"
            local ui = App.ui
            ui.layout:pane{ id = "clock", side = "left", title = "时钟", icon = 1325, size = 180, bar = 8 }
            ui.layout:pane{ id = "plain" }
            ui.layout:pane{ id = "clock", side = "left", title = "时钟 v2", icon = 1325, size = 180, bar = 8 }
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let panes = h.panes();
        assert_eq!(panes.len(), 2, "same id overwrote, it did not append");
        assert_eq!(panes[0].id, "clock");
        assert_eq!(panes[0].title, "时钟 v2");
        assert_eq!(panes[0].size, 180.0);
        assert_eq!(panes[0].bar, 8.0, "轨道宽是整数写的，也得读成 f32");
        assert_eq!(panes[0].region(), "pane.clock");
        // 只写 id：侧边默认 bottom、标题默认 id、图标 0、厚度 0、没有轨道。
        assert_eq!(panes[1].side, "bottom");
        assert_eq!(panes[1].title, "plain");
        assert_eq!(panes[1].icon, 0);
        assert_eq!(panes[1].size, 0.0);
        assert_eq!(panes[1].bar, 0.0);
    }

    /// 侧边写错不猜：报错并列出四个合法值。
    #[test]
    fn a_bad_pane_side_is_named() {
        let h = host();
        let err = h
            .load(
                "a",
                r#"App.ui.layout:pane{ id = "x", side = "middle" }"#,
                &mut NullBridge,
            )
            .expect_err("rejected");
        assert!(err.to_string().contains("middle"), "{err}");
        assert!(err.to_string().contains("left"), "{err}");
    }

    /// 重载把**同一个块**上一次声明的窗格清掉，不会越改越多。
    ///
    /// 热重载就是这么跑的：同一个文件名（块名）再 `load` 一次。
    #[test]
    fn reloading_drops_the_previous_panes() {
        let h = host();
        h.load("a", r#"App.ui.layout:pane{ id = "old" }"#, &mut NullBridge)
            .expect("loads");
        assert_eq!(h.panes().len(), 1);
        h.load("a", r#"App.ui.layout:pane{ id = "new" }"#, &mut NullBridge)
            .expect("reloads");
        let panes = h.panes();
        assert_eq!(panes.len(), 1, "the previous run's pane is gone");
        assert_eq!(panes[0].id, "new");
    }

    // ── 运行时改：`update` / `remove` / `list` ──────────────────────────

    /// 「推」那一半：一格已经声明好了，事件来了只改文字。
    ///
    /// 这是 §4.3 里「保存提示是推」的落点 —— 不用把整条声明重写一遍。
    #[test]
    fn update_pushes_status_text_without_redeclaring() {
        let h = host();
        h.load(
            "a",
            r##"
            App.ui.layout:status{
              { id = "msg", width = "fill" },
              { id = "clock", width = { max = 104 }, priority = 5 },
            }
            App.ui.layout:update("status.clock", { text = "已保存" })
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let row = h.status();
        let clock = row.iter().find(|s| s.id == "clock").expect("clock");
        assert_eq!(clock.template, "已保存");
        assert_eq!(clock.priority, 5, "没写的字段不动");
        // 布局脏了才有人去重排：这一位不置，屏幕上还是旧那一条。
        assert_eq!(h.take_dirty() & DIRTY_LAYOUT, DIRTY_LAYOUT);
    }

    /// `text` 和 `update` 是同一格的两条路。推了静态文字就得把拉取函数摘掉 ——
    /// 否则屏幕上还是函数算出来的那个，推了等于没推。
    #[test]
    fn pushing_text_takes_the_pull_function_out() {
        let h = host();
        h.load(
            "a",
            r##"
            App.ui.layout:status{ { id = "clock", update = function() return "12:00" end } }
            assert(App.ui.layout:list()["status.clock"].update ~= nil, "先有拉取函数")
            App.ui.layout:update("status.clock", { text = "已保存" })
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let row = h.status();
        let clock = row.iter().find(|s| s.id == "clock").expect("clock");
        assert_eq!(clock.template, "已保存");
        assert!(!clock.dynamic, "拉取函数已经摘掉了");
    }

    /// 改字段走和声明期同一套校验：改不动的字段当场报错并列出能改的。
    #[test]
    fn update_refuses_fields_it_cannot_change() {
        let h = host();
        let err = h
            .load(
                "a",
                r##"
                App.ui.layout:pane{ id = "clock" }
                App.ui.layout:update("pane.clock", { colour = "#fff" })
                "##,
                &mut NullBridge,
            )
            .expect_err("rejected");
        let text = err.to_string();
        assert!(text.contains("colour"), "{text}");
        assert!(text.contains("title"), "要列出能改的：{text}");
    }

    /// 没有那个元素时不留一个空壳：当场报错，并说清「先声明」。
    #[test]
    fn update_and_remove_refuse_a_missing_element() {
        let h = host();
        for src in [
            r#"App.ui.layout:update("pane.ghost", { title = "x" })"#,
            r#"App.ui.layout:remove("pane.ghost")"#,
        ] {
            let err = h.load("a", src, &mut NullBridge).expect_err("rejected");
            let text = err.to_string();
            assert!(text.contains("pane.ghost"), "{text}");
            assert!(text.contains("先声明"), "{text}");
        }
    }

    /// `remove` 把数组压实：中间留个空位，后面的项会被 `raw_len` 漏掉。
    #[test]
    fn remove_compacts_the_list() {
        let h = host();
        h.load(
            "a",
            r##"
            App.ui.layout:pane{ id = "one" }
            App.ui.layout:pane{ id = "two" }
            App.ui.layout:pane{ id = "three" }
            App.ui.layout:remove("pane.two")
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let ids: Vec<String> = h.panes().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, vec!["one", "three"], "中间那块拿掉了，后面那块顶上来");
    }

    /// `list()` 的键就是 `update` / `remove` 收的那个地址，三种元素各一份。
    #[test]
    fn list_addresses_every_kind() {
        let h = host();
        h.load(
            "a",
            r##"
            App.ui.layout:pane{ id = "clock", side = "right", title = "时钟" }
            App.ui.layout:status{ { id = "msg", width = "fill" } }
            App.ui.layout:toolbar{ icon = 60, tip = "切换", command = "skin.clock_format" }
            local all = App.ui.layout:list()
            assert(all["pane.clock"].title == "时钟", "窗格按 id 寻址")
            assert(all["pane.clock"].side == "right")
            assert(all["status.msg"].width == "fill", "状态段按 id 寻址")
            assert(all["toolbar.skin.clock_format"].tip == "切换", "按钮按命令名寻址")
            assert(all["toolbar.skin.clock_format"].icon == 60)
            local n = 0
            for _ in pairs(all) do n = n + 1 end
            assert(n == 3, "三种元素各一条，实际 " .. n)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
    }

    /// `update` 改的是**原来那条记录**，块名不能跟着换成调用者的 ——
    /// 换了之后声明方一重载就清不掉它，于是越跑越多。
    #[test]
    fn update_keeps_the_declaring_chunk() {
        let h = host();
        h.load_in(
            "owner",
            "panels.lua",
            r#"App.ui.layout:pane{ id = "clock" }"#,
            &mut NullBridge,
        )
        .expect("declares");
        h.load_in(
            "other",
            "tweak.lua",
            r#"App.ui.layout:update("pane.clock", { title = "秒表" })"#,
            &mut NullBridge,
        )
        .expect("updates");
        assert_eq!(h.panes()[0].title, "秒表", "改是改到了");
        // 声明方被拆掉，这条声明也该跟着走。块名要是被 update 换成了 other，
        // 它就留下来变成孤儿。
        h.purge_package("owner");
        assert!(h.panes().is_empty(), "块名还挂在声明方身上");
    }

    /// 状态栏段：声明顺序就是左右顺序，宽度四种写法各归各位。
    #[test]
    fn status_segments_parse_into_the_model() {
        let h = host();
        h.load(
            "a",
            r##"
            local ui = App.ui
            ui.layout:status{
              { id = "mode", text = "%M", priority = 90, click = "toggle_overwrite" },
              { id = "pos", text = "%l:%c", width = 60 },
              { id = "file", text = "%f", width = { min = 80, max = 260 } },
              { width = "fill" },
            }
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let segs = h.status();
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].id, "mode");
        assert_eq!(segs[0].template, "%M");
        assert_eq!(segs[0].priority, 90);
        assert_eq!(segs[0].click, "toggle_overwrite");
        assert_eq!(segs[0].width, dg_skin::Width::Auto, "没写就是 auto");
        assert!(!segs[0].dynamic, "没有 update 就不算动态");
        assert_eq!(segs[1].width, dg_skin::Width::Fixed(60.0));
        assert_eq!(segs[2].width, dg_skin::Width::Range { min: 80.0, max: 260.0 });
        assert_eq!(segs[3].id, "", "fill 段没有 id");
        assert_eq!(segs[3].width, dg_skin::Width::Fill);
        assert_eq!(segs[3].region(), None);
        assert_eq!(segs[0].region().as_deref(), Some("status.mode"));
    }

    /// 再声明一次是**整条替换**，不是往后追加。
    #[test]
    fn status_replaces_the_whole_row() {
        let h = host();
        h.load(
            "a",
            r#"App.ui.layout:status{ { id = "a" }, { id = "b" } }"#,
            &mut NullBridge,
        )
        .expect("loads");
        assert_eq!(h.status().len(), 2);
        h.load("a", r#"App.ui.layout:status{ { id = "c" } }"#, &mut NullBridge)
            .expect("reloads");
        let segs = h.status();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].id, "c");
        // 空表 = 一条都不要。
        h.load("a", "App.ui.layout:status{}", &mut NullBridge)
            .expect("clears");
        assert!(h.status().is_empty());
    }

    /// 没 id 又不是 fill：报错，并说清为什么（id 就是绘制区域名）。
    #[test]
    fn a_status_segment_needs_an_id_or_fill() {
        let h = host();
        let err = h
            .load("a", r#"App.ui.layout:status{ { text = "%M" } }"#, &mut NullBridge)
            .expect_err("rejected");
        assert!(err.to_string().contains("status."), "{err}");
    }

    /// 段名重复会共用一份缓存，当场拒绝。
    #[test]
    fn a_duplicate_status_id_is_refused() {
        let h = host();
        let err = h
            .load(
                "a",
                r#"App.ui.layout:status{ { id = "x" }, { id = "x" } }"#,
                &mut NullBridge,
            )
            .expect_err("rejected");
        assert!(err.to_string().contains("\"x\""), "{err}");
    }

    /// 宽度写错不猜：只认四种形状。
    #[test]
    fn a_bad_status_width_is_refused() {
        let h = host();
        let err = h
            .load(
                "a",
                r#"App.ui.layout:status{ { id = "x", width = "half" } }"#,
                &mut NullBridge,
            )
            .expect_err("rejected");
        assert!(err.to_string().contains("half"), "{err}");
    }

    /// `ui.layout:defaults("status")` 就是内置那十三个字段，可以直接改了再传回去。
    #[test]
    fn status_defaults_is_the_builtin_row() {
        let h = host();
        h.load(
            "a",
            r#"
            local ui = App.ui
            local segs = ui.layout:defaults("status")
            assert(#segs == 13, "13 built-in fields, got " .. #segs)
            assert(segs[1].id == "message" and segs[1].width == "fill")
            table.insert(segs, { id = "clock", priority = 5 })
            ui.layout:status(segs)
            "#,
            &mut NullBridge,
        )
        .expect("loads");
        let segs = h.status();
        assert_eq!(segs.len(), 14);
        assert_eq!(segs[0].id, "message");
        assert_eq!(segs[0].width, dg_skin::Width::Fill);
        assert_eq!(segs[0].priority, 100);
        assert_eq!(segs[13].id, "clock");
    }

    /// 不带参数调 `ui.layout:status()` 读回当前声明 —— 改完能原样传回去。
    #[test]
    fn status_reads_back_what_was_declared() {
        let h = host();
        h.load(
            "a",
            r#"
            local ui = App.ui
            ui.layout:status{ { id = "pos", text = "%l:%c", width = { min = 40, max = 90 }, priority = 70 } }
            local back = ui.layout:status()
            assert(#back == 1)
            assert(back[1].id == "pos")
            assert(back[1].text == "%l:%c")
            assert(back[1].priority == 70)
            assert(back[1].width.min == 40 and back[1].width.max == 90)
            assert(back[1].n == nil, "the chunk stamp is not the skin's business")
            ui.layout:status(back)
            "#,
            &mut NullBridge,
        )
        .expect("loads");
        assert_eq!(h.status()[0].width, dg_skin::Width::Range { min: 40.0, max: 90.0 });
    }

    /// `update` 回调跑在绘制档：拿得到 `ctx.doc`，拿不到 bridge。
    #[test]
    fn status_text_runs_the_update_callback() {
        let h = host();
        h.load(
            "a",
            r##"
            App.ui.layout:status{
              { id = "clock", update = function(ctx) return "12:00" end },
              { id = "doc", update = function(ctx) return ctx.doc.name end },
            }
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        assert!(h.status()[0].dynamic);
        assert_eq!(h.status_text("clock").as_deref(), Some("12:00"));
        // 没有 update 的段答 None：调用方退回模板。
        assert_eq!(h.status_text("nope"), None);
    }

    /// 回调抛错按「这一格没有文字」处理，不连累别的格。
    #[test]
    fn a_broken_update_yields_nothing() {
        let h = host();
        h.load(
            "a",
            r#"App.ui.layout:status{ { id = "boom", update = function() error("nope") end } }"#,
            &mut NullBridge,
        )
        .expect("loads");
        assert_eq!(h.status_text("boom"), None);
    }

    /// 重载后不再声明就回到内置那条 —— 旧声明不能留着。
    #[test]
    fn reloading_drops_the_previous_status() {
        let h = host();
        h.load("a", r#"App.ui.layout:status{ { id = "old" } }"#, &mut NullBridge)
            .expect("loads");
        assert_eq!(h.status().len(), 1);
        h.load("a", "-- nothing", &mut NullBridge).expect("reloads");
        assert!(h.status().is_empty());
    }

    /// 尺寸变了也得重画 —— 键里有宽高。
    #[test]
    fn changing_the_size_misses() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, "#112233")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let mut wide = input();
        wide.rect = (0.0, 0.0, 200.0, 30.0);
        h.paint("status.pane", &input()).expect("runs");
        h.paint("status.pane", &wide).expect("runs");
        assert_eq!(h.paint_stats(), (0, 2));
    }

    /// **文字只在回调真读它的时候才比对**。
    ///
    /// `c:text(r, "@text", …)` 是推荐写法：文字走占位符、回放时才替换，回调根本
    /// 不读 `ctx.text`，所以「行 12，列 34」每帧都变也照样命中。反过来，按文字
    /// 分支的皮肤读了 `ctx.text`，那就得老实比 —— 缓存不能替它猜。
    #[test]
    fn text_is_compared_only_when_the_callback_reads_it() {
        // 走占位符：不读 ctx.text。
        let placeholder = host();
        placeholder
            .load(
                "t",
                r##"
                App.ui.paint:on("status.pane", function(c, ctx)
                  c:text(ctx.rect, "@text", "#FFFFFF")
                  return true
                end)
                "##,
                &mut NullBridge,
            )
            .expect("loads");
        let mut a = input();
        let mut b = input();
        b.text = "行 12，列 34".into();
        placeholder.paint("status.pane", &a).expect("runs");
        placeholder.paint("status.pane", &b).expect("runs");
        assert_eq!(
            placeholder.paint_stats(),
            (1, 1),
            "a placeholder never reads ctx.text, so the text may change"
        );
        a.text = String::new();
        assert!(placeholder.paint("status.pane", &a).expect("runs").handled);

        // 按文字分支：读了 ctx.text，文字变了必须重算。
        let branching = host();
        branching
            .load(
                "t",
                r##"
                App.ui.paint:on("status.pane", function(c, ctx)
                  if ctx.text == "" then c:fill(ctx.rect, "#FF0000") end
                  return true
                end)
                "##,
                &mut NullBridge,
            )
            .expect("loads");
        let mut empty = input();
        empty.text = String::new();
        let mut full = input();
        full.text = "UTF-8".into();
        assert_eq!(branching.paint("status.pane", &empty).expect("runs").ops.len(), 1);
        assert!(branching.paint("status.pane", &full).expect("runs").ops.is_empty());
        assert_eq!(branching.paint_stats(), (0, 2));
    }

    /// 滑块同理：读了 `ctx.thumb` 就比，没读就不比。滚动条每滚一格 thumb 都变，
    /// 不这样分，整条就永远命中不了。
    #[test]
    fn the_thumb_is_compared_only_when_read() {
        let uses_thumb = host();
        uses_thumb
            .load(
                "t",
                r##"
                App.ui.paint:on("scroll.v", function(c, ctx)
                  c:round(ctx.thumb, 2, "#FFD166")
                  return true
                end)
                "##,
                &mut NullBridge,
            )
            .expect("loads");
        let mut moved = input();
        moved.thumb = (10.0, 90.0, 100.0, 30.0);
        uses_thumb.paint("scroll.v", &input()).expect("runs");
        uses_thumb.paint("scroll.v", &moved).expect("runs");
        assert_eq!(uses_thumb.paint_stats(), (0, 2), "a moved thumb is a new key");

        let ignores_thumb = host();
        ignores_thumb
            .load(
                "t",
                r##"
                App.ui.paint:on("scroll.v", function(c, ctx)
                  c:fill(ctx.rect, "#0E151B")
                  return true
                end)
                "##,
                &mut NullBridge,
            )
            .expect("loads");
        ignores_thumb.paint("scroll.v", &input()).expect("runs");
        ignores_thumb.paint("scroll.v", &moved).expect("runs");
        assert_eq!(
            ignores_thumb.paint_stats(),
            (1, 1),
            "a painter that never looks at the thumb does not care where it is"
        );
    }

    fn doc() -> PaintDoc {
        PaintDoc {
            encoding: "UTF-8".into(),
            bom: false,
            eol: "LF".into(),
            syntax: "rs".into(),
            lines: 12,
            read_only: false,
            modified: false,
            large: false,
        }
    }

    /// `ctx.doc` 把外壳推的快照原样交出去 —— 没推过时是空表，读字段得 nil 不报错。
    #[test]
    fn the_doc_snapshot_reaches_the_painter() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              local d = ctx.doc
              local colour = "#00FF00"
              if d.modified then colour = "#FF0000" end
              if d.eol == "CRLF" then colour = "#0000FF" end
              c:fill(ctx.rect, colour)
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");

        fn fill_of(out: &PaintOut) -> Color {
            match out.ops[0] {
                UiOp::Fill { color, .. } => color,
                ref other => panic!("{other:?}"),
            }
        }
        // 没推过快照：`d` 是空表，两个字段都是 nil → 走默认那支。
        let out = h.paint("status.pane", &input()).expect("runs");
        assert_eq!(fill_of(&out), Color::rgb(0x00, 0xFF, 0x00), "no snapshot");

        let mut d = doc();
        d.modified = true;
        h.set_paint_doc(d);
        let out = h.paint("status.pane", &input()).expect("runs");
        assert_eq!(fill_of(&out), Color::rgb(0xFF, 0x00, 0x00), "modified");

        let mut d = doc();
        d.eol = "CRLF".into();
        h.set_paint_doc(d);
        let out = h.paint("status.pane", &input()).expect("runs");
        assert_eq!(fill_of(&out), Color::rgb(0x00, 0x00, 0xFF), "eol wins");
    }

    /// 存盘、改一行之后只有**读过 `ctx.doc`** 的画法要重算 —— 没读的那些照样命中。
    #[test]
    fn changing_the_doc_misses_only_for_painters_that_read_it() {
        let reads_doc = host();
        reads_doc
            .load(
                "t",
                r##"
                App.ui.paint:on("status.pane", function(c, ctx)
                  if ctx.doc.modified then
                    c:fill(ctx.rect, "#FF0000")
                  else
                    c:fill(ctx.rect, "#00FF00")
                  end
                  return true
                end)
                "##,
                &mut NullBridge,
            )
            .expect("loads");
        reads_doc.set_paint_doc(doc());
        reads_doc.paint("status.pane", &input()).expect("runs");
        reads_doc.paint("status.pane", &input()).expect("runs");
        assert_eq!(reads_doc.paint_stats(), (1, 1), "unchanged doc hits");
        let mut changed = doc();
        changed.modified = true;
        reads_doc.set_paint_doc(changed.clone());
        reads_doc.paint("status.pane", &input()).expect("runs");
        assert_eq!(reads_doc.paint_stats(), (1, 2), "a flipped bit misses");

        let ignores_doc = host();
        ignores_doc
            .load(
                "t",
                r##"
                App.ui.paint:on("status.pane", function(c, ctx)
                  c:fill(ctx.rect, "#0E151B")
                  return true
                end)
                "##,
                &mut NullBridge,
            )
            .expect("loads");
        ignores_doc.set_paint_doc(doc());
        ignores_doc.paint("status.pane", &input()).expect("runs");
        ignores_doc.set_paint_doc(changed);
        ignores_doc.paint("status.pane", &input()).expect("runs");
        assert_eq!(
            ignores_doc.paint_stats(),
            (1, 1),
            "a painter that never looks at the doc does not care what it says"
        );
    }

    /// 死循环的绘制回调被预算掐掉：`paint` 照样返回，结果是「没画」。
    #[test]
    fn a_runaway_painter_is_cut_off() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, "#00FF00")
              while true do end
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("returns, not hangs");
        assert!(!out.handled, "a painter that never returns did not paint");
        // 掐断前发出来的指令还留在列表里；`handled` 是假的，五个调用点都在回放
        // 之前先看它，所以这一格仍然由内置画法画。
        assert_eq!(out.ops.len(), 1, "the fill before the loop survived");
    }

    /// 皮肤用 `pcall` 把预算错误吞掉、再返回 `true`，照样算「没画」。
    #[test]
    fn a_painter_that_swallows_the_budget_error_still_does_not_paint() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, "#00FF00")
              -- 钩子抛的错在这里被接住，循环退出，回调正常返回 true。
              pcall(function() while true do end end)
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let out = h.paint("status.pane", &input()).expect("returns");
        assert!(!out.handled, "a painter that blew its budget did not paint");
    }

    /// 一个正常的画法只烧掉预算的一小角 —— 这条守着「别把预算定得太紧」。
    #[test]
    fn a_plain_painter_uses_a_small_slice_of_its_budget() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              local r = ctx.rect
              c:gradient(r, "#16212B", "#0E151B", "v")
              c:fill(r:with_width(3), "#7FD1B9")
              c:text(r:inset(9, 0), "@text", "#90A4AE", { valign = "middle" })
              c:line(r.right - 1, r.y + 4, r.right - 1, r.bottom - 4, "#1E3A46", 1)
              if ctx.doc.modified then c:rect(r, "#2E4A5A", 1) end
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        h.set_paint_doc(doc());
        h.paint("status.pane", &input()).expect("runs");
        let used = h.budget_used();
        assert!(
            used <= 8 * crate::budget::STEP,
            "a plain painter used {used} instructions, budget is {}",
            crate::budget::PAINT
        );
    }

    /// 预算是**按次**算的，不是整条 VM 共用一个池子：两格各烧掉一半预算，都该成功。
    #[test]
    fn the_budget_is_renewed_for_every_cell() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              local n = 0
              for _ = 1, 60000 do n = n + 1 end
              c:fill(ctx.rect, n > 0 and "#00FF00" or "#FF0000")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let mut second = input();
        second.index = 1;
        let a = h.paint("status.pane", &input()).expect("runs");
        let used = h.budget_used();
        assert!(a.handled, "the first cell ran out after {used} instructions");
        // 第一格必须真烧掉大半预算，否则「第二格也成功」证明不了预算是按次算的。
        assert!(
            used > crate::budget::PAINT / 2,
            "the loop only burned {used}; raise the iteration count"
        );
        let b = h.paint("status.pane", &second).expect("runs");
        assert!(b.handled, "the second cell got a fresh budget");
    }

    /// 每帧推同一份快照不该把缓存推掉 —— 内容一样就当没发生。
    #[test]
    fn pushing_the_same_snapshot_again_keeps_the_hit() {
        let same = host();
        same.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              if ctx.doc.large then
                c:fill(ctx.rect, "#FF0000")
              else
                c:fill(ctx.rect, "#00FF00")
              end
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        same.set_paint_doc(doc());
        same.paint("status.pane", &input()).expect("runs");
        // 外壳每帧都推，内容不变。
        same.set_paint_doc(doc());
        same.paint("status.pane", &input()).expect("runs");
        assert_eq!(same.paint_stats(), (1, 1), "an identical snapshot is a no-op");
    }

    /// 换脚本必须把缓存丢掉 —— 留着就是拿旧脚本的画法画新脚本。
    #[test]
    fn reloading_the_skin_drops_the_cache() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, "#FF0000")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        match h.paint("status.pane", &input()).expect("runs").ops[0] {
            UiOp::Fill { color, .. } => assert_eq!(color, Color::rgb(0xFF, 0, 0)),
            other => panic!("{other:?}"),
        }
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, "#00FF00")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("reloads");
        match h.paint("status.pane", &input()).expect("runs").ops[0] {
            UiOp::Fill { color, .. } => {
                assert_eq!(color, Color::rgb(0, 0xFF, 0), "the new skin, not the cached one")
            }
            other => panic!("{other:?}"),
        }
    }

    /// 热重载时**语法错的皮肤不能把还在工作的那一份清掉**：`load` 先编译再
    /// `purge`，编译失败时旧的注册原封不动。
    #[test]
    fn a_syntax_error_on_reload_keeps_the_working_skin() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, "#FF0000")
              return true
            end)
            App.ui.paint:hint("tab", { cursor = "hand" })
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let err = h
            .load("t", "App.ui.paint:on(", &mut NullBridge)
            .expect_err("syntax error");
        assert!(!format!("{err}").is_empty());
        // 旧的那一套还在：画法照样跑，悬停声明照样读得到。
        match h.paint("status.pane", &input()).expect("still paints").ops[0] {
            UiOp::Fill { color, .. } => assert_eq!(color, Color::rgb(0xFF, 0, 0)),
            other => panic!("{other:?}"),
        }
        assert!(h.hint("tab").is_some(), "the old hint survived the bad reload");
    }

    /// 验收线：鼠标在 42 格的工具栏上挪一格，整条重画一遍，命中率要过 95%。
    /// 这一格挪动只改了两格的状态（原来那格从 hot 变普通、新那格变 hot），其余
    /// 40 格输入一模一样。缓存要做的就是把那 40 格变成「克隆一个指针」。
    #[test]
    fn a_toolbar_hover_step_hits_ninety_five_percent() {
        const CELLS: usize = 42;
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("toolbar.button", function(c, ctx)
              if ctx.hot then
                c:fill(ctx.rect, "#1B2C36")
                c:rect(ctx.rect, "#2E4A5A", 1)
              elseif ctx.checked then
                c:fill(ctx.rect, "#1E3A46")
              end
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");

        let cell = |i: usize, hot: usize| {
            let mut inp = input();
            inp.part = format!("cm_command_{i}");
            inp.text = format!("命令 {i}");
            inp.rect = (0.0, 0.0, 46.0, 27.0);
            inp.state = if i == hot { dg_skin::State::HOT.bits() } else { 0 };
            inp.index = i as u16;
            inp
        };

        // 第一遍：全是未命中。
        for i in 0..CELLS {
            h.paint("toolbar.button", &cell(i, usize::MAX)).expect("runs");
        }
        assert_eq!(h.paint_stats(), (0, CELLS as u64));

        // 悬停从第 7 格挪到第 8 格：整条重画。
        let before = h.paint_stats();
        for i in 0..CELLS {
            h.paint("toolbar.button", &cell(i, 7)).expect("runs");
        }
        for i in 0..CELLS {
            h.paint("toolbar.button", &cell(i, 8)).expect("runs");
        }
        let (hits, misses) = h.paint_stats();
        let step_hits = hits - before.0;
        let step_misses = misses - before.1;
        assert_eq!(step_hits + step_misses, 2 * CELLS as u64);
        let rate = step_hits as f64 / (step_hits + step_misses) as f64;
        assert!(
            rate > 0.95,
            "hover step hit rate {:.1}% ({step_hits} hits / {step_misses} misses)",
            rate * 100.0
        );
    }

    /// 尺寸和状态都一样、只是位置不同的两块，**不能**共用一份结果：页签条和
    /// 状态栏的 op 用的是客户区绝对坐标，`c:fill(ctx.rect, …)` 里带着左上角。
    /// 只按宽高做键的话，第二个页签会画到第一个页签的位置上去。
    #[test]
    fn the_same_size_at_a_different_place_is_a_different_cell() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("tab", function(c, ctx)
              c:fill(ctx.rect, "#112233")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let mut first = input();
        first.rect = (10.0, 0.0, 100.0, 20.0);
        first.part = "tab".into();
        let mut second = input();
        second.rect = (110.0, 0.0, 100.0, 20.0);
        second.part = "tab".into();

        let a = h.paint("tab", &first).expect("runs");
        let b = h.paint("tab", &second).expect("runs");
        assert_eq!(h.paint_stats(), (0, 2), "same size, different x");
        match (a.ops[0], b.ops[0]) {
            (UiOp::Fill { rect: ra, .. }, UiOp::Fill { rect: rb, .. }) => {
                assert_eq!(ra, Rect::new(10.0, 0.0, 100.0, 20.0));
                assert_eq!(rb, Rect::new(110.0, 0.0, 100.0, 20.0), "the second cell's own x");
            }
            other => panic!("{other:?}"),
        }
    }

    /// 读了 `App.now()` 的画法不进缓存 —— 存下来等于把动画冻住。
    ///
    /// 判据不是「有没有副作用」，而是「这份 op 是不是只在那一毫秒成立」。
    /// 例子里那条靠 50 ms 心跳扫来扫去的细线就靠这一条活着。
    #[test]
    fn a_painter_that_reads_the_clock_is_never_cached() {
        let h = host();
        h.load(
            "t",
            r##"
            App.ui.paint:on("status.pane", function(c, ctx)
              local t = App.now() % 100
              c:fill(ctx.rect:with_width(t + 1), "#112233")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        h.paint("status.pane", &input()).expect("runs");
        h.paint("status.pane", &input()).expect("runs");
        assert_eq!(
            h.paint_stats(),
            (0, 2),
            "both runs go to Lua, neither is stored"
        );
    }

    /// 命中必须和重算**一模一样**：缓存对回放层是透明的。
    ///
    /// 这条比逐像素比更硬：同一个脚本、同一份输入，一份走缓存、一份走全新宿主，
    /// 出来的 op 列表、字面量表、点集、接管标志都得逐项相等。
    #[test]
    fn a_hit_is_indistinguishable_from_a_fresh_run() {
        let script = r##"
            App.ui.paint:on("tab", function(c, ctx)
              if ctx.active then
                c:round(ctx.rect, 4, "#1E3A46")
                c:text(ctx.rect, "@text", "#FFFFFF", { ellipsis = true })
                c:poly({ {x = 1, y = 2}, {x = 3, y = 4} }, "#FFD166")
              end
              return true
            end)
            "##;
        let mut inp = input();
        inp.part = "tab".into();
        inp.state = dg_skin::State::ACTIVE.bits();

        let cached = host();
        cached.load("t", script, &mut NullBridge).expect("loads");
        cached.paint("tab", &inp).expect("runs");
        let hit = cached.paint("tab", &inp).expect("hits");
        assert_eq!(cached.paint_stats(), (1, 1));

        let fresh = host();
        fresh.load("t", script, &mut NullBridge).expect("loads");
        let once = fresh.paint("tab", &inp).expect("runs");

        assert_eq!(format!("{:?}", hit.ops), format!("{:?}", once.ops));
        assert_eq!(hit.strings, once.strings);
        assert_eq!(format!("{:?}", hit.points), format!("{:?}", once.points));
        assert_eq!(hit.handled, once.handled);
    }

    /// 悬停声明读得回来：区域名归一化，指针名解析成枚举，`tip` 原样存着等展开。
    #[test]
    fn a_hint_declaration_reads_back() {
        let h = host();
        h.load(
            "t",
            r#"
            App.ui.paint:hint("tab", { cursor = "hand", tip = "@label" })
            App.ui.paint:hint("toolbar.button", { cursor = "hand" })
            "#,
            &mut NullBridge,
        )
        .expect("loads");

        let spec = h.hint("tab").expect("declared");
        assert_eq!(spec.cursor, Some(dg_skin::Cursor::Hand));
        assert_eq!(
            spec.resolve_tip("保存", "保存文件", "1001").as_deref(),
            Some("保存文件")
        );
        // 工具栏只声明了指针：提示归控件，`s:hint` 里不写它。
        let bar = h.hint("toolbar.button").expect("declared");
        assert_eq!(bar.cursor, Some(dg_skin::Cursor::Hand));
        assert_eq!(bar.resolve_tip("保存", "保存文件", "1001"), None);
        // 没声明过的区域没有。
        assert!(h.hint("status.pane").is_none());
        assert!(h.has_hints());
    }

    /// `tip` 只有本来没有提示的区域收。写在别处当场报错，并说清提示归谁 ——
    /// 静默忽略会让人对着「提示怎么不出来」查半天。
    #[test]
    fn a_tip_on_a_control_owned_region_is_refused() {
        let h = host();
        for region in ["toolbar.button", "status.pane"] {
            let err = h
                .load(
                    "t",
                    &format!(r#"App.ui.paint:hint("{region}", {{ tip = "@text" }})"#),
                    &mut NullBridge,
                )
                .expect_err("refused");
            let msg = format!("{err}");
            assert!(msg.contains(region), "{msg}");
            assert!(msg.contains("tab / dock.tab / pane.<id>"), "{msg}");
        }
        // 拒掉的那条不留半份注册。
        assert!(h.hint("toolbar.button").is_none());
    }

    /// 写错区域名的画法当场报错。
    ///
    /// 以前是**静默接受**：认不出来的名字注册得上、也真的会被调用，但进不了
    /// 缓存键 —— 于是每一帧都重跑一遍 Lua，画面看着对，代价白付。这种「能画但
    /// 一直在白算」比报错难查得多。
    ///
    /// 注意 `status.<任意>` 现在**是合法的**（见 `Region` 的家族规则）：段名就是
    /// 缓存键的一部分，所以这里拿一个连家族前缀都不对的名字。
    #[test]
    fn a_typo_in_a_region_is_refused_at_declaration() {
        let h = host();
        let err = h
            .load(
                "t",
                r#"App.ui.paint:on("statusbar.pane", function() return true end)"#,
                &mut NullBridge,
            )
            .expect_err("bad region");
        assert!(format!("{err}").contains("statusbar.pane"), "{err}");
        assert!(!h.has_paint("statusbar.pane"), "no half registration");
    }

    /// 区域名和指针名都当场校验 —— 皮肤里写错一个词，错在这一行，不是「指针
    /// 怎么不动」。
    #[test]
    fn a_typo_in_a_hint_is_refused_at_declaration() {
        let h = host();
        let bad_region = h
            .load(
                "t",
                r#"App.ui.paint:hint("toolbar.botton", { cursor = "hand" })"#,
                &mut NullBridge,
            )
            .expect_err("bad region");
        assert!(
            format!("{bad_region}").contains("toolbar.botton"),
            "{bad_region}"
        );

        let bad_cursor = h
            .load("t", r#"App.ui.paint:hint("tab", { cursor = "pointer" })"#, &mut NullBridge)
            .expect_err("bad cursor");
        assert!(format!("{bad_cursor}").contains("pointer"), "{bad_cursor}");
        // 报错的那一条不留半份注册。
        assert!(h.hint("tab").is_none());
    }

    /// 空声明等于没写；`unhint` 撤得掉；重载只清自己那一块。
    #[test]
    fn hints_are_empty_until_declared_and_dropped_on_reload() {
        let h = host();
        h.load(
            "a",
            r#"App.ui.paint:hint("tab", { cursor = "hand" })"#,
            &mut NullBridge,
        )
        .expect("loads");
        assert!(h.hint("tab").is_some());

        // 一条什么都没有的声明不算声明。
        h.load("a", r#"App.ui.paint:hint("tab", {})"#, &mut NullBridge)
            .expect("loads");
        assert!(h.hint("tab").is_none());

        h.load(
            "a",
            r#"App.ui.paint:hint("tab", { cursor = "hand" })"#,
            &mut NullBridge,
        )
        .expect("loads");
        h.load("a", r#"App.ui.paint:unhint("tab")"#, &mut NullBridge)
            .expect("loads");
        assert!(h.hint("tab").is_none());

        // 另一个块重载不该动我这一条。
        h.load(
            "a",
            r#"App.ui.paint:hint("tab", { tip = "@text" })"#,
            &mut NullBridge,
        )
        .expect("loads");
        h.load("b", "", &mut NullBridge).expect("loads");
        assert!(h.hint("tab").is_some());
        h.load("a", "", &mut NullBridge).expect("loads");
        assert!(h.hint("tab").is_none());
    }

    /// `ui.layout:toolbar` 是**追加**：调两次就是两个按钮，顺序就是声明顺序。
    ///
    /// 和 `ui.layout:status` 相反（那个是整条替换）—— 工具栏那四十几个按钮是用户自己
    /// 排的，皮肤只有加一格的权力。
    #[test]
    fn toolbar_declarations_append_in_order() {
        let h = host();
        h.load(
            "t",
            r##"
            local ui = App.ui
            ui.layout:toolbar{ icon = 60, tip = "时钟", command = "a.clock", after = 57600 }
            ui.layout:toolbar{ icon = 61, command = "b.save" }
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let rows = h.toolbar_buttons();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].icon, 60);
        assert_eq!(rows[0].tip, "时钟");
        assert_eq!(rows[0].command, "a.clock");
        assert_eq!(rows[0].after, 57600);
        // 没写 tip 就用命令名 —— 空提示等于没提示。
        assert_eq!(rows[1].tip, "b.save");
        assert_eq!(rows[1].after, 0);
    }

    /// 没有 `command` 的按钮点下去什么也不发生，画出来只会让人以为坏了。
    #[test]
    fn toolbar_without_a_command_is_refused() {
        let h = host();
        let err = h
            .load(
                "t",
                r#"App.ui.layout:toolbar{ icon = 3 }"#,
                &mut NullBridge,
            )
            .expect_err("refused");
        assert!(format!("{err}").contains("command"), "{err}");
    }

    /// 重载换掉的是同一个 chunk 的那几条，不是再追加一遍。
    #[test]
    fn reloading_replaces_the_toolbar_rows_of_that_chunk() {
        let h = host();
        h.load(
            "t",
            r#"App.ui.layout:toolbar{ icon = 1, command = "one" }"#,
            &mut NullBridge,
        )
        .expect("loads");
        assert_eq!(h.toolbar_buttons().len(), 1);
        h.load(
            "t",
            r#"App.ui.layout:toolbar{ icon = 2, command = "two" }"#,
            &mut NullBridge,
        )
        .expect("loads");
        let rows = h.toolbar_buttons();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].command, "two");
    }

    /// `--ui-lint` 的清单：声明过画法的区域，排序去重。
    #[test]
    fn paint_regions_lists_every_declared_callback_sorted() {
        let h = host();
        h.load(
            "t",
            r##"
            local ui = App.ui
            ui.paint:on("status.pane", function(c, ctx) return true end)
            ui.paint:on("toolbar.button", function(c, ctx) return true end)
            ui.paint:on("dock.tab", function(c, ctx) return true end)
            ui.paint:on("status.pane", function(c, ctx) return true end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        assert_eq!(
            h.paint_regions(),
            vec![
                "dock.tab".to_string(),
                "status.pane".to_string(),
                "toolbar.button".to_string(),
            ]
        );
    }

    /// 没声明过画法就是空清单 —— lint 拿它判断「这个皮肤不接管绘制」。
    #[test]
    fn paint_regions_is_empty_without_callbacks() {
        let h = host();
        h.load("t", r#"local ui = App.ui"#, &mut NullBridge)
            .expect("loads");
        assert!(h.paint_regions().is_empty());
    }

    /// 回调抛的错要能被 `take_paint_error` 取走，而且取一次就没了。
    ///
    /// 绘制期没有返回值能带错误，stderr 是唯一的去处 —— `--ui-lint` 要在表里
    /// 标出是哪一格挂了，靠的就是这一份。
    #[test]
    fn a_paint_error_is_kept_for_the_caller_and_taken_once() {
        let h = host();
        h.load(
            "t",
            r##"
            local ui = App.ui
            ui.paint:on("status.pane", function(c, ctx)
              error("boom")
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        assert!(h.take_paint_error().is_none(), "还没画过");
        let out = h.paint("status.pane", &input()).expect("注册过");
        assert!(!out.handled, "抛错等于没画");
        let err = h.take_paint_error().expect("错了就有记录");
        assert!(err.contains("boom"), "{err}");
        assert!(h.take_paint_error().is_none(), "取走一次就没了");
    }

    /// 画成功时不该留下上一次的错。
    #[test]
    fn a_successful_paint_clears_the_previous_error() {
        let h = host();
        h.load(
            "t",
            r##"
            local ui = App.ui
            ui.paint:on("status.pane", function(c, ctx)
              if ctx.part == "Encoding" then error("boom") end
              c:fill(ctx.rect, "#112233")
              return true
            end)
            "##,
            &mut NullBridge,
        )
        .expect("loads");
        let mut a = input();
        let _ = h.paint("status.pane", &a);
        assert!(h.take_paint_error().is_some());
        a.part = "LineCol".to_string();
        assert!(h.paint("status.pane", &a).expect("注册过").handled);
        assert!(h.take_paint_error().is_none(), "成功那次不该继承旧错");
    }

    /// 一个最小的 bridge：绘制测试不需要文档，但 `load` 要一个。
    struct NullBridge;

    impl crate::Bridge for NullBridge {
        fn alert(&mut self, _msg: &str) {}
        fn log(&mut self, _msg: &str) {}
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
        fn span_text(&self, _s: crate::Pos, _e: crate::Pos) -> Option<String> {
            None
        }
        fn insert_at(&mut self, _p: crate::Pos, _text: &str) -> bool {
            false
        }
        fn replace_span(&mut self, _s: crate::Pos, _e: crate::Pos, _text: &str) -> bool {
            false
        }
        fn sel_count(&self) -> usize {
            0
        }
        fn sel_range(&self, _index: usize) -> Option<(crate::Pos, crate::Pos)> {
            None
        }
        fn sel_text(&self) -> String {
            String::new()
        }
        fn sel_set(&mut self, _s: crate::Pos, _e: crate::Pos) {}
    }
}
