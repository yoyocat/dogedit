//! 跨后端 oracle：同一文本，`Document::from_text`（Mem，按行为重写
//! 语义）与 `Document::open_large`（大文件模式）必须给出完全一致
//! 的行/文本/(line,col) 区间视图。大文件模式的正确性以此为准绳
//! （本实现的验收项）。

use dg_core::linedoc::Pos;
use dg_core::Document;
use std::io::Write;
use std::time::Duration;

fn p(line: usize, col: usize) -> Pos {
    Pos { line, col }
}

fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("evcore-oracle-{name}-{}", std::process::id()));
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(bytes).unwrap();
    p
}

fn compare(name: &str, text: &str) {
    let mem = Document::from_text(text);
    let path = temp_file(name, text.as_bytes());
    let large = Document::open_large(&path).unwrap();
    // 等待后台索引完成（小文件同步路径立即 done）
    large
        .large()
        .unwrap()
        .wait_for_scan(Duration::from_secs(30));

    assert_eq!(mem.line_count(), large.line_count(), "{name}: line count");

    let mut a = String::new();
    let mut b = String::new();
    for line in 0..mem.line_count() {
        a.clear();
        b.clear();
        assert!(mem.line_text(line, &mut a), "{name}: mem line {line}");
        assert!(large.line_text(line, &mut b), "{name}: large line {line}");
        assert_eq!(a, b, "{name}: line {line} text");
        assert_eq!(mem.line_len(line), large.line_len(line), "{name}: line {line} len");
        assert_eq!(mem.eol_kind(line), large.eol_kind(line), "{name}: line {line} eol");
        assert_eq!(mem.line_eol(line), large.line_eol(line), "{name}: line {line} Eol");
    }

    // 全文等价（large 的 to_text 走 overlay，O(N)，oracle 专用）。
    assert_eq!(mem.to_text(), large.to_text(), "{name}: full text");

    // (line, col) 区间抽查（覆盖行中、行尾与跨行区间）。
    let lines = mem.line_count();
    for line in 0..lines {
        let len = mem.line_len(line);
        for col in [0, len / 2, len] {
            let end = (line + 1).min(lines - 1);
            let s = p(line, col);
            let e = p(end, mem.line_len(end));
            assert_eq!(
                mem.range_text(s, e),
                large.range_text(s, e),
                "{name}: range {line}:{col}"
            );
        }
    }
    // Mem 的字节检查点与原文 UTF-8 长度一致。
    assert_eq!(mem.total_bytes(), text.len(), "{name}: total bytes");
    // 大文件臂同样答得出字节数：语法附加的体积闸只问大小，不该为此
    // 撞上「字节桥不在这条臂上」的 panic。
    assert_eq!(large.total_bytes(), text.len(), "{name}: large total bytes");
    assert_eq!(mem.line_byte_offset(0), 0, "{name}: first byte");
    assert_eq!(
        mem.line_byte_col(lines - 1, mem.line_len(lines - 1)),
        text.len(),
        "{name}: last byte"
    );
    assert_eq!(large.is_large(), true);
    assert_eq!(mem.is_large(), false);
    std::fs::remove_file(&path).ok();
}

#[test]
fn oracle_ascii_log() {
    compare(
        "ascii",
        "2026-10-25 08:00:01 INFO start\n2026-10-25 08:00:02 WARN slow query\n2026-10-25 08:00:03 ERROR boom\n",
    );
}

#[test]
fn oracle_mixed_eols() {
    compare("mixed", "a\r\nb\rc\nd\r\n\n\re");
}

#[test]
fn oracle_cjk_utf8() {
    compare("cjk", "第一行\n第二行\r\n中文与 English 混排\n没有行尾的末行");
}

#[test]
fn oracle_empty_and_edge() {
    compare("empty", "");
    compare("single-noeol", "one line only");
    compare("only-eol", "\n");
    compare("crlf-pairs", "\r\n\r\n");
}

#[test]
fn oracle_jsonl() {
    compare(
        "jsonl",
        "{\"ts\":\"2026-10-25T08:00:00Z\",\"lvl\":\"info\",\"msg\":\"ok\"}\n{\"ts\":\"2026-10-25T08:00:01Z\",\"lvl\":\"error\",\"msg\":\"失败\"}\n",
    );
}

#[test]
fn oracle_long_line() {
    // 单行超过物理切分阈值的前置缩小版：两段 100 字符无 EOL
    let long = "x".repeat(200);
    compare("longline", &format!("{long}\n{long}"));
}

