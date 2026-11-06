; 由 tools/make_queries.py 生成，请勿手改 —— 重新生成会覆盖。
; 上游 nvim-treesitter a9c4e14 的 runtime/queries/elm/folds.scm（Apache-2.0，见 queries/LICENSE）。
((function_call_expr) @_fn
  (#not-has-parent? @_fn parenthesized_expr)) @fold

[
  (case_of_branch)
  (case_of_expr)
  (value_declaration)
  (type_declaration)
  (type_alias_declaration)
  (list_expr)
  (record_expr)
  (parenthesized_expr)
  (import_clause)+
] @fold
