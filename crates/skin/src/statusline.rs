//! 状态栏的排版模型：一串**段**，宽度、优先级、以及内容模板。
//!
//! 这一层是纯的 —— 不碰窗口、不碰 Lua、不碰文档，输入是「每段量出来多宽」和
//! 「这一条有多宽」，输出是「每段实际多宽」。所以它能被单测钉死，而真正的
//! 状态栏窗口只负责把结果交给控件、按段画。
//!
//! 三件事在这里定下来：
//!
//! 1. **宽度**：`auto`（按文字量）/ 固定像素 / 区间 / `fill`（吃掉剩下的）。
//! 2. **优先级**：窗口窄了先丢谁。数字**越大越重要**，丢的时候从最小的开始。
//!    这比 Vim 的 `%<` 强 —— 那个只能指定一个截断点，这里是每段各有一个数。
//! 3. **`fill`**：等价于 Vim 的 `%=`。没写的话自动补一个在末尾，这样整条永远
//!    铺满，不会在右边留一条没人画的控制条底色。
//!
//! 段的内容不在这一层：模板展开在 [`expand`]，Lua 的 `update` 回调在宿主那边跑
//! （它要 bridge，而绘制档没有）。

use std::fmt;

/// 一段的宽度怎么定。
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Width {
    /// 按这一段的文字量出来（宿主测，含两侧内边距）。
    Auto,
    /// 固定宽度，DIP。
    Fixed(f32),
    /// 量出来之后夹在区间里，DIP。给「会变长的路径」用：太窄就截，太宽也不
    /// 让它把别人挤掉。
    Range {
        /// 下限。
        min: f32,
        /// 上限。
        max: f32,
    },
    /// 吃掉剩下的宽度。一条里最多一个；没写就自动补一个在末尾。
    Fill,
}

impl Default for Width {
    fn default() -> Self {
        Width::Auto
    }
}

impl Width {
    /// 把「量出来的像素」算成「这一段要多少像素」。`fill` 在这里答 0 —— 它的
    /// 宽度要等别人都算完才知道。
    ///
    /// `max <= 0` 是「没有上限」（`{ max = 104 }` 里 `min` 省掉时就是这样）；
    /// `min` 为 0 就是「没有下限」，量多少算多少。
    pub fn resolve(self, measured: i32, dpi: i32) -> i32 {
        let scale = |dip: f32| (dip * dpi.max(96) as f32 / 96.0).round() as i32;
        match self {
            Width::Auto => measured.max(0),
            Width::Fixed(dip) => scale(dip).max(0),
            Width::Range { min, max } => {
                let lo = scale(min).max(0);
                let hi = if max <= 0.0 {
                    i32::MAX
                } else {
                    scale(max).max(lo)
                };
                measured.clamp(lo, hi)
            }
            Width::Fill => 0,
        }
    }
}

/// 一条状态栏里的一段（声明部分，没有文字）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Segment {
    /// 段名，也是绘制区域名 `status.<id>` 的后半截、`ctx.part`。`fill` 段可以没有。
    pub id: String,
    /// 内容模板，`%l` 之类。空串 = 这段的文字由 `update` 给。
    pub template: String,
    /// 宽度规则。
    pub width: Width,
    /// 窗口窄了先丢谁：数字越大越重要，丢的时候从最小的开始。默认 50。
    pub priority: u8,
    /// 点一下跑哪个命令（`App.command` 注册的名字，或内置命令名）。空 = 不可点。
    pub click: String,
    /// 这段有没有 `update` 回调（文字要在宿主那边跑 Lua 才算得出来）。
    pub dynamic: bool,
}

impl Segment {
    /// 绘制区域名：`status.<id>`。没有 id 的段（`fill`）没有画法。
    pub fn region(&self) -> Option<String> {
        (!self.id.is_empty()).then(|| format!("status.{}", self.id))
    }
}

/// 排版结果里的一段。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot {
    /// 实际宽度，像素。
    pub width: i32,
    /// 这一段在这一版排版里还在不在（宽度不够时会被丢掉）。
    pub shown: bool,
}

