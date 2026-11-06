# 自绘控件共用化方案（customdraw / text / ctlcolor）

> 目标：把「同一件事写了四份」的自绘控件代码收成一份，像 `ownerdraw` 收掉四个
> 自绘按钮那样。范围只限**已经重复**且**重复得有风险**的部分，不追求把每个
> 画像素的函数都塞进一个抽象里。
>
> 前置：`crates/app/src/win32/ownerdraw.rs`（较早的重构）已经把四个对话框的
> `BS_OWNERDRAW` 按钮合成一份，本方案是它的续集。
>
> 基线：项目当前 HEAD。

## 0. 结论摘要

| 阶段 | 做什么                                          | 收益                                    | 改动面           | 预期像素变化      |
| -- | -------------------------------------------- | ------------------------------------- | ------------- | ----------- |
| 1  | `ownerdraw::text`：把 `label` 的画字部分提出来       | 修掉 3 处**仍然带结尾 NUL** 的画字（勾选符偏 3 px）    | 4 文件         | 有，且是修好的方向   |
| 2  | `customdraw::row`：`NM_CUSTOMDRAW` 行着色阶梯 4 合 1 | 最大重复；`dirtree`/`resultlist` 目前字节级相同 | 5 文件         | **必须 0**    |
| 3  | `customdraw::ctlcolor`：27 个 `WM_CTLCOLOR*` 分支 | 最多次数的惯用法，且漏 `SetBkColor` 是真实坑         | 11 文件（5 个 CRLF） | **必须 0**    |
| 4  | 表头带合并 —— **不做**：几何来源不同，签名不成立             | 0（共用的画字部分阶段 1 已抽走）                    | 0 文件          | —           |
| 5  | 源码 lint：禁止 `chain([0])` 进 `DrawTextW`           | 这个 bug 已经复发两次                          | 1 文件（`ownerdraw` 的 `mod tests`） | 无           |

顺序理由：1 最小、修活 bug、并给 4 提供原语；2 重复最多、风险最集中；3 机械但
面最广；4 依赖 1；5 收尾防复发。

## 1. 现状事实（调研结论，含行号）

### 1.1 `NM_CUSTOMDRAW` 行着色阶梯：4 份，其中 2 份字节级相同

| 文件                | 行    | 决策                                     | 额外阶段            |
| ----------------- | ---- | -------------------------------------- | --------------- |
| `dirtree.rs`      | 797  | selected / hot / normal                | —               |
| `resultlist.rs`   | 308  | selected / hot / normal                | —               |
| `shortcutdlg.rs`  | 1062 | selected / live / dead                 | —               |
| `markdlg.rs`      | 823  | selected / 标记槽位色                       | `CDDS_SUBITEM`  |

`dirtree` 与 `resultlist` 的整段（`NM_CUSTOMDRAW =>` 到 `NM_DBLCLK` 之前）在
空白归一化后**完全相同**，各 542 字符。四份共用的骨架是：

```rust
if stage == CDDS_PREPAINT      { return CDRF_NOTIFYITEMDRAW; }
if stage == CDDS_ITEMPREPAINT  { /* 挑一对颜色 */ ; return CDRF_NEWFONT; }
0
```

差异只有「挑哪一对颜色」。类型上分两族：树用 `NMTVCUSTOMDRAW` + `CDIS_SELECTED/
CDIS_HOT` 位标志，列表用 `NMLVCUSTOMDRAW` + `LVIS_SELECTED`（原始 `.0 & mask`）。
但四份都已经归约成 `(selected, hot)` 两个 bool，所以抽象点就在这一对 bool 上。

### 1.2 `WM_CTLCOLOR*`：27 个分支 / 11 个文件

| 文件               | 行号                                                              | 分支数 |
| ---------------- | --------------------------------------------------------------- | --- |
| `optionsdlg.rs`  | 2958, 2967, 4647, 4657, 4810, 4820, 5768, 5778, 5893            | 9   |
| `gotoany.rs`     | 2064, 2071, 2078                                                | 3   |
| `shortcutdlg.rs` | 1344, 1351, 1358                                                | 3   |
| `encdlg.rs`      | 751, 759                                                        | 2   |
| `grepdlg.rs`     | 2280, 2288                                                      | 2   |
| `searchdlg.rs`   | 381, 388                                                        | 2   |
| `tooldlg.rs`     | 1968, 1976                                                      | 2   |
| `markdlg.rs`     | 963                                                             | 1   |
| `minifind.rs`    | 648                                                             | 1   |
| `syntaxdlg.rs`   | 715                                                             | 1   |
| `window.rs`      | 18722                                                           | 1   |

