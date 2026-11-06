# 用 Lua 画界面、建元素

主题文件（`themes/<name>.ini`）管颜色、尺寸、形状、字体；**要改「长什么样」、
要新加一块东西，得写 Lua**。这一页是那套 API 的完整接口。

```
dogedit                                  # 仓库里的 packages/luapaint、packages/snake 会被自动装上
dogedit --packages                       # 不开窗，只报扫到哪些包、能不能装、按什么顺序
dogedit --ui-lint packages/luapaint/paint.lua   # 不开窗，把每个画法跑一遍并报账（见五之十六）
dogedit --ui-lint packages/snake/snake.lua     # 贪吃蛇窗格也能离线跑一遍画法
```

脚本放在两处，**跑在同一个 Lua 状态里**：

| 位置 | 是什么 | 清单 |
| --- | --- | --- |
| `packages/<id>/` | 包 | `package.toml`，`scripts` 定跑哪些文件、按什么顺序 |
| `scripts/*.lua` | 还没打包的散装脚本 | 没有，包名 = 文件名 |

清单里写了 `launch = "文字"` 的包是**按需启动**的：开机一条都不跑，只在「扩展」菜单占一行，点它才装载；关掉它最后一块窗格就整包退出（注册全摘 + Lua GC）。所以 `--ui-lint packages/snake/snake.lua` 能离线单跑那份画法，不等于开机时蛇已经在跑。详见 `script_api.html` 第 15 章。

`App.ui` 下面是四张表和一个函数，各管一件事 —— 这是全文的前提：

| 表 / 函数 | 管什么 | 一节 |
| --- | --- | --- |
| `App.ui.paint` | 接管某个区域怎么画 | 一 ~ 五之八 |
| `App.ui.layout` | 声明多出哪些元素（窗格 / 工具栏按钮 / 状态格） | 五之十三 ~ 五之十五 |
| `App.ui.on` / `off` | 点击、右键、拖放 | 五之一 |
| `App.ui.repaint([区域名])` | 状态改完后丢绘制缓存、请求重画 | 五之二、六 |
| `App.ui.style` | 运行时改外观：颜色 / 度量 / 形状 / 字体 | 五之十七 |

`App.ui.layout` 自己还有四个动词管**运行时**：`:update` / `:remove` / `:list`
（五之十八）。状态格的数据源不是第五张表 —— 它是 `ui.layout:status` 里那一格
自己的 `update` 函数（五之十四）。

`App.ui.repaint` 不是第五张表，而是一个函数：命令、事件、定时器改了 Lua
upvalue 之后点名一块区域（常用 `"pane.<id>"`），宿主丢这份绘制缓存并作废对应
窗口；不给名字或给别的区域名则退回全量重画。绘制回调保持只读，别在里面改状态
或调 `repaint`。点击回调例外：宿主本来就自动丢这一块的缓存。

`App.theme` 不在这套 UI API 里：它是**只读**的当前主题（`App.theme.name()` /
`.color(token)` / `.list()`），给「想跟着主题走、但不在绘制回调里」的脚本用。
绘制回调里读 `ctx.theme` 更快。

## 一、最小的一个

```lua
local ui = App.ui

ui.paint:on("status.pane", function(c, ctx)
  c:fill(ctx.rect, "#203040")
  c:text(ctx.rect:inset(6, 0), "@text", "#00FF88", { ellipsis = true })
  return true
end)
```

`"status.pane"` 是区域名（见第四节）。回调收到两块东西：画布 `c` 和只读上下文
`ctx`。**返回 `true` 就是「这一块我画了」**，内置画法让位。

## 二、返回值决定谁画

| 返回 | 含义 |
| --- | --- |
| `true` | 这一块完全由脚本负责，内置画法不跑 |
| `false` / `nil` | 交回内置画法。已经发出去的指令**不会**被用 |
| 抛错 | 记一行日志到 stderr，交回内置画法 |

第三行是刻意的：**一个坏皮肤不该让窗口白屏**。所以 `return false` 是一条安全
退路 —— 想只改某一格、其余保持原样时就这么写。

绘制期**没有 bridge**：回调里 `App.doc.*`、`App.sel.*` 一律报错，报错又按
「没画」处理。这不是约定，是没给工具 —— `EditorState` 在 `WM_PAINT` 期间正被
借走，重入是硬约束。

## 三、画布 `c`

坐标是 **DIP**（96 DPI 的像素），回放层按窗口 DPI 放大。颜色写 `"#RRGGBB"`。

| 调用 | 说明 |
| --- | --- |
| `c:fill(rect, color)` | 实心填充 |
| `c:rect(rect, color[, width])` | 描边，画在矩形内侧 |
| `c:round(rect, radius[, fill[, stroke[, stroke_w]]])` | 圆角矩形 |
| `c:line(x1, y1, x2, y2, color[, width])` | 直线 |
| `c:poly({ {x=,y=}, ... }, color[, width[, closed]])` | 折线/多边形，点也可写成 `{x, y}` |
| `c:circle(cx, cy, r[, fill[, stroke[, stroke_w]]])` | 圆 |
| `c:gradient(rect, from, to[, dir])` | 线性渐变，`dir` 是 `"v"`（默认）或 `"h"` |
| `c:text(rect, source, color[, opts])` | 一段文字，见下 |
| `c:image(rect, index, fit[, tint])` | 图标格里的一格，`fit` = `stretch`/`tile`/`center` |
| `c:clip(rect)` / `c:unclip()` | 压栈/弹栈裁剪 |
| `c:shadow(rect[, radius[, color[, blur]]])` | 外阴影（纯 GDI 回放时跳过） |
| `c:spacer()` | 占位，回放时忽略 |
| `c:count()` | 已经发了几条指令，脚本自检用 |

`c:text` 的 `source` 有三种**占位符**，其余一律当字面量：

- `"@text"` —— 这个部件当前的文字（状态格内容、页签标题）
- `"@label"` —— 部件的标签（菜单项名字，去掉快捷键列）
- `"@value"` —— 部件的数值（快捷键列、百分比）

占位符是缓存能活下来的关键：状态栏的「行 12，列 34」每帧都变，字面量进了缓存键
就永远命中不了；用占位符之后指令列表只随尺寸和状态变。

`opts`：`{ font = "Consolas", align = "center", valign = "top", ellipsis = true, clip = rect }`。

## 四、上下文 `ctx`

| 字段 | 说明 |
| --- | --- |
| `ctx.rect` | 这一块的矩形，带 `:inset(dx[,dy])` / `:outset` / `:with_width(w)` / `:with_height(h)` / `:move(dx,dy)`，以及 `x`/`y`/`w`/`h`/`right`/`bottom` |
| `ctx.thumb` | 块里那个「会动的部件」的矩形，同样带上面那套方法。滚动条是滑块，其它区域等于 `ctx.rect` —— 一律按「有就用、没有就和 `rect` 一样」写，不必先判断区域名 |
| `ctx.bar` | 竖直滚动条的整条轨道，同样带上面那套方法。只有声明了 `bar` 的脚本窗格会有值，别的区域是**空矩形**（`w == 0`）—— 写 `if ctx.bar.w > 0 then` 一处判断就够（见「五之十三」的「滚动」） |
| `ctx.scroll` | 竖直滚动位置（DIP），已经夹好。**由皮肤自己减**，别的区域恒 0 |
| `ctx.w` / `ctx.h` | `ctx.rect` 的宽高，省得每次写 `ctx.rect.w`。跟着 `rect` 走，读它不加缓存条件 |
| `ctx.part` | 部件名，比如 `"message"`、`"encoding"`、`"ln"`；滚动条是 `"v"` / `"h"`；工具栏是命令名 `cm_file_save` 这种 |
| `ctx.text` / `ctx.label` / `ctx.value` | 占位符对应的文字 |
| `ctx.dpi` | 这一块解析到的 DPI |
| `ctx.hot` / `ctx.active` / `ctx.pressed` / `ctx.disabled` / `ctx.checked` / `ctx.focus` / `ctx.selected` / `ctx.modified` / `ctx.alt` | 9 个布尔状态位，互相独立（按下并且改过并且有焦点是同时成立的） |
| `ctx.theme["status.bg"]` | 当前主题的 89 个 token，值是 `"#RRGGBB"`。皮肤换主题时这张表跟着变，不用重新注册 |
| `ctx.doc` | 当前文档的只读快照（见下），8 个字段 |
| `ctx.rows` | 只有 `editor.overlay` 有：可见行表（见「五之十一」）；别的区域是空数组，`#ctx.rows == 0` |
| `ctx.metrics` | 编辑器字体的度量：`x_width` / `cjk_width` / `font_height` / `line_height` / `gutter_x` / `line_number_x` / `line_number_w`。同样只有编辑区那两个区域有 |

`ctx.theme` 是**同一张表**：`set_palette` 换主题时清空重灌，所以已经拿在手里的
引用也跟着变。

`ctx.doc` 是**外壳每帧推一次的快照**，不是查询接口 —— 绘制档没有 bridge，问不了
`App.doc.meta()`，所以答案提前摆好：

```lua
if ctx.doc.modified then ... end        -- 改过没保存
if ctx.doc.large then ... end           -- 大文件模式（没有语法高亮、没有折叠）
if ctx.doc.eol == "CRLF" then ... end   -- "LF" / "CRLF" / "CR"
```

字段：`encoding`、`bom`、`eol`、`syntax`（`"rs"`/`"py"`，没挂语法是空串）、
`lines`、`read_only`、`modified`、`large`。

- **没有快照时是一张空表**，读字段得 `nil`（假），不报错 —— `if ctx.doc.modified`
  这种写法不用先判空。
- 比 `App.doc.meta()` **少一个 `bytes`**：算总字节要走字节索引，那东西一改就整表
  重建，放进每帧的快照等于给打字加一道全量扫描。要总字节用 `App.doc.meta()`。
- **每读一次 `ctx.doc` 建一张表**：写 `local d = ctx.doc` 存起来，别在循环里反复读。
- 读了它，缓存就跟着文档走：改一个字、存一次盘都会重算这一格；没读的画法不受影响
  （见「六、缓存」）。

## 五、注册与撤销

