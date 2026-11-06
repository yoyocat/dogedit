//! 度量：把散在绘制代码里的常量变成一张具名表。
//!
//! 每个度量都有**默认值**和**夹取规则**。皮肤写 `status.height = 500` 不报错，
//! 夹到 48；绘制代码永远不需要为越界值写防御分支 —— 越界在 `set` 那一步就被
//! 夹住了。
//!
//! 三种夹取形状：
//!
//! - `Range(lo, hi)`：普通区间。
//! - `ZeroOr(lo, hi)`：`0` 是一个有意义的特殊值（「用字体度量」/「用图标格」），
//!   其余夹到区间。页签高度、状态栏高度、菜单行高都是这一类：默认 0 意味着
//!   「跟字体走」，所以默认外观逐像素等于没这张表的时候。
//! - `OneOf(&[..])`：只允许几个离散值（工具栏图标只做 16/24/32，非整格的位图
//!   在 125% DPI 下会糊）。
//!
//! 度量值一般是 **DIP**（96 DPI 的逻辑像素），`scaled(px, dpi)` 那一步仍在绘制
//! 代码里做，和今天一样。少数几项是**设备像素**（页签的内边距与关闭钮格宽），
//! 因为那个绘制器本来就不缩放它们 —— 标在各自的注释里，接线的代码照做。
//!
//! **这里只放 `Theme` 没有的几何。** 滚动条的粗细、不透明度、淡出时长本来就在
//! `Theme` 上（`scrollbar_thumb_width` 等），皮肤从 `[style]` 里按数字写，不在这
//! 张表里再来一份 —— 两个来源迟早会打架。

use std::fmt;

use dg_render::theme::ThemeFile;

/// 一个具名度量。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum MetricId {
    /// 状态栏高度；0 表示用字体度量。默认 0。
    StatusHeight,
    /// 状态格文字左边距。默认 2。
    StatusPad,
    /// 状态格图标预留宽度。默认 36。
    StatusIconPad,
    /// 状态格最小宽度。默认 45。
    StatusMinWidth,
    /// 水平分隔带（上下分栏）的厚度。默认 5。
    SplitBandH,
    /// 垂直分隔带（左右分栏）的厚度。默认 6。
    SplitBandV,
    /// 页签高度；0 表示用字体度量。默认 0。DIP，随 DPI 放大。
    TabHeight,
    /// 页签左右内边距。默认 4。**设备像素，不随 DPI 放大** —— 页签条里的
    /// `inset`/`extra` 本来就是字面量，只有图标格跟着 DPI 走。
    TabPadX,
    /// 页签关闭钮的格宽；0 表示用图标格。默认 0。设备像素，同上。
    TabClose,
    /// 停靠页签高度；0 表示用字体度量。默认 0。
    DockTabHeight,
    /// 菜单行高；0 表示用字体度量。默认 0。
    MenuRowHeight,
    /// 菜单左右内边距。默认 8；加载期会用字体度量覆盖。
    MenuPad,
    /// 工具栏图标格边长。默认 16。
    ToolbarIcon,
}

/// 度量总数，也是 [`Metrics`] 数组的长度。
pub const METRIC_COUNT: usize = 13;

