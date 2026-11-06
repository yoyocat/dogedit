#!/usr/bin/env python3
"""生成 queries\\ —— 盖在解析器包上的折叠查询。

语言包里的 `queries\\folds.scm` 来自 Helix，而 Helix 只给 6 门语言带了折叠
查询；其余语言走 `dg_syntax::folding::folding_spans` 的 kind 启发式。这个脚本
把 nvim-treesitter 的 folds.scm 转成一层覆盖：

    python tools/make_queries.py --nvim E:\\dogedit\\analysis\\nvim-ts

- `; inherits:` 在这里展开成拼接。tree-sitter 本身不做继承，我们的加载器一门
  语言只编译一个文件；不展开的话，`; inherits:` 独占一行的语言（javascript、
  tsx、scss、php…）会编出一份零 pattern 的查询 —— 折叠区直接变空，比启发式还差。
- `DROP` 表里的节点名在我们钉的 grammar 里不存在（上游 grammar 版本不同），
  删掉那一条；整份被删空的语言干脆不写，让它继续走启发式。
- 只处理 `parsers\\` 里真有的语言 —— 没有 grammar 就没有查询可挂。

输出进版本库；`parsers\\` 不进（那是 `make_parsers.py` 从 Helix 重建的，重建时
会把这层覆盖盖上去）。写完复核一遍：

    cargo run -p dg-syntax --example query_sweep -- parsers
"""

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

HEADER = """\
; 由 tools/make_queries.py 生成，请勿手改 —— 重新生成会覆盖。
; 上游 nvim-treesitter {commit} 的 runtime/queries/{lang}/folds.scm（Apache-2.0，见 queries/LICENSE）。
{parents}"""

# 上游 grammar 里叫一个名字、我们钉的 Helix grammar 里叫另一个名字的节点：
# 换名，语义不变。
RENAME = {
    "erlang": {"fun_decl": "function", "anonymous_fun": "anonymous_function"},
    "javascript": {"function_expression": "function"},
    "typescript": {"function_expression": "function"},
    "tsx": {"function_expression": "function"},
    "svelte": {"else_if_block": "else_if_statement"},
    "xml": {"doctypedecl": "doctype_decl", "Comment": "comment"},
    "fortran": {"do_loop": "do_loop_statement"},
}

# 我们这边根本没有对应节点的：删掉这一条，其余照用；整份删空的语言不写，
# 让它继续走 kind 启发式。查法：cargo run -p dg-syntax --example query_sweep
# -- parsers，报 Invalid node type 的就是它（一次只报一个，删完要再跑）。
DROP = {
    "c": ["gnu_asm_expression"],
    "cmake": ["block_def"],
    "diff": ["hunks", "hunk", "block"],
    "erlang": ["case_expr", "maybe_expr", "map_expr", "export_attribute", "export_type_attribute"],
    "svelte": ["else_block", "then_block", "catch_block", "key_statement", "snippet_statement"],
}


def query_root(nvim: Path) -> Path:
    """接受 `<检出目录>` 或 `<检出目录>/runtime`，返回 queries 目录。"""
    for base in (nvim / "runtime", nvim):
        if (base / "queries").is_dir():
            return base / "queries"
    sys.exit(f"找不到 queries/：{nvim}")


def strip_dropped(text: str, name: str) -> str:
    """先按 RENAME 换名，再删掉 DROP 里那些节点名所在的整行。"""
    for old, new in RENAME.get(name, {}).items():
        text = re.sub(rf"\({re.escape(old)}\)", f"({new})", text)
    for node in DROP.get(name, []):
        text = re.sub(rf"^[ \t]*\({re.escape(node)}\)[ \t]*\r?\n", "", text, flags=re.M)
    return text


