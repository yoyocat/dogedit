//! `UiOp` 列表 → GDI 回放。
//!
//! 这是皮肤真正落到屏幕上的那一步：一份纯数据的 op 列表进来，一串 GDI 调用出去。
//! 它**不认识皮肤**：没有 Lua、没有 toml、没有文档、没有状态机。所以它可以脱离
//! 皮肤单独测 —— 把一份 op 列表画进一张 DIB，逐像素断言（见本文件末尾的测试）。
//!
//! 三条设计约束：
//!
//! 1. **不分配**。画刷和画笔走 [`super::gdicache`]，字体和位图由加载期建好、这里只
//!    按下标取。回放路径上一次 `Vec::new` 都没有（点集除外，那是调用方的表）。
//! 2. **不猜**。`shadow` 和带 `tint` 的 `image` 在纯 GDI 里做不出来（需要一张 32bpp
//!    的中间表面做 alpha 合成），所以它们被**跳过并计数**，而不是画一个走样的东西。
//!    加载期看到 `skipped > 0` 会提示一次。
//! 3. **坐标是 DIP**。op 里的矩形是逻辑像素，回放按 `ctx.dpi / 96` 放大到设备像素；
//!    取整只在最后一步做，四舍五入。
//!
//! 文字走 `DrawTextW`（不是 `TextOutW`）：对齐、垂直居中、裁剪、省略号它一次都管了，
//! 而 chrome 里每一处文字本来就是一个矩形。代价是文字按单行处理 —— 这是刻意的，
//! 页签标题、状态格、菜单行没有一处是多行的。

#![allow(unsafe_op_in_unsafe_fn)]
// 回放层先落地、后接线：调用方（配方加载器）在 M1 的下一块。在那之前只有本文件
// 的测试在用它，所以「非测试构建下没人用」是暂时的、已知的。
#![cfg_attr(not(test), allow(dead_code))]

use crate::win32::gdi::to_colorref;
use std::cell::RefCell;

use dg_render::pipeline::DrawList;
use dg_render::{Color, Rect};
use dg_skin::{Align, Dir, Fit, Point, TextSource, UiOp, VAlign};
#[cfg(test)]
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, DeleteDC, DrawTextW, Ellipse, FillRect, FrameRect, GetObjectW,
    GetStockObject, IntersectClipRect, LineTo, MoveToEx, NULL_BRUSH, NULL_PEN, PS_SOLID, Polygon,
    Polyline, RestoreDC, RoundRect, SRCCOPY, SaveDC, SelectObject, SetBkMode, SetTextColor,
    StretchBlt, BITMAP, DT_BOTTOM, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_TOP, DT_VCENTER, HBITMAP, HBRUSH, HDC, HFONT,
    HGDIOBJ, HPEN, TRANSPARENT,
};

thread_local! {
    /// 回放路径上唯一的两次分配都发生在这里，而且只发生一次：第一次用到时扩到
    /// 够大，之后每帧只是 `clear` + 填。文字和点集都靠它躲开每帧 `Vec::new`。
    static TEXT_BUF: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
    static POINT_BUF: RefCell<Vec<POINT>> = const { RefCell::new(Vec::new()) };
}

/// 回放一次的上下文：加载期建好的资源 + 当前部件那几段文字。
pub(super) struct ReplayCtx<'a> {
    /// 字体表，`FontId` 是它的下标。
    pub fonts: &'a [HFONT],
    /// 图片表，`ImageId` 是它的下标。
    pub images: &'a [HBITMAP],
    /// 字符串表，`TextSource::Literal` 是它的下标。
    pub strings: &'a [String],
    /// 点集表，`PointsId` 是它的一段。加载期建好，这里只借。
    pub points: &'a [Point],
    /// `@text`：这个部件当前的文字。
    pub text: &'a str,
    /// `@label`：部件的标签。
    pub label: &'a str,
    /// `@value`：部件的数值。
    pub value: &'a str,
    /// 该区域解析到的 DPI，坐标按它从 DIP 放大。
    pub dpi: i32,
}

impl ReplayCtx<'_> {
    fn scale(&self) -> f32 {
        if self.dpi > 0 {
            self.dpi as f32 / 96.0
        } else {
            1.0
        }
    }
}

/// 一次回放的账。`skipped` 是 GDI 画不出来的那几条（`shadow`、带 `tint` 的图片）。
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct ReplayStats {
    /// 列表里一共多少条。
    pub ops: usize,
    /// 真正画出来的条数。
    pub drawn: usize,
    /// 跳过的条数。
    pub skipped: usize,
}