```lua
local ui = App.ui
ui.paint:on("tab", handler)     -- 注册（同名覆盖）
ui.paint:off("tab")            -- 撤销，回到内置画法
ui.paint:list()                 -- 已经注册了回调的区域名，排过序
ui.on("tab", "press", fn)    -- 点击回调，见「五之一」
ui.off("tab", "press")      -- 撤掉
ui.paint:hint("tab", { cursor = "hand", tip = "@text" })   -- 指针和提示，见「五之七」
ui.paint:unhint("tab")             -- 撤掉
ui.layout:pane{ id = "clock", side = "right", title = "时钟", size = 220 }  -- 新建停靠窗格，见「五之十三」
ui.layout:status{ { id = "clock", update = fn }, { width = "fill" } }        -- 接管整条状态栏，见「五之十四」
ui.layout:defaults("status")         -- 内置那十三个字段，可以改了再传回去
ui.layout:status()                  -- 读回当前声明（不带参数）
ui.layout:toolbar{ icon = 60, command = "skin.clock" }   -- 工具栏加一格，见「五之十五」
ui.layout:toolbar()                 -- 读回已经声明的那几个
```

区域名与主题 token 的前缀、`dg_skin::Region` 是同一套词。已经接上绘制的是：

| 区域 | 绘制 | `cursor` | `tip` |
| --- | --- | --- | --- |
| `status.pane` | ✅ 每一格（`ctx.hot` 是指针停在这一格上） | ✅ | 控件的（文字放不下时自己弹） |
| `status.<id>` | ✅ 一格，`<id>` 是 `ui.layout:status` 声明的那一段（见「五之十四」） | ✅ | 控件的 |
| `tab` | ✅ 每一格（`ctx.text` 是标题，`active`/`hot`/`modified` 是这一格自己的） | ✅ | ✅ |
| `scroll.v` / `scroll.h` | ✅ 整条（`ctx.thumb` 是滑块，`hot` 是指针在条上或正在拖） | ✅ | — |
| `toolbar.button` | ✅ 每一格（见「五之五」：**底子归你，图标归控件**） | ✅ | 控件的（问 `cmdtext`） |
| `dock.tab` | ✅ 停靠窗格那条页签的每一格（见「五之六」，同样是**底子归你**） | ✅ | ✅ |
| `editor.overlay` | ✅ **整屏一次**，`ctx.rows` 是可见行表（见「五之十一」） | — | — |
| `editor.gutter` | ✅ **整列一次**，返回 true 就替掉七个内置组件（见「五之十二」） | — | — |
| `pane.<id>` | ✅ **整块一次**，`<id>` 是 `ui.layout:pane` 声明的那个名字（见「五之十三」） | ✅ | ✅ |
| `statusbar` / `tab.glyph` / `menu.row` | 词表里有，还没接 | — | — |

注册一个还没接上的区域不报错，只是不会有人调它 —— `ui.paint:list()` 里看得到。
但**名字写错**（词表里没有这个词）会在 `ui.paint:on` / `ui.paint:hint` 那一行当场报错：
以前是静默收下，那种画法永远进不了缓存键，于是每帧重跑一遍 Lua —— 画面对，
代价白付，比报错难查得多。

## 五之一、点击

自己画的格子当然要能点。区域那边已经算好了「点到哪一格」，脚本只要接住：

```lua
local ui = App.ui
ui.on("tab", "press", function(ctx) ... return true end)   -- 左键
ui.on("tab", "rclick", function(ctx) ... return true end)  -- 右键
ui.off("tab", "press")                                    -- 撤掉
```

`ctx` 和绘制时那份**一模一样**（`rect`/`part`/`text`/9 个状态位/`theme`），只是
没有画布 —— 点击不是画。

返回值决定谁做事：

| 返回 | 意思 |
| --- | --- |
| `true` | 我接管了：内置动作（切页签、弹右键菜单）**不执行** |
| `false` / `nil` | 我看过了，内置的照做（默认行为不变） |
| 抛错 | 记一行日志，按「让位」处理 —— 画错的皮肤不该让页签点不动 |

- 事件名只有 `"press"` 和 `"rclick"`，写错当场报错。
- **点击是 Intent 档，不是绘制档**：它发生在 `WM_LBUTTONDOWN` 里，那时
  `EditorState` 没被借走，所以 handler 里 `print`、`App.run`、`App.doc.meta()`
  都能用（绘制期不行，那是重入）。想在点击里改文档，`App.run("cm_...")` 排队
  最稳。
- 重载同一个脚本块会连同这些回调一起换掉。

## 五之二、时间和定时器

沙箱里没有 `os`，所以这一对是脚本唯一的时钟：

```lua
App.now()                  -- 宿主创建以来的毫秒数，单调，整数
App.timer(50, f)           -- 一次性：50 ms 后跑一次，返回 id
App.timer(50, f, true)     -- 重复：每 50 ms 一次
App.cancel(id)             -- 撤掉，返回 true / false
```

- **派发在 `WM_TIMER` 里，不在绘制期。** 回调拿到的还是 Paint 档的能力（没有
  `&mut EditorState`），所以它不能改文档，只能记状态。
- **只要有定时器跑过，宿主就作废一帧**（frame + 状态栏 + 全部脚本窗格；窗格是
  独立子窗口，父窗口作废不到，得单独点名）。所以动画的写法是「心跳定时器 +
  绘制回调里读 `App.now()`」，定时器本身什么都不用做：

  ```lua
  App.timer(50, function() end, true)          -- 只负责叫醒

  ui.paint:on("status.pane", function(c, ctx)
    local t = (App.now() % 1600) / 1600        -- 0..1，1.6 秒一个来回
    c:fill(ctx.rect:with_width(3), "#FFD166")
    return true
  end)
  ```

  如果状态放在 Lua 变量里，而不是靠 `App.now()` 每帧变化，就在定时器里改完状态
  后调 `App.ui.repaint("pane.<id>")`：显式丢这一块的缓存、只作废那块窗格。
  不给名字则整帧重画；绘制回调只读状态，不推进、不调 `repaint`。

- 间隔限定 `1..86400000` ms（0 会在每一跳都发，等于忙等）。
- 同时最多 64 个定时器；重载同一个脚本块会**连同它的定时器一起**换掉。
- **回调抛错就注销那个定时器**，并且只记一行日志 —— 一个每 50 ms 报错的皮肤
  不该刷满日志，它也不会自己好起来。

## 五之三、文档元信息与视口

状态栏要显示的东西，皮肤自己画的时候也得有：

```lua
local d = App.doc.meta()
-- encoding  "UTF-8" / "GBK" / "windows-1252" …（原样，不翻译成 "CP 936"）
-- bom       true / false
-- eol       "LF" / "CRLF" / "CR"
-- syntax    "rs" / "py" …，没挂语法是空串
-- bytes     内存里 UTF-8 的字节数（和文件大小对得上，未命名文档是 0）
-- lines     行数（末尾换行会多出一行，和 `App.doc.line_count()` 一致）
-- read_only / modified / large

local v = App.view.info()
-- top_line / top_col   左上角看到的第一个格子（0 基）
-- rows / cols          可见几行、能塞下几个格子
-- caret_line / caret_col  光标（0 基，列是**字符**列）
```

- 这两张表是**复用**的（和 `ctx` 一样）：别存下来比旧值，存了也会跟着变。
  好处是每帧每格都问一次也不分配。
- `view.info()` 的行号是 0 基、列是字符列，**和 `App.doc.*` 的 1 基行号 /
  字节列不是一把尺子** —— 前者是给几何用的，后者是给编辑用的。
- `cols` 是「能塞下几个 `x` 宽度的格子」，换行模式打开时它不是视觉列数。
- 写视口用 `App.view.scroll_to(line[, col])`（0 基，列省略按 0）。它**只排队**：
  回调里调完视口还没动，等这次入口返回、`EditorState` 的借用结束才滚 —— 写操作
  都是这个规矩。它**不挪光标**（只滚，符合「点了滚动条」的语义），行号超界会
  夹到最后一行。`App.pending()` 能看还排着几件。
- **这两个都走 bridge，所以 Paint 档用不了**（绘制期没有 bridge，调用会抛
  「no bridge」并按交还处理）。Paint 档要显示这些值时读 `ctx.text` —— 那已经
  是内置画法格式化好的那一格文字。给 Paint 档准备 `ctx.doc` 快照还在 M2b 的
  待办里。

## 五之四、滚动条：没有半透明

`scroll.v` / `scroll.h` 拿到的 `ctx` 和别处一样，只是多一层物理约束：

```lua
local ui = App.ui
local function scroll(c, ctx)
  if ctx.hot then c:fill(ctx.rect, "#0E151B") end          -- 轨道
  c:round(ctx.thumb, math.min(ctx.thumb.w, ctx.thumb.h) / 2, "#FFD166")  -- 滑块
  return true
end
ui.paint:on("scroll.v", scroll)
ui.paint:on("scroll.h", scroll)
```

- **画的像素一律不透明，没画的一律全透。** 滚动条是一条分层窗口，像素带逐点
  alpha 交给 DWM，底下的正文要从透明处透出来；而 GDI 只写颜色、不写 alpha ——
  所以「画过哪几像素」是拿一个抠图色铺底、回放、再逐点比出来的。抠图色按这次
  的指令表挑一个没用过的颜色，皮肤填什么颜色都不会变成透明洞；代价是 `fill`
  的颜色 alpha 被忽略，想要「半透明的轨道」做不到。要正文透出来就别画那块底。
- **`ctx.thumb` 是算好的滑块矩形**：映射、最短长度、居中都在内置模型里。别自己
  按 `ctx.value` 算 —— 那是当前滚动位置，不是几何。
- **`ctx.hot` 在这里是准的**：指针落在条上、或者正在拖，都是 `hot`；正在拖另外
  带 `pressed`。
- 接管之后内置的**修改行标记**（那两条红/绿细条）就不画了 —— 整条是你的。
- 滚动条窗口是自己的一条窗口，`InvalidateRect` 到主窗口不会顺带刷它；改完皮肤
  重开进程即可（热重载在 M5）。

## 五之五、工具栏：底子归你，图标归控件

`toolbar.button` 每格调一次，`ctx` 的坐标是**格内**的（左上角是 0,0），不是整条的：

```lua
ui.paint:on("toolbar.button", function(c, ctx)
  local r = ctx.rect
  if ctx.disabled then return false end      -- 让控件自己画灰图标
  if ctx.pressed then c:fill(r, "#2A4553")
  elseif ctx.checked then
    c:fill(r, "#1E3A46")
    c:fill(r:with_height(2):move(0, r.h - 2), "#FFD166")
  elseif ctx.hot then
    c:fill(r, "#1B2C36")
    c:rect(r, "#2E4A5A", 1)
  end
  return true
end)
```