/// 夹取规则。
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Clamp {
    /// 普通区间，闭区间。
    Range(f32, f32),
    /// `0` 单独合法（表示「跟字体/图标格走」），其余夹到区间。
    ZeroOr(f32, f32),
    /// 只允许这几个离散值，取最接近的那个。
    OneOf(&'static [f32]),
}

impl Clamp {
    /// 把 `value` 夹进规则。`NaN` 一律落到下限（或 `OneOf` 的第一项）。
    pub fn apply(self, value: f32) -> f32 {
        if value.is_nan() {
            return match self {
                Clamp::OneOf(allowed) => allowed[0],
                Clamp::Range(lo, _) | Clamp::ZeroOr(lo, _) => lo,
            };
        }
        match self {
            Clamp::Range(lo, hi) => value.clamp(lo, hi),
            Clamp::ZeroOr(lo, hi) => {
                if value == 0.0 {
                    0.0
                } else {
                    value.clamp(lo, hi)
                }
            }
            Clamp::OneOf(allowed) => {
                let mut best = allowed[0];
                for candidate in allowed {
                    if (value - candidate).abs() < (value - best).abs() {
                        best = *candidate;
                    }
                }
                best
            }
        }
    }
}

impl MetricId {
    /// 全部度量，顺序即名字表顺序，也是主题体检的顺序。
    pub const ALL: [MetricId; METRIC_COUNT] = [
        MetricId::StatusHeight,
        MetricId::StatusPad,
        MetricId::StatusIconPad,
        MetricId::StatusMinWidth,
        MetricId::SplitBandH,
        MetricId::SplitBandV,
        MetricId::TabHeight,
        MetricId::TabPadX,
        MetricId::TabClose,
        MetricId::DockTabHeight,
        MetricId::MenuRowHeight,
        MetricId::MenuPad,
        MetricId::ToolbarIcon,
    ];

    /// 词表名字。
    pub const fn as_str(self) -> &'static str {
        match self {
            MetricId::StatusHeight => "status.height",
            MetricId::StatusPad => "status.pad",
            MetricId::StatusIconPad => "status.icon_pad",
            MetricId::StatusMinWidth => "status.min_w",
            MetricId::SplitBandH => "split.band_h",
            MetricId::SplitBandV => "split.band_v",
            MetricId::TabHeight => "tab.height",
            MetricId::TabPadX => "tab.pad_x",
            MetricId::TabClose => "tab.close",
            MetricId::DockTabHeight => "dock.tab.height",
            MetricId::MenuRowHeight => "menu.row_h",
            MetricId::MenuPad => "menu.pad",
            MetricId::ToolbarIcon => "toolbar.icon",
        }
    }

    /// 出厂值。**必须等于绘制代码里原来那个常量** —— 否则「空皮肤逐像素一致」
    /// 这条承诺一上来就破了。每一项都在下面的测试里对着源码常量钉住。
    pub const fn default_value(self) -> f32 {
        match self {
            MetricId::StatusHeight => 0.0,
            MetricId::StatusPad => 2.0,
            MetricId::StatusIconPad => 36.0,
            MetricId::StatusMinWidth => 45.0,
            MetricId::SplitBandH => 5.0,
            MetricId::SplitBandV => 6.0,
            MetricId::TabHeight => 0.0,
            MetricId::TabPadX => 4.0,
            MetricId::TabClose => 0.0,
            MetricId::DockTabHeight => 0.0,
            MetricId::MenuRowHeight => 0.0,
            MetricId::MenuPad => 8.0,
            MetricId::ToolbarIcon => 16.0,
        }
    }

    /// 夹取规则。
    pub const fn clamp(self) -> Clamp {
        match self {
            MetricId::StatusHeight => Clamp::ZeroOr(16.0, 48.0),
            MetricId::StatusPad => Clamp::Range(0.0, 24.0),
            MetricId::StatusIconPad => Clamp::Range(0.0, 64.0),
            MetricId::StatusMinWidth => Clamp::Range(20.0, 200.0),
            MetricId::SplitBandH | MetricId::SplitBandV => Clamp::Range(2.0, 16.0),
            MetricId::TabHeight | MetricId::DockTabHeight => Clamp::ZeroOr(16.0, 64.0),
            MetricId::TabPadX => Clamp::Range(0.0, 32.0),
            MetricId::TabClose => Clamp::ZeroOr(8.0, 24.0),
            MetricId::MenuRowHeight => Clamp::ZeroOr(16.0, 48.0),
            MetricId::MenuPad => Clamp::Range(0.0, 32.0),
            MetricId::ToolbarIcon => Clamp::OneOf(&[16.0, 24.0, 32.0]),
        }
    }

    /// 从词表名字解析。
    pub fn parse(name: &str) -> Result<MetricId, MetricParseError> {
        let want = name.trim();
        for id in MetricId::ALL {
            if id.as_str().eq_ignore_ascii_case(want) {
                return Ok(id);
            }
        }
        Err(MetricParseError {
            name: want.to_string(),
        })
    }
}