/// 把一份 op 列表画进 `hdc`。
pub(super) unsafe fn replay(hdc: HDC, ops: &[UiOp], ctx: &ReplayCtx<'_>) -> ReplayStats {
    let mut stats = ReplayStats {
        ops: ops.len(),
        ..Default::default()
    };
    if ops.is_empty() {
        return stats;
    }
    let scale = ctx.scale();
    let _ = SetBkMode(hdc, TRANSPARENT);
    // 裁剪栈：`Clip` 存一次 DC，`Unclip` 弹一次。列表没配平也不能把 DC 弄坏，
    // 收尾时把多存的那几层弹掉。
    let mut clips = 0i32;
    // 图片用的内存 DC，第一条图片 op 才建。
    let mut mem = HDC::default();

    for op in ops {
        match *op {
            UiOp::Spacer => {}
            UiOp::Fill { rect, color } => {
                let rc = to_rect(rect, scale);
                let _ = FillRect(hdc, &rc, super::gdicache::brush(to_colorref(color)));
                stats.drawn += 1;
            }
            UiOp::Rect {
                rect,
                color,
                width,
            } => {
                let rc = to_rect(rect, scale);
                let brush = super::gdicache::brush(to_colorref(color));
                // `FrameRect` 只有 1 px，宽了就一层层往里画。
                let layers = ((width * scale).round() as i32).clamp(1, 8);
                let mut inner = rc;
                for _ in 0..layers {
                    if inner.right <= inner.left || inner.bottom <= inner.top {
                        break;
                    }
                    let _ = FrameRect(hdc, &inner, brush);
                    inner.left += 1;
                    inner.top += 1;
                    inner.right -= 1;
                    inner.bottom -= 1;
                }
                stats.drawn += 1;
            }
            UiOp::Round {
                rect,
                radius,
                fill,
                stroke,
                stroke_w,
            } => {
                let rc = to_rect(rect, scale);
                let brush = fill
                    .map(|c| super::gdicache::brush(to_colorref(c)))
                    .unwrap_or_else(|| HBRUSH(GetStockObject(NULL_BRUSH).0));
                let pen = stroke
                    .map(|c| {
                        let w = ((stroke_w * scale).round() as i32).max(1);
                        super::gdicache::pen(PS_SOLID, w, to_colorref(c))
                    })
                    .unwrap_or_else(|| HPEN(GetStockObject(NULL_PEN).0));
                let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
                let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
                let d = (radius * 2.0 * scale).round() as i32;
                let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, d, d);
                SelectObject(hdc, old_pen);
                SelectObject(hdc, old_brush);
                stats.drawn += 1;
            }
            UiOp::Line {
                a,
                b,
                color,
                width,
            } => {
                let w = ((width * scale).round() as i32).max(1);
                let pen = super::gdicache::pen(PS_SOLID, w, to_colorref(color));
                let old = SelectObject(hdc, HGDIOBJ(pen.0));
                let _ = MoveToEx(hdc, px(a.x, scale), px(a.y, scale), None);
                let _ = LineTo(hdc, px(b.x, scale), px(b.y, scale));
                SelectObject(hdc, old);
                stats.drawn += 1;
            }
            UiOp::Poly {
                points,
                color,
                width,
                closed,
            } => {
                // 点集在加载期的表里；越界就是皮肤坏了，跳过而不是画一半。
                let Some(src) = points.slice(ctx.points) else {
                    stats.skipped += 1;
                    continue;
                };
                let w = ((width * scale).round() as i32).max(1);
                let pen = super::gdicache::pen(PS_SOLID, w, to_colorref(color));
                let old = SelectObject(hdc, HGDIOBJ(pen.0));
                let ok = POINT_BUF.with(|buf| {
                    let mut buf = buf.borrow_mut();
                    buf.clear();
                    buf.extend(src.iter().map(|p| POINT {
                        x: px(p.x, scale),
                        y: px(p.y, scale),
                    }));
                    if closed {
                        let brush = super::gdicache::brush(to_colorref(color));
                        let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
                        let r = Polygon(hdc, &buf);
                        SelectObject(hdc, old_brush);
                        r
                    } else {
                        Polyline(hdc, &buf)
                    }
                });
                SelectObject(hdc, old);
                if ok.as_bool() {
                    stats.drawn += 1;
                } else {
                    stats.skipped += 1;
                }
            }
            UiOp::Circle {
                center,
                r,
                fill,
                stroke,
                stroke_w,
            } => {
                let brush = fill
                    .map(|c| super::gdicache::brush(to_colorref(c)))
                    .unwrap_or_else(|| HBRUSH(GetStockObject(NULL_BRUSH).0));
                let pen = stroke
                    .map(|c| {
                        let w = ((stroke_w * scale).round() as i32).max(1);
                        super::gdicache::pen(PS_SOLID, w, to_colorref(c))
                    })
                    .unwrap_or_else(|| HPEN(GetStockObject(NULL_PEN).0));
                let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
                let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
                let (cx, cy, rr) = (px(center.x, scale), px(center.y, scale), px(r, scale));
                let _ = Ellipse(hdc, cx - rr, cy - rr, cx + rr, cy + rr);
                SelectObject(hdc, old_pen);
                SelectObject(hdc, old_brush);
                stats.drawn += 1;
            }
            UiOp::Gradient {
                rect,
                from,
                to,
                dir,
            } => {
                gradient(hdc, rect, from, to, dir, scale);
                stats.drawn += 1;
            }
            UiOp::Text {
                rect,
                text,
                font,
                color,
                align,
                valign,
                clip,
                ellipsis,
            } => {
                let source = match text {
                    TextSource::Literal(id) => ctx.strings.get(id.0 as usize).map(|s| s.as_str()),
                    TextSource::Text => Some(ctx.text),
                    TextSource::Label => Some(ctx.label),
                    TextSource::Value => Some(ctx.value),
                };
                let Some(source) = source else {
                    stats.skipped += 1;
                    continue;
                };
                let mut rc = to_rect(rect, scale);
                if let Some(c) = clip {
                    let c = to_rect(c, scale);
                    rc.right = rc.right.min(c.right);
                    rc.bottom = rc.bottom.min(c.bottom);
                    rc.left = rc.left.max(c.left);
                    rc.top = rc.top.max(c.top);
                }
                if rc.right <= rc.left || rc.bottom <= rc.top || source.is_empty() {
                    stats.skipped += 1;
                    continue;
                }
                let old_font = match ctx.fonts.get(font.0 as usize) {
                    Some(handle) if !handle.0.is_null() => {
                        Some(SelectObject(hdc, HGDIOBJ(handle.0)))
                    }
                    _ => None,
                };
                let _ = SetTextColor(hdc, to_colorref(color));
                let mut flags = DT_SINGLELINE | DT_NOPREFIX;
                flags |= match align {
                    Align::Start => DT_LEFT,
                    Align::Center => DT_CENTER,
                    Align::End => DT_RIGHT,
                };
                flags |= match valign {
                    VAlign::Top => DT_TOP,
                    VAlign::Middle => DT_VCENTER,
                    VAlign::Bottom => DT_BOTTOM,
                };
                if ellipsis {
                    flags |= DT_END_ELLIPSIS;
                }
                TEXT_BUF.with(|buf| {
                    let mut buf = buf.borrow_mut();
                    buf.clear();
                    buf.extend(source.encode_utf16());
                    let _ = DrawTextW(hdc, &mut buf, &mut rc, flags);
                });
                if let Some(old) = old_font {
                    SelectObject(hdc, old);
                }
                stats.drawn += 1;
            }
            UiOp::Image {
                rect,
                src,
                tint,
                fit,
            } => {
                let Some(bitmap) = ctx.images.get(src.0 as usize).copied() else {
                    stats.skipped += 1;
                    continue;
                };
                // 着色要一次 alpha 合成，纯 GDI 做不了。
                if tint.is_some() {
                    stats.skipped += 1;
                    continue;
                }
                let Some((bw, bh)) = bitmap_size(bitmap) else {
                    stats.skipped += 1;
                    continue;
                };
                let rc = to_rect(rect, scale);
                let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
                if w <= 0 || h <= 0 || bw <= 0 || bh <= 0 {
                    stats.skipped += 1;
                    continue;
                }
                if mem.0.is_null() {
                    mem = CreateCompatibleDC(Some(hdc));
                    if mem.0.is_null() {
                        stats.skipped += 1;
                        continue;
                    }
                }
                let old = SelectObject(mem, HGDIOBJ(bitmap.0));
                match fit {
                    Fit::Stretch => {
                        let _ = StretchBlt(
                            hdc, rc.left, rc.top, w, h, Some(mem), 0, 0, bw, bh, SRCCOPY,
                        );
                    }
                    Fit::Center => {
                        // 原始大小居中；图比框大时按框裁，不许溢出。
                        let saved = SaveDC(hdc);
                        let _ = IntersectClipRect(hdc, rc.left, rc.top, rc.right, rc.bottom);
                        let _ = BitBlt(
                            hdc,
                            rc.left + (w - bw) / 2,
                            rc.top + (h - bh) / 2,
                            bw,
                            bh,
                            Some(mem),
                            0,
                            0,
                            SRCCOPY,
                        );
                        let _ = RestoreDC(hdc, saved);
                    }
                    Fit::Tile => {
                        // 平铺要按格切，最后一行/一列不能爬出框外。
                        let saved = SaveDC(hdc);
                        let _ = IntersectClipRect(hdc, rc.left, rc.top, rc.right, rc.bottom);
                        let mut y = rc.top;
                        while y < rc.bottom {
                            let mut x = rc.left;
                            while x < rc.right {
                                let _ = BitBlt(hdc, x, y, bw, bh, Some(mem), 0, 0, SRCCOPY);
                                x += bw;
                            }
                            y += bh;
                        }
                        let _ = RestoreDC(hdc, saved);
                    }
                }
                SelectObject(mem, old);
                stats.drawn += 1;
            }
            UiOp::Clip { rect } => {
                let rc = to_rect(rect, scale);
                SaveDC(hdc);
                clips += 1;
                let _ = IntersectClipRect(hdc, rc.left, rc.top, rc.right, rc.bottom);
                stats.drawn += 1;
            }
            UiOp::Unclip => {
                if clips > 0 {
                    let _ = RestoreDC(hdc, -1);
                    clips -= 1;
                }
                stats.drawn += 1;
            }
            UiOp::Shadow { .. } => {
                // 外阴影要一张 32bpp 中间表面做 alpha 合成，纯 GDI 画不出来。
                stats.skipped += 1;
            }
        }
    }

    // 列表没配平：把多存的那几层弹掉，别把 DC 留给下一个人。
    while clips > 0 {
        let _ = RestoreDC(hdc, -1);
        clips -= 1;
    }
    if !mem.0.is_null() {
        let _ = DeleteDC(mem);
    }
    stats
}

