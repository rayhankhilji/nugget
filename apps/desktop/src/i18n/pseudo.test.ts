// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import en from "./locales/en.json";
import { detectLocale, loadLocale, LOCALES, matchLocale, savedLocale, t } from ".";
import { format, signature } from "./icu";
import { accent, pseudoCatalog, pseudoMessage } from "./pseudo";

const catalog = pseudoCatalog(en as Record<string, string>);
const ARGS = { count: 2, n: 2, name: "x", total: 3 };

/** Format with placeholder arguments: every name the message uses gets a short value. */
function render(src: string): string {
  const args: Record<string, string | number> = { ...ARGS };
  for (const a of signature(src).args) if (!(a in args)) args[a] = 2;
  return format(src, "en", args);
}

describe("pseudo-locale", () => {
  afterEach(() => {
    localStorage.clear();
  });

  it("keeps every message's arguments, tags and branch keys", () => {
    const bad: string[] = [];
    for (const [k, v] of Object.entries(en as Record<string, string>)) {
      const p = catalog[k]!;
      if (JSON.stringify(signature(p)) !== JSON.stringify(signature(v))) bad.push(k);
      // Plural/select keywords and branch keys are untouched.
      const keys = (s: string) => [...s.matchAll(/,\s*(plural|select)\s*,|(=\d+|\b(?:zero|one|two|few|many|other)\b)\s*\{/g)].map((m) => m[0].replace(/\s+/g, ""));
      if (JSON.stringify(keys(p)) !== JSON.stringify(keys(v))) bad.push(`${k} (keywords)`);
    }
    expect(bad).toEqual([]);
  });

  it("makes text about a third longer on average, and short strings longer too", () => {
    let enLen = 0;
    let psLen = 0;
    const short: string[] = [];
    for (const [k, v] of Object.entries(en as Record<string, string>)) {
      const a = [...render(v)].length;
      const b = [...render(catalog[k]!)].length;
      enLen += a;
      psLen += b;
      if (/\p{L}/u.test(v) && b < a + 3) short.push(k);
    }
    expect(psLen / enLen).toBeGreaterThanOrEqual(1.3);
    expect(short).toEqual([]);
  });

  it("accents letters but not placeholders, # or tag names", () => {
    expect(accent("Search sources")).toBe("Šéáŕçĥ šóúŕçéš");
    const p = pseudoMessage("Found {n, plural, one {# item} other {# items}} in <code>{name}</code>");
    expect(p).toMatch(/^\[Fóúñď \{n, plural, one \{# íţéḿ\} other \{# íţéḿš\}\} íñ <code>\{name\}<\/code> \S.*\]$/u);
    expect(format(p, "en", { n: 1, name: "a.md" })).toContain("1 íţéḿ íñ a.md");
    expect(pseudoMessage("{name}")).toBe("{name}");
    expect(pseudoMessage("OK").length).toBeGreaterThanOrEqual(6);
  });

  it("is loadable and savable but never detected", async () => {
    expect(LOCALES.find((l) => l.code === "en-XA")?.pseudo).toBe(true);
    expect(matchLocale(["en-XA"])).toBe("en");
    expect(detectLocale()).not.toBe("en-XA");
    localStorage.setItem("nugget.locale", "en-XA");
    expect(savedLocale()).toBe("en-XA");
    await loadLocale("en-XA");
    expect(t("app.nav.search")).toMatch(/^\[.*\]$/);
    expect(document.documentElement.dir).toBe("ltr");
    await loadLocale("en");
  });
});
