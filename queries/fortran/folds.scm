; 由 tools/make_queries.py 生成，请勿手改 —— 重新生成会覆盖。
; 上游 nvim-treesitter a9c4e14 的 runtime/queries/fortran/folds.scm（Apache-2.0，见 queries/LICENSE）。
; by @oponkork
[
  (if_statement)
  (where_statement)
  (enum_statement)
  (do_loop_statement)
  (derived_type_definition)
  (function)
  (subroutine)
  (interface)
] @fold
