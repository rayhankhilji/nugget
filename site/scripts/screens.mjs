// Converts DS1's real app screenshots (docs/brand/screens/*.png, 1440×900 @1x) into the WebP files
// the landing page uses, under public/screens/. Each crop is cut at 1:1 so its UI text stays legible.
// Run after DS1 retakes the screens: npm run screens
import sharp from "sharp";
import { mkdir, stat } from "node:fs/promises";

const SRC = new URL("../../docs/brand/screens/", import.meta.url);
const OUT = new URL("../public/screens/", import.meta.url);
const MAX = 300 * 1024;

/** [output name, source file, crop {left, top, width, height} or null, resize width or null] */
const JOBS = [
  // The showcase window, whole, at two widths.
  ["home-light", "home-light.png", null, null],
  ["home-light-960", "home-light.png", null, 960],
  ["home-dark", "home-dark.png", null, null],
  ["home-dark-960", "home-dark.png", null, 960],
  // 01 Ask: the morning brief with its person · app citation chips and the sources row.
  ["brief-light", "home-light.png", { left: 498, top: 430, width: 680, height: 236 }, null],
  ["brief-dark", "home-dark.png", { left: 498, top: 430, width: 680, height: 236 }, null],
  // 02 Ghost text: the compose window with the grey suggestion and its Tab hint.
  ["ghost", "ghost-text.png", { left: 290, top: 203, width: 860, height: 258 }, null],
  // 03 Needs you: the approval pane, with Approve ⌘↵, Reject, Edit E and Snooze H.
  ["approve", "needs-you.png", { left: 604, top: 175, width: 580, height: 385 }, null],
  // 04 Your AI: choosing the AI in the free app. The dark shot, because the light one shows a model id.
  ["choose-ai", "open-core-choose-ai-dark.png", { left: 400, top: 96, width: 640, height: 462 }, null],
];

await mkdir(OUT, { recursive: true });
let bad = 0;
for (const [name, file, crop, width] of JOBS) {
  let img = sharp(new URL(file, SRC).pathname);
  if (crop) img = img.extract(crop);
  if (width) img = img.resize({ width });
  const out = new URL(`${name}.webp`, OUT).pathname;
  await img.webp({ quality: 82, effort: 6 }).toFile(out);
  const { size } = await stat(out);
  if (size > MAX) bad++;
  console.log(`${size > MAX ? "OVER" : "ok  "} ${name}.webp ${(size / 1024).toFixed(1)} KB`);
}
if (bad) process.exit(1);
