//! Layout oracle tests — the fixed expected widths and break points
//! Behavioural oracle for the text-layout algorithms.
//! And assert the engine reproduces them.

use dg_layout::measure::{Font, TableMeasurer, TextMeasurer};
use dg_layout::vertical::RowIndex;
use dg_layout::wrap::{
    CharWrapper, LayoutCtx, PixelWrapper, WinEdgeWrapper, WordEdgeWrapper, WrapEngine, Wrapper,
};
use dg_core::Folding;

fn ctx(m: &mut TableMeasurer) -> LayoutCtx<'_> {
    LayoutCtx {
        measurer: m,
        font: Font::Primary,
        tab_px: 80,
    }
}

#[test]
fn char_wrap_exact_overflow() {
    // Fixed-width font, 10 px/char, limit 100 px -> 10 chars
    // per segment (x == limit still fits; the next char
    // overflows), break at the exact overflow char (char mode).
    let mut m = TableMeasurer {
        ascii: [10; 128],
        cjk_width: 20,
        fallback: 10,
        avg: 10,
    };
    let wrapper = WinEdgeWrapper {
        width_px: 100,
        word_wrap: false,
    };
    let text = "abcdefghijklmno";
    let mut engine = WrapEngine::default();
    let rows = engine.wrap_line(0, text, &wrapper, &mut ctx(&mut m));
    assert_eq!(rows, 2); // 15 chars -> 10 + 5, 1 break, count + 1
    let arr = &engine.store.arrays[0];
    assert_eq!(arr.data, vec![10]);
}

#[test]
fn word_wrap_breaks_at_last_space() {
    // "aaa bbb ccc" with 8px chars: WinEdgeWrapper's floor
    // (min 8·xWidth = 64) raises the 60px limit to 64, so the
    // walk overflows at the second space (x=72 at index 8) and
    // breaks at the last breakable position inside the limit —
    // the space at index 7. The remainder "ccc" fits (x=40).
    let mut m = TableMeasurer::default();
    let wrapper = WinEdgeWrapper {
        width_px: 60,
        word_wrap: true,
    };
    let text = "aaa bbb ccc";
    let mut engine = WrapEngine::default();
    let rows = engine.wrap_line(0, text, &wrapper, &mut ctx(&mut m));
    assert_eq!(rows, 2); // 1 break, count + 1
    let arr = &engine.store.arrays[0];
    assert_eq!(arr.data[0], 7);
    assert_eq!(arr.data, vec![7]);
}

#[test]
fn break_rules_move_the_landing_point() {
    // 10 px 等宽、限宽 100 px、按字符断：第 11 个字符（下标 10）溢出来，
    // 断点本来落在下标 10 —— 也就是让那一格顶到下一行的行头。
    let mut m = TableMeasurer {
        ascii: [10; 128],
        cjk_width: 20,
        fallback: 10,
        avg: 10,
    };
    let wrapper = WinEdgeWrapper {
        width_px: 100,
        word_wrap: false,
    };
    // 行头禁排：`,` 不许起行，断点退一格，逗号跟着上一行走。
    let mut engine = WrapEngine::default();
    engine.break_head = vec![','];
    engine.wrap_line(0, "aaaaaaaaaa,bbb", &wrapper, &mut ctx(&mut m));
    assert_eq!(engine.store.arrays[0].data, vec![9]);
    // 行尾禁排：`(` 不许收尾，断点同样退一格，左括号跟着挪到下一行。
    let mut engine = WrapEngine::default();
    engine.break_tail = vec!['('];
    engine.wrap_line(0, "aaaaaaaaa(bb", &wrapper, &mut ctx(&mut m));
    assert_eq!(engine.store.arrays[0].data, vec![9]);
    // 两串都空 = 老行为，断点就在溢出的那一格。
    let mut engine = WrapEngine::default();
    engine.wrap_line(0, "aaaaaaaaaa,bbb", &wrapper, &mut ctx(&mut m));
    assert_eq!(engine.store.arrays[0].data, vec![10]);
    // 退让不许排出一行空行：整段都是禁则字符时停在原地犯一次。
    let mut engine = WrapEngine::default();
    engine.break_head = vec![','];
    engine.wrap_line(0, ",,,,,,,,,,,x", &wrapper, &mut ctx(&mut m));
    assert!(
        engine.store.arrays[0]
            .data
            .windows(2)
            .all(|w| w[1] > w[0]),
        "断点必须严格往前，否则排得出空行"
    );
}

