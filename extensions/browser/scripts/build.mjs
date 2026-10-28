// Build the extension for Chromium-family browsers and Firefox from one source tree.
//
//   node scripts/build.mjs          -> dist/chromium, dist/firefox
//   node scripts/build.mjs --e2e    -> dist/e2e (Chromium, test hook on, localhost pre-granted)
//
// Plain esbuild bundles (IIFE, no code splitting, no eval, no remote code).

import { build } from "esbuild";
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const e2e = process.argv.includes("--e2e");
const base = JSON.parse(readFileSync(join(root, "static/manifest.base.json"), "utf8"));

/** Firefox add-on id; must match `FIREFOX_EXTENSION_ID` in crates/nugget-browser/src/install.rs. */
const GECKO_ID = "browser@nugget.dev";

const targets = e2e
  ? [{ name: "e2e", manifest: { ...base, host_permissions: ["http://127.0.0.1/*"], background: { service_worker: "background.js" } } }]
  : [
      { name: "chromium", manifest: { ...base, background: { service_worker: "background.js" } } },
      {
        name: "firefox",
        manifest: (() => {
          const { key: _key, minimum_chrome_version: _min, ...rest } = base;
          return {
            ...rest,
            background: { scripts: ["background.js"] },
            browser_specific_settings: { gecko: { id: GECKO_ID, strict_min_version: "128.0" } },
          };
        })(),
      },
    ];

for (const target of targets) {
  const out = join(root, "dist", target.name);
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });
  await build({
    entryPoints: {
      background: join(root, "src/background.ts"),
      content: join(root, "src/content.ts"),
      popup: join(root, "src/popup.ts"),
    },
    outdir: out,
    bundle: true,
    format: "iife",
    target: ["chrome116", "firefox128"],
    platform: "browser",
    legalComments: "none",
    minify: false,
    sourcemap: false,
    define: { NUGGET_E2E: e2e ? "true" : "false" },
    logLevel: "warning",
  });
  for (const f of ["popup.html", "popup.css"]) cpSync(join(root, "static", f), join(out, f));
  writeFileSync(join(out, "manifest.json"), JSON.stringify(target.manifest, null, 2) + "\n");
  checkNoEval(out);
  console.log(`built dist/${target.name}`);
}

// Belt and braces for the "no eval / no remote code" rule: fail the build if a bundle contains
// dynamic code evaluation or a remote script reference.
function checkNoEval(dir) {
  for (const f of ["background.js", "content.js", "popup.js"]) {
    const src = readFileSync(join(dir, f), "utf8");
    for (const bad of [/\beval\s*\(/, /\bnew Function\s*\(/, /importScripts\s*\(/, /<script[^>]+src=["']https?:/]) {
      if (bad.test(src)) throw new Error(`${f} contains forbidden code: ${bad}`);
    }
  }
}
