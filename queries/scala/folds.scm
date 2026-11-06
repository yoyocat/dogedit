; 由 tools/make_queries.py 生成，请勿手改 —— 重新生成会覆盖。
; 上游 nvim-treesitter a9c4e14 的 runtime/queries/scala/folds.scm（Apache-2.0，见 queries/LICENSE）。
(call_expression
  (block) @fold)

[
  (class_definition)
  (trait_definition)
  (object_definition)
  (function_definition)
  (val_definition)
  (import_declaration)
  (while_expression)
  (do_while_expression)
  (for_expression)
  (try_expression)
  (match_expression)
] @fold