impl fmt::Display for MetricId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一张度量表。数组下标就是 [`MetricId`] 的序号。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    values: [f32; METRIC_COUNT],
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// 全部出厂值。
    pub const fn new() -> Self {
        let mut values = [0.0f32; METRIC_COUNT];
        let mut i = 0;
        while i < METRIC_COUNT {
            values[i] = MetricId::ALL[i].default_value();
            i += 1;
        }
        Self { values }
    }

    pub fn get(&self, id: MetricId) -> f32 {
        self.values[id as usize]
    }

    /// 写入并夹取，返回真正存进去的值。越界不报错，只夹。
    pub fn set(&mut self, id: MetricId, value: f32) -> f32 {
        let stored = id.clamp().apply(value);
        self.values[id as usize] = stored;
        stored
    }

    /// 按名字写入，给 `[metrics]` 段的解析用。
    pub fn set_by_name(&mut self, name: &str, value: f32) -> Result<f32, MetricParseError> {
        let id = MetricId::parse(name)?;
        Ok(self.set(id, value))
    }

    /// 把主题的 `[Metrics]` 段叠到这张表上 —— 主题是**底**。
    ///
    /// 顺序是「出厂值 → 主题 → 皮肤」：调用方先 `apply_theme`，再让
    /// 后面可以被运行时覆盖（`App.ui.style.metric`）。所以一份主题能改整个外壳的
    /// 几何（状态栏多高、页签多高），而一套皮肤仍然可以自带它必须的尺寸。
    ///
    /// 主题文件是跨版本共用的：随包发布、用户手改、旧程序也要能读。所以认不出的
    /// 名字和越界的值都**只记不报**，写进返回的 [`ThemeMetricReport`] 让诊断页
    /// 去说 —— 一个拼错的键不该让整份主题失效。
    ///
    /// 键名和皮肤 `[metrics]` 完全相同（[`MetricId::as_str`]），同一份键名表，
    /// 同一套夹取规则；两个文件里复制粘贴不用改名。
    pub fn apply_theme(&mut self, file: &ThemeFile) -> ThemeMetricReport {
        let mut report = ThemeMetricReport::default();
        // `ThemeFile::metrics` 是 `BTreeMap`，迭代本身就是按名字排好的。
        for (name, written) in &file.metrics {
            match MetricId::parse(name) {
                Ok(id) => {
                    let stored = self.set(id, *written);
                    if stored != *written {
                        report.clamped.push((name.clone(), *written, stored));
                    }
                }
                Err(_) => report.unknown.push(name.clone()),
            }
        }
        report
    }

    /// 恢复出厂值。
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// 有没有一项和出厂值不同（主题体检用）。
    pub fn is_default(&self) -> bool {
        MetricId::ALL
            .iter()
            .all(|id| self.get(*id) == id.default_value())
    }
}

/// 主题 `[Metrics]` 段里没被原样采纳的部分。
///
/// 两个列都不影响装载：主题里的坏键只忽略，不让整份主题失败。诊断页把这两列
/// 出来，用户才知道自己写的 `tab.hight` 为什么一点反应都没有。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeMetricReport {
    /// 认不出的度量名，按名字排序（来自 `BTreeMap` 的迭代顺序）。
    pub unknown: Vec<String>,
    /// 认得出但越界的键：`(名字, 写的值, 真正存进去的值)`。
    pub clamped: Vec<(String, f32, f32)>,
}

impl ThemeMetricReport {
    /// 一条问题都没有？
    pub fn is_empty(&self) -> bool {
        self.unknown.is_empty() && self.clamped.is_empty()
    }

    /// 一行摘要，给状态栏消息和 `--ui-doctor` 用；没问题时是空串。
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.unknown.is_empty() {
            parts.push(format!("未知度量 {}", self.unknown.join(" ")));
        }
        if !self.clamped.is_empty() {
            let list = self
                .clamped
                .iter()
                .map(|(name, written, stored)| format!("{name} {written}→{stored}"))
                .collect::<Vec<_>>()
                .join(" ");
            parts.push(format!("越界夹取 {list}"));
        }
        parts.join("；")
    }
}

/// 度量名不认识。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricParseError {
    /// 用户写的那个名字（已 trim）。
    pub name: String,
}

impl MetricParseError {
    /// 全部合法名字，按表顺序。给报错信息用。
    pub fn valid_names() -> String {
        MetricId::ALL
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl fmt::Display for MetricParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown metric {:?}", self.name)
    }
}

