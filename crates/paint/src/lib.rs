//! Tokenizer 与绘制器之间的公共「画字词汇」：`[Style]` 字形位和着色区间。
//!
//! 这些是纯数据（几个 `u8` 常量 + 一个 POD 结构体），本来却定义在
//! `dg-syntax` 里，于是只为了读 `STYLE_BOLD` 的 `dg-render` 得把 tree-sitter、
//! `libloading`、`zip`、`sha2` 整棵依赖树拖进来。放到这个零依赖叶子 crate 后，
//! `dg-syntax` 与 `dg-render` 各依赖它，方向就干净了。
//!
//! `dg_syntax` 仍旧 `pub use` 这些名字，旧的 `dg_syntax::STYLE_*` /
//! `dg_syntax::HighlightRun` 导入路径不变。
//!
//! **位值是对外协议**：主题 ini 的 `[Style]` 列、脚本、皮肤和主题对话框都按
//! 这套布局读，改一个值等于改文件格式。

/// `[Style]` 的八位。
///
/// | 位 | 值 | 主题对话框那一列 |
/// |----|-----|------------------|
/// | 0 | 1 | 粗 |
/// | 1 | 2 | 斜 |
/// | 2 | 4 | 矩（四周一圈框） |
/// | 3 | 8 | 删（删除线） |
/// | 4 | 16 | 下（下划线） |
/// | 5 | 32 | 双（双下划线） |
/// | 6 | 64 | 点（点线） |
/// | 7 | 128 | 波（波浪线） |
///
/// 低两位（粗/斜）在随包主题里有现成写法可对：27 个文件里 `Comment1=2`
/// 一律是斜体注释、`Word1=1` 一律是粗体关键字。其余六位没有任何标记说明它们
/// 原来说什么，所以这里按"16 最常见、且它落在 Url/Email
/// 上（URL 该有下划线）"来定 —— 27 个文件里 44 处 `16` 一个都不用改。
///
/// 下 / 双 / 点 / 波 是**下划线一族**，语义上互斥（同一条线不可能又是点又是
/// 波浪）：同时置了好几位时画最花的那一种，主题对话框里点其中一列会把这一族
/// 的其它位清掉。
///
/// 随包的 27 个文件里有三格带着按这套布局读得出、但**原始意图无从考证**的位：
/// `Found=8` 与 `Word4=8`（DogEdit.ini）、`Pair=33`（Solarized (Dark).ini）。
/// 读的时候不特殊对待 —— 8 就是删、33 就是粗+双 —— 但 `Pair` / `Found` /
/// `Word4` 都不在 `App` 的 `STYLE_KEYS` 里，到不了画字那一步，所以屏幕上什么
/// 都没变，只有主题对话框那两行会显示对应的勾。文件不改：这三格留着，将来
/// 重新解释这一族时还能看到当初写的是什么值。
pub const STYLE_BOLD: u8 = 0x01;
pub const STYLE_ITALIC: u8 = 0x02;
pub const STYLE_BOX: u8 = 0x04;
pub const STYLE_STRIKE: u8 = 0x08;
pub const STYLE_UNDERLINE: u8 = 0x10;
pub const STYLE_DOUBLE: u8 = 0x20;
pub const STYLE_DOTTED: u8 = 0x40;
pub const STYLE_WAVY: u8 = 0x80;

/// 下划线那一族：`下 / 双 / 点 / 波`。
pub const STYLE_UNDERLINE_FAMILY: u8 =
    STYLE_UNDERLINE | STYLE_DOUBLE | STYLE_DOTTED | STYLE_WAVY;

/// `{ color, length, style }` 三元组 ——
/// tokenizer's output format (global array
/// the field). `length` is in characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightRun {
    pub color: u32,
    pub len: u32,
    /// `STYLE_*` 的按位或（粗/斜/矩/删/下/双/点/波）；0 = 正体。
    pub style: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 位值是对外协议，一个都不能挪。
    #[test]
    fn style_bits_are_a_file_format() {
        assert_eq!(STYLE_BOLD, 1);
        assert_eq!(STYLE_ITALIC, 2);
        assert_eq!(STYLE_BOX, 4);
        assert_eq!(STYLE_STRIKE, 8);
        assert_eq!(STYLE_UNDERLINE, 16);
        assert_eq!(STYLE_DOUBLE, 32);
        assert_eq!(STYLE_DOTTED, 64);
        assert_eq!(STYLE_WAVY, 128);
        assert_eq!(STYLE_UNDERLINE_FAMILY, 0xF0);
        // 八个位正好铺满一个 u8，[Style] 只取低 8 位。
        assert_eq!(
            STYLE_BOLD
                | STYLE_ITALIC
                | STYLE_BOX
                | STYLE_STRIKE
                | STYLE_UNDERLINE_FAMILY,
            0xFF
        );
    }

    #[test]
    fn a_run_is_plain_data() {
        let run = HighlightRun {
            color: 0xAABBCC,
            len: 7,
            style: STYLE_BOLD | STYLE_WAVY,
        };
        let copied = run;
        assert_eq!(copied, run);
        assert_eq!(copied.style & 0xFF, 0x81);
    }
}
