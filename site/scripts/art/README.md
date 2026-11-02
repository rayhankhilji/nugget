# Procedural art

Deterministic generators for the site's background art. They use fixed seeds and no network or third-party images. Every pixel comes from the code in this folder.

```sh
# from site/
node scripts/art/sky.mjs && node scripts/art/landscape.mjs
```

Both scripts write to `public/art/` and use only `sharp`, which is already a devDependency.

## `sky.mjs`: hero sky

Outputs `sky-light.webp` and `sky-dark.webp`. Each is 640×360 lossless WebP, where 1 px is one dither cell.

- A vertical sky gradient carries faint cirrus streaks drawn with periodic gradient-noise fBm. Four layers of metaball cumulus sit below it, with flat bases, crowned tops and self-shadowing. The far layers melt into haze.
- The image is quantised to the palette with an 8×8 Bayer ordered dither.
- It tiles seamlessly in x. The noise is periodic, the cloud puffs wrap around, and the width is a multiple of 8.
- Every cell in the top 55% is clamped to a colour on which `#F6F3EC` text passes WCAG AA. The script prints the worst-case contrast for that region.
- Display it at 3 CSS px per cell: `background-size: 1920px 1080px; image-rendering: pixelated; background-repeat: repeat-x`. One tile is 1920 CSS px wide.

## `landscape.mjs`: footer engraving

Outputs `landscape-light.webp` and `landscape-dark.webp` at 2400×640.

- The scene is built as an SVG made only of variable-width hatching ribbons in a single ink: ruled sky, a ringed sun or moon, three far ridges, a lake with a glitter path, and framing near hills. Line weight and spacing carry the tone.
- The SVG is rasterised with sharp, quantised to a 16-colour palette (one ink plus anti-aliasing) and saved as lossless WebP.
- The top edge dissolves into the background colour.
- Set `DUMP_SVG=<dir>` to also write the intermediate SVGs.

## Palette

| Use | Light | Dark |
| --- | --- | --- |
| Sky | `#070798` `#1F3BFF` `#5A6CFF` `#A3ADFF` `#E8E3FF` `#F6F3EC` | `#0B0B1A` `#070798` `#1F3BFF` `#5A6CFF` |
| Landscape ink / ground | `#1F3BFF` (near hills `#070798`) on `#F6F3EC` | `#5A6CFF` on `#0B0B1A` |

## Licence

The scripts and their output are original procedural work, released under the project licence.
