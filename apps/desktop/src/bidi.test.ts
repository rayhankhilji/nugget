import { describe, expect, it } from "vitest";
import { textDir } from "./bidi";

describe("textDir (LL2, GG7-10)", () => {
  it("lays out Latin-led Arabic right-to-left", () => {
    expect(textDir("iPhone 15 سيصل يوم الخميس، والفاتورة ١٢٠ دينار.")).toBe("rtl");
    expect(textDir("**Nugget Pro** هو الاشتراك الحالي [1].")).toBe("rtl");
    expect(textDir("ORD-2026-5531 تم شحن طلبك")).toBe("rtl");
  });
  it("keeps mostly-Latin text with an Arabic name left-to-right", () => {
    expect(textDir("سام said the invoice is due on Friday.")).toBe("ltr");
    expect(textDir("שלום, see you at the meeting tomorrow")).toBe("ltr");
  });
  it("counts CJK and Devanagari as left-to-right", () => {
    expect(textDir("会议改到下午两点。")).toBe("ltr");
    expect(textDir("कृपया रिपोर्ट भेजें।")).toBe("ltr");
  });
  it("ignores link targets", () => {
    expect(textDir("راجع [الفاتورة](https://example.com/invoices/very-long-path-name)")).toBe("rtl");
    expect(textDir("راجع https://example.com/invoices/very-long-path-name")).toBe("rtl");
  });
  it("leaves text without letters, or balanced, to the browser", () => {
    expect(textDir("")).toBe("auto");
    expect(textDir(null)).toBe("auto");
    expect(textDir("١٢٠ / 120 — 3.5")).toBe("auto");
    expect(textDir("ab بت")).toBe("auto");
  });
});
