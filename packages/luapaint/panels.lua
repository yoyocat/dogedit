-- LuaPaint 的第二半：**元素** —— 工具栏按钮、三块窗格、状态栏那一条。
--
-- 和 `paint.lua` 分开是为了演示：同一个包的多个文件共用一个 `_ENV`（所以
-- `skin.clock_format` 在这里注册、`paint.lua` 里那个按钮照样点得到），但
-- **文件级的 `local` 不共享** —— 所以下面还要再写一次 `local ui = App.ui`。
--
-- 分工就是这一句：`paint.lua` 只改「已有的东西长什么样」，这个文件只声明
-- 「多出哪些东西」。它给 `pane.<id>` 画的样子也在这里，因为那是元素自己的
-- 外观，跟着元素走。
--
-- 顺序由 `package.toml` 的 `scripts` 定：`paint.lua` 在前。

local ui = App.ui

-- 往工具栏加一个按钮。
--
-- **追加，不是整条替换**：那四十几个按钮是用户自己的（`[ToolBar]` 里存着他
-- 排过的顺序），皮肤只有「加一格」的权力，没有重排的权力。热重载按脚本块清，
-- 所以重跑一遍还是这一个。
--
-- `icon` 是精灵格号 —— 和页签图标、状态栏图标是同一张图，不用另备素材。
-- `after` 是命令号（插在那一格后面），`0` / 不写就是末尾。这里插在「新建」
-- （57600）后面，第一格旁边，一定看得见。
--
-- 点下去走的是 `App.command` 那条路：脚本命令优先，没有就落到内置命令表，
-- 所以 `command = "file.save"` 这种写法也能用。
ui.layout:toolbar{ icon = 60, tip = "切换状态栏时钟制式", command = "skin.clock_format", after = 57600 }

-- ---------------------------------------------------------------------------
-- 脚本窗格（`ui.layout:pane`）：皮肤自己长出来的停靠窗格。
--
-- 上面那些画法都是「接管一个已有的区域」。这一块不一样：它是**新加的**，和
-- 目录树、输出窗口、大纲并列 —— 有页签、能拖到四条边、能右键关掉，区别只有
-- 一处：内容没有内置画法，全归脚本。
--
-- 声明只管结构（几块、叫什么、停哪边、多厚），画法仍走 `ui.paint:on("pane.<id>")`。
-- 这样绘制还是那套「纯函数 + 缓存」，和别处一模一样。
-- ---------------------------------------------------------------------------

-- 一块窗格的外壳：底 + 左边一道色条。`ctx.hot` 是指针停在这块窗格里。
-- `ctx.focus` 是这块窗格拿着键盘焦点 —— 点一下它就有了，画一圈亮边。
local function pane_shell(c, ctx, accent)
  c:fill(ctx.rect, ctx.hot and "#16212B" or "#0E151B")
  c:fill(ctx.rect:with_width(3), accent)
  if ctx.focus then
    c:rect(ctx.rect, accent, 1)
  end
end

-- 一块窗格里的两行字：第一行大、第二行小。
local function pane_lines(c, ctx, big, small, ink)
  local w = math.max(0, ctx.w - 20)
  c:text({ x = 10, y = 12, w = w, h = 30 }, big, ink, { valign = "top" })
  c:text({ x = 10, y = 44, w = w, h = 20 }, small, "#5C7A8A", { valign = "top" })
end

-- 时钟：从宿主创建算起跑了多久。
--
-- 沙箱里没有 `os`，`App.now()` 是唯一的时钟（宿主创建以来的毫秒数，单调）。
-- 它每毫秒都在变，所以这一块**不进缓存**、每帧重算 —— 文件开头那个 50 ms 心跳
-- 负责叫醒，位置和数字都在这里现读。
ui.layout:pane{ id = "uptime", side = "right", title = "时钟", icon = 60, size = 200 }

ui.paint:on("pane.uptime", function(c, ctx)
  pane_shell(c, ctx, "#FFD166")
  local total = math.floor(App.now() / 1000)
  local h = math.floor(total / 3600) % 24
  local m = math.floor(total / 60) % 60
  local s = total % 60
  pane_lines(
    c,
    ctx,
    string.format("%02d:%02d:%02d", h, m, s),
    string.format("已运行 %d 分 %d 秒", math.floor(total / 60), s),
    "#FFD166"
  )
  return true
end)

-- 文档信息：`ctx.doc` 是外壳每帧推过来的只读快照（这一档没有 bridge）。
--
-- 只读快照里没有「字数」—— 数一遍要走整份文档，放进每帧的快照等于给打字加一道
-- 全量扫描。要看字数就在点击回调里问 `App.doc.meta()`（那一档有 bridge）。
ui.layout:pane{ id = "docinfo", side = "right", title = "文档信息", icon = 45, size = 200 }

