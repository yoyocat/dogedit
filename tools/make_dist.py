#!/usr/bin/env python3
r"""组装便携版 zip（绿色版）到 dist\：exe + 随包数据 + 许可文件。

用法：
    python tools/make_dist.py                      # 版本号取自 Cargo.toml（dg-app）
    python tools/make_dist.py --version 0.2.0      # 显式版本号（CI 用 tag 去 v 传入）
    python tools/make_dist.py --parsers all        # 把 parsers\ 全量打进包（默认不带）

包内布局（程序一律先在 exe 旁边找数据，所以全部平铺在顶层目录下）：

    dogedit.exe             target\release 产物，子系统校验 + --selftest 都过才进包
    setting.ini             生成的空配置（只有 [Setting]）：没有这个文件时程序
                            对设置的写入会整体放弃，带上它首次运行起就能落盘；
                            空段读不到任何键，行为与默认值完全一致
    key.ini                 默认键位表（crates\app\key.ini），放盘上供直接修改
    template.ini + template\    新建文件模板 —— 唯一没有内建副本的数据，必须随包
    lang\ themes\             语言表 / 主题（exe 里有内建副本，带上是为了让用户
                            能直接改外置文件）
    docs\                     用户手册与脚本 API；「在线帮助」(F1) 就在 exe 旁边
                            找这个目录，缺了它帮助命令只能报「没找到帮助文件」
    packages\                 脚本包；示例包 luapaint / snake 不进发布包，只在仓库
                            里当脚本 API 的例子（没有其它包时这个目录整个不带）
    LICENSE NOTICE README.md licenses\    许可文本与图标出处声明

语法解析器包（parsers\）默认不进包：体积是整包的大头，程序里有「解析器下载」
可联网自装；--parsers all 时改为本仓库现有的 parsers\ 全量打进包。
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / "dist"

# 许可文件在包内的归宿：图标出处声明沿用仓库里的原名，加前缀防与 MIT LICENSE 混淆
ICON_LICENSE = Path("crates/app/assets/icons/LICENSE")
ICON_SOURCES = Path("crates/app/assets/icons/SOURCES.txt")

# 不进发布包的脚本包：示例包留在仓库里给大家照着写就够了，随包发出去会被自动装
# 上，而用户改不动它（Bundled 是只读根）
PACKAGES_EXCLUDED = {"luapaint", "snake"}

# 帮助命令真正会打开的页面。user_manual.html 是 F1 的首选，script_api.html
# 是手册里唯一的外链；两者缺一，发布包里的「在线帮助」就是坏的。
DOCS_REQUIRED = ("user_manual.html", "script_api.html")


def utf8_stdio() -> None:
    """把两条输出流固定成 UTF-8。

    Windows 上输出被重定向到管道时（CI 就是），编码取系统 locale，英文
    runner 是 cp1252：中文提示、错误信息乃至 --help 一 print 就抛
    UnicodeEncodeError。CI 日志按 UTF-8 收，这里显式写 UTF-8；
    backslashreplace 兜住子进程输出里可能夹带的孤立代理项。
    """
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="backslashreplace")


def die(msg: str):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def app_version() -> str:
    """从 cargo metadata 拿 dg-app 的版本号（避开手解析 Cargo.toml）。"""
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT, check=True, capture_output=True, text=True, encoding="utf-8",
    ).stdout
    for pkg in json.loads(out)["packages"]:
        if pkg["name"] == "dg-app":
            return pkg["version"]
    die("cargo metadata 里找不到 dg-app")


def build() -> Path:
    """编译 release 产物，返回 dogedit.exe 的路径。"""
    target = os.environ.get("CARGO_TARGET_DIR")
    target_dir = Path(target) if target else ROOT / "target"
    if not target_dir.is_absolute():
        target_dir = ROOT / target_dir
    exe = target_dir / "release" / "dogedit.exe"
    subprocess.run(
        ["cargo", "build", "--release", "-p", "dg-app"], cwd=ROOT, check=True
    )
    if not exe.exists():
        die(f"编译完没找到 {exe}")
    return exe


def check_gui_subsystem(exe: Path) -> None:
    """发行版必须是 GUI 子系统，否则双击打开会多出一个控制台窗口。

    PE 可选头的 Subsystem 字段：2 = WINDOWS_GUI，3 = WINDOWS_CUI。后者由系统
    在进程启动时配一个控制台，从资源管理器打开就是一个 DOS 窗口。开发构建
    有意保留控制台，所以这道检查只放在打包这一步。非 Windows 平台上没有 PE
    产物（交叉编译演练），跳过。
    """
    if sys.platform != "win32":
        return
    with exe.open("rb") as f:
        dos = f.read(0x40)
        if len(dos) < 0x40 or dos[:2] != b"MZ":
            die(f"{exe.name} 不是 PE 文件")
        pe = struct.unpack_from("<I", dos, 0x3C)[0]
        f.seek(pe)
        if f.read(4) != b"PE\0\0":
            die(f"{exe.name} 的 PE 头不对")
        # PE 签名 4 字节 + COFF 头 20 字节 + 可选头内 0x44 处的 Subsystem
        f.seek(pe + 4 + 20 + 0x44)
        (subsystem,) = struct.unpack("<H", f.read(2))
    if subsystem != 2:
        die(f"{exe.name} 是控制台子系统 (Subsystem={subsystem})，双击会弹 DOS 窗口")


def selftest(exe: Path) -> None:
    """无窗自检，过不了就不打包。"""
    # 别让 text=True 按 locale 解码：cp1252 会把 exe 的 UTF-8 输出读成乱码，
    # 碰到未定义字节还会抛 UnicodeDecodeError
    run = subprocess.run([str(exe), "--selftest"], cwd=ROOT, capture_output=True,
                         text=True, encoding="utf-8", errors="replace")
    for stream in (run.stdout, run.stderr):
        if stream.strip():
            print(stream.strip())
    if run.returncode != 0:
        die("--selftest 未通过，中止打包")


def copy_file(src: Path, dst: Path) -> None:
    if not src.is_file():
        die(f"缺少随包文件 {src.relative_to(ROOT)}")
    shutil.copyfile(src, dst)


def copy_tree(src: Path, dst: Path) -> None:
    if not src.is_dir():
        die(f"缺少随包目录 {src.relative_to(ROOT)}")
    shutil.copytree(src, dst)


def copy_docs(src: Path, dst: Path) -> None:
    r"""随包文档：整棵 docs\ 复制，打包前先钉住帮助命令要用的那两页。

    README 也链到 docs\ 里的设计笔记，只挑两页带走会把那些链接留在包里断着，
    所以这里是整棵树。校验放在复制前，缺页直接中止而不是发出一个坏包。
    """
    for name in DOCS_REQUIRED:
        if not (src / name).is_file():
            die(f"缺少随包文档 {src.relative_to(ROOT) / name}，在线帮助打不开")
    copy_tree(src, dst)


def copy_packages(src: Path, dst: Path) -> None:
    """脚本包：按 PACKAGES_EXCLUDED 剔除示例包，剩下的逐个目录复制。"""
    if not src.is_dir():
        die(f"缺少随包目录 {src.relative_to(ROOT)}")
    kept = [p for p in sorted(src.iterdir()) if p.name not in PACKAGES_EXCLUDED]
    if not kept:
        return
    dst.mkdir(parents=True)
    for entry in kept:
        if entry.is_dir():
            shutil.copytree(entry, dst / entry.name)
        else:
            shutil.copyfile(entry, dst / entry.name)


def stage(exe: Path, ver: str, parsers: str) -> Path:
    """按白名单把随包内容拼到 dist/stage/dogedit-<ver>/，返回 staging 根。"""
    stage_root = DIST / "stage"
    top = stage_root / f"dogedit-{ver}"
    if stage_root.exists():
        shutil.rmtree(stage_root)
    top.mkdir(parents=True)

    shutil.copyfile(exe, top / "dogedit.exe")
    # 空配置：UTF-8 无 BOM，程序两种编码都能读
    (top / "setting.ini").write_bytes(b"[Setting]\n")
    copy_file(ROOT / "crates/app/key.ini", top / "key.ini")
    copy_file(ROOT / "template.ini", top / "template.ini")
    copy_tree(ROOT / "template", top / "template")
    copy_tree(ROOT / "lang", top / "lang")
    copy_tree(ROOT / "themes", top / "themes")
    copy_docs(ROOT / "docs", top / "docs")
    copy_packages(ROOT / "packages", top / "packages")
    copy_file(ROOT / "LICENSE", top / "LICENSE")
    copy_file(ROOT / "NOTICE", top / "NOTICE")
    copy_file(ROOT / "README.md", top / "README.md")

    licenses = top / "licenses"
    licenses.mkdir()
    copy_file(ROOT / ICON_LICENSE, licenses / "tabler-icons-LICENSE.txt")
    copy_file(ROOT / ICON_SOURCES, licenses / "tabler-icons-SOURCES.txt")

    if parsers == "all":
        if not (ROOT / "parsers").is_dir():
            die("要 --parsers all，但本仓库还没有 parsers\\（用 tools/make_parsers.py 生成）")
        copy_tree(ROOT / "parsers", top / "parsers")
    return stage_root


def make_zip(stage_root: Path, ver: str) -> Path:
    """staging 整树压成 dist/dogedit-<ver>-x64.zip（保留顶层目录），返回 zip 路径。"""
    zip_path = DIST / f"dogedit-{ver}-x64.zip"
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for path in sorted(stage_root.rglob("*")):
            if path.is_file():
                z.write(path, path.relative_to(stage_root).as_posix())
    return zip_path


def check_help(zip_path: Path, ver: str) -> None:
    """最后再看一眼 zip 内部的路径：帮助读的是 `<exe>\\docs`，不是仓库根目录。"""
    with zipfile.ZipFile(zip_path) as z:
        names = set(z.namelist())
    prefix = f"dogedit-{ver}/docs/"
    for name in DOCS_REQUIRED:
        if prefix + name not in names:
            die(f"发布包缺少 {prefix}{name}，在线帮助打不开")


def make_sha256(zip_path: Path) -> Path:
    digest = hashlib.sha256(zip_path.read_bytes()).hexdigest()
    sha_path = zip_path.with_name(zip_path.name + ".sha256")
    # 按字节写死 LF 结尾：write_text 在 Windows 上会换成 CRLF，校验工具会把
    # 回车当成文件名的一部分
    sha_path.write_bytes(f"{digest}  {zip_path.name}\n".encode("ascii"))
    return sha_path


def main() -> None:
    utf8_stdio()
    ap = argparse.ArgumentParser(description="组装便携版 zip（见文件头 docstring）")
    ap.add_argument("--version", help="包版本号；缺省用 Cargo.toml 里 dg-app 的版本")
    ap.add_argument("--parsers", choices=("none", "all"), default="none",
                    help="语法解析器包：none=不进包（默认），all=全量打包")
    args = ap.parse_args()

    ver = args.version or app_version()
    if not re.fullmatch(r"[A-Za-z0-9._-]+", ver):
        die(f"版本号 {ver!r} 含有不适合进文件名的字符")

    exe = build()
    check_gui_subsystem(exe)
    selftest(exe)
    stage_root = stage(exe, ver, args.parsers)
    zip_path = make_zip(stage_root, ver)
    check_help(zip_path, ver)
    sha_path = make_sha256(zip_path)
    shutil.rmtree(stage_root)

    size_mb = zip_path.stat().st_size / 1024 / 1024
    print(f"打包完成: {zip_path.name}  ({size_mb:.1f} MB)")
    print(f"校验文件: {sha_path.name}")
    print(f"版本来源: {'命令行' if args.version else 'Cargo.toml'}")


if __name__ == "__main__":
    main()
