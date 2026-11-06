//! 缓存键：20 字节、`Copy`、无字符串。
//!
//! 缓存存的是一份 op 列表。命中的判断必须便宜 —— 一帧要问几十次 —— 所以键是
//! 一个定长结构体，散列只是五次乘法-异或，绘制路径上没有一次分配、没有一个
//! `HashMap<String, _>`。
//!
//! 键里放什么、不放什么，是这个设计最关键的一处取舍：
//!
//! **放**：区域、部件号、状态位、尺寸、DPI、字体、序号/总数、部件修订号。
//! **不放**：文字内容、颜色、主题名。文字用占位符（[`crate::TextSource`]）在
//! 回放时替换，颜色来自主题 token —— 它们变了只需要重新解析 token，不需要重跑
//! 脚本。真正会让 op 列表变的只有上面那九项。
//!
//! 尺寸按 **1/16 DIP** 量化：`180.0` 和 `180.01` 是同一个键（同一个像素格），
//! `180.0` 和 `180.1` 不是。区间是 0..4096 DIP，够任何一个窗口。

use crate::{FontId, Region, State};

/// 一份 op 列表的缓存键。20 字节，`Copy`。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct CacheKey {
    /// `region(8) | part(8) | state(16)`
    meta: u32,
    /// `w(16) | h(16)`，1/16 DIP 量化
    size: u32,
    /// `dpi(8) | font(8) | index(8) | count(8)`
    ctx: u32,
    /// 部件修订号：标题变了、图标换了，宿主把它 +1
    rev: u32,
    /// 保留：回放后端、缩放档、将来的标志位
    flags: u32,
}

/// 尺寸的量化精度：1/16 DIP。
const SIZE_STEPS: f32 = 16.0;

/// 量化到 1/16 DIP 的无符号格数。
fn quantize(value: f32) -> u16 {
    if !value.is_finite() {
        return 0;
    }
    (value.max(0.0) * SIZE_STEPS).round().clamp(0.0, u16::MAX as f32) as u16
}

impl CacheKey {
    /// 一个区域里的一类部件。
    pub const fn new(region: Region, part: u16) -> Self {
        Self {
            meta: ((region as u32) << 24) | ((part as u32 & 0xFF) << 16),
            size: 0,
            ctx: 0,
            rev: 0,
            flags: 0,
        }
    }

    /// 这个键属于哪个区域。宿主丢掉一整个区域的缓存时用它分堆 —— 点击之后
    /// 脚本可能改了画法，而缓存的前提（画法是纯函数）在点击这一侧不成立。
    pub const fn region(self) -> u32 {
        self.meta >> 24
    }

    /// 状态位集合。
    pub const fn state(mut self, state: State) -> Self {
        self.meta = (self.meta & 0xFFFF_0000) | (state.bits() as u32);
        self
    }

    /// 画布尺寸，DIP。按 1/16 DIP 量化。
    pub fn size(mut self, w: f32, h: f32) -> Self {
        self.size = ((quantize(w) as u32) << 16) | quantize(h) as u32;
        self
    }

    /// 该区域解析到的 DPI。
    pub const fn dpi(mut self, dpi: u16) -> Self {
        self.ctx = (self.ctx & !0x0000_00FF) | ((dpi & 0xFF) as u32);
        self
    }

    /// 该区域解析到的字体。换字体等于换画法。
    pub const fn font(mut self, font: FontId) -> Self {
        self.ctx = (self.ctx & !0x0000_FF00) | (((font.0 & 0xFF) as u32) << 8);
        self
    }

    /// 第几个 / 共几个。页签条上每个页签的宽度不同，两者都要进键。
    ///
    /// `ctx` 是 `dpi(8) | font(8) | index(8) | count(8)`：低半是 DPI 和字体，
    /// 高半是序号。所以这里只清高半 —— 写成 `& 0xFFFF_0000` 会把 DPI 一起抹掉，
    /// 换个显示器就不失效了。
    pub const fn index(mut self, index: u16, count: u16) -> Self {
        self.ctx = (self.ctx & 0x0000_FFFF)
            | (((index & 0xFF) as u32) << 16)
            | (((count & 0xFF) as u32) << 24);
        self
    }

    /// 部件修订号。文字/图标换了但尺寸没变时，宿主把它 +1。
    pub const fn rev(mut self, rev: u32) -> Self {
        self.rev = rev;
        self
    }

    /// 保留字，给回放后端之类的用。
    pub const fn flags(mut self, flags: u32) -> Self {
        self.flags = flags;
        self
    }

