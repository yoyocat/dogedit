//! Typing-path benchmark: simulates the per-keystroke work the win32
//! backend does on a real source file, and prints the cost per character.
//! cargo run --release -p dg-app --example typing_bench -- <file.rs> The
//! keystroke path no longer runs the syntax parse: the edit is folded
//! into the tree's offsets inside `apply_edit` and the parse rides the
//! debounce timer. So the bench measures: * the keystroke itself: apply
//! (edit + fold + index update) + incremental relayout — what blocks the
//! user; * the deferred parse — what a pause after a burst pays. The
//! "legacy" section re-measures what the pre-incremental code did per
//! keystroke: a full `to_text()` + from-scratch tree-sitter parse
//! (`attach_syntax`) and a full relayout.

use dg_app::app::Editor;
use dg_core::Document;
use dg_core::linedoc::Pos;
use dg_layout::measure::{CachedMeasurer, Font, TableMeasurer};
use dg_layout::wrap::{CharWrapperEx, LayoutCtx};
use dg_syntax::LanguageRegistry;
use std::time::Instant;

fn ctx_of<'a>(m: &'a mut CachedMeasurer<TableMeasurer>) -> LayoutCtx<'a> {
    LayoutCtx {
        measurer: m,
        font: Font::Primary,
        tab_px: 32,
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "crates/dg-app/src/win32/window.rs".to_string());
    let text = std::fs::read_to_string(&path).expect("read source file");
    let doc = Document::from_text(&text);
    let lines = doc.line_count();
    let chars = doc.total_bytes();
    let mid = Pos {
        line: lines / 2,
        col: doc.line_len(lines / 2) / 2,
    };

    let mut ed = Editor::new();
    ed.open_text(&text);
    // 发行版不内置 grammar：从 parsers\ 取（没生成就直说）。
    let mut reg = LanguageRegistry::new();
    reg.scan_plugins(std::path::Path::new("parsers"));
    ed.attach_syntax(
        reg.get("rust")
            .expect("parsers\\rust 未生成：python tools/make_parsers.py --helix <Helix>"),
    );

    let wrapper = CharWrapperEx { chars: 0xFFFF }; // effectively no wrap
    let mut m = CachedMeasurer::new(TableMeasurer::default());
    ed.relayout(&wrapper, &mut ctx_of(&mut m));

    println!(
        "file: {path}  lines: {lines}  chars: {chars}  bytes: {}",
        text.len()
    );

    // The keystroke path (syntax parse deferred to the timer).
    let n = 3000;
    let (mut t_apply, mut t_sync, mut t_layout) = (0f64, 0f64, 0f64);
    let t0 = Instant::now();
    for i in 0..n {
        // walk the caret forward through the middle of the file
        let at = Pos {
            line: mid.line,
            col: mid.col + (i % 97) * (i % 7 + 1),
        }; // past-end cols clamp inside the edit
        let a = Instant::now();
        ed.apply_insert(at, "x"); // edit + fold + index update
        t_apply += a.elapsed().as_secs_f64();
        let c = Instant::now();
        for (first, old_lines, new_lines) in ed.take_layout_regions() {
            ed.relayout_region(first, old_lines, new_lines, &wrapper, &mut ctx_of(&mut m));
        }
        t_layout += c.elapsed().as_secs_f64();
    }
    let dt = t0.elapsed();
    println!(
        "keystroke path (parse deferred): {n} chars in {:?}  =>  {:.2} µs/char  (apply+fold {:.1} / layout {:.1} µs)",
        dt,
        dt.as_secs_f64() * 1e6 / n as f64,
        t_apply * 1e6 / n as f64,
        t_layout * 1e6 / n as f64
    );

    // What the debounce timer pays when typing pauses: the incremental
    // parse of whatever burst accumulated.
    let rounds = 60;
    let t0 = Instant::now();
    for _ in 0..rounds {
        let a = Instant::now();
        ed.sync_syntax();
        t_sync += a.elapsed().as_secs_f64();
        ed.apply_insert(mid, "x");
    }
    let dt = t0.elapsed();
    println!(
        "deferred parse (timer, mid-file): {rounds} in {:?}  =>  {:.0} µs/parse",
        dt,
        t_sync * 1e6 / rounds as f64
    );

    // Enter key (structural edit) path — what it would cost with the
    // parse still deferred: the edit and relayout only.
    let t0 = Instant::now();
    for i in 0..300 {
        let at = Pos {
            line: mid.line,
            col: mid.col + i * 13,
        };
        ed.apply_insert(at, "\n");
        for (first, old_lines, new_lines) in ed.take_layout_regions() {
            ed.relayout_region(first, old_lines, new_lines, &wrapper, &mut ctx_of(&mut m));
        }
    }
    let dt = t0.elapsed();
    println!(
        "Enter key path (parse deferred):  300 keys in {:?}  =>  {:.2} µs/key",
        dt,
        dt.as_secs_f64() * 1e6 / 300.0
    );

    // What the old code paid per keystroke: full reparse.
    let rounds = 20;
    let t0 = Instant::now();
    for _ in 0..rounds {
        let lang = reg.get("rust").unwrap();
        ed.attach_syntax(lang); // reset + from-scratch parse == old sync_syntax
    }
    let dt = t0.elapsed();
    println!(
        "legacy full reparse (old sync_syntax): {rounds} in {:?}  =>  {:.0} µs/keystroke",
        dt,
        dt.as_secs_f64() * 1e6 / rounds as f64
    );

    // What the old code paid per keystroke: full relayout.
    let t0 = Instant::now();
    for _ in 0..20 {
        ed.relayout(&wrapper, &mut ctx_of(&mut m));
    }
    let dt = t0.elapsed();
    println!(
        "legacy full relayout:                 20 in {:?}  =>  {:.0} µs/keystroke",
        dt,
        dt.as_secs_f64() * 1e6 / 20.0
    );
}
