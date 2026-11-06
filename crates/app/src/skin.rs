//! 进程级的外观：度量、每格形状、具名字体。
//!
//! 三样东西**全部来自主题文件**：`[Metrics]` / `[Shape]` / `[Fonts]`，
//! 只有 `themes/<name>.ini` 这一个来源 —— 同一个字段不该有两个门。
//!
//! 为什么是进程级而不是每文档：度量（状态栏多高、页签多高）决定**窗口怎么排**，
//! 一个进程只有一份布局；颜色才是每文档一份。两者混在一起会变成「切页签时窗口
//! 突然重排」。
//!
//! 绘制代码不直接读这里：它读 `Theme` 上的颜色，形状和字体在读主题时已经展开成
//! `Shape` / `FontSpec` 了。这个模块只做三件事：装、查、算。

use std::path::Path;
use std::sync::RwLock;

use dg_render::theme::ThemeFile;
use dg_skin::{FontSpec, FontToken, Fonts, Metrics, Recipe, State, ThemeMetricReport};

/// 合成后的度量表。
static METRICS: RwLock<Metrics> = RwLock::new(Metrics::new());
/// 每格形状。`shape()` 在页签条/状态栏的绘制路径上，读它是一次原子 load。
static RECIPE: RwLock<Recipe> = RwLock::new(Recipe::new());
/// 具名字体。`font_spec()` 只在建 HFONT 时读，不在绘制里。
static FONTS: RwLock<Fonts> = RwLock::new(Fonts::new());
/// 一份主题展开之后剩下的问题。
///
/// 三段分开记是因为修法不同：度量要改数字，形状要改区域名或键，字体要改字段。
/// 合成一行只是为了打日志方便。
#[derive(Debug, Default)]
pub struct Report {
    pub metrics: ThemeMetricReport,
    pub shapes: Vec<String>,
    pub fonts: Vec<String>,
}

impl Report {
    /// 一条问题都没有？
    pub fn is_empty(&self) -> bool {
        self.metrics.is_empty() && self.shapes.is_empty() && self.fonts.is_empty()
    }

    /// 一行摘要，给日志和诊断页用；没问题时是空串。
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let m = self.metrics.summary();
        if !m.is_empty() {
            parts.push(m);
        }
        parts.extend(self.shapes.iter().cloned());
        parts.extend(self.fonts.iter().cloned());
        parts.join("；")
    }
}

/// 按当前主题重算度量、形状、字体。
///
/// 调用的地方只有一处：`apply_theme_by_name`（主题读完之后）。必须在
/// `panels.layout` 之前 —— 分隔带厚度就是从这里读的。
///
/// `over` 是脚本改的那几格（`App.ui.style:metric` / `:shape` / `:font`）。它是
/// **最后一层**：出厂值 → `themes/<name>.ini` → 脚本。传空表就是「照主题来」。
///
/// 写不通的键**不在这里失败**：主题文件是共用的手写文件，一条写错不该让整套
/// 外观消失。问题进 [`Report`]，由调用方打日志。
pub fn recompose(theme: &ThemeFile, over: &dg_script::style::Overrides) -> Report {
    let mut metrics = Metrics::new();
    let metrics_report = metrics.apply_theme(theme);
    let (mut recipe, shapes) = Recipe::from_theme_file(theme);
    let (mut fonts, fonts_report) = Fonts::from_theme_file(theme);

    // 运行时那三样盖上去。颜色不在这里：它是每文档一份，由调用方逐份 `set_token`。
    let mut shapes = shapes;
    let font_problems = fonts_report;
    for (id, written) in over.metrics() {
        // `set` 自己夹取（和主题文件同一条规则），夹掉了要说出来 —— 写 500 得到
        // 48 而没人提，就是「改了没反应」的另一种样子。
        let stored = metrics.set(*id, *written);
        if stored != *written {
            shapes.push(format!("style.metric {}={written} 夹成 {stored}", id.as_str()));
        }
    }
    for s in over.shapes() {
        if let Err(e) = recipe.set(s.region, s.state.as_deref(), s.key, s.value.as_value()) {
            shapes.push(format!("style.shape {}：{e}", s.region));
        }
    }
    for (token, patch) in over.fonts() {
        // 盖在主题给的那份之上：`style:font("mono", {size=12})` 不该把字面换掉。
        let base = fonts.get(*token).cloned().unwrap_or_default();
        fonts.set(*token, patch.apply(&base));
    }

    *METRICS.write().unwrap_or_else(|e| e.into_inner()) = metrics;
    *RECIPE.write().unwrap_or_else(|e| e.into_inner()) = recipe;
    *FONTS.write().unwrap_or_else(|e| e.into_inner()) = fonts;
    // 布局在 `dg-ui` 里，它读自己那份快照。分隔带的厚度就是靠这条线过去的。
    dg_ui::panel::set_metrics(metrics);

    Report {
        metrics: metrics_report,
        shapes,
        fonts: font_problems,
    }
}