- **只让出底色和 1 px 边框，图标还是控件的。** 那套精灵图在控件的 image list 里，
  而 `c:image` 要的是文件里的位图，脚本拿不到 —— 硬要脚本画等于让它把 42 个图标
  重画一遍。所以这一区的语义是「每格的底子归你，glyph 归控件」。
- **`ctx.part` 是命令名**（`cm_file_save` 这种），按它分支比按序号稳；`ctx.text` /
  `ctx.label` 是命令的短名和说明，想画文字标签直接取。分隔线的 `part`/`text` 都是
  空串，`ctx.value` 是 `0`。
- **`disabled` 时返回 `false`**：控件要用 `ILD_BLEND50` 把图标压成半灰，那条路
  需要格子底色是「它以为的那个」—— 你画了它就不压了。
- 状态位：`pressed` 来自 `CDIS_SELECTED`（comctl32 没有 `CDIS_PRESSED` 这个位），
  `checked` 来自 `CDIS_CHECKED`，两者可以同时为真。带下拉箭头的按钮另带 `alt`。
- **带箭头的格子会调两次**：控件在画完底色之后又把箭头那一小条刷成自己的底，
  所以收尾时会把你的回调**裁到那条上再回放一次**，然后才画 ▼。写纯函数就没事；
  如果回调里有计数、随机数这类副作用，一次绘制会走到两次。
- **`ctx.hot` 在工具栏上是准的，但那是应用自己跟的**：这条工具栏被
  `SetWindowTheme` 起了自定义主题名，comctl32 于是整条关掉自己的热跟踪，
  `CDIS_HOT` 永远不会来。所以窗口过程在 `WM_MOUSEMOVE` 里自己做命中测试、
  记下索引，绘制时补一位。副作用是 `TB_GETHOTITEM` 也读不到 —— 别指望它。
- 悬停换格会重画**整条**（一次 42 格脚本调用）。按 `CacheKey` 缓存落地之后这一步
  才免费；在那之前鼠标横扫工具栏是有成本的。

## 五之六、停靠窗格页签：坐标是客户区绝对坐标

`dock.tab` 画的是停靠窗格顶上（或底下）那条页签条，每块窗格一格。它和工具栏
**只差一件事：坐标系**。工具栏是格内的（左上角 0,0），这里是**整条客户区的绝对
坐标** —— 因为一格的位置本身就在 `ctx.rect` 里，回放时不用挪视口：

```lua
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
  c:line(r.right - 1, r.y + 2, r.right - 1, r.bottom - 2, "#0E151B", 1)
  return true
end)
```

- **同样是底子归你、图标和文字归控件**。窗格图标就是工具栏那套精灵图（按窗格命令号
  取格），标题由控件在回调之后画上去 —— 所以 `ctx.text` 只用来**量宽、分支**，别自己画。
- `ctx.part` 是区域名 `dock.tab`（这一区没有「部件」之分），`ctx.text` 是窗格标题，
  `ctx.value` 是**窗格 id**（`100` 起的是输出窗格）。同一个窗格被拖到另一条边上，
  `value` 不变、`index` 会变 —— 想跨重排保持样式就认 `value`。
- **先画未选中的，选中的最后画**。选中那一格压在和邻居共用的缝上，页签才读得出「凸起」。
  所以你的 `ctx.rect` 是**整格**（含内置留给那道缝的 2 px），底色会盖过去 —— 想要缝
  自己补一条，像上面那样在 `right - 1` 画一道竖线。
- `active` 是「这一格是宿主当前显示的那块」，`hot` 是指针停在这一格上。`ctx.hot` 由
  控件在 `WM_MOUSEMOVE` 里自己跟（和内置那道悬停高亮同一个字段），不是 `CDIS_HOT`。
- 这一条页签**没有**半透明：它不是分层窗口，`c:fill` 画下去就是不透明像素。想透出
  底下的东西只能不画那块。

## 五之七、指针和提示

画的是「这一格长什么样」，`ui.paint:hint` 说的是「指针停在这一格上时，指针长什么样、弹什么」：

```lua
ui.paint:hint("toolbar.button", { cursor = "hand" })
ui.paint:hint("tab",           { cursor = "hand", tip = "@text" })
ui.paint:hint("dock.tab",      { cursor = "hand", tip = "@text" })
```

- 这是**一张表，不是回调**。悬停每秒问几十次，为两个常量进一次 Lua 不值当；更要紧的是
  回调能写出死循环 —— 指针卡住比画错难受得多。要按格分支就在绘制档里用 `ctx.part` 分，
  提示这边有占位符就够了。
- `cursor` 的名字：`arrow` `ibeam` `hand` `cross` `size_ns` `size_we` `size_nwse`
  `size_nesw` `size_all` `wait` `appstarting` `no`。写错**当场报错**并列出全部，不退回默认。
- `tip` 里可以用 `@text` / `@label` / `@value`，展开成这一格自己的三样东西 —— 和绘制档
  `c:text(r, "@text", …)` 认的是同一组。不认识的占位符原样留着（打错字要看得见）。
- **`tip` 只有 `tab` 和 `dock.tab` 收**。工具栏的提示是控件按 `TTN_GETDISPINFOW` 问
  `cmdtext` 要的，状态栏的是文字放不下时 comctl32 自己弹的 —— 在那两处再挂一条跟踪提示
  就是两个框叠在一起。写在别处会在声明那一行报错，并告诉你提示归谁。
- `cursor` 五个区域都收（`status.pane`、`tab`、`scroll.v`/`scroll.h`、`toolbar.button`、
  `dock.tab`）。不写就不动指针，保持内置的形状。
- 提示挂在格子的**左下角往下两像素**，横着对齐格子左边界 —— 不压住格子自己那行字。
- 同一个区域只能有一条声明；改主意就再写一次，撤掉用 `ui.paint:unhint(region)`。
- 换格的那一刻就落笔，不等下一次移动：`WM_SETCURSOR` 在 `WM_MOUSEMOVE` **之前**发，
  而形状往往正是这次移动才发现的（页签条要两条移动才把 `hot` 立起来）。

## 五之八、改一下，立刻看得见

**装载过的每一个脚本**都是边改边看的：存一次盘，窗口里就换一版，不用退出重开。
外壳每 700 ms 逐个看一眼「修改时间 + 字节数」，哪个变了就重跑哪一个，然后把每一条
自绘带重画。包里有三个文件就盯三个 —— 改 `panels.lua` 不会把 `paint.lua` 一起重跑。

- **写错了不炸。** `load` 是**先编译、编译过了才卸掉旧的那一版**。所以存进一个语法错，
  屏幕上保持的是上一版成功的样子，只在 stderr 留一行：
  ```
  [script] panels.lua: runtime error: [string "panels.lua"]:196: ...
  ```
  改对了下一跳就接上。**不弹对话框**是故意的 —— 弹窗会把窗口抢走，正改着的那份文档
  就丢焦点了。
- **只有脚本会热重载。** 主题文件（`themes/<name>.ini`）是另一条 700 ms 的轮询，两不相干。
  `<exe目录>/scripts/*.lua` 也是启动时装一次。
- **一次改动会重跑整份文件。** 不是只跑你改的那几行 —— 所以文件里的顺序、`local`
  的复用关系照旧；反过来说，写在文件顶层的副作用（`print`、`App.command` 注册）
  会**再执行一遍**。注册类接口都是「同名覆盖」，重复跑不会叠出两条命令。
- **重跑之后重画的是整条带**，不是一格：工具栏、两条页签条、状态栏、滚动条、停靠
  页签条，各是独立子窗口，父窗口那份作废管不到它们。
- **看不见就 `print`。** 绘制回调里也能用，直接进 stderr（启动时那个控制台），
  不影响这一帧的画。把 `#ctx.rows`、`ctx.w`、`ctx.rect` 打出来，比盯着屏幕猜快得多。
- 文件被删掉、或半中间写坏（`stat` 读不到）时什么都不做，等下一次读得到再说。

## 五之九、不写 Lua 也能改外观：形状键

不是每个改动都值 20 行回调。圆角、格间空隙、框线粗细、格内留白这四样，写在
**主题文件**的 `[Shape]` 里就行，一行一个 —— 不用写一句 Lua，也不用另存一个文件：

```ini
[Shape]
tab.radius=6                ; 选中页签的圆角（DIP）
tab.gap=6                   ; 格与格之间留空，左右各缩一半
tab.border=2                ; 框线宽度，至少 1
tab.pad_y=4                 ; 填充块上下各缩一次，文字在变矮的块里重新居中
tab.states.active.radius=0  ; 状态层：只写要改的键，其余沿用常态
```

键的语法是 `区域[.states.<状态>].<键>`。**`states` 是中缀不是一层**：区域名自己
可以带点（`dock.tab`），所以切分点是最后一个 `.states.`，不是第一个点。

四条容易踩的：

- **`radius > 0` 时框也跟着圆**。只磨填充、框留方的，四个角上会露出一圈底色。
- **`border` 只加粗、不取消**（至少 1 px）。要看不见框，把 `tab.frame` 这个颜色
  调成和页签带同色 —— 那是颜色的事，不是粗细的事。
- **`gap` 缩的是整格**（框和填充一起），奇数不会丢像素：`gap = 1` 是左 0 右 1，
  两格之间正好空 1。**文字和图标不跟着挪**：格子宽度是布局量的，量的时候不知道
  `gap`，挪了只会把贴边的标签截掉（`gap = 4` 就能把「快捷目录」最后一个字裁掉）。
  真要让标签也变窄，配 `[metrics]` 的 `tab.pad_x` 一起调。
- **`pad_y` 缩的是填充块**，文字跟着在变矮的块里重新居中。

**现在接线的是这三条带**，每个区域认的键不一样：

| 区域 | `radius` | `gap` | `border` | `pad_y` | `font` |
|---|---|---|---|---|---|
| `tab` | 填充块圆角，框也跟着圆 | 整格左右各缩一半 | 框线宽度 | 填充块上下各缩 | 未接线（写 `[fonts] tab`） |
| `dock.tab` | 同上 | 同上 | 同上 | 同上 | 未接线（写 `[fonts] pane`） |
| `status.pane` | — | 文字左边距 | 格间竖线宽度 | — | 未接线（写 `[fonts] status`） |

`status.pane` 那两个破折号不是偷懒：这一格的内置画法**从不填自己那一块**（只填
整条带），`radius` 没有东西可磨；文字是垂直居中的，对称的 `pad_y` 内缩等于什么
都没做。与其画一个看不出差别的东西，不如直接不认 —— 写了会按名字警告。

