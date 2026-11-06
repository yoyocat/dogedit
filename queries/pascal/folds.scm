; 由 tools/make_queries.py 生成，请勿手改 —— 重新生成会覆盖。
; 上游 nvim-treesitter a9c4e14 的 runtime/queries/pascal/folds.scm（Apache-2.0，见 queries/LICENSE）。
[
  (interface)
  (implementation)
  (initialization)
  (finalization)
  (if)
  (ifElse)
  (while)
  (repeat)
  (for)
  (foreach)
  (try)
  (case)
  (caseCase)
  (asm)
  (with)
  (declVar)
  (declConst)
  (declEnum)
  (declProcRef)
  (declExports)
  (declType)
  (defProc)
  (declField)
  (declProp)
  (comment)
] @fold

(interface
  (declProc) @fold)