impl std::error::Error for MetricParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名字表钉死。
    #[test]
    fn the_names_are_the_contract() {
        let names: Vec<&str> = MetricId::ALL.iter().map(|m| m.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "status.height",
                "status.pad",
                "status.icon_pad",
                "status.min_w",
                "split.band_h",
                "split.band_v",
                "tab.height",
                "tab.pad_x",
                "tab.close",
                "dock.tab.height",
                "menu.row_h",
                "menu.pad",
                "toolbar.icon",
            ]
        );
        assert_eq!(MetricId::ALL.len(), METRIC_COUNT);
    }

    /// 每个名字都能解析回自己。
    #[test]
    fn every_name_round_trips() {
        for id in MetricId::ALL {
            assert_eq!(MetricId::parse(id.as_str()), Ok(id));
        }
        assert_eq!(
            MetricId::parse("  STATUS.HEIGHT "),
            Ok(MetricId::StatusHeight)
        );
        assert_eq!(
            MetricId::parse("status.heightt"),
            Err(MetricParseError {
                name: "status.heightt".into()
            })
        );
    }

    /// 出厂值必须落在自己的夹取区间里 —— 否则一上来就被夹，默认值等于假的。
    #[test]
    fn every_default_survives_its_own_clamp() {
        for id in MetricId::ALL {
            let value = id.default_value();
            assert_eq!(id.clamp().apply(value), value, "{id} default is out of range");
        }
    }

    /// 出厂值就是绘制代码里原来那些常量。这条是「空皮肤逐像素一致」的地基：
    /// 数字对不上，接完线第一张截图就会差。
    #[test]
    fn the_defaults_are_the_constants_they_replaced() {
        assert_eq!(MetricId::StatusPad.default_value(), 2.0, "statusbar TEXT_PAD");
        assert_eq!(MetricId::TabPadX.default_value(), 4.0, "tabstrip pad");
        // dg-ui 的 SPLITTER = 4：竖分隔条 SPLITTER + 2 = 6，横分隔条 SPLITTER + 1 = 5。
        assert_eq!(MetricId::SplitBandV.default_value(), 6.0, "dg-ui SPLIT_BAND");
        assert_eq!(MetricId::SplitBandH.default_value(), 5.0, "dg-ui SPLIT_BAND_H");
        // 「跟字体/图标格走」的那几项默认必须是 0。
        for id in [
            MetricId::StatusHeight,
            MetricId::TabHeight,
            MetricId::TabClose,
            MetricId::DockTabHeight,
            MetricId::MenuRowHeight,
        ] {
            assert_eq!(id.default_value(), 0.0, "{id} should follow the font");
        }
        assert_eq!(MetricId::ToolbarIcon.default_value(), 16.0);
    }

    /// 越界只夹不报错，夹完的值就是存进去的值。
    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        let mut m = Metrics::new();
        assert_eq!(m.set(MetricId::StatusHeight, 500.0), 48.0);
        assert_eq!(m.get(MetricId::StatusHeight), 48.0);
        assert_eq!(m.set(MetricId::StatusHeight, 1.0), 16.0);
        assert_eq!(m.set(MetricId::StatusPad, -5.0), 0.0);
    }

    /// `ZeroOr`：0 是合法的「跟字体走」，1 却要夹到下限。
    #[test]
    fn zero_means_follow_the_font() {
        let mut m = Metrics::new();
        assert_eq!(m.set(MetricId::TabHeight, 0.0), 0.0);
        assert_eq!(m.set(MetricId::TabHeight, 1.0), 16.0);
        assert_eq!(m.set(MetricId::TabHeight, 999.0), 64.0);
        assert_eq!(m.set(MetricId::MenuRowHeight, 0.0), 0.0);
        assert_eq!(m.set(MetricId::TabClose, 0.0), 0.0, "0 = use the icon cell");
        assert_eq!(m.set(MetricId::TabClose, 3.0), 8.0);
    }

    /// 工具栏图标只认整格：17 落到 16，21 落到 24，20 正好在中间取小的。
    #[test]
    fn the_toolbar_icon_snaps_to_a_whole_cell() {
        let mut m = Metrics::new();
        assert_eq!(m.set(MetricId::ToolbarIcon, 17.0), 16.0);
        assert_eq!(m.set(MetricId::ToolbarIcon, 20.0), 16.0, "a tie takes the lower cell");
        assert_eq!(m.set(MetricId::ToolbarIcon, 21.0), 24.0);
        assert_eq!(m.set(MetricId::ToolbarIcon, 31.0), 32.0);
        assert_eq!(m.set(MetricId::ToolbarIcon, 1000.0), 32.0);
    }

    /// `NaN` 不许漏进绘制代码。
    #[test]
    fn nan_lands_on_the_floor() {
        let mut m = Metrics::new();
        assert_eq!(m.set(MetricId::StatusPad, f32::NAN), 0.0);
        assert_eq!(m.set(MetricId::ToolbarIcon, f32::NAN), 16.0);
    }

    /// 按名字写入，解析失败时一个值都不改。
    #[test]
    fn set_by_name_parses_and_writes() {
        let mut m = Metrics::new();
        assert_eq!(m.set_by_name("status.pad", 12.0), Ok(12.0));
        assert_eq!(m.get(MetricId::StatusPad), 12.0);
        assert!(m.set_by_name("nope", 1.0).is_err());
        assert_eq!(m.get(MetricId::StatusPad), 12.0, "nothing was written");
    }

    /// `reset` 回出厂值。
    #[test]
    fn reset_restores_the_defaults() {
        let mut m = Metrics::new();
        m.set(MetricId::StatusHeight, 40.0);
        m.reset();
        assert_eq!(m.get(MetricId::StatusHeight), 0.0);
    }

    /// `is_default` 只在真的全是出厂值时为真。
    #[test]
    fn is_default_notices_a_single_change() {
        let mut m = Metrics::new();
        assert!(m.is_default());
        m.set(MetricId::TabHeight, 30.0);
        assert!(!m.is_default());
        m.reset();
        assert!(m.is_default());
    }

    /// 主题的 `[Metrics]` 用同一份键名表写进来，夹取规则也一样。
    #[test]
    fn theme_metrics_use_the_same_names_and_clamp() {
        let file = dg_render::theme::parse_theme_ini(
            "[Metrics]\nstatus.height=26\ntab.height=30\ntoolbar.icon=24\n",
        );
        let mut m = Metrics::new();
        let report = m.apply_theme(&file);
        assert!(report.is_empty(), "nothing to complain about: {report:?}");
        assert_eq!(m.get(MetricId::StatusHeight), 26.0);
        assert_eq!(m.get(MetricId::TabHeight), 30.0);
        assert_eq!(m.get(MetricId::ToolbarIcon), 24.0);
        // untouched keys stay at the factory value
        assert_eq!(m.get(MetricId::StatusPad), MetricId::StatusPad.default_value());
    }

    /// 越界只夹、不报错，但夹过什么要能说出来 —— 否则用户改了没反应，无从查。
    #[test]
    fn out_of_range_theme_metrics_are_clamped_and_reported() {
        let file = dg_render::theme::parse_theme_ini("[Metrics]\nstatus.height=500\n");
        let mut m = Metrics::new();
        let report = m.apply_theme(&file);
        assert_eq!(m.get(MetricId::StatusHeight), 48.0);
        assert_eq!(
            report.clamped,
            vec![("status.height".to_string(), 500.0, 48.0)]
        );
        assert!(report.unknown.is_empty());
        assert!(report.summary().contains("status.height"));
    }

    /// 认不出的名字只记下来，不影响同一段里认识的键。
    #[test]
    fn unknown_theme_metric_names_are_reported_not_fatal() {
        let file = dg_render::theme::parse_theme_ini(
            "[Metrics]\ntab.hight=30\nstatus.pad=12\nscroll.width=8\n",
        );
        let mut m = Metrics::new();
        let report = m.apply_theme(&file);
        assert_eq!(
            report.unknown,
            vec!["scroll.width".to_string(), "tab.hight".to_string()],
            "sorted, both named"
        );
        // The good key still landed.
        assert_eq!(m.get(MetricId::StatusPad), 12.0);
        assert!(report.summary().contains("tab.hight"));
    }

    /// 没有 `[Metrics]` 段的主题一个值都不改 —— 旧主题的像素基线靠这条。
    #[test]
    fn a_theme_without_metrics_leaves_the_table_alone() {
        let file = dg_render::theme::parse_theme_ini("[Theme]\nDefault=#1E1F22,#D4D7DD\n");
        let mut m = Metrics::new();
        let report = m.apply_theme(&file);
        assert!(report.is_empty());
        assert!(m.is_default());
    }
}