/// 按宽度规则和可用宽度算每一段多宽。
///
/// `measured` 是宿主量出来的像素（`fill` 段的值会被忽略）。返回的 `Slot` 与输入
/// 一一对应；`shown == false` 的段宽度是 0，宿主跳过它。
///
/// 规则，按顺序：
///
/// 1. 每段按 [`Width::resolve`] 定宽，`fill` 记 0。
/// 2. 总量超了就**从优先级最低的一段开始丢**，一次丢一段，直到放得下。优先级
///    相同丢靠右的（右边的通常是次要信息，而且丢右边不会让左边跳位置）。
/// 3. 剩下的宽度给 `fill`；没有 `fill` 段就全给最后一个还活着的段 —— 整条必须
///    铺满，否则控件会在右边画自己的底色。
/// 4. 一段都不剩（窄到极点）：全部隐藏，返回全 0。
pub fn layout(segments: &[Segment], measured: &[i32], avail: i32, dpi: i32) -> Vec<Slot> {
    let n = segments.len();
    let mut slots = vec![Slot { width: 0, shown: false }; n];
    if n == 0 || avail <= 0 {
        return slots;
    }
    let fill_at = segments.iter().position(|s| s.width == Width::Fill);
    let mut width: Vec<i32> = segments
        .iter()
        .zip(measured.iter().chain(std::iter::repeat(&0)))
        .map(|(seg, m)| seg.width.resolve(*m, dpi))
        .collect();
    let mut alive = vec![true; n];

    // 2. 超宽：按优先级从低到高丢，同优先级丢靠右的。
    let sum = |w: &[i32], alive: &[bool]| -> i32 {
        w.iter()
            .zip(alive.iter())
            .filter(|(_, a)| **a)
            .map(|(w, _)| *w)
            .sum()
    };
    let mut total = sum(&width, &alive);
    while total > avail {
        let victim = (0..n)
            .filter(|i| alive[*i] && Some(*i) != fill_at)
            .min_by_key(|i| (segments[*i].priority, std::cmp::Reverse(*i)));
        let Some(victim) = victim else {
            break;
        };
        alive[victim] = false;
        total = sum(&width, &alive);
    }

    // 3. 余量给 fill，没有 fill 就给最后一个活着的。
    let leftover = (avail - total).max(0);
    let target = fill_at
        .filter(|i| alive[*i])
        .or_else(|| (0..n).rev().find(|i| alive[*i]));
    if let Some(t) = target {
        width[t] += leftover;
    }

    for i in 0..n {
        slots[i] = Slot {
            width: if alive[i] { width[i].max(0) } else { 0 },
            shown: alive[i],
        };
    }
    slots
}

/// 内置状态栏的十三个字段，用**同一套词汇**表达，按默认左右顺序。
///
/// 存在的理由只有一个：让皮肤能「在内置那条后面加一格」，而不是把十三个字段
/// 抄一遍再维护。`id` 就是 `dg_ui::statusbar::StatusField::name` 那个词 ——
/// 宿主按 id 取内置文字，所以这里的 `template` 全是空串、`dynamic` 全是 false：
/// 文字不归模板管，归内置模型管（`就绪` / `4.2 KB` / `U+4E2D` 这些模板表达不了）。
///
/// `message` 是 `Fill`：整条的余量给它，和内置的「第一格吃掉剩下的」一致。
pub fn default_segments() -> Vec<Segment> {
    const DEFAULTS: [(&str, Width, u8); 13] = [
        ("message", Width::Fill, 100),
        ("filesize", Width::Auto, 20),
        ("sync", Width::Auto, 15),
        ("selection", Width::Auto, 30),
        ("syntax", Width::Auto, 45),
        ("tabsize", Width::Auto, 40),
        ("ln", Width::Auto, 65),
        ("col", Width::Auto, 65),
        ("char", Width::Auto, 10),
        ("lines", Width::Auto, 25),
        ("eol", Width::Auto, 50),
        ("editmode", Width::Auto, 70),
        ("encoding", Width::Auto, 55),
    ];
    DEFAULTS
        .iter()
        .map(|(id, width, priority)| Segment {
            id: (*id).to_string(),
            width: *width,
            priority: *priority,
            ..Segment::default()
        })
        .collect()
}