/// M6 编辑 oracle：同一随机 (line, col) 编辑序列打到 Mem 与 Large
/// 两个后端，每步全属性一致（行/文本/EOL），且大文件流式合并写出
/// == Mem 全文。
#[test]
fn oracle_random_edits_match() {
    let mut seed = 0x0DDB_2025_CAFE_BABEu64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..3 {
        let start = match round {
            0 => "ab\ncd\r\nef\rgh".to_string(),
            1 => "第一行\nsecond line\r\n混合 tail".to_string(),
            _ => "x\ny\nz".to_string(),
        };
        let mut mem = Document::from_text(&start);
        let path = temp_file(&format!("edit{round}"), start.as_bytes());
        let mut large = Document::open_large(&path).unwrap();
        large
            .large()
            .unwrap()
            .wait_for_scan(Duration::from_secs(30));
        let alphabet = ['x', '\n', '\r', '世', 'é', ' ', 'q'];
        for i in 0..200 {
            // 随机 (line, col) 区间：行内有序，跨行最多两行。
            let lines = mem.line_count();
            let a_line = (next() as usize) % lines;
            let a_col = (next() as usize) % (mem.line_len(a_line) + 1);
            let b_line = (a_line + (next() as usize) % 2).min(lines - 1);
            let b_col = (next() as usize) % (mem.line_len(b_line) + 1);
            let (a_col, b_col) = if a_line == b_line {
                (a_col.min(b_col), a_col.max(b_col))
            } else {
                (a_col, b_col)
            };
            let (s, e) = (p(a_line, a_col), p(b_line, b_col));
            let text = match next() % 3 {
                0 => String::new(),
                1 => alphabet[(next() as usize) % alphabet.len()].to_string(),
                _ => (0..1 + (next() as usize) % 8)
                    .map(|_| alphabet[(next() as usize) % alphabet.len()])
                    .collect(),
            };
            // 大文件预检（Pos → 大文件内部 char 域）。
            {
                let ld = large.large().unwrap();
                let to_char = |q: Pos| ld.line_to_char(q.line).unwrap_or_else(|| ld.len_chars()) + q.col;
                assert!(
                    large.large_edit_would_fail(to_char(s), to_char(e), &text).is_none(),
                    "step {i} 预检拒绝"
                );
            }
            eprintln!("STEP {i} lines={lines} op {s:?}..{e:?} text={text:?}");
            mem.edit_pos(s, e, &text);
            large.edit_pos(s, e, &text);
            assert_eq!(large.line_count(), mem.line_count(), "step {i} line count");
            assert_eq!(large.to_text(), mem.to_text(), "step {i} text");
            let (mut ta, mut tb) = (String::new(), String::new());
            for li in 0..mem.line_count() {
                ta.clear();
                tb.clear();
                assert!(large.line_text(li, &mut ta), "step {i} line {li}");
                assert!(mem.line_text(li, &mut tb));
                assert_eq!(ta, tb, "step {i} line {li}");
                assert_eq!(large.eol_kind(li), mem.eol_kind(li), "step {i} line {li} eol");
            }
        }
        // 流式合并写出 == Mem 全文。
        let mut out = Vec::new();
        large.large().unwrap().write_merged(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), mem.to_text());
        assert!(large.large().unwrap().is_dirty());
        std::fs::remove_file(&path).ok();
    }
}

/// M6：undo/redo 经 UndoStack 在 Large 后端与 Mem 后端行为一致。
#[test]
fn oracle_undo_redo_on_large() {
    let text = "alpha\nbeta\ngamma\n";
    let mut mem = Document::from_text(text);
    let path = temp_file("undo", text.as_bytes());
    let mut large = Document::open_large(&path).unwrap();
    large.large().unwrap().wait_for_scan(Duration::from_secs(30));
    let mut sm = dg_core::UndoStack::new();
    let mut sl = dg_core::UndoStack::new();
    // 三次编辑：插入 / 跨行替换 / 行内删除。
    let ops: [(Pos, Pos, &str); 3] = [
        (p(0, 0), p(0, 0), "X"),
        (p(0, 5), p(1, 4), "二"),
        (p(0, 0), p(0, 3), ""),
    ];
    for (s, e, t) in ops {
        for (doc, stack) in [(&mut mem, &mut sm), (&mut large, &mut sl)] {
            let captured = dg_core::UndoStack::capture(doc, s, e);
            let op = if t.is_empty() {
                dg_core::EditOp::Delete { start: s, end: e, text: captured }
            } else if e != s {
                dg_core::EditOp::Replace { start: s, end: e, old_text: captured, new_text: t.to_string() }
            } else {
                dg_core::EditOp::Insert { pos: s, text: t.to_string() }
            };
            stack.apply(doc, op);
        }
        assert_eq!(large.to_text(), mem.to_text());
    }
    // 全部 undo 后两侧一致且等于原文。
    for _ in 0..3 {
        sm.undo(&mut mem);
        sl.undo(&mut large);
        assert_eq!(large.to_text(), mem.to_text());
    }
    assert_eq!(large.to_text(), text);
    // undo 到底后 overlay 段表不必归零（内容等价但仍是片链形态），
    // 但合并输出必须逐字节等于原文 —— 保存语义由此保证；脏与否由
    // undo 栈锚点（is_modified）判定，与 overlay 清洁度无关。
    let mut out = Vec::new();
    large.large().unwrap().write_merged(&mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), text, "undo 到底后合并输出应等于原文");
    // redo 全部恢复。
    for _ in 0..3 {
        sm.redo(&mut mem);
        sl.redo(&mut large);
        assert_eq!(large.to_text(), mem.to_text());
    }
    std::fs::remove_file(&path).ok();
}
