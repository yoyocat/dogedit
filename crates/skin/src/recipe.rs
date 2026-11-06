//! 配方：**按区域、按状态**生效的形状键。
//!
//! 颜色和几何各有归宿：颜色进 [`dg_render::Theme`] 的 token，全局几何进
//! [`crate::metrics::Metrics`] 的具名度量。配方管第三类 —— 既不是颜色、也不是
//! 全局标量的东西：这一格的圆角多大、格与格之间留多宽、边框几像素、用哪号字。
//! 它们按区域不同，同一个区域里还按状态不同（悬停时圆角变小、按下时边框变粗）。
//!
//! ## 和 `Metrics` 的分界线
//!
//! 一条硬规矩：**同一件事不许有两个来源**。`pad_x` 已经是 `[metrics]` 里的度量
//! （页签左右内边距），所以 `[style.tab] pad_x` 会被 [`crate::skin`] 拒掉，并
//! 指回 `[metrics]`。这里只放度量表里没有的键。
//!
//! ## 三层覆盖，顺序固定
//!
//! ```toml
//! [style.tab]                    # 第一层：这个区域的常态
//! radius = 4
//!
//! [style.tab.states.active]      # 第二层：活动页签
//! radius = 0
//!
//! [style.tab.states.modified]    # 第二层：改过的页签
//! gap = 2
//! ```
//!
//! 同一组状态里命中多个 `states` 时，按 [`State::NAMES`] 的顺序依次叠 ——
//! `hot` 在前、`alt` 在后，后面的盖前面的。**不按 toml 里的书写顺序**：那张表
//! 按键排序遍历，谁赢不能看字母序，更不能看人写在哪一行。
//!
//! 每一层用 `Option` 记「写没写过」，所以 `radius = 0` 是「明确要直角」，
//! 和「没写」不是一回事 —— 没有这一点，状态层就没法把常态层的圆角取消掉。

use std::fmt;

use dg_render::theme::ThemeFile;

use crate::region::Region;
use crate::state::State;

/// 一个形状键。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ShapeKey {
    /// 圆角半径，DIP。默认 0（直角）。
    Radius,
    /// 相邻两格之间留的空隙，DIP。默认 0（贴在一起）。
    Gap,
    /// 描边宽度，DIP。默认 0（不描边）。
    Border,
    /// 格内上下留白，DIP。默认 0。
    PadY,
    /// 字体名（`font.ui` 这类字体 token）。默认空串 = 用这个区域自己的字体。
    Font,
}

impl ShapeKey {
    /// 全部形状键，顺序即 [`ShapeKey::NAMES`] 的顺序，也是主题体检的顺序。
    pub const ALL: [ShapeKey; 5] = [
        ShapeKey::Radius,
        ShapeKey::Gap,
        ShapeKey::Border,
        ShapeKey::PadY,
        ShapeKey::Font,
    ];

    /// 名字表，主题文件 `[Shape]` 段里就写这些词。
    pub const NAMES: [&'static str; 5] = ["radius", "gap", "border", "pad_y", "font"];

    /// 主题文件里的写法。
    pub const fn name(self) -> &'static str {
        match self {
            ShapeKey::Radius => "radius",
            ShapeKey::Gap => "gap",
            ShapeKey::Border => "border",
            ShapeKey::PadY => "pad_y",
            ShapeKey::Font => "font",
        }
    }

    /// 认一个键名。大小写不敏感，和样式键一致。
    pub fn parse(name: &str) -> Option<ShapeKey> {
        ShapeKey::ALL
            .into_iter()
            .find(|k| k.name().eq_ignore_ascii_case(name))
    }

    /// 数值键的夹取区间（闭区间）。`font` 不是数值，回 `None`。
    ///
    /// 和度量表同一个态度：越界**夹住**而不是报错 —— 皮肤作者手写
    /// `radius = 40` 的意思是「圆一点」，不是「我打错了」，没必要开不了窗。
    pub const fn range(self) -> Option<(f32, f32)> {
        match self {
            ShapeKey::Radius => Some((0.0, 24.0)),
            ShapeKey::Gap => Some((0.0, 16.0)),
            ShapeKey::Border => Some((0.0, 4.0)),
            ShapeKey::PadY => Some((0.0, 32.0)),
            ShapeKey::Font => None,
        }
    }
}

/// 一层形状覆盖。`None` = 这一层没写这个键。
///
/// 用 `Option` 而不是「默认值即未写」，是因为 `radius = 0` 必须能把上一层的
/// 圆角取消掉 —— 状态层存在的意义就是「和常态不一样」。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapePatch {
    radius: Option<f32>,
    gap: Option<f32>,
    border: Option<f32>,
    pad_y: Option<f32>,
    font: Option<String>,
}

impl ShapePatch {
    /// 一层什么都没写？
    pub fn is_empty(&self) -> bool {
        self.radius.is_none()
            && self.gap.is_none()
            && self.border.is_none()
            && self.pad_y.is_none()
            && self.font.is_none()
    }

