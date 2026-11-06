#!/usr/bin/env python3
"""Compose the toolbar, output-bar and tab-strip sprite sheets, twice.

The glyphs come from the RGBA atlases in `crates/app/assets/icons/` — one cell
per icon name, 16/24/32 px tall — and this script only tints and lays them out.
Each sheet is a 32 bpp BI_RGB BMP with straight alpha, which is what
`toolbar::decode_strip` and the image lists expect. Run from the repository
root:

    python tools/make_assets.py

The five sheets in `sprites/` carry the LIGHT ink and the five in
`sprites/dark/` carry the DARK ink. Both hold the same glyphs in the same
order; the app picks the set whose contrast suits the toolbar face it is
about to paint. The atlas exports are Lanczos downsamples, so every stroke
wears a wide skirt of faint coverage -- on a pale bar that fringe reads as
blur and makes the glyphs look heavy. The LIGHT pass firms those edges up
(see `shaped_alpha`), while the DARK pass keeps the untouched coverage.
The atlases themselves are imported by `tools/import_icons.py`.
"""

import os
import struct
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPRITES = os.path.join(ROOT, "crates", "app", "assets", "sprites")
ATLAS = os.path.join(ROOT, "crates", "app", "assets", "icons")

# Command-family ink KEYS, resolved through LIGHT or DARK at generation time.
# A single flat tint cannot be optimal on both chrome faces: the best shared
# luminance reaches only ~3.7:1 on each. Two sheets let each ink sit far away
# from its own background -- LIGHT inks at Y~0.162 on the #E7E9EC bar (4.1:1),
# DARK inks at Y~0.34 on the #202020 bar (~6:1). Icons need only 3:1, so the
# LIGHT set stays well above the darkest possible ink: HLS S~0.56 keeps the
# bar light and airy rather than dense, while the crisp edges carry the
# definition. The hues and the command-family mapping are the same in both
# tables, so a theme switch changes depth, never meaning.
BRIGHT = "BRIGHT"  # neutral slate: info / idle tab close
BLUE = "BLUE"
RED = "RED"
GOLD = "GOLD"
AMBER = "AMBER"
GREEN = "GREEN"
VIOLET = "VIOLET"
CYAN = "CYAN"
HOVER = "HOVER"  # tab close, hovered
PLAIN = "PLAIN"  # tab close, idle
PRESS = "PRESS"  # tab close, pressed

LIGHT = {
    "BLUE": (0x37, 0x6F, 0xC4),
    "CYAN": (0x26, 0x7A, 0x85),
    "GREEN": (0x24, 0x80, 0x4A),
    "VIOLET": (0x99, 0x48, 0xCB),
    "AMBER": (0x97, 0x65, 0x2A),
    "GOLD": (0x84, 0x6F, 0x25),
    "RED": (0xC7, 0x38, 0x38),
    "BRIGHT": (0x63, 0x70, 0x90),
    "PLAIN": (0x64, 0x70, 0x8E),
    "HOVER": (0x37, 0x6F, 0xC4),
    "PRESS": (0x2C, 0x58, 0x9C),
}

DARK = {
    "BLUE": (0x7B, 0xA0, 0xD1),
    "CYAN": (0x4C, 0xAA, 0xBD),
    "GREEN": (0x45, 0xB0, 0x82),
    "VIOLET": (0xB3, 0x90, 0xD1),
    "AMBER": (0xD1, 0x96, 0x5B),
    "GOLD": (0xB8, 0x9F, 0x31),
    "RED": (0xDB, 0x86, 0x8C),
    "BRIGHT": (0x96, 0x9E, 0xA9),
    "PLAIN": (0x98, 0x9E, 0xA7),
    "HOVER": (0x7B, 0xA0, 0xD1),
    "PRESS": (0x55, 0x8B, 0xD2),
}

# Set by main() once per face; put() resolves a key through it.
INK = LIGHT