/// 当前度量。主题也没写 `[Metrics]` 时就是出厂值 —— 绘制代码只读这里，不读常量。
///
/// 是一次 `RwLock` 读（一个原子 load），绘制路径上可以随手调；真要抠那几纳秒
/// 的话，各窗格在 `WM_DPICHANGED` 时把值抄进自己的 `Metrics` 结构体里（页签条
/// 就是这么做的）。
pub fn metrics() -> Metrics {
    *METRICS.read().unwrap_or_else(|e| e.into_inner())
}

/// 皮肤要求的工具栏图标格边长（16/24/32）。没写就返回 `None`，让调用方照旧读
/// `setting.ini` 的 `IconSize` —— 主题不该默默盖掉用户自己的设置。
pub fn toolbar_icon() -> Option<i32> {
    let value = metrics().get(dg_skin::MetricId::ToolbarIcon);
    if value == dg_skin::MetricId::ToolbarIcon.default_value() {
        None
    } else {
        Some(value as i32)
    }
}

/// 某个字体 token 的配置。没写这一项就是 `None`（= 用内置字体）。
///
/// 回的是克隆：`FontSpec` 只有一个 `String`，而这条路径每次建字体只走一次
/// （`build_fonts`），不在绘制里。
pub fn font_spec(token: FontToken) -> Option<FontSpec> {
    FONTS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(token)
        .cloned()
}

/// 某个区域在某组状态下的形状。没写时是**出厂形状**（全 0、不换字体）——
/// 所以绘制代码不需要判「有没有配形状」，读到的值天然等于内置外观。
pub fn shape(region: &str, state: State) -> dg_skin::Shape {
    RECIPE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .resolve(region, state)
}

// ---------------------------------------------------------------------------
// `--ui-lint`
// ---------------------------------------------------------------------------

/// `--ui-lint` 的结果：一张能直接 `diff` 的表，外加「有几处错」给退出码用。
#[derive(Debug)]
pub struct LintReport {
    pub text: String,
    /// 抛过错的**区域**数（不是次数）。`> 0` 时进程该以 1 退出 —— 一张画法抛错的
    /// 皮肤是坏的，「它还能跑」只是因为它错的那一格退回了内置画法。
    pub errors: usize,
}