结构恒定，只有三元组不同：

```rust
let hdc = HDC(wparam.0 as *mut _);
SetTextColor(hdc, <ink>);
SetBkColor(hdc, <face>);
<brush>.0 as isize
```

两个例外：`window.rs` 把 `dock_tree` 的过滤框转交 `dock_tree.color_edit(hdc)`；
`tooldlg.rs` 的返回类型是 `Option<LRESULT>` 而不是 `isize`。

另外注意：全仓 `SetBkColor` 共 49 次，除这 27 个分支外，其余在自绘函数内部
（属于阶段 1 的地盘）。

### 1.3 文字绘制：3 处仍把结尾 NUL 交给 `DrawTextW`（活的 bug）

`windows` 0.62 的 `DrawTextW` 签名是 `&mut [u16]`，`cchText` 取 `slice.len()`，
所以结尾 NUL 会被当成**一个字符**。「添加(A)」画出来是 52 px（裸切片）vs 59 px
（带 NUL），宽 7 px，居中因此偏 3 px。

| 文件               | 行    | 画什么           | 后果                              |
| ---------------- | ---- | ------------- | ------------------------------- |
| `markdlg.rs`     | 893  | 勾选符 `✓`       | 14 px 的框里居中偏左 3 px（阶段 1 修）      |
| `shortcutdlg.rs` | 1143 | 表头四列标题        | 左对齐，不位移，但量出来宽 7 px              |
| `syntaxdlg.rs`   | 664  | 表头标题          | 同上                              |

`menu.rs` 已经知道这个坑，用 `label.len().saturating_sub(1)` 绕开——第三种
写法。三处都手抄了同一段 `SelectObject(font) + SetBkMode(TRANSPARENT) +
SetTextColor + DrawTextW`。

### 1.4 表头带：2 份（评估后判定**不可合并**）

`shortcutdlg.rs` 的 `paint_header`（约 60 行）与 `syntaxdlg.rs` 的
`on_header_draw`（约 40 行）看着像同一件事：底色带 → 逐列格子 → 1 px 分隔线 →
`DT_LEFT|DT_SINGLELINE|DT_VCENTER|DT_NOPREFIX` 的标题。

当时判断「差别只在驱动方式」，实际动手时发现**几何来源也不同**：

- `shortcutdlg` 的表头是它**自己画在对话框上**的——底下是一个普通 `ListBox`，
  没有表头控件。所以它有整条 band 的矩形、有 `COLUMNS[i]` 宽度表，自己算每一格
  的 x，分隔线画在**两格之间**（整条带高）。
- `syntaxdlg` 的表头是 `SysHeader32`，`NM_CUSTOMDRAW` **一次只给一格**：
  `dwItemSpec` 是列号、`nmcd.rc` 是那一格的矩形。它没有 band、没有宽度表，
  分隔线画在**每格右缘**、上下各内缩 `3/dpi`。

也就是说 `header_band(hdc, band, cells, style)` 这个签名对 `syntaxdlg` 根本不成立
——它拿不到 band，除非跨回调把格子攒起来，那比重复更糟。真正共用的部分
（「填一个矩形 + 左对齐画一行标题」）在阶段 1 已经就是 `ownerdraw::draw_text` 了。

### 1.5 不值得共用的（附理由，避免以后重复讨论）

- **`tabstrip::paint_tab`（1261/1312）**：用 `MoveToEx`/`LineTo` 画不对称的偏移，
  顶部一条状态带，填充越过客户区底边。不是「矩形 + 边框 + 文字」，塞进 `Look`
  只会比重复更糟。
- **`toolbar::custom_draw`（815）**：形状接近（面色 + 1 px 边框 + ink 按状态），
  但要应答 comctl32 的 `TBCDRF_*` 标志字（`0xC1000 << 4`），是在复刻目标行为。
  可以借用 `Edge`/`Ink` 的颜色，管线搬不过去。
- **`menu.rs`**：gutter、勾选位、快捷键列、子菜单箭头，几何全是定制的。
- **`outputwin.rs`**：只是把 `NM_CUSTOMDRAW` 转给 `toolbar::custom_draw`，
  自己没有画法。

## 2. 阶段计划

### 阶段 1：`ownerdraw::text` —— 抽出画字原语，修掉 3 处 NUL

`ownerdraw.rs` 里把 `label`（228 行）的正文提成：

