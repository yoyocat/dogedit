#!/usr/bin/env python3
"""生成「解析器下载」的服务端内容：包 zip + registry.json（make_registry.py）。

用法：
    python tools/make_registry.py --parsers parsers --out deploy/parser-registry/public
    python tools/make_registry.py --parsers parsers --out ... --check      # 只检查不生成
    python tools/make_registry.py --parsers parsers --out ... --only rust,lua

输入是 make_parsers.py 生成的 `parsers\\<name>\\` 插件包目录，输出是部署
目录（`deploy/parser-registry/public`）里的一份快照：

    registry.json                     注册表（客户端整表拉取 / Worker 过滤）
    pkg/<name>/<version>.zip          每包一份 zip，内容 = 包目录本身

每条注册表记录的字段来自包内 manifest.toml（name/display/version/
extensions/filenames/first_lines/abi 原样投影，description/author/license
是 manifest 里的可选附加字段）；size 与 sha256 由生成的 zip 现算。

zip 里一律 `/` 分隔的相对路径（客户端 install.rs 的校验规则之一），
manifest.toml 在根、DLL 用包内 manifest 声明的文件名、queries 原样带上。
"""

import argparse
import datetime
import hashlib
import json
import re
import sys
import tomllib
import zipfile
from pathlib import Path

# 包名与客户端 dg-syntax 的规则一致：a-z 0-9 _ -。
NAME_RE = re.compile(r"^[a-z0-9_-]+$")


def fail(msg: str) -> None:
    print(f"make_registry: {msg}", file=sys.stderr)
    sys.exit(1)


def read_manifest(pkg_dir: Path) -> dict:
    path = pkg_dir / "manifest.toml"
    if not path.is_file():
        fail(f"{pkg_dir}: 缺 manifest.toml")
    with open(path, "rb") as f:
        try:
            data = tomllib.load(f)
        except tomllib.TOMLDecodeError as e:
            fail(f"{pkg_dir}: manifest.toml 解析失败: {e}")
    name = str(data.get("name", "")).strip()
    if not NAME_RE.match(name):
        fail(f"{pkg_dir}: name `{name}` 不合法（只允许 a-z 0-9 _ -）")
    if name != pkg_dir.name:
        fail(f"{pkg_dir}: manifest.name `{name}` 与目录名不一致")
    if not (pkg_dir / str(data.get("dll", f"{name}.dll"))).is_file():
        fail(f"{pkg_dir}: DLL 不存在")
    return data


def normalize_exts(raw) -> list[str]:
    out: list[str] = []
    for e in raw or []:
        e = str(e).strip().lstrip(".").lower()
        if e and e not in out:
            out.append(e)
    return out


def zip_package(out_dir: Path, pkg_dir: Path, name: str, manifest: dict) -> tuple[Path, int, str]:
    """把一个包目录打成 zip，返回 (落点, 字节数, sha256)。"""
    version = str(manifest.get("version", "0.0.0")).strip() or "0.0.0"
    out = out_dir / "pkg" / name / f"{version}.zip"
    out.parent.mkdir(parents=True, exist_ok=True)
    if out.exists():
        out.unlink()
    # 一律 `/` 分隔；时间戳固定，让同内容的包产出同字节的 zip（哈希稳定）。
    stamp = (2026, 1, 1, 0, 0, 0)
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for path in sorted(pkg_dir.rglob("*")):
            rel = path.relative_to(pkg_dir)
            if path.is_dir():
                continue
            if any(part.startswith(".") for part in rel.parts):
                continue
            info = zipfile.ZipInfo(rel.as_posix(), date_time=stamp)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            z.writestr(info, path.read_bytes())
    raw = out.read_bytes()
    return out, len(raw), hashlib.sha256(raw).hexdigest()


def entry_for(out_dir: Path, pkg_dir: Path, name: str, manifest: dict) -> dict:
    _, size, sha = zip_package(out_dir, pkg_dir, name, manifest)
    return {
        "name": name,
        "display": str(manifest.get("display", name)),
        "version": str(manifest.get("version", "0.0.0")).strip() or "0.0.0",
        "extensions": normalize_exts(manifest.get("extensions")),
        "filenames": [str(x) for x in manifest.get("filenames", [])],
        "first_lines": [str(x) for x in manifest.get("first_lines", [])],
        "abi": list(manifest["abi"]) if "abi" in manifest else None,
        "size": size,
        "sha256": sha,
        "path": f"pkg/{name}/{str(manifest.get('version', '0.0.0')).strip() or '0.0.0'}.zip",
        "description": str(manifest.get("description", "")),
        "author": str(manifest.get("author", "")),
        "license": str(manifest.get("license", "")),
    }


def main() -> None:
    ap = argparse.ArgumentParser(description="生成解析器注册表与包 zip")
    ap.add_argument("--parsers", default="parsers", help="插件包根目录（默认 parsers）")
    ap.add_argument("--out", default="deploy/parser-registry/public", help="部署目录")
    ap.add_argument("--only", default="", help="只处理这些包名（逗号分隔）")
    ap.add_argument("--check", action="store_true", help="只检查，不生成")
    args = ap.parse_args()
    pkg_dir_from_args = Path(args.parsers).resolve()
    out_dir = Path(args.out).resolve()

    pkgs = sorted(p for p in pkg_dir_from_args.iterdir() if p.is_dir() and not p.name.startswith("."))
    if args.only:
        keep = {x.strip() for x in args.only.split(",") if x.strip()}
        pkgs = [p for p in pkgs if p.name in keep]
    if not pkgs:
        fail(f"{pkg_dir_from_args}: 没有包目录（先跑 tools/make_parsers.py？）")

    # 扩展名冲突检查：注册表按包名字典序取第一个命中，登记两遍只会让
    # 第二个包的高亮静默失效 —— 生成端就拦下来。
    seen: dict[str, str] = {}
    entries = []
    for p in pkgs:
        m = read_manifest(p)
        for e in normalize_exts(m.get("extensions")):
            if e in seen and seen[e] != p.name:
                fail(f"扩展名 .{e} 同时属于 {seen[e]} 和 {p.name}（registry 只认先者）")
            seen[e] = p.name
        # --check 不写盘：生成 entry 前置的冲突检查已经够拦；zip 交给正式跑。
        if not args.check:
            entries.append(entry_for(out_dir, p, p.name, m))

    registry = {
        "schema": 1,
        "updated": datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "packages": sorted(entries, key=lambda e: e["name"]),
    }
    if args.check:
        print(f"check: {len(pkgs)} 个包，{len(seen)} 个扩展名，无冲突")
        return

    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "registry.json").write_text(
        json.dumps(registry, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    total = sum(e["size"] for e in registry["packages"])
    print(
        f"registry: {len(entries)} 个包，zip 共 {total / 1024 / 1024:.1f} MB → {out_dir}"
    )


if __name__ == "__main__":
    main()