/// 把 `skin.lua` 的每个画法在**无窗口**环境里跑一遍，报「这一格到底画出了什么」。
///
/// 为什么要有它：脚本（多数是 LLM 写的）最常见的坏法不是画错，而是**根本没被
/// 调到** —— `p:on("status.pane", …)` 里 `return` 写早了一行，屏幕上那一格就
/// 静悄悄退回内置画法，没有任何人报错。`App.ui.inspect(x,y)` 解决不了这件事：
/// 它只在回调**已经**被调用时才有意义。一张离线清单可以 —— 而且不用开窗、
/// 不用截图、输出能进 `diff`。
///
/// 跨平台：`dg-script` 本身没有窗口依赖，所以这条路径在 cocoa/gtk 上一样能用。
pub fn lint(lua_path: &Path) -> Result<LintReport, String> {
    if !lua_path.is_file() {
        return Err(format!("{}: 文件不存在", lua_path.display()));
    }
    let host = dg_script::ScriptHost::new().map_err(|e| e.0)?;
    // 装载期和绘制期打的日志都收起来：表尾统一报条数，别和表格搅在一起。
    let logs: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    {
        let sink = std::sync::Arc::clone(&logs);
        host.set_log_sink(std::sync::Arc::new(move |m: &str| {
            if let Ok(mut v) = sink.lock() {
                v.push(m.to_string());
            }
        }));
    }
    host.run_file(lua_path, &mut dg_script::HeadlessBridge)
        .map_err(|e| format!("{}: {e}", lua_path.display()))?;

    // `ctx.doc` 给一份像那么回事的快照：不读它的画法不受影响，读它的也不至于
    // 因为「没文档」而走另一条分支（那正是要检查的那条）。
    host.set_paint_doc(dg_script::PaintDoc {
        encoding: "UTF-8".to_string(),
        bom: false,
        eol: "LF".to_string(),
        syntax: "rs".to_string(),
        lines: 120,
        read_only: false,
        modified: true,
        large: false,
    });

    let regions = host.paint_regions();
    let states = lint_states();
    let rows = lint_rows();
    let segments = host.status();
    // 一个区域可能要跑好几遍：`status.pane` 是**整条通用的兜底画法**，每一格都
    // 走它、靠 `ctx.part` 区分。只跑一遍等于只检查了第一格。
    let jobs = lint_jobs(&regions, &segments);
    let mut reports: Vec<RegionReport> = regions.iter().map(|r| RegionReport::new(r)).collect();

    for (region, part) in &jobs {
        let geom = LintGeom::of(region);
        let Some(idx) = regions.iter().position(|r| r == region) else {
            continue;
        };
        for (name, state) in &states {
            let input = dg_script::PaintInput {
                rect: geom.rect,
                thumb: geom.thumb,
                bar: geom.bar,
                scroll: geom.scroll,
                state: state.bits(),
                part: part.clone(),
                text: lint_text(region).to_string(),
                label: "Label".to_string(),
                value: "42".to_string(),
                dpi: 96,
                index: 0,
            };
            // 编辑区那两个区域只走 `paint_rows`：`ctx.rows` 是它们的存在理由，
            // 走 `paint` 会拿到一张空表，等于没检查。
            let out = if region.starts_with("editor.") {
                host.paint_rows(region, &input, &rows, Some(lint_metrics()))
            } else {
                host.paint(region, &input)
            };
            let budget = host.budget_used();
            let err = host.take_paint_error();
            reports[idx].push(*name, out.as_deref(), budget, err);
        }
    }

    // 数**区域**，不数「跑了几次」：一格坏画法在六档状态里抛六次，报「6 处错误」
    // 会让人以为有六格坏了。
    let errors = reports.iter().filter(|r| r.error.is_some()).count();
    let mut warnings: Vec<String> = Vec::new();
    lint_cross_check(&host, &regions, &mut warnings);

    Ok(LintReport {
        text: format_lint(lua_path, &reports, &warnings, &logs, states.len(), errors),
        errors,
    })
}

/// 「跑哪些 (区域, `ctx.part`)」。
///
/// 绝大多数区域一遍就够，`part` 就是区域名点号后面那段。唯一的例外是
/// `status.pane`：它是整条状态栏的**兜底**画法，每一格都走它、靠 `ctx.part`
/// 区分，只跑一遍等于只检查了第一格。皮肤没声明 `ui.layout:status` 时退回内置那 13 格，
/// 这里只跑 `message` 一格 —— 其余 12 格走的是同一段代码，多跑只是重复。
fn lint_jobs(regions: &[String], segments: &[dg_skin::Segment]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for region in regions {
        if region == "status.pane" {
            if segments.is_empty() {
                out.push((region.clone(), "message".to_string()));
            } else {
                for s in segments.iter().filter(|s| !s.id.is_empty()) {
                    out.push((region.clone(), s.id.clone()));
                }
            }
        } else {
            let part = region.split_once('.').map_or("", |(_, s)| s);
            out.push((region.clone(), part.to_string()));
        }
    }
    out
}

/// 表格跑的那几档状态。`normal` 打头 —— 皮肤通常只对 `normal` 写了分支。
fn lint_states() -> Vec<(&'static str, dg_skin::State)> {
    use dg_skin::State;
    vec![
        ("normal", State::normal()),
        ("hot", State::HOT),
        ("active", State::ACTIVE),
        ("pressed", State::PRESSED),
        ("disabled", State::DISABLED),
        ("checked", State::CHECKED),
    ]
}

