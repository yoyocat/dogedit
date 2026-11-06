#!/usr/bin/env python3
"""Rebuild the bundled UI language tables.

Both tables are re-emitted from the key/id structure: fresh metadata, a stable
section order, UTF-8 output, and an English table in sentence case with the
project's own terminology. The Chinese table keeps its values because it is
also the canonical key space for the Chinese literals compiled into the code;
only its metadata, order and encoding change.

    python tools/make_lang.py
"""

import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LANG = os.path.join(ROOT, "lang")

INFO = [
    ("Author", "DogEdit contributors"),
    ("Home", ""),
    ("Reviewer", "DogEdit contributors"),
]

PROPER = {
    "utf", "utf-8", "utf-16", "html", "css", "php", "lua", "python",
    "javascript", "rust", "url", "ini", "ftp", "ssh", "lsp", "json", "xml",
    "c", "c++", "c#", "base64", "md5", "sha1", "grep", "ctags", "wsl",
    "api", "sql", "regex", "markdown", "dogedit",
}

PHRASES = [
    ("Configure Templates", "Set up templates"),
    ("Preferences", "Settings"),
    ("Delete", "Remove"),
]


def decode(path):
    data = open(path, "rb").read()
    if data.startswith(b"\xff\xfe"):
        return data[2:].decode("utf-16-le")
    if data.startswith(b"\xef\xbb\xbf"):
        return data[3:].decode("utf-8")
    if len(data) > 2 and data[1] == 0 and data[3] == 0:
        return data.decode("utf-16-le")
    return data.decode("utf-8")


def parse(text):
    sections = {}
    order = []
    cur = None
    for raw in text.splitlines():
        line = raw.strip().lstrip("\ufeff")
        if not line or line.startswith(";") or line.startswith("//"):
            continue
        if line.startswith("[") and line.endswith("]"):
            cur = line[1:-1].strip()
            if cur not in sections:
                sections[cur] = []
                order.append(cur)
            continue
        if cur is None or "=" not in line:
            continue
        key, value = line.split("=", 1)
        sections[cur].append((key.strip(), value.strip()))
    return order, sections


def case_segment(seg):
    """Sentence-case one line of a value, leaving protected words alone."""
    parts = seg.split(" ")
    out = []
    cap_next = True
    for word in parts:
        core = word.strip("&()[]{}<>:;,.'\"!?-…")
        if not word:
            out.append(word)
            continue
        if (
            cap_next
            or not core
            or any(ch.isdigit() for ch in core)
            or (core.isupper() and len(core) > 1)
            or core.lower() in PROPER
            or "%" in core
            or core.startswith("\\")
        ):
            out.append(word)
        else:
            out.append(word[0].lower() + word[1:])
        cap_next = bool(re.search(r"[.!?:]$", core))
    return " ".join(out)


def sentence_case(value):
    for a, b in PHRASES:
        value = value.replace(a, b)
    # `\n` is written into the table as a literal escape and starts a new
    # sentence: case each part on its own, or a word like "DogEdit!\nWe" is
    # neither a known proper noun nor all-caps and gets lowered.
    return "\\n".join(case_segment(part) for part in value.split("\\n"))


def transform_en(value):
    value = value.replace("...", "…")
    return sentence_case(value)


def emit(path, order, sections, transform):
    lines = ["; DogEdit UI language table", ""]
    lines.append("[Info]")
    for k, v in INFO:
        lines.append(f"{k}={v}")
    lines.append("")
    men = sorted(
        [s for s in order if re.fullmatch(r"MENU\d+", s)],
        key=lambda s: 0 if s == "MENU128" else int(s[4:]),
    )
    others = [s for s in order if re.fullmatch(r"DLG\d+", s)]
    others.sort(key=lambda s: int(s[3:]))
    rest = [
        s
        for s in order
        if s not in men and s not in others and s.upper() != "INFO"
    ]
    for sec in men + others + rest:
        rows = sections[sec]
        if sec.startswith("MENU"):
            rows = sorted(rows, key=lambda kv: (kv[0].startswith("P"), int(re.sub(r"\D", "", kv[0]) or 0)))
        elif sec.startswith("DLG") or sec == "STRING":
            rows = sorted(rows, key=lambda kv: int(kv[0]) if kv[0].isdigit() else 1 << 32)
        lines.append(f"[{sec}]")
        for k, v in rows:
            lines.append(f"{k}={transform(v) if transform else v}")
        lines.append("")
    with open(path, "w", newline="\n", encoding="utf-8") as f:
        f.write("\n".join(lines))


def main():
    for name, transform in (("en-US.lng", transform_en), ("zh-CN.lng", None)):
        path = os.path.join(LANG, name)
        order, sections = parse(decode(path))
        emit(path, order, sections, transform)
    print("rewrote lang/en-US.lng and lang/zh-CN.lng")


if __name__ == "__main__":
    main()
