-- M2: 去除每行行尾空白(脚本版;原生命令是 删除行尾空白,
-- id 64 — 这个脚本走的是和原生命令同一条 undo 通道,做 A/B 对照)。
-- 列坐标是 UTF-8 字节列,#keep/#t 与 App.doc.replace 的列语义一致,
-- 所以含中文的行也正确。
local n = 0
for i = 1, App.doc.line_count() do
    local t = App.doc.line(i)
    local keep = t:match("^(.*%S)") or ""
    if #keep < #t then
        -- 列区间是 [start, end):末端要用 #t+1 表示"行尾之后"。
        App.doc.replace(i, #keep + 1, i, #t + 1, "")
        n = n + 1
    end
end
if n > 0 then
    App.log(("trim_trailing.lua: %d lines trimmed"):format(n))
end
return n