    /// 这一层写过这个**数值**键吗？（`font` 不算，走 [`ShapePatch::font`]。）
    pub fn number_written(&self, key: ShapeKey) -> bool {
        match key {
            ShapeKey::Radius => self.radius.is_some(),
            ShapeKey::Gap => self.gap.is_some(),
            ShapeKey::Border => self.border.is_some(),
            ShapeKey::PadY => self.pad_y.is_some(),
            ShapeKey::Font => false,
        }
    }

    /// 写一个数值键，按 [`ShapeKey::range`] 夹住。`font` 走 [`ShapePatch::set_font`]。
    ///
    /// 返回 `false` 表示这个键不是数值键（调用方该走另一条路）。
    pub fn set_number(&mut self, key: ShapeKey, value: f32) -> bool {
        let Some((lo, hi)) = key.range() else {
            return false;
        };
        let v = if value.is_nan() { lo } else { value.clamp(lo, hi) };
        match key {
            ShapeKey::Radius => self.radius = Some(v),
            ShapeKey::Gap => self.gap = Some(v),
            ShapeKey::Border => self.border = Some(v),
            ShapeKey::PadY => self.pad_y = Some(v),
            ShapeKey::Font => return false,
        }
        true
    }

    /// 写字体名。空串等于「没写」—— 那和「用区域自己的字体」是一回事。
    pub fn set_font(&mut self, name: &str) {
        let name = name.trim();
        self.font = if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        };
    }

    /// 这一层写的数值。
    pub fn number(&self, key: ShapeKey) -> Option<f32> {
        match key {
            ShapeKey::Radius => self.radius,
            ShapeKey::Gap => self.gap,
            ShapeKey::Border => self.border,
            ShapeKey::PadY => self.pad_y,
            ShapeKey::Font => None,
        }
    }

    /// 这一层写的字体名。
    pub fn font(&self) -> Option<&str> {
        self.font.as_deref()
    }

    /// 把这一层叠到 `base` 上：写过的盖掉，没写的不动。
    pub fn apply_to(&self, base: &mut Shape) {
        if let Some(v) = self.radius {
            base.radius = v;
        }
        if let Some(v) = self.gap {
            base.gap = v;
        }
        if let Some(v) = self.border {
            base.border = v;
        }
        if let Some(v) = self.pad_y {
            base.pad_y = v;
        }
        if let Some(v) = &self.font {
            base.font = v.clone();
        }
    }
}

/// 解析之后的形状：每个键都有值，绘制代码不用再判空。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shape {
    /// 圆角半径，DIP。
    pub radius: f32,
    /// 相邻两格之间的空隙，DIP。
    pub gap: f32,
    /// 描边宽度，DIP。
    pub border: f32,
    /// 格内上下留白，DIP。
    pub pad_y: f32,
    /// 字体名；空串表示用这个区域自己的字体。
    pub font: String,
}

impl Shape {
    /// 一个数值键。
    pub fn number(&self, key: ShapeKey) -> f32 {
        match key {
            ShapeKey::Radius => self.radius,
            ShapeKey::Gap => self.gap,
            ShapeKey::Border => self.border,
            ShapeKey::PadY => self.pad_y,
            ShapeKey::Font => 0.0,
        }
    }

    /// 出厂形状：全 0、不换字体。**空皮肤必须等于这个** —— 它是「没这张表」
    /// 的那一版外观。
    pub fn is_default(&self) -> bool {
        *self == Shape::default()
    }
}

/// 一个区域的形状：常态那一层 + 每个状态一层。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegionRecipe {
    base: ShapePatch,
    /// 状态名 → 那一层的覆盖。顺序按第一次写进来的先后，应用时另按
    /// [`State::NAMES`] 排 —— 见模块文档。
    states: Vec<(String, ShapePatch)>,
}

impl RegionRecipe {
    /// 常态那一层。
    pub fn base(&self) -> &ShapePatch {
        &self.base
    }

    /// 写过的状态层，按状态名。
    pub fn states(&self) -> &[(String, ShapePatch)] {
        &self.states
    }

    /// 一层都没写？
    pub fn is_empty(&self) -> bool {
        self.base.is_empty() && self.states.iter().all(|(_, p)| p.is_empty())
    }

    /// 这一区域写过 `font` 吗（任何一层）？
    ///
    /// 字体 token 还没接线，`skin.rs` 拿它决定要不要补一句警告 —— 数值键写了
    /// 看得见、`font` 写了看不见，这个差别必须说出来。
    pub fn touches_font(&self) -> bool {
        self.base.font.is_some() || self.states.iter().any(|(_, p)| p.font.is_some())
    }

    /// 这个区域写过哪些形状键（任何一层），按 [`ShapeKey::ALL`] 的顺序。
    ///
    /// 给「这个区域不看某个键」的警告用：写了 `radius` 但那一格不填底，得说
    /// 一声，不然作者会以为是自己写错了。
    pub fn written_keys(&self) -> Vec<ShapeKey> {
        let mut out = Vec::new();
        for key in ShapeKey::ALL {
            let hit = match key {
                ShapeKey::Font => self.touches_font(),
                _ => {
                    self.base.number_written(key)
                        || self.states.iter().any(|(_, p)| p.number_written(key))
                }
            };
            if hit {
                out.push(key);
            }
        }
        out
    }