**另外两条带不走配方**，这也是结论不是欠账：

- **工具栏按钮**：底子是 comctl32 画的（我们只在脚本接管时才画），`radius` 改不动
  它。要圆角按钮就写 `ui.paint:on("toolbar.button", ...)` —— 那条路本来就有。
- **滚动条**：滑块出厂就是半圆（`r = min(w,h)/2`），而真正想调的四样
  （滑块宽、轨道宽、透明度、淡入淡出）早就是 `[metrics]` 的 `scroll.*`。配方在这里
  加不出东西。

别的区域写进去**不会报错**，但会按名字补一句日志 —— `Recipe::from_theme_file`
返回 `(配方, 问题列表)`，坏键不连累好键：一个区域名打错，其余几十行照旧生效。
看见了就说明还没生效，别以为是写法错了。

想让形状**跟着状态变**（选中时圆角归零、悬停时留白变大），主题文件写不了 ——
那是运行时的活，用 `App.ui.style:shape`（见五之十七）。

问题分两种，说的是两件事：**整个区域**还没接线（`toolbar.button`），和区域接了
但**这个键它不看**（`tab.font`）。后者不是懒 —— 状态栏的每一格内置画法从不填
自己那一块，所以 `radius` 没有东西可磨；文字是垂直居中的，对称的 `pad_y` 内缩
等于什么都没做。与其画一个看不出差别的东西，不如直接点名。

## 五之十、换字体：`[fonts]`

六个 token，一个都不能多，写错名字当场报错并列出六个。和形状键一样在主题文件里：

```ini
[Fonts]
mono.face=JetBrains Mono        ; 编辑器
mono.size=12
tab.face=Segoe UI               ; 文档页签
tab.size=10
tab.weight=600
ui.face=Segoe UI                ; 界面正文
ui.size=9
status.size=9                   ; 状态栏
menu.size=9                     ; 菜单
pane.size=9                     ; 停靠窗格
```

键的语法是 `<token>.<字段>`，字段四个：`face`（字面名）、`size`（pt）、
`weight`（100–900，也认 `normal`/`bold`）、`italic`（`yes`/`no`、`1`/`0`、
`true`/`false`）。**越界夹住不报错**：`size = 200` 的意思是「大一点」，
不是「我打错了」—— 夹到 6–48 pt，`weight` 夹到 100–900。

**一个字段写坏，整个 token 不生效** —— 不是「其余三个照用」。`mono.size=huge`
以前会造出一个只有默认值的 `mono` 槽，把内置的 Consolas 顶掉、字号退回 9 pt：
一个错字让整个 token 变成另一副样子。现在先校验、四个都认了才写进去。

**只写 `size` 不写 `face`** 是合法的，意思分别是：

- `mono` → 编辑器内置的 `Consolas`。等宽字体没有「系统默认」这一说，
  `NONCLIENTMETRICS` 给的是界面字体，拿它排代码会把整张单元格网格变成比例宽度。
- 其余五个 → 系统 UI 字体（`NONCLIENTMETRICS` 那只）。没写 `[Fonts]` 时就是这个。
  `（系统默认）`。

**现在接线的是六个**：

| token | 落到哪 | 换字体的连带动作 |
|---|---|---|
| `mono` | 编辑器字面 | 单元格宽度、行高、缩进线、光标、命中测试、折行宽度**全部重算** |
| `tab` | 文档页签条 | 页签高度与每格宽度重算（`tab_width` 按新字体量） |
| `ui` | 界面正文：对话框、提示条、打开文档列表…… | 无 —— 控件自己 `WM_SETFONT` 后重新量 |
| `menu` | 菜单条（自绘，行高由字体量出来） | 菜单栏变高，下面整片跟着下移 |
| `status` | 状态栏 | comctl32 按字体重新报自然高度（实测 @120 DPI：9 pt → 29 px、14 pt → 41 px、20 pt → 57 px） |
| `pane` | **脚本窗格**（`ui.layout:pane`，见「五之十三」） | 无 —— 窗格自己持一只 HFONT，`c:text` 回放时用它 |

**`ui` 是 `menu` / `status` 的兜底**：只写 `ui` 的时候，菜单和状态栏也一起换 ——
不会出现「对话框变大了、菜单还是 9 pt」那种半拉子。想只放大正文、把菜单钉住，
两条都写：

```toml
[fonts]
ui   = { size = 14 }
menu = { size = 9 }     # 菜单保持原样
```

实测这一对的差别：只写 `ui` 时整窗都在变（`y=40..950`，菜单变高把下面整片推下去）；
加上 `menu = 9` 之后变化**只落在状态栏那一条**（`y=891..950`）。

`pane` 只管脚本窗格：内置的停靠窗格（目录树、输出窗口、大纲……）画字用的是编辑器
字体，写 `pane` 不会让它们变大。要连内置的一起换，写 `ui` 或者 `mono`。

两条容易踩的：

- **`mono` 的字号是「基准字号」**，不是「覆盖缩放」。菜单里的「增大/减小字号」
  从它起步，「原始字号」回到的也是它 —— 皮肤写 12 pt 之后，「原始」是 12 pt，
  不会把文档按 10 pt 缩回去。
- **字面名写错不会开不了窗**：系统里找不到就退回 `Consolas`，并在 stderr 留一行
  `[theme] 字体 "No Such Face 9000" 没装，编辑器退回 Consolas`。宁可字体不对，
  也不能因为一个打错的字面名让整张版停在旧字体上。

**主题文件改了不用重开。** 当前主题如果有一份外部副本（`themes/<name>.ini`），
外壳每 700 ms 看一眼指纹，变了就整份重读 —— 四段全跟着。`[Metrics]` / `[Fonts]`
改的是「按字体量出来的尺寸」，所以重读时会连带重排一遍窗口：状态栏变高，下面
整片跟着上移。

## 五之十一、编辑区覆盖层：`editor.overlay`

别的区域是「一格一次回调」，`editor.overlay` 反过来：**整屏一次**，一次拿到所有
可见行，自己循环。因为行装饰要跨行看（斑马纹、成对的高亮、按顺序排的标记），
一格一次就只能看到自己那一格。

```lua
App.ui.paint:on("editor.overlay", function(c, ctx)
  local rows = ctx.rows
  for _, r in ipairs(rows) do
    c:fill({ x = 0, y = r.y, w = 6, h = r.h }, "#FF00FF")
    if r.active then
      c:fill({ x = 0, y = r.y, w = ctx.w, h = r.h }, "#1E3A5F")
    end
  end
  return true
end)
```

`ctx.rows` 是数组，**从 1 起**，一行一项：

| 字段 | 意思 |
|---|---|
| `line` | 逻辑行号，**从 0 起**（和 `App.doc.line()` 一致；数组下标才从 1 起） |
| `y` / `h` | 这一行在编辑区里的顶边和高度，像素。**一行被折成几段时 `h` 是整条带子的高**，不是一段 |
| `rows` | 折了几段（没折行是 1） |
| `display` | 显示行号（折叠打开时它会跳过被藏起来的行） |
| `active` | 光标在这一行 |
| `folded` | 这一行**下面**藏着折起来的行（该画「+」） |
| `foldable` | 这一行是折叠起点 |
| `bookmark` | 有书签 |
| `icon` | 行注记的图标号，没有是 0 |
| `state` | 行修改状态：0 未改、1 改过没存、2 已写回 |
| `tilde` | 上一行没有行尾符 —— 行号栏该画 `~` 而不是数字 |
| `eol` | 这一行的行尾符：`"CRLF"` / `"LF"` / `"CR"`，末行是空串 |

坐标系是**编辑区自己**的，原点在编辑区左上角 —— 包括行号栏。所以 `x = 0` 那条
压在最左边的留白上，`ctx.w` 是编辑区整宽。`ctx.rect` 就是整个编辑区，`ctx.w` /
`ctx.h` 是它的宽高。

四条要知道的：

- **`return true` 别忘**。和别的区域一样，返回 `nil`/`false` 等于「我没画」，
  一条 op 都不会上屏。
- **不进缓存**。这是它和别的区域最本质的差别：输入是可见行表，滚动一格全变，
  比对四十行和直接跑一遍一样贵。所以它每帧都进 Lua —— 回调要短，别在里面
  建大表（`ctx.rows` 每读一次整张重建，`local rows = ctx.rows` 只读一次）。
- **只能画 `fill` / `rect` / `text`**。编辑区的像素是经 DX 表面合成的 `DrawCmd`，
  不是 GDI 的 DC：`gradient`、`line`、`circle`、`image` 这些在这条路上回放不出来，
  会被数出来并在 stderr 报一次
  `[script] editor.overlay: N 条 op 画不出来（这条路只认 fill / rect / text）`。
  要渐变就自己拿 `fill` 铺几条。
- **`text` 只认左上角**。回放到这里只有一个位置，没有字体、没有对齐、没有省略号 ——
  拿它标个记号可以，别拿它排正文（正文归编辑器自己画，盖在它下面）。

**覆盖层在文字和行号栏之上**：想「垫在文字底下」得让颜色带点透明感（挑个暗色），
实色铺满会把那一行的字盖掉。

**大文件模式照常**：行表来自视口，不碰整篇文档；只是 `state` 恒 0（大文件不记
逐行修改状态），`display` 就是行号（大文件没有折叠）。

## 五之十二、编辑区行号栏：`editor.gutter`

`editor.overlay` 是**盖**在文字上，`editor.gutter` 是**替**掉整列：返回 true，那七个
内置组件（书签条、行号、诊断条、行注记图标、折叠、边界线、行修改状态）这一帧
**一遍都不跑**。

```lua
App.ui.paint:on("editor.gutter", function(c, ctx)
  local m = ctx.metrics
  -- 整列底色，含最后一行以下那截
  c:fill({ x = 0, y = 0, w = ctx.w, h = ctx.h }, "#2B1B3D")
  for _, r in ipairs(ctx.rows) do
    local text = tostring(r.display)
    local w = #text * m.x_width
    -- 和内置行号栏同一处右对齐，只换颜色
    local x = m.line_number_x + m.line_number_w - 3 - w
    c:text({ x = x, y = r.y, w = w, h = r.h }, text, "#FFB86C")
  end
  return true
end)
```

- **`ctx.w` 是整列的宽，`ctx.h` 是整个编辑区的高**。最后一行以下那一截也归你 ——
  返回 true 之后没人替你填底，不填就是编辑器背景色。