def resolve(root: Path, name: str, stack: tuple[str, ...] = ()) -> tuple[str, list[str]]:
    """返回 (展开继承后的查询文本, 展开过的父语言)。缺失的父语言只报警。"""
    path = root / name / "folds.scm"
    if not path.exists():
        return "", []
    text = path.read_text(encoding="utf-8")
    parents: list[str] = []
    m = re.search(r"^; inherits:\s*(.+)$", text, re.M)
    if m:
        parents = [p.strip() for p in m.group(1).split(",") if p.strip()]
    parts = []
    expanded = []
    for parent in parents:
        if parent in stack or parent == name:
            continue
        sub, _ = resolve(root, parent, stack + (name,))
        if not sub:
            print(f"  缺父查询：{parent}（{name} 继承它）")
            continue
        parts.append(sub)
        expanded.append(parent)
    parts.append(text)
    return "\n".join(parts), expanded


def upstream_commit(nvim: Path) -> str:
    """检出目录的短 commit，写进每份文件的头注释（拿不到就空着）。"""
    try:
        out = subprocess.run(
            ["git", "-C", str(nvim), "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        return out.stdout.strip() or "?"
    except (OSError, subprocess.SubprocessError):
        return "?"


def main() -> None:
    ap = argparse.ArgumentParser(description="从 nvim-treesitter 生成 queries\\ 折叠覆盖层")
    ap.add_argument("--nvim", required=True, help="nvim-treesitter 检出目录（含 runtime\\）")
    ap.add_argument("--parsers", default="parsers", help="解析器包目录（默认 ./parsers）")
    ap.add_argument("--out", default="queries", help="输出目录（默认 ./queries）")
    ap.add_argument("--dry-run", action="store_true", help="只打印将要做什么")
    a = ap.parse_args()

    root = query_root(Path(a.nvim))
    parsers = Path(a.parsers)
    if not parsers.is_absolute():
        parsers = ROOT / parsers
    if not parsers.is_dir():
        sys.exit(f"没有解析器包：{parsers}（先跑 tools/make_parsers.py）")

    out = Path(a.out)
    if not out.is_absolute():
        out = ROOT / out

    langs = sorted(d.name for d in parsers.iterdir() if d.is_dir())
    commit = upstream_commit(Path(a.nvim))
    written = 0
    written_names: set[str] = set()
    skipped: list[str] = []
    for name in langs:
        nvim_name = name.replace("-", "_")
        text, expanded = resolve(root, nvim_name)
        if not text.strip():
            skipped.append(f"{name}（上游没有 folds.scm）")
            continue
        text = strip_dropped(text, name)
        # `; inherits:` 已经展开进正文，留着会让人以为这层还靠继承。
        text = re.sub(r"^; inherits:.*$\r?\n?", "", text, flags=re.M)
        # 一个节点 pattern 都不剩（只剩 `[] @fold` 这种壳）就当没有。
        if not re.search(r"\(\s*[A-Za-z_]", text):
            skipped.append(f"{name}（删完 DROP 就空了）")
            continue
        parents = f"; 已展开继承：{', '.join(expanded)}\n" if expanded else ""
        if a.dry_run:
            print(f"  [dry] {name}  <- {nvim_name}  {len(text.splitlines())} 行")
            continue
        dst = out / name
        dst.mkdir(parents=True, exist_ok=True)
        (dst / "folds.scm").write_text(
            HEADER.format(lang=nvim_name, parents=parents, commit=commit) + text.strip() + "\n",
            encoding="utf-8",
        )
        written += 1
        written_names.add(name)

    if not a.dry_run:
        for d in sorted(out.iterdir()):
            if d.is_dir() and d.name not in written_names:
                print(f"  清掉旧目录: {d.name}")
                shutil.rmtree(d)

    print(f"语言 {len(langs)} 个，写出 {written} 份，跳过 {len(skipped)} 个")
    for s in skipped:
        print(f"  跳过 {s}")
    if not a.dry_run:
        print(f"输出目录：{out}")


if __name__ == "__main__":
    main()
