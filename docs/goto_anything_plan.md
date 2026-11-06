# Goto Anything（跳转到文件）设计

> 目标：`cm_goto_anything`（id 1760，Ctrl+P，菜单「跳转到文件...」）。
> UI 采用 285×208 DLU 的可调整大小对话框；索引保持在内存里，复用
> `dg_core::fuzzy` 与后台线程 + PostMessage，支持评分排序与匹配高亮。

## 0. 结论摘要

| 决策点 | 选择 | 理由 |
|---|---|---|
| 窗体 | 模式less 对话框，285×208 DLU，可调整大小 | 对话框管理器负责 Tab/Enter/Esc/IME；不阻塞主窗口 |
| 文件数据源 | ① Everything 在跑 → WM_COPYDATA 查询；② 否则本地 walker + 目录 mtime 增量校验 | 少一个数据库依赖；有 Everything 时不需要本地索引 |
| 匹配算法 | `dg_core::fuzzy` 子序列匹配，评分排序 + 命中位置 | 子序列语义直观，且能高亮命中字符 |
| 符号查询 | 当前文档符号 + 可选的工作区 ctags 索引 | 语法树/LSP 已提供文档符号，ctags 作为补充 |
| 渲染 | 自绘列表行：名称 + 次要文本，命中字符用主题色高亮 | 与编辑器其余部分共用 `Theme` |

## 1. 入口与接入点

- 命令 id 1760；`key.ini` 默认 `C+P=cm_goto_anything`。
- 菜单行在 `crates/app/src/win32/cmdtext.rs`，分发在
  `crates/app/src/win32/window.rs`。
- 打开目标复用 `open_file(path)` + `goto_target: Option<(path, line, col)>`：
  文件打开后定位到 1-based 行、0-based 列。

## 2. 对话框结构

| 控件 | id | 作用 |
|---|---|---|
| 查询框 | 1200 | 子类化 `Edit`，每次 `EN_CHANGE` 触发一次查询 |
| 结果列表 | 1201 | `LBS_NODATA + LBS_OWNERDRAWFIXED`，行内容由本模块绘制 |
| 状态行 | 1202 | 索引状态、结果数、来源说明 |
| 重新索引 | 1203 | 清空缓存并重扫 |
| 关闭 | 2 | 等同 `IDCANCEL` |
| 标题静态文本 | 3000 | 可随查询更新 |

- 创建一次，之后重复使用同一个对话框实例；关闭只隐藏。
- `WM_GETMINMAXINFO` 从当前字体度量推导最小尺寸；`WM_SIZE` 重排子控件。
- 列表只保存条目索引，绘制时取数据，滚动不复制字符串。

## 3. 数据源

### 3.1 内存索引

`crates/core/src/fileindex.rs`：

- 后台线程遍历目录，产出 `Vec<FileEntry>`；
- 每个列出的目录记录一个 `DirStamp`（路径 + mtime），`revalidate` 只重新
  stat 目录，文件数不参与；
- 数量与深度有上限，跳过隐藏目录和 `SKIP_DIRS`（`.git`、`target`、
  `node_modules` 等）。

### 3.2 Everything IPC

`crates/app/src/win32/everything.rs`：

- 通过 `WM_COPYDATA` 向 Everything 发查询；
- 每次按键只取一页（`max_results = 400`），总数取自回复头；
- 不可用时返回原因，由状态行提示并回退到本地 walker。

### 3.3 符号（`@` 前缀）

- 当前文档符号优先（语法树 / LSP）；
- 工作区 `ctags -R` 索引可选，默认关闭；需要 `[Ctags] Enabled=1`，
  只扫描 `Dir` 与 `Extensions` 声明的范围；
- ctags 只是外部工具，仓库不捆绑二进制。

## 4. 匹配与排序

- 输入按子序列匹配路径与文件名；`@` 模式匹配符号名。
- 排序：评分高者在前，同分时短名在前，再按名称字典序，保证稳定。
- 命中的字符位置随结果返回，绘制时用主题的搜索高亮色描出。
- 空查询展示最近文件/目录概览；纯数字输入提示「跳转到行 N」。

## 5. 线程与刷新

- 索引线程与 UI 通过 `PostMessageW(WM_APP + n)` 通信，UI 线程只做装配。
- 每次查询带一个递增序号；只接受最新序号的结果，避免旧结果回填。
- 目录 mtime 未变则跳过重扫；Everything 模式下不维护本地索引。

## 6. 键盘与焦点

- 上/下移动选择，Enter 打开，Esc 关闭；Tab 在查询框与列表间切换。
- 打开结果时保留当前文档，新文件在后台读取，定位在加载完成后执行。
- 输入法组合期间不触发索引查询，组合结束后补一次。

## 7. 文件布局

| 文件 | 内容 |
|---|---|
| `crates/app/src/win32/gotoany.rs` | 对话框、列表绘制、键鼠处理、结果装配 |
| `crates/app/src/win32/everything.rs` | Everything IPC 查询与回复解析 |
| `crates/core/src/fileindex.rs` | 目录 walker、目录戳、fuzzy 查询 |
| `crates/app/src/win32/ctags.rs` | 可选的工作区符号源 |
| `crates/app/src/keys.rs` | 命令路由与 `key.ini` 绑定 |

## 8. 验收

- 空目录、单文件、十万文件目录都能打开且不卡 UI；
- Everything 在/不在两种路径都能给出结果或明确原因；
- `@` 符号查询在 ctags 关闭时只显示当前文档符号；
- 结果顺序稳定，重复查询不产生闪烁；
- 打开二进制/超大文件时走既有的大文件路径，不在本对话框内崩溃。
