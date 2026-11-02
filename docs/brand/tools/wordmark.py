"""Heed's wordmark (Apache-2.0): "heed" in Fraunces, outlined so the logo needs no font.

    python3 docs/brand/tools/wordmark.py path/to/fraunces-latin-full-normal.woff2

Writes docs/brand/assets/heed-wordmark{,-light,-mono}.svg and the WORDMARK constants in
apps/desktop/src/brand/marks.ts. The "h" is Fraunces' WONK alternate (h.alt, the leaning h with
the hooked leg), leaned a further 8 degrees: the WONK axis only swaps it in through GSUB, so it is picked here by name.
Needs fontTools.
"""
import math
import re
import sys

from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

ROOT = __file__.rsplit("/docs/", 1)[0]
ASSETS = f"{ROOT}/docs/brand/assets/"
MARKS = f"{ROOT}/apps/desktop/src/brand/marks.ts"
WONK = {"h": "h.alt", "n": "n.alt", "m": "m.alt"}
LEAN = 8  # degrees: the logo's "h" leans forward a touch more than the font's, for the logo only


def wordmark(font_path, text="heed", size=100, tracking=-0.03):
    font = instancer.instantiateVariableFont(TTFont(font_path), {"wght": 380, "opsz": 144, "SOFT": 100, "WONK": 1})
    gs, cmap = font.getGlyphSet(), font.getBestCmap()
    s = size / font["head"].unitsPerEm
    pen, bounds, x = SVGPathPen(gs), BoundsPen(gs), 0.0
    for ch in text:
        g = WONK.get(ch) if WONK.get(ch) in gs else cmap[ord(ch)]
        k = s * math.tan(math.radians(LEAN)) if ch == "h" else 0
        t = (s, 0, k, -s, x, size)
        gs[g].draw(TransformPen(pen, t))
        gs[g].draw(TransformPen(bounds, t))
        x += gs[g].width * s + tracking * size
    x0, y0, x1, y1 = bounds.bounds
    pad = 2
    d = re.sub(r"\d+\.\d{3,}", lambda m: f"{float(m.group()):.2f}".rstrip("0").rstrip("."), pen.getCommands())
    return d, f"{x0 - pad:.1f} {y0 - pad:.1f} {x1 - x0 + 2 * pad:.1f} {y1 - y0 + 2 * pad:.1f}"


if __name__ == "__main__":
    d, vb = wordmark(sys.argv[1])
    for name, fill in [("heed-wordmark.svg", "#14110F"), ("heed-wordmark-light.svg", "#F6F3EC"), ("heed-wordmark-mono.svg", "currentColor")]:
        with open(ASSETS + name, "w") as f:
            f.write(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb}" role="img" aria-label="heed"><path fill="{fill}" d="{d}"/></svg>\n')
    ts = open(MARKS).read()
    ts = re.sub(r'(export const WORDMARK_VIEWBOX = )"[^"]*"', lambda m: f'{m.group(1)}"{vb}"', ts)
    ts = re.sub(r'(export const WORDMARK = )"[^"]*"', lambda m: f'{m.group(1)}"{d}"', ts)
    open(MARKS, "w").write(ts)
    print("viewBox", vb, "path", len(d), "chars")
