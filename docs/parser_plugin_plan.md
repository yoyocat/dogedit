# tree-sitter 解析器插件化方案

> 目标：让用户**注册使用自定义 tree-sitter 语法解析器**（DLL 等形式），运行时动态加载，
> 不必把所有语言的解析器提前编进主程序。
>
> 技术基线：tree-sitter 0.27。

## 0. 结论摘要

| 决策点 | 选型 |
|---|---|
| 插件形态 | **目录式插件包**：`parsers\<name>\` 内 `manifest.toml` + `<name>.dll`（+ 可选 queries） |
| Helix DLL 复用 | **Helix 官方 grammar DLL 直接拷进 `parsers\` 插件包当普通解析器用**（245/245 通过，见 §2.5），不做 Helix 专属接入 |
| 加载方式 | v1 **进程内** `LoadLibraryW` + 取 `tree_sitter_<name>` 导出符号（与 Neovim/Helix 同类做法） |
| 加载时机 | 启动时只扫 manifest；**首次用到该语言时**才加载 DLL（惰性） |
| 注册中心 | 现有 `LanguageRegistry` 改为插件包驱动：`parsers\` 是唯一 grammar 来源，二进制不再内置语法（§4） |
| 扩展名映射 | manifest 声明 `extensions` 为默认 + `setting.ini [Syntax]` 手工覆盖，替代散落的硬编码 match |
| 配色 | v1 复用现有 `ThemeConfig`（manifest 可带 `[theme]` 覆盖表）；v2 升级为 `queries/highlights.scm` |
| 隔离 | v1 不做进程隔离（信任模型同 Neovim）；v3 可选 out-of-proc 解析宿主 |
| 卸载/热更 | **永不 `FreeLibrary`**（Tree/Parser 持有 DLL 内代码指针）；rescan 只新增，不改已加载项，换 DLL 需重启 |

核心好消息：`Highlighter` / `folding_spans` 只消费不透明的 `tree_sitter::Language`，
**对语言来源零感知**，插件机制是纯增量改动，解析/高亮/折叠管线一行不用动。

## 1. 现状与接入点

**dg-syntax（解析层）**
- `registry.rs`：`LanguageRegistry`，`HashMap<String, Language>`，全部走 cargo feature 编译期注册
  （json/rust/cpp/javascript/python/html，`Cargo.toml [features]`）。
- `highlight.rs`：`Highlighter::new(lang)` 增量解析 + `highlight_range()` 产出 `HighlightRun{color,len}`；
  配色走 `ThemeConfig`（node kind **子串匹配** → 颜色，非 .scm query）。
- `folding.rs`：`folding_spans(tree)` 从树形推折叠区。

**dg-app（应用层）**
- `window.rs`（open_file）/ `:6277`（apply_rename）：打开/重命名时
  `syntax_lang = 小写扩展名`（注意：存的是**扩展名**不是 grammar 名）。
- `window.rs`、`:21770`、`app.rs/6308`：散落的 `match ext { "rs" => "rust", ... }`
  硬编码扩展名→grammar 映射，然后 `reg.get(grammar)` + `attach_syntax(lang)`。
- `window.rs/8568` 等处**每次使用都 `LanguageRegistry::new()`**（现状便宜，插件化后要改成启动时建一次）。
- `statusbar.rs` `syntax_display_name()`、`window.rs` `snippets::bundled(syntax_lang)`、
  `window.rs` `lsp_start()` 的语言映射。
- 语义混用点：`syntax_lang` 有时是扩展名（"rs"），测试里又直接塞 grammar 名（`window.rs` 塞 "rust"）——本方案顺带统一。

## 2. 插件包格式

```
目标行为 所在目录/
└── parsers/                    # 启动扫描根（可由 setting.ini 增配目录）
    └── go/                     # 包目录名 = manifest.name（不匹配则拒绝加载）
        ├── manifest.toml
        ├── go.dll              # 解析器动态库（Windows；将来 .so/.dylib）
        └── queries/            # 可选，v2 启用
            ├── highlights.scm
            └── folds.scm
```

`manifest.toml`：

```toml
name     = "go"          # registry 键，全局唯一，小写 [a-z0-9_-]
display  = "Go"          # 状态栏/菜单显示名
version  = "0.1.0"
entry    = "tree_sitter_go"   # DLL 导出符号；缺省 = tree_sitter_{name}
dll      = "go.dll"           # 缺省 = {name}.dll

# 文件类型默认映射（setting.ini 可覆盖）
extensions = ["go"]
filenames  = []                # 可选：精确文件名匹配，如 "makefile"
first_lines = []               # 可选：首行前缀，如 "#!/usr/bin/env python3"（见 §5.4）

# 注意：配色**不**写在 manifest 里 —— 旧的 [theme] 段已删除，写了会报错。
# 配色只走 queries/highlights.scm（见 §6）。