    /// 取某一层来写，没有就建一层。
    fn layer(&mut self, state: Option<&str>) -> &mut ShapePatch {
        match state {
            None => &mut self.base,
            Some(name) => {
                if let Some(i) = self.states.iter().position(|(n, _)| n == name) {
                    return &mut self.states[i].1;
                }
                self.states.push((name.to_string(), ShapePatch::default()));
                let last = self.states.len() - 1;
                &mut self.states[last].1
            }
        }
    }

    /// 这组状态下的形状：常态打底，再按 [`State::NAMES`] 的顺序叠命中的状态层。
    pub fn resolve(&self, state: State) -> Shape {
        let mut shape = Shape::default();
        self.base.apply_to(&mut shape);
        for (bit, name) in State::ALL.into_iter().zip(State::NAMES) {
            if !state.contains(bit) {
                continue;
            }
            if let Some((_, patch)) = self.states.iter().find(|(n, _)| n == name) {
                patch.apply_to(&mut shape);
            }
        }
        shape
    }
}

/// 已经读配方的区域，以及每个区域**认哪几个形状键**。
///
/// 列表之外的区域只解析、不生效；列表里但键不在 `keys` 里的，写了也看不见。
/// 两种情况加载时都补警告 —— 「写了没反应」比「写了报错」难查得多。
///
/// 为什么一个区域会不认某个键：不是懒，是那个键在那个区域上**没有意义**。
/// 状态栏的每一格内置画法从不填自己那一块（只填整条带），所以 `radius` 没有
/// 东西可磨；文字是垂直居中的，对称的 `pad_y` 内缩等于什么都没做。与其画一个
/// 看不出差别的东西，不如直接说「这一格不看这个键」。
pub const WIRED_REGIONS: [WiredRegion; 3] = [
    WiredRegion {
        name: "tab",
        keys: &[
            ShapeKey::Radius,
            ShapeKey::Gap,
            ShapeKey::Border,
            ShapeKey::PadY,
        ],
    },
    WiredRegion {
        name: "dock.tab",
        keys: &[
            ShapeKey::Radius,
            ShapeKey::Gap,
            ShapeKey::Border,
            ShapeKey::PadY,
        ],
    },
    // 状态栏只认两个：这一格不填底（`radius` 没东西可磨），文字垂直居中
    // （对称的 `pad_y` 是 no-op）。`gap` 是文字左边距，`border` 是格间竖线宽度。
    WiredRegion {
        name: "status.pane",
        keys: &[ShapeKey::Gap, ShapeKey::Border],
    },
];

/// 一个已经接线的区域：名字 + 它认的形状键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WiredRegion {
    /// 区域名（`tab`、`status.pane` …）。
    pub name: &'static str,
    /// 这个区域真的会读的键，顺序即主题体检的顺序。
    pub keys: &'static [ShapeKey],
}

/// 这个区域接线了没？接了线就回它认的键。
pub fn wired_keys(region: &str) -> Option<&'static [ShapeKey]> {
    WIRED_REGIONS
        .iter()
        .find(|w| w.name == region)
        .map(|w| w.keys)
}

/// 文件里写了形状、但那个区域（连同它的族）**一个都没接绘制**的那些区域名。
///
/// `Recipe::from_theme_file` 只报「区域名不存在」「键不认识」—— 写了一个真区域
/// 但那个区域还没人画，它照样收下，屏幕上看不出任何差别。这是「写了没反应」里
/// 最难查的一种：名字对、键也对，就是没人读。诊断（`--ui-doctor`）要能说出这
/// 句话，而「哪些区域接了线」只有这张表知道。
///
/// 去重、按第一次出现的顺序；区域族只要**有一个**成员接了线就不算死
/// （`status.gap` 落到 `status.pane`，那一条是活的）。
pub fn unwired_regions(file: &ThemeFile) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for (key, value) in &file.shapes {
        let Ok((region, _, _, _)) = shape_entry(key, value) else {
            continue;
        };
        let expanded = regions_for(region);
        if expanded.is_empty() {
            continue;
        }
        if expanded.iter().any(|r| wired_keys(r).is_some()) {
            continue;
        }
        if !out.iter().any(|r| *r == region) {
            out.push(region);
        }
    }
    out
}

/// 整份配方：区域名 → 形状。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recipe {
    regions: Vec<(String, RegionRecipe)>,
}

impl Recipe {
    /// 空配方：所有区域都回出厂形状。
    ///
    /// `const` 是为了 `static RECIPE: RwLock<Recipe> = RwLock::new(Recipe::new())`
    /// —— `Default::default()` 不是 `const`，而绘制路径上的静态量不能等到运行时初始化。
    pub const fn new() -> Recipe {
        Recipe {
            regions: Vec::new(),
        }
    }

