import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";
import { lintSource, looksLikeProse } from "./lint";

const SRC = join(__dirname, "..");

function tsxFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const p = join(dir, e.name);
    if (e.isDirectory()) return tsxFiles(p);
    return e.name.endsWith(".tsx") && !e.name.endsWith(".test.tsx") ? [p] : [];
  });
}

describe("hard-coded string lint", () => {
  it("finds JSX text, visible attributes and prose literals", () => {
    const src = `
      const a = "nav-item";
      const b = setError("Install failed.");
      const c = <p title="Open it" className="x y">Hello there · {n}</p>;
      const d = <span>·</span>;
      // i18n-ignore: brand
      const e = <b>Nugget</b>;
    `;
    const found = lintSource("x.tsx", src).map((f) => `${f.kind}:${f.text}`);
    expect(found).toEqual(['literal:Install failed.', 'attribute:title="Open it"', "jsx-text:Hello there ·"]);
  });

  it("tells prose from identifiers", () => {
    expect(looksLikeProse("Add a source")).toBe(true);
    expect(looksLikeProse("Failed.")).toBe(true);
    expect(looksLikeProse("btn btn-primary")).toBe(false);
    expect(looksLikeProse("models.install")).toBe(false);
    expect(looksLikeProse("Chat")).toBe(false);
    expect(looksLikeProse("https://example.com/A b")).toBe(false);
  });

  it("no user-visible string in the app bypasses the catalog", () => {
    const findings = tsxFiles(SRC).flatMap((f) =>
      lintSource(relative(SRC, f), readFileSync(f, "utf8")).map((x) => `${x.file}:${x.line} ${x.kind} ${x.text}`),
    );
    expect(findings, "Move these strings into src/i18n/locales/en.json and use t()").toEqual([]);
  });
});