#[test]
fn tab_snaps_to_tab_stop() {
    // tab_px = 80; after "a" (x=8) a tab snaps to 80, so the
    // next char starts at x=80. With a 100px limit the 'b' at
    // x=88 does not overflow; a second tab snaps to 160 >= 100
    // and forces a break at the tab position.
    let mut m = TableMeasurer::default();
    let wrapper = WinEdgeWrapper {
        width_px: 100,
        word_wrap: false,
    };
    let text = "a\tb\tc";
    let mut engine = WrapEngine::default();
    let rows = engine.wrap_line(0, text, &wrapper, &mut ctx(&mut m));
    // walk: a->x=8; tab->x=80; b->x=88; tab->x=160>=100 ->
    // break at i=3 (the second tab, no breakable recorded);
    // the tab re-snaps to 80 on the next row, then c->x=88.
    // Break positions: [3].
    assert_eq!(rows, 2); // 1 break, count + 1
    assert_eq!(engine.store.arrays[0].data, vec![3]);
}

#[test]
fn crlf_stops_the_walk() {
    let mut m = TableMeasurer::default();
    let wrapper = WinEdgeWrapper {
        width_px: 1000,
        word_wrap: false,
    };
    let text = "ab\r\ncd";
    let mut engine = WrapEngine::default();
    let rows = engine.wrap_line(0, text, &wrapper, &mut ctx(&mut m));
    assert_eq!(rows, 2); // 1 break (the CR), count + 1
    assert_eq!(engine.store.arrays[0].data, vec![2]);
}

#[test]
fn pixel_wrapper_is_fixed_width() {
    let mut m = TableMeasurer::default();
    let wrapper = PixelWrapper { px: 25 };
    let text = "abcdefgh";
    let mut engine = WrapEngine::default();
    let rows = engine.wrap_line(0, text, &wrapper, &mut ctx(&mut m));
    assert_eq!(rows, 3); // 8 chars * 8px / 25px -> 3+3+2 chars, 2 breaks
    assert_eq!(engine.store.arrays[0].data, vec![3, 6]);
}

#[test]
fn char_wrapper_limits_by_characters() {
    let mut m = TableMeasurer::default();
    let wrapper = CharWrapper { chars: 4 };
    let text = "abcdefghij";
    let mut engine = WrapEngine::default();
    let rows = engine.wrap_line(0, text, &wrapper, &mut ctx(&mut m));
    assert_eq!(rows, 3); // 4 + 4 + 2 chars, 2 breaks, count + 1
    assert_eq!(engine.store.arrays[0].data, vec![4, 8]);
}

#[test]
fn virtual_wrapper_never_breaks() {
    let mut m = TableMeasurer::default();
    let wrapper = dg_layout::wrap::VirtualWrapper;
    let text = "abc";
    let mut engine = WrapEngine::default();
    let rows = engine.wrap_line(0, text, &wrapper, &mut ctx(&mut m));
    assert_eq!(rows, 1); // no breaks, empty array, count + 1
    assert_eq!(engine.store.arrays[0].data, vec![]);
}

#[test]
fn virtual_mode_never_measures_a_line() {
    // 不换行: one row per line, no break array, and the measurer is never asked —
    // a keystroke must not walk (or measure) the whole text.
    struct NoMeasure;
    impl TextMeasurer for NoMeasure {
        fn glyph_width(&mut self, _c: char, _f: Font) -> u8 {
            panic!("virtual mode measured a character")
        }
        fn measure_run(&mut self, _s: &str, _f: Font) -> f32 {
            panic!("virtual mode measured a run")
        }
        fn avg_width(&self, _f: Font) -> u8 {
            panic!("virtual mode asked for the average width")
        }
    }
    let mut m = NoMeasure;
    let mut engine = WrapEngine::default();
    let rows = engine.wrap_line_virtual(0, 1234);
    assert_eq!(rows, 1);
    assert!(engine.store.arrays[0].data.is_empty());
    assert_eq!(engine.store.arrays[0].cells, 1234);
    let mut w = LayoutCtx {
        measurer: &mut m,
        font: Font::Primary,
        tab_px: 80,
    };
    // The same through the trait object: the wrapper answers for the mode.
    assert_eq!(
        engine.wrap_line(0, "abcdef", &dg_layout::wrap::VirtualWrapper, &mut w),
        1
    );
}