/// 把一份 op 列表画进 [`DrawList`] —— 编辑区的 `editor.overlay` 走这条。
///
/// 为什么不能和上面共用一条路：编辑区的像素**没有 DC**。它整屏由合成表面
/// （`DrawCmd`）出，GDI 那条路只在合成表面建不起来时兜底填一次底色。所以这里
/// 只能认能映射到 `DrawCmd` 的三种 op：
///
/// | op | 落到哪 | 说明 |
/// |---|---|---|
/// | `fill` | `FillRect` | 完全等价 |
/// | `rect` | 四条 `FillRect` | 边框宽度按 `width`，至少 1 DIP |
/// | `text` | `TextRun` | 用编辑器自己的字体，**只认矩形的左上角** |
///
/// 其余（`round`/`line`/`poly`/`circle`/`gradient`/`image`/`clip`/`shadow`）记进
/// `skipped`，由调用方报一次 —— 画不出来就说出来，别让人以为画了。
///
/// `text` 的三处折扣是这条路的天花板：`DrawCmd::TextRun` 只有「在哪儿、写什么、
/// 什么颜色」，没有字体、对齐、省略号。要真正的排版得给 `TextRun` 加字段，那是
/// 另一件事；`editor.overlay` 的常见用法（行底色、边标记、blame 小字）不需要。
pub(super) fn replay_list(ops: &[UiOp], ctx: &ReplayCtx<'_>, draw: &mut DrawList) -> ReplayStats {
    let mut stats = ReplayStats {
        ops: ops.len(),
        ..Default::default()
    };
    let scale = ctx.scale();
    for op in ops {
        match op {
            UiOp::Fill { rect, color } => {
                draw.fill(*rect, *color);
                stats.drawn += 1;
            }
            UiOp::Rect { rect, color, width } => {
                let w = (width * scale).max(1.0);
                let (x, y, rw, rh) = (rect.x, rect.y, rect.w, rect.h);
                if rw <= 0.0 || rh <= 0.0 {
                    stats.skipped += 1;
                    continue;
                }
                draw.fill(Rect::new(x, y, rw, w), *color);
                draw.fill(Rect::new(x, y + rh - w, rw, w), *color);
                // 两条竖边去掉已经画过的角，避免半透明色在角上叠两次。
                let inner_h = (rh - 2.0 * w).max(0.0);
                if inner_h > 0.0 {
                    draw.fill(Rect::new(x, y + w, w, inner_h), *color);
                    draw.fill(Rect::new(x + rw - w, y + w, w, inner_h), *color);
                }
                stats.drawn += 1;
            }
            UiOp::Text {
                rect, text, color, ..
            } => {
                let source = match text {
                    TextSource::Literal(id) => ctx.strings.get(id.0 as usize).map(|s| s.as_str()),
                    TextSource::Text => Some(ctx.text),
                    TextSource::Label => Some(ctx.label),
                    TextSource::Value => Some(ctx.value),
                };
                let Some(source) = source.filter(|s| !s.is_empty()) else {
                    stats.skipped += 1;
                    continue;
                };
                if rect.w <= 0.0 || rect.h <= 0.0 {
                    stats.skipped += 1;
                    continue;
                }
                draw.text(rect.x, rect.y, source, *color);
                stats.drawn += 1;
            }
            _ => stats.skipped += 1,
        }
    }
    stats
}