# One entry per image index of the bar's table; cell 8 doubles as the
# separator's index, and a separator never draws its bitmap. Each glyph
# carries the ink of its command family: documents and paths blue, editing
# amber, panels and view green, find/go-to violet, tools cyan, close/exit
# and other destructive rows red, markers gold.
TOOLBAR = [
    ("file-plus", BLUE),
    ("folder-open", BLUE),
    ("file-settings", BLUE),
    ("trash", RED),
    ("reload", BLUE),
    ("device-floppy", BLUE),
    ("history", BLUE),
    ("file-pencil", BLUE),
    ("square-x", RED),
    ("x", RED),
    ("window-off", RED),
    ("logout", RED),
    ("archive", BLUE),
    ("arrow-back-up", AMBER),
    ("arrow-forward-up", AMBER),
    ("scissors", AMBER),
    ("copy", AMBER),
    ("link", BLUE),
    ("clipboard", AMBER),
    ("forms", BLUE),
    ("external-link", BLUE),
    ("arrow-bar-left", RED),
    ("arrow-bar-right", RED),
    ("arrow-right", GREEN),
    ("arrow-left", GREEN),
    ("message-plus", AMBER),
    ("window-maximize", GREEN),
    ("app-window", GREEN),
    ("select-all", AMBER),
    ("cursor-text", AMBER),
    ("text-wrap", GREEN),
    ("indent-increase", AMBER),
    ("indent-decrease", AMBER),
    ("row-insert-bottom", AMBER),
    ("letter-case-upper", AMBER),
    ("letter-case-lower", AMBER),
    ("letter-case", AMBER),
    ("letter-case-toggle", AMBER),
    ("arrow-merge", AMBER),
    ("arrows-exchange", AMBER),
    ("row-remove", AMBER),
    ("backspace", AMBER),
    ("space-off", AMBER),
    ("list-tree", GREEN),
    ("folder-star", GREEN),
    ("file-text", GREEN),
    ("terminal-2", GREEN),
    ("braces", GREEN),
    ("world", GREEN),
    ("quote", AMBER),
    ("message-minus", AMBER),
    ("brand-git", CYAN),
    ("server-2", CYAN),
    ("quote-off", AMBER),
    ("align-left", AMBER),
    ("align-right", AMBER),
    ("align-center", AMBER),
    ("clipboard-text", AMBER),
    ("clipboard-x", AMBER),
    ("calendar", AMBER),
    ("clock", AMBER),
    ("binary", CYAN),
    ("binary-off", CYAN),
    ("chevron-down", VIOLET),
    ("chevron-up", VIOLET),
    ("command", VIOLET),
    ("maximize", GREEN),
    ("code", VIOLET),
    ("list-numbers", GREEN),
    ("search", VIOLET),
    ("ruler", GREEN),
    ("world-www", GREEN),
    ("list-search", VIOLET),
    ("replace", VIOLET),
    ("file-diff", VIOLET),
    ("folder-search", VIOLET),
    ("highlight", GREEN),
    ("layout-bottombar", GREEN),
    ("layout-navbar", GREEN),
    ("layout-grid", GREEN),
    ("clipboard-copy", BLUE),
    ("brackets", AMBER),
    ("columns-3", GREEN),
    ("zoom-reset", GREEN),
    ("player-record", RED),
    ("fold-down", GREEN),
    ("player-play", GREEN),
    ("repeat", CYAN),
    ("fold-up", GREEN),
    ("chevrons-up", GREEN),
    ("chevrons-down", GREEN),
    ("bookmark", GOLD),
    ("bookmark-off", RED),
    ("bookmark-plus", GOLD),
    ("chart-bar", GREEN),
    ("hexagon-letter-h", GREEN),
    ("language", BLUE),
    ("lock", BLUE),
    ("player-pause", CYAN),
    ("sum", CYAN),
    ("sort-ascending", AMBER),
    ("sort-descending", AMBER),
    ("refresh", CYAN),
    ("layout-navbar-inactive", GREEN),
    ("layout-bottombar-inactive", GREEN),
    ("settings", CYAN),
    ("file-code", CYAN),
    ("keyboard", CYAN),
    ("tools", CYAN),
    ("zoom-in", GREEN),
    ("puzzle", CYAN),
    ("info-circle", BRIGHT),
]