```rust
/// 往任意 DC 里画一行字，字体和墨色由调用方给。`label` 就是这个加一次状态
/// 查询；表头和勾选符直接调它。
pub(super) unsafe fn text(
    hdc: HDC, mut rc: RECT, font: HFONT, ink: COLORREF, s: &str, align: Align,
) {
    let old = SelectObject(hdc, HGDIOBJ(font.0));
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, ink);
    // 不带结尾 NUL：windows 0.62 的 DrawTextW 取 slice.len() 当 cchText，
    // NUL 会被算成一个字符（宽度差 7 px，居中因此偏 3 px）。
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    let flags = match align { Align::Center => DT_CENTER, Align::Left(_) => DT_LEFT };
    DrawTextW(hdc, &mut buf, &mut rc, flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    SelectObject(hdc, old);
}
```

`label` 退化成三行（状态 → `label_rect` → `text`）。

迁移点：

- `markdlg.rs-901` 勾选符 → `ownerdraw::text(di.hDC, box_rc, st.font, ink, "✓", Align::Center)`，
  去掉 `chain([0])` 和手抄的 5 行。
- `shortcutdlg.rs-1150` 表头 → `Align::Left(0)`（`rc.left` 自己先加 6 px）。
- `syntaxdlg.rs-672` 表头 → 同上。

**验证**：这三个界面前后截图（命令 id 见 3.2 的表），差异应当**只有字形**，且是
向右/居中移动 ~3 px。勾选符那段只在「开启该功能」勾上时才执行，需要先用 posted
`BM_CLICK` 把选项打开再截，否则前后两图都是 0 差异、什么也没验证到。

### 阶段 2：`customdraw::row` —— 4 合 1

新模块 `crates/app/src/win32/customdraw.rs`（LF）。用 trait 吃掉两族类型：

```rust
pub(super) trait Row {
    fn stage(&self) -> u32;
    fn item(&self) -> usize;
    fn selected(&self) -> bool;
    fn hot(&self) -> bool;
    fn set(&mut self, fg: COLORREF, bg: COLORREF);
}
impl Row for NMLVCUSTOMDRAW { /* LVIS_SELECTED；hot 恒 false，两个列表站点都不用 */ }
impl Row for NMTVCUSTOMDRAW { /* CDIS_SELECTED / CDIS_HOT */ }

/// 阶梯：PREPAINT 要 item 通知；ITEM（或 SUBITEM）问一次配色，回 CDRF_NEWFONT。
pub(super) fn row<T: Row>(
    cd: &mut T,
    subitems: bool,
    pick: impl FnOnce(usize, bool, bool) -> (COLORREF, COLORREF),
) -> isize {
    let stage = cd.stage();
    if stage == CDDS_PREPAINT.0 { return CDRF_NOTIFYITEMDRAW as isize; }
    if stage == CDDS_ITEMPREPAINT.0 && subitems { return CDRF_NOTIFYSUBITEMDRAW as isize; }
    if stage == CDDS_ITEMPREPAINT.0 || stage == (CDDS_ITEMPREPAINT.0 | CDDS_SUBITEM.0) {
        let (fg, bg) = pick(cd.item(), cd.selected(), cd.hot());
        cd.set(fg, bg);
        return CDRF_NEWFONT as isize;
    }
    0
}
```

调用点变成（以 `resultlist` 为例，20 行 → 6 行）：

```rust
NM_CUSTOMDRAW => {
    let cd = &mut *(lparam.0 as *mut NMTVCUSTOMDRAW);
    Some(LRESULT(customdraw::row(cd, false, |_, sel, hot| {
        (self.fg, if sel { self.sel_bg } else if hot { self.hover } else { self.bg })
    })))
}
```

`markdlg` 传 `subitems = true`，`pick` 里读 `st.markers` 取槽位色。

**必须原样保留**：`CDRF_NOTIFYITEMDRAW`（PREPAINT）、`CDRF_NEWFONT`（ITEM）、
`CDRF_NOTIFYSUBITEMDRAW`（markdlg 的 ITEM 级）。返回值错了列表会变黑白。

**验证**：四个界面（目录树、搜索结果、快捷键、标记）前后截图，**必须 0 像素**。
选中态和悬停态现在没法用真鼠标驱动（本会话 `GetForegroundWindow()` 返回 0，
见 3.3），所以补两条单元测试：`stage()` 的解码，和「四种配色决策各选对颜色」
（构造 `NMLVCUSTOMDRAW` 直接断言 `clrText/clrTextBk`）。

### 阶段 3：`customdraw::ctlcolor` —— 27 处收成一个函数

