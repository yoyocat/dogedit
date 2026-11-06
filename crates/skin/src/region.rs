//! 区域：皮肤能接管的一块画布。
//!
//! 一个 region 就是一个**独立的绘制入口**，也是缓存与预算的计量单位：它的
//! `paint` 只拿到自己的矩形、状态和上下文，拿不到别的区域。所以「页签条变慢」
//! 不会拖累状态栏，超预算也只降级那一个区域。
//!
//! 名字是主题文件里键的前缀、Lua 里 `s:paint("tab", ...)` 的第一个参数，也是
//! 诊断输出打出来的那一列。加区域要同时加进 [`Region::ALL`] 与名字表。

use std::fmt;

/// 皮肤能接管的一块画布。
///
/// 顺序就是 [`Region::ALL`] 的顺序，也是名字表的顺序：改顺序会让诊断输出
/// 的列错位，别改。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Region {
    /// 窗口底色、面板之间的分隔带。
    Frame,
    /// 菜单栏 band 背景（高度归系统）。
    Menubar,
    /// 弹出菜单的一行，含勾选、图标、快捷键列。
    MenuRow,
    /// 菜单分隔线。
    MenuSeparator,
    /// 工具栏 band。
    Toolbar,
    /// 工具栏的一个按钮（normal/hot/pressed/checked/disabled）。
    ToolbarButton,
    /// 文档页签条 band。
    TabStrip,
    /// 一个文档页签，含图标、标题、修改标记、关闭钮。
    Tab,
    /// 页签条尾部的四格（◀ ▶ ▾ ✕）。
    TabGlyph,
    /// 停靠窗格的页签（caption）。
    DockTab,
    /// 状态栏 band。
    StatusBar,
    /// 一个状态格。
    StatusPane,
    /// 右下角尺寸柄。
    StatusGrip,
    /// 竖向滚动条。
    ScrollV,
    /// 横向滚动条。
    ScrollH,
    /// 编辑区背景、行底色、选区、当前行、括号对。
    Editor,
    /// 行号/折叠/图标/行状态四条 gutter，整列一次。
    EditorGutter,
    /// 列标尺。
    EditorRuler,
    /// 缩略图。
    EditorMinimap,
    /// 末行以下的空白。
    EditorBelow,
    /// 可见行的自定义装饰：一次调用拿整屏行表。
    EditorOverlay,
    /// 停靠面板 client 背景。
    Panel,
    /// 列表/树的一行。
    PanelRow,
    /// 输出窗格顶部的页签条。
    OutputBar,
    /// 对话框底与分组框。
    Dialog,
    /// 自绘按钮。
    DialogButton,
    /// 输入框/下拉框的配色。
    DialogField,
    /// 自动完成/提示窗。
    Popup,
    /// 脚本自建窗格的 client。`s:pane{ id = "clock" }` 的区域名是 `pane.clock`，
    /// 解析后归到这一格 —— 每一块窗格用 [`Region::key`] 区分。
    Custom,
}

impl Region {
    /// 全部区域，顺序即名字表顺序。
    pub const ALL: [Region; 29] = [
        Region::Frame,
        Region::Menubar,
        Region::MenuRow,
        Region::MenuSeparator,
        Region::Toolbar,
        Region::ToolbarButton,
        Region::TabStrip,
        Region::Tab,
        Region::TabGlyph,
        Region::DockTab,
        Region::StatusBar,
        Region::StatusPane,
        Region::StatusGrip,
        Region::ScrollV,
        Region::ScrollH,
        Region::Editor,
        Region::EditorGutter,
        Region::EditorRuler,
        Region::EditorMinimap,
        Region::EditorBelow,
        Region::EditorOverlay,
        Region::Panel,
        Region::PanelRow,
        Region::OutputBar,
        Region::Dialog,
        Region::DialogButton,
        Region::DialogField,
        Region::Popup,
        Region::Custom,
    ];

