; 由 tools/make_queries.py 生成，请勿手改 —— 重新生成会覆盖。
; 上游 nvim-treesitter a9c4e14 的 runtime/queries/gdscript/folds.scm（Apache-2.0，见 queries/LICENSE）。
[
  ; Body fold will "join" the next adjacent fold into a SUPER fold.
  ; This is an issue with the grammar.
  ; (body)
  (if_statement)
  (elif_clause)
  (else_clause)
  (for_statement)
  (while_statement)
  (class_definition)
  (enum_definition)
  (match_statement)
  (pattern_section)
  (function_definition)
  (lambda)
  (constructor_definition)
] @fold

; It's nice to be able to fold the if/elif/else clauses and the entire
; if_statement.
(if_statement
  (body) @fold)

; Fold strings that are probably doc strings.
(expression_statement
  (string) @fold)
