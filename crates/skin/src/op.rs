//! 绘制原语：12 条，回放层只认这 12 条。
//!
//! 为什么是「op 列表」而不是直接调 GDI：一份 op 列表是纯数据，可以缓存、可以
//! 压紧、可以整块 `memcpy`、可以按区域丢掉；一串 GDI 调用不行。皮肤的 `paint`
//! 回调产出 op 列表，宿主把它缓存起来，之后每帧只是回放。
//!
//! **`UiOp` 是 `Copy` 的**，这一点是刻意的：文字和点集都放在加载期的表里，op 里
//! 只存下标（[`TextId`] / [`PointsId`]），所以一份 op 列表可以整块拷、可以当
//! `&[UiOp]` 直接喂给回放层，回放路径上没有一次分配、没有一个 `Box`。
//!
//! 文字用占位符 [`TextSource::Text`] / [`TextSource::Label`] / [`TextSource::Value`]
//! 而不是字面量，是缓存能活下来的关键：状态栏的「行 12，列 34」每帧都变，文字进了
//! 缓存键就永远命中不了；用占位符之后 op 列表只随尺寸和状态变。

use dg_render::{Color, Rect};

/// 一个点，DIP。`dg_render` 里那一份是唯一定义，这里转出来给皮肤 op 用。
pub use dg_render::Point;

/// 加载期字符串表里的一个词。`@text`/`@label`/`@value` 之外的文字都走这里。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct TextId(pub u16);

/// 加载期点集表里的一段。多边形、折线用。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct PointsId {
    pub start: u16,
    pub len: u16,
}

impl PointsId {
    /// 从整张点集表里切出这一段。越界（皮肤写坏了）返回 `None`，
    /// 让回放层跳过这条而不是画一半。
    pub fn slice(self, table: &[Point]) -> Option<&[Point]> {
        let start = self.start as usize;
        let len = self.len as usize;
        table.get(start..start.checked_add(len)?)
    }
}

/// 加载期字体表里的一个字体。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct FontId(pub u16);

/// 加载期图片表里的一张图（图标格、脚本带的位图）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct ImageId(pub u16);

/// 一段文字从哪来。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TextSource {
    /// 加载期表里的字面量。
    Literal(TextId),
    /// `@text`：这个部件当前的文字（页签标题、状态格内容）。
    Text,
    /// `@label`：部件的标签（菜单项名字，去掉快捷键列）。
    Label,
    /// `@value`：部件的数值（菜单快捷键列、进度百分比）。
    Value,
}

/// 水平对齐。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Align {
    /// 靠左（LTR）。
    #[default]
    Start,
    Center,
    End,
}

/// 垂直对齐。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum VAlign {
    Top,
    #[default]
    Middle,
    Bottom,
}

/// 渐变方向。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Dir {
    /// 从上到下。
    #[default]
    V,
    /// 从左到右。
    H,
}

/// 图片怎么填进矩形。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Fit {
    /// 拉伸到矩形。
    #[default]
    Stretch,
    /// 平铺。
    Tile,
    /// 居中，不缩放。
    Center,
}

/// 一条绘制原语。
///
/// 参数名与 Lua 的字段名一致；回放层（GDI / D2D / ARGB）各自把
/// 同一份列表画出来，不共享实现，只共享语义。
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum UiOp {
    /// 实心填充。
    Fill { rect: Rect, color: Color },
    /// 空心矩形（描边，画在矩形内侧）。
    Rect {
        rect: Rect,
        color: Color,
        width: f32,
    },
    /// 圆角矩形，填充与描边都可选。
    Round {
        rect: Rect,
        radius: f32,
        fill: Option<Color>,
        stroke: Option<Color>,
        stroke_w: f32,
    },
    /// 一段直线。
    Line {
        a: Point,
        b: Point,
        color: Color,
        width: f32,
    },
    /// 折线/多边形，点集在加载期表里。
    Poly {
        points: PointsId,
        color: Color,
        width: f32,
        closed: bool,
    },
    /// 圆。
    Circle {
        center: Point,
        r: f32,
        fill: Option<Color>,
        stroke: Option<Color>,
        stroke_w: f32,
    },
    /// 线性渐变。
    Gradient {
        rect: Rect,
        from: Color,
        to: Color,
        dir: Dir,
    },
    /// 一段文字，画在 `rect` 里：对齐、裁剪、省略号都相对这个矩形。
    ///
    /// 用矩形而不是一个点，是因为 `DrawTextW`/`DrawTextLayout` 的语义本来就是
    /// 矩形，而 chrome 里的每一处文字（页签标题、状态格、菜单行）本来就有矩形。
    /// 给一个点的话，垂直居中和省略号都得在外面先算一遍。
    Text {
        rect: Rect,
        text: TextSource,
        font: FontId,
        color: Color,
        align: Align,
        valign: VAlign,
        clip: Option<Rect>,
        ellipsis: bool,
    },
    /// 一张图。
    Image {
        rect: Rect,
        src: ImageId,
        tint: Option<Color>,
        fit: Fit,
    },
    /// 压栈裁剪。
    Clip { rect: Rect },
    /// 弹栈。
    Unclip,
    /// 外阴影。
    Shadow {
        rect: Rect,
        radius: f32,
        color: Color,
        blur: f32,
    },
    /// 配方里的占位，回放时忽略（[`OpList::compact`] 会直接丢掉）。
    Spacer,
}

