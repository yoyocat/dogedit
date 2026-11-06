//! 绘制路径上的 GDI 对象缓存：画刷与画笔。
//!
//! 每个绘制函数原先都在自己的循环里 `CreateSolidBrush` + `FillRect` +
//! `DeleteObject`：菜单每画一行建一两个画刷，状态栏每个分格建一个，页签条每次
//! 重画建两个。这些对象的**键只有颜色**，值在整个进程里都不变，建一次就够 ——
//! 而它们偏偏落在最热的路径上：菜单的每一行、滚动条淡入淡出的每一帧。
//!
//! 缓存是**线程本地**的。GDI 对象属于创建它的线程，绘制全在 UI 线程发生，
//! 别的线程要画就自己建一份，这样不需要锁，也不会跨线程选对象。
//!
//! 表是定长的直接映射表，满了就顶掉那一格（`DeleteObject` 旧的）。主题调色板
//! 只有几十种颜色，正常永远撞不满；撞上了也只是重建一次，比 LRU 便宜。
//!
//! 契约：**拿到的句柄不能删**，也不能在返回时还留在 DC 里
//! （`FillRect` 不改变 DC 的选中对象，`SelectObject` 用过的要还回去）。

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, HGDIOBJ, HPEN, PS_SOLID,
    PEN_STYLE, RoundRect, SelectObject,
};

/// 画刷表的格数。必须是 2 的幂。
const BRUSH_SLOTS: usize = 256;

/// 画笔表的格数。必须是 2 的幂。
const PEN_SLOTS: usize = 128;

const NULL_BRUSH: HBRUSH = HBRUSH(std::ptr::null_mut());
const NULL_PEN: HPEN = HPEN(std::ptr::null_mut());

/// 一格画刷。`handle` 为空表示这一格没被用过。
#[derive(Clone, Copy)]
struct BrushSlot {
    color: u32,
    handle: HBRUSH,
}

/// 一格画笔。键是三元组：线型、线宽、颜色。
#[derive(Clone, Copy)]
struct PenSlot {
    style: i32,
    width: i32,
    color: u32,
    handle: HPEN,
}

const EMPTY_BRUSH: BrushSlot = BrushSlot {
    color: 0,
    handle: NULL_BRUSH,
};

const EMPTY_PEN: PenSlot = PenSlot {
    style: 0,
    width: 0,
    color: 0,
    handle: NULL_PEN,
};

struct BrushCache {
    slots: [BrushSlot; BRUSH_SLOTS],
}

struct PenCache {
    slots: [PenSlot; PEN_SLOTS],
}

impl BrushCache {
    const fn new() -> Self {
        Self {
            slots: [EMPTY_BRUSH; BRUSH_SLOTS],
        }
    }

    /// 直接映射：颜色乘黄金比例常数取高位，低 8 位当格号。
    fn index(color: u32) -> usize {
        (color.wrapping_mul(0x9E37_79B1) >> 24) as usize & (BRUSH_SLOTS - 1)
    }

    fn get(&mut self, color: COLORREF) -> HBRUSH {
        let at = Self::index(color.0);
        let slot = &mut self.slots[at];
        if !slot.handle.0.is_null() && slot.color == color.0 {
            return slot.handle;
        }
        // 顶掉的那一格可能是别的颜色，它没有别人在用（我们从不把缓存句柄留
        // 在 DC 里），可以直接删。
        unsafe {
            let made = CreateSolidBrush(color);
            if made.0.is_null() {
                return NULL_BRUSH;
            }
            if !slot.handle.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(slot.handle.0));
            }
            *slot = BrushSlot {
                color: color.0,
                handle: made,
            };
            made
        }
    }
}

impl PenCache {
    fn new() -> Self {
        Self {
            slots: [EMPTY_PEN; PEN_SLOTS],
        }
    }

    /// 画笔的键有三个分量，散列把它们一起拌进去。
    fn index(style: i32, width: i32, color: u32) -> usize {
        let mixed = color
            .wrapping_mul(0x9E37_79B1)
            .wrapping_add((style as u32).wrapping_mul(0x85EB_CA6B))
            .wrapping_add((width as u32).wrapping_mul(0xC2B2_AE35));
        (mixed >> 24) as usize & (PEN_SLOTS - 1)
    }

    fn get(&mut self, style: PEN_STYLE, width: i32, color: COLORREF) -> HPEN {
        let at = Self::index(style.0, width, color.0);
        let slot = &mut self.slots[at];
        if !slot.handle.0.is_null()
            && slot.style == style.0
            && slot.width == width
            && slot.color == color.0
        {
            return slot.handle;
        }
        unsafe {
            let made = CreatePen(style, width, color);
            if made.0.is_null() {
                return NULL_PEN;
            }
            if !slot.handle.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(slot.handle.0));
            }
            *slot = PenSlot {
                style: style.0,
                width,
                color: color.0,
                handle: made,
            };
            made
        }
    }
}

impl Drop for BrushCache {
    fn drop(&mut self) {
        for slot in &self.slots {
            if !slot.handle.0.is_null() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(slot.handle.0));
                }
            }
        }
    }
}

