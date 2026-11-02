/**
 * Social cards: a Fraunces line over the dithered sky, with the wordmark. Rendered once at build time
 * (static output) with satori + resvg, from static TTF instances of Fraunces (SOFT 100, WONK 1, wght 320)
 * and Geist Mono in src/og/.
 */
import type { APIRoute, GetStaticPaths } from "astro";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import satori from "satori";
import { Resvg } from "@resvg/resvg-js";
import { OG_PAGES } from "../../lib/og";

// Resolved from the project root: the build runs in site/ (also on Vercel, with Root Directory = site).
const here = (p: string) => join(process.cwd(), p);
const fraunces = readFileSync(here("src/og/fraunces-display.ttf"));
const mono = readFileSync(here("src/og/geist-mono.ttf"));
const sky = `data:image/png;base64,${readFileSync(here("src/og/sky.png")).toString("base64")}`;
const word = `data:image/svg+xml;base64,${readFileSync(here("public/brand/heed-wordmark-light.svg")).toString("base64")}`;

export const getStaticPaths: GetStaticPaths = () => Object.keys(OG_PAGES).map((slug) => ({ params: { slug } }));

type Node = { type: string; props: Record<string, unknown> & { children?: unknown } };
const h = (type: string, style: Record<string, unknown>, children?: unknown, extra: Record<string, unknown> = {}): Node => ({
  type,
  props: { style, ...extra, ...(children === undefined ? {} : { children }) },
});

export const GET: APIRoute = async ({ params }) => {
  const page = OG_PAGES[params["slug"] ?? "home"] ?? OG_PAGES["home"]!;
  const tree = h(
    "div",
    { width: 1200, height: 630, display: "flex", position: "relative", backgroundColor: "#070798" },
    [
      h("img", { position: "absolute", left: 0, top: 0, width: 1200, height: 630 }, undefined, { src: sky, width: 1200, height: 630 }),
      h(
        "div",
        { position: "absolute", left: 80, top: 56, right: 80, display: "flex", flexDirection: "column" },
        [
          h("img", { width: 160, height: 70 }, undefined, { src: word, width: 160, height: 70 }),
          h(
            "div",
            { marginTop: 32, fontFamily: "Geist Mono", fontSize: 22, letterSpacing: 3, color: "rgba(246,243,236,0.9)", textTransform: "uppercase" },
            page.label,
          ),
          h(
            "div",
            { marginTop: 16, fontFamily: "Fraunces", fontSize: 70, lineHeight: 1.02, letterSpacing: -2, color: "#F6F3EC", maxWidth: 1000 },
            page.line,
          ),
        ],
      ),
    ],
  );
  const svg = await satori(tree as unknown as Parameters<typeof satori>[0], {
    width: 1200,
    height: 630,
    fonts: [
      { name: "Fraunces", data: fraunces, weight: 300, style: "normal" },
      { name: "Geist Mono", data: mono, weight: 400, style: "normal" },
    ],
  });
  const png = new Resvg(svg, { fitTo: { mode: "width", value: 1200 } }).render().asPng();
  return new Response(new Uint8Array(png), { headers: { "Content-Type": "image/png" } });
};
