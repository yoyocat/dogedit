#!/usr/bin/env python3
"""从 Helix 发行包生成 parsers\\ 解析器插件目录（parser_plugin_plan.md §2）。

用法：
    python tools/make_parsers.py --helix E:\\helix-25.07.1-x86_64-windows
    python tools/make_parsers.py --helix <runtime 父目录> --list      # 只列语言表
    python tools/make_parsers.py --helix <...> --check                # 只检查可用性
    python tools/make_parsers.py --helix <...> --only rust,lua,go     # 只装几个
    python tools/make_parsers.py --helix <...> --clean                # 清掉表外旧包

每个包是 `parsers\\<name>\\`：

    manifest.toml       name/display/extensions（entry 与 dll 走缺省值）
    <name>.dll          从 runtime\\grammars\\<name>.dll 拷来
    queries\\*.scm      从 runtime\\queries\\<name>\\ 拷全部 .scm（highlights/folds
                       之外还有 indents/locals/injections/tags/textobjects），
                       再盖上本仓库 queries\\<name>\\ 的覆盖层（见 make_queries.py）

Helix 的 grammar DLL 是标准 tree-sitter 语言库（导出 `tree_sitter_<name>`，
`-` 映射为 `_`），与本项目的加载器完全兼容 —— 245/245 都能加载。

生成的目录不进版本库（第三方二进制 + 体积），用本脚本随时重建。
"""

import argparse
import shutil
import sys
from pathlib import Path

# (grammar 名, 显示名, 扩展名表)
#
# grammar 名必须等于 Helix `runtime\grammars\<name>.dll` 的文件名主干；
# 扩展名一律小写、不带点（主程序按 `Path::extension().to_ascii_lowercase()`
# 取值）。同一扩展名只能出现在一个包里 —— `--check` 会报冲突，
# 因为 registry 的 `resolve_ext` 按包名字典序取第一个命中。
LANGS = [
    # ---- 系统/通用编程语言 ----
    ("rust", "Rust", ["rs"]),
    ("c", "C", ["c", "h"]),
    # `.cpp` 一族照收；带符号的 `.c++`/`.h++` 也是正经后缀 —— 查询参数
    # 有百分号编码，服务端能原样匹配到。
    ("cpp", "C++", ["cpp", "cc", "cxx", "hpp", "hh", "hxx", "c++", "h++"]),
    ("c-sharp", "C#", ["cs"]),
    ("java", "Java", ["java"]),
    ("python", "Python", ["py", "pyi"]),
    ("go", "Go", ["go"]),
    ("ruby", "Ruby", ["rb", "rake", "gemspec"]),
    ("php", "PHP", ["php"]),
    ("perl", "Perl", ["pl", "pm"]),
    ("lua", "Lua", ["lua"]),
    ("kotlin", "Kotlin", ["kt", "kts"]),
    ("swift", "Swift", ["swift"]),
    ("scala", "Scala", ["scala", "sbt", "sc"]),
    ("haskell", "Haskell", ["hs"]),
    ("ocaml", "OCaml", ["ml", "mli"]),
    ("elixir", "Elixir", ["ex", "exs"]),
    ("erlang", "Erlang", ["erl", "hrl"]),
    ("clojure", "Clojure", ["clj", "cljs", "cljc", "edn"]),
    ("dart", "Dart", ["dart"]),
    ("zig", "Zig", ["zig"]),
    ("nim", "Nim", ["nim"]),
    ("julia", "Julia", ["jl"]),
    ("r", "R", ["r"]),
    ("fortran", "Fortran", ["f90", "f95", "f03", "f08", "f", "for"]),
    ("pascal", "Pascal", ["pas", "pp"]),
    ("groovy", "Groovy", ["groovy", "gradle"]),
    ("elm", "Elm", ["elm"]),
    ("crystal", "Crystal", ["cr"]),
    ("gdscript", "GDScript", ["gd"]),
    ("nix", "Nix", ["nix"]),
    ("hcl", "HCL / Terraform", ["hcl", "tf", "tfvars"]),
    ("solidity", "Solidity", ["sol"]),
    # ---- 脚本/命令行 ----
    # zsh 没有独立 grammar，Helix 的 bash 语法能吃掉大部分 zsh 文件。
    ("bash", "Bash", ["sh", "bash", "zsh"]),
    ("fish", "Fish", ["fish"]),
    ("powershell", "PowerShell", ["ps1", "psm1", "psd1"]),
    ("make", "Make", ["mk", "mak"]),
    ("cmake", "CMake", ["cmake"]),
    ("dockerfile", "Dockerfile", ["dockerfile"]),
    # ---- Web ----
    ("html", "HTML", ["html", "htm"]),
    ("css", "CSS", ["css"]),
    ("scss", "SCSS", ["scss"]),
    # tree-sitter-javascript 原生解析 JSX，所以 .jsx 也归它。
    ("javascript", "JavaScript", ["js", "mjs", "cjs", "jsx"]),
    ("typescript", "TypeScript", ["ts", "mts", "cts"]),
    ("tsx", "TSX", ["tsx"]),
    ("vue", "Vue", ["vue"]),
    ("svelte", "Svelte", ["svelte"]),
    ("astro", "Astro", ["astro"]),
    ("graphql", "GraphQL", ["graphql", "gql"]),
    # Helix 没有通用 GNU as 语法，nasm 覆盖 .asm/.s 的常见写法。
    ("nasm", "汇编 (NASM)", ["asm", "nasm", "s"]),
    # ---- 数据/配置/文档 ----
    ("json", "JSON", ["json"]),
    ("yaml", "YAML", ["yaml", "yml"]),
    ("toml", "TOML", ["toml"]),
    ("xml", "XML", ["xml", "xsl", "xslt", "svg", "plist"]),
    ("ini", "INI", ["ini", "cfg", "conf"]),
    ("csv", "CSV", ["csv"]),
    ("sql", "SQL", ["sql"]),
    ("markdown", "Markdown", ["md", "markdown"]),
    ("latex", "LaTeX", ["tex", "sty", "cls"]),
    ("typst", "Typst", ["typ"]),
    ("diff", "Diff", ["diff", "patch"]),
    ("wat", "WebAssembly", ["wat", "wast"]),
]