    /// 散列：FNV-1a 走五个字，五次乘法，无分配。
    ///
    /// 不用 `DefaultHasher`：它每次构造要一个随机种子，而且 `write_u32` 走
    /// trait 分派；这里要的是「便宜到可以在 120 Hz 的鼠标移动里跑」。
    pub const fn hash(self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let words = [self.meta, self.size, self.ctx, self.rev, self.flags];
        let mut i = 0;
        while i < 5 {
            h ^= words[i] as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
            i += 1;
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CacheKey {
        CacheKey::new(Region::Tab, 0)
            .state(State::HOT)
            .size(180.0, 30.0)
            .dpi(120)
            .font(FontId(1))
            .index(3, 7)
    }

    /// 键必须是 20 字节的 `Copy` —— 缓存表按它做开放寻址，一帧问几十次。
    #[test]
    fn the_key_is_twenty_bytes_and_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<CacheKey>();
        assert_eq!(std::mem::size_of::<CacheKey>(), 20);
    }

    /// 同样的输入给同样的键和同样的散列。
    #[test]
    fn the_same_inputs_are_the_same_key() {
        assert_eq!(base(), base());
        assert_eq!(base().hash(), base().hash());
    }

    /// 九个输入里任何一个变了，键都必须变 —— 漏一个就是一块永远刷新的脏像素。
    #[test]
    fn every_input_can_change_the_key() {
        let k = base();
        assert_ne!(k, CacheKey::new(Region::StatusBar, 0).state(State::HOT).size(180.0, 30.0).dpi(120).font(FontId(1)).index(3, 7), "region");
        assert_ne!(k, CacheKey::new(Region::Tab, 1).state(State::HOT).size(180.0, 30.0).dpi(120).font(FontId(1)).index(3, 7), "part");
        assert_ne!(k, base().state(State::ACTIVE), "state");
        assert_ne!(k, base().size(181.0, 30.0), "width");
        assert_ne!(k, base().size(180.0, 31.0), "height");
        assert_ne!(k, base().dpi(96), "dpi");
        assert_ne!(k, base().font(FontId(2)), "font");
        assert_ne!(k, base().index(4, 7), "index");
        assert_ne!(k, base().index(3, 8), "count");
        assert_ne!(k, base().rev(1), "rev");
        assert_ne!(k, base().flags(1), "flags");
    }

    /// 序号和 DPI/字体挤在同一个字里，但**各占一半**：写序号不能把 DPI 抹掉，
    /// 否则换个显示器（尺寸按 DIP 算、不变）就命中一份按旧 DPI 算的 op 列表。
    #[test]
    fn the_index_does_not_wipe_the_dpi_or_the_font() {
        let plain = base();
        let numbered = base().index(0, 0);
        assert_ne!(plain, numbered, "index is part of the key");
        // 序号没变、DPI 变了，键必须跟着变。
        assert_ne!(numbered, base().index(0, 0).dpi(96));
        // 同理字体。
        assert_ne!(numbered, base().index(0, 0).font(FontId(2)));
        // 反过来也一样：DPI 没变、序号变了。
        assert_ne!(base().dpi(120).index(1, 0), base().dpi(120).index(2, 0));
    }

    /// 尺寸按 1/16 DIP 量化：同一个像素格是同一个键，差一格就不是。
    #[test]
    fn sizes_are_quantized_to_a_sixteenth_of_a_dip() {
        let a = base().size(180.0, 30.0);
        let b = base().size(180.01, 30.0);
        let c = base().size(180.1, 30.0);
        assert_eq!(a, b, "a hundredth of a DIP is the same cell");
        assert_ne!(a, c, "a tenth of a DIP is not");
    }

    /// 负数、无穷、NaN 都不许变成奇怪的下标。
    #[test]
    fn nonsense_sizes_fall_back_to_zero() {
        assert_eq!(base().size(-5.0, 0.0), base().size(0.0, 0.0));
        assert_eq!(base().size(f32::INFINITY, 0.0), base().size(0.0, 0.0));
        assert_eq!(base().size(f32::NAN, 0.0), base().size(0.0, 0.0));
    }

    /// 状态位不能串到 region/part 上去 —— 打包写错了就会互相覆盖。
    #[test]
    fn the_packed_fields_do_not_bleed() {
        let all = State::from_bits(0x1FF);
        let k = CacheKey::new(Region::Custom, 0xFF).state(all);
        assert_eq!(k.hash(), CacheKey::new(Region::Custom, 0xFF).state(all).hash());
        assert_ne!(k, CacheKey::new(Region::Custom, 0xFF).state(State::normal()));
        assert_ne!(k, CacheKey::new(Region::MenuRow, 0xFF).state(all));
    }

    /// 散列在连续键上要散开：1000 个连号键，重复的散列值应当极少。
    #[test]
    fn the_hash_spreads_consecutive_keys() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..1000u32 {
            seen.insert(base().rev(i).hash());
        }
        assert!(seen.len() >= 995, "only {} distinct hashes", seen.len());
    }
}
