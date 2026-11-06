; 由 tools/make_queries.py 生成，请勿手改 —— 重新生成会覆盖。
; 上游 nvim-treesitter a9c4e14 的 runtime/queries/php/folds.scm（Apache-2.0，见 queries/LICENSE）。
; 已展开继承：php_only
[
  (if_statement)
  (switch_statement)
  (while_statement)
  (do_statement)
  (for_statement)
  (foreach_statement)
  (try_statement)
  (function_definition)
  (class_declaration)
  (interface_declaration)
  (trait_declaration)
  (enum_declaration)
  (function_static_declaration)
  (method_declaration)
  (namespace_use_declaration)+
  (array_creation_expression)
  (match_expression)
] @fold