# 可选：abi = [13, 15]   # 声明兼容区间，仅作快速拒绝；真正校验在运行时
# 可选：max_file_bytes = 4194304   # 超过这个大小的文档不附加本语法（见 §5.5）
```

规则：
- 包目录名 ≠ `manifest.name` → 拒绝并报错（防两包同名互踩目录）。
- `name` 与 `register()` 注册的同名项重名 → **外部覆盖内置**，并在输出窗口记一条日志
  （用户可换新版本 grammar 而不必等发行版）。
- ABI 校验以运行时为准（见 §3），manifest 里的 `abi` 只是文档性快速检查，可省。

## 2.5 已验证：Helix 官方 DLL 直接拷用

结论：**可以。** Helix 官方发行包的 grammar DLL 就是标准 tree-sitter 语言库，**不做 Helix 专属
接入**——拷进 §2 的插件包目录，当普通解析器用。实测（探针脚本：仓库根 `helix_dll_probe.py`，
LoadLibraryW + GetProcAddress + 调用导出函数 + 读 `TSLanguage` 偏移 0 的 `abi_version`，
布局核对自 vendored 0.27 `src/parser.h`）：

- **245/245 加载成功**，导出符号全部命中朴素映射 `tree_sitter_{name.replace('-', "_")}`
  （如 `c-sharp.dll` → `tree_sitter_c_sharp`），无一需要查导出表；语言指针全非空。
- **ABI 直读直方图：13×59、14×180、15×6** —— 全部落在 tree-sitter 0.27 的兼容区间
  `[MIN_COMPATIBLE_LANGUAGE_VERSION=13, LANGUAGE_VERSION=15]`，`set_language` 会全部接受。

拷用配方（以 go 为例，DLL 与 queries 都从 Helix 发行包拷入）：

```
parsers/
└── go/
    ├── manifest.toml          # 自己写：name="go"、extensions=["go"]；entry 缺省即 tree_sitter_go
    ├── go.dll                 # ← 拷自 helix runtime\grammars\go.dll
    └── queries/
        └── highlights.scm     # 可选：拷自 helix runtime\queries\go\，走 §6 渲染
```

两点注意：

- **配色**：不拷 queries 时走现有 `ThemeConfig` 子串匹配（有基础色）；拷了 `highlights.scm`
  就按 §6 的 query 路径渲染，capture 名（`@keyword`、`@function.method` 等点分层级）→
  颜色按**最长前缀**解析。`queries\<name>\` 里若引用了 `_` 开头的共享片段（如 `_typescript`），
  把被引用的片段一并拷入并在加载端做拼接（v2 实现时验证具体语法）。
- **扩展名映射自己填**：Helix 把 `languages.toml` 编进了它的二进制，runtime 里查不到
  file-types；好在用户通常只拷几种语言，manifest 的 `extensions` 手写即可。

## 3. 动态加载层（dg-syntax 新增 `dynlang.rs`）

依赖：`libloading`（Windows `LoadLibraryW` 路径）。tree-sitter 侧无需新依赖——
`tree-sitter-language` 0.1.7 已在依赖树里（grammar crate 的载体），提供 `LanguageFn::from_raw`。

### 3.1 加载流程

```rust
use libloading::{Library, Symbol};
use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

pub struct LoadedParser {
    pub lang: Language,
    /// 必须随进程存活。Language 指向 DLL 内静态数据，
    /// Tree/Parser/Query 都可能持有 DLL 里的代码指针——
    /// 任何时刻 FreeLibrary 都是 UB。
    _lib: Library, // 故意不实现 Drop 语义的使用：放进进程级单例，永不释放
}

pub unsafe fn load_parser(dll_path: &Path, entry: &str) -> Result<LoadedParser, PluginError> {
    // 绝对路径加载，避免 Windows DLL 搜索路径劫持
    let lib = Library::new(dll_path)?;                        // LoadLibraryW
    let sym: Symbol<unsafe extern "C" fn() -> *const ()> =
        lib.get(entry.as_bytes())?;                           // GetProcAddress
    let fp = *sym;
    std::mem::forget(sym);                                    // Symbol 借用 lib，转出手动管理
    let lang = Language::from(LanguageFn::from_raw(fp));      // 0.27 已核实存在
    // 版本 + 结构体合法性校验（set_language 内部做同样检查，这里提前拿明确错误）
    if !(tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION)
        .contains(&lang.abi_version())
    {
        return Err(PluginError::Abi { got: lang.abi_version() });
    }
    if !lang.is_parseable() {
        return Err(PluginError::NotParseable);
    }
    // 冒烟探针：空文档 parse 一次，拦掉加载即崩的低级坏包
    let mut p = tree_sitter::Parser::new();
    p.set_language(&lang).map_err(PluginError::SetLanguage)?;
    p.parse(b"", None);
    Ok(LoadedParser { lang, _lib: lib })
}
```

错误分类（`PluginError`）：`Manifest(字段缺失/非法)`、`Io`、`SymbolMissing(entry)`、
`Abi{got}`、`NotParseable`、`SetLanguage(LanguageError)`。每类都有用户可读文案 + 日志落输出窗口。

### 3.2 生命周期铁律

1. **永不 `FreeLibrary`**。`Library` 存进程级单例（`OnceLock<ParserStore>`），随进程退出释放。
2. **惰性加载**：启动只解析各包 `manifest.toml`（毫秒级）；`get("go")` 首次命中才 `LoadLibrary`。
3. **rescan 只增不改**：菜单「重新扫描解析器」只追加新发现的包名；已加载语言的 DLL 变更需重启生效
   （UI 提示"该语言将在重启后更新"）。

### 3.3 Windows 细节

- 全部用绝对路径 `LoadLibraryW`；不触碰 DLL 搜索顺序（防劫持）。
- 第三方 DLL 的 CRT 依赖（vcruntime 等）走系统常规解析；用 `tree-sitter build` 产出的 DLL 与本机一致。
- 解析器的 `TSLanguage` 是 DLL 内静态数据，节点分配由**我们进程里的 tree-sitter core** 完成，
  不存在跨 CRT malloc 混用问题（这也是 Neovim 进程内加载成立的原因）。

## 4. Registry 改造（registry.rs）

```rust
pub enum Source { Builtin, External(PathBuf) }