- **`ctx.rows` 和覆盖层是同一张表**（见「五之十一」），字段一模一样。这一列要用的
  主要是 `display`（显示行号）、`y`/`h`（行带）、`bookmark`、`folded`/`foldable`、
  `icon`、`state`、`tilde`、`eol`。
- **返回 true 但一条 op 都不画是合法的**：那等于「我要一列空白」—— 比如只留书签
  列、把行号关掉的皮肤。覆盖层不是这样（空 op 等于没画），这条不一样。
- **要和内置行号栏对齐**用 `line_number_x + line_number_w - 3 - 位数 × x_width`。
  别用 `gutter_x`：那是整列的宽，行号只占其中一格，右边还有折叠、图标和边界线，
  照它右对齐会把数字推到正文边上。
- **`c:text` 在这条路上用的是编辑器自己的字体**（`DrawCmd::TextRun` 不带字体），
  所以行号长得和正文一致；`y` 是行带的顶边，和内置画法同一个约定。
- 同样**只认 `fill` / `rect` / `text`**，同样**不进缓存**：每帧一次回调，循环要短。

## 五之十三、脚本窗格：`ui.layout:pane`

前两条改的是**编辑区**里的一块。这一条反过来：让皮肤**自己长出一块停靠窗格**，
和目录树、输出窗口、大纲并列 —— 同样的页签、同样的四条边、同样的右键菜单
（移除 / 左侧 / 右侧 / 顶部 / 底部 / 上↔下），区别只有一处：**内容没有内置画法**。

```lua
local ui = App.ui
local clicks = 0

ui.layout:pane{ id = "clock", side = "right", title = "时钟", icon = 44, size = 220 }

ui.paint:on("pane.clock", function(c, ctx)
  c:fill(ctx.rect, ctx.hot and "#243447" or "#1B2634")
  c:text(ctx.rect:inset(10, 8):with_height(24), "点了 " .. clicks .. " 次",
         "#FFB86C", { valign = "top" })
  return true
end)

ui.on("pane.clock", "press", function(ctx)
  clicks = clicks + 1          -- 改了自己的状态，下一次绘制就会看见
  return true
end)
```

| 字段 | 说明 |
| --- | --- |
| `id` | 必填。窗格的身份，也是区域名 `pane.<id>` 的后半截（`ctx.part` 同样是它） |
| `side` | `"left"` / `"right"` / `"top"` / `"bottom"`，默认 `"bottom"`。写别的当场报错 |
| `title` | 页签上的字。不写就用 `id` |
| `icon` | 页签图标的**工具栏精灵格号**（和 `ui.layout:toolbar` 用的是同一张图，0 = 不给图标） |
| `size` | 这一边的厚度（DIP）。不写 = 用停靠栏自己的记忆值 |
| `bar` | 竖直滚动条的轨道宽（DIP），默认 0 = 不要轨道（滚轮照样能滚）。见下面「滚动」 |

**`paint` 不在这里声明**，仍然走 `ui.paint:on("pane.<id>", fn)`。这样绘制还是那套
「纯函数 + 缓存」的东西，`ui.layout:pane` 只负责**结构**（有几块、叫什么、停在哪、多厚）。

### 画法

- **整块一次回调**，不是一格一次：`ctx.rect` 就是整块窗格的客户区，`ctx.w`/`ctx.h`
  是它的宽高。内部怎么分格、要不要分格，归皮肤自己算。
- **整帧一次贴上去**：底色和皮肤那几十条 op 先画在一张离屏位图上，最后只 `BitBlt`
  一次。所以滚轮、拖滑块那种连续重画不会闪 —— 窗格条、输出窗格用的是同一条规矩。
  这条对皮肤是免费的，不用自己缓冲。
- 坐标是 **DIP**，和其它区域一样（窗口 244 px 宽、125% DPI 时 `ctx.w` 是 195）。
  回放层按 DPI 乘回像素，所以别自己再乘一遍。
- 这是一块**真的 HWND**，`c:` 的十二条原语在这里全都能用 —— 包括 `c:image` 和
  `c:shadow`，和编辑区那两条（只能 `fill`/`rect`/`text`）不一样。
- `c:text` 用的是**这一块自己的字体**，即 `[fonts] pane`（不写就退回 `ui`）。
- **返回 true 才有画**：返回 false / nil / 不返回，就只剩主题底色（`panel_bg`）。
  返回**数字**等于 true 外加一句「内容有 N DIP 高」，也就是能滚（见下一节）。
- `ctx.hot` 是「指针停在这块窗格里」。没有更细的状态位 —— v1 的粒度就是整块。
- `ctx.focus` 是「这块窗格拿着键盘焦点」，见下一节。

### 滚动

内容比窗格高的时候能滚。分工是**内容高由皮肤报、几何由宿主算、像素由皮肤画**：

```lua
ui.layout:pane{ id = "help", side = "bottom", title = "清单", size = 170, bar = 8 }

ui.paint:on("pane.help", function(c, ctx)
  -- 列表：clip 到表头以下，再减偏移
  c:clip({ x = 0, y = 40, w = ctx.w, h = math.max(0, ctx.h - 40) })
  for i, line in ipairs(lines) do
    c:text({ x = 10, y = 40 + (i - 1) * 18 - ctx.scroll, w = ctx.w - 20, h = 18 }, line, "#B8C7D4")
  end
  c:unclip()
  -- 表头：**不加** ctx.scroll，所以钉在顶上
  c:text({ x = 10, y = 10, w = 200, h = 20 }, "钉住的表头", "#8AB4F8")
  -- 轨道和滑块：矩形是宿主给的，这里只填色
  if ctx.bar.w > 0 then
    c:fill(ctx.bar, "#0A1116")
    c:fill(ctx.thumb, "#4A6B85")
  end
  return 40 + #lines * 18 + 10      -- ← 内容总高（DIP）
end)
```

**`return 数字` 就是报内容高。** 数字在 Lua 里本来就算真值、本来就等于「我画了」，所以
`return true` 和 `return 1200` 只差这一条信息，老画法一个字都不用改。没报 = 0 = 不滚。
不能写在 `ui.layout:pane` 里：时钟、日志、列表都是画的时候才知道有多高。

宿主算好的三样：

| `ctx` 字段 | 是什么 |
| --- | --- |
| `ctx.scroll` | 当前偏移（DIP），已经夹进 `[0, 内容高 - 视口高]` |
| `ctx.thumb` | 滑块矩形（DIP）。滚不动时是空矩形 |
| `ctx.bar` | 整条轨道（DIP）。滚不动或 `bar = 0` 时是空矩形（`w == 0`） |

**偏移是皮肤自己减的**，回放层不替它平移。所以钉住的表头是免费的 —— 那一行不加
`ctx.scroll` 就行。代价是**滚出内容区的东西要自己 `c:clip`**：偏移只挪位置不裁剪，
不 clip 的话滚上去的行会盖在表头上（`c:clip` 是 `IntersectClipRect`，可以套多层）。

**交互全归宿主**：滚轮一格 48 DIP、点轨道空白翻一屏、拖滑块。`App.ui.on` 的 `ctx` 里没有
指针坐标，皮肤没法自己算「拖到哪儿了」，所以这些不给脚本 —— 皮肤只填色。点在轨道上
也**不会**触发 `ui.on("pane.<id>", "press")`。

**`ctx.rect` 是内容区，不是客户区**：有轨道时右边那 `bar` 宽的一竖条不算内容，所以
`c:fill(ctx.rect, …)` 不会画到轨道底下。

`bar = 0`（默认）只是**不要轨道**，滚轮照样能滚 —— 想画在左边、或者画一条横的，拿
`ctx.scroll` 自己算就行。窗格尺寸或内容高变了，宿主会把偏移夹回去（多要一帧，收敛是
必然的）。

`packages/luapaint/panels.lua` 的 `pane.help` 是现成的例子：底部一块，表头钉住、十二条列表能滚。

### 点击、提示

`ui.on("pane.<id>", "press" / "rclick", fn)` 和 `ui.paint:hint("pane.<id>", {...})` 照常
能用，`ctx` 和绘制那份一模一样。点击是 Intent 档（有 bridge），`print`、`App.run`、
`App.doc.meta()` 都能用。

### 焦点和键盘

点一下窗格，焦点就过来了（`ctx.focus` 变真，`State::FOCUS` 亮起），这时**裸键也算数**：

```lua
App.keymap("r", "pane.refresh")          -- 注意：是 App.keymap，不是 s:key

App.command("pane.refresh", function()
  local m = App.doc.meta()
  print(string.format("%d 行 %d 字节", m.lines, m.bytes))
end)
```

- **键盘 API 只有 `App.keymap`**，没有「皮肤快捷键」这一套。`App.keymap` /
  `App.command` / `App.on` 都归 dg-script，包里直接就能用，热重载时一起清掉。
- **方向键、空格等具名键也能绑**：`Up` / `Down` / `Left` / `Right` / `Space` /
  `Enter` / `Esc` / `Tab` / `Backspace` / `Insert` / `Delete` / `Home` / `End` /
  `PageUp` / `PageDown`，大小写不敏感；别名 `Return` / `Escape` / `Back` /
  `Ins` / `Del` / `PgUp` / `PgDn` 同样认。`F1`..`F12` 照旧。编辑区仍只让
  `Ctrl` / `Alt` 组合通过，所以方向键玩法要先点一下脚本窗格拿焦点。
- **编辑区里裸键绑不生效**。`r`、`Shift+R` 这些在正文里是要落进文档的字，所以派发只认
  带 `Ctrl` 或 `Alt` 的组合。同一条 `App.keymap("r", ...)` 在窗格里有焦点时按 `r` 会跑，
  点回正文按 `r` 就是打一个 `r` —— 这不是两个 API，是同一份绑定按焦点决定生效范围。
- **窗格没接的键会转给主窗口**，所以点了窗格之后 `Ctrl+S`、`Ctrl+F` 照常。
- 只有脚本窗格能拿键盘焦点。工具栏、状态栏、页签条这些自绘区域没有焦点可言，那边只能绑
  `Ctrl`/`Alt` 组合键。
- 焦点只有一处：点第二块窗格，第一块就失去了。它也不持久 —— 切走窗口再切回来，焦点在
  编辑区。