    /// 一条都没写？
    pub fn is_empty(&self) -> bool {
        self.regions.iter().all(|(_, r)| r.is_empty())
    }

    /// 写过几个区域。
    pub fn len(&self) -> usize {
        self.regions.iter().filter(|(_, r)| !r.is_empty()).count()
    }

    /// 某个区域（可能还没写过）。
    pub fn region(&self, region: &str) -> Option<&RegionRecipe> {
        self.regions
            .iter()
            .find(|(n, _)| n == region)
            .map(|(_, r)| r)
    }

    /// 这组状态下的形状。没写过的区域回出厂形状 —— 绘制代码不必判空。
    pub fn resolve(&self, region: &str, state: State) -> Shape {
        self.region(region)
            .map(|r| r.resolve(state))
            .unwrap_or_default()
    }

    /// 写一条。`state` 是 `states.<name>` 里那个名字，`None` 表示常态层。
    ///
    /// 状态名要在 [`State::NAMES`] 里 —— 写错了当场报错并列出全部，别让
    /// 「写了没反应」这种事发生。
    pub fn set(
        &mut self,
        region: &str,
        state: Option<&str>,
        key: ShapeKey,
        value: ShapeValue<'_>,
    ) -> Result<(), ShapeError> {
        if let Some(name) = state {
            if !State::NAMES.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                return Err(ShapeError::UnknownState {
                    name: name.to_string(),
                });
            }
        }
        // 状态名统一成小写：`states.Hot` 和 `states.hot` 是同一层，不能各存一份。
        let state = state.map(|n| n.to_ascii_lowercase());
        let patch = self.layer(region, state.as_deref());
        match value {
            ShapeValue::Number(v) => {
                if !patch.set_number(key, v) {
                    // 只有 `font` 是字符串键，走到这里说明给它写了数字。
                    return Err(ShapeError::NotText { key: key.name() });
                }
            }
            ShapeValue::Text(s) => {
                if key != ShapeKey::Font {
                    return Err(ShapeError::NotNumber { key: key.name() });
                }
                patch.set_font(s);
            }
        }
        Ok(())
    }

    fn layer(&mut self, region: &str, state: Option<&str>) -> &mut ShapePatch {
        if let Some(i) = self.regions.iter().position(|(n, _)| n == region) {
            return self.regions[i].1.layer(state);
        }
        self.regions
            .push((region.to_string(), RegionRecipe::default()));
        let last = self.regions.len() - 1;
        self.regions[last].1.layer(state)
    }

    /// 全部写过的区域，按写进来的先后。
    pub fn regions(&self) -> impl Iterator<Item = (&str, &RegionRecipe)> {
        self.regions
            .iter()
            .filter(|(_, r)| !r.is_empty())
            .map(|(n, r)| (n.as_str(), r))
    }

    /// 从主题文件的 `[Shape]` 段建一份配方。
    ///
    /// 键的写法是 `区域[.states.<状态>].<键>`，和分层的
    /// `[style.tab.states.active] radius` 落在**同一个格子**上 —— 换的是文件
    /// 格式，不是语义。所以分段规则必须和那边一致：
    ///
    /// - `states` 是**中缀**，不占名字的一层：`tab.states.active.radius` 是
    ///   `(tab, active, radius)`；
    /// - 区域名自己可以带点：`dock.tab.gap` 的区域是 `dock.tab`，不是 `dock`；
    /// - 区域族（`status` / `scroll` / `menu`）展开成各自的区域，因为作者在
    ///   `[style.status] bg` 上已经用过这三个词。
    ///
    /// 回一份 `(配方, 问题清单)`，不在这里失败：主题文件是**共用**的手写文件，
    /// 一条写错不该让整套外观消失。问题清单交给调用方去打日志 —— 静默是唯一
    /// 不能接受的处置方式。
    pub fn from_theme_file(file: &ThemeFile) -> (Recipe, Vec<String>) {
        let mut recipe = Recipe::default();
        let mut problems = Vec::new();
        for (key, value) in &file.shapes {
            match shape_entry(key, value) {
                Ok((region, state, shape, value)) => {
                    let regions = regions_for(region);
                    if regions.is_empty() {
                        problems.push(format!(
                            "[Shape] {key}：{region:?} 不是区域；能写的有 {}",
                            region_names()
                        ));
                        continue;
                    }
                    for r in regions {
                        if let Err(e) = recipe.set(r, state, shape, value) {
                            problems.push(format!("[Shape] {key}：{e}"));
                        }
                    }
                }
                Err(why) => problems.push(format!("[Shape] {key}：{why}")),
            }
        }
        (recipe, problems)
    }
}