struct Entry {
    source: Source,
    display: String,
    exts: Vec<String>,
    // 惰性：Some=已解析的 manifest；External 未加载时 lang 为空
    lang: OnceCell<Language>,
    loader: Option<BundleLoader>,   // dll 路径 + entry，get() 时触发
}

impl LanguageRegistry {
    pub fn new() -> Self { /* 空表：发行版不内置任何 grammar */ }

    /// 启动时调用：扫描 parsers\ 目录，解析 manifest，返回可展示的诊断列表
    pub fn scan_plugins(&mut self, root: &Path) -> Vec<PluginDiag>;

    /// 现有 get() 语义不变：命中 External 且未加载 → 惰性 load，失败返回 None 并记日志
    pub fn get(&self, name: &str) -> Option<Language>;

    pub fn plugin_list(&self) -> impl Iterator<Item = PluginInfo>; // 供 UI 列表
}
```

- 内置注册路径（`register(name, Language)`）签名不变，留给嵌入方/测试。
- `dg-app` 侧把 5 处 `LanguageRegistry::new()` 改为持有一份（WindowState 字段，启动时
  `scan_plugins(exe_dir.join("parsers"))`），失败诊断写输出窗口。

**实施决策：二进制不再内置任何 grammar。** 原先 `dg-syntax` 用 cargo
feature 编进 json/rust/cpp/javascript/python/html 六个 grammar，与插件包完全重复，
还带来版本漂移与编译时间。现在：

- `LanguageRegistry::new()` 是空表，`register()` 只留给嵌入方/测试；
- 语法唯一来源是 `parsers\`（`tools/make_parsers.py` 生成，见 §10）；
- `builtin_extension` 更名 **`default_extension`** —— 表里只有名字没有 grammar，
  作用是包没写 `manifest.extensions` 时按常识兜底；
- 那六个 grammar crate 降级为 **dev-dependency**（`dg-syntax` 单测、`dg-app` 测试与
  `typing_bench`），`fixture-json-dll` 自带自己的 `tree-sitter-json`。
  `cargo tree -p dg-app -e normal` 现在只剩 `tree-sitter` + `tree-sitter-language`。

代价：全新 clone 且没跑生成脚本时没有任何语法着色（相关测试对 `parsers\` 缺失会跳过）。

## 5. 扩展名 → 语法映射（消灭散落的 match）

```
解析优先级（高→低）：
1. 插件包 manifest.filenames                      （精确文件名，如 Dockerfile）
2. 插件包 manifest.extensions                     （用户装包即生效，零配置）
3. 兜底扩展名表 dg_syntax::default_extension      （只有名字，没有 grammar）
4. 插件包 manifest.first_lines                    （首行前缀嗅探，如 shebang）
5. 无 → 不附加 tree-sitter（沿用现无语法路径）
```

第 1、2 步与第 4 步分开：前三步只看文件名，拿到就返回；只有**名字里看不出来**
（无扩展名，或扩展名没人认领）时才去读文件头那 256 字节。所以 `notes.txt` 里
写着 `#!/usr/bin/env python3` 也能按 python 打开，而 `mod.py` 仍然走扩展名 ——
用户改扩展名的意图优先于内容（见 §5.4）。

**2026-10-26 修订：`[Syntax] Map` 那一层已删除。** 原来它排在最前面，等于默认
不信任包自己的声明 —— 同一件事（某个扩展名归哪个语法）有两个来源，排查时要翻
两个文件。扩展名是**包数据**，只该由 manifest 说；`setting.ini` 只留**用户状态**
（`[Syntax] Disabled`）。配套删除的还有 `[Syntax] ExtraParserDirs`：找包目录改成
固定的三处（exe 旁 / 工作目录 / `%APPDATA%\dogedit\parsers`），不再靠配置
文件告诉程序去哪找配置。

两个包抢同一个扩展名时按**包名字典序、先到先得**（`plugins` 是 `BTreeMap`，
`resolve_ext` 顺序遍历；测试 `two_packages_claiming_one_extension_go_to_the_first_name`）。
想换就把包改名，或在对话框里禁掉先到的那个 —— 不需要覆盖表。

**实施决策：`syntax_lang` 保持存扩展名，不改为 grammar 名。**
原因：`apply_comments`（`dg_core::textops::comment_tokens`）、`snippets::bundled`、
statusbar 显示名、`lsp_start` 全部按扩展名键控——统一成 grammar 名会波及这四组功能，
而映射收口本身不需要动语义。因此只把**两处真实 attach 点**（`window.rs` 大文件转常规、
文件加载完成路径）的 ext→grammar match 换成 `resolve_grammar(ext)`（在可变借用 `act_mut()`
之前解析）；`21770`/`app.rs` 处的 match 是测试代码，保持原样。

## 5.1 实施状态（P1 完成）

- dg-syntax：`manifest.rs` + `dynlang.rs`（libloading，进程级 LIVED_LIBS 永不卸载）+
  registry（BTreeMap 外部表、`scan_plugins`/`get`/`resolve_ext`/`plugin_list`，默认空表）；16/16 测试。
- `fixture-json-dll` cdylib 测试夹具（导出 `tree_sitter_fixture_json` 转发
  tree_sitter_json；**导出名不得与转发的 extern 同名**，否则链接器自解析成无限递归）。
- dg-app：`EditorState` 持有 registry；`resolve_grammar()`（manifest > 内置表；原
  ini Map 一层已按 §5 修订删除）；
  两处 attach 点收口；`load_syntax_config()` 启动扫描。
- UI：菜单 文档→高级→「重新扫描解析器」(36007, `cm_plugin_rescanparsers`)，报告写输出窗格。
  id 原用 36005，与工具菜单两条 dg-script 脚本行撞车（脚本在查表前拦截，点下去跑的是
  `hello.lua`）；36005/36006 已提成 `cmdids::SCRIPT_MENU_IDS` 并加测试锁死。