impl Drop for PenCache {
    fn drop(&mut self) {
        for slot in &self.slots {
            if !slot.handle.0.is_null() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(slot.handle.0));
                }
            }
        }
    }
}

thread_local! {
    static BRUSHES: RefCell<BrushCache> = RefCell::new(BrushCache::new());
    static PENS: RefCell<PenCache> = RefCell::new(PenCache::new());
}

/// 这个颜色的实心画刷。**不能删**，也不要在返回时还留在 DC 里。
pub(super) unsafe fn brush(color: COLORREF) -> HBRUSH {
    BRUSHES.with(|c| c.borrow_mut().get(color))
}

/// 这个线型/线宽/颜色的画笔。同样不能删。
pub(super) unsafe fn pen(style: PEN_STYLE, width: i32, color: COLORREF) -> HPEN {
    PENS.with(|c| c.borrow_mut().get(style, width, color))
}

/// 填一块矩形；`radius > 0` 时四角磨圆（半径是**设备像素**）。
///
/// `radius <= 0` 走的就是 `FillRect` —— 配方没写圆角时和以前逐像素一致，
/// 这条分支是「空皮肤 0 像素」那条验收线的落点。
///
/// 圆角用 `RoundRect`，它的椭圆宽高是**直径**，所以传 `radius * 2`；它同时用
/// 当前画笔画边，所以画笔也换成同一个颜色（比「换成空画笔再换回来」少两次
/// `SelectObject`，而且不会在边上留一圈黑）。
pub(super) unsafe fn fill_round(hdc: HDC, rc: &RECT, color: COLORREF, radius: i32) {
    // edition 2024 里 `unsafe fn` 的函数体默认是安全的，GDI 调用得自己圈。
    unsafe {
        if radius <= 0 {
            FillRect(hdc, rc, brush(color));
            return;
        }
        let saved_brush = SelectObject(hdc, HGDIOBJ(brush(color).0));
        let saved_pen = SelectObject(hdc, HGDIOBJ(pen(PS_SOLID, 1, color).0));
        let _ = RoundRect(
            hdc,
            rc.left,
            rc.top,
            rc.right,
            rc.bottom,
            radius * 2,
            radius * 2,
        );
        SelectObject(hdc, saved_pen);
        SelectObject(hdc, saved_brush);
    }
}

/// 丢掉本线程缓存的所有对象。主题整套换掉时调一次，把旧调色板还回去 ——
/// 不调也不会错，只是旧颜色会一直占着格子，直到被同格的其它颜色顶掉。
pub(super) fn clear() {
    BRUSHES.with(|c| *c.borrow_mut() = BrushCache::new());
    PENS.with(|c| *c.borrow_mut() = PenCache::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同一个颜色两次拿到的是同一个句柄 —— 这才是缓存的意义。
    #[test]
    fn one_colour_one_brush() {
        unsafe {
            let a = brush(COLORREF(0x0012_3456));
            let b = brush(COLORREF(0x0012_3456));
            assert!(!a.0.is_null());
            assert_eq!(a.0, b.0, "the same colour is the same brush");
            let c = brush(COLORREF(0x0012_3457));
            assert_ne!(a.0, c.0, "a different colour is a different brush");
        }
    }

    /// 画笔的键是三元组：只差线宽也算不同的对象。
    #[test]
    fn a_pen_is_keyed_by_style_width_and_colour() {
        use windows::Win32::Graphics::Gdi::{PS_NULL, PS_SOLID};
        unsafe {
            let thin = pen(PS_SOLID, 1, COLORREF(0x00FF_0000));
            let thick = pen(PS_SOLID, 2, COLORREF(0x00FF_0000));
            let again = pen(PS_SOLID, 1, COLORREF(0x00FF_0000));
            let null = pen(PS_NULL, 1, COLORREF(0x00FF_0000));
            assert!(!thin.0.is_null());
            assert_eq!(thin.0, again.0, "same key, same pen");
            assert_ne!(thin.0, thick.0, "width is part of the key");
            assert_ne!(thin.0, null.0, "style is part of the key");
        }
    }

    /// 建对象到底多贵、命中多贵。`cargo test -- --nocapture` 才看得到。
    /// 数字是这台机器的，不写进断言 —— 它只用来证明缓存不是凭空加的。
    #[test]
    fn a_hit_is_cheaper_than_a_create() {
        use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
        unsafe {
            const N: u32 = 20_000;
            let t0 = std::time::Instant::now();
            for i in 0..N {
                let b = CreateSolidBrush(COLORREF(0x0010_0000 + i % 97));
                let _ = DeleteObject(HGDIOBJ(b.0));
            }
            let create = t0.elapsed().as_nanos() as f64 / N as f64;
            let t1 = std::time::Instant::now();
            for i in 0..N {
                let b = brush(COLORREF(0x0020_0000 + i % 97));
                std::hint::black_box(b);
            }
            let hit = t1.elapsed().as_nanos() as f64 / N as f64;
            eprintln!("[gdi] create+delete {create:.0} ns, cache hit {hit:.0} ns");
            assert!(hit < create, "a hit has to be cheaper than a create");
        }
    }
}