**点击之后这块的绘制缓存会被丢掉**（只丢这一块）。原因是：handler 改的是 Lua 里的
变量，宿主看不见，缓存没法知道哪一条该失效。所以「点一下换配色 / 计数 +1」这种写法
直接就能用，不必自己找什么东西去顶缓存。代价是一次点击重算这一块 —— 人手频率。

命令、事件和定时器里改 Lua 状态就没有这份自动待遇：改完自己调
`App.ui.repaint("pane.<id>")`（不给名字则整帧重画），或者在动画画法里读
`App.now()`。`packages/snake/snake.lua` 是前一种写法的完整例子：定时器推进一跳，
只丢 `pane.snake` 的缓存和窗口，绘制回调全程只读。

### 缓存

`pane.<id>` 是词表里正经的区域名（归到 `custom` 那一格，`key` 保留整个名字），
所以这一块**和别的区域一样进缓存**：输入没变就不进 Lua。想让它在「什么都没变」
的时候也动起来，就得读一个每帧都变的东西 —— `App.now()`（读了就不缓存，见「六」），
或者 `ctx.doc`（跟着文档走）。

`packages/luapaint/panels.lua` 里有三块现成的：`pane.uptime`（读 `App.now()`，秒表）、
`pane.docinfo`（读 `ctx.doc`，行数 / 语法 / 换行 / 改过没）、`pane.help`（读
`ctx.scroll`，能滚的清单），三条页签各停一边。

`packages/snake/snake.lua` 走的是另一条路：状态只在命令 / 定时器里变，画法不读
时钟也不改状态，改完调 `App.ui.repaint("pane.snake")` 显式失效；静止时整块照常
命中缓存。

**读了才比对**：`ctx.scroll` / `ctx.bar` 和 `ctx.part` / `ctx.text` 那几样一样，是
「读到哪样比哪样」（见「六」）。所以滚动**只让读过 `ctx.scroll` 的画法重算** ——
一块不随偏移变的窗格，滚它一格也不会多跑一次 Lua。

### 声明是活的

脚本存一次就重跑一遍（热重载，见「五之八」），窗格表跟着换：

- **同名 = 同一块**：窗口留着，只换标题、图标、厚度和边。焦点、滚动位置不丢。
- **`side` 改了**：搬到新那一边，并按新的 `size` 定厚度。
- **皮肤里删掉了**：窗口销毁，页签从宿主里摘掉。
- **新出现的**：建窗口并停靠到它声明的边。
- **用户手动关掉的那一块，重载后不会自己冒出来** —— 只有新窗格才自动停靠。

`size` 只在「这一块刚停到这一边」时生效（新建，或皮肤把它换了边）。之后那是用户
拖出来的宽度，存一次脚本不该把它拽回去。

实测（@120 DPI，一块 `side="right", size=220` 的窗格）：首次停靠 269 px 宽；
把 `panels.lua` 改成 `side="left", size=180` 并存盘，2.5 秒内搬到左边、宽度 219 px、
页签文字跟着换。

### 别做的两件事

- **别在绘制里读 `App.now()` 又指望它被缓存**。时钟窗格当然要读它（「五之二」），
  读了的画法本来就不进缓存 —— 那就每帧跑一次，循环要短。
- **别用 `ui.layout:pane` 复制一个已有的内置窗格**（比如再画一遍目录树）。它拿不到树的
  数据；内置窗格的内容都在宿主那一侧。
- **别在滚动的窗格里省掉 `c:clip`**。`ctx.scroll` 只挪位置，滚出内容区的行会画到
  表头、甚至轨道上去（见「滚动」）。

## 五之十四、状态栏：`ui.layout:status`

`ui.paint:on("status.pane", fn)` 只改**画法**，格子还是内置那十三个，位置和宽度也归
宿主。想让状态栏变成 Vim 的 `statusline` —— 自己决定有几格、每格显示什么、窗口
窄了先丢谁 —— 用 `ui.layout:status`。

```lua
local ui = App.ui

ui.layout:status{
  { id = "mode",  text = "%M",                 priority = 90 },
  { id = "pos",   text = "%l:%c · %p%%",       priority = 70 },
  { id = "file",  text = "%f%m%r",  width = { min = 80, max = 260 }, priority = 60 },
  { id = "clock", width = 80, priority = 5, click = "skin.clock",
    update = function()
      local t = math.floor(App.now() / 1000) % 86400
      return string.format("%02d:%02d", math.floor(t / 3600), math.floor(t / 60) % 60)
    end },
  { width = "fill" },     -- 余量给它
}
```

调了它就是**接管整条**：内置那十三个字段一个都不画。不想重写一遍，就先把它读出来：

```lua
local row = ui.layout:defaults("status")          -- 内置的十三段，顺序一模一样
table.insert(row, { id = "pct", text = "%p%%", width = { min = 40, max = 56 } })
ui.layout:status(row)
```

不调 `ui.layout:status` 的话状态栏一个字都不变 —— 声明是接管，不是追加。`ui.layout:status{}`（空表）
是「一格都不要」，`ui.layout:status()`（不带参数）读回当前声明，改完能原样传回去。

**两个包都调它就是后装的那个说了算**（按 `order` 排，同 `order` 按 id）。状态栏只有一条，没有
「各占一半」这回事：后装的那次 `status()` 把前一条整条换掉。要共存就一个包声明、别的包用
`ui.layout:update` 改自己那几格 —— 这也是「谁的声明谁负责」的唯一一种分工。

### 一段写什么

| 键 | 含义 |
| --- | --- |
| `id` | 段名。**同时是绘制区域名** `status.<id>` 和 `ctx.part`。没有 `id` 的段只能是 `fill`。 |
| `text` | 内容模板，见下表。空串 = 这段的文字不归模板管。 |
| `update` | `function(ctx) -> string`，每 500 ms 跑一次（见下）。**优先于 `text`**。 |
| `width` | `"auto"`（默认）/ 数字（固定 DIP）/ `"fill"`（吃掉余量）/ `{ min=, max= }`（两个都可省，省掉的一头等于没有）。 |
| `priority` | 0..255，**越大越重要**。窗口窄了从最小的开始丢。默认 50。 |
| `click` | 点一下跑哪个命令。空 = 不可点。 |

段名的字符集和别的区域名一样（字母数字 `_` `-`，不超过 40 字节），**重复的段名当场
报错** —— 名字是缓存键的一部分，两段同名会共用一份画法。

文字有三条来源，按优先级：

1. `update(ctx)` —— 脚本算的。和 `ui.paint:on` 拿同一份只读上下文（`ctx.doc`、
   `App.now()`、`ctx.theme`），但**没有 bridge**，它跑在量宽度那条路上。
2. `text` 模板 —— 宿主展开，见下表。
3. **内置字段** —— 按 `id` 查。`就绪`、`4.2 KB`、`U+4E2D`、`制表符:4` 这些模板
   表达不了（要查语言表、要格式化字节数），继承来的格走这一条。

三条都不成立就是空串 —— 一格空着，比画一个 `%q` 上去好。

### 模板占位符

借 Vim 的字母，语义照表写死：

| 占位符 | 展开成 | | 占位符 | 展开成 |
| --- | --- | --- | --- | --- |
| `%l` | 行（从 1） | | `%y` | 语法名，没有就是空 |
| `%c` | 列（从 0） | | `%e` / `%E` | 换行方式 / 编码 |
| `%L` | 总行数 | | `%M` | 插入 / 覆写 / 列模式 / 多选 |
| `%p` | 光标百分比 0..100 | | `%S` / `%s` | 选中字符数 / 选区行数 |
| `%f` / `%F` | 文件名 / 完整路径 | | `%a` | 大文件时 `大文件` |
| `%m` | 改过没保存时 `[+]` | | `%%` | 一个字面 `%` |
| `%r` | 只读时 `[RO]` | | | |

**不认的占位符当场报错**（`ui.layout:status` 那一行），不原样吐出来 —— 写错一个字母就整条
不变样，是那种「看着没问题、其实没生效」的坑。

### 宽度和丢段

`width` 的四种写法：

- `"auto"`（默认）：量这一格的文字，加上**左右各一份** `[metrics] status.pad`（默认
  2 DIP，96 DPI 下合计 4 像素），再夹到 `status.min_w`（默认 45）以上。
- 数字：固定多少 DIP，和文字多长无关。
- `{ min = , max = }`：量出来之后夹在区间里。**给会变长的文字用** —— 路径、时间。
  两个键都能省：`{ max = 104 }` 是「按文字量，最多 104」，`{ min = 40 }` 是「至少 40，
  长了随便」。省掉的那一头等于没有。
- `"fill"`：吃掉剩下的宽度。一条里最多一个；没写就自动补一个在末尾，所以整条永远
  铺满，右边不会留一条没人画的控制条底色。

**自己画状态格的皮肤要设 `status.pad`**：宿主量宽度时只认这个数，它和你在
`ui.paint:on("status.<id>")` 里 `c:text(r:inset(N, 0), …)` 的那个 `N` 必须是同一个值。
量得比画得窄，文字就会被 `ellipsis` 吃掉半截 —— 而 `width = { min = 80 }` 这种手写下限
只能救一格，救不了所有格。示例皮肤两个地方都是 9。

窗口窄了放不下时，**按 `priority` 从低到高一段一段丢**，直到放得下。同优先级丢靠右
的（右边通常是次要信息，而且丢右边不会让左边跳位置）。`fill` 段永远不丢 —— 它是余量
的去处。这比 Vim 的 `%<` 强：那个只能指定一个截断点，这里是每段各有一个数。

丢掉的段**根本不进控件的 part 数组**，不是宽度给 0 留在那儿 —— 宽度 0 的 part 会留下
一条点得到、画不出的缝。

### 点击

`click` 写的是命令名，和 `App.run` 认的是同一张表：**脚本命令优先，没有就落到内置命令
表**。所以 `click = "file.save"` 和 `click = "my_own_command"` 都能用。

点完之后宿主会把整条重推一遍 —— 命令可能改了这一格自己的状态（制式、计数器、展开
位），而那些变量宿主看不见。

### 动态段会自己醒

带 `update` 的段每 500 ms 重算一次文字，因为**没有别的消息会到**：时钟、CPU 占用、
外部计数器，宿主都不知道它们变了。全模板或者全内置的那条根本不装这个定时器，所以
「只改画法」的皮肤一点额外开销都没有。

一次重算 = 每段一次文字测量 + 有回调的那几段各进一次 Lua。`apply` 自己比对文字，
没变就不重画。

### 画法

每一段有自己的区域名 `status.<id>`：