/// 编辑区那三行假行。给三行而不是零行：`ctx.rows[1]` 存不存在会把画法分成
/// 两条路，零行只能查到「没内容时怎么画」那一条。
fn lint_rows() -> Vec<dg_script::PaintRow> {
    let mut out = Vec::with_capacity(3);
    for i in 0..3u32 {
        out.push(dg_script::PaintRow {
            line: i,
            y: i as f32 * 20.0,
            h: 20.0,
            rows: 1,
            display: i + 1,
            active: i == 1,
            folded: false,
            foldable: i == 2,
            bookmark: i == 0,
            icon: 0,
            state: if i == 1 { 1 } else { 0 },
            tilde: false,
            eol: if i == 2 { 0 } else { 2 },
        });
    }
    out
}

/// 编辑区字体度量。96 DPI 的 Consolas 10 pt 差不多就是这个数。
fn lint_metrics() -> dg_script::Metrics {
    dg_script::Metrics {
        x_width: 8.0,
        cjk_width: 16.0,
        font_height: 15.0,
        line_height: 20.0,
        gutter_x: 60.0,
        line_number_x: 8.0,
        line_number_w: 36.0,
    }
}

/// 区域名 → 一份「像那么回事」的几何。
///
/// 数字不重要（皮肤该按 `ctx.rect` 算，不按区域名猜），但**比例**要像：给滚动条
/// 一个 200×14 的长条、给状态格一个 120×22 的窄条，`if ctx.rect.h < 20` 这类
/// 分支才走对路。
struct LintGeom {
    rect: (f32, f32, f32, f32),
    thumb: (f32, f32, f32, f32),
    bar: (f32, f32, f32, f32),
    scroll: f32,
}

impl LintGeom {
    fn of(region: &str) -> Self {
        let none = (0.0, 0.0, 0.0, 0.0);
        let full = |w: f32, h: f32| LintGeom {
            rect: (0.0, 0.0, w, h),
            thumb: (0.0, 0.0, w, h),
            bar: none,
            scroll: 0.0,
        };
        if region.starts_with("scroll.v") {
            return LintGeom {
                rect: (0.0, 0.0, 14.0, 200.0),
                thumb: (2.0, 10.0, 10.0, 40.0),
                bar: (0.0, 0.0, 14.0, 200.0),
                scroll: 20.0,
            };
        }
        if region.starts_with("scroll.h") {
            return LintGeom {
                rect: (0.0, 0.0, 200.0, 14.0),
                thumb: (10.0, 2.0, 40.0, 10.0),
                bar: (0.0, 0.0, 200.0, 14.0),
                scroll: 20.0,
            };
        }
        if region.starts_with("pane.") {
            // 内容区让出轨道宽 —— 这是宿主给 `ctx.rect` 的口径（见 `PaintInput::bar`）。
            return LintGeom {
                rect: (0.0, 0.0, 246.0, 180.0),
                thumb: (246.0, 20.0, 14.0, 60.0),
                bar: (246.0, 0.0, 14.0, 180.0),
                scroll: 20.0,
            };
        }
        if region.starts_with("status.") {
            return full(120.0, 22.0);
        }
        if region.starts_with("toolbar") {
            return full(32.0, 32.0);
        }
        if region.starts_with("editor.") {
            return full(640.0, 320.0);
        }
        if region == "tab" || region.starts_with("dock.tab") {
            return full(140.0, 28.0);
        }
        full(200.0, 100.0)
    }
}

/// `@text` 的样例值。按区域给点像样的词，别让「截断」和「对齐」查不出来。
fn lint_text(region: &str) -> &'static str {
    if region.starts_with("status.") {
        "UTF-8"
    } else if region.starts_with("toolbar") {
        "Save"
    } else if region == "tab" || region.starts_with("dock.tab") {
        "main.rs"
    } else {
        "Sample 文本 42"
    }
}

/// 这个区域的画法**应该**看状态吗？
///
/// 只看这几类：按钮、页签、滚动条、编辑区。窗格和状态格不看状态是正常的
/// （时钟不会因为鼠标移上去就变），拿它们报「`ctx.state` 没用上」纯属噪音。
fn state_sensitive(region: &str) -> bool {
    region.starts_with("toolbar")
        || region == "tab"
        || region.starts_with("dock.tab")
        || region.starts_with("scroll.")
        || region.starts_with("editor.")
        || region.starts_with("menu")
}

