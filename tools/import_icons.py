#!/usr/bin/env python3
"""Import the icon atlas that make_assets.py composes the sheets from.

Downloads the Tabler Icons exports pinned below, keeps their alpha mask and
packs one RGBA atlas per cell size into `crates/app/assets/icons/`. This is an
import step, not part of a normal build: the atlases are committed, and only
`make_assets.py` runs when the sheets are regenerated. Needs Pillow and network
access.

    python tools/import_icons.py
"""

import io
import os
import urllib.request

from PIL import Image

import make_assets

VERSION = "3.50.0"
BASE = f"https://cdn.jsdelivr.net/npm/@tabler/icons-png@{VERSION}/icons"
SIZES = (16, 24, 32)

LICENSE = """MIT License

Copyright (c) 2020-2026 Paweł Kuna

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""


def fetch(name):
    style = "filled" if name in make_assets.FILLED else "outline"
    url = f"{BASE}/{style}/{name}.png"
    request = urllib.request.Request(url, headers={"User-Agent": "dogedit-icon-import"})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return Image.open(io.BytesIO(response.read())).convert("RGBA")
        except OSError:
            if attempt == 2:
                raise
    raise AssertionError(name)


def main():
    os.makedirs(make_assets.ATLAS, exist_ok=True)
    masks = {}
    for number, name in enumerate(make_assets.NAMES, 1):
        source = fetch(name)
        if source.width != source.height:
            raise SystemExit(f"{name}: export is not square")
        masks[name] = source.getchannel("A")
        print(f"{number}/{len(make_assets.NAMES)} {name}")
    for size in SIZES:
        atlas = Image.new("RGBA", (size * len(make_assets.NAMES), size), (0, 0, 0, 0))
        for number, name in enumerate(make_assets.NAMES):
            glyph = Image.new("RGBA", (size, size), (0, 0, 0, 0))
            glyph.putalpha(masks[name].resize((size, size), Image.LANCZOS))
            atlas.paste(glyph, (number * size, 0))
        atlas.save(os.path.join(make_assets.ATLAS, f"icons-{size}.png"))
    with open(os.path.join(make_assets.ATLAS, "LICENSE"), "w", newline="\n", encoding="utf-8") as f:
        f.write(LICENSE)
    lines = [
        f"Tabler Icons {VERSION} — https://tabler.io/icons",
        f"Source: {BASE}/<style>/<name>.png",
        "Each atlas cell is one export scaled to the cell size with a Lanczos filter.",
        "Filled variants used: " + ", ".join(sorted(make_assets.FILLED)),
        "",
        "Icons:",
    ]
    lines += [f"{number:3d} {name}" for number, name in enumerate(make_assets.NAMES)]
    with open(os.path.join(make_assets.ATLAS, "SOURCES.txt"), "w", newline="\n", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    print(f"wrote {len(SIZES)} atlases, LICENSE and SOURCES.txt")


if __name__ == "__main__":
    main()