- 测试：dg-syntax 27/27、dg-app 476/476（含插件端到端：扫描→LoadLibrary→attach→重扫报告）。

## 5.2 实施状态（P2 完成）

- **高亮 query**：`ThemeConfig.capture_rules`（capture 名 → 色，**最长点分前缀**匹配）
  + `Highlighter::set_query/clear_query/has_query`。`highlight_range` 新增第 5 个参数
  `node_text: &dyn Fn(usize, usize) -> Vec<u8>`，供 `#match?`/`#eq?` 等谓词取节点文本
  （tree-sitter 的 `TextProvider` 闭包实现，按需取节点字节，不需要整文档文本）。
- **层叠语义**：capture 按出现顺序上色，后出现的覆盖先出现的（Helix 语义）。`paint()`
  负责把重叠区间裁开。`QueryCursor::set_byte_range` 是**相交**语义，跨行 token
  （块注释）在它覆盖的每一行都命中，所以逐行绘制也能正确上色。
- **未 capture 的文本 = `default_color`**（普通前景）——与 Helix 一致；不再回落到 kind 配色。
  这是 v2 与 v1 的可见差异：开了 query 就完全按 query 走。
- **折叠 query**：`folding::query_spans` + `Highlighter::set_fold_query/fold_spans`。
  与 kind 启发式的关键差别是**保留嵌套**（`fold_level_at`/`fold_to_level` 正好按嵌套深度工作）；
  没有 folds.scm 时仍走 `folding_spans`。
- **接线**：`Editor::attach_syntax_with(lang, highlights, folds)`（`attach_syntax` 等价于
  两个 None）；`EditorState::load_syntax(grammar)` 一次取 `(Language, highlights, folds)`，
  两个 attach 点与「重新加载着色」(1653) 都改走它 —— 改完 .scm 点重载即生效。
- **配色来源**：`syntax_config_from_theme` 把 18 个 capture 前缀映射到同一套 kind 色板
  （`string.escape` 跟 `string`、`constant.numeric` 跟 `number`、`constructor`/`namespace`
  跟 `type`、`variable.*` 跟 `property`…），换主题时两条路径不漂。
- **诊断**：`PluginInfo.queries` 列出包内 `*.scm`；「重新扫描解析器」报告里每个包带
  `[query: highlights, folds]`。query 读取失败（非 UTF-8/权限）进诊断；编译失败不回滚语法
  （保留 kind 路径），但错误记进 `Editor.syntax_query_error`，报告里按「[文档 N 的 query]」列出。
- **自检工具**：`cargo run -p dg-syntax --example plugin_probe -- <包目录> <源文件>` 打印
  manifest→DLL→query→runs/折叠区全链路；`query_sweep` 批量试编译整个 runtime 目录。
- **实测（Helix 25.07.1 runtime）**：279 个 query 目录 / 656 个 .scm，231 个语言全通过，
  DLL 加载 0 失败，仅 3 个文件编不过 —— `solidity/highlights.scm`（`"type"` 匿名 token
  在 grammar 里不存在）、`latex/folds.scm`（`environment` 节点类型不存在）、
  `gdscript/tags.scm`（impossible pattern）。都是上游 query 与 DLL 版本漂移，我们侧正确降级。
- 测试：dg-syntax 27/27、dg-app 476/476。新增端到端 `plugin_highlights_query_paints_captures`
  （kind 色 vs capture 色、坏 query 回落、1653 重载拾取新 .scm、folds 嵌套）。

## 5.3 实施状态（语法列表 UI + INI 写回 完成）

- **`[Syntax] Disabled`**：新的 ini 键，逗号分隔的**包名**（= 包目录名，大小写敏感）。
  空值/缺省 = 全部启用。`load_syntax_config()` 在扫描**之后**才调
  `LanguageRegistry::apply_disabled()` —— 名字得先注册进来才谈得上禁用。
- **注册表的启用位**：`PluginEntry.enabled` + `set_enabled()` / `apply_disabled()` /
  `disabled_names()`；`get` / `has` / `resolve_ext` / `resolve_filename` 一律跳过禁用包。
  禁用**不卸载** DLL（§3.2），已打开的文档保留自己的高亮器。
- **`win32/syntaxdlg.rs`**：工具→设置→「语法着色」(1459) 的 `#32770` 模态框，形状照
  `encdlg`（空 `DLGTEMPLATE` + 建完再塞子控件 + `prompt::run_modal_loop`）。五列
  （语言 / 显示名 / 扩展名 / 状态 / query）+ `LVS_EX_CHECKBOXES` 勾选 + 全选/全不选 +
  顶部活计数摘要 + 底部诊断摘要。勾选只在**点确定**时收成禁用名单，取消/关窗什么都不改。
- **确定之后**：写回 `[Syntax] Disabled` → `reapply_syntax()` 按新映射重着色所有文档
  （1653「重新加载着色」抽出的同一条路径，只是不响提示音）→ 状态栏给启用/禁用计数。
- `syntax_report()`（36007）也带上启用/禁用计数与 `[已禁用]` 标记，两处说法一致。
- `manifest [theme]` 字段**删除**（见 §6）：写了报错并指向 `queries/highlights.scm`；
  示例包 `docs/parsers_example/lua` 与 README 的字段表同步改过。
- 测试：dg-syntax 29/29、dg-app 486/486。

## 5.4 实施状态（`first_lines` 首行嗅探 完成）