    /// 词表里的名字，小写、点分。
    pub const fn as_str(self) -> &'static str {
        match self {
            Region::Frame => "frame",
            Region::Menubar => "menubar",
            Region::MenuRow => "menu.row",
            Region::MenuSeparator => "menu.separator",
            Region::Toolbar => "toolbar",
            Region::ToolbarButton => "toolbar.button",
            Region::TabStrip => "tabstrip",
            Region::Tab => "tab",
            Region::TabGlyph => "tab.glyph",
            Region::DockTab => "dock.tab",
            Region::StatusBar => "statusbar",
            Region::StatusPane => "status.pane",
            Region::StatusGrip => "status.grip",
            Region::ScrollV => "scroll.v",
            Region::ScrollH => "scroll.h",
            Region::Editor => "editor",
            Region::EditorGutter => "editor.gutter",
            Region::EditorRuler => "editor.ruler",
            Region::EditorMinimap => "editor.minimap",
            Region::EditorBelow => "editor.below",
            Region::EditorOverlay => "editor.overlay",
            Region::Panel => "panel",
            Region::PanelRow => "panel.row",
            Region::OutputBar => "output.bar",
            Region::Dialog => "dialog",
            Region::DialogButton => "dialog.button",
            Region::DialogField => "dialog.field",
            Region::Popup => "popup",
            Region::Custom => "custom",
        }
    }

    /// 从词表名字解析。大小写不敏感，前后空白忽略 —— 主题文件是人写的。
    ///
    /// 带名字的族也认，归到它们各自那一格：
    ///
    /// - `pane.<id>` —— 脚本自建窗格（`s:pane`），归 [`Region::Custom`]；
    /// - `status.<id>` —— 脚本状态栏的一段（`s:status`），归 [`Region::StatusPane`]。
    ///
    /// 每一块窗格 / 每一段状态栏是同一个区域、不同的名字，所以查表要用
    /// [`Region::key`] 而不是 `as_str()` —— 否则两块会共用一份提示和一条缓存。
    pub fn parse(name: &str) -> Result<Region, RegionParseError> {
        let want = name.trim();
        for region in Region::ALL {
            if region.as_str().eq_ignore_ascii_case(want) {
                return Ok(region);
            }
        }
        for (prefix, region) in FAMILIES {
            if family_id(want, prefix).is_some() {
                return Ok(region);
            }
        }
        Err(RegionParseError {
            name: want.to_string(),
        })
    }

    /// 查表用的规范名。
    ///
    /// 精确区域用小写词表名（`"Tab"` → `"tab"`，大小写和空白都归一）；带名字的
    /// 族保留整个名字（`"pane.Clock"` → `"pane.clock"`）—— 每一块窗格、每一段
    /// 状态栏要有自己的提示、自己的缓存条目，塌成一个 `"custom"` 就全串了。
    pub fn key(name: &str) -> Result<String, RegionParseError> {
        let want = name.trim();
        for (prefix, _) in FAMILIES {
            if let Some(id) = family_id(want, prefix) {
                return Ok(format!("{}{}", prefix, id.to_lowercase()));
            }
        }
        Ok(Region::parse(want)?.as_str().to_string())
    }

    /// 这个区域的悬停提示能不能由皮肤给。
    ///
    /// 只有两条页签条：它们本来一条提示都没有，皮肤说了算。`toolbar.button` 的
    /// 提示是控件按 `TTN_GETDISPINFOW` 问 `cmdtext` 要的，`status.pane` 的是
    /// comctl32 在文字放不下时自己弹的 —— 在那两处再挂一条跟踪提示就是两个框叠
    /// 在一起。所以是「收不收」而不是「让不让」：声明期就拒掉。
    ///
    /// 脚本窗格（`pane.<id>`）也收：那是一块皮肤自己造的窗口，没有任何控件在
    /// 它上面挂提示。
    pub const fn takes_skin_tip(self) -> bool {
        matches!(self, Region::Tab | Region::DockTab | Region::Custom)
    }

    /// 这个区域是不是「自绘窗口」：真窗口、自己的消息循环、能拿鼠标键盘。
    /// 决定 §6.3 的事件矩阵里它收得到哪些事件。
    pub const fn is_window(self) -> bool {
        matches!(
            self,
            Region::TabStrip
                | Region::ScrollV
                | Region::ScrollH
                | Region::Editor
                | Region::EditorGutter
                | Region::EditorRuler
                | Region::EditorMinimap
                | Region::EditorBelow
                | Region::StatusBar
                | Region::Toolbar
                | Region::Custom
        )
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 词表里没有这个区域名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionParseError {
    /// 用户写的那个名字（已 trim）。
    pub name: String,
}

impl fmt::Display for RegionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown region {:?}", self.name)
    }
}

impl std::error::Error for RegionParseError {}

/// 带名字的区域族：前缀 → 它归到哪一格。
///
/// 加族的时候四件事一起过：`parse`（认名字）、`key`（保留名字）、
/// `takes_skin_tip`（提示归谁）、以及宿主那边的缓存序号（部件号只有 8 位）。
const FAMILIES: [(&str, Region); 2] = [
    ("pane.", Region::Custom),
    ("status.", Region::StatusPane),
];