# 拷哪些 query：highlights 决定配色，folds 决定折叠区，这两个加载器会读；
# indents/locals/injections/tags/textobjects 现在没人消费，但都是 KB 级文本，
# 随包带走 —— 以后做缩进/注入/符号列表不用重新发包。加载器按文件名只取
# highlights/folds，多余文件安静躺着。
QUERY_FILES = [
    "highlights.scm",
    "folds.scm",
    "indents.scm",
    "injections.scm",
    "locals.scm",
    "textobjects.scm",
    "tags.scm",
]

# 上游 query 与 DLL 版本漂移、编不过的文件 —— 不拷进包，让那个语言
# 安静地走 kind 回退，而不是每次附加语法都记一条诊断。
# 用 `cargo run --release -p dg-syntax --example query_sweep -- parsers`
# 重新核对（升级 Helix 后先跑一遍，这里的名单可能已经过时）。
SKIP_QUERIES = {
    ("latex", "folds.scm"),      # Invalid node type "environment"
    ("solidity", "highlights.scm"),  # 用了 grammar 里不存在的匿名 token "type"
}

# 没有可用扩展名的常见文件：manifest.filenames 精确匹配（大小写不敏感），
# 命中时 syntax_lang 直接记 grammar 名。
FILENAMES = {
    "dockerfile": ["Dockerfile", "Containerfile"],
    "cmake": ["CMakeLists.txt"],
    "make": ["Makefile", "makefile", "GNUmakefile"],
}