/// 模板展开要用的一份快照。
///
/// 由宿主每帧从文档/光标/选区算出来 —— 和 `StatusModel` 同源，但这里是**给模板
/// 用的字符串**，不经过语言表。字段都是展开时真正读得到的，没有 `Option`：拿不到
/// 的填默认值（0 / 空串），模板里不会出现 `nil`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// 光标所在行，从 1 开始（Vim 的 `%l`）。
    pub line: usize,
    /// 光标所在列，从 0 开始（Vim 的 `%c`）。
    pub col: usize,
    /// 总行数（Vim 的 `%L`）。
    pub lines: usize,
    /// 光标在文件里的百分比，0..100（Vim 的 `%p`）。
    pub pct: usize,
    /// 文件名（不含目录）。
    pub name: String,
    /// 完整路径。
    pub path: String,
    /// 改过没保存。
    pub modified: bool,
    /// 只读。
    pub read_only: bool,
    /// 语法名，没挂语法是空串。
    pub syntax: String,
    /// 换行方式：`LF` / `CRLF` / `CR`。
    pub eol: String,
    /// 编码标签。
    pub encoding: String,
    /// 插入 / 覆写 / 列模式 / 多选。
    pub mode: String,
    /// 选中的字符数。
    pub sel_chars: usize,
    /// 选区跨了几行。
    pub sel_lines: usize,
    /// 大文件模式。
    pub large: bool,
}

/// 模板里的一个占位符不认。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceholderError {
    /// 出问题的那个字符。
    pub ch: char,
}

impl fmt::Display for PlaceholderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown placeholder %{}", self.ch)
    }
}

impl std::error::Error for PlaceholderError {}

