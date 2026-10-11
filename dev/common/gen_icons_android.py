#!/usr/bin/env python3
"""Adaptive-icon safe-zone fix for the Android icons produced by `tauri icon`.

Why: `tauri icon` renders the master full-bleed into ic_launcher_foreground.png,
but an Android adaptive icon is masked to a circle/squircle and only guarantees
the central 66dp as visible. A full-bleed foreground therefore gets its edges
clipped by the launcher, the mark looks oversized, and (with the default white
background) the cropped corners show white. This post-processor:

  1. scales each mipmap-*/ic_launcher_foreground.png down into the safe zone,
  2. sets values/ic_launcher_background.xml to the brand color so the masked
     ring is brand-colored instead of the scaffold default white.

It is invoked by dev/common/gen_icons.(ps1|sh) right after `tauri icon` and
before the icons are synced into the checked-out gen res. Requires Pillow.
"""
import os
import re
import sys

from PIL import Image

# 0.70 brings the mark's bounding half-diagonal (43.9% of canvas) inside the
# 66dp adaptive safe circle (30.6% of canvas). #F3B93B == rgb(243,185,59), the
# brand yellow sampled from the source chip.
FG_SCALE = 0.70
BG_COLOR = "#F3B93B"


def fix(android_dir: str) -> int:
    scaled = 0
    for entry in sorted(os.listdir(android_dir)):
        if not entry.startswith("mipmap-"):
            continue
        fg = os.path.join(android_dir, entry, "ic_launcher_foreground.png")
        if not os.path.isfile(fg):
            continue
        im = Image.open(fg).convert("RGBA")
        w, h = im.size
        nw, nh = round(w * FG_SCALE), round(h * FG_SCALE)
        small = im.resize((nw, nh), Image.LANCZOS)
        canvas = Image.new("RGBA", (w, h), (0, 0, 0, 0))
        canvas.paste(small, ((w - nw) // 2, (h - nh) // 2), small)
        canvas.save(fg)
        scaled += 1

    bg = os.path.join(android_dir, "values", "ic_launcher_background.xml")
    if os.path.isfile(bg):
        with open(bg, encoding="utf-8") as fh:
            txt = fh.read()
        txt = re.sub(
            r'(<color name="ic_launcher_background">)[^<]*(</color>)',
            r"\g<1>%s\g<2>" % BG_COLOR,
            txt,
        )
        with open(bg, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(txt)

    print(f"  [android] {scaled} foreground(s) scaled to {FG_SCALE}; background -> {BG_COLOR}")
    return scaled


if __name__ == "__main__":
    if len(sys.argv) != 2 or not os.path.isdir(sys.argv[1]):
        print("usage: gen_icons_android.py <icons/android dir>", file=sys.stderr)
        sys.exit(2)
    fix(sys.argv[1])