- **manifest 字段终于有人读了**：`first_lines` 从 P1 起就解析、校验、但没人消费；
  现在由 `LanguageRegistry::resolve_first_line(line)` 做**前缀**匹配（`starts_with`，
  不是相等），按包名字典序遍历，跳过禁用包和空串，先到先得 —— 与扩展名冲突同一套规则。
- **`window.rs` 的 `syntax_key_for()` 变成三级**：`resolve_filename(文件名)` →
  `resolve_grammar(扩展名)` → `shebang_grammar(路径)`。第三级才开文件，只读
  **256 字节**（`File::open` + `read`），用 `from_utf8_lossy` 容错，取第一行交给注册表。
  `first_line()` 顺手剥 UTF-8 BOM 和行尾 `\r` —— Windows 上脚本带 CRLF 是常态，
  不剥就永远匹配不上 `#!/usr/bin/env python3`。
- **为什么不把它提到扩展名之前**：`mod.py` 里若写着 `#!/usr/bin/env node`，按内容走
  会突然变成 JS 高亮，而用户给文件起 `.py` 就是在表态。扩展名/文件名是**声明**，
  shebang 是**兜底**，只在名字看不出时才看内容。
- **`tools/make_parsers.py` 带上了 `SHEBANGS` 表**：39 条前缀，覆盖 python / bash /
  javascript / ruby / perl / php / lua / r / elixir / julia / fish / nix / groovy /
  crystal / swift / dart / powershell，生成时写进各包 manifest 的 `first_lines`；
  `--check` 额外校验"必须 `#!` 开头"和"同一前缀不得被两个包声明"。
  当前：62 个语言、118 个扩展名、39 条前缀、0 问题。
- 测试：dg-syntax 31/31（含 `the_first_line_sniffs_the_script_interpreter`）；
  dg-app 的 `a_script_without_extension_is_sniffed_from_its_shebang`（临时包 + 四个文件：
  无扩展名 / BOM+CRLF / `mod.py` / `notes.txt`）与 `preset_parsers_dir_resolves_common_files`
  里的真 shebang 脚本断言。

## 5.5 实施状态（`max_file_bytes` 体积上限 完成）

- **最后一个只解析不消费的字段也接上了**。判定在
  `LanguageRegistry::fits_file(name, bytes)`：闭区间（`bytes <= max` 才附加）、
  缺省不设限、`register` 注册的内置项没有 manifest 也就不设限、禁用的包一律
  `false`。**不触发 DLL 加载**（只读 manifest），所以能在 attach 之前随便调。
- **三处 attach 点全部收口**：加载完成（`on_load_events` 的 `Done`）、
  大文件转普通模式（`restore_syntax`）、重新着色（`reapply_syntax`）。后两者
  现在共用 `syntax_plan() -> Vec<SyntaxSlot>`，`SyntaxSlot` 三态：
  `Attach` / `TooLarge` / `None`。多这一态是因为 `reapply_syntax` 要区分
  "手工关掉"（保留旧高亮器只重解析）和"现在超限了"（**摘掉**高亮器）——
  否则把包的上限改小、点「重新加载着色」看不出任何变化。
- **超限只影响 tree-sitter**：`syntax_lang` 不动，注释符号、snippet、状态栏
  照旧认这门语言，只是没有高亮和折叠。**并且会说出来** —— 输出窗格写一行
  `语法 json：文档 12.3 MB 超过包上限 4.0 MB，未附加 tree-sitter`。
  静默降级是 `manifest [theme]` 那次的教训。
- 与全局 `LARGE_FILE_THRESHOLD`（50 MB，走 dg-large 只读路径）的关系：那个
  阈值在更前面就分流了，所以包的 `max_file_bytes` 实际只需要管 50 MB 以下的
  文件。两者互不覆盖，也不互相推导 —— 一个说"这文件太大编辑器扛不住"，
  一个说"这门语法太慢，别接"。
- 生成器**不**给预置包写默认上限：Helix 的 manifest 里没有这个信息，猜一个
  数字等于替用户做决定。要限制就在包的 `manifest.toml` 里加一行。
- 测试：dg-syntax 32/32（`the_size_limit_comes_from_the_package`：边界 64、
  超一字节、无 manifest、禁用）；
  dg-app `a_package_can_refuse_files_over_its_size_limit`（2 字节挂上 → 涨到
  200 字节判 `TooLarge` → `reapply_syntax` 摘掉高亮器，且 `syntax_lang` 不变）。

## 6. 配色：v1 theme 表 → v2 queries

**v1（与现状同构）**：`highlight_range` 继续吃 `ThemeConfig`。合并顺序：
全局 `syntax_config_from_theme`（app.rs）→ 内置默认。子串匹配语义不变，
保证任何现有 grammar（含外部）开箱有色。

> 原计划里的第一层「`manifest [theme]`（该文档专属）」**已删除**：
> 字段解析并校验了，却从来没有消费点 —— 留着等于让用户配一份不生效的颜色。
> 现在 manifest 里出现 `[theme]` 直接报错，并指向 `queries/highlights.scm`。

**v2（已实现，见 §5.2）**：`queries/highlights.scm`。
- `Query::new(lang, bytes)` + `QueryCursor` 匹配可见范围，capture 名（`@keyword` 等）→
  主题色映射表放主题文件（`syntax_config_from_theme` 扩展一节）。
- 这是 tree-sitter 生态标准格式：用户可直接把 grammar 仓库自带的 highlights.scm 拷进 `queries/`，
  免配置获得精准配色（比子串匹配强得多）。
- `folds.scm` 同理替换 `folding_spans` 的启发式（保留启发式作为无 query 时的回退）。
- 加载失败/编译不过的 query → 记日志 + 回退 v1 路径，绝不因此丢高亮。

