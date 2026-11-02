"""Duotone public-domain paintings to Heed ultramarine on cream (docs/brand/CREDITS.md lists them).

    python3 docs/brand/tools/duotone.py IN.jpg OUT.webp [--width 1400]

Shadows map to signal deep (#070798), mid-tones pass through signal (#1F3BFF), highlights to
canvas cream (#F6F3EC). The result is a WebP under 250 KB.
"""
import io
import sys

from PIL import Image, ImageOps

STOPS = [(0.0, (7, 7, 152)), (0.45, (31, 59, 255)), (1.0, (246, 243, 236))]


def lut(channel):
    out = []
    for i in range(256):
        t = i / 255
        for (a, ca), (b, cb) in zip(STOPS, STOPS[1:]):
            if a <= t <= b:
                k = (t - a) / (b - a)
                out.append(round(ca[channel] + (cb[channel] - ca[channel]) * k))
                break
    return out


def duotone(src, dst, width=1400, budget=250_000):
    im = Image.open(src).convert("L")
    # Trim the frame edge some museum photographs keep.
    m = round(min(im.size) * 0.02)
    im = im.crop((m, m, im.width - m, im.height - m))
    im = ImageOps.autocontrast(im, cutoff=1)
    if im.width > width:
        im = im.resize((width, round(im.height * width / im.width)), Image.LANCZOS)
    rgb = Image.merge("RGB", [im.point(lut(c)) for c in range(3)])
    for q in (80, 72, 64, 56, 48, 40):
        buf = io.BytesIO()
        rgb.save(buf, "WEBP", quality=q, method=6)
        if buf.tell() <= budget:
            break
    open(dst, "wb").write(buf.getvalue())
    return buf.tell()


if __name__ == "__main__":
    w = int(sys.argv[sys.argv.index("--width") + 1]) if "--width" in sys.argv else 1400
    print(duotone(sys.argv[1], sys.argv[2], w))
