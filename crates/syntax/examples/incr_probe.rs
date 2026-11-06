//! Micro-benchmark: tree-sitter incremental parse cost vs edit
//! position on one large file. Answers whether every edit
//! invalidates the whole tree (cost ~ full parse) or a local
//! subtree (cost ~ constant).
//!
//! cargo run --release -p dg-syntax --example incr_probe -- <file>

use dg_syntax::Highlighter;
use std::time::Instant;

fn point_of(text: &str, byte: usize) -> tree_sitter::Point {
    let mut row = 0;
    let mut col = 0;
    for (i, b) in text.bytes().enumerate() {
        if i >= byte {
            break;
        }
        if b == b'\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    tree_sitter::Point { row, column: col }
}

fn time_edit(h: &mut Highlighter, text: &str, at: usize, rounds: usize) -> f64 {
    let t0 = Instant::now();
    for _ in 0..rounds {
        let start = point_of(text, at);
        let end = tree_sitter::Point {
            row: start.row,
            column: start.column + 1,
        };
        h.edit_at(at, at, at + 1, start, start, end);
        h.parse_str(text);
    }
    t0.elapsed().as_secs_f64() * 1e6 / rounds as f64
}

fn main() {
    let path = std::env::args().nth(1).expect("file");
    let text = std::fs::read_to_string(&path).expect("read");
    let lang = tree_sitter::Language::from(tree_sitter_rust::LANGUAGE);
    let mut h = Highlighter::new(lang.clone());
    h.reset(&text);
    println!("file: {path}  bytes: {}", text.len());
    for frac in [0usize, 1, 8, 4, 2, 1] {
        let at = text.len() / frac.max(1);
        let us = time_edit(&mut h, &text, at, 50);
        println!("edit at byte {at:>8} (1/{frac}): {us:>9.1} µs/parse");
    }
    // fresh full parse for reference
    let t0 = Instant::now();
    for _ in 0..5 {
        let mut h2 = Highlighter::new(lang.clone());
        h2.reset(&text);
    }
    println!(
        "full from-scratch parse: {:.0} µs",
        t0.elapsed().as_secs_f64() * 1e6 / 5.0
    );
}