/// `pane.<id>` / `status.<id>` 里的 `<id>`：非空、只有字母数字和 `_`/`-`、
/// 不超过 40 字节。
///
/// 名字会进缓存键、提示表和 `s:regions()` 的返回，所以不能任意：空名字会让两块
/// 撞在一起，带点的名字会让「区域族」这件事变得没边。
fn family_id<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    let head = name.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let id = &name[prefix.len()..];
    let ok = !id.is_empty()
        && id.len() <= 40
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名字表钉死：改名字就是改对外契约，必须同时改这里和文档。
    #[test]
    fn the_names_are_the_contract() {
        let names: Vec<&str> = Region::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "frame",
                "menubar",
                "menu.row",
                "menu.separator",
                "toolbar",
                "toolbar.button",
                "tabstrip",
                "tab",
                "tab.glyph",
                "dock.tab",
                "statusbar",
                "status.pane",
                "status.grip",
                "scroll.v",
                "scroll.h",
                "editor",
                "editor.gutter",
                "editor.ruler",
                "editor.minimap",
                "editor.below",
                "editor.overlay",
                "panel",
                "panel.row",
                "output.bar",
                "dialog",
                "dialog.button",
                "dialog.field",
                "popup",
                "custom",
            ]
        );
    }

    /// 名字唯一，否则 `parse` 会命中先出现的那个，后一个永远取不到。
    #[test]
    fn no_two_regions_share_a_name() {
        for (i, a) in Region::ALL.iter().enumerate() {
            for b in &Region::ALL[i + 1..] {
                assert_ne!(a.as_str(), b.as_str(), "{a:?} and {b:?}");
            }
        }
    }

    /// 每个名字都能解析回自己。
    #[test]
    fn every_name_round_trips() {
        for region in Region::ALL {
            assert_eq!(Region::parse(region.as_str()), Ok(region));
        }
    }

    /// 主题文件是人写的：大小写与空白不该让整份主题加载失败。
    #[test]
    fn parsing_is_forgiving_about_case_and_space() {
        assert_eq!(Region::parse("  Tab "), Ok(Region::Tab));
        assert_eq!(Region::parse("STATUS.PANE"), Ok(Region::StatusPane));
        assert_eq!(Region::parse("tab strip"), Err(RegionParseError { name: "tab strip".into() }));
    }

    /// 自绘窗口的那几个能收鼠标键盘，其余只是矩形。事件矩阵按这个分。
    #[test]
    fn only_real_windows_get_input() {
        assert!(Region::ScrollV.is_window());
        assert!(Region::Custom.is_window());
        assert!(!Region::StatusPane.is_window(), "a pane is a rect inside the strip");
        assert!(!Region::MenuRow.is_window(), "a menu row is painted by the popup");
    }

    /// `pane.<id>` 是脚本窗格的名字，归到 `custom` 那一格；每一块窗格自己的名字
    /// 由 `key` 保留 —— 塌成一个 `"custom"` 的话两块窗格的提示和缓存会串。
    #[test]
    fn a_pane_name_parses_as_custom_and_keeps_its_own_key() {
        assert_eq!(Region::parse("pane.clock"), Ok(Region::Custom));
        assert_eq!(Region::parse("  PANE.Clock "), Ok(Region::Custom));
        assert_eq!(Region::key("pane.clock").as_deref(), Ok("pane.clock"));
        assert_eq!(Region::key("  PANE.Clock ").as_deref(), Ok("pane.clock"));
        assert_eq!(Region::key("Tab").as_deref(), Ok("tab"));
        // 两块不同的窗格必须是两个键。
        assert_ne!(
            Region::key("pane.a").unwrap(),
            Region::key("pane.b").unwrap()
        );
        // 名字会进缓存键和提示表，所以不能任意。
        assert!(Region::parse("pane.").is_err(), "empty id");
        assert!(Region::parse("pane.a.b").is_err(), "dots would make it a family");
        assert!(Region::parse("pane.a b").is_err(), "space");
        assert!(Region::parse("panel.clock").is_err(), "no such family");
    }

    /// `status.<id>` 是状态栏一段的名字，归到 `status.pane` 那一格，同样每段
    /// 一个键 —— 否则十几段状态栏会共用一份缓存和一条提示。
    #[test]
    fn a_status_segment_name_parses_as_a_status_pane() {
        assert_eq!(Region::parse("status.pos"), Ok(Region::StatusPane));
        assert_eq!(Region::parse("  STATUS.Pos "), Ok(Region::StatusPane));
        assert_eq!(Region::key("status.pos").as_deref(), Ok("status.pos"));
        assert_eq!(Region::key("  STATUS.Pos ").as_deref(), Ok("status.pos"));
        assert_ne!(
            Region::key("status.pos").unwrap(),
            Region::key("status.enc").unwrap()
        );
        // 精确名照旧：`status.pane` 是那一格自己，不是族。
        assert_eq!(Region::key("status.pane").as_deref(), Ok("status.pane"));
        assert!(Region::parse("status.").is_err());
    }
}
