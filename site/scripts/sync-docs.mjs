// Copies docs pages the website renders from the Heed book into src/content/docs/, so the site builds
// on its own (Vercel's root directory is site/). Run from site/: `node scripts/sync-docs.mjs`;
// `--check` exits non-zero if a copy is stale. The smoke test runs the same comparison.
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

const SITE = fileURLToPath(new URL("..", import.meta.url));
export const DOCS = [{ from: "../docs/book/src/guide/self-host.md", to: "src/content/docs/self-host.md" }];

/** The site copy of a book page: the book's text, with a header saying where it came from. */
export function render(from) {
  const md = readFileSync(join(SITE, from), "utf8");
  return `<!-- Generated from ${from.replace("../", "")} by scripts/sync-docs.mjs. Edit the book page, not this copy. -->\n\n${md}`;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  let stale = 0;
  for (const d of DOCS) {
    if (!existsSync(join(SITE, d.from))) {
      console.error(`missing ${d.from}; run inside the nugget repo`);
      process.exit(2);
    }
    const out = render(d.from);
    if (process.argv.includes("--check")) {
      const ok = existsSync(join(SITE, d.to)) && readFileSync(join(SITE, d.to), "utf8") === out;
      if (!ok) stale++;
      console.log(`${ok ? "up to date" : "stale"}: ${d.to}`);
    } else {
      writeFileSync(join(SITE, d.to), out);
      console.log(`wrote ${d.to}`);
    }
  }
  process.exit(stale ? 1 : 0);
}