# 首行前缀嗅探（manifest.first_lines）：给 `runme` 这种没有扩展名的脚本用。
# 前缀匹配、大小写敏感 —— 就是 shebang 原样的写法。`#!/usr/bin/env python`
# 顺手覆盖 python3 / python3.12。只列真会出现在 shebang 里的解释器。
SHEBANGS = {
    "python": ["#!/usr/bin/env python", "#!/usr/bin/python", "#!/usr/local/bin/python"],
    # zsh/dash 都归 bash 包（Helix 没有独立 grammar），shebang 也一起收。
    "bash": [
        "#!/bin/sh",
        "#!/bin/bash",
        "#!/bin/zsh",
        "#!/usr/bin/env bash",
        "#!/usr/bin/env sh",
        "#!/usr/bin/env zsh",
    ],
    "javascript": ["#!/usr/bin/env node", "#!/usr/bin/node"],
    "ruby": ["#!/usr/bin/env ruby", "#!/usr/bin/ruby"],
    "perl": ["#!/usr/bin/env perl", "#!/usr/bin/perl"],
    "php": ["#!/usr/bin/env php", "#!/usr/bin/php"],
    "lua": ["#!/usr/bin/env lua", "#!/usr/bin/lua"],
    "r": ["#!/usr/bin/env Rscript", "#!/usr/bin/Rscript"],
    "elixir": ["#!/usr/bin/env elixir", "#!/usr/bin/elixir"],
    "julia": ["#!/usr/bin/env julia", "#!/usr/bin/julia"],
    "fish": ["#!/usr/bin/env fish", "#!/usr/bin/fish"],
    "nix": ["#!/usr/bin/env nix-shell", "#!/usr/bin/nix-shell"],
    "groovy": ["#!/usr/bin/env groovy", "#!/usr/bin/groovy"],
    "crystal": ["#!/usr/bin/env crystal", "#!/usr/bin/crystal"],
    "swift": ["#!/usr/bin/env swift", "#!/usr/bin/swift"],
    "dart": ["#!/usr/bin/env dart", "#!/usr/bin/dart"],
    "powershell": ["#!/usr/bin/env pwsh", "#!/usr/bin/pwsh"],
}

# 本仓库自带的折叠查询覆盖层（`queries\<name>\*.scm`）：上游没给、或者给得
# 不够的语言由它补。生成脚本是 tools/make_queries.py，这里只负责盖上去。
OVERLAY = Path(__file__).resolve().parent.parent / "queries"

MANIFEST = """\
# 由 tools/make_parsers.py 从 Helix {version} 生成，请勿手改 ——
# 重新生成会覆盖。要定制就改脚本里的 LANGS 表。
name = "{name}"
display = "{display}"
version = "{version}"
extensions = [{exts}]
{filenames}{first_lines}abi = [13, 15]
"""


def runtime_dirs(helix: Path):
    """接受 `<发行包>` 或 `<发行包>/runtime`，返回 (grammars, queries)。"""
    for base in (helix / "runtime", helix):
        if (base / "grammars").is_dir() and (base / "queries").is_dir():
            return base / "grammars", base / "queries"
    sys.exit(f"找不到 grammars/ 与 queries/：{helix}")


def check(grammars: Path, queries: Path) -> int:
    """检查语言表：DLL 是否齐全、扩展名是否冲突。返回问题条数。"""
    problems = 0
    seen = {}
    for name, _display, exts in LANGS:
        dll = grammars / f"{name}.dll"
        if not dll.exists():
            print(f"  缺 DLL: {name}")
            problems += 1
        if not (queries / name / "highlights.scm").exists():
            print(f"  无 highlights.scm: {name}（会退回按 node kind 配色）")
        for e in exts:
            if e in seen:
                print(f"  扩展名冲突: .{e} 同时属于 {seen[e]} 和 {name}")
                problems += 1
            else:
                seen[e] = name
    for name in FILENAMES:
        if name not in {t[0] for t in LANGS}:
            print(f"  FILENAMES 里的 {name} 不在语言表中")
            problems += 1
    # shebang 前缀也是「先到先得」：同一个前缀挂在两个包上，命中哪个就看包名了。
    seen_sheb = {}
    for name, prefixes in SHEBANGS.items():
        if name not in {t[0] for t in LANGS}:
            print(f"  SHEBANGS 里的 {name} 不在语言表中")
            problems += 1
            continue
        for p in prefixes:
            if not p.startswith("#!"):
                print(f"  {name} 的 first_lines 条目不以 #! 开头: {p}")
                problems += 1
            if p in seen_sheb:
                print(f"  shebang 冲突: {p} 同时属于 {seen_sheb[p]} 和 {name}")
                problems += 1
            else:
                seen_sheb[p] = name
    print(
        f"语言 {len(LANGS)} 个，扩展名 {len(seen)} 个，shebang 前缀 {len(seen_sheb)} 个，"
        f"问题 {problems} 条"
    )
    return problems


