; 由 tools/make_queries.py 生成，请勿手改 —— 重新生成会覆盖。
; 上游 nvim-treesitter a9c4e14 的 runtime/queries/css/folds.scm（Apache-2.0，见 queries/LICENSE）。
[
  ; top-level block statements from https://github.com/tree-sitter/tree-sitter-css/blob/master/grammar.js
  ; note: (block) is not used due to unideal behavior when node before block node spans multiple lines
  (rule_set)
  (at_rule)
  (supports_statement)
  (media_statement)
  (keyframe_block)
  (import_statement)+
] @fold