/// 一个区域的累计账。
struct RegionReport {
    name: String,
    runs: usize,
    ops_max: usize,
    ops_sum: usize,
    drawn: usize,
    content: Option<f32>,
    reads: u8,
    budget_max: u32,
    error: Option<String>,
    /// 每次跑出来的 op 条数，用来发现「状态变了但画法没变」。
    per_state: Vec<usize>,
}

impl RegionReport {
    fn new(name: &str) -> Self {
        RegionReport {
            name: name.to_string(),
            runs: 0,
            ops_max: 0,
            ops_sum: 0,
            drawn: 0,
            content: None,
            reads: 0,
            budget_max: 0,
            error: None,
            per_state: Vec::new(),
        }
    }

    fn push(
        &mut self,
        _state: &str,
        out: Option<&dg_script::PaintOut>,
        budget: u32,
        error: Option<String>,
    ) {
        self.runs += 1;
        self.budget_max = self.budget_max.max(budget);
        if let Some(e) = error {
            if self.error.is_none() {
                self.error = Some(e);
            }
        }
        let n = out.map_or(0, |o| o.ops.len());
        self.per_state.push(n);
        self.ops_sum += n;
        self.ops_max = self.ops_max.max(n);
        if let Some(o) = out {
            if o.handled {
                self.drawn += 1;
            }
            if let Some(c) = o.content {
                self.content = Some(c);
            }
            for (bit, _) in READ_NAMES {
                if o.reads.has(bit) {
                    self.reads |= bit;
                }
            }
        }
    }
}

/// 声明与画法对不对得上：**画法没人认领** → 死代码。
///
/// 反方向（声明了但没画法）不查：状态段和窗格都有内置兜底 —— 状态格退回内置
/// 文字画法，窗格退回一块底色。那不算错，报了只会淹掉真问题。
fn lint_cross_check(host: &dg_script::ScriptHost, regions: &[String], out: &mut Vec<String>) {
    let panes = host.panes();
    let pane_regions: Vec<String> = panes.iter().map(|p| p.region()).collect();
    for region in regions {
        if region.starts_with("pane.") && !pane_regions.iter().any(|r| r == region) {
            out.push(format!(
                "画法没有对应窗格：`{region}` —— 没有 `ui.layout:pane{{id=...}}` 认领它，这段代码永远不会跑"
            ));
        }
    }

    // 活着的 `status.<id>` 区域 = 皮肤声明过的段；皮肤没接管时是内置那 14 格
    // （`ui.paint:on("status.LineCol")` 在没接管的状态栏上照样跑得到）。
    let segments = host.status();
    let live: Vec<String> = if segments.is_empty() {
        dg_ui::statusbar::PANES
            .iter()
            .map(|f| format!("status.{}", f.name()))
            .collect()
    } else {
        segments
            .iter()
            .filter(|s| !s.id.is_empty())
            .map(|s| format!("status.{}", s.id))
            .collect()
    };
    for region in regions {
        // `status.pane` 是**整条通用的兜底画法**，不是某一格的段名：皮肤没声明
        // `ui.layout:status` 时每一格都走它（`ctx.part` 区分是哪一格）。它永远有活干。
        if region == "status.pane" {
            continue;
        }
        if region.starts_with("status.") && !live.iter().any(|r| r == region) {
            out.push(format!(
                "画法没有对应状态段：`{region}` —— 没有任何一段的 id 是它，这段代码永远不会跑"
            ));
        }
    }
}

/// `Reads` 的位 → 名字。顺序就是文档里那个顺序。
const READ_NAMES: [(u8, &str); 8] = [
    (dg_script::Reads::PART, "part"),
    (dg_script::Reads::TEXT, "text"),
    (dg_script::Reads::LABEL, "label"),
    (dg_script::Reads::VALUE, "value"),
    (dg_script::Reads::THUMB, "thumb"),
    (dg_script::Reads::DOC, "doc"),
    (dg_script::Reads::SCROLL, "scroll"),
    (dg_script::Reads::BAR, "bar"),
];

