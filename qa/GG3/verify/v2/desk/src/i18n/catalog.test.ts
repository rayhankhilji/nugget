import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";
import { LOCALES } from ".";
import { format, signature } from "./icu";

const DIR = join(__dirname, "locales");
const load = (code: string): Record<string, string> => JSON.parse(readFileSync(join(DIR, `${code}.json`), "utf8"));
const en = load("en");
const enKeys = Object.keys(en).sort();
/** Keys whose English text is also right in a translation (brand names, keys, format-only strings). */
const sameAsEnglish: Record<string, string[]> = JSON.parse(readFileSync(join(__dirname, "same-as-english.json"), "utf8"));

const full = LOCALES.filter((l) => l.code !== "en" && !l.stub && !l.pseudo);
const stubs = LOCALES.filter((l) => l.stub);

/** Plural categories each language actually uses (CLDR), from the platform's own data. */
function categories(code: string): string[] {
  return new Intl.PluralRules(code).resolvedOptions().pluralCategories;
}

describe("message catalogs", () => {
  it("has a file for every locale and a locale for every file", () => {
    const files = readdirSync(DIR)
      .filter((f) => f.endsWith(".json"))
      .map((f) => f.slice(0, -5))
      .sort();
    // Pseudo-locales are generated from English and have no file.
    expect(files).toEqual(LOCALES.filter((l) => !l.pseudo).map((l) => l.code).sort());
  });

  it("English messages all parse", () => {
    for (const [k, v] of Object.entries(en)) {
      expect(() => format(v, "en", {}), k).not.toThrow();
    }
  });

  describe.each(full.map((l) => [l.code]))("%s", (code) => {
    let cat: Record<string, string> = {};
    beforeAll(() => {
      cat = load(code);
    });

    it("has every English key and no others", () => {
      const keys = Object.keys(cat).sort();
      expect(enKeys.filter((k) => !(k in cat)), `missing in ${code}.json`).toEqual([]);
      expect(keys.filter((k) => !(k in en)), `unknown in ${code}.json`).toEqual([]);
    });

    it("translates every message (no empty strings)", () => {
      expect(Object.entries(cat).filter(([, v]) => typeof v !== "string" || !v.trim()).map(([k]) => k)).toEqual([]);
    });

    it("keeps each message's arguments and tags", () => {
      const bad: string[] = [];
      for (const k of enKeys) {
        if (!(k in cat)) continue;
        let sig;
        try {
          sig = signature(cat[k]!);
        } catch (e) {
          bad.push(`${k}: ${(e as Error).message}`);
          continue;
        }
        const want = signature(en[k]!);
        if (JSON.stringify(sig) !== JSON.stringify(want)) {
          bad.push(`${k}: has ${JSON.stringify(sig)}, English has ${JSON.stringify(want)}`);
        }
      }
      expect(bad).toEqual([]);
    });

    it("gives plurals the branches this language needs", () => {
      const needed = categories(code);
      const bad: string[] = [];
      for (const [k, v] of Object.entries(cat)) {
        for (const m of v.matchAll(/\{\s*\w+\s*,\s*plural\s*,/g)) {
          const body = v.slice(m.index);
          for (const c of needed) {
            // `other` is required; `one` too where the language distinguishes it. Rarer categories
            // (`many`, `few`) fall back to `other`, which is acceptable for UI counts.
            if ((c === "other" || c === "one") && !new RegExp(`\\b${c}\\s*\\{`).test(body)) bad.push(`${k} (${c})`);
          }
        }
      }
      expect(bad).toEqual([]);
    });

    it("has no message still in English unless allow-listed", () => {
      const allowed = new Set([...(sameAsEnglish["*"] ?? []), ...(sameAsEnglish[code] ?? [])]);
      const english = enKeys.filter((k) => cat[k] === en[k] && !allowed.has(k));
      expect(english, `still English in ${code}.json (translate, or add to same-as-english.json)`).toEqual([]);
    });

    it("lists only real keys for review", () => {
      const file = join(DIR, "review", `${code}.json`);
      if (!existsSync(file)) return;
      const review = JSON.parse(readFileSync(file, "utf8")) as { keys: string[] };
      expect(Array.isArray(review.keys)).toBe(true);
      expect(review.keys.filter((k) => !(k in en))).toEqual([]);
    });
  });

  it("allow-lists only keys that exist, for locales that exist", () => {
    for (const [code, keys] of Object.entries(sameAsEnglish)) {
      if (code !== "*") expect(full.map((l) => l.code), `allow-list for unknown locale ${code}`).toContain(code);
      expect(keys.filter((k) => !(k in en)), `unknown keys allow-listed under ${code}`).toEqual([]);
    }
  });

  describe.each(stubs.map((l) => [l.code, l.dir]))("stub %s", (code, dir) => {
    it("is right-to-left and only has known, well-formed keys", () => {
      expect(dir).toBe("rtl");
      const cat = load(code);
      for (const [k, v] of Object.entries(cat)) {
        expect(en, `unknown key ${k}`).toHaveProperty([k]);
        expect(signature(v), k).toEqual(signature(en[k]!));
      }
    });
  });
});