```rust
/// 让 `Color` 和已经是 `COLORREF` 的调用方都能传，省掉每处的 to_colorref。
pub(super) trait AsColorref { fn as_colorref(self) -> COLORREF; }
impl AsColorref for Color { fn as_colorref(self) -> COLORREF { to_colorref(self) } }
impl AsColorref for COLORREF { fn as_colorref(self) -> COLORREF { self } }

pub(super) fn ctlcolor(
    hdc: HDC, ink: impl AsColorref, face: impl AsColorref, brush: HBRUSH,
) -> isize {
    unsafe {
        SetTextColor(hdc, ink.as_colorref());
        SetBkColor(hdc, face.as_colorref());
    }
    brush.0 as isize
}
```

每个分支从 4 行降到 2 行：

```rust
WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
    let st = state(hwnd);
    customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.theme.toolbar_fg, st.theme.toolbar_bg, st.bg)
}
```

两个例外单独处理：`tooldlg` 包一层 `Some(LRESULT(..))`；`window.rs` 保留
`dock_tree` 的委派分支，只在非委派路径上调 helper。

**验证**：`optionsdlg`（9 处，含 3 个不同 proc）、`encdlg`、`searchdlg`、`grepdlg`、
`gotoany`、`tooldlg`、`minifind`、`markdlg`、`syntaxdlg` 全部 **0 像素**。
`tooldlg` 与 `gotoany` 是模态的，用 `_dlg_shot.ps1` 的对应菜单 id 取图。

### 阶段 4：表头带合并 —— **不做**（评估后判定不成立）

原计划签名 `customdraw::header_band(hdc, band: RECT, cells: &[(i32, &str)], style: HeaderStyle)`
——底色、逐格文字、1 px 分隔线、底部分隔线、可选的外框。

**为什么放弃**：这个签名假设两个站点都有「一条 band + 一张宽度表」。`shortcutdlg`
有，`syntaxdlg` 没有——它是 `SysHeader32` 的 `NM_CUSTOMDRAW`，一次回调只给一格
（`dwItemSpec` + `nmcd.rc`），band 和宽度表都在控件里，拿不到。要让它适配就得跨
回调攒格子，比现在的两份代码更难读。

就算硬凑一个「画一格」的版本，参数是 `(hdc, rc, bg, 分隔线色, 分隔线内缩, 左内缩,
右内缩, dpi, font, ink, title)` 共 11 个，`shortcutdlg` 那边省 0 行（它的分隔线是
格子之间、不是右缘，内缩也不是 3/dpi），`syntaxdlg` 省 4 行。计划里本来就写了
「如果阶段 1 之后觉得收益不够，这一段可以不做；它只有两个站点」——这就是那个情况。

真正共用的那部分（填矩形 + 左对齐画标题）阶段 1 已经抽成 `ownerdraw::draw_text`，
两个站点现在都在调它。剩下的差异是**几何来源不同**，不是重复。

### 阶段 5：防回归 —— 源码 lint 测试

这个 NUL 坑已经复发两次（`ownerdraw` 迁移时一次、勾选符还留着一次），值得一条
测试钉住。

**落地位置改了**：计划写在 `customdraw.rs` 的 `mod tests`，实际放进
`ownerdraw.rs` 的 `mod tests`——被约束的是 `draw_text` 和它的缓冲区，坑的说明也在
那儿，测试离开被测代码放到隔壁模块没有好处。

两条测试：

1. `the_draw_text_buffer_is_exactly_the_text`：`draw_text` 建缓冲区的三行提成
   `text_buf(s)`，断言 `len() == s.encode_utf16().count()`、末位不是终止符、
   空串为空、代理对算两个码元（不是 `chars()` 的 1）。
2. `no_draw_text_buffer_is_nul_terminated`：扫 `src/win32/*.rs` 的**每一条**
   `DrawTextW` 调用，取第二个实参；实参自己带 `chain([0])`/`push(0)` 就失败，
   是 `&mut buf` 就顺着 `let` 找绑定、再查绑定到调用之间有没有 `buf.push(0)`。

三条实现上的坑（都是先写错再改对的，别退回去）：

- **跳过 `mod tests {` 而不是 `#[cfg(test)]`**。`#[cfg(test)]` 也标在文件中间的
  测试专用辅助函数上（`outputwin.rs`、`menu.rs`），按它截断会把后面真正
  画字的地方一起切掉，扫描从 16 条静默变成 13 条。
- **多行调用不能先塞一个换行**。`DrawTextW(` 单独一行时，若先 `push('\n')` 再收
  参数，第一个实参是空串，`args[1]` 就从 `&mut buf` 错位成 `hdc`，绑定查不到、
  静默 `continue`——`menu`/`outputwin` 的 4 条就是这样漏掉的。
- **`&mut label[..label_len]` 要跳过**：那是「NUL 缓冲区切掉终止符」的正确写法
  （`menu.rs`），不是坑。

