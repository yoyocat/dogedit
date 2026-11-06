//! 一格的形状：配方里的四个数值键，换算成**设备像素**。
//!
//! 两个页签条（文档页签 `tab`、停靠页签 `dock.tab`）的几何是同一套 —— 填充块
//! 从框线内沿起、上下各探出一点把下一行封住，文字框比填充块再缩一点。所以换算
//! 也只写一份：`radius` / `gap` / `border` / `pad_y` 在这里一次性乘上 DPI，
//! 绘制那一层拿到的全是整数像素。
//!
//! 为什么不各自乘：`paint_tab` 和 `paint_label` 各读一次配方，在 125% 下
//! `2.5` 四舍五入两次就可能一次得 2、一次得 3 —— 同一格的两半对不上。
//!
//! ## 出厂值 = 内置几何
//!
//! `Shape::default()` 换算出来是「四个缩进全 0、框线 1 px」，正好是没配方时
//! 那几条写死的数字。所以「空皮肤 0 像素」这条验收线在这一层就能用单测钉住，
//! 不必靠截图。

use windows::Win32::Foundation::RECT;

use crate::win32::toolbar::dpi_scale;

/// 一格的形状，已经换算成设备像素。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CellShape {
    /// 左边缩进（`gap` 的一半）。
    pub left_inset: i32,
    /// 右边缩进（`gap` 的另一半，奇数时比左边多 1 px）。
    pub right_inset: i32,
    /// 选中格填充块的圆角半径。
    pub radius: i32,
    /// 框线宽度，**至少 1** —— 内置的框就是 1 px，`border` 只加粗不取消。
    pub border: i32,
    /// 填充块和文字框上下各缩进。
    pub pad_y: i32,
}

impl CellShape {
    /// 读一格的配方。
    pub(super) fn of(shape: &dg_skin::Shape, dpi: u32) -> CellShape {
        let scale = dpi_scale(dpi);
        let gap = dip(shape.gap, scale);
        CellShape {
            left_inset: gap / 2,
            // 奇数 `gap` 不会因为除不尽而少一像素：两格合起来正好空 `gap`。
            right_inset: gap - gap / 2,
            radius: dip(shape.radius, scale),
            border: dip(shape.border, scale).max(1),
            pad_y: dip(shape.pad_y, scale),
        }
    }

    /// 框线内沿的上边。`border = 1` 时是内置的 `top + 2`。
    pub(super) fn inner_top(self, top: i32) -> i32 {
        top + 1 + self.border
    }

    /// 框线内沿的右边。`border = 1` 时是内置的 `right - 2`。
    pub(super) fn inner_right(self, right: i32) -> i32 {
        right - 1 - self.border
    }

    /// 这一格的**块**：填充和（圆角时的）框都画在这个矩形上，两者的角才对得上。
    ///
    /// `left`/`right` 由调用方先缩好 `gap`；这里只管上下：`pad_y` 各缩一次，
    /// `+4` 是内置的越界量 —— 填充探到客户区外，把下一行封住。
    pub(super) fn body(self, left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top: self.inner_top(top) + self.pad_y,
            right: self.inner_right(right),
            bottom: bottom + 4 - self.pad_y,
        }
    }

    /// 把文字框夹进这一格的块里 —— **只夹不挪**。
    ///
    /// 不能让 `gap` 去搬文字：格子宽度是 `tab_width` 量的，量的时候不知道 `gap`，
    /// 把文字跟着挪就一定会把贴边的标签截掉。夹住上限则相反：`gap = 0` 时块比
    /// 文字框大，四次 `max`/`min` 全不生效（所以出厂路径逐像素没动）；`gap > 0`
    /// 时最多把文字裁到块里，起始位置还是布局给的那个。
    pub(super) fn fit_text(self, text: &mut RECT, body: &RECT) {
        text.left = text.left.max(body.left + 1);
        text.right = text.right.min(body.right - 1);
        text.top = text.top.max(body.top);
        text.bottom = text.bottom.min(body.bottom);
    }
}

/// DIP → 设备像素。形状键都是 DIP，绘制是像素。
pub(super) fn dip(value: f32, scale: f32) -> i32 {
    (value * scale).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 出厂配方 = 内置几何。这是「空皮肤 0 像素」在页签这一格的证据：不写
    /// `[style.tab]` 时四个缩进全 0、框线还是 1 px、内沿还是原来的
    /// `top + 2` / `right - 2`。
    #[test]
    fn an_empty_recipe_leaves_the_geometry_alone() {
        let s = CellShape::of(&dg_skin::Shape::default(), 96);
        assert_eq!((s.left_inset, s.right_inset), (0, 0));
        assert_eq!(s.radius, 0, "0 = 直角，走 FillRect");
        assert_eq!(s.pad_y, 0);
        assert_eq!(s.border, 1, "内置的框就是 1 px");
        assert_eq!(s.inner_top(40), 42);
        assert_eq!(s.inner_right(200), 198);
        // 块和原来那个写死的矩形逐字段相同。
        let body = s.body(10, 40, 200, 60);
        assert_eq!(
            (body.left, body.top, body.right, body.bottom),
            (10, 42, 198, 64)
        );
    }

    /// 奇数 `gap` 不会被除不尽吞掉：两格各缩一半，合起来正好空 `gap`。
    #[test]
    fn an_odd_gap_keeps_its_pixel() {
        for gap in 1..=5 {
            let mut shape = dg_skin::Shape::default();
            shape.gap = gap as f32;
            let s = CellShape::of(&shape, 96);
            assert_eq!(s.left_inset + s.right_inset, gap, "gap = {gap} 分完还剩像素");
            assert!(s.right_inset - s.left_inset <= 1, "两边最多差 1 px");
        }
    }

    /// 形状键是 DIP：125% 下 4 → 5，`border` 加粗也照样放大。
    #[test]
    fn the_shape_is_scaled_by_dpi() {
        let mut shape = dg_skin::Shape::default();
        shape.radius = 4.0;
        shape.border = 2.0;
        shape.pad_y = 2.0;
        let s = CellShape::of(&shape, 120);
        assert_eq!(s.radius, 5);
        assert_eq!(s.border, 3);
        assert_eq!(s.pad_y, 3, "2 * 1.25 = 2.5，四舍五入");
    }

    /// `border` 只加粗不取消 —— 0 是「没写」，不是「不要框」。
    #[test]
    fn a_zero_border_still_draws_one_pixel() {
        let mut shape = dg_skin::Shape::default();
        shape.border = 0.0;
        assert_eq!(CellShape::of(&shape, 96).border, 1);
    }

    /// `pad_y` 上下对称地缩块，块的中心不动 —— 文字在变矮的块里重新居中时
    /// 不会左右横跳（这条也是第一版缩文字框、结果一个像素都没挪的那个坑）。
    #[test]
    fn pad_y_shrinks_the_body_around_its_centre() {
        let base = CellShape::of(&dg_skin::Shape::default(), 96);
        let mut shape = dg_skin::Shape::default();
        shape.pad_y = 4.0;
        let padded = CellShape::of(&shape, 96);
        let a = base.body(0, 40, 100, 60);
        let b = padded.body(0, 40, 100, 60);
        assert_eq!(b.top - a.top, 4);
        assert_eq!(a.bottom - b.bottom, 4);
        assert_eq!(a.top + a.bottom, b.top + b.bottom, "中心不动");
    }
}