ui.paint:on("pane.docinfo", function(c, ctx)
  pane_shell(c, ctx, "#7FD1B9")
  local d = ctx.doc
  local kind = d.syntax ~= "" and d.syntax or "纯文本"
  local marks = {}
  if d.modified then
    marks[#marks + 1] = "已修改"
  end
  if d.read_only then
    marks[#marks + 1] = "只读"
  end
  if d.large then
    marks[#marks + 1] = "大文件"
  end
  local tail = #marks > 0 and table.concat(marks, " · ") or "未修改"
  pane_lines(
    c,
    ctx,
    string.format("%d 行 · %s", d.lines, kind),
    string.format("%s · %s", d.eol, tail),
    "#7FD1B9"
  )
  return true
end)

-- 点一下文档信息那一块：在 stderr 里打一份完整统计。
-- 点击是 Intent 档（有 bridge），所以能问 `App.doc.meta()`。
ui.on("pane.docinfo", "press", function()
  local m = App.doc.meta()
  print(string.format("文档：%d 行，%d 字节，%s", m.lines, m.bytes, m.eol))
  return true
end)

-- 键盘：`App.keymap` 就是 dg-script 的键盘 API，没有另一套「皮肤快捷键」。
--
-- **裸键只有窗格拿着焦点时才生效**。编辑区里裸字母必须落进文档，所以那边只认
-- 带 Ctrl/Alt 的组合（这条闸门在派发里，脚本改不动）。点一下「文档信息」这块
-- 窗格，焦点就过来了，这时按 `r` 才算数；点回正文，`r` 又变回打字。
local doc_reads = 0
App.keymap("r", "skin.refresh_docinfo")
App.command("skin.refresh_docinfo", function()
  doc_reads = doc_reads + 1
  local m = App.doc.meta()
  print(string.format("[窗格键 r] 第 %d 次：%d 行 %d 字节", doc_reads, m.lines, m.bytes))
end)

ui.paint:hint("pane.uptime", { tip = "宿主创建以来的运行时间" })
ui.paint:hint("pane.docinfo", { tip = "点一下拿焦点，然后按 r 刷新统计" })

-- ── 会滚的窗格：固定表头 + 长列表 ──────────────────────────────────────────
--
-- 内容比窗格高的时候能滚。三件事分工明确：
--
-- * **内容高**由 `ui.paint:on` 的返回值报（`return 表头 + 行数 * 行高`）—— 数字在
--   Lua 里本来就算真值，所以写 `return 数字` 和写 `return true` 只差这一条信息。
--   写死在 `ui.layout:pane` 里不行：时钟、日志这类内容每帧都可能变高。
-- * **几何**由宿主算：`ctx.bar` 是轨道、`ctx.thumb` 是滑块、`ctx.scroll` 是偏移，
--   而且 `ctx.rect` 已经减掉轨道宽，所以内容区不会画到轨道底下。
-- * **像素**归这里：滚轮、拖滑块、点轨道翻页都是宿主接的（`App.ui.on` 的 ctx 里没有
--   指针坐标，皮肤没法自己算拖到哪儿了），皮肤只管填色。
--
-- 偏移是皮肤自己减的，不是宿主替它平移 —— 好处就是下面这个表头：那一行不加
-- `ctx.scroll`，于是钉在顶上；列表加了，于是滚。
local help_lines = {
  "ui.paint:on         接管任意区域的画法",
  "ui.paint:hint       悬停提示（声明式）",
  "App.ui.on           点击 / 右键 / 拖放",
  "ui.layout:pane      新长出来的停靠窗格",
  "ui.layout:toolbar   工具栏追加一个按钮",
  "ui.layout:status    整条状态栏由脚本给",
  "ui.layout:defaults  把内置那条读出来",
  "App.keymap          键盘（裸键要焦点）",
  "App.command         按名字注册命令",
  "App.timer           定时回调",
  "App.doc             文档信息与元数据",
  "ctx.theme           89 个颜色 token",
}

ui.layout:pane{ id = "help", side = "bottom", title = "滚动示例", icon = 111, size = 170, bar = 8 }

-- 表头高、行高、内边距都在这儿：内容高要按它们算，所以只能有一份。
local HELP_HEAD = 40
local HELP_ROW = 18
local HELP_PAD = 10

ui.paint:on("pane.help", function(c, ctx)
  pane_shell(c, ctx, "#8AB4F8")
  local w = math.max(0, ctx.w - HELP_PAD * 2)
  -- 列表先画（而且要 `clip` 到表头以下）：滚上去的行会盖在表头上，`ctx.scroll`
  -- 只管位置，裁剪得自己说。`c:clip` 是 `IntersectClipRect`，可以套好几层。
  c:clip({ x = 0, y = HELP_HEAD, w = ctx.w, h = math.max(0, ctx.h - HELP_HEAD) })
  for i, line in ipairs(help_lines) do
    local y = HELP_HEAD + (i - 1) * HELP_ROW - ctx.scroll
    c:text({ x = HELP_PAD, y = y, w = w, h = HELP_ROW }, line, "#B8C7D4", { valign = "top" })
  end
  c:unclip()
  -- 表头：**不加** `ctx.scroll`，所以它钉在顶上。
  c:text({ x = HELP_PAD, y = 10, w = w, h = 20 }, "LuaPaint 能改的地方", "#8AB4F8", { valign = "top" })
  c:fill({ x = HELP_PAD, y = 32, w = w, h = 1 }, "#2A3A47")
  -- 轨道和滑块：矩形是宿主给的，这里只填色。指针停在窗格里就亮一点。
  if ctx.bar.w > 0 then
    c:fill(ctx.bar, "#0A1116")
    c:fill(ctx.thumb, ctx.hot and "#8AB4F8" or "#4A6B85")
  end
  return HELP_HEAD + #help_lines * HELP_ROW + HELP_PAD
end)

ui.paint:hint("pane.help", { tip = "滚轮、拖滑块、点轨道都能滚" })

-- ── 状态栏：在内置那条后面加两格 ──────────────────────────────────────────
--
-- `ui.layout:defaults("status")` 给的是内置那十三个字段（消息、大小、选区、语法、行列…），
-- 顺序和内置那条**一模一样**；`table.insert` 加的东西就排在它们后面。
--
-- 不调 `ui.layout:status` 的话状态栏一个字都不变 —— 声明是「接管整条」，不是「追加」。
-- 所以要追加，第一步就是先把内置那条读出来。
local row = ui.layout:defaults("status")

-- 一格纯模板。`%p` 是光标在全文的百分比 —— 内置那十三个字段里没有这一格，
-- 但模板能表达，所以一行就够，不用写画法。`width` 夹在 40..56 DIP：
-- 百分比永远是一两位数字，给个区间比给个死数稳（见 `dg_skin::Width`）。
table.insert(row, {
  id = "pct",
  text = "%p%%",
  width = { min = 40, max = 56 },
  priority = 25,
})

-- 一格回调。`update(ctx)` 和 `ui.paint:on` 拿的是同一份只读上下文（`ctx.doc`、
-- `App.now()`、`ctx.theme`），但它**只返回文字**、不画 —— 画法归下面那段。
--
-- `priority` 是「窗口窄了先丢谁」：数字越小越先丢，时钟是锦上添花，给它 5。
-- `click` 是点一下跑的命令名 —— 脚本命令和内置命令同一张表。
local store = App.storage("lua_clock")
local clock24 = store:get("h24") ~= "0"

table.insert(row, {
  id = "clock",
  -- 宽度交给文字自己：`[metrics] status.pad = 9` 和下面画法的 `inset(9, 0)` 是
  -- 同一个数，宿主量宽度时左右各留一份，所以「12:00 AM」这种长一点的文字
  -- 也不会被 `ellipsis` 吃掉。`max` 只是别让它把邻居挤走。
  width = { max = 104 },
  priority = 5,
  click = "skin.clock_format",
  update = function()
    local t = math.floor(App.now() / 1000) % 86400
    local h = math.floor(t / 3600)
    local m = math.floor(t / 60) % 60
    if clock24 then
      return string.format("%02d:%02d", h, m)
    end
    return string.format("%d:%02d %s", h % 12 == 0 and 12 or h % 12, m, h < 12 and "AM" or "PM")
  end,
})

App.command("skin.clock_format", function()
  clock24 = not clock24
  store:set("h24", clock24 and "1" or "0")
end)

ui.layout:status(row)

-- 给时钟那一格画自己的样子：区域名就是 `status.<id>`。
--
-- 内置那条 `status.pane` 的画法**同时**还挂着（见文件开头那段）：没声明段名的
-- 地方都走它。一格一格接管，是这套东西的常规用法。
--
-- 这里读了 `ctx.text` 并按文字分支（`AM`/`PM` 决定颜色）。读过的字段缓存会连带
-- 比对 —— 所以点一下切了制式，颜色跟着换。推荐写法是不读、直接
-- `c:text(r, "@text", …)`：那样时间变了也照样命中，`@text` 在回放时才替换。
ui.paint:on("status.clock", function(c, ctx)
  local r = ctx.rect
  local ink = ctx.text:find("M") and "#7FD1B9" or "#FFD166"
  c:gradient(r, "#16212B", "#0E151B", "v")
  c:fill(r:with_width(3), ink)
  c:text(r:inset(9, 0), "@text", ink, { valign = "middle", ellipsis = true })
  c:line(r.right - 1, r.y + 4, r.right - 1, r.bottom - 4, "#1E3A46", 1)
  return true
end)