**未做的**：`indents.scm`（Helix 用它做缩进，我们的自动缩进仍按词法器/启发式）；
`locals.scm` / `injections.scm`（注入需要多语言树，超出本方案范围）。

## 7. 安全与信任模型

- **v1 进程内**：自定义 DLL = 本机原生代码，权限同主程序。这是 Neovim/Helix 的模型，
  对本地单用户编辑器是合理默认。缓解措施：ABI 校验、`is_parseable`、空文档探针、
  惰性加载（坏包只影响用到它的那一刻）、错误分类与日志。
  诚实说明：**探针拦不住恶意或有越界 bug 的解析器**——进程内无法隔离原生代码崩溃。
- **v3 可选 out-of-proc**：`dg-parser-host.exe` 独立进程加载 DLL，主程序经匿名管道发
  `{Init{dll}, Reset{doc_len}, Edit{InputEdit}, Feed{chunk}, QueryRange{bytes}}`，
  宿主回 `{LeafRuns{ranges+kinds}}`（配色仍在本进程算）。宿主崩溃 → 重启 + 该文档降级无高亮。
  v1 的 `Highlighter` 输出边界（leaf ranges）已为此留好缝，不必提前实现。
- **WASM 路线评估**：tree-sitter 支持 wasm 语言包（wasmtime），沙箱彻底但依赖重（+几十 MB）、
  解析慢数倍。暂不采用，格式上预留（manifest 加 `format = "dll" | "wasm"` 一字段即可）。

## 8. 用户侧：怎么拿到一个解析器 DLL（写入用户文档，P2）

0. **拷现成的 Helix 发行包**（最省事）：`helix-*-x86_64-windows\runtime\grammars\<name>.dll`
   直接拷进 `parsers\<name>\` 即可，245/245 实测可用（配方见 §2.5）；要 Helix 级配色就把它
   `queries\<name>\highlights.scm` 一并拷入。
   批量版：`python tools/make_parsers.py --helix <Helix 发行包>` 一次铺 62 个语言
   （118 个扩展名，约 77 MB），见 §8.1。
1. **tree-sitter CLI 构建**：grammar 仓库根执行
   `tree-sitter build --output <dogedit>\parsers\go\go.dll`（CLI ≥ 0.22，Windows 直接产 DLL）。
2. **MSVC 手动编译**：`cl /LD /O2 src\parser.c src\scanner.c /Fe:go.dll /link /export:tree_sitter_go`
   （无 scanner.c 则省略）。
3. **直接下载** grammar 仓库 release 的 Windows DLL（注意导出符号名与 manifest `entry` 对上）。

模板与字段说明见 `docs/parsers_example/`（lua/go 两个可直接抄的 `manifest.toml`
+ 三步装机配方 + 自检命令）。

### 8.1 预置目录（`tools/make_parsers.py`）

```
python tools/make_parsers.py --helix <Helix 发行包> [--out parsers]
                            [--only rust,lua] [--check] [--list] [--clean] [--dry-run]
```

- 语言表在脚本的 `LANGS` 里：**62 个语言 / 118 个扩展名**，每个条目是
  `(grammar 名, 显示名, 扩展名表)`。`--check` 校验 DLL 齐全性与扩展名冲突
  （registry 的 `resolve_ext` 按包名字典序取首个命中，所以同一扩展名只能属于
  一个包）。
- `SHEBANGS` 表（39 条前缀）单独维护，生成时写进各包的 `first_lines`；`--check`
  同样校验前缀唯一性。没扩展名的脚本（`bin/run` 里写着 `#!/usr/bin/env python3`）
  就靠这张表认出来。
- 每个包产出 `manifest.toml` + `<name>.dll` + `queries\{highlights,folds}.scm`；
  `Dockerfile` / `CMakeLists.txt` / `Makefile` 这类没有可用扩展名的，走
  `manifest.filenames`（大小写不敏感，优先于扩展名）。
- `SKIP_QUERIES` 记着上游编不过的 query（`latex/folds.scm`、`solidity/highlights.scm`），
  不拷进去，让那两个语言安静地走 kind 回退。
