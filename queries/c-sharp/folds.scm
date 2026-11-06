; 由 tools/make_queries.py 生成，请勿手改 —— 重新生成会覆盖。
; 上游 nvim-treesitter a9c4e14 的 runtime/queries/c_sharp/folds.scm（Apache-2.0，见 queries/LICENSE）。
body: [
  (declaration_list)
  (switch_body)
  (enum_member_declaration_list)
] @fold

accessors: (accessor_list) @fold

initializer: (initializer_expression) @fold

[
  (block)
  (preproc_if)
  (preproc_elif)
  (preproc_else)
  (using_directive)+
] @fold
