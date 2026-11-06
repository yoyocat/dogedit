#!/usr/bin/env python3
"""Regenerate the bundled theme palettes.

Each theme keeps its file name, key order and `[Style]` flags, but every colour
is computed from a small per-theme spec (background, foreground, accent hue).
No colour value is copied from another theme. Run from the repository root:

    python tools/make_themes.py
"""

import os
import colorsys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
THEMES = os.path.join(ROOT, "themes")

# name: (background, foreground, accent hue, dark?)
SPECS = {
    "Acid": ("#141A10", "#D9EFC8", 92, True),
    "Andes": ("#1A2028", "#D3E2EC", 204, True),
    "Black": ("#050505", "#D2D2D2", 210, True),
    "Blue": ("#0B1A2A", "#D8E8F8", 212, True),
    "Blue Green": ("#0A1E1C", "#D2EFE8", 172, True),
    "Cobalt": ("#0B1730", "#D4E4FF", 222, True),
    "Dark Blue": ("#0A1430", "#CADAFF", 230, True),
    "Darkness": ("#101012", "#CACACC", 18, True),
    "Desert": ("#2A2118", "#EADAC2", 34, True),
    "Eclipse": ("#101A20", "#CAD6DE", 196, True),
    "Espresso Libre": ("#1C1A17", "#E2D8CA", 30, True),
    "Evening": ("#1A1D24", "#D2DAE8", 214, True),
    "DogEdit": ("#FBFBFB", "#202226", 210, False),
    "DogEdit_Dark": ("#1E2126", "#D9DFE8", 206, True),
    "Fish": ("#102026", "#D1EAEF", 186, True),
    "Markdown": ("#FFFFFF", "#1F2226", 210, False),
    "Markdown_Dark": ("#1B1D1F", "#DADADA", 200, True),
    "Monokai": ("#232823", "#F1F1E8", 142, True),
    "Obsidian": ("#15171A", "#D2D6DC", 194, True),
    "PlasticCodeWrap": ("#1F2329", "#D9DFE7", 212, True),
    "Slate": ("#1C232B", "#D4DEE8", 208, True),
    "Slate Forever": ("#202830", "#DAE4EE", 206, True),
    "Solarized (Dark)": ("#002B36", "#E8EADB", 176, True),
    "Solarized (Light)": ("#FDF6E3", "#1E3040", 176, False),
    "Tabula": ("#F7F3E8", "#2A2A2A", 26, False),
    "mildBlue": ("#F2F6FB", "#1F3040", 212, False),
}

SOURCES = {}
RENAMES = {}


def rgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def hx(c):
    return "#%02X%02X%02X" % tuple(max(0, min(255, int(round(v)))) for v in c)


