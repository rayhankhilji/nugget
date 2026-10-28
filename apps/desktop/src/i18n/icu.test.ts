import { afterEach, describe, expect, it } from "vitest";
import { format, formatParts, MessageSyntaxError, signature } from "./icu";
import { matchLocale, setCatalogForTest, t, LOCALES, type Catalog } from ".";
import en from "./locales/en.json";

describe("ICU subset", () => {
  it("interpolates and formats numbers for the locale", () => {
    expect(format("Hi {name}", "en", { name: "Ada" })).toBe("Hi Ada");
    expect(format("{n} items", "en", { n: 12345 })).toBe("12,345 items");
    expect(format("{n} items", "de", { n: 12345 })).toBe("12.345 items");
    expect(format("{n, number, percent}", "fr", { n: 0.5 })).toMatch(/^50\s?%$/);
    expect(format("missing {x}", "en", {})).toBe("missing ");
  });

  it("selects plural branches with Intl.PluralRules", () => {
    const msg = "{n, plural, =0 {no files} one {# file} other {# files}}";
    expect(format(msg, "en", { n: 0 })).toBe("no files");
    expect(format(msg, "en", { n: 1 })).toBe("1 file");
    expect(format(msg, "en", { n: 2000 })).toBe("2,000 files");
    // French: 0 and 1 are both "one".
    const fr = "{n, plural, one {# fichier} other {# fichiers}}";
    expect(format(fr, "fr", { n: 0 })).toBe("0 fichier");
    expect(format(fr, "fr", { n: 2 })).toBe("2 fichiers");
    // Japanese has only "other".
    expect(format("{n, plural, other {#件}}", "ja", { n: 1 })).toBe("1件");
  });

  it("supports select and nesting", () => {
    const msg = "{who, select, me {You have} other {{who} has}} {n, plural, one {# fact} other {# facts}}";
    expect(format(msg, "en", { who: "me", n: 1 })).toBe("You have 1 fact");
    expect(format(msg, "en", { who: "Ada", n: 3 })).toBe("Ada has 3 facts");
  });

  it("formats dates", () => {
    const d = new Date(Date.UTC(2026, 9, 28, 12));
    expect(format("{d, date, long}", "en", { d })).toContain("2026");
    expect(format("{d, date, long}", "de", { d })).toContain("Oktober");
  });

  it("keeps apostrophes literal", () => {
    expect(format("l'application de {name}", "fr", { name: "Ada" })).toBe("l'application de Ada");
  });

  it("renders tags as parts and never parses markup from arguments", () => {
    const parts = formatParts("Open <code>{path}</code> now", "en", { path: "<b>x</b>" }, (tag, children) => ({
      tag,
      children,
    }));
    expect(parts).toEqual(["Open ", { tag: "code", children: ["<b>x</b>"] }, " now"]);
    expect(format("Open <code>{path}</code>", "en", { path: "a" })).toBe("Open a");
  });

  it("rejects malformed messages", () => {
    expect(() => format("{n, plural, one {x}}", "en")).toThrow(MessageSyntaxError);
    expect(() => format("<b>open", "en")).toThrow(MessageSyntaxError);
    expect(() => format("{n, wat}", "en")).toThrow(MessageSyntaxError);
  });

  it("reports a message's arguments and tags", () => {
    expect(signature("<b>{a}</b> {n, plural, one {# {b}} other {#}}")).toEqual({ args: ["a", "b", "n"], tags: ["b"] });
  });
});

describe("runtime", () => {
  afterEach(() => setCatalogForTest("en", en));

  it("falls back to English for missing keys and broken translations", () => {
    setCatalogForTest("xx", { "common.retry": "Nochmal", "common.cancel": "{broken" } as Catalog);
    expect(t("common.retry")).toBe("Nochmal");
    expect(t("common.close")).toBe("Close");
    expect(t("common.cancel")).toBe("Cancel");
  });

  it("matches OS languages to supported locales", () => {
    expect(matchLocale(["de-AT", "en"])).toBe("de");
    expect(matchLocale(["pt-PT"])).toBe("pt-BR");
    expect(matchLocale(["zh-Hans-CN"])).toBe("zh-CN");
    expect(matchLocale(["sw", "fr-CA"])).toBe("fr");
    expect(matchLocale(["sw"])).toBe("en");
    expect(matchLocale(["not a tag", "ja-JP"])).toBe("ja");
    // Stubs aren't picked automatically.
    expect(matchLocale(["ar-EG"], LOCALES.filter((l) => !l.stub))).toBe("en");
  });
});
