; 由 tools/make_queries.py 生成，请勿手改 —— 重新生成会覆盖。
; 上游 nvim-treesitter a9c4e14 的 runtime/queries/perl/folds.scm（Apache-2.0，见 queries/LICENSE）。
(comment)+ @fold

(pod) @fold

; fold the block-typed package and class statements only
(package_statement
  (block)) @fold

(class_statement
  (block)) @fold

[
  (subroutine_declaration_statement)
  (method_declaration_statement)
  (conditional_statement)
  (loop_statement)
  (for_statement)
  (cstyle_for_statement)
  (block_statement)
  (defer_statement)
  (phaser_statement)
] @fold

(try_statement
  (block) @fold)

(eval_expression
  (block) @fold)

(anonymous_subroutine_expression) @fold

; perhaps folks want to fold these too?
[
  (anonymous_array_expression)
  (anonymous_hash_expression)
] @fold
