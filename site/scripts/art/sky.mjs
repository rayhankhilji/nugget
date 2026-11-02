// Heed — dithered sky generator.
//
// Produces site/public/art/sky-light.webp and sky-dark.webp: a procedural
// cloud field (periodic gradient-noise fBm, implemented below) quantised to a
// small single-blue palette with an 8×8 ordered Bayer dither, like a
// risograph print. 1 image pixel = 1 dither cell; display it at 3 CSS px per
// cell with `image-rendering: pixelated`.
//
// The noise is periodic in x, so the image tiles seamlessly horizontally.
// Everything is deterministic (fixed seed, no randomness from the runtime).
//
// Run from site/: node scripts/art/sky.mjs

import sharp from "sharp";
import { mkdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "..", "..", "public", "art");
mkdirSync(outDir, { recursive: true });

// Logical size in dither cells. Width must be a multiple of 8 (Bayer period)
// for the seamless horizontal tile.
const W = 640;
const H = 360;
const TEXT = "#F6F3EC"; // text colour that must stay legible on the top 55%
const TEXT_REGION = 0.55;

// ---------------------------------------------------------------- colour --
const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
const lin = (c) => {
  c /= 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const luminance = ([r, g, b]) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
const contrast = (a, b) => {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
};

// ----------------------------------------------------------------- noise --
// Small deterministic PRNG + integer hash.
function mulberry32(seed) {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
function hash2(ix, iy, seed) {
  let h = Math.imul(ix, 0x27d4eb2d) ^ Math.imul(iy, 0x165667b1) ^ Math.imul(seed, 0x9e3779b1);
  h = Math.imul(h ^ (h >>> 15), 0x85ebca6b);
  h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
  return (h ^ (h >>> 16)) >>> 0;
}
const fade = (t) => t * t * t * (t * (t * 6 - 15) + 10);
const GRADS = Array.from({ length: 16 }, (_, i) => {
  const a = (i / 16) * Math.PI * 2;
  return [Math.cos(a), Math.sin(a)];
});

// 2-D gradient noise, periodic in x with integer period `px` (lattice cells).
function pnoise(x, y, px, seed) {
  const x0 = Math.floor(x);
  const y0 = Math.floor(y);
  const fx = x - x0;
  const fy = y - y0;
  const g = (ix, iy, dx, dy) => {
    const gr = GRADS[hash2(((ix % px) + px) % px, iy, seed) & 15];
    return gr[0] * dx + gr[1] * dy;
  };
  const n00 = g(x0, y0, fx, fy);
  const n10 = g(x0 + 1, y0, fx - 1, fy);
  const n01 = g(x0, y0 + 1, fx, fy - 1);
  const n11 = g(x0 + 1, y0 + 1, fx - 1, fy - 1);
  const u = fade(fx);
  const v = fade(fy);
  return (n00 + u * (n10 - n00) + v * (n01 - n00) + u * v * (n00 - n10 - n01 + n11)) * 1.41;
}

// fBm with u in [0,1) across the tile; base period `p` lattice cells.
function fbm(u, y, p, octaves, seed, gain = 0.5) {
  let sum = 0;
  let amp = 1;
  let norm = 0;
  let period = p;
  for (let o = 0; o < octaves; o++) {
    sum += amp * pnoise(u * period, y * period, period, seed + o * 101);
    norm += amp;
    amp *= gain;
    period *= 2;
  }
  return sum / norm;
}

const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const smooth = (a, b, v) => {
  const t = clamp((v - a) / (b - a));
  return t * t * (3 - 2 * t);
};
const lerp = (a, b, t) => a + (b - a) * t;
// Piecewise-linear curve through [y, value] stops.
function curve(stops, y) {
  if (y <= stops[0][0]) return stops[0][1];
  for (let i = 1; i < stops.length; i++) {
    if (y <= stops[i][0]) {
      const [y0, v0] = stops[i - 1];
      const [y1, v1] = stops[i];
      return lerp(v0, v1, smooth(0, 1, (y - y0) / (y1 - y0)));
    }
  }
  return stops[stops.length - 1][1];
}

// 8×8 Bayer matrix, thresholds in (0,1).
const BAYER = (() => {
  const m = [[0]];
  let n = 1;
  let cur = m;
  while (n < 8) {
    const next = Array.from({ length: n * 2 }, () => new Array(n * 2));
    for (let y = 0; y < n; y++)
      for (let x = 0; x < n; x++) {
        const v = cur[y][x] * 4;
        next[y][x] = v;
        next[y][x + n] = v + 2;
        next[y + n][x] = v + 3;
        next[y + n][x + n] = v + 1;
      }
    cur = next;
    n *= 2;
  }
  return cur.map((row) => row.map((v) => (v + 0.5) / 64));
})();

// ---------------------------------------------------------------- clouds --
// Cumulus are built as metaballs: each cloud is a cluster of soft elliptical
// "puffs" sitting on a flat base line, which gives rounded cauliflower tops
// and flat undersides that read as clouds rather than noise. Layers go from
// far (small, low, near the horizon, hazy) to near (large, higher).
// Puffs wrap around in x so the field is periodic.

const LAYERS = [
  // base: y of the flat underside (0..1); r: puff radius range in cells;
  // count: clouds in the tile; rise: extra height of the cloud's crown;
  // span: half-width in puff radii; haze: 0 = full contrast, 1 = melts away.
  { base: 0.958, r: [3, 6], count: 20, span: [2.5, 4.5], rise: 0.012, haze: 0.7 },
  { base: 0.915, r: [5, 9], count: 12, span: [2.4, 4.2], rise: 0.025, haze: 0.45 },
  { base: 0.86, r: [7, 13], count: 8, span: [2.2, 3.8], rise: 0.045, haze: 0.2 },
  { base: 0.795, r: [10, 18], count: 5, span: [2.0, 3.4], rise: 0.07, haze: 0.0 },
];

function buildLayer(layer, seed, idx) {
  const rnd = mulberry32(seed * 977 + idx * 131);
  const F = new Float32Array(W * H);
  const baseY = layer.base * (H - 1);
  // Spread clouds evenly-ish with jitter so gaps stay calm.
  for (let c = 0; c < layer.count; c++) {
    const cx = ((c + rnd() * 0.95) / layer.count) * W;
    const span = lerp(layer.span[0], layer.span[1], rnd()) * layer.r[1]; // cloud half-width
    const rMax = lerp(layer.r[0], layer.r[1], 0.55 + 0.45 * rnd());
    // 1) a skirt of small puffs along the flat base,
    const skirt = Math.ceil((2 * span) / (rMax * 0.55)) + 1;
    for (let p = 0; p < skirt; p++) {
      const k = (p + 0.5) / skirt;
      const r = rMax * lerp(0.45, 0.65, rnd());
      addPuff(F, cx + (k * 2 - 1) * span, baseY - r * 0.6, r * 1.5, r * 0.8);
    }
    // 2) a heaped body of larger puffs, tallest near the middle,
    const body = Math.ceil((2 * span * 0.62) / (rMax * 0.6)) + 1;
    const crownAt = lerp(0.3, 0.7, rnd()); // the tallest heap is off-centre
    for (let p = 0; p < body; p++) {
      const k = (p + 0.5) / body;
      const dome = Math.exp(-(((k - crownAt) / 0.38) ** 2));
      const r = rMax * lerp(0.6, 1.0, dome) * (0.85 + 0.25 * rnd());
      const px = cx + (k * 2 - 1) * span * 0.62 + (rnd() - 0.5) * r * 0.4;
      const py = baseY - r * 0.9 - dome * layer.rise * H * (0.7 + 0.4 * rnd());
      addPuff(F, px, py, r * 1.12, r);
      // 3) cauliflower crowns on the upper heaps.
      {
        const crowns = 1 + Math.floor(rnd() * (dome > 0.45 ? 3 : 2));
        for (let q = 0; q < crowns; q++) {
          const r2 = r * lerp(0.3, 0.55, rnd());
          addPuff(F, px + (rnd() - 0.5) * r * 1.6, py - r * lerp(0.45, 0.8, rnd()), r2 * 1.1, r2);
        }
      }
    }
  }
  // Flat bases: fade density out just under the base line.
  for (let y = 0; y < H; y++) {
    const m = 1 - smooth(baseY - 1.5, baseY + 2.5, y);
    if (m === 1) continue;
    for (let x = 0; x < W; x++) F[y * W + x] *= m;
  }
  return F;
}

function addPuff(F, px, py, rx, ry) {
  const x0 = Math.floor(px - rx * 2.6);
  const x1 = Math.ceil(px + rx * 2.6);
  const y0 = Math.max(0, Math.floor(py - ry * 2.6));
  const y1 = Math.min(H - 1, Math.ceil(py + ry * 2.6));
  for (let y = y0; y <= y1; y++) {
    const dy = (y - py) / ry;
    for (let xi = x0; xi <= x1; xi++) {
      const dx = (xi - px) / rx;
      const d2 = dx * dx + dy * dy;
      if (d2 > 6.8) continue;
      const x = ((xi % W) + W) % W;
      F[y * W + x] += Math.exp(-d2 * 1.1);
    }
  }
}

// ------------------------------------------------------------- variants --
const VARIANTS = {
  light: {
    file: "sky-light.webp",
    seed: 7,
    // Palette, darkest → lightest; tone value i maps to palette[i].
    palette: ["#070798", "#1F3BFF", "#5A6CFF", "#A3ADFF", "#E8E3FF", "#F6F3EC"],
    // Base sky tone by height (0 = top, 1 = bottom).
    sky: [
      [0.0, 0.0],
      [0.12, 0.0],
      [0.55, 0.98],
      [0.7, 1.7],
      [0.84, 2.7],
      [0.93, 3.7],
      [0.985, 5.0],
    ],
    textCap: 1, // max palette index allowed in the text region
    lit: 5.0, // sunlit cloud tops
    hazeLift: 0.35, // distant clouds drift toward this share of the lit tone
    shadow: 3.4, // shaded flanks
    under: 2.5, // flat undersides
    wisp: 0.5, // high cirrus brightness boost
    floor: 5, // palette index forced in the last rows
  },
  dark: {
    file: "sky-dark.webp",
    seed: 7,
    palette: ["#0B0B1A", "#070798", "#1F3BFF", "#5A6CFF"],
    sky: [
      [0.0, 0.0],
      [0.1, 0.0],
      [0.55, 0.9],
      [0.75, 1.15],
      [0.9, 0.7],
      [0.985, 0.0],
    ],
    textCap: 2,
    lit: 2.35,
    hazeLift: 0.08,
    shadow: 1.55,
    under: 0.85,
    wisp: 0.45,
    floor: 0,
    stars: true,
  },
};

function render(cfg) {
  const pal = cfg.palette.map(hex);
  const N = pal.length - 1;
  const textRGB = hex(TEXT);
  const textRows = Math.ceil(H * TEXT_REGION);
  const tone = new Float32Array(W * H);

  // Sky gradient + faint high cirrus streaks (dithered within the text cap).
  for (let y = 0; y < H; y++) {
    const v = y / (H - 1);
    const base = curve(cfg.sky, v);
    const band = smooth(0.14, 0.3, v) * (1 - smooth(0.4, 0.56, v));
    for (let x = 0; x < W; x++) {
      const u = x / W;
      let wisp = 0;
      if (band > 0) {
        const n = fbm(u + fbm(u, v * 3, 2, 2, cfg.seed + 40) * 0.08, v * 14, 3, 3, cfg.seed + 500, 0.45);
        wisp = smooth(0.12, 0.42, n) * band;
      }
      tone[y * W + x] = base + wisp * cfg.wisp;
    }
  }

  // Cloud layers, far to near. Light comes from above and slightly left.
  const fieldAt = (F, x, y) => F[Math.max(0, Math.min(H - 1, y)) * W + (((x % W) + W) % W)];
  LAYERS.forEach((layer, li) => {
    const F = buildLayer(layer, cfg.seed, li);
    const baseY = layer.base * (H - 1);
    const reach = Math.max(2, Math.round(layer.r[1] * 0.35));
    // Per-column top of the layer, for a vertical light→shade gradient.
    const top = new Float32Array(W).fill(baseY);
    for (let x = 0; x < W; x++) {
      for (let y = 0; y < baseY; y++) {
        if (F[y * W + x] > 0.6) {
          top[x] = y;
          break;
        }
      }
    }
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const f = F[y * W + x];
        if (f < 0.25) continue;
        // Edge noise keeps silhouettes from looking like perfect circles.
        const e =
          pnoise((x / W) * 40, (y / H) * 26, 40, cfg.seed + 77 + li) * 0.1 +
          pnoise((x / W) * 96, (y / H) * 60, 96, cfg.seed + 88 + li) * 0.06;
        const alpha = smooth(0.55, 0.7, f + e);
        if (alpha <= 0) continue;
        // Height within this column of cloud: 0 at the crown, 1 at the base.
        const span = Math.max(4, baseY - top[x]);
        const h = clamp((y - top[x]) / span);
        // Self-shadowing from the upper-left.
        const occ = fieldAt(F, x - reach, y - reach);
        const side = smooth(0.6, 1.6, occ);
        let t = lerp(cfg.lit, cfg.shadow, smooth(0.25, 0.8, h));
        t = lerp(t, cfg.under, smooth(0.78, 1.0, h));
        t -= side * (cfg.lit - cfg.shadow) * 0.35 * (1 - h);
        // Atmospheric haze toward the sky tone.
        const k = y * W + x;
        t = lerp(t, tone[k] + (cfg.lit - tone[k]) * cfg.hazeLift, layer.haze);
        tone[k] = lerp(tone[k], t, alpha);
      }
    }
  });

  // Melt everything into the page colour along the bottom edge.
  for (let y = 0; y < H; y++) {
    const m = smooth(0.9, 0.99, y / (H - 1));
    if (m === 0) continue;
    for (let x = 0; x < W; x++) tone[y * W + x] = lerp(tone[y * W + x], cfg.floor, m);
  }

  // Sparse stars for the night sky: single cells, deterministic, never above
  // the text cap so contrast is preserved.
  if (cfg.stars) {
    const rnd = mulberry32(cfg.seed * 31 + 3);
    for (let i = 0; i < 60; i++) {
      const x = Math.floor(rnd() * W);
      const y = Math.floor(Math.pow(rnd(), 1.3) * H * 0.5);
      const k = y * W + x;
      if (tone[k] < 0.5) tone[k] = 2;
    }
  }

  const px = Buffer.alloc(W * H * 3);
  let worst = Infinity;
  for (let y = 0; y < H; y++) {
    const inText = y < textRows;
    const isFloor = y >= H - 3;
    for (let x = 0; x < W; x++) {
      let t = clamp(tone[y * W + x], 0, N);
      if (inText) t = Math.min(t, cfg.textCap);
      let i = Math.floor(t);
      const f = t - i;
      if (i < N && f > BAYER[y & 7][x & 7]) i++;
      if (inText) i = Math.min(i, cfg.textCap);
      if (isFloor) i = cfg.floor;
      const c = pal[i];
      const k = (y * W + x) * 3;
      px[k] = c[0];
      px[k + 1] = c[1];
      px[k + 2] = c[2];
      if (inText) worst = Math.min(worst, contrast(textRGB, c));
    }
  }
  return { px, worst, textRows };
}

for (const [name, cfg] of Object.entries(VARIANTS)) {
  const { px, worst, textRows } = render(cfg);
  const out = join(outDir, cfg.file);
  await sharp(px, { raw: { width: W, height: H, channels: 3 } })
    .webp({ lossless: true, effort: 6 })
    .toFile(out);
  const kb = (statSync(out).size / 1024).toFixed(1);
  console.log(
    `${name.padEnd(5)} ${cfg.file}  ${W}×${H} cells  ${kb} KB  ` +
      `worst-case ${TEXT} contrast in top ${Math.round(TEXT_REGION * 100)}% (rows 0–${textRows - 1}): ${worst.toFixed(2)}:1`,
  );
}