/// 展开一段模板。
///
/// 认这些（借 Vim 的字母，语义照文档写死，不猜）：
///
/// | 占位符 | 展开成 |
/// | --- | --- |
/// | `%l` / `%c` / `%L` | 行 / 列 / 总行数 |
/// | `%p` | 光标百分比，`0`..`100` |
/// | `%f` / `%F` | 文件名 / 完整路径 |
/// | `%m` | 改过没保存时 `[+]`，否则空 |
/// | `%r` | 只读时 `[RO]`，否则空 |
/// | `%y` | 语法名，没有就是空 |
/// | `%e` / `%E` | 换行方式 / 编码 |
/// | `%M` | 插入 / 覆写 / 列模式 / 多选 |
/// | `%S` / `%s` | 选中字符数 / 选区行数 |
/// | `%a` | 大文件时 `大文件`，否则空 |
/// | `%%` | 一个字面 `%` |
///
/// 不认的占位符**当场报错**，不原样吐出来：写错一个字母就整条不变样，是那种
/// 「看着没问题、其实没生效」的坑。
pub fn expand(template: &str, facts: &Facts) -> Result<String, PlaceholderError> {
    let mut out = String::with_capacity(template.len() + 16);
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(p) = chars.next() else {
            return Err(PlaceholderError { ch: '%' });
        };
        match p {
            '%' => out.push('%'),
            'l' => out.push_str(&facts.line.to_string()),
            'c' => out.push_str(&facts.col.to_string()),
            'L' => out.push_str(&facts.lines.to_string()),
            'p' => out.push_str(&facts.pct.to_string()),
            'f' => out.push_str(&facts.name),
            'F' => out.push_str(&facts.path),
            'm' => out.push_str(if facts.modified { "[+]" } else { "" }),
            'r' => out.push_str(if facts.read_only { "[RO]" } else { "" }),
            'y' => out.push_str(&facts.syntax),
            'e' => out.push_str(&facts.eol),
            'E' => out.push_str(&facts.encoding),
            'M' => out.push_str(&facts.mode),
            'S' => out.push_str(&facts.sel_chars.to_string()),
            's' => out.push_str(&facts.sel_lines.to_string()),
            'a' => out.push_str(if facts.large { "大文件" } else { "" }),
            other => return Err(PlaceholderError { ch: other }),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(width: Width, priority: u8) -> Segment {
        Segment {
            width,
            priority,
            ..Segment::default()
        }
    }

    fn named(id: &str, width: Width, priority: u8) -> Segment {
        Segment {
            id: id.to_string(),
            width,
            priority,
            ..Segment::default()
        }
    }

    /// `fill` 吃掉剩下的；整条铺满。
    #[test]
    fn fill_absorbs_the_remainder() {
        let segs = [seg(Width::Auto, 50), seg(Width::Fill, 50), seg(Width::Auto, 50)];
        let slots = layout(&segs, &[100, 0, 80], 400, 96);
        assert_eq!(slots[0].width, 100);
        assert_eq!(slots[2].width, 80);
        assert_eq!(slots[1].width, 220, "400 - 100 - 80");
        assert!(slots.iter().all(|s| s.shown));
    }

    /// 没有 `fill` 段时，余量给最后一个还活着的 —— 否则控件在右边画自己的底色。
    #[test]
    fn the_last_segment_takes_the_leftover_when_no_fill_is_declared() {
        let segs = [seg(Width::Auto, 50), seg(Width::Auto, 50)];
        let slots = layout(&segs, &[100, 80], 400, 96);
        assert_eq!(slots[0].width, 100);
        assert_eq!(slots[1].width, 300);
    }

    /// 超宽先丢优先级最低的；同优先级丢靠右的。
    #[test]
    fn the_lowest_priority_is_dropped_first_and_ties_go_right() {
        let segs = [
            named("keep", Width::Auto, 100),
            named("drop", Width::Auto, 10),
            named("tie_a", Width::Auto, 50),
            named("tie_b", Width::Auto, 50),
        ];
        // 100 + 100 + 100 + 100 = 400，只给 320：先丢 drop(10)，正好放下。
        let slots = layout(&segs, &[100, 100, 100, 100], 320, 96);
        assert!(slots[0].shown);
        assert!(!slots[1].shown, "priority 10 goes first");
        assert!(slots[2].shown && slots[3].shown);

        // 只给 200：还得再丢一个，两个 50 里丢靠右的（tie_b）。
        let slots = layout(&segs, &[100, 100, 100, 100], 200, 96);
        assert!(slots[0].shown);
        assert!(!slots[1].shown);
        assert!(slots[2].shown, "tie goes to the left one");
        assert!(!slots[3].shown);
    }

    /// `fill` 段不会被丢掉 —— 它是余量的去处，丢了就没人铺满。
    #[test]
    fn fill_is_never_the_victim() {
        let segs = [seg(Width::Fill, 0), named("a", Width::Auto, 1), named("b", Width::Auto, 2)];
        // 200 的量、给 150：丢 a(1) 就够了，fill 拿走剩下的 50。
        let slots = layout(&segs, &[0, 100, 100], 150, 96);
        assert!(slots[0].shown, "fill survives");
        assert!(!slots[1].shown, "a has the lowest priority");
        assert!(slots[2].shown);
        assert_eq!(slots[0].width, 50, "150 - 100");

        // 窄到只放得下一段：fill 仍然活着，而且它一个人吃掉整条（其余都丢了）。
        let slots = layout(&segs, &[0, 100, 100], 50, 96);
        assert!(slots[0].shown, "fill survives even with nothing to fill");
        assert_eq!(slots[0].width, 50, "it is the only one left");
        assert!(!slots[1].shown && !slots[2].shown);
    }

    /// 窄到一段都放不下：全隐藏，宽度全 0（不是负的 —— `SB_SETPARTS` 会拒）。
    #[test]
    fn nothing_fits_means_everything_is_hidden() {
        let segs = [named("a", Width::Fixed(200.0), 10), named("b", Width::Fixed(200.0), 20)];
        let slots = layout(&segs, &[0, 0], 100, 96);
        assert!(slots.iter().all(|s| !s.shown && s.width == 0));
        assert!(layout(&segs, &[0, 0], 0, 96).iter().all(|s| s.width == 0));
        assert!(layout(&[], &[], 100, 96).is_empty());
    }

    /// 区间宽度夹在 min/max 之间，且跟着 DPI 走。
    #[test]
    fn a_range_clamps_the_measurement() {
        assert_eq!(Width::Range { min: 40.0, max: 120.0 }.resolve(10, 96), 40);
        assert_eq!(Width::Range { min: 40.0, max: 120.0 }.resolve(300, 96), 120);
        assert_eq!(Width::Range { min: 40.0, max: 120.0 }.resolve(80, 96), 80);
        assert_eq!(Width::Fixed(50.0).resolve(999, 96), 50);
        assert_eq!(Width::Fixed(50.0).resolve(0, 120), 63, "50 DIP at 120 DPI");
        // 区间写反了也不会 panic：min 说了算。
        assert_eq!(Width::Range { min: 80.0, max: 20.0 }.resolve(50, 96), 80);
    }

    /// `{ max = 104 }` / `{ min = 40 }`：省掉的那一头等于没有。以前两个都得写，
    /// 只写一个会报 `field not found` —— 而「按文字量，最多 104」正是最常用的写法。
    #[test]
    fn a_half_open_range_has_no_cap_or_no_floor() {
        assert_eq!(Width::Range { min: 0.0, max: 104.0 }.resolve(60, 96), 60);
        assert_eq!(Width::Range { min: 0.0, max: 104.0 }.resolve(300, 96), 104);
        assert_eq!(Width::Range { min: 40.0, max: 0.0 }.resolve(10, 96), 40);
        assert_eq!(Width::Range { min: 40.0, max: 0.0 }.resolve(900, 96), 900);
    }

    fn facts() -> Facts {
        Facts {
            line: 12,
            col: 34,
            lines: 900,
            pct: 5,
            name: "window.rs".into(),
            path: r"E:\dogedit\crates\app\src\win32\window.rs".into(),
            modified: true,
            read_only: false,
            syntax: "rs".into(),
            eol: "CRLF".into(),
            encoding: "UTF-8".into(),
            mode: "插入".into(),
            sel_chars: 42,
            sel_lines: 3,
            large: false,
        }
    }

    /// 模板：每个占位符都按文档展开，`%%` 是字面量。
    #[test]
    fn every_placeholder_expands() {
        assert_eq!(expand("%l:%c", &facts()).unwrap(), "12:34");
        assert_eq!(expand("%L 行 %p%%", &facts()).unwrap(), "900 行 5%");
        assert_eq!(expand("%f %F", &facts()).unwrap(), "window.rs E:\\dogedit\\crates\\app\\src\\win32\\window.rs");
        assert_eq!(expand("%m%r", &facts()).unwrap(), "[+]");
        assert_eq!(expand("%y %e %E %M", &facts()).unwrap(), "rs CRLF UTF-8 插入");
        assert_eq!(expand("%S 字符 %s 行", &facts()).unwrap(), "42 字符 3 行");
        assert_eq!(expand("100%%", &facts()).unwrap(), "100%");
        assert_eq!(expand("", &facts()).unwrap(), "");
        // 没有语法、没改过、不是只读：这几项展开成空串，模板里不会漏出 nil。
        let bare = Facts { syntax: String::new(), modified: false, ..facts() };
        assert_eq!(expand("[%y][%m][%r]", &bare).unwrap(), "[][][]");
    }

    /// 不认的占位符当场报错 —— 原样吐出来就成了「看着没问题、其实没生效」。
    #[test]
    fn an_unknown_placeholder_is_an_error() {
        assert_eq!(expand("%q", &facts()), Err(PlaceholderError { ch: 'q' }));
        assert_eq!(expand("abc %", &facts()), Err(PlaceholderError { ch: '%' }));
    }

    /// 区域名：有 id 才有画法，`fill` 段没有。
    #[test]
    fn only_a_named_segment_has_a_region() {
        assert_eq!(named("pos", Width::Auto, 50).region().as_deref(), Some("status.pos"));
        assert_eq!(seg(Width::Fill, 50).region(), None);
    }
}
