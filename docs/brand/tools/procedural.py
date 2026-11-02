"""Heed's original procedural imagery (Apache-2.0): an ultramarine dithered landscape, an
engraved (line-hatched) landscape, and a soft generated sky.

    python3 docs/brand/tools/procedural.py apps/desktop/src/brand/img

Everything is drawn from seeded value noise, so the output is reproducible. Needs numpy and Pillow.
"""
import io
import os
import sys

import numpy as np
from PIL import Image

SIGNAL = (31, 59, 255)
DEEP = (7, 7, 152)
CREAM = (246, 243, 236)


def value_noise(w, h, scale, rng, octaves=5):
    """Fractal value noise in [0, 1], `scale` px per base cell."""
    out = np.zeros((h, w))
    amp, total = 1.0, 0.0
    for o in range(octaves):
        s = max(2, int(scale / (2**o)))
        gw, gh = w // s + 2, h // s + 2
        grid = rng.random((gh, gw))
        ys, xs = np.mgrid[0:h, 0:w] / s
        x0, y0 = xs.astype(int), ys.astype(int)
        fx, fy = xs - x0, ys - y0
        fx, fy = fx * fx * (3 - 2 * fx), fy * fy * (3 - 2 * fy)
        a, b = grid[y0, x0], grid[y0, x0 + 1]
        c, d = grid[y0 + 1, x0], grid[y0 + 1, x0 + 1]
        out += amp * ((a * (1 - fx) + b * fx) * (1 - fy) + (c * (1 - fx) + d * fx) * fy)
        total += amp
        amp *= 0.5
    return out / total


def ridge_line(w, rng, base, height, scale):
    """A mountain ridge: y per column."""
    n = value_noise(w, 1, scale, rng, octaves=6)[0]
    return base - height * (n - n.min()) / (np.ptp(n) or 1)


def landscape(w, h, seed=7, clouds=True):
    """Luminance in [0, 1] (1 = bright sky): a sky gradient and four mountain ranges with haze."""
    rng = np.random.default_rng(seed)
    ys = np.mgrid[0:h, 0:w][0] / h
    img = 0.92 - 0.25 * (1 - ys)  # sky: brighter near the horizon
    cloud = value_noise(w, h, 260, rng)
    img += (0.10 if clouds else 0.0) * (cloud - 0.5) * (ys < 0.5)
    ranges = [(0.48, 0.30, 380, 0.70), (0.62, 0.26, 300, 0.52), (0.76, 0.20, 220, 0.34), (0.90, 0.16, 160, 0.16)]
    for base, height, scale, tone in ranges:
        ridge = ridge_line(w, rng, base * h, height * h, scale)
        mask = np.mgrid[0:h, 0:w][0] >= ridge[None, :]
        tex = value_noise(w, h, 40, rng, octaves=3)
        img = np.where(mask, tone + 0.10 * (tex - 0.5), img)
    return np.clip(img, 0, 1)


BAYER8 = np.array(
    [[0, 32, 8, 40, 2, 34, 10, 42], [48, 16, 56, 24, 50, 18, 58, 26], [12, 44, 4, 36, 14, 46, 6, 38], [60, 28, 52, 20, 62, 30, 54, 22],
     [3, 35, 11, 43, 1, 33, 9, 41], [51, 19, 59, 27, 49, 17, 57, 25], [15, 47, 7, 39, 13, 45, 5, 37], [63, 31, 55, 23, 61, 29, 53, 21]]
) / 64.0


def dither(lum, cell=2):
    """Ordered (Bayer) dither to two colours, each threshold cell `cell` px."""
    h, w = lum.shape
    t = np.kron(np.tile(BAYER8, (h // (8 * cell) + 1, w // (8 * cell) + 1)), np.ones((cell, cell)))[:h, :w]
    on = lum > t
    rgb = np.where(on[..., None], CREAM, SIGNAL).astype(np.uint8)
    return Image.fromarray(rgb, "RGB")


def engraving(lum, pitch=6):
    """Horizontal engraving lines whose thickness follows darkness."""
    h, w = lum.shape
    ys = np.mgrid[0:h, 0:w][0]
    phase = (ys % pitch) / pitch  # 0..1 within each line
    wobble = 0.03 * np.sin(np.mgrid[0:h, 0:w][1] / 37.0 + ys / 23.0)
    thickness = 0.14 + 0.78 * np.clip(1 - lum + wobble, 0, 1) ** 1.3
    ink = np.abs(phase - 0.5) * 2 < thickness
    rgb = np.where(ink[..., None], DEEP, CREAM).astype(np.uint8)
    return Image.fromarray(rgb, "RGB")


def sky(w, h, seed=3):
    """A soft daylight sky: ultramarine at the top to warm cream at the horizon, with clouds."""
    rng = np.random.default_rng(seed)
    ys = (np.mgrid[0:h, 0:w][0] / h)[..., None]
    top, mid, bottom = np.array([70, 96, 255]), np.array([170, 190, 255]), np.array([246, 236, 222])
    col = np.where(ys < 0.55, top + (mid - top) * (ys / 0.55), mid + (bottom - mid) * ((ys - 0.55) / 0.45))
    c = value_noise(w, h, 300, rng, octaves=6)
    cloud = np.clip((c - 0.52) * 3.2, 0, 1)[..., None] * (1 - ys * 0.6)
    col = col * (1 - cloud * 0.75) + np.array([255, 255, 255]) * cloud * 0.75
    return Image.fromarray(np.clip(col, 0, 255).astype(np.uint8), "RGB")


def save(im, path, budget=250_000):
    if path.endswith(".png"):
        im.convert("P", palette=Image.ADAPTIVE, colors=2 if "dither" in path or "engrav" in path else 64).save(path, optimize=True)
        return os.path.getsize(path)
    for q in (82, 74, 66, 58, 50):
        buf = io.BytesIO()
        im.save(buf, "WEBP", quality=q, method=6)
        if buf.tell() <= budget:
            break
    open(path, "wb").write(buf.getvalue())
    return buf.tell()


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "."
    os.makedirs(out, exist_ok=True)
    lum = landscape(1600, 640)
    print("dither", save(dither(lum), os.path.join(out, "dither-landscape.png")))
    print("engraving", save(engraving(landscape(1600, 640, seed=11, clouds=False)), os.path.join(out, "engraving-landscape.png")))
    print("sky", save(sky(1600, 1000), os.path.join(out, "sky.webp")))
