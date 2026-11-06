use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

/// Node-based folding spans — replace the flag list as the
/// fold-flag source. Returns byte ranges `(start, end)`
/// of definition-like nodes that span more than one line.
/// Rows come from the tree's own positions, so no text is
/// needed — no whole-document text on any
/// syntax path.
///
/// 这是**没有** `queries/folds.scm` 时的启发式兜底（按 node kind 名字
/// 猜）。插件包给了 folds.scm 就改走 [`query_spans`]（plan §6 v2）。
///
/// 扁平化按**起始行**做：同一行开始的几个节点（`if x { match y {`）只留
/// 最外层那个，下游仍是「一行一个标志」；但不整棵子树跳过了 —— 早先那样
/// 一个 `impl` 体（`declaration_list`）会把里面所有函数的 `block` 一起吞掉。
pub fn folding_spans(tree: &Tree) -> Vec<(usize, usize)> {
    let mut by_row: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    let root = tree.root_node();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if foldable_kind(node.kind()) && node.end_position().row > node.start_position().row {
            let span = (node.start_byte(), node.end_byte());
            by_row
                .entry(node.start_position().row)
                .and_modify(|e| {
                    if span.1 > e.1 {
                        *e = span;
                    }
                })
                .or_insert(span);
        }
        for i in 0..node.child_count() {
            if let Some(c) = node.child(i) {
                stack.push(c);
            }
        }
    }
    by_row.into_values().collect()
}

/// 能折的 node kind：带 `definition` 的一律算（C/Python 的
/// `function_definition` 都在这条上），其余是各家语法的容器节点。
fn foldable_kind(kind: &str) -> bool {
    kind.contains("definition")
        || matches!(
            kind,
            "block"
                | "statement_block"
                | "object"
                | "array"
                | "function"
                | "class"
                | "class_body"
                | "declaration_list"
                | "match_block"
                | "field_declaration_list"
                | "enum_variant_list"
                | "use_list"
        )
}

/// Query-based folding (`queries/folds.scm`, Helix 约定 capture 名
/// `@fold`). capture 名不参与判断 —— 被 capture 且跨行的 node 就是
/// 一个折叠区。与 [`folding_spans`] 不同，这里**保留嵌套**：Helix 的
/// folds.scm 会同时 capture 外层函数和内层 block，`fold_level_at` /
/// `fold_to_level` 正好按嵌套深度工作。
pub fn query_spans(
    tree: &Tree,
    query: &Query,
    node_text: &dyn Fn(usize, usize) -> Vec<u8>,
) -> Vec<(usize, usize)> {
    let root = tree.root_node();
    let mut cursor = QueryCursor::new();
    // 谓词（#match? 等）的文本来源，与高亮路径同一套闭包。
    let provider = |node: Node| std::iter::once(node_text(node.start_byte(), node.end_byte()));
    let mut spans: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut captures = cursor.captures(query, root, provider);
    while let Some((m, idx)) = captures.next() {
        let node = m.captures()[*idx].node;
        if node.end_position().row > node.start_position().row {
            spans.insert((node.start_byte(), node.end_byte()));
        }
    }
    spans.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn rust_tree(text: &str) -> Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter::Language::from(tree_sitter_rust::LANGUAGE))
            .expect("rust grammar");
        p.parse(text, None).expect("parses")
    }

    #[test]
    fn kind_heuristic_folds_blocks() {
        let tree = rust_tree("fn f() {\n    let x = 1;\n}\n");
        let spans = folding_spans(&tree);
        assert!(!spans.is_empty(), "function/block 至少一个折叠区");
    }

    #[test]
    fn query_spans_keep_nesting() {
        let tree = rust_tree("fn f() {\n    let x = 1;\n}\n");
        let lang = tree_sitter::Language::from(tree_sitter_rust::LANGUAGE);
        let q = Query::new(&lang, "(function_item) @fold\n(block) @fold\n").expect("query");
        let text = "fn f() {\n    let x = 1;\n}\n";
        let nt = |s: usize, e: usize| text.as_bytes()[s..e].to_vec();
        let spans = query_spans(&tree, &q, &nt);
        assert_eq!(spans.len(), 2, "函数与 block 各一个（嵌套保留）: {spans:?}");
        // 外层函数包含内层 block（block 的 `}` 与函数同位，故用 >=）。
        assert!(
            spans[0].0 < spans[1].0 && spans[0].1 >= spans[1].1,
            "外层应包住内层: {spans:?}"
        );
    }

    #[test]
    fn query_spans_skip_single_line_captures() {
        let tree = rust_tree("fn f() {}\n");
        let lang = tree_sitter::Language::from(tree_sitter_rust::LANGUAGE);
        let q = Query::new(&lang, "(function_item) @fold\n").expect("query");
        let nt = |_s: usize, _e: usize| Vec::new();
        assert!(query_spans(&tree, &q, &nt).is_empty(), "单行不算折叠区");
    }

    #[test]
    fn nested_blocks_each_get_a_marker() {
        // 嵌套的块各占一行，各自一个区：只留最外层的话，函数体里的 if
        // 一个标志都不会有。
        let tree = rust_tree("fn f() {\n    if a {\n        g();\n    }\n}\n");
        let spans = folding_spans(&tree);
        assert_eq!(spans.len(), 2, "函数体与 if 体各一个: {spans:?}");
    }

    #[test]
    fn an_impl_body_does_not_swallow_its_methods() {
        // impl 的体是 declaration_list：它自己一个区，里面的 fn 各自一个。
        let tree = rust_tree("impl X {\n    fn f() {\n        g();\n    }\n}\n");
        let spans = folding_spans(&tree);
        assert_eq!(spans.len(), 2, "impl 与 fn 各一个: {spans:?}");
    }

    #[test]
    fn two_blocks_opening_on_one_line_share_the_marker() {
        // 同一行开的两个块只留一个（外层的），否则一行要画两个标志。
        let tree = rust_tree("fn f() {\n    match x { _ => {\n        g();\n    } }\n}\n");
        let spans = folding_spans(&tree);
        assert_eq!(spans.len(), 2, "fn 体 + match（内层块被并掉）: {spans:?}");
    }
}