```lua
ui.paint:on("status.clock", function(c, ctx)
  c:fill(ctx.rect, ctx.theme["status.bg"])
  c:text(ctx.rect:inset(9, 0), "@text", "#FFD166", { valign = "middle" })
  return true
end)
```

`ctx.text` 就是这一段算出来的文字。**读了它就按文字分支**：缓存会连带比对文字，
时间一变就重算（上面那段按 `AM`/`PM` 换颜色用的就是这一条）。推荐写法是不读、直接
`c:text(r, "@text", …)` —— 那样文字变了也照样命中，`@text` 在回放时才替换。

`status.pane` 那条老画法**同时有效**：某一段没注册自己的 `status.<id>`，就退回
`status.pane`（`ctx.part` 是段名）。所以只写了 `status.pane` 的老皮肤，加上
`ui.layout:status` 之后画法不会突然消失。

### 自己画的格要留够宽度

宽度是「文字 + 左右各一份 `[metrics] status.pad`」。默认 2 DIP，够内置那条用；
**皮肤自己画的格如果左边留了 9 DIP 的色条位，就得把 `status.pad` 也设成 9**，
否则量得比画得窄，`ellipsis` 会把文字吃掉半截：

```toml
[metrics]
"status.pad" = 9     ; 和 panels.lua 里 inset(9, 0) 的那个 9 是同一个数
```

`width = { min = 80 }` 那种手写下限也能顶一阵，但它只救一格、而且文字一长就失效 ——
真正该对齐的是内边距。上限还是可以照写（`{ max = 104 }` 就够，`min` 省掉等于没有）。

### 声明是活的

改 `panels.lua` 里的 `ui.layout:status`（加一段、删一段、改宽度、改 `click`），下一次热重载就
生效 —— 段表是数据不是窗口，`SB_SETPARTS` 跟着重发。`update` 回调本身也是热的。

## 五之十五、工具栏按钮：`ui.layout:toolbar`

```lua
local ui = App.ui
ui.layout:toolbar{ icon = 60, tip = "切换时钟制式", command = "skin.clock_format", after = 57600 }
```

| 字段 | 说明 |
| --- | --- |
| `command` | **必填**。点下去跑的命令名。脚本命令优先，没有就落到内置命令表 —— `"file.save"` 这种写法也能用 |
| `icon` | 工具栏精灵格号（和 `ui.layout:pane` 的 `icon` 同一张 112 格图）。`60` 是时钟，`34` 不是 —— 两套编号不一样，别按 Tabler 的字母序数 |
| `tip` | 悬停提示。不写就用 `command` —— 空提示等于没提示 |
| `after` | 插在这个命令号那一格后面。`0` / 不写 = 末尾。找不到（比如用户把那格删了）也落到末尾 |

- **这是追加，不是整条替换**（和 `ui.layout:status` 相反）。工具栏那四十几个按钮是用户自己
  的 —— `[ToolBar]` 里存着他排过的顺序 —— 皮肤只有「加一格」的权力。
- 一个 `ui.layout:toolbar` 一个按钮。加三个就写三行，顺序就是声明顺序。
- **画法仍走 `ui.paint:on("toolbar.button", fn)`**，和别的按钮一样。想单独打扮自己那一格
  就按 `ctx.part` 分支 —— 皮肤按钮的 `ctx.part` 是声明里的 `command`，`ctx.text` /
  `ctx.label` 是 `tip`（内置按钮那里是命令名和 `cmdtext` 的说明）。
- 皮肤按钮**恒可用**：命令跑不跑得起来是点击那一刻的事，画成灰的等于提前替脚本撒谎。
  真写错了命令名，点下去状态栏会说「没有命令 …」。
- 热重载会重建按钮表（按钮数是表的一部分，多一格就是多一格宽，整条带子跟着重排）。
- **没有 `ui.layout:toolbar{}` 的整条接管**：工具栏不像状态栏那样能整套换掉，只能加。

## 五之十六、写完先体检：`--ui-lint`

```
dogedit.exe --ui-lint packages\luapaint\paint.lua
```

不开窗、不截图，把那个文件里**每个**画法在无窗口的 Lua 宿主里跑一遍（六档
状态各一次），打一张表：

```
ui-lint: packages/luapaint/paint.lua

10 个区域 · 6 档状态 · 96 dpi · 无窗口

region            ops   max  drawn  content  instr  reads
dock.tab            2     3    6/6        -    <1k  -
pane.docinfo        4     4    6/6        -     1k  doc
pane.help          20    20    6/6      266     1k  thumb scroll bar
scroll.v            1     2    6/6        -    <1k  thumb
status.clock        4     4    6/6        -    <1k  text
status.pane         5     6  90/90        -     1k  part text doc
toolbar.button      0     2    5/6        -     1k  -

结论：10 个区域，0 处错误
```

| 列 | 意思 |
| --- | --- |
| `ops` | 每次跑平均发了几条绘制指令 |
| `max` | 六档里最多的一次发了几条 |
| `drawn` | 有几档说了「我画了」（`return true` 或返回数字）。`5/6` 是正常的 —— 灰掉的按钮不该说画了 |
| `content` | `return` 的那个数字（内容高，滚动条要用的）。没返回就是 `-` |
| `instr` | 这一格烧掉几千条 Lua 指令。`<1k` = 连一次预算钩子都没响，正常值 |
| `reads` | 读了 `ctx` 里哪几样「缓存键放不下」的东西。读了 `text`/`doc`/`scroll` 就多一份比对开销 |

表下面按「需要人管」的顺序列四种**静默失效** —— 它们的共同点是：屏幕上只是少画了
一块，没有任何人会报错。

- **抛错**：这一格退回内置画法。只报一次（同一个错在六档里抛六次，只写一行），
  完整栈在 stderr。
- **从未说「我画了」**：回调返回了 `nil`/`false`，等于交回内置画法。常见原因是
  `return true` 写在某个 `if` 里面。
- **说画了但一条 op 都没发**：`return true` 但 `c:` 一次没调，或者全被条件跳过了。
- **六档状态画出完全一样的 op**：只查按钮、页签、滚动条、编辑区。时钟不看状态是
  正常的，不报。

最后一段是**声明和画法对不上**：

- `ui.paint:on("pane.x")` 但没有 `ui.layout:pane{ id = "x" }` → 这段代码永远不会跑。
- `ui.paint:on("status.y")` 但没有任何一段的 `id` 是 `y` → 同上。皮肤没接管状态栏时，
  能跑的是内置那 14 格（`message` / `filesize` / … / `encoding` / `offset`）；
  `status.pane` 是**整条兜底画法**，永远有活干，不算。

**退出码**：`0` 干净，`1` 有区域抛错（能进 CI），`2` 是路径不对 / 文件不存在 /
语法错。

三条边界，知道一下：

- **一次只读一个文件**。包里有几个文件就分别跑几次 —— 它收的是文件，不是包。
- **几何是捏的**：按区域名给个比例像样的矩形（滚动条 200×14、状态格 120×22、窗格
  让出轨道宽）。你的画法该按 `ctx.rect` 算，不该按区域名猜尺寸 —— 所以捏的值只要
  「比例像」就够了。真机上某一格看起来不对，那是**几何**的事，不是这张表能查的。
- **它不告诉你「好不好看」**。查的是「跑没跑、画了几笔、错没错」，颜色和间距还得
  自己看。

顺带一句为什么没有 `App.ui.inspect(x, y)`：在 `ui.paint:on` 里，那个函数能答的东西
`ctx` 已经全有了（坐标、矩形、部件名、状态、文字、DPI、主题…）。真正缺的信号是
反面那个 —— **某个区域从来没被调用**，而「传一组坐标进来」恰恰要求人先知道坐标。

## 五之十七、运行时改外观：`App.ui.style`

主题文件是**声明式**的：写得了「页签标记是绿的」，写不了「文件改了变橙、保存后
变回来」。会变的那部分只能由脚本表达，`App.ui.style` 就是那个口子。

四段对四个动词，和主题文件的四段一一对应：

| 主题文件 | 运行时 |
| --- | --- |
| `[Theme] tab.mark=#E67E22` | `App.ui.style:set("tab.mark", "#E67E22")` |
| `[Metrics] status.height=26` | `App.ui.style:metric("status.height", 26)` |
| `[Shape] tab.states.active.radius=0` | `App.ui.style:shape("tab", "active", "radius", 0)` |
| `[Fonts] mono.face=Consolas` | `App.ui.style:font("mono", { face = "Consolas" })` |

```lua
-- 文件一改，页签标记换橙色；保存后换回来
App.on("doc.modified", function() App.ui.style:set("tab.mark", "#E67E22") end)
App.on("doc.saved",    function() App.ui.style:reset("tab.mark") end)
```

**值传 `nil` 就是复位**（回到主题给的那个）。这条规则四段通用，所以不需要四套
`reset` 重载。`set` 另外还留着一个 `style:reset(token)` 的写法 —— 颜色这一格用
得最多，值得一个短名字。

### 四组动词

```lua
-- 颜色：89 个 token，见主题文件的 [Theme]
style:set(token, color)         -- nil = 复位
style:reset(token)
style:get(token)                -- 生效值（含覆盖）；没这个 token 回 nil
style:list()                    -- 只列**被改过**的：{ ["tab.mark"] = "#E67E22" }

-- 度量：带夹取（写 500 会夹成 48，并往日志里说一句）
style:metric(name, value)       -- nil = 复位
style:metrics()                 -- { ["status.height"] = 26 }

-- 形状：区域 / 状态 / 键，和 [Shape] 的语法一一对应
style:shape(region, state, key, value)   -- state 可为 nil（常态层）；value = nil 复位
style:shapes()                  -- { ["tab.states.active.radius"] = 0 }

-- 字体：按字段改，没写的字段留着主题给的
style:font(token, { face = "Consolas", size = 12 })  -- nil = 整个 token 复位
style:font(token, "size", 12)                        -- 单字段写法
style:font(token, "size", nil)                       -- 清一个字段（表里的 nil 等于没写）
style:fonts()                   -- { ["mono.face"] = "Consolas" }
```

`style:metrics()` / `:shapes()` / `:fonts()` 给出来的键**就是主题文件里的写法**，
所以调试完直接粘回 `themes/<name>.ini` 就变成永久的：

```lua
App.log(App.ui.style:shapes()["tab.radius"])   -- 顺手看一眼改成了什么
```

### 三条边界