#[test]
fn win_edge_minimum_is_8_x_width() {
    // width_px below the 8*xWidth floor is raised to the floor.
    let mut m = TableMeasurer::default();
    let wrapper = WinEdgeWrapper {
        width_px: 5,
        word_wrap: false,
    };
    let mut w = LayoutCtx {
        measurer: &mut m,
        font: Font::Primary,
        tab_px: 80,
    };
    // 8 * 8 = 64 px floor
    assert_eq!(wrapper.limit_px(&mut w), 64);
}

#[test]
fn row_index_maps_through_folding() {
    let mut m = TableMeasurer::default();
    let wrapper = WinEdgeWrapper {
        width_px: 100,
        word_wrap: false,
    };
    let lines = ["abcdefghijklmnopqrst", "xy", "zw"];
    let mut engine = WrapEngine::default();
    let mut folding = Folding::default();
    let mut c = ctx(&mut m);
    let total = engine.relayout(lines.iter().copied(), &wrapper, &mut c);
    // line 0: 20 chars -> 1 break at char 12 (2 rows),
    // line 1: 2 (1 row), line 2: 2 (1 row)
    assert_eq!(total, 2 + 1 + 1);

    let mut idx = RowIndex::new();
    idx.rebuild(&engine.store, &folding);
    assert_eq!(idx.line_to_visual_row(1), 2);
    assert_eq!(idx.row_to_line_segment(3), Some((2, 0)));

    // fold line 1 away
    folding.set(1, true);
    idx.rebuild(&engine.store, &folding);
    assert_eq!(idx.total_rows(), 2 + 1);
    assert_eq!(idx.row_to_line_segment(2), Some((2, 0)));
}

#[test]
fn width_cache_warms_up() {
    use dg_layout::measure::CachedMeasurer;
    let mut cm = CachedMeasurer::new(TableMeasurer::default());
    assert_eq!(cm.char_width('a', Font::Primary), 8);
    // second call hits the flat BMP table
    assert_eq!(cm.char_width('a', Font::Primary), 8);
    // CJK rule: codepoint >= 0x2000 selects the CJK font
    assert_eq!(cm.char_width('あ', Font::Primary), 16);
    assert_eq!(cm.char_width('あ', Font::Primary), 16);
}

#[test]
fn measure_run_sums_widths() {
    let mut m = TableMeasurer::default();
    assert_eq!(m.measure_run("abc", Font::Primary), 24.0);
    assert_eq!(m.tab_width(4, Font::Primary), 32);
}

/// CJK corpus entry (the plan's
/// oracle corpus calls for CJK):
/// each ideograph is one column
/// wide, so a 10-column limit
/// fits exactly ten kana.
#[test]
fn cjk_wraps_at_fullwidth_boundary() {
    let mut m = TableMeasurer::default();
    let mut engine = WrapEngine::default();
    let text = "あいうえおかきくけこ";
    // 10 full-width chars at
    // 16 px each = 160 px
    // (TableMeasurer gives
    // codepoints >= 0x2000 a
    // 16 px width).
    let wrapper = WordEdgeWrapper { limit: 160 };
    let rows = engine.wrap_line(0, text, &wrapper, &mut ctx(&mut m));
    assert_eq!(rows, 1);
    // One pixel over forces a
    // break: the engine splits
    // at the last breakable
    // point (here, the char
    // before the overflow).
    let wrapper = WordEdgeWrapper { limit: 159 };
    let rows = engine.wrap_line(1, text, &wrapper, &mut ctx(&mut m));
    assert_eq!(rows, 2);
}

/// Control characters (the plan's
/// oracle corpus calls for them):
/// a mid-line CR terminates the
/// display row early.
#[test]
fn control_char_terminates_row() {
    let mut m = TableMeasurer::default();
    let mut engine = WrapEngine::default();
    // "ab" then a CR then more:
    // the walk stops at the CR,
    // recording one break at the
    // CR position -> 2 rows
    // (count + 1).
    let text = "ab\rcd";
    let wrapper = WinEdgeWrapper {
        width_px: 800,
        word_wrap: true,
    };
    let rows = engine.wrap_line(0, text, &wrapper, &mut ctx(&mut m));
    assert_eq!(rows, 2);
    assert_eq!(engine.store.arrays[0].data, vec![2]);
}
