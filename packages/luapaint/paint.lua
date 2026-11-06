-- LuaPaint 的第一半：**画法** —— 状态栏每一格、页签、停靠页签、滚动条、工具栏。
--
-- 这是一个普通的脚本包，和别的包一样从 `packages/` 装载，只是它做的事刚好
-- 都是「画」：状态栏、页签、滚动条、工具栏的底子都由这里的绘制回调接管。
--
-- 绘制回调**没有 bridge** —— 拿不到文档，也拿不到 `&mut EditorState`。
-- 这是刻意的：绘制期重入会撞上正被借走的编辑状态，所以干脆不给工具。
-- 回调拿到的只有一块画布 `c` 和一份只读上下文 `ctx`。
--
-- 返回值就是「这一格我画了没有」：
--   true        内置画法让位
--   false / nil 交回内置画法（只想改某一格时这么写）
--   抛错        记一行日志，交回内置画法
local ui = App.ui

-- 心跳：这一档拿不到 `&mut EditorState`，所以定时器回调什么都不做 —— 它只
-- 负责「叫醒」。宿主派发完定时器会重画一帧（frame + 状态栏），绘制回调里
-- 读 `App.now()` 就有了时间。没有 `os` 的沙箱里这是唯一的时间来源。
App.timer(50, function() end, true)

-- 每个部件一组颜色，没列到的用默认那组。
local INK = {
  message = "#FFD166",
  encoding = "#7FD1B9",
  eol = "#7FD1B9",
  ln = "#8ECAE6",
  col = "#8ECAE6",
  char = "#8ECAE6",
  filesize = "#B0BEC5",
}
local DEFAULT_INK = "#90A4AE"

ui.paint:on("status.pane", function(c, ctx)
  local r = ctx.rect
  local ink = INK[ctx.part] or DEFAULT_INK

  -- 文档快照：`ctx.doc` 是外壳每帧推过来的只读表（这一档没有 bridge，问不了
  -- `App.doc.meta()`）。换行方式这一格按它换色 —— 和格里的文字同源，但皮肤
  -- 不用去解析文字就能配色。只有读过它的条目才会跟着文档变（见「六、缓存」）。
  if ctx.part == "eol" and ctx.doc.eol == "CRLF" then
    ink = "#FF8A65"
  end

  -- 底：竖直渐变，让每格看起来是嵌进去的。
  c:gradient(r, "#16212B", "#0E151B", "v")

  -- 左边一道窄条，颜色就是这个部件自己的色。
  c:fill(r:with_width(3), ink)

  -- 文字：左边留 9 px，垂直居中，放不下就省略号。
  c:text(r:inset(9, 0), "@text", ink, {
    valign = "middle",
    ellipsis = true,
  })

  -- 右边界：一格一条，省得相邻两格糊在一起。
  c:line(r.right - 1, r.y + 4, r.right - 1, r.bottom - 4, "#1E3A46", 1)

  -- 一道来回扫的细线：`App.now()` 是宿主毫秒时钟，1.6 秒一个来回。
  -- 这是皮肤能动起来的全部秘密 —— 心跳定时器 + 每帧重算位置。
  local span = math.max(0, r.w - 24)
  local t = (App.now() % 1600) / 1600
  if span > 0 then
    local bar = r:move(0, r.h - 2):with_width(24):with_height(2)
    c:fill(bar:move(t * span, 0), ink)
  end

  -- 悬停/按下：脚本自己判断，内置画法不管这些格子。
  if ctx.hot then
    c:rect(r, "#2E4A5A", 1)
  end

  return true
end)

-- 页签条：每一格自己画。`ctx` 给的是**这一格自己的**状态位：
-- active（当前页签）/ hot（鼠标悬停）/ modified（改过没保存）。
ui.paint:on("tab", function(c, ctx)
  local r = ctx.rect

  -- 底：选中最亮，悬停次之，其余压暗。
  local bg = "#141F27"
  if ctx.active then
    bg = "#1E3A46"
  elseif ctx.hot then
    bg = "#1B2C36"
  end
  c:fill(r, bg)

  -- 选中：顶上一条 2 px 高亮，和状态栏那条窄条同一个色。
  if ctx.active then
    c:fill(r:with_height(2), "#FFD166")
  end
  -- 改过没保存：左边一道红。内置画法是顶上一条红带，这里换个方向。
  if ctx.modified then
    c:fill(r:with_width(2), "#EF476F")
  end

  -- 标题。选中用亮字，其余用灰字。
  c:text(r:inset(10, 0), "@text", ctx.active and "#F2F5F7" or "#8FA3B0", {
    valign = "middle",
    ellipsis = true,
  })

  -- 右边界：一格一条，省得相邻两格糊在一起。
  c:line(r.right - 1, r.y + 3, r.right - 1, r.bottom - 3, "#0E151B", 1)

  return true
end)

