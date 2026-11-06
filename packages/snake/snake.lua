-- 贪吃蛇：一份**完全用脚本 API 写出来**的小程序，用来展示 Lua 层能做什么。
--
-- 用到的东西：
--   ui.layout:pane   新长出一块停靠窗格（真的 HWND，能拿键盘焦点）
--   ui.paint:on      每帧用画布原语画棋盘、蛇、食物和 HUD
--   App.timer        心跳：在回调里推进游戏状态，再 App.ui.repaint 叫一次重画
--   App.keymap       方向键 / WASD / 空格 / R（具名键由 dg-script 解析）
--   App.command      命令表：菜单、快捷键和 ui.on 都走同一条路
--   App.storage      最高分落盘，重启还在
--   ui.on / :hint    点一下取焦点，悬停提示
--
-- 规矩只有一条：**状态在意图档改，画法只读不算**。绘制回调里没有 bridge、
-- 也不该有随机数；所以 `math.random` 只在 `spawn_food` 里出现，而那是从
-- 定时器 / 命令里调的。画法输入没变时宿主会复用上一份 op 列表，
-- `App.ui.repaint("pane.snake")` 负责在状态变了之后把那份缓存丢掉。

local ui = App.ui

local COLS, ROWS = 20, 18
local HEAD_H, FOOT_H, PAD = 46, 26, 8
local TICK_MS = 110

local C = {
  bg = "#0E151B",
  board = "#111E27",
  grid = "#1C2E39",
  border = "#2C4857",
  head = "#6BE6A2",
  body = "#2FA978",
  body_alt = "#258A62",
  food = "#FF6B6B",
  ink = "#D7E6EE",
  dim = "#71909F",
  accent = "#FFD166",
  warn = "#FF9A93",
  panel = "#172A35",
}

local store = App.storage("snake")
local best = tonumber(store:get("best") or "0") or 0

-- phase: ready（等开始）/ run / pause / over / win
local snake, dir, pending, food, score, phase

local function finish(next_phase)
  phase = next_phase
  if score > best then
    best = score
    store:set("best", tostring(best))
  end
end

local function occupied(x, y)
  for i = 1, #snake do
    if snake[i].x == x and snake[i].y == y then
      return true
    end
  end
  return false
end

