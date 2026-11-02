// Generates src/data/apps.json, the apps the website says Heed connects to, from the cloud's catalogue
// in crates/nugget-cloud/src/apps/mod.rs (CATALOG), so the site never claims an app the cloud can't sync.
// Run from site/: `node scripts/sync-apps.mjs`. `--check` exits non-zero if the JSON is stale.
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

const SITE = fileURLToPath(new URL("..", import.meta.url));
const SRC = join(SITE, "../crates/nugget-cloud/src");
const OUT = join(SITE, "src/data/apps.json");

/** Reads the catalogue as [{ slug, name }] in its declared order. */
export function readCatalog(src = SRC) {
  const mod = readFileSync(join(src, "apps/mod.rs"), "utf8");
  const body = mod.slice(mod.indexOf("pub const CATALOG"));
  const block = body.slice(0, body.indexOf("];"));
  const consts = (file) => {
    const text = readFileSync(file, "utf8");
    return Object.fromEntries([...text.matchAll(/pub const (\w+): &str = ([^;]+);/g)].map((m) => [m[1], m[2].trim()]));
  };
  const resolve = (path) => {
    // e.g. google::GMAIL -> apps/google.rs; ingest::GMAIL -> ingest.rs
    const [mod_, name] = path.split("::");
    const file = mod_ === "ingest" ? join(src, "ingest.rs") : join(src, `apps/${mod_}.rs`);
    const v = consts(file)[name];
    if (!v) throw new Error(`can't resolve ${path}`);
    return v.startsWith('"') ? JSON.parse(v) : resolve(v);
  };
  return [...block.matchAll(/slug:\s*([\w:]+),\s*name:\s*"([^"]+)"/g)].map((m) => ({ slug: resolve(m[1]), name: m[2] }));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  if (!existsSync(join(SRC, "apps/mod.rs"))) {
    console.error(`no catalogue at ${SRC}; run inside the nugget repo`);
    process.exit(2);
  }
  const json = `${JSON.stringify(readCatalog(), null, 2)}\n`;
  if (process.argv.includes("--check")) {
    const ok = existsSync(OUT) && readFileSync(OUT, "utf8") === json;
    console.log(ok ? "apps.json is up to date" : "apps.json is stale: run node scripts/sync-apps.mjs");
    process.exit(ok ? 0 : 1);
  }
  writeFileSync(OUT, json);
  console.log(`wrote ${OUT}`);
}
