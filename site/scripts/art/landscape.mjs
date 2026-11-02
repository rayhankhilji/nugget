// Heed — engraved landscape generator.
//
// Produces site/public/art/landscape-light.webp and landscape-dark.webp: a
// banknote/etching-style landscape drawn only with hatching. Every stroke is
// a ribbon whose width varies along its length, so tone is carried purely by
// line weight and spacing, in a single ink colour. The scene is built as an
// SVG string and rasterised with sharp.
//
// Deterministic: fixed seeds, no network, no external assets.
// Run from site/: node scripts/art/landscape.mjs

import sharp from "sharp";
import { mkdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "..", "..", "public", "art");
mkdirSync(outDir, { recursive: true });

const W = 2400;
const H = 640;
const STEP = 6; // x sampling step for every stroke, in px

const VARIANTS = {
  light: { file: "landscape-light.webp", bg: "#F6F3EC", ink: "#1F3BFF", deep: "#070798", weight: 1, sky: 1, night: false },
  // On a dark ground light ink looks heavier, so strokes are trimmed a touch.
  dark: { file: "landscape-dark.webp", bg: "#0B0B1A", ink: "#5A6CFF", deep: "#5A6CFF", weight: 0.8, sky: 0.6, night: true },
};

// ------------------------------------------------------------- helpers --
const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const lerp = (a, b, t) => a + (b - a) * t;
const smooth = (a, b, v) => {
  const t = clamp((v - a) / (b - a));
  return t * t * (3 - 2 * t);
};
function mulberry32(seed) {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
// 1-D smooth value noise with fBm, values roughly in [-1, 1].
function noise1D(seed) {
  const rnd = mulberry32(seed);
  const table = Array.from({ length: 512 }, () => rnd() * 2 - 1);
  const n = (x) => {
    const i = Math.floor(x);
    const f = x - i;
    const u = f * f * f * (f * (f * 6 - 15) + 10);
    return lerp(table[i & 511], table[(i + 1) & 511], u);
  };
  return (x, octaves = 4) => {
    let s = 0;
    let a = 1;
    let norm = 0;
    let fx = x;
    for (let o = 0; o < octaves; o++) {
      s += a * n(fx + o * 37.1);
      norm += a;
      a *= 0.5;
      fx *= 2.03;
    }
    return s / norm;
  };
}
const f1 = (v) => Math.round(v * 10) / 10;

// A ribbon: a polyline of [x, y, width]. Width ≤ MIN breaks the stroke, so a
// line can taper to nothing and resume — the engraver's "lifted burin".
const MIN_W = 0.18;
function ribbon(pts) {
  let d = "";
  let run = [];
  const flush = () => {
    if (run.length >= 2) {
      const top = run.map(([x, y, w]) => `${f1(x)},${f1(y - w / 2)}`);
      const bot = run.map(([x, y, w]) => `${f1(x)},${f1(y + w / 2)}`).reverse();
      d += `M${top.join("L")}L${bot.join("L")}Z`;
    }
    run = [];
  };
  for (const p of pts) {
    if (p[2] > MIN_W) run.push(p);
    else {
      if (run.length) run.push([p[0], p[1], 0]);
      flush();
    }
  }
  flush();
  return d;
}

// ---------------------------------------------------------------- scene --
const SUN = { x: 1660, y: 338, r: 70 };
const HORIZON = 392; // the far shore / lake line

// Ridge profiles, back to front. `top(x)` is the silhouette height (y).
function makeRidge({ seed, base, amp, freq, oct = 5, lift = () => 0 }) {
  const n = noise1D(seed);
  return (x) => base - amp * (0.5 + 0.5 * n(x * freq, oct)) - lift(x);
}
const ridges = [
  // Farthest range: fine, close-set rules, so it reads pale and airy.
  {
    top: makeRidge({
      seed: 11,
      base: HORIZON,
      amp: 60,
      freq: 1 / 230,
      lift: (x) => 95 * Math.exp(-(((x - 640) / 330) ** 2)) + 55 * Math.exp(-(((x - 1250) / 260) ** 2)),
    }),
    gap: 3.3,
    w: [0.3, 0.75],
  },
  // Middle range, a little heavier.
  {
    top: makeRidge({
      seed: 23,
      base: HORIZON,
      amp: 38,
      freq: 1 / 170,
      lift: (x) => 48 * Math.exp(-(((x - 1020) / 300) ** 2)) + 62 * Math.exp(-(((x - 2120) / 260) ** 2)) - 20,
    }),
    gap: 3.6,
    w: [0.42, 1.0],
  },
  // Nearest of the far ranges: low foothills on the shore, the sun sets
  // into their notch.
  {
    top: makeRidge({
      seed: 37,
      base: HORIZON,
      amp: 22,
      freq: 1 / 120,
      lift: (x) => 30 * Math.exp(-(((x - 300) / 260) ** 2)) + 34 * Math.exp(-(((x - 1930) / 200) ** 2)) - 6,
    }),
    gap: 3.9,
    w: [0.55, 1.25],
  },
];

// Near hills rise from the bottom on both sides, framing the lake.
const nearN = noise1D(51);
const nearTopLeft = (x) => {
  const t = x / W;
  const shape = 330 + 330 * smooth(0.04, 0.6, t) + 22 * Math.sin(t * 9.5);
  return shape + 30 * nearN(x / 220, 4);
};
const farN = noise1D(67);
const nearTopRight = (x) => {
  const t = x / W;
  const shape = 640 - 210 * smooth(0.52, 0.98, t) + 14 * Math.sin(t * 13 + 1);
  return shape + 12 * farN(x / 120, 4);
};
const midN = noise1D(79);
const midHill = (x) => {
  // A long low shoulder across the middle distance.
  const t = x / W;
  return 540 - 70 * Math.exp(-(((t - 0.33) / 0.18) ** 2)) - 36 * Math.exp(-(((t - 0.86) / 0.1) ** 2)) + 8 * midN(x / 110, 4);
};

function slopeAt(f, x) {
  return (f(x + 3) - f(x - 3)) / 6;
}

// Contour hatching for a hill: lines follow the silhouette and relax toward
// horizontal as they go down, spacing opening up with depth (perspective).
function hatchHill(top, { gap, w, floor = H + 10, depthGrow = 0.012, light = 1, flatten = 160, seed = 1 }) {
  const jit = noise1D(seed);
  let d = "";
  let offset = 1.6;
  let j = 0;
  while (offset < H) {
    const pts = [];
    const relax = smooth(0, flatten, offset);
    for (let x = -STEP; x <= W + STEP; x += STEP) {
      const t0 = top(x);
      // Flatten the undulation as lines go deeper into the hill.
      const local = lerp(t0, (top(x - 60) + t0 + top(x + 60)) / 3, relax * 0.8);
      const y = local + offset;
      if (y > floor || y > H + 4) {
        pts.push([x, y, 0]);
        continue;
      }
      const s = slopeAt(top, x);
      // Light from the sun (upper right): right-facing flanks are lighter.
      const facing = Math.tanh(s * 2.2); // >0 faces the sun
      const sunSide = Math.exp(-(((x - SUN.x) / 700) ** 2));
      let tone = 0.55 - 0.32 * facing * light - 0.15 * sunSide * light;
      // Darker deeper in the hill, and just below the crest line (an
      // engraver's crisp edge).
      tone += 0.25 * smooth(0, 220, offset) + 0.3 * Math.exp(-offset / 3.5);
      tone += 0.08 * jit(x / 40 + j * 1.7, 2);
      const width = lerp(w[0], w[1], clamp(tone));
      pts.push([x, y, width]);
    }
    d += ribbon(pts);
    offset += gap * (1 + depthGrow * offset);
    j++;
  }
  return d;
}

function silhouette(top, bottom = H + 20) {
  let d = `M-20,${bottom}`;
  for (let x = -20; x <= W + 20; x += STEP) d += `L${x},${f1(top(x))}`;
  return d + `L${W + 20},${bottom}Z`;
}

// Sky: horizontal rules thickening toward the horizon, opening into a halo
// round the sun and vanishing toward the top edge.
function hatchSky(k = 1) {
  const n = noise1D(5);
  let d = "";
  for (let y = 8; y < HORIZON + 4; y += 5.4) {
    const pts = [];
    for (let x = -STEP; x <= W + STEP; x += STEP) {
      const dist = Math.hypot((x - SUN.x) / 1.15, y - SUN.y);
      const halo = smooth(SUN.r * 1.2, SUN.r * 4.4, dist); // 0 near the sun
      // Tone: dissolves at the top edge, deepest in the upper sky, then
      // brightens into a glow along the horizon so the ridges stand out.
      const tone =
        smooth(20, 210, y) * (1 - 0.62 * smooth(150, HORIZON - 10, y));
      // Soft streaks of cloud; near the top they break the rules apart.
      const streak = 0.55 + 0.45 * n(x / 300 + y * 0.021, 3) + 0.25 * n(x / 80 - y * 0.05, 2);
      const w = 1.45 * k * tone * halo * streak - 0.12 * (1 - smooth(60, 260, y));
      pts.push([x, y + 0.5 * n(x / 300 + y, 2), w]);
    }
    d += ribbon(pts);
  }
  return d;
}

// The sun: a crisp ring with fine horizontal rules in its lower half, and
// concentric rings around it that thin out with distance.
function sunArt() {
  let d = "";
  const ring = (r, w) => {
    const pts = [];
    for (let a = 0; a <= 360; a += 2) {
      const t = (a * Math.PI) / 180;
      pts.push([SUN.x + r * Math.cos(t), SUN.y + r * Math.sin(t)]);
    }
    // Ring as an annulus with constant width.
    const outer = pts.map(([x, y]) => [SUN.x + (x - SUN.x) * (1 + w / 2 / r), SUN.y + (y - SUN.y) * (1 + w / 2 / r)]);
    const inner = pts.map(([x, y]) => [SUN.x + (x - SUN.x) * (1 - w / 2 / r), SUN.y + (y - SUN.y) * (1 - w / 2 / r)]);
    d += `M${outer.map(([x, y]) => `${f1(x)},${f1(y)}`).join("L")}Z`;
    d += `M${inner
      .reverse()
      .map(([x, y]) => `${f1(x)},${f1(y)}`)
      .join("L")}Z`;
  };
  ring(SUN.r, 1.6);
  let r = SUN.r + 9;
  let k = 0;
  while (r < SUN.r * 3.4) {
    ring(r, Math.max(0.3, 1.0 - k * 0.09));
    r += 8 + k * 2.4;
    k++;
  }
  // Rules across the lower part of the disc, getting heavier downward.
  for (let y = SUN.y - SUN.r * 0.15; y < SUN.y + SUN.r; y += 6) {
    const half = Math.sqrt(Math.max(0, SUN.r * SUN.r - (y - SUN.y) ** 2)) - 3;
    if (half <= 2) continue;
    const t = (y - (SUN.y - SUN.r * 0.15)) / (SUN.r * 1.15);
    const w = lerp(0.35, 1.6, t);
    d += ribbon([
      [SUN.x - half, y, w * 0.4],
      [SUN.x - half * 0.6, y, w],
      [SUN.x + half * 0.6, y, w],
      [SUN.x + half, y, w * 0.4],
    ]);
  }
  return d;
}

// Lake: broken horizontal strokes, with a glittering column under the sun.
function hatchLake(night = false) {
  const rnd = mulberry32(97);
  let d = "";
  for (let y = HORIZON + 3; y < H; y += 4.4 + (y - HORIZON) * 0.03) {
    const depth = (y - HORIZON) / (H - HORIZON);
    let x = -10 - rnd() * 40;
    while (x < W + 10) {
      const len = lerp(40, 160, rnd()) * (1 + depth);
      const gapLen = lerp(4, 22, rnd());
      const cx = x + len / 2;
      const glitter = Math.exp(-(((cx - SUN.x) / (SUN.r * (0.9 + depth * 1.6))) ** 2));
      const base = lerp(0.45, 1.25, depth) * (1 - 0.9 * glitter);
      if (base > MIN_W) {
        const pts = [];
        for (let t = 0; t <= 1.0001; t += 0.125) {
          const taper = Math.sin(Math.PI * t) ** 0.6;
          pts.push([x + len * t, y, base * taper + 0.01]);
        }
        d += ribbon(pts);
      } else if (rnd() < (night ? 0.95 : 0.5)) {
        // Short sparkles in the glitter path.
        const sl = lerp(6, 18, rnd());
        d += ribbon([
          [x, y, 0.2],
          [x + sl / 2, y, night ? 1.3 : 0.9],
          [x + sl, y, 0.2],
        ]);
      }
      x += len + gapLen;
    }
  }
  return d;
}

function buildSVG(v) {
  const k = v.weight;
  const scaleW = (o) => ({ ...o, w: o.w.map((x) => x * k) });
  const parts = [];
  const ink = (d, color = v.ink) => parts.push(`<path fill="${color}" d="${d}"/>`);
  const mask = (d) => parts.push(`<path fill="${v.bg}" d="${d}"/>`);

  ink(hatchSky(v.sky));
  ink(sunArt());

  ridges.forEach((r, i) => {
    mask(silhouette(r.top, HORIZON + 2));
    ink(hatchHill(r.top, scaleW({ gap: r.gap, w: r.w, floor: HORIZON, depthGrow: 0.002, flatten: 50, seed: 200 + i, light: 0.8 })));
  });

  // Lake surface (masks the sky below the horizon).
  parts.push(`<rect x="-20" y="${HORIZON}" width="${W + 40}" height="${H - HORIZON + 20}" fill="${v.bg}"/>`);
  ink(hatchLake(v.night));

  // Middle shoulder, then the framing near hills.
  mask(silhouette(midHill));
  ink(hatchHill(midHill, scaleW({ gap: 5.2, w: [0.55, 1.6], depthGrow: 0.006, flatten: 120, seed: 301 })));

  mask(silhouette(nearTopRight));
  ink(hatchHill(nearTopRight, scaleW({ gap: 6.2, w: [0.7, 2.3], depthGrow: 0.006, flatten: 140, seed: 401 })), v.deep);

  mask(silhouette(nearTopLeft));
  ink(hatchHill(nearTopLeft, scaleW({ gap: 6.6, w: [0.8, 2.6], depthGrow: 0.006, flatten: 160, seed: 501 })), v.deep);

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">
<rect width="${W}" height="${H}" fill="${v.bg}"/>
${parts.join("\n")}
</svg>`;
}

for (const [name, v] of Object.entries(VARIANTS)) {
  const svg = buildSVG(v);
  const out = join(outDir, v.file);
  if (process.env.DUMP_SVG) (await import("node:fs")).writeFileSync(join(process.env.DUMP_SVG, `${name}.svg`), svg);
  await sharp(Buffer.from(svg), { density: 72 })
    .flatten({ background: v.bg })
    .png({ palette: true, colors: 16, dither: 0 })
    .toBuffer()
    // Single ink + antialiasing fits a 16-colour palette with no visible
    // loss; lossless WebP of that is far smaller and crisper than lossy.
    .then((png) => sharp(png).webp({ lossless: true, effort: 6 }).toFile(out));
  const kb = (statSync(out).size / 1024).toFixed(1);
  console.log(`${name.padEnd(5)} ${v.file}  ${W}×${H}  ${kb} KB  (svg ${(svg.length / 1024).toFixed(0)} KB)`);
}