OUTPUT = [
    ("chevron-left", GREEN),
    ("chevron-right", GREEN),
    ("trash", RED),
    ("chevron-down", GOLD),
    ("eye", BLUE),
    ("layout-sidebar-right", GREEN),
    ("text-wrap", GREEN),
    ("star", GOLD),
    ("x", RED),
]

NAMES = sorted({name for name, _ in TOOLBAR} | {name for name, _ in OUTPUT})

# Filled variants are imported from the set's filled folder instead of outline.
FILLED = {"player-record", "player-play", "star"}


def read_png(path):
    """Decode an 8-bit RGBA, non-interlaced PNG into top-down pixels."""
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    pos = 8
    idat = []
    w = h = 0
    while pos < len(data):
        size = int.from_bytes(data[pos:pos + 4], "big")
        kind = data[pos + 4:pos + 8]
        chunk = data[pos + 8:pos + 8 + size]
        pos += 12 + size
        if kind == b"IHDR":
            w = int.from_bytes(chunk[0:4], "big")
            h = int.from_bytes(chunk[4:8], "big")
            depth, color, interlace = chunk[8], chunk[9], chunk[12]
            if depth != 8 or color != 6 or interlace != 0:
                raise SystemExit(f"{path}: expected 8-bit RGBA")
        elif kind == b"IDAT":
            idat.append(chunk)
        elif kind == b"IEND":
            break
    raw = zlib.decompress(b"".join(idat))
    stride = w * 4
    px = bytearray(w * h * 4)
    prev = bytearray(stride)
    at = 0
    for y in range(h):
        filter_kind = raw[at]
        at += 1
        line = bytearray(raw[at:at + stride])
        at += stride
        if filter_kind == 1:
            for i in range(4, stride):
                line[i] = (line[i] + line[i - 4]) & 0xFF
        elif filter_kind == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif filter_kind == 3:
            for i in range(stride):
                left = line[i - 4] if i >= 4 else 0
                line[i] = (line[i] + ((left + prev[i]) >> 1)) & 0xFF
        elif filter_kind == 4:
            for i in range(stride):
                a = line[i - 4] if i >= 4 else 0
                b = prev[i]
                c = prev[i - 4] if i >= 4 else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
        elif filter_kind != 0:
            raise SystemExit(f"{path}: bad filter {filter_kind}")
        px[y * stride:(y + 1) * stride] = line
        prev = line
    return w, h, px


class Atlas:
    def __init__(self, size):
        w, h, px = read_png(os.path.join(ATLAS, f"icons-{size}.png"))
        if h != size or w % size or w // size != len(NAMES):
            raise SystemExit(f"icons-{size}.png: expected {len(NAMES)} cells")
        self.size = size
        self.stride = w
        self.px = px

    def alpha(self, cell, x, y):
        return self.px[(y * self.stride + cell * self.size + x) * 4 + 3]


# The LIGHT pass only: a 3x3 unsharp mask pushes the inner edge of a stroke
# towards solid and its outer fringe towards clear, then a shifted coverage
# ramp drops the remaining faint pixels and firms up what is left. The result
# is a narrower, harder stroke on the pale bar. Roughly a quarter of the
# 16 px atlas sits at alpha 1..63; hardening clears almost all of it.
UNSHARP = 0.8
EDGE_LO = 0.23
EDGE_HI = 0.91
EDGE_GAMMA = 0.95


def clamp_byte(v):
    return 0 if v < 0 else (255 if v > 255 else int(v + 0.5))


