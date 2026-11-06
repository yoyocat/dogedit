# 示例解析器包

复制到 `dogedit.exe` 同级的 `parsers\` 下即可用；目录名必须与 `manifest.toml`
的 `name` 完全一致。

**二进制里不带任何内置语法** —— `parsers\` 是 grammar 的唯一来源。一个包都不装
也能正常编辑，只是没有着色和折叠（扩展名、注释符、状态栏这些不依赖 grammar 的
功能照旧）。

```
<dogedit>\parsers\
    lua\
        manifest.toml
        lua.dll
        queries\
            highlights.scm
            folds.scm
    go\
        manifest.toml
        go.dll
        queries\
            highlights.scm
```

## 一次装一批（推荐）

仓库带一个生成器，直接从 Helix 发行包铺满 `parsers\`：

```
python tools/make_parsers.py --helix <Helix 发行包>
python tools/make_parsers.py --helix <...> --check     # 只体检语言表
python tools/make_parsers.py --helix <...> --list      # 只列语言与扩展名
python tools/make_parsers.py --helix <...> --only rust,lua,go
```

默认铺 62 个语言（118 个扩展名，约 77 MB），覆盖：

- **编程**：Rust、C/C++、C#、Java、Python、Go、Ruby、PHP、Perl、Lua、Kotlin、
  Swift、Scala、Haskell、OCaml、Elixir、Erlang、Clojure、Dart、Zig、Nim、
  Julia、R、Fortran、Pascal、Groovy、Elm、Crystal、GDScript、Nix、
  HCL/Terraform、Solidity
- **脚本**：Bash（含 zsh）、Fish、PowerShell、Make、CMake、Dockerfile
- **Web**：HTML、CSS、SCSS、JavaScript（含 JSX）、TypeScript、TSX、Vue、
  Svelte、Astro、GraphQL
- **数据/文档**：JSON、YAML、TOML、XML、INI、CSV、SQL、Markdown、LaTeX、
  Typst、Diff、WebAssembly、汇编（NASM）

生成物不进版本库（第三方二进制 + 体积），随时用脚本重建。

**程序去哪找 `parsers\`**：固定三处 —— exe 所在目录、当前工作目录、
`%APPDATA%\dogedit\parsers`（装到 `Program Files` 时唯一可写的落点）。
所以从仓库根 `cargo run` 时，仓库根的 `parsers\` 直接生效；便携安装时前两者
是同一个目录（会去重，不会报"重名插件包"）。**没有配置项可以改这个顺序**：
原来的 `[Syntax] ExtraParserDirs` 已删除。

**扩展名归哪个语法**只由包自己的 `manifest.toml` 决定（`extensions` /
`filenames`）；`setting.ini` 里只剩 `[Syntax] Disabled` 这一项语法配置（用户在
「语法着色」对话框里勾出来的）。原来那层 `[Syntax] Map` 手工覆盖已删除 ——
想让 `.rlib` 也算 rust，就把 `rlib` 加进 `parsers\rust\manifest.toml` 的
`extensions`。

直接双击 `target\debug\dogedit.exe` 时工作目录也是 `target\debug`，两个候选
都指向那里 —— 开发时可以在 `target\debug` 下建一个指向仓库 `parsers\` 的
目录联接（`cargo clean` 会连它一起删掉）：

```
mklink /J target\debug\parsers parsers
```

## 三步装一个包（以 lua 为例）

1. **拷 DLL**：从 Helix 发行包 `helix-*-x86_64-windows\runtime\grammars\lua.dll`
   拷成 `parsers\lua\lua.dll`。Helix 的 245 个 grammar DLL 都是标准 tree-sitter
   语言库，导出名 `tree_sitter_<name>`（`-` 换成 `_`），直接可用。
2. **拷 query**（可选，但强烈建议）：把 `runtime\queries\lua\highlights.scm`
   和 `folds.scm` 拷进 `parsers\lua\queries\`。有 query 就走 capture 配色，
   没有就退回按 node kind 子串猜的默认配色。
3. **写 manifest**：见本目录 `lua\manifest.toml`，`extensions` 填要让这个语法
   接管的文件扩展名；`Dockerfile` / `CMakeLists.txt` / `Makefile` 这种没有
   可用扩展名的，用 `filenames` 精确匹配（大小写不敏感，优先于扩展名）。
   无扩展名的脚本还可以用 `first_lines` 按首行前缀认，例如
   `first_lines = ["#!/usr/bin/env lua"]`，打开 `bin/run` 就会按 lua 着色
   （剥掉 BOM 和 CRLF 后做前缀匹配；扩展名/文件名认得出时不看内容）。

装好后菜单 **文档 → 高级 → 重新扫描解析器**（`cm_plugin_rescanparsers`），
输出窗格会列出识别到的包和它们的 query 文件；有问题的包会带诊断原因。

## 字段说明

| 字段 | 必需 | 缺省 | 说明 |
| --- | --- | --- | --- |
| `name` | 是 | — | 注册键，只允许 `a-z0-9_-`，**必须等于包目录名** |
| `display` | 否 | `name` | 状态栏/列表显示名 |
| `version` | 否 | `0.0.0` | 仅作文档 |
| `entry` | 否 | `tree_sitter_{name}` | DLL 导出符号（`-` 映射为 `_`） |
| `dll` | 否 | `{name}.dll` | 包目录内的 DLL 文件名 |
| `extensions` | 否 | `[]` | 自动接管的扩展名（小写、无点） |
| `filenames` | 否 | `[]` | 精确文件名（大小写不敏感），优先于扩展名 |
| `abi` | 否 | 不校验 | 声明的兼容 ABI 区间，如 `[13, 15]` |
| `first_lines` | 否 | `[]` | 首行前缀（如 `#!/usr/bin/env python3`），名字看不出类型时才用 |
| `max_file_bytes` | 否 | 不设限 | 超过这个字节数的文档不附加本语法（只影响高亮/折叠） |