impl UiOp {
    /// 这条 op 占的矩形；没有面积的概念时返回 `None`。
    ///
    /// 用来丢「零面积」的 op：一个宽 0 的填充在三种回放后端上都是空操作，但
    /// 它会占缓存空间、会让 `Clip` 压栈。
    pub fn area(&self) -> Option<Rect> {
        match *self {
            UiOp::Fill { rect, .. }
            | UiOp::Rect { rect, .. }
            | UiOp::Round { rect, .. }
            | UiOp::Gradient { rect, .. }
            | UiOp::Image { rect, .. }
            | UiOp::Clip { rect }
            | UiOp::Shadow { rect, .. }
            | UiOp::Text { rect, .. } => Some(rect),
            UiOp::Line { a, b, .. } => Some(Rect::new(
                a.x.min(b.x),
                a.y.min(b.y),
                (a.x - b.x).abs(),
                (a.y - b.y).abs(),
            )),
            UiOp::Circle { center, r, .. } => {
                Some(Rect::new(center.x - r, center.y - r, r * 2.0, r * 2.0))
            }
            // 点集的面积要遍历点表，这里不算；spacer 没有面积。
            UiOp::Poly { .. } | UiOp::Unclip | UiOp::Spacer => None,
        }
    }

    /// 这条 op 是不是「一定画不出东西」。
    fn is_empty(&self) -> bool {
        match *self {
            UiOp::Spacer => true,
            _ => self
                .area()
                .is_some_and(|r| r.w <= 0.0 || r.h <= 0.0),
        }
    }
}

/// 一份 op 列表。
///
/// `compact()` 是构建期的最后一步：丢掉零面积与 `Spacer`。**不**做相邻同色合并
/// —— 那要按后端分别量，先让回放层跑起来，再决定压多少（P4）。
#[derive(Clone, Default, Debug)]
pub struct OpList {
    ops: Vec<UiOp>,
}

impl OpList {
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// 预留容量。加载期知道条数时用，避免构建时反复扩容。
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            ops: Vec::with_capacity(cap),
        }
    }

    pub fn push(&mut self, op: UiOp) {
        self.ops.push(op);
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn as_slice(&self) -> &[UiOp] {
        &self.ops
    }

    pub fn clear(&mut self) {
        self.ops.clear();
    }

    /// 这份列表在内存里占多少字节（缓存预算按它算）。
    pub fn byte_len(&self) -> usize {
        self.ops.len() * std::mem::size_of::<UiOp>()
    }

    /// 构建期收尾：丢掉画不出东西的 op。顺序保持，缓存键不受影响。
    pub fn compact(&mut self) {
        self.ops.retain(|op| !op.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(w: f32, h: f32) -> UiOp {
        UiOp::Fill {
            rect: Rect::new(0.0, 0.0, w, h),
            color: Color::rgb(1, 2, 3),
        }
    }

    /// op 必须是 `Copy` 且足够小：回放层按 `&[UiOp]` 走，一帧几十条。
    #[test]
    fn an_op_is_copy_and_stays_small() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<UiOp>();
        let size = std::mem::size_of::<UiOp>();
        eprintln!(
            "[ui] UiOp {size} bytes, CacheKey {} bytes",
            std::mem::size_of::<crate::CacheKey>()
        );
        assert!(size <= 56, "UiOp grew to {size} bytes");
    }

    /// 占位符与字面量是两种来源：前者不进缓存键，后者是加载期表里的下标。
    #[test]
    fn text_can_come_from_the_host_or_from_the_skin() {
        let host = UiOp::Text {
            rect: Rect::new(0.0, 0.0, 100.0, 20.0),
            text: TextSource::Text,
            font: FontId(0),
            color: Color::rgb(0, 0, 0),
            align: Align::Start,
            valign: VAlign::Middle,
            clip: None,
            ellipsis: false,
        };
        assert!(matches!(host, UiOp::Text { text: TextSource::Text, .. }));
        assert!(matches!(
            TextSource::Literal(TextId(7)),
            TextSource::Literal(TextId(7))
        ));
    }

    /// `compact` 丢掉 spacer 和零面积，保留其余的顺序。
    #[test]
    fn compact_drops_the_ops_that_paint_nothing() {
        let mut list = OpList::new();
        list.push(fill(10.0, 4.0));
        list.push(UiOp::Spacer);
        list.push(fill(0.0, 4.0));
        list.push(UiOp::Clip {
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
        });
        list.push(UiOp::Unclip);
        list.push(fill(2.0, 2.0));
        assert_eq!(list.len(), 6);
        list.compact();
        assert_eq!(list.len(), 3, "the two fills and the unclip survive");
        assert!(matches!(list.as_slice()[0], UiOp::Fill { .. }));
        assert!(matches!(list.as_slice()[1], UiOp::Unclip));
        assert!(matches!(list.as_slice()[2], UiOp::Fill { .. }));
    }

    /// 文字画在矩形里，所以它有自己的面积 —— 宽 0 的文本框确实什么也不画。
    #[test]
    fn text_is_never_dropped_as_empty() {
        let op = UiOp::Text {
            rect: Rect::new(0.0, 0.0, 100.0, 20.0),
            text: TextSource::Label,
            font: FontId(0),
            color: Color::rgb(0, 0, 0),
            align: Align::Start,
            valign: VAlign::Middle,
            clip: None,
            ellipsis: false,
        };
        assert!(!op.is_empty());
    }

    /// 缓存预算按字节算，所以字节数得可查。
    #[test]
    fn byte_len_follows_the_op_size() {
        let mut list = OpList::with_capacity(4);
        list.push(fill(1.0, 1.0));
        list.push(fill(2.0, 2.0));
        assert_eq!(list.byte_len(), 2 * std::mem::size_of::<UiOp>());
        list.clear();
        assert_eq!(list.byte_len(), 0);
    }
}