/// 渐变：按像素带一层层填。`GradientFill` 的结果随驱动走，这里要的是可复现。
///
/// 带数上限 256 —— 再高肉眼看不出差别，但每带一次 `FillRect`。
unsafe fn gradient(hdc: HDC, rect: Rect, from: Color, to: Color, dir: Dir, scale: f32) {
    let rc = to_rect(rect, scale);
    let (len, vertical) = match dir {
        Dir::V => (rc.bottom - rc.top, true),
        Dir::H => (rc.right - rc.left, false),
    };
    if len <= 0 {
        return;
    }
    let bands = len.clamp(1, 256);
    for i in 0..bands {
        let t = (i as f32 + 0.5) / bands as f32;
        let color = mix(from, to, t);
        let brush = super::gdicache::brush(to_colorref(color));
        let band = if vertical {
            let top = rc.top + i * len / bands;
            let bottom = rc.top + (i + 1) * len / bands;
            RECT {
                top,
                bottom: bottom.max(top + 1),
                ..rc
            }
        } else {
            let left = rc.left + i * len / bands;
            let right = rc.left + (i + 1) * len / bands;
            RECT {
                left,
                right: right.max(left + 1),
                ..rc
            }
        };
        let _ = FillRect(hdc, &band, brush);
    }
}