-- 停靠窗格的页签条（目录树 / 文件列表 / 结果 / 输出那一条）。
--
-- 和文档页签不一样：**图标和文字仍归控件**。每一格的图标取自工具栏那张精灵图，
-- 脚本拿不到，所以这一区只画底子。别在这里画字 —— 控件会再画一遍，两遍就糊了。
-- `ctx.text` 是这一格的标题，只用来做尺寸/配色判断，不用来画。
ui.paint:on("dock.tab", function(c, ctx)
  local r = ctx.rect

  if ctx.active then
    c:fill(r, "#1E3A46")
    c:fill(r:with_height(2):move(0, r.h - 2), "#FFD166")
  elseif ctx.hot then
    c:fill(r, "#1B2C36")
  else
    c:fill(r, "#16212B")
  end
  -- 右边界：一格一条，省得相邻两格糊在一起。
  c:line(r.right - 1, r.y + 2, r.right - 1, r.bottom - 2, "#0E151B", 1)

  return true
end)

-- 滚动条：竖的横的同一条画法。
--
-- 这一档和别处不一样的地方是**没有半透明**：滚动条是分层窗口，像素带逐点
-- alpha 交给 DWM，而 GDI 只写颜色不写 alpha —— 所以「画过的像素」一律算
-- 不透明，没画的一律算全透。想要正文透出来就别画那块底，别指望半透明。
--
-- `ctx.thumb` 是内置画法算好的滑块矩形（映射、最短长度、居中都在里面），
-- 直接拿来画就行；`ctx.rect` 是整条。`ctx.hot` = 指针在这条上或正在拖。
local function scroll(c, ctx)
  local r = ctx.rect
  local t = ctx.thumb

  -- 轨道只在指针上来时出现：平时整条是透明的，正文一像素都不挡。
  if ctx.hot then
    c:fill(r, "#0E151B")
  end

  -- 滑块：圆角胶囊，悬停换成状态栏那个黄。
  local ink = ctx.hot and "#FFD166" or "#3E5A6B"
  c:round(t, math.min(t.w, t.h) / 2, ink)

  return true
end

ui.paint:on("scroll.v", scroll)
ui.paint:on("scroll.h", scroll)

-- 工具栏：每一格的底色和边框归脚本，**图标仍是控件画的** —— 那套精灵图在
-- image list 里，脚本拿不到，所以这一区的语义是「底子归你，glyph 归控件」。
-- `ctx.part` 是命令名（`cm_file_save`），按它分支比按顺序稳。
ui.paint:on("toolbar.button", function(c, ctx)
  local r = ctx.rect

  -- 禁用态交回内置：它的灰化走 `ILD_BLEND50`，脚本画不出同样的效果。
  if ctx.disabled then
    return false
  end

  if ctx.pressed then
    c:fill(r, "#2A4553")
  elseif ctx.checked then
    -- 勾选的按钮（自动换行）：底色加脚下一道黄。
    c:fill(r, "#1E3A46")
    c:fill(r:with_height(2):move(0, r.h - 2), "#FFD166")
  elseif ctx.hot then
    c:fill(r, "#1B2C36")
    c:rect(r, "#2E4A5A", 1)
  end
  return true
end)

-- 指针提示：悬停在这一格时指针长什么样、弹什么提示。
--
-- 这是**声明式的一张表**，不是回调 —— 悬停每秒问几十次，为两个常量进一次 Lua
-- 不值当，而且回调能写出死循环，指针卡住比画错更难受。
--
-- `tip` 里可以用 `@text` / `@label` / `@value`，展开成这一格自己的三样东西
-- （和绘制档 `c:text(r, "@text", …)` 认的是同一组）。`cursor` 的名字见
-- `docs/script_ui.md`：arrow / ibeam / hand / cross / size_* / wait / …
--
-- `tip` 只有 `tab` 和 `dock.tab` 收：工具栏的提示是控件按 `cmdtext` 要的，
-- 状态栏的是文字放不下时 comctl32 自己弹的 —— 在那两处再挂一条就是两个框。
ui.paint:hint("tab", { cursor = "hand", tip = "@text" })
ui.paint:hint("dock.tab", { cursor = "hand", tip = "@text" })