/// 拆一条 `[Shape]` 键，并把值按叶子类型收好。
///
/// 拆不开、叶子不认识、值类型不对 —— 三种都在这里变成一句人话，因为只有这一层
/// 同时知道「键写成什么样」和「值该是什么」。
fn shape_entry<'a>(
    key: &'a str,
    value: &'a str,
) -> Result<(&'a str, Option<&'a str>, ShapeKey, ShapeValue<'a>), String> {
    let Some((head, leaf)) = key.rsplit_once('.') else {
        return Err("要写成「区域.键」，比如 tab.radius".to_string());
    };
    let Some(shape) = ShapeKey::parse(leaf) else {
        // 度量写到这里是**最常见**的错法（`tab.pad_x` 在 `[Metrics]` 里），
        // 所以先认它再说「不认识这个键」。度量名是**带区域的**全名
        // （`tab.pad_x`），所以整条键和光秃秃的叶子都要试一遍。
        let metric = crate::metrics::MetricId::parse(key).is_ok()
            || crate::metrics::MetricId::parse(leaf).is_ok();
        if metric {
            return Err(format!(
                "{leaf:?} 是 [Metrics] 里的度量，不要写在 [Shape] 里 —— 两个来源迟早会打架"
            ));
        }
        return Err(format!(
            "{leaf:?} 不是形状键；能写的有 {}",
            ShapeKey::ALL
                .iter()
                .map(|k| k.name())
                .collect::<Vec<_>>()
                .join(" ")
        ));
    };
    // `states` 是中缀：取**最后**一个，所以 `dock.tab.states.active` 的区域是
    // `dock.tab` 而不是 `dock`。
    let (region, state) = match head.rsplit_once(".states.") {
        Some((region, state)) => (region, Some(state)),
        None => (head, None),
    };
    let value = if shape == ShapeKey::Font {
        ShapeValue::Text(value)
    } else {
        match value.trim().parse::<f32>() {
            Ok(v) if v.is_finite() => ShapeValue::Number(v),
            _ => {
                return Err(format!(
                    "{leaf:?} 的值要写数字，你写的是 {value:?}"
                ));
            }
        }
    };
    Ok((region, state, shape, value))
}

/// 一条形状键的值。
#[derive(Clone, Copy, Debug)]
pub enum ShapeValue<'a> {
    /// 数值（`radius` / `gap` / `border` / `pad_y`）。
    Number(f32),
    /// 字符串（只有 `font`）。
    Text(&'a str),
}

/// 形状键写错时的说法。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShapeError {
    /// `states.<name>` 里的名字不认识。
    UnknownState { name: String },
    /// 这个区域名不是区域。
    UnknownRegion { name: String },
    /// 数值键写成了字符串。
    NotNumber { key: &'static str },
    /// `font` 写成了数字。
    NotText { key: &'static str },
    /// 把 `[metrics]` 已经有的东西又写了一遍。
    BelongsToMetrics { key: &'static str },
}

impl fmt::Display for ShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShapeError::UnknownState { name } => write!(
                f,
                "状态名 {name:?} 不认识；能写的有 {}",
                State::NAMES.join(" ")
            ),
            ShapeError::UnknownRegion { name } => write!(
                f,
                "形状键要写在区域上，{name:?} 不是区域；能写的有 {}",
                region_names()
            ),
            ShapeError::NotNumber { key } => {
                write!(f, "形状键 {key:?} 的值必须是数字")
            }
            ShapeError::NotText { key } => {
                write!(f, "形状键 {key:?} 的值必须是字符串（只有 font 是字符串）")
            }
            ShapeError::BelongsToMetrics { key } => write!(
                f,
                "{key:?} 是 [metrics] 里的度量，不要写在 [style] 里 —— 两个来源迟早会打架"
            ),
        }
    }
}

impl std::error::Error for ShapeError {}