扫描下限写死 16（`complete` 3、`tabstrip` 6、`gotoany` 2、`menu` 2、`grepdlg` 1、
`outputwin` 1、`ownerdraw` 1），少找到就失败，防止扫描静默变成空转。

**负向验证**（必须做，否则「永远绿」的 lint 等于没有）：分别往 `gotoany.rs`
的绑定里塞 `chain([0])`、往 `outputwin.rs` 的绑定后塞 `label_wide.push(0)`，
两次都要失败并指出文件行号；验完 `git checkout` 还原。

## 3. 验证手段

### 3.1 每阶段都跑

```
cargo test --workspace          # 开工前 942 passed；收工时 948（阶段 2 加 4、阶段 5 加 2）
cargo build -p dg-app           # 必须 0 warning
./target/debug/dogedit.exe --selftest
```

### 3.2 像素基线

截图探针 -Cmd <id> -Out <png>` 取前后两版，Pillow 逐像素比：

- 阶段 1：差异**允许**存在，但必须在字形 bbox 内（渲染 `_diff.png` 放大 6 倍确认）。
- 阶段 2、3、4：**必须 0**。非 0 就是改坏了，不是「看起来差不多」。

各界面怎么打开（`-Cmd` 用的命令 id，已核对 `cmdtext.rs`）：

| 界面                        | 文件              | 打开方式                                   |
| ------------------------- | --------------- | -------------------------------------- |
| 自定义标记                     | `markdlg.rs`    | `-Cmd 1318`                            |
| 快捷键管理                     | `shortcutdlg.rs`| `-Cmd 1460`                            |
| 语法着色                      | `syntaxdlg.rs`  | `-Cmd 1459`                            |
| 转换编码                      | `encdlg.rs`     | `-Cmd 1659`（**收尾按取消**，确定会写 setting.ini） |
| 在文件中查找                    | `grepdlg.rs`    | `-Cmd 1408`                            |
| 用户工具                      | `tooldlg.rs`    | `-Cmd 1580`                            |
| 选项                        | `optionsdlg.rs` | `-Cmd 1336`（9 个 ctlcolor 分支都在这里）        |
| 跳转（Ctrl+P）                | `gotoany.rs`    | `-Cmd 1760`                            |
| 查找条                       | `minifind.rs`   | `-Cmd 57636`，但它是 **owned 顶层窗口**，用 `_find_bar_*` 系列 |
| 目录树 / 搜索结果                | `dirtree.rs` / `resultlist.rs` | 主窗口里的停靠面板，不需要命令；搜索结果需要先跑一次查找才有行 |
| 搜索进度                      | `searchdlg.rs`  | 只在「在文件中查找」执行期间出现，没有独立命令 id，用 `_dlg_shot` 抓不到 |

### 3.3 输入队列限制（会影响验证方式）

本会话 `GetForegroundWindow()` 返回 0，真实鼠标消息不到窗口，悬停/选中态没法用
真鼠标驱动。所以：

- 行着色、按钮悬停这类**状态相关**的改动，一律补单元测试（构造 `NMLVCUSTOMDRAW` /
  真隐藏 BUTTON + `SendMessageW`），不靠截图。
- 需要「切到某个状态再截图」时，用 posted 消息（`BM_CLICK`、`WM_MOUSEMOVE`），
  不用 `mouse_event`。

## 4. 风险与回滚

- **CRLF 文件**：本方案的 11 个 ctlcolor 文件里有 5 个是 CRLF —— `encdlg.rs`、
  `grepdlg.rs`、`optionsdlg.rs`、`tooldlg.rs`、`window.rs`。用 python 批量改时
  pattern 必须按 `\r\n` 构造，提交前 `git ls-files --eol` 的 `w/` 必须等于 `i/`，
  `git diff --stat` 只能剩真实改动。
- **禁止 `cargo fmt`**（仓库是旧版 rustfmt 格式化的，会重排 40 个文件 5 万行）。
- **`CDRF_*` 返回值**必须逐字节一致，改错的表现是列表整片黑白，不是编译错误。
- **分阶段提交**，每阶段一个 commit，body 带实测像素数；任一阶段像素不为预期就
  单独 `git checkout --` 回滚那一个文件，不影响其他阶段。

## 5. 不做的事

- 不把 `tabstrip` / `toolbar` / `menu` 的画法并进来（理由见 1.5）。
- 不改任何配色、几何、字号——这是纯重构，像素基线是唯一的验收标准（阶段 1 除外，
  它修的是 bug）。
- 不动 `outputwin` 的 DX 管线（`paint_text` 走的是另一套，和 GDI 无关）。