- **区域名会展开。** `style:shape("status", nil, "gap", 2)` 一次写 `status.pane` 和
  `status.grip` 两个区域 —— 和主题文件里写 `status.gap=2` 展开成的是同一批。
  `style:shapes()` 列出的是展开后的结果，不是那个族名。
- **写错当场报错。** token 名、度量名、区域名、状态名、形状键、字体 token、字体
  字段，七种写错各有各的说法，并且都列出能写哪些。静默收下才是最难查的 ——
  「改了没反应」比报错贵得多。
- **改完是「下一次重画就对了」，不是「当场」。** 覆盖记在脚本侧，外壳在每个入口
  返回后统一重算一次外观。同一帧里连改十个 token 只重画一次。

### 为什么字体和度量会连带重排窗口

`status.height` 不是「状态栏多高」这个数字本身 —— comctl32 在 `WM_SETFONT` 之后
报的那个自然高度才是。所以改度量或字体时外壳会把按 DPI 缓存的那几只字体作废、
重算条高、再整排一遍（和换 DPI 是同一套动作）。屏幕上看到的是「状态栏变高、
下面的东西整体上移」，不是某一格挪了一像素。

## 五之十八、运行时增删改元素：`ui.layout:update` / `:remove` / `:list`

声明那一套（五之十三 ~ 五之十五）是「装载时说什么就有什么」。可有些东西是
**事件来了才知道**的：存完盘给状态栏推一句「已保存」，检测到某种情况才把一块
窗格拆掉。为这些改一整条声明太笨，所以有三个动词：

```lua
-- 推：事件来了才写。这一格原来是靠 update 函数自己走秒的，推了静态文字
-- 就把那个函数摘掉 —— 否则屏幕上还是函数算出来的那个，推了等于没推。
App.on("doc.saved", function()
  App.ui.layout:update("status.clock", { text = "已保存" })
end)

App.ui.layout:update("pane.uptime", { title = "秒表", side = "left" })
App.ui.layout:remove("pane.probe")
App.ui.layout:list()      -- { ["pane.uptime"] = {...}, ["status.clock"] = {...}, … }
```

**地址是「种类.名字」**，按第一个点切（名字自己可以带点）：

| 种类 | 名字是 | 能改的字段 |
| --- | --- | --- |
| `pane.<id>` | 窗格 id | `side` `title` `icon` `size` `bar` |
| `status.<id>` | 段 id | `text` `update` `width` `priority` `click` |
| `toolbar.<命令名>` | 按钮的命令名（按钮没有 id） | `icon` `tip` |

四条规则：

- **只列外壳真的会读的字段。** 收下一个没人读的字段等于「改了没反应」，所以改
  别的字段当场报错并列出能改的那几个。
- **改的是原来那条声明**，块名（属于哪个脚本文件）不跟着换。否则声明方一重载
  就清不掉它，越跑越多。
- **`list()` 的键就是 `update` / `remove` 收的那个地址** —— 调试时
  `for k in pairs(App.ui.layout:list()) do print(k) end` 就是「我建了什么」。
- **窗格会真的拆掉**：外壳重排时把不在声明表里的那块销毁，页签也跟着摘走。状态
  段和工具栏按钮下一次重排就消失。

和外观那套一样，改完是「下一跳重排就对了」：布局脏位由外壳在每个入口返回后取
走，然后建/拆窗口、重取状态段、重建工具栏（和热重载走同一条路）。

## 六、缓存：回调不保证每帧都跑

绘制结果按「区域 + 部件 + 状态 + 尺寸 + DPI + 序号」缓存。**输入没变就不进 Lua**，
直接还上一份 op 列表。这不是可选项，是性能的地基：鼠标在 42 格的工具栏上挪一格，
整条要重画一遍，其中 41 格的输入一模一样 —— 缓存让那 41 格变成「克隆一个指针」，
命中率实测 97% 以上。

对写皮肤的人来说，只有三条要知道：

- **别指望回调每帧都被调用。** 它可能一次都不被调用。所以不能靠回调里的副作用
  推进状态（计数、自增、`math.random`）—— 要动就在命令 / 事件 / `App.timer`
  里改状态，再 `App.ui.repaint("pane.<id>")` 丢掉这份缓存；让动画每帧变的另一条
  路是在画法里读 `App.now()`（那条路不进缓存）。
- **你读了什么，就按什么失效。** 缓存比对的是「上一次算这份 op 时你读过的那几样」
  加上尺寸和状态：
  - 读 `ctx.text` / `ctx.label` / `ctx.value` / `ctx.thumb` / `ctx.part` / `ctx.doc` →
    那一样变了就重算。所以**别在回调里读用不上的字段**，读了就是给自己加失效条件。
    读 `ctx.doc` 的代价是「改一个字、存一次盘就重算这一格」—— 按文档状态配色值得，
    只是别顺手 `local d = ctx.doc` 之后再也不用。
  - 用 `c:text(r, "@text", …)` 这种占位符写法 → 回调不读 `ctx.text`，状态栏的
    「行 12，列 34」每帧都变也照样命中。这是推荐写法，也是它存在的原因。
  - 位置（`ctx.rect` 的左上角）**总是**参与比对：页签条和状态栏的 op 用的是客户区
    绝对坐标，尺寸相同、位置不同的两格不能共用一份结果。
- **读了 `App.now()` 就不缓存。** 每毫秒都不一样的东西存下来等于把动画冻住。
  代价是这个区域退回「每帧重算」—— 例子里那条靠 50 ms 心跳扫来扫去的细线就是
  这么活的。做动画可以，别指望它被缓存。

换了脚本（`load` 第二次）缓存整表作废，不存在「新脚本沿用旧画法」。

### 查缓存为什么不命中：`--skin-debug`

```
dogedit.exe --skin-debug
```

打开之后每个绘制回调**连跑两遍**，比两次画出来的东西一样不一样。不一样就说明这个
画法不纯（缓存的前提就是纯函数），会印一行，带第一处不同的位置和两边的原值：

```
[script] paint 有副作用：status.pane 第 0 格（message）两遍跑出来不一样 —— 第 0 条 op：
Fill { … color: Color { r: 0, g: 255, b: 255, a: 255 } } → Fill { … color: Color { r: 255, g: 0, b: 255, a: 255 } }。
缓存的前提是纯函数，副作用请放到 update 里。
```

- **一处只报一次**，不是每帧一行。
- **读了 `App.now()` 的画法不比** —— 它本来就每毫秒不一样。这类画法本来也不进缓存。
- 关窗时打一行总账，顺手把命中率报出来：

  ```
  [script] debug: 有副作用的画法 0 处；绘制缓存命中 3/1049（0.3%），另有 1014 次读了 App.now() 不进缓存
  ```

  命中率低先看后面那个数：**读了时钟的画法一次都命中不了**，样例皮肤的状态栏就是
  这么活的（那条扫来扫去的细线），所以它的命中率天然很低。把时钟去掉再看这个数。
- 也可以不写命令行，用环境变量 `DOGEDIT_SKIN_DEBUG=1`。
- 它让每次绘制翻倍，只在查问题时开。

## 七、预算：跑不完的回调会被掐掉

沙箱挡住的是 `io`/`os`/`package`，挡不住 `while true do end`。所以每个回调都带
一份**指令预算**，烧光就当场抛错、这一次调用按「没画」处理：

| 入口 | 预算（Lua VM 指令条数） |
| --- | --- |
| 绘制回调 `ui.paint:on(...)` | 20 万 |
| 皮肤文件本身（`load` 顶层） | 2000 万 |
| 事件 / 命令 / 定时器 / 点击回调 | 2000 万 |

- 预算**按次算**，不是整条 VM 共用一个池子：一格烧光不影响下一格。
- 20 万条指令大约是「画十几笔」的一百倍。正常画法一次几千条，撞不到。
- 撞上了会看到一行 `[script] status.pane paint: runtime error: script exceeded its
  instruction budget (200000 instructions)`，然后那一格回到内置画法 —— 不是白屏。
- **它是防手滑，不是防恶意。** Lua 的 `pcall` 能接住钩子抛的错，所以存心绕的
  脚本可以把错误吞掉继续跑；应用这边会再兜一次（烧满预算的那次调用一律不算
  「画了」），但真正意义上的硬墙需要把脚本放到另一个进程里，这一版没有。
- 计的是**指令数**不是毫秒：同一段脚本每次烧掉的一样多，你能算、测试也能断言。

## 八、能踩的坑

1. **不要在回调里缓存 `ctx`**。它每帧被改写，存下来只会读到最后一帧的值。
2. **不要在回调里做重活**。绘制在 UI 线程上，一次 `WM_PAINT` 的预算是 8 ms。
   输入没变时不会进 Lua（见第六节），但真跑起来的那次要在预算内；真卡住了
   还有指令预算兜着（见第七节），代价是这一格退回内置画法。
3. **`c:image` 的 `index` 是图标格的序号**，不是路径。图标表由内置画法建。
4. **`ctx.hot` 五个区域都是准的**（状态栏、页签条、滚动条、工具栏、停靠页签）。状态栏
   和工具栏都不是 comctl32 自己报的 —— 那边根本不发悬停消息，是窗口过程在
   `WM_MOUSEMOVE` 里自己做命中测试、`TME_LEAVE` 清位，绘制时补一位。所以
   `TB_GETHOTITEM` 这类控件查询读不到，只认 `ctx.hot`。
5. 字体：`opts.font` 收下并进字体表，但回放层现在只有「这个区域自己的字体」
   一格 —— 想让某块用别的字体，改 `[fonts]` 里对应的 token（见五之十）。
6. **绘制回调里 `print` 和 `App.log` 都能用**，两条落到同一个地方（stderr，挂调试器时
   还进 `OutputDebugString`）。`App.log` 以前要 bridge，绘制期会报
   `no bridge: called outside a host entry point`，把整个回调掐掉 —— 现在不会了。
   **`App.alert` 仍然不行**：那是模态框，画到一半弹一个出来就是死锁。点击回调
   （`App.ui.on`）里三个都行。

## 九、完整例子

`packages/luapaint/` 是这一页所有东西的实例：渐变底、每格一条彩色窄条、
按部件名分色、右边一条分隔线、`ctx.hot` 时加一圈描边、一条靠心跳定时器扫来
扫去的 2 px 细线、竖横两条滚动条的胶囊滑块、工具栏每格按状态换底子、停靠
页签那条按 `active`/`hot` 换底、选中格脚下一道黄线的画法，以及三行 `ui.paint:hint`：
工具栏和两条页签条上指针变手形，两条页签条还把标题当提示弹出来。
