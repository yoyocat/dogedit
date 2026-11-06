//! 外壳的词表：区域、状态、绘制原语、度量、形状、字体、缓存键。
//!
//! 这个 crate 只放**契约**，不放绘制实现：没有 GDI，没有 Lua，没有窗口。
//! 它读的那份文件是 `themes/<name>.ini` —— 外观只有这一个来源。解析在
//! `dg-render`，这里的 [`Recipe::from_theme_file`] / [`Fonts::from_theme_file`]
//! 只负责「把文本翻译成上面这套枚举」，因为翻译的规则和词表本身是同一件事。
//!
//! 它的存在意义是让三件互不相干的事情说同一套话 ——
//!
//! 1. 主题文件 / Lua 里写的 `region = "tab"`、`state.hot`、`fill(rect, "#3A3A3A")`
//!    要和 Rust 侧的枚举一一对应（[`Region`]、[`State`]、[`UiOp`]）；
//! 2. 内置画法改成吐 op 之后，回放层只需要认识这 12 条原语（[`UiOp`]）；
//! 3. 缓存键是一个 20 字节的 `Copy` 结构体（[`CacheKey`]），绘制路径上不分配、
//!    不哈希字符串。
//!
//! 词表是**对外契约**：一旦发布只能加，不能改语义。所以每个枚举都有一个
//! `as_str` / `from_str` 对，并且有测试把名字表钉死 —— 改一个名字就是改契约，
//! 测试会先炸。
//!
//! 度量（[`Metrics`]）全部带 clamp 区间：主题写 `status.height = 500` 不报错，
//! 夹到 48。绘制代码永远不需要为一个越界值写防御分支。

pub mod cache;
pub mod fonts;
pub mod hint;
pub mod metrics;
pub mod op;
pub mod recipe;
pub mod region;
pub mod state;
pub mod statusline;

pub use cache::CacheKey;
pub use fonts::{FONT_COUNT, FontFieldError, FontSpec, FontToken, Fonts, WIRED_FONTS};
pub use hint::{Cursor, CursorParseError, HintSpec};
pub use metrics::{MetricId, Metrics, ThemeMetricReport};
pub use op::{
    Align, Dir, Fit, FontId, ImageId, OpList, Point, PointsId, TextId, TextSource, UiOp, VAlign,
};
pub use region::{Region, RegionParseError};
pub use recipe::{
    Recipe, RegionRecipe, Shape, ShapeError, ShapeKey, ShapePatch, ShapeValue, WIRED_REGIONS,
    WiredRegion, region_names, regions_for, unwired_regions, wired_keys,
};
pub use state::{Primary, State, StateParseError};
pub use statusline::{expand, layout, Facts, PlaceholderError, Segment, Slot, Width};
