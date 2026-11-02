// After the build, turn straight quotes in every page's text into typographic ones (see
// src/lib/smartquotes.mjs). Code, scripts and attributes are left untouched.
import { readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { smartHtml } from "../src/lib/smartquotes.mjs";

async function* html(dir) {
  for (const e of await readdir(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) yield* html(p);
    else if (e.name.endsWith(".html")) yield p;
  }
}

export default function smartquotes() {
  return {
    name: "heed-smartquotes",
    hooks: {
      "astro:build:done": async ({ dir }) => {
        for await (const file of html(fileURLToPath(dir))) {
          const src = await readFile(file, "utf8");
          const out = smartHtml(src);
          if (out !== src) await writeFile(file, out);
        }
      },
    },
  };
}