配色不写在 manifest 里：`queries/highlights.scm` 才是唯一的配色来源
（capture 名对应的颜色在主题文件里）。旧的 `[theme]` 段已废弃，写了会直接报错
并指向 `queries/highlights.scm`，不会静默忽略。

`max_file_bytes` 是给"这门语法在大文件上太慢"用的：超限的文件照常打开、照常
编辑，只是不挂 tree-sitter（没有高亮和折叠），输出窗格会写一行说明。不写就是
不设限；真正的"文件太大编辑器扛不住"是另一条路（50 MB 以上走大文件只读模式）。

## 自检

不想开主程序时，用仓库里的探针直接验证一个包：

```
cargo run -p dg-syntax --example plugin_probe -- docs/parsers_example/lua src/main.lua
```

它会打印 manifest 校验、DLL 加载（含 ABI 版本）、每个 query 的编译结果、
折叠区、以及逐行的高亮 runs（`#RRGGBB`）。

整批体检（两种目录自动识别）：

```
cargo run --release -p dg-syntax --example query_sweep -- parsers          # 插件包目录
cargo run --release -p dg-syntax --example query_sweep -- <Helix>\runtime   # Helix runtime
```

前者走真正的 `LanguageRegistry`，逐包加载 DLL 并编译 query，装完一批包后
想知道"哪些有问题"就用它。

## 已知的上游不一致

Helix 25.07.1 的 runtime 里，656 个 `.scm` 有 3 个编译不过（都是 query 与
DLL 版本漂移，不是加载器的问题）：

- `solidity/highlights.scm` —— 用了 grammar 里不存在的匿名 token `"type"`
- `latex/folds.scm` —— 用了不存在的节点类型 `environment`
- `gdscript/tags.scm` —— impossible pattern

生成器把前两个列在 `SKIP_QUERIES` 里不拷（让这两个语言安静地走 kind 回退，
而不是每次附加语法都记一条诊断）；`tags.scm` 本项目不消费，无影响。
遇到其它同类文件，高亮/折叠会静默退回按 kind 猜测的路径，不会让文档失去
语法支持。