- **生成物不进版本库**（第三方二进制 + 体积，`.gitignore` 已加 `/parsers`）。
- 扫描根顺序（`scan_syntax_plugins`）：exe 旁 `parsers\` → 工作目录 `parsers\` →
  `%APPDATA%\dogedit\parsers`，按规范路径去重。所以从仓库根 `cargo run` 时仓库
  里的 `parsers\` 直接生效，便携安装时前两者是同一个目录也不会报"重名插件包"；
  装到 `Program Files` 时用户自己的包放 `%APPDATA%` 那处（原 `[Syntax]
  ExtraParserDirs` 已删，见 §5）。
- 装完体检：`cargo run --release -p dg-syntax --example query_sweep -- parsers`
  （走真注册表，逐包加载 DLL + 编译 query）；当前预置 62 包 / 67 query / 0 问题。
- 回归：`preset_parsers_dir_resolves_common_files`（dg-app）在 `parsers\` 存在时
  断言扩展名、文件名、以及"键 → grammar → Language"整条链；不存在则跳过。

## 9. 测试策略

- **fixture DLL**：workspace 加 `crates/fixture-json-dll`（`crate-type=["cdylib"]`，包装
  `tree_sitter_json::LANGUAGE`，导出 **`tree_sitter_fixture_json`**）→ 走真·LoadLibrary
  全链路。导出名**不得**与转发的 `extern` 同名，否则链接器把导出解析成自己、无限递归
  （实测栈溢出）。扫描前先 `register()` 一个同名项 → 断言外部包覆盖它（用根节点 kind 区分）。
- **坏包用例**：导出返回 null 的 DLL → `NotParseable`；缺符号 → `SymbolMissing`；
  ABI 声明区间拒绝；manifest 非法拒绝。均断言 `get()` 返回 None 且诊断入列表。
- **一致性**：把 highlight.rs 现有增量/fold 测试的 lang 换成 DLL 加载来源跑一遍
  （证明"来源不同、行为一致"）。
- **resolve 优先级**表驱动单测（ini 覆盖 > manifest > 兜底扩展名表）。

## 10. 实施计划

**P1 核心（建议一次 PR 串）**
1. `dg-syntax`：`manifest.rs`（解析+校验）、`dynlang.rs`（§3）、registry 两级改造、单测。
2. `fixture-json-dll` 测试 crate。
3. `dg-app`：启动 `scan_plugins` 接线；`resolve()` 收口 5 处 match；`syntax_lang` 语义统一
   （grammar 名）；`[Syntax]` INI 段读写。
4. 最小 UI：语法列表菜单项（内置/外部 + 状态）+「重新扫描解析器」；失败诊断进输出窗口。

**P2 体验（已完成，见 §5.2）**
5. ✅ `queries/highlights.scm` + `folds.scm` 支持（§6 v2）——拷 Helix queries 配色时的依赖。
6. ✅ 用户文档（§8 获取方式，"拷 Helix 发行包 DLL"列为最快路径，附 §2.5 配方）+
   示例包一个（建议 go 或 lua，生态 DLL 好拿）。

**P3 可选**
7. out-of-proc 隔离宿主（§7 v3）；wasm 格式；解析器包管理（在线列表/一键安装，另立方案）。

## 11. 开放问题

- 解析器目录是否要同时支持 `%APPDATA%` 用户级目录（与便携式 INI 语义冲突时谁赢）——建议 v1 只用 exe 旁 `parsers\`，与现有 `history.ini` 同级约定一致。
- `snippets::bundled` 的键从扩展名切到 grammar 名是否影响用户已有 snippet 文件命名——迁移时对齐。
- 大文件（LargeDoc）路径是否给插件包开 `max_file_bytes` 之外的旁路（v1 不开，维持现状）。

## 12. 剩余工作（2026-10-26 盘点）

⬜ 未做 · ◐ 部分 · ✅ 已做。排序按"用户能感知的价值"。

### 12.1 计划里列了、确实没做

| # | 项 | 出处 | 现状 |
|---|---|---|---|
| 1 | **语法列表 UI**：列出已装语言（显示名/扩展名/query 状态）、启用禁用 | §10 P1.4 | ✅ `win32/syntaxdlg.rs`，1459 有 `Command` 了（见 §5.3） |
| 2 | `[Syntax]` INI 段**写**回（UI 改完存盘） | §10 P1.3 | ✅ 只留 `Disabled`（用户状态，对话框写回）；`Map` / `ExtraParserDirs` 已**删除**而不是补 UI，见 §5 |
| 3 | manifest `[theme]` 覆盖表 | §2、§6 v1 | ✅ 决定**删字段**而不是实现（见 §6）：写了直接报错并指向 `queries/highlights.scm` |
| 4 | `first_lines`（shebang 嗅探） | §2 | ✅ `resolve_first_line` 前缀匹配 + `syntax_key_for` 第三级，见 §5.4 |
| 5 | `max_file_bytes`（每包阈值） | §2 | ✅ `fits_file` + 三处 attach 点收口，见 §5.5 |
| 6 | `indents.scm`（自动缩进） | §6 | ⬜ 缩进仍按词法器/启发式 |
| 7 | `locals.scm` / `injections.scm` | §6 | ⬜ 注入要多种语言树，明确超出本方案 |
| 8 | out-of-proc 解析宿主 `dg-parser-host.exe` | §7 v3 | ⬜ 解析器崩溃仍会带走编辑器（信任模型同 Neovim） |
| 9 | wasm 解析器格式 | §7 | ⬜ manifest 预留 `format` 字段即可，未动 |
| 10 | 解析器包管理（在线列表/一键安装） | §10 P3 | ⬜ 另立方案 |
| 11 | **发布包怎么带 `parsers\`** | §8.1 | ⬜ 只有本地生成脚本，没有 release 打包步骤；`parsers/` 不进版本库 |
| 12 | 跨平台 DLL 后缀 | §2 | ⬜ `manifest.dll` 缺省写死 `{name}.dll`；macOS/Linux 要 `.dylib`/`.so`（libloading 本身支持，只是缺省名不对） |

### 12.2 测试欠账（§9）

| 项 | 现状 |
|---|---|
| 坏包：null 返回 → `NotParseable`、运行时 ABI 越界 → `Abi{got}`、`SetLanguage` 失败 | ⬜ 只有 `SymbolMissing` 有测试，其余错误变体没有夹具 |
| "来源不同、行为一致"：highlight 测试的 lang 换成 DLL 加载来源 | ⬜ 仍直接用 dev-dependency 的 grammar |
| resolve 优先级表驱动单测（ini > manifest > 兜底表） | ◐ registry 层覆盖 manifest > 兜底；ini 覆盖只在 dg-app 一个端到端测试里 |

### 12.3 本次盘点顺手修掉的两个 bug

1. **命令 id 撞车**：`cm_plugin_rescanparsers` 用了 36005，而两条 dg-script 脚本行
   （`Lua: Hello` / `Lua: 去除行尾空白`）也是 36005/36006，且脚本在查静态表**之前**拦截 ——
   点「重新扫描解析器」实际跑的是 `hello.lua`。已改到 **36007**，脚本 id 提成
   `cmdids::SCRIPT_MENU_IDS` 常量，并加测试锁死"静态表不得占用这两个 id"。
2. **重扫刷屏**：`scan_plugins` 对已注册的同名包一律报"重名插件包"，按一次
   「重新扫描解析器」冒出 62 条假诊断。现在同一目录重扫静默跳过，只有**另一个目录**的同名包
   才报冲突；报告尾部补一句"已加载的 DLL 不会重载，换 DLL 需重启"（§3.2 的 UI 提示）。

### 12.5 本轮（语法列表 UI）踩出来的 Win32 细节

1. **`LVM_INSERTITEM` 不读 `state` / `stateMask`**（文档原话 "not used by this message"）。
   勾选状态必须随后单独 `LVM_SETITEMSTATE`，否则插进去的行一律是未选 —— 界面上看着
   "每行都有勾选框"，其实一个都没勾。`syntaxdlg::fill_list` 现在插完立刻
   `set_row_state()`。
2. **`NMLISTVIEW.uChanged` 装的是 `LVIF_*`，不是 `LVIS_*`**。拿
   `LVIS_STATEIMAGEMASK`（0xF000）去与它永远是 0，`LVN_ITEMCHANGED` 的勾选分支
   永远不执行 —— 计数器和摘要不会跟着变。正确写法是 `u_changed & LVIF_STATE`，
   勾选与否再从 `uOldState` / `uNewState` 的 state image 位读。
   **`markdlg.rs` 里是同一个错**（`n.u_changed & LVIS_STATEIMAGEMASK.0 != 0`），
   加上 `rebuild_list` 也用 `LVM_INSERTITEM` 的 `state` 重建（同第 1 条），
   所以「自定义标记」对话框的勾选框当时是纯装饰：点了不进模型，打开也全是未选。
   两处已一起修（`check_from_notify` + `set_row_state`），探针验证
   `row0 state: 2000 → 1000 → 2000`。
3. **自定义绘制管不到列表的空白区**。`NM_CUSTOMDRAW` 只覆盖控件交出来的行，
   最后一行以下、勾选框左边那条空白由控件自己用 `COLOR_WINDOW` 刷 —— 深色
   主题上就是一块白。必须 `LVM_SETBKCOLOR` + `LVM_SETTEXTBKCOLOR` +
   `LVM_SETTEXTCOLOR`（`shortcutdlg` / `tooldlg` 早就是这么写的）。
4. **列表的表头不是列表的兄弟，是它的子窗口**。`SysListView32` 会顺手建一个
   `SysHeader32`，表头的 `NM_CUSTOMDRAW` 只到**列表**，到不了对话框 ——
   `idFrom` 还是 0，按 id 也认不出来。要深色表头只有一条路：`SetWindowSubclass`
   列表，在子类过程里认出 `hdr.hwndFrom == LVM_GETHEADER` 的那条 `NM_CUSTOMDRAW`，
   自己刷底色 + `DrawTextW` 画标题（`CDDS_PREPAINT` 刷整条、`CDDS_ITEMPREPAINT`
   逐列刷并返回 `CDRF_SKIPDEFAULT`）。`chrome::dark_theme_children` 对这一条
   无效，它把 `SysListView32` 命名成 `DarkMode_Explorer`，表头依旧白。
5. **对话框状态不能在 `WM_DESTROY` 里释放**。子控件先销毁，它们最后发的
   `WM_NOTIFY` / `WM_COMMAND` 还会进 `dlg_proc` 读 `GWLP_USERDATA`；在
   `WM_DESTROY` 里 `Box::from_raw` + 置 0，那条迟到通知就是空指针解引用 ——
   新版 rustc 的 debug 断言直接把进程 abort（"null pointer dereference
   occurred" / "thread caused non-unwinding panic. aborting."），**关一次
   自定义标记对话框就把编辑器带走了**。正确做法（`shortcutdlg` 已注释过）：
   `WM_DESTROY` 只清 `DLG`，`WM_NCDESTROY` 再释放 `Box`。`markdlg.rs` 已改。
   同一个写法还在 `grepdlg.rs` / `minifind.rs` / `searchdlg.rs` / `hexview.rs`
   里，尚未验证是否也会收到迟到通知。

### 12.4 已经作废的计划项

- "`syntax_lang` 统一成 grammar 名"（§10 P1.3）：被 §5 决策取代，保持扩展名语义。
- "registry 两级 Builtin + External"（§10 P1.1）：被 §4 决策取代，二进制不再内置 grammar。
- "`snippets::bundled` 键从扩展名切到 grammar 名"（§11）：因上一条作废。

## 附：API 核对结果（tree-sitter 0.27 vendored 源码）

- `tree_sitter::LANGUAGE_VERSION` / `MIN_COMPATIBLE_LANGUAGE_VERSION`：常量存在（lib.rs-59）。
- `Language::abi_version()`：存在（lib.rs，`ts_language_abi_version`）。
- `Language::is_parseable()`：存在；`Parser::set_language` 内部即做区间 + parseable 校验（lib.rs）。
- `tree_sitter_language::LanguageFn::from_raw(unsafe extern "C" fn() -> *const ())`：存在（0.1.7）。
- `TSLanguage` 布局：`abi_version` 为偏移 0 的 `u32`（`src/parser.h`），探针据此直读；
  `ts_language_is_parseable` = `self && self->lex_fn`（`src/language.c:85`）。
- Helix 25.07.1 grammar DLL 实测：245/245 可加载，ABI ∈ {13, 14, 15}，见 §2.5。