def mix(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def hue(h, s, l):
    r, g, b = colorsys.hls_to_rgb((h % 360) / 360.0, l, s)
    return (r * 255, g * 255, b * 255)


def lum(c):
    return (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) / 255.0


def palette(spec):
    bg, fg, h, dark = rgb(spec[0]), rgb(spec[1]), spec[2], spec[3]
    l = 0.70 if dark else 0.32
    muted = mix(fg, bg, 0.45)
    comment = mix(fg, bg, 0.55)
    soft = mix(bg, fg, 0.18)

    def syn(shift, s=0.62, ll=None):
        return hue(h + shift, s, l if ll is None else ll)

    sel_bg = hue(h, 0.58, 0.38 if dark else 0.78)
    sel_fg = hue(h, 0.15, 0.96 if dark else 0.12)
    tip_bg = hue(h + 8, 0.55, 0.30 if dark else 0.90)
    tip_fg = hue(h, 0.20, 0.95 if dark else 0.15)
    hl_fg = hue(0, 0.0, 0.98 if dark else 0.10)
    cols = {
        "Default": (bg, fg),
        "Selection": (sel_bg, sel_fg),
        "WhiteSpace": (bg, mix(fg, bg, 0.35)),
        "LineNumber": (bg, muted),
        "Folding": (bg, muted),
        "Pair": (bg, syn(135, 0.58)),
        "Ruler": (mix(bg, fg, 0.35), hue(h, 0.45, 0.35 if dark else 0.55)),
        "Found": (bg, hue(36, 0.85, 0.62 if dark else 0.38)),
        "Comment1": (bg, comment),
        "Comment2": (bg, comment),
        "String1": (bg, syn(70)),
        "String2": (bg, syn(70)),
        "Tag": (bg, syn(328)),
        "Macro": (bg, syn(90)),
        "Url": (bg, syn(160)),
        "Email": (bg, syn(120, 0.45)),
        "Number": (bg, syn(52)),
        "Word1": (bg, syn(328)),
        "Word2": (bg, syn(180)),
        "Word3": (bg, syn(120)),
        "Word4": (bg, syn(250)),
        "Function": (bg, syn(180)),
        "Var": (bg, syn(18)),
        "SubLan": (bg, muted),
        "Operator": (bg, mix(fg, bg, 0.10)),
        "WordComplete": (sel_bg, sel_fg),
        "BookMark": (bg, hue(50, 0.82, 0.60 if dark else 0.38)),
        "IconArea": (bg, mix(bg, (0, 0, 0), 0.35)),
        "ConChar": (mix(fg, bg, 0.08), mix(bg, fg, 0.12)),
        "CallTip": (tip_bg, tip_fg),
        "Type": (bg, syn(300)),
        "Error": (bg, hue(2, 0.85, 0.62 if dark else 0.40)),
        "TabNormal": (bg, muted),
        "TabSelected": (sel_bg, sel_fg),
        "TabHover": (mix(bg, sel_bg, 0.45), fg),
    }
    hues = [2, 32, 52, 118, 212, 62, 322, 268]
    for i, hh in enumerate(hues, 1):
        cols["Highlight%d" % i] = (hue(hh, 0.62, 0.34 if dark else 0.82), hl_fg)
    singles = {
        "IndentLine": mix(bg, fg, 0.22),
        "SelIndicator": mix(bg, fg, 0.48),
        "WaveLine": hue(2, 0.80, 0.56 if dark else 0.42),
        "ActiveLine": mix(bg, fg, 0.09 if dark else 0.05),
        "FoldingLine": syn(180, 0.55, 0.50),
        "DefaultBgNull": bg,
        "BoundaryLine": mix(bg, fg, 0.26),
        "MaskLine1": hue(40, 0.30, 0.55),
        "MaskLine2": hue(150, 0.30, 0.50),
        "MaskLine3": hue(210, 0.30, 0.55),
        "MaskLine4": hue(320, 0.30, 0.55),
        "Caret": fg,
    }
    return cols, singles


def rewrite(path, name):
    lines = open(path, encoding="utf-8").read().splitlines()
    theme = []
    style = []
    cur = None
    for line in lines:
        s = line.strip()
        if s.startswith("["):
            cur = s.lower()
            if cur == "[theme]":
                theme.append(line)
            elif cur == "[style]":
                style.append(line)
            continue
        if cur == "[theme]":
            theme.append(line)
        elif cur == "[style]":
            style.append(line)
    # Blank lines between the two sections are added below; keeping the ones the
    # source already had would grow the gap on every run.
    while theme and not theme[-1].strip():
        theme.pop()

    cols, singles = palette(SPECS[name])
    out = []
    for line in theme:
        s = line.strip()
        if not s or s.startswith("[") or "=" not in s:
            out.append(line)
            continue
        key, val = s.split("=", 1)
        key = key.strip()
        if key in cols:
            a, b = cols[key]
            val = hx(a) + "," + hx(b)
        elif key in singles:
            val = hx(singles[key])
        elif "," in val:
            val = hx(mix(rgb(SPECS[name][0]), rgb(SPECS[name][1]), 0.5)) + "," + hx(rgb(SPECS[name][1]))
        else:
            val = hx(singles["IndentLine"])
        out.append(key + "=" + val)
    if style:
        out.append("")
        out.extend(style)
    return "\n".join(out) + "\n"


def main():
    for name in SPECS:
        src = os.path.join(THEMES, SOURCES.get(name, name + ".ini"))
        if not os.path.exists(src):
            continue
        text = rewrite(src, name)
        dst = os.path.join(THEMES, RENAMES.get(name + ".ini", name + ".ini"))
        with open(dst, "w", newline="\n", encoding="utf-8") as f:
            f.write(text)
        if dst != src:
            os.remove(src)
    print("rewrote %d themes" % len(SPECS))


if __name__ == "__main__":
    main()