fn format_lint(
    path: &Path,
    reports: &[RegionReport],
    warnings: &[String],
    logs: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    states: usize,
    errors: usize,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("ui-lint: {}\n", path.display()));
    if reports.is_empty() {
        out.push_str("\n没有任何 `ui.paint:on(区域, 函数)` —— 这个皮肤不接管绘制。\n");
        for w in warnings {
            out.push_str(&format!("warning: {w}\n"));
        }
        return out;
    }
    out.push_str(&format!(
        "\n{} 个区域 · {states} 档状态 · 96 dpi · 无窗口\n\n",
        reports.len()
    ));

    let width = reports
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(8)
        .max(8);
    out.push_str(&format!(
        "{:<width$}  {:>5}  {:>4}  {:>5}  {:>7}  {:>5}  {}\n",
        "region",
        "ops",
        "max",
        "drawn",
        "content",
        "instr",
        "reads",
        width = width
    ));
    for r in reports {
        let ops = if r.runs == 0 {
            0
        } else {
            r.ops_sum / r.runs
        };
        let content = match r.content {
            Some(c) => format!("{c:.0}"),
            None => "-".to_string(),
        };
        let mut reads: Vec<&str> = Vec::new();
        for (bit, name) in READ_NAMES {
            if r.reads & bit != 0 {
                reads.push(name);
            }
        }
        let reads = if reads.is_empty() {
            "-".to_string()
        } else {
            reads.join(" ")
        };
        // 预算按 1000 条指令一格跳，所以这里只有「几千」的分辨率。`<1k` 是
        // 「一次钩子都没响」—— 那才是绝大多数皮肤的正常值。
        let instr = if r.budget_max == 0 {
            "<1k".to_string()
        } else {
            format!("{}k", r.budget_max / 1000)
        };
        out.push_str(&format!(
            "{:<width$}  {:>5}  {:>4}  {:>5}  {:>7}  {:>5}  {}\n",
            r.name,
            ops,
            r.ops_max,
            format!("{}/{}", r.drawn, r.runs),
            content,
            instr,
            reads,
            width = width
        ));
    }

    // 表格下面按「需要人管」的顺序列细节：先错，再疑点，最后声明对不上。
    let broken: Vec<&RegionReport> = reports.iter().filter(|r| r.error.is_some()).collect();
    if !broken.is_empty() {
        out.push_str("\n错误（这一格退回了内置画法，完整栈在 stderr）：\n");
        for r in broken {
            let msg = r.error.as_deref().unwrap_or("");
            // 只要第一行：栈是给人看的，进表会把表撑散、也没法 diff。
            let first = msg.lines().next().unwrap_or("").trim();
            let more = msg.lines().count().saturating_sub(1);
            if more > 0 {
                out.push_str(&format!("  {}: {first}（另有 {more} 行栈）\n", r.name));
            } else {
                out.push_str(&format!("  {}: {first}\n", r.name));
            }
        }
    }
    let idle: Vec<&RegionReport> = reports.iter().filter(|r| r.drawn == 0).collect();
    if !idle.is_empty() {
        out.push_str("\n从未说「我画了」（回调返回 nil/false，或一条 op 都没有）：\n");
        for r in idle {
            out.push_str(&format!("  {}\n", r.name));
        }
    }
    let empty: Vec<&RegionReport> = reports
        .iter()
        .filter(|r| r.drawn > 0 && r.ops_max == 0)
        .collect();
    if !empty.is_empty() {
        out.push_str("\n说画了但一条 op 都没发（`c:` 调用漏了，或者全被条件跳过）：\n");
        for r in empty {
            out.push_str(&format!("  {}\n", r.name));
        }
    }
    let flat: Vec<&RegionReport> = reports
        .iter()
        .filter(|r| state_sensitive(&r.name))
        .filter(|r| {
            r.runs > 1
                && r.ops_max > 0
                && r.per_state.iter().all(|n| *n == r.per_state[0])
        })
        .collect();
    if !flat.is_empty() {
        out.push_str("\n六档状态画出完全一样的 op（`ctx.state` 没被用到）：\n");
        for r in flat {
            out.push_str(&format!("  {}\n", r.name));
        }
    }
    if !warnings.is_empty() {
        out.push_str("\n声明与画法对不上：\n");
        for w in warnings {
            out.push_str(&format!("  {w}\n"));
        }
    }
    if let Ok(logs) = logs.lock() {
        if !logs.is_empty() {
            out.push_str(&format!("\n日志 {} 条（装载期与绘制期，全部走 stderr）：\n", logs.len()));
            for line in logs.iter().take(10) {
                out.push_str(&format!("  {line}\n"));
            }
            if logs.len() > 10 {
                out.push_str(&format!("  …… 还有 {} 条\n", logs.len() - 10));
            }
        }
    }
    out.push_str(&format!(
        "\n结论：{} 个区域，{} 处错误\n",
        reports.len(),
        errors
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── `--ui-lint` ──────────────────────────────────────────────────────

    /// 造一个临时脚本文件跑一遍，跑完删掉。
    ///
    /// `lint` 只读那一个文件，不需要包目录、不需要清单 —— 少造一份就少一处
    /// 「测试夹具自己写错」。
    fn lint_src(tag: &str, lua: &str) -> Result<LintReport, String> {
        let dir = std::env::temp_dir().join(format!("dg-lint-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("paint.lua");
        if !lua.is_empty() {
            std::fs::write(&path, lua).expect("write");
        }
        let out = lint(&path);
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    /// 一份干净的画法：每个区域都画、都说「我画了」，一处错都没有。
    #[test]
    fn lint_reports_a_healthy_skin_as_clean() {
        let report = lint_src(
            "ok",
            r##"
            local ui = App.ui
            ui.paint:on("status.pane", function(c, ctx)
              c:fill(ctx.rect, "#112233")
              return true
            end)
            ui.paint:on("toolbar.button", function(c, ctx)
              c:fill(ctx.rect, "#112233")
              return true
            end)
            "##,
        )
        .expect("runs");
        assert_eq!(report.errors, 0, "{}", report.text);
        assert!(report.text.contains("status.pane"), "{}", report.text);
        assert!(report.text.contains("toolbar.button"), "{}", report.text);
        assert!(report.text.contains("0 处错误"), "{}", report.text);
        // 每一档状态都画了 —— `drawn` 那一列是 6/6（`status.pane` 每格再乘一遍）。
        assert!(report.text.contains("6/6"), "{}", report.text);
    }

    /// 抛错的画法要计数并进退出码 —— 它在屏幕上只是「悄悄退回内置画法」。
    #[test]
    fn lint_counts_a_raising_callback_as_an_error() {
        let report = lint_src(
            "raising",
            r##"
            local ui = App.ui
            ui.paint:on("status.pane", function(c, ctx)
              error("boom")
            end)
            "##,
        )
        .expect("runs");
        assert_eq!(report.errors, 1, "{}", report.text);
        assert!(report.text.contains("错误"), "{}", report.text);
        assert!(report.text.contains("boom"), "{}", report.text);
        // 只报一次：六档状态跑出同一个错，表里不该出现六行。
        assert_eq!(report.text.matches("boom").count(), 1, "{}", report.text);
    }

    /// 「说画了但一条 op 都没发」和「从不说我画了」是两种不同的坏法，分开报。
    #[test]
    fn lint_separates_the_two_ways_of_drawing_nothing() {
        let report = lint_src(
            "silent",
            r##"
            local ui = App.ui
            ui.paint:on("status.pane", function(c, ctx) return true end)
            ui.paint:on("toolbar.button", function(c, ctx) end)
            "##,
        )
        .expect("runs");
        assert!(
            report.text.contains("说画了但一条 op 都没发"),
            "{}",
            report.text
        );
        assert!(
            report.text.contains("从未说「我画了」"),
            "{}",
            report.text
        );
    }

    /// 画法没人认领 → 死代码。窗格要 `ui.layout:pane`，状态格要 `ui.layout:status` 里有那一段。
    #[test]
    fn lint_flags_paints_that_no_declaration_claims() {
        let report = lint_src(
            "dead",
            r##"
            local ui = App.ui
            ui.paint:on("pane.ghost", function(c, ctx)
              c:fill(ctx.rect, "#112233")
              return true
            end)
            ui.paint:on("status.nowhere", function(c, ctx)
              c:fill(ctx.rect, "#112233")
              return true
            end)
            "##,
        )
        .expect("runs");
        assert!(report.text.contains("pane.ghost"), "{}", report.text);
        assert!(report.text.contains("status.nowhere"), "{}", report.text);
        assert!(report.text.contains("永远不会跑"), "{}", report.text);
    }

    /// 没有 `skin.lua` 是**错误**而不是「0 个区域」：那条路多半是路径写错了。
    #[test]
    fn lint_refuses_a_missing_file() {
        let err = lint_src("empty", "").expect_err("文件不存在");
        assert!(err.contains("文件不存在"), "{err}");
    }
}