/// 能写形状键的区域名，空格隔开，给报错用。
///
/// 公开是因为运行时那条路（`App.ui.style:shape`）要把同一句话原样说出来 ——
/// 同一个错误在两处入口长出两种说法，是最难查的那种不一致。
pub fn region_names() -> String {
    Region::ALL
        .iter()
        .map(|r| r.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 颜色那一套里有三个**区域族**（`status` / `scroll` / `menu`），形状这边
/// 跟着认，展开成它们各自的区域 —— 作者已经在 `[style.status] bg` 上用过
/// 这三个词，在这里忽然不认会很别扭。
pub fn family_regions(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "status" => Some(&["status.pane", "status.grip"]),
        "scroll" => Some(&["scroll.v", "scroll.h"]),
        "menu" => Some(&["menu.row", "menu.separator"]),
        _ => None,
    }
}

/// 一个路径前缀能落到哪些区域。认不出来回空。
pub fn regions_for(name: &str) -> Vec<&'static str> {
    if let Some(family) = family_regions(name) {
        return family.to_vec();
    }
    Region::ALL
        .iter()
        .find(|r| r.as_str().eq_ignore_ascii_case(name))
        .map(|r| vec![r.as_str()])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(radius: f32) -> ShapeValue<'static> {
        let _ = radius;
        ShapeValue::Number(radius)
    }

    /// 名字表和 `ALL` 一一对应，改一个忘了另一个会被这条抓住。
    #[test]
    fn the_five_names_are_the_contract() {
        assert_eq!(ShapeKey::ALL.len(), ShapeKey::NAMES.len());
        for (key, name) in ShapeKey::ALL.into_iter().zip(ShapeKey::NAMES) {
            assert_eq!(key.name(), name);
            assert_eq!(ShapeKey::parse(name), Some(key));
            // 大小写不敏感，和样式键一致。
            assert_eq!(ShapeKey::parse(&name.to_uppercase()), Some(key));
        }
        assert_eq!(ShapeKey::parse("radios"), None);
    }

    /// 只有 `font` 不是数值键。
    #[test]
    fn font_is_the_only_text_key() {
        for key in ShapeKey::ALL {
            assert_eq!(
                key.range().is_none(),
                key == ShapeKey::Font,
                "{} 的 range",
                key.name()
            );
        }
    }

    /// 空皮肤 = 出厂形状：全 0、不换字体。
    #[test]
    fn an_empty_recipe_resolves_to_the_factory_shape() {
        let r = Recipe::default();
        assert!(r.is_empty());
        let s = r.resolve("tab", State::HOT);
        assert!(s.is_default());
        assert_eq!(s.radius, 0.0);
        assert_eq!(s.font, "");
    }

    /// 常态层生效，状态层叠在上面。
    #[test]
    fn a_state_layer_overrides_the_base() {
        let mut r = Recipe::default();
        r.set("tab", None, ShapeKey::Radius, shape(4.0)).unwrap();
        r.set("tab", Some("active"), ShapeKey::Radius, shape(0.0))
            .unwrap();
        assert_eq!(r.resolve("tab", State::normal()).radius, 4.0);
        assert_eq!(r.resolve("tab", State::ACTIVE).radius, 0.0);
        // 不相干的状态不受影响。
        assert_eq!(r.resolve("tab", State::HOT).radius, 4.0);
        // 别的区域不受影响。
        assert_eq!(r.resolve("dock.tab", State::ACTIVE).radius, 0.0);
    }

    /// `radius = 0` 必须能把常态层的圆角取消掉 —— 这正是状态层存在的理由。
    #[test]
    fn an_explicit_zero_cancels_the_base() {
        let mut r = Recipe::default();
        r.set("tab", None, ShapeKey::Radius, shape(6.0)).unwrap();
        r.set("tab", Some("hot"), ShapeKey::Radius, shape(0.0))
            .unwrap();
        assert_eq!(r.resolve("tab", State::HOT).radius, 0.0);
        assert_eq!(r.resolve("tab", State::normal()).radius, 6.0);
    }

    /// 命中多个状态层时按 `State::NAMES` 的顺序叠，不看书写顺序。
    #[test]
    fn overlapping_states_resolve_in_name_order_not_write_order() {
        let mut a = Recipe::default();
        a.set("tab", Some("alt"), ShapeKey::Gap, shape(8.0)).unwrap();
        a.set("tab", Some("hot"), ShapeKey::Gap, shape(2.0)).unwrap();

        let mut b = Recipe::default();
        b.set("tab", Some("hot"), ShapeKey::Gap, shape(2.0)).unwrap();
        b.set("tab", Some("alt"), ShapeKey::Gap, shape(8.0)).unwrap();

        let both = State::HOT.with(State::ALT);
        // `hot` 在名字表里排在 `alt` 前面，所以 `alt` 后写后赢 —— 两种书写顺序
        // 给出同一个答案。
        assert_eq!(a.resolve("tab", both).gap, 8.0);
        assert_eq!(b.resolve("tab", both).gap, 8.0);
    }

    /// 越界夹住，不报错。
    #[test]
    fn out_of_range_values_are_clamped() {
        let mut r = Recipe::default();
        r.set("tab", None, ShapeKey::Radius, shape(99.0)).unwrap();
        r.set("tab", None, ShapeKey::Gap, shape(-5.0)).unwrap();
        let s = r.resolve("tab", State::normal());
        assert_eq!(s.radius, 24.0);
        assert_eq!(s.gap, 0.0);
    }

    /// 状态名写错当场报错，并列出全部合法名字。
    #[test]
    fn an_unknown_state_names_the_valid_ones() {
        let mut r = Recipe::default();
        let err = r
            .set("tab", Some("hover"), ShapeKey::Radius, shape(2.0))
            .unwrap_err();
        assert_eq!(
            err,
            ShapeError::UnknownState {
                name: "hover".into()
            }
        );
        let text = err.to_string();
        assert!(text.contains("hot"), "{text}");
        assert!(text.contains("modified"), "{text}");
    }

    /// 状态名大小写不敏感，`Hot` 和 `hot` 是同一层。
    #[test]
    fn a_state_name_is_case_insensitive() {
        let mut r = Recipe::default();
        r.set("tab", Some("Hot"), ShapeKey::Radius, shape(3.0))
            .unwrap();
        assert_eq!(r.resolve("tab", State::HOT).radius, 3.0);
        assert_eq!(r.region("tab").unwrap().states().len(), 1);
    }

    /// `font` 只收字符串，数值键只收数字。
    #[test]
    fn the_value_type_has_to_match_the_key() {
        let mut r = Recipe::default();
        assert_eq!(
            r.set("tab", None, ShapeKey::Radius, ShapeValue::Text("big"))
                .unwrap_err(),
            ShapeError::NotNumber { key: "radius" }
        );
        assert_eq!(
            r.set("tab", None, ShapeKey::Font, shape(12.0)).unwrap_err(),
            ShapeError::NotText { key: "font" }
        );
        r.set("tab", None, ShapeKey::Font, ShapeValue::Text("font.ui"))
            .unwrap();
        assert_eq!(r.resolve("tab", State::normal()).font, "font.ui");
    }

    /// 空串字体等于没写 —— 「用区域自己的字体」和「没指定」是一回事。
    #[test]
    fn an_empty_font_means_unset() {
        let mut r = Recipe::default();
        r.set("tab", None, ShapeKey::Font, ShapeValue::Text("  "))
            .unwrap();
        assert!(r.is_empty());
        assert_eq!(r.resolve("tab", State::normal()).font, "");
    }

    /// 三个区域族展开成各自的区域，其余按区域名认。
    #[test]
    fn the_three_families_expand_to_their_regions() {
        assert_eq!(regions_for("status"), vec!["status.pane", "status.grip"]);
        assert_eq!(regions_for("scroll"), vec!["scroll.v", "scroll.h"]);
        assert_eq!(regions_for("menu"), vec!["menu.row", "menu.separator"]);
        assert_eq!(regions_for("tab"), vec!["tab"]);
        assert_eq!(regions_for("dock.tab"), vec!["dock.tab"]);
        // 大小写不敏感。
        assert_eq!(regions_for("TAB"), vec!["tab"]);
        assert!(regions_for("nonsense").is_empty());
    }

    /// 只写了状态层、没写常态层也算写了（`is_empty` 要看两层）。
    #[test]
    fn a_state_only_recipe_is_not_empty() {
        let mut r = Recipe::default();
        r.set("tab", Some("hot"), ShapeKey::Border, shape(1.0))
            .unwrap();
        assert!(!r.is_empty());
        assert_eq!(r.len(), 1);
        assert_eq!(r.resolve("tab", State::HOT).border, 1.0);
        assert_eq!(r.resolve("tab", State::normal()).border, 0.0);
    }

    /// 接线表里的每个名字都得是真的区域、每个区域都得认至少一个键 ——
    /// 表写错了（比如把 `dock.tabs` 拼成 `docktab`）就是「写了永远看不见」。
    #[test]
    fn the_wiring_table_names_real_regions_and_real_keys() {
        for w in WIRED_REGIONS {
            assert!(
                crate::region::Region::parse(w.name).is_ok(),
                "{} 不是区域名",
                w.name
            );
            assert!(!w.keys.is_empty(), "{} 一个键都不认", w.name);
        }
        assert_eq!(wired_keys("tab").map(<[ShapeKey]>::len), Some(4));
        assert_eq!(wired_keys("dock.tab").map(<[ShapeKey]>::len), Some(4));
        assert!(wired_keys("toolbar.button").is_none());
    }

    /// `written_keys` 看的是**所有层**，而且按 `ShapeKey::ALL` 的顺序回 ——
    /// 警告里的键序不该随作者的书写顺序变。
    #[test]
    fn written_keys_covers_every_layer_in_a_fixed_order() {
        let mut r = Recipe::default();
        r.set("tab", Some("hot"), ShapeKey::Gap, shape(2.0)).unwrap();
        r.set("tab", None, ShapeKey::Radius, shape(4.0)).unwrap();
        r.set("tab", Some("hot"), ShapeKey::Font, ShapeValue::Text("font.ui"))
            .unwrap();
        let written = r.region("tab").unwrap().written_keys();
        assert_eq!(
            written,
            vec![ShapeKey::Radius, ShapeKey::Gap, ShapeKey::Font],
            "半径、空隙、字体 —— 按 ALL 的顺序，不按书写顺序"
        );
    }

    // ── `[Shape]`（主题文件）────────────────────────────────────────────────

    /// 四行 `[Shape]` 各自落到该落的那一格：常态两格、`active` 两格。
    ///
    /// 这条是「键怎么拆」的验收线：`tab.states.active.radius` 落的是
    /// `(tab, active, radius)`。拆错一个位置，四行里会有两行静默失踪 ——
    /// 主题文件写错了不报错，只是没反应，所以只能靠这条钉住。
    #[test]
    fn ini_shape_keys_land_in_the_right_cells() {
        let file = dg_render::parse_theme_ini(
            "[Shape]
tab.radius=4
tab.gap=2
tab.states.active.radius=0
tab.states.active.font=font.ui
",
        );
        let (r, problems) = Recipe::from_theme_file(&file);
        assert!(problems.is_empty(), "{problems:?}");
        let tab = r.region("tab").expect("tab");
        assert_eq!(tab.base().number(ShapeKey::Radius), Some(4.0));
        assert_eq!(tab.base().number(ShapeKey::Gap), Some(2.0));
        assert_eq!(tab.states().len(), 1, "只有 active 一层");
        let (name, active) = &tab.states()[0];
        assert_eq!(name, "active");
        assert_eq!(active.number(ShapeKey::Radius), Some(0.0));
        assert!(tab.touches_font(), "font.ui 落在 active 那一层");
    }

    /// `states` 是中缀，区域名自己可以带点 —— 两条一起测，因为它们抢的是
    /// 同一个分隔符：`dock.tab.states.active.gap` 的区域必须是 `dock.tab`。
    #[test]
    fn states_is_an_infix_even_when_the_region_has_dots() {
        let file = dg_render::parse_theme_ini(
            "[Shape]
dock.tab.gap=2
dock.tab.states.active.gap=6
",
        );
        let (recipe, problems) = Recipe::from_theme_file(&file);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(recipe.resolve("dock.tab", State::normal()).gap, 2.0);
        assert_eq!(recipe.resolve("dock.tab", State::ACTIVE).gap, 6.0);
        // 不能顺手在 `dock` 上留一条 —— 那是拆错分隔符的症状。
        assert!(recipe.region("dock").is_none(), "拆出了不存在的区域 dock");
    }

    /// 区域族照 `[style.status]` 的老规矩展开，不另立一套。
    #[test]
    fn a_family_name_expands_like_it_did_in_style() {
        let file = dg_render::parse_theme_ini("[Shape]\nstatus.gap=1\n");
        let (recipe, problems) = Recipe::from_theme_file(&file);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(recipe.resolve("status.pane", State::normal()).gap, 1.0);
        assert_eq!(recipe.resolve("status.grip", State::normal()).gap, 1.0);
    }

    /// 一条写错不牵连其他条：问题进清单，好的照旧生效。
    #[test]
    fn a_bad_entry_is_reported_and_the_rest_still_applies() {
        let file = dg_render::parse_theme_ini(
            "[Shape]
tab.radius=4
tab.gap=big
nope.radius=2
tab.pad_x=9
tab.states.sideways.radius=1
tab.nosuchkey=1
",
        );
        let (recipe, problems) = Recipe::from_theme_file(&file);
        assert_eq!(recipe.resolve("tab", State::normal()).radius, 4.0, "好的那条要生效");
        assert_eq!(problems.len(), 5, "{problems:?}");
        // 每条都说清是哪个键，并且把「该写哪」指出来 —— 这是它存在的理由。
        let text = problems.join("\n");
        assert!(text.contains("tab.gap") && text.contains("数字"), "{text}");
        assert!(text.contains("nope.radius") && text.contains("不是区域"), "{text}");
        assert!(
            text.contains("tab.pad_x") && text.contains("[Metrics]"),
            "度量写进 [Shape] 要指回 [Metrics]：{text}"
        );
        assert!(text.contains("sideways"), "{text}");
        assert!(text.contains("tab.nosuchkey"), "{text}");
    }

    /// 没有 `[Shape]` 段时是空配方，绘制代码读到的就是出厂形状。
    #[test]
    fn a_theme_without_shape_says_nothing() {
        let (recipe, problems) = Recipe::from_theme_file(&dg_render::parse_theme_ini(
            "[Theme]\nCaret=#FF0000\n",
        ));
        assert!(problems.is_empty());
        assert!(recipe.is_empty());
        assert_eq!(recipe.resolve("tab", State::HOT), Shape::default());
    }

    /// 写了真区域、但那个区域还没接绘制 —— 诊断要能点名，`from_theme_file`
    /// 不会（名字对、键也对，它没有理由拒绝）。
    #[test]
    fn unwired_regions_names_what_nothing_reads() {
        let file = dg_render::parse_theme_ini(
            "[Shape]
editor.gap=2
tab.radius=4
menu.row.radius=3
status.gap=1
editor.gap=2
",
        );
        // `editor.gap` 写了两次（同一个区域），点名只报一次；`tab` / `status`
        // 都接了线，不报。
        assert_eq!(unwired_regions(&file), vec!["editor", "menu.row"]);
        assert!(Recipe::from_theme_file(&file).1.is_empty(), "这三个区域名都合法");
    }

    /// 区域族只要有一个成员接线就不算死：`status.gap` 落到 `status.pane`。
    #[test]
    fn a_family_with_one_wired_member_is_alive() {
        let file = dg_render::parse_theme_ini("[Shape]\nstatus.gap=1\n");
        assert!(unwired_regions(&file).is_empty());
    }

    /// 写错的键不进这份清单：那是 `from_theme_file` 的活，别两边各报一遍。
    #[test]
    fn a_bad_shape_key_is_not_called_unwired() {
        let file = dg_render::parse_theme_ini("[Shape]\ntabb.radius=1\ntab.nosuch=2\n");
        assert!(unwired_regions(&file).is_empty());
    }
}