/// 两色按 `t` 线性插值，四个通道都插。
fn mix(from: Color, to: Color, t: f32) -> Color {
    let lerp = |a: u8, b: u8| -> u8 {
        let a = a as f32;
        let b = b as f32;
        (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
    };
    Color {
        r: lerp(from.r, to.r),
        g: lerp(from.g, to.g),
        b: lerp(from.b, to.b),
        a: lerp(from.a, to.a),
    }
}

/// DIP → 设备像素，四舍五入。
fn px(value: f32, scale: f32) -> i32 {
    (value * scale).round() as i32
}

/// DIP 矩形 → GDI 矩形。右/下边界按左/上同样的方式取整，保证宽度不抖。
fn to_rect(rect: Rect, scale: f32) -> RECT {
    RECT {
        left: px(rect.x, scale),
        top: px(rect.y, scale),
        right: px(rect.x + rect.w, scale),
        bottom: px(rect.y + rect.h, scale),
    }
}

/// 位图的原始像素尺寸。`Fit::Stretch` 要它当源矩形，`Fit::Center`/`Fit::Tile`
/// 要它当步长 —— 都不能拿目标矩形代替（那会从图里切一块出来，或者平铺成一条）。
unsafe fn bitmap_size(bitmap: HBITMAP) -> Option<(i32, i32)> {
    let mut bm = BITMAP::default();
    let n = GetObjectW(
        HGDIOBJ(bitmap.0),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut core::ffi::c_void),
    );
    if n == 0 {
        None
    } else {
        Some((bm.bmWidth, bm.bmHeight))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_skin::{FontId, ImageId};
    use windows::Win32::Graphics::Gdi::{
        CreateDIBSection, DeleteObject, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    };

    /// 一张可以逐像素读的画布：32bpp、自上而下、BGRA。
    struct Canvas {
        hdc: HDC,
        bitmap: HBITMAP,
        bits: *mut u8,
        w: i32,
        h: i32,
    }

    impl Canvas {
        fn new(w: i32, h: i32) -> Self {
            unsafe {
                let mut info = BITMAPINFO::default();
                info.bmiHeader = BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                };
                let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
                let bitmap =
                    CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)
                        .expect("dib");
                let hdc = CreateCompatibleDC(None);
                assert!(!hdc.0.is_null());
                SelectObject(hdc, HGDIOBJ(bitmap.0));
                // 先铺一层已知底色，好让「没画」和「画了黑」区分开。
                let bg = super::super::gdicache::brush(COLORREF(0x0010_1010));
                let rc = RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                };
                let _ = FillRect(hdc, &rc, bg);
                Self {
                    hdc,
                    bitmap,
                    bits: bits as *mut u8,
                    w,
                    h,
                }
            }
        }

        /// (r, g, b)，自左上起算。
        fn at(&self, x: i32, y: i32) -> (u8, u8, u8) {
            assert!(x >= 0 && y >= 0 && x < self.w && y < self.h, "out of canvas");
            let at = ((y * self.w + x) * 4) as usize;
            unsafe {
                let p = self.bits.add(at);
                (*p.add(2), *p.add(1), *p)
            }
        }

        fn count_not(&self, rgb: (u8, u8, u8)) -> usize {
            let mut n = 0;
            for y in 0..self.h {
                for x in 0..self.w {
                    if self.at(x, y) != rgb {
                        n += 1;
                    }
                }
            }
            n
        }
    }

    impl Drop for Canvas {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
                let _ = DeleteDC(self.hdc);
            }
        }
    }

    const BG: (u8, u8, u8) = (16, 16, 16);

    fn ctx<'a>(fonts: &'a [HFONT], strings: &'a [String]) -> ReplayCtx<'a> {
        ReplayCtx {
            fonts,
            images: &[],
            strings,
            points: &[],
            text: "行 12，列 34",
            label: "打开(&O)",
            value: "Ctrl+O",
            dpi: 96,
        }
    }

    /// 实心填充落在它自己的矩形里，别处一个像素都不动。
    #[test]
    fn a_fill_stays_inside_its_rect() {
        let canvas = Canvas::new(40, 30);
        let ops = [UiOp::Fill {
            rect: Rect::new(8.0, 6.0, 10.0, 5.0),
            color: Color::rgb(255, 0, 0),
        }];
        let strings: Vec<String> = Vec::new();
        let stats = unsafe { replay(canvas.hdc, &ops, &ctx(&[], &strings)) };
        assert_eq!(stats, ReplayStats { ops: 1, drawn: 1, skipped: 0 });
        assert_eq!(canvas.at(8, 6), (255, 0, 0));
        assert_eq!(canvas.at(17, 10), (255, 0, 0), "the far corner is inside");
        assert_eq!(canvas.at(18, 10), BG, "one past the right edge");
        assert_eq!(canvas.at(8, 11), BG, "one past the bottom edge");
        assert_eq!(canvas.at(7, 6), BG, "one before the left edge");
    }

    /// 1 px 的边框只画边，中间留着底色。
    #[test]
    fn a_frame_draws_its_edge_only() {
        let canvas = Canvas::new(40, 30);
        let ops = [UiOp::Rect {
            rect: Rect::new(4.0, 4.0, 20.0, 12.0),
            color: Color::rgb(0, 255, 0),
            width: 1.0,
        }];
        let strings: Vec<String> = Vec::new();
        unsafe { replay(canvas.hdc, &ops, &ctx(&[], &strings)) };
        assert_eq!(canvas.at(4, 4), (0, 255, 0), "top-left");
        assert_eq!(canvas.at(23, 4), (0, 255, 0), "top-right");
        assert_eq!(canvas.at(4, 15), (0, 255, 0), "bottom-left");
        assert_eq!(canvas.at(12, 9), BG, "the middle is untouched");
    }

    /// 渐变：两端各自接近 from/to，中间夹在两者之间。
    #[test]
    fn a_gradient_runs_from_one_colour_to_the_other() {
        let canvas = Canvas::new(20, 64);
        let ops = [UiOp::Gradient {
            rect: Rect::new(0.0, 0.0, 20.0, 64.0),
            from: Color::rgb(0, 0, 0),
            to: Color::rgb(200, 200, 200),
            dir: Dir::V,
        }];
        let strings: Vec<String> = Vec::new();
        unsafe { replay(canvas.hdc, &ops, &ctx(&[], &strings)) };
        let (top, _, _) = canvas.at(10, 0);
        let (mid, _, _) = canvas.at(10, 32);
        let (bottom, _, _) = canvas.at(10, 63);
        assert!(top < 12, "the top is near black, got {top}");
        assert!(bottom > 188, "the bottom is near the far colour, got {bottom}");
        assert!(top < mid && mid < bottom, "monotone: {top} {mid} {bottom}");
    }

    /// 裁剪真的裁住了：填充超出裁剪框的部分不许出现。
    #[test]
    fn a_clip_holds_the_fill_back() {
        let canvas = Canvas::new(40, 30);
        let ops = [
            UiOp::Clip {
                rect: Rect::new(10.0, 10.0, 10.0, 10.0),
            },
            UiOp::Fill {
                rect: Rect::new(0.0, 0.0, 40.0, 30.0),
                color: Color::rgb(0, 0, 255),
            },
            UiOp::Unclip,
            UiOp::Fill {
                rect: Rect::new(30.0, 2.0, 5.0, 5.0),
                color: Color::rgb(255, 255, 0),
            },
        ];
        let strings: Vec<String> = Vec::new();
        let stats = unsafe { replay(canvas.hdc, &ops, &ctx(&[], &strings)) };
        assert_eq!(stats.skipped, 0);
        assert_eq!(canvas.at(10, 10), (0, 0, 255), "inside the clip");
        assert_eq!(canvas.at(19, 19), (0, 0, 255), "inside the clip");
        assert_eq!(canvas.at(9, 10), BG, "left of the clip");
        assert_eq!(canvas.at(20, 19), BG, "right of the clip");
        assert_eq!(canvas.at(32, 4), (255, 255, 0), "after unclip");
    }

    /// 没配平的 `Clip` 不许把 DC 的裁剪状态漏给下一个人。
    #[test]
    fn an_unbalanced_clip_is_popped_at_the_end() {
        let canvas = Canvas::new(20, 20);
        let ops = [UiOp::Clip {
            rect: Rect::new(0.0, 0.0, 2.0, 2.0),
        }];
        let strings: Vec<String> = Vec::new();
        unsafe { replay(canvas.hdc, &ops, &ctx(&[], &strings)) };
        // 回放结束后自己画一个满幅填充，还能铺满 —— 说明裁剪被弹掉了。
        let brush = unsafe { super::super::gdicache::brush(COLORREF(0x00FF_FFFF)) };
        let rc = RECT {
            left: 0,
            top: 0,
            right: 20,
            bottom: 20,
        };
        unsafe { FillRect(canvas.hdc, &rc, brush) };
        assert_eq!(canvas.at(15, 15), (255, 255, 255));
    }

    /// 文字画在它的矩形里，不出界；占位符取的是 ctx 里那一段。
    #[test]
    fn text_stays_in_its_box_and_reads_the_placeholder() {
        let font = unsafe { crate::win32::uifont::create(96) };
        assert!(!font.0.is_null());
        let fonts = [font];
        let strings: Vec<String> = Vec::new();
        let canvas = Canvas::new(120, 40);
        let ops = [UiOp::Text {
            rect: Rect::new(4.0, 4.0, 100.0, 20.0),
            text: TextSource::Text,
            font: FontId(0),
            color: Color::rgb(255, 255, 255),
            align: Align::Start,
            valign: VAlign::Middle,
            clip: None,
            ellipsis: false,
        }];
        let stats = unsafe { replay(canvas.hdc, &ops, &ctx(&fonts, &strings)) };
        assert_eq!(stats.skipped, 0);
        let inside = {
            let mut n = 0;
            for y in 4..24 {
                for x in 4..104 {
                    if canvas.at(x, y) != BG {
                        n += 1;
                    }
                }
            }
            n
        };
        assert!(inside > 20, "the text drew {inside} pixels");
        // 盒子外面（右边、下边）一个像素都不许有。
        for y in 0..40 {
            for x in 104..120 {
                assert_eq!(canvas.at(x, y), BG, "ink at ({x},{y}) outside the box");
            }
        }
        for x in 0..120 {
            for y in 24..40 {
                assert_eq!(canvas.at(x, y), BG, "ink at ({x},{y}) below the box");
            }
        }
        unsafe {
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }

    /// 文字比盒子宽时按省略号截断，仍然不出界。
    #[test]
    fn an_ellipsis_keeps_the_text_inside() {
        let font = unsafe { crate::win32::uifont::create(96) };
        let fonts = [font];
        let strings: Vec<String> = Vec::new();
        let canvas = Canvas::new(60, 30);
        let ops = [UiOp::Text {
            rect: Rect::new(0.0, 0.0, 40.0, 18.0),
            text: TextSource::Label,
            font: FontId(0),
            color: Color::rgb(255, 255, 255),
            align: Align::Start,
            valign: VAlign::Middle,
            clip: None,
            ellipsis: true,
        }];
        unsafe { replay(canvas.hdc, &ops, &ctx(&fonts, &strings)) };
        for y in 0..30 {
            for x in 40..60 {
                assert_eq!(canvas.at(x, y), BG, "ink at ({x},{y}) past the ellipsis box");
            }
        }
        unsafe {
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }

    /// 闭合多边形把内部填满；顶点表越界就跳过，不画半条边。
    #[test]
    fn a_closed_poly_fills_its_interior() {
        let points = [
            Point::new(10.0, 0.0),
            Point::new(20.0, 18.0),
            Point::new(0.0, 18.0),
        ];
        let canvas = Canvas::new(24, 24);
        let ops = [UiOp::Poly {
            points: dg_skin::PointsId { start: 0, len: 3 },
            color: Color::rgb(0, 255, 0),
            width: 1.0,
            closed: true,
        }];
        let strings: Vec<String> = Vec::new();
        let mut c = ctx(&[], &strings);
        c.points = &points;
        let stats = unsafe { replay(canvas.hdc, &ops, &c) };
        assert_eq!(stats.drawn, 1);
        assert_eq!(canvas.at(10, 15), (0, 255, 0), "inside, near the base");
        assert_eq!(canvas.at(1, 1), BG, "outside the triangle");

        // 同一个点集，要 9 个点却只有 3 个 —— 皮肤写坏了，跳过。
        let bad = [UiOp::Poly {
            points: dg_skin::PointsId { start: 0, len: 9 },
            color: Color::rgb(0, 255, 0),
            width: 1.0,
            closed: true,
        }];
        let before = canvas.count_not(BG);
        let stats = unsafe { replay(canvas.hdc, &bad, &c) };
        assert_eq!(stats.skipped, 1);
        assert_eq!(canvas.count_not(BG), before, "nothing more was drawn");
    }

    /// 一张 4x4 的实色位图，返回位图和它的像素起点（好让调用方填色）。
    fn solid_bitmap(w: i32, h: i32, rgb: (u8, u8, u8)) -> (HBITMAP, *mut u8) {
        unsafe {
            let mut info = BITMAPINFO::default();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)
                .expect("dib");
            let p = bits as *mut u8;
            for i in 0..(w * h) as usize {
                *p.add(i * 4) = rgb.2;
                *p.add(i * 4 + 1) = rgb.1;
                *p.add(i * 4 + 2) = rgb.0;
            }
            (bitmap, p)
        }
    }

    /// `Fit::Center` 按位图的原始尺寸居中，不是拉满目标框。
    #[test]
    fn a_centered_image_keeps_its_own_size() {
        let (bitmap, _) = solid_bitmap(4, 4, (255, 0, 0));
        let images = [bitmap];
        let canvas = Canvas::new(20, 20);
        let ops = [UiOp::Image {
            rect: Rect::new(0.0, 0.0, 20.0, 20.0),
            src: ImageId(0),
            tint: None,
            fit: Fit::Center,
        }];
        let strings: Vec<String> = Vec::new();
        let mut c = ctx(&[], &strings);
        c.images = &images;
        let stats = unsafe { replay(canvas.hdc, &ops, &c) };
        assert_eq!(stats.drawn, 1);
        assert_eq!(canvas.at(8, 8), (255, 0, 0), "top-left of the 4x4 block");
        assert_eq!(canvas.at(11, 11), (255, 0, 0), "bottom-right of the block");
        assert_eq!(canvas.at(7, 8), BG, "one pixel left of the block");
        assert_eq!(canvas.at(12, 11), BG, "one pixel right of the block");
        assert_eq!(canvas.at(0, 0), BG, "the corner is not stretched");
        unsafe {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
    }

    /// `Fit::Tile` 按位图尺寸铺满，最后一格被框裁掉，不爬出去。
    #[test]
    fn a_tiled_image_is_cut_at_the_box() {
        let (bitmap, _) = solid_bitmap(4, 4, (0, 0, 255));
        let images = [bitmap];
        let canvas = Canvas::new(24, 24);
        let ops = [UiOp::Image {
            rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            src: ImageId(0),
            tint: None,
            fit: Fit::Tile,
        }];
        let strings: Vec<String> = Vec::new();
        let mut c = ctx(&[], &strings);
        c.images = &images;
        let stats = unsafe { replay(canvas.hdc, &ops, &c) };
        assert_eq!(stats.drawn, 1);
        // 10x10 的框里一格都不剩（4 的倍数铺不满，最后一行/列被裁）。
        for y in 0..10 {
            for x in 0..10 {
                assert_eq!(canvas.at(x, y), (0, 0, 255), "hole at ({x},{y})");
            }
        }
        assert_eq!(canvas.at(10, 10), BG, "the tile stops at the box");
        unsafe {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
    }

    /// 画不出来的两条（阴影、带着色的图片）跳过并计数，不画走样的东西。
    #[test]
    fn what_gdi_cannot_draw_is_counted_not_faked() {
        let canvas = Canvas::new(30, 30);
        let ops = [
            UiOp::Shadow {
                rect: Rect::new(4.0, 4.0, 20.0, 20.0),
                radius: 4.0,
                color: Color::rgba(0, 0, 0, 80),
                blur: 6.0,
            },
            UiOp::Image {
                rect: Rect::new(0.0, 0.0, 10.0, 10.0),
                src: ImageId(0),
                tint: Some(Color::rgb(255, 0, 0)),
                fit: Fit::Stretch,
            },
        ];
        let strings: Vec<String> = Vec::new();
        let stats = unsafe { replay(canvas.hdc, &ops, &ctx(&[], &strings)) };
        assert_eq!(stats.skipped, 2);
        assert_eq!(stats.drawn, 0);
        assert_eq!(canvas.count_not(BG), 0, "nothing was painted");
    }

    /// 空列表什么都不做，也不碰 DC。
    #[test]
    fn an_empty_list_is_a_no_op() {
        let canvas = Canvas::new(8, 8);
        let strings: Vec<String> = Vec::new();
        let stats = unsafe { replay(canvas.hdc, &[], &ctx(&[], &strings)) };
        assert_eq!(stats, ReplayStats::default());
        assert_eq!(canvas.count_not(BG), 0);
    }

    /// 字面量从字符串表取；表里没有就跳过，不画空白。
    #[test]
    fn a_literal_comes_from_the_string_table() {
        let font = unsafe { crate::win32::uifont::create(96) };
        let fonts = [font];
        let strings = vec!["OK".to_string()];
        let canvas = Canvas::new(60, 30);
        let good = [UiOp::Text {
            rect: Rect::new(0.0, 0.0, 50.0, 20.0),
            text: TextSource::Literal(dg_skin::TextId(0)),
            font: FontId(0),
            color: Color::rgb(255, 255, 255),
            align: Align::Start,
            valign: VAlign::Middle,
            clip: None,
            ellipsis: false,
        }];
        let stats = unsafe { replay(canvas.hdc, &good, &ctx(&fonts, &strings)) };
        assert_eq!(stats.drawn, 1);
        assert!(canvas.count_not(BG) > 5, "the literal drew something");

        let missing = [UiOp::Text {
            rect: Rect::new(0.0, 0.0, 50.0, 20.0),
            text: TextSource::Literal(dg_skin::TextId(9)),
            font: FontId(0),
            color: Color::rgb(255, 255, 255),
            align: Align::Start,
            valign: VAlign::Middle,
            clip: None,
            ellipsis: false,
        }];
        let before = canvas.count_not(BG);
        let stats = unsafe { replay(canvas.hdc, &missing, &ctx(&fonts, &strings)) };
        assert_eq!(stats.skipped, 1);
        assert_eq!(canvas.count_not(BG), before, "nothing more was drawn");
        unsafe {
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }
}