def build(grammars: Path, queries: Path, out: Path, version: str,
          only: set[str] | None, clean: bool, dry: bool) -> None:
    out.mkdir(parents=True, exist_ok=True)
    wanted = [t for t in LANGS if only is None or t[0] in only]
    if only:
        unknown = only - {t[0] for t in LANGS}
        if unknown:
            sys.exit(f"--only 里有表外语言: {', '.join(sorted(unknown))}")

    total = 0
    copied_q = 0
    overlaid = 0
    for name, display, exts in wanted:
        src = grammars / f"{name}.dll"
        if not src.exists():
            print(f"  跳过 {name}: 没有 DLL")
            continue
        dst = out / name
        if dry:
            print(f"  [dry] {name}  <- {src.name}  {exts}")
            continue
        dst.mkdir(exist_ok=True)
        shutil.copy2(src, dst / f"{name}.dll")
        total += src.stat().st_size
        names = FILENAMES.get(name, [])
        shebangs = SHEBANGS.get(name, [])
        (dst / "manifest.toml").write_text(
            MANIFEST.format(
                version=version,
                name=name,
                display=display,
                exts=", ".join(f'"{e}"' for e in exts),
                filenames=(
                    "filenames = [{}]\n".format(", ".join(f'"{f}"' for f in names))
                    if names
                    else ""
                ),
                first_lines=(
                    "first_lines = [{}]\n".format(
                        ", ".join(f'"{s}"' for s in shebangs)
                    )
                    if shebangs
                    else ""
                ),
            ),
            encoding="utf-8",
        )
        qdst = dst / "queries"
        if qdst.exists():
            shutil.rmtree(qdst)
        src_q = queries / name
        for f in QUERY_FILES:
            if (name, f) in SKIP_QUERIES:
                continue
            if (src_q / f).exists():
                qdst.mkdir(exist_ok=True)
                shutil.copy2(src_q / f, qdst / f)
                copied_q += 1
        # 覆盖层最后写：同名文件以它为准。
        src_o = OVERLAY / name
        if src_o.is_dir():
            for f in sorted(src_o.glob("*.scm")):
                qdst.mkdir(exist_ok=True)
                shutil.copy2(f, qdst / f.name)
                overlaid += 1

    if clean and not dry:
        keep = {t[0] for t in wanted}
        for d in sorted(out.iterdir()):
            if d.is_dir() and d.name not in keep:
                print(f"  清掉表外包: {d.name}")
                shutil.rmtree(d)

    if not dry:
        print(f"完成：{len(wanted)} 个包，DLL {total / 1048576:.1f} MB，query 文件 {copied_q} 个")
        if overlaid:
            print(f"覆盖层 queries\\ 盖上去 {overlaid} 份")
        print(f"输出目录：{out}")


def main() -> None:
    ap = argparse.ArgumentParser(description="从 Helix runtime 生成 parsers\\ 插件包")
    ap.add_argument("--helix", required=True, help="Helix 发行包目录（含 runtime\\）")
    ap.add_argument("--out", default="parsers", help="输出目录（默认 ./parsers）")
    ap.add_argument("--only", default="", help="只装这些 grammar，逗号分隔")
    ap.add_argument("--clean", action="store_true", help="删掉输出目录里表外的旧包")
    ap.add_argument("--check", action="store_true", help="只检查语言表，不写文件")
    ap.add_argument("--list", action="store_true", help="只打印语言表")
    ap.add_argument("--dry-run", action="store_true", help="只打印将要做什么")
    a = ap.parse_args()

    if a.list:
        for name, display, exts in LANGS:
            print(f"{name:<12} {display:<20} {', '.join(exts)}")
        print(f"\n共 {len(LANGS)} 个语言")
        return

    grammars, queries = runtime_dirs(Path(a.helix))
    if a.check:
        sys.exit(1 if check(grammars, queries) else 0)
    # 版本号取自 Helix 目录名（helix-25.07.1-x86_64-windows → 25.07.1）
    version = Path(a.helix).resolve().name
    version = version.removeprefix("helix-").split("-")[0] or "unknown"
    only = {s.strip() for s in a.only.split(",") if s.strip()} or None
    build(grammars, queries, Path(a.out), version, only, a.clean, a.dry_run)


if __name__ == "__main__":
    main()