local function spawn_food()
  local free = {}
  for y = 1, ROWS do
    for x = 1, COLS do
      if not occupied(x, y) then
        free[#free + 1] = { x = x, y = y }
      end
    end
  end
  if #free == 0 then
    food = nil
    finish("win")
    return
  end
  food = free[math.random(#free)]
end

local function reset()
  snake = { { x = 7, y = 9 }, { x = 6, y = 9 }, { x = 5, y = 9 } }
  dir = { x = 1, y = 0 }
  pending = {}
  score = 0
  phase = "ready"
  -- 第一颗食物写死，开局的路径是可预期的；吃到之后才随机。
  food = { x = 14, y = 9 }
end

local function begin()
  if phase == "over" or phase == "win" then
    reset()
  end
  if phase == "ready" or phase == "pause" then
    phase = "run"
  end
end

-- 一跳里最多收两次转向，且不许直接掉头。用待处理队列而不是立刻改 dir，
-- 是为了「快速连按上、左」不会在同一帧里穿过自己的脖子。
local function turn(dx, dy)
  local last = pending[#pending] or dir
  if (last.x == -dx and last.y == -dy) or (last.x == dx and last.y == dy) then
    return
  end
  if #pending < 2 then
    pending[#pending + 1] = { x = dx, y = dy }
  end
end

local function step()
  if phase ~= "run" then
    return
  end
  if #pending > 0 then
    dir = table.remove(pending, 1)
  end
  local head = snake[1]
  local nx, ny = head.x + dir.x, head.y + dir.y

  if nx < 1 or nx > COLS or ny < 1 or ny > ROWS then
    finish("over")
    return
  end

  local eating = food ~= nil and nx == food.x and ny == food.y
  -- 不吃的时候尾巴这一格会让出来，所以最后一节不算碰撞。
  local last = eating and #snake or (#snake - 1)
  for i = 1, last do
    if snake[i].x == nx and snake[i].y == ny then
      finish("over")
      return
    end
  end

  table.insert(snake, 1, { x = nx, y = ny })
  if eating then
    score = score + 10
    spawn_food()
  else
    table.remove(snake)
  end
end

-- ---------------------------------------------------------------------------
-- 命令：菜单、快捷键、点击和自检都落在这张表上。
-- ---------------------------------------------------------------------------

local function redraw()
  -- 点名这一块：宿主只作废这块窗格的缓存和窗口，工具栏/状态栏不必陪着重画。
  ui.repaint("pane.snake")
end

local DIRECTIONS = {
  up = { 0, -1 },
  down = { 0, 1 },
  left = { -1, 0 },
  right = { 1, 0 },
}
for name, d in pairs(DIRECTIONS) do
  App.command("snake." .. name, function()
    begin()
    turn(d[1], d[2])
    redraw()
  end)
end

App.command("snake.toggle", function()
  if phase == "run" then
    phase = "pause"
  else
    begin()
  end
  redraw()
end)

App.command("snake.restart", function()
  reset()
  phase = "run"
  redraw()
end)

-- 定时器回调走的是同一份 step；把单步也注册成命令，headless 测试就能
-- 不靠真实时钟把一局跑完（--ui-lint 和引擎测试都用它）。
App.command("snake.step", function()
  step()
  redraw()
end)

App.command("snake.state", function()
  local h = snake[1]
  print(string.format(
    "snake: phase=%s score=%d best=%d len=%d head=%d,%d food=%d,%d",
    phase, score, best, #snake, h.x, h.y,
    food and food.x or 0, food and food.y or 0
  ))
end)

-- 心跳只在真的在跑的时候请求重画。静止时绘制缓存继续生效，别白烧一帧。
App.timer(TICK_MS, function()
  if phase ~= "run" then
    return
  end
  step()
  redraw()
end, true)

-- ---------------------------------------------------------------------------
-- 键盘：具名键（Up/Down/Left/Right/Space）和裸字母共用一套 App.keymap。
-- 编辑区只认 Ctrl/Alt 组合；点一下窗格让焦点过来，方向键和 WASD 才生效。
-- ---------------------------------------------------------------------------

local KEYS = {
  Up = "snake.up",
  Down = "snake.down",
  Left = "snake.left",
  Right = "snake.right",
  w = "snake.up",
  s = "snake.down",
  a = "snake.left",
  d = "snake.right",
  Space = "snake.toggle",
  p = "snake.toggle",
  r = "snake.restart",
}
for gesture, command in pairs(KEYS) do
  App.keymap(gesture, command)
end

-- ---------------------------------------------------------------------------
-- 元素与画法
-- ---------------------------------------------------------------------------

ui.layout:pane{ id = "snake", side = "right", title = "贪吃蛇", icon = 0, size = 300 }

ui.on("pane.snake", "press", function()
  -- 宿主在派发之前已经把焦点给了这块窗格；这里只管开局。
  if phase ~= "run" then
    begin()
  end
  redraw()
  return true
end)

ui.paint:hint("pane.snake", { cursor = "arrow", tip = "点一下拿焦点，方向键 / WASD 玩" })

local function geometry(ctx)
  local avail_w = ctx.w - PAD * 2
  local avail_h = ctx.h - HEAD_H - FOOT_H - PAD * 2
  local cell = math.max(4, math.floor(math.min(avail_w / COLS, avail_h / ROWS)))
  local bw, bh = cell * COLS, cell * ROWS
  local x = math.floor((ctx.w - bw) / 2)
  local y = HEAD_H + math.floor((ctx.h - HEAD_H - FOOT_H - bh) / 2)
  return x, y, cell, bw, bh
end

local PHASE_TEXT = {
  ready = { "按空格开始", "方向键 / WASD 转向 · R 重开" },
  pause = { "已暂停", "空格继续 · R 重开" },
  over = { "撞到了！", "空格 / R 再来一局" },
  win = { "通关！", "空格 / R 再来一局" },
}

ui.paint:on("pane.snake", function(c, ctx)
  local bx, by, cell, bw, bh = geometry(ctx)

  c:fill(ctx.rect, C.bg)

  -- HUD：分数、最高分、当前状态 / 焦点提示。
  c:text({ x = PAD, y = 4, w = ctx.w - PAD * 2, h = 20 },
    string.format("贪吃蛇    分数 %d    最高 %d", score, best),
    C.ink, { valign = "middle" })
  local label = ({ ready = "准备", run = "进行中", pause = "暂停", over = "结束", win = "通关" })[phase]
  c:text({ x = PAD, y = 24, w = ctx.w - PAD * 2, h = 18 },
    label .. (ctx.focus and "  ·  键盘已就位" or "  ·  点一下拿焦点"),
    ctx.focus and C.head or C.dim, { valign = "middle" })

  -- 棋盘：一圈边、格线画在格子里，不动的地方全靠缓存。
  c:fill({ x = bx - 2, y = by - 2, w = bw + 4, h = bh + 4 }, C.border)
  c:fill({ x = bx, y = by, w = bw, h = bh }, C.board)
  for i = 1, COLS - 1 do
    c:line(bx + i * cell, by, bx + i * cell, by + bh, C.grid)
  end
  for j = 1, ROWS - 1 do
    c:line(bx, by + j * cell, bx + bw, by + j * cell, C.grid)
  end

  -- 食物：一个圆点，比方块好认。
  if food then
    c:circle(bx + (food.x - 0.5) * cell, by + (food.y - 0.5) * cell,
      math.max(2, cell * 0.32), C.food)
  end

  -- 蛇：从尾巴往头画，头最后盖在最上面。
  local inset = cell > 8 and 1 or 0
  for i = #snake, 1, -1 do
    local s = snake[i]
    local color = C.body
    if i == 1 then
      color = C.head
    elseif i % 2 == 0 then
      color = C.body_alt
    end
    c:round({
      x = bx + (s.x - 1) * cell + inset,
      y = by + (s.y - 1) * cell + inset,
      w = cell - inset * 2,
      h = cell - inset * 2,
    }, math.max(1, cell * 0.22), color)
  end

  -- 非运行态：棋盘中间压一块说明。GDI 没有半透明，就用实心面板。
  local note = PHASE_TEXT[phase]
  if note then
    local w = math.min(bw, 230)
    local box = {
      x = bx + math.floor((bw - w) / 2),
      y = by + math.floor(bh * 0.36),
      w = w,
      h = 62,
    }
    c:fill(box, C.panel)
    c:rect(box, C.border, 1)
    local title = note[1]
    if phase == "over" or phase == "win" then
      title = string.format("%s  得分 %d", title, score)
    end
    c:text({ x = box.x, y = box.y + 4, w = box.w, h = 28 }, title,
      phase == "over" and C.warn or C.accent, { align = "center", valign = "middle" })
    c:text({ x = box.x, y = box.y + 32, w = box.w, h = 22 }, note[2],
      C.dim, { align = "center", valign = "middle" })
  end

  -- 焦点框：拿到键盘时亮一圈，提示「现在按键是给游戏的」。
  if ctx.focus then
    c:rect({ x = 0, y = 0, w = ctx.w, h = ctx.h }, C.accent, 1)
  end

  c:text({ x = PAD, y = ctx.h - FOOT_H + 2, w = ctx.w - PAD * 2, h = FOOT_H - 4 },
    "方向键 / WASD 转向 · 空格 开始/暂停 · R 重开",
    C.dim, { align = "center", valign = "middle", ellipsis = true })

  return true
end)

math.randomseed(math.floor(App.now()) + 7919)
reset()