def shaped_alpha(atlas, index, size, crisp):
    """One cell's coverage; `crisp` hardens the edges for the light bar."""
    src = bytearray(size * size)
    for y in range(size):
        for x in range(size):
            src[y * size + x] = atlas.alpha(index, x, y)
    if not crisp:
        return src

    sharp = bytearray(size * size)
    for y in range(size):
        for x in range(size):
            p = y * size + x
            total = count = 0
            for dy in (-1, 0, 1):
                yy = y + dy
                if not 0 <= yy < size:
                    continue
                for dx in (-1, 0, 1):
                    xx = x + dx
                    if 0 <= xx < size:
                        total += src[yy * size + xx]
                        count += 1
            sharp[p] = clamp_byte(src[p] + UNSHARP * (src[p] - total / count))

    lo = EDGE_LO * 255
    span = (EDGE_HI - EDGE_LO) * 255
    out = bytearray(size * size)
    for p, a in enumerate(sharp):
        n = min(1.0, max(0.0, (a - lo) / span))
        out[p] = clamp_byte(255 * n ** EDGE_GAMMA)
    return out


def put(buf, width, x, y, color, alpha):
    r, g, b = INK[color]
    at = (y * width + x) * 4
    buf[at] = b
    buf[at + 1] = g
    buf[at + 2] = r
    buf[at + 3] = alpha


def cell(atlas, index, buf, width, ox, oy, color, crisp):
    size = atlas.size
    coverage = shaped_alpha(atlas, index, size, crisp)
    for y in range(size):
        for x in range(size):
            alpha = coverage[y * size + x]
            if alpha:
                put(buf, width, ox + x, oy + y, color, alpha)


def toolbar_sheet(size, crisp):
    atlas = Atlas(size)
    width = len(TOOLBAR) * size
    buf = bytearray(width * size * 4)
    for number, (name, color) in enumerate(TOOLBAR):
        cell(atlas, NAMES.index(name), buf, width, number * size, 0, color, crisp)
    return width, size, buf


def output_strip(crisp):
    atlas = Atlas(16)
    width = len(OUTPUT) * 16
    buf = bytearray(width * 16 * 4)
    for number, (name, color) in enumerate(OUTPUT):
        cell(atlas, NAMES.index(name), buf, width, number * 16, 0, color, crisp)
    return width, 16, buf


def tabstrip(crisp):
    atlas = Atlas(16)
    width = 1200
    buf = bytearray(width * 16 * 4)
    index = NAMES.index("x")
    for number, color in ((22, HOVER), (56, PLAIN), (57, PRESS)):
        cell(atlas, index, buf, width, number * 16, 0, color, crisp)
    return width, 16, buf


def write_bmp(path, width, height, px):
    stride = width * 4
    data = bytearray()
    for y in range(height - 1, -1, -1):
        data += px[y * stride:(y + 1) * stride]
    size = 54 + len(data)
    head = b"BM" + struct.pack("<IHHI", size, 0, 0, 54)
    info = struct.pack("<IiiHHIIiiII", 40, width, height, 1, 32, 0, len(data), 2835, 2835, 0, 0)
    with open(path, "wb") as f:
        f.write(head + info + data)


def main():
    global INK
    os.makedirs(SPRITES, exist_ok=True)
    dark = os.path.join(SPRITES, "dark")
    os.makedirs(dark, exist_ok=True)
    for directory, inks in ((SPRITES, LIGHT), (dark, DARK)):
        # put() resolves each ink key through INK as it lays the glyph down,
        # so the two passes share every atlas read and every cell position.
        # Only the light sheets get the harder edges; the dark ones keep the
        # coverage exactly as the atlas exports it.
        INK = inks
        crisp = directory == SPRITES
        write_bmp(os.path.join(directory, "tabstrip16.bmp"), *tabstrip(crisp))
        write_bmp(os.path.join(directory, "toolbar16.bmp"), *toolbar_sheet(16, crisp))
        write_bmp(os.path.join(directory, "toolbar24.bmp"), *toolbar_sheet(24, crisp))
        write_bmp(os.path.join(directory, "toolbar32.bmp"), *toolbar_sheet(32, crisp))
        write_bmp(os.path.join(directory, "output16.bmp"), *output_strip(crisp))
    print("wrote 10 sprite sheets (light + dark)")


if __name__ == "__main__":
    main()
