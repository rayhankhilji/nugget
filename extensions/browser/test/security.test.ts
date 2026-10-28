// Red-team regressions (W8): each test is a proof of concept that failed before its fix.

import { describe, expect, it } from "vitest";
import { PageAgent } from "../src/agent";
import { createOverlay } from "../src/overlay";
import { ProtocolError } from "../src/protocol";
import { type BrowserDeps, Router } from "../src/router";
import type { ContentRequest, ContentResponse } from "../src/content";
import { buildSnapshot } from "../src/snapshot";
import { dom, measure, registry } from "./helpers";

function snap(doc: Document, reg = registry()) {
  return buildSnapshot(doc, { registry: reg, measure, now: () => 1 });
}

async function code(p: Promise<unknown>): Promise<string | undefined> {
  try {
    await p;
  } catch (e) {
    if (e instanceof ProtocolError) return e.code;
    throw e;
  }
  return undefined;
}

describe("BR-1: sensitive contenteditable / textarea contents", () => {
  it("never puts a sensitive editable's contents in the page text or its name", () => {
    const doc = dom(`<body>
      <div contenteditable="true" id="cc-number">4111111111111111</div>
      <div contenteditable="true" aria-label="Card number">5500005555555559</div>
      <span id="lbl">Code</span><textarea aria-labelledby="pin">482913</textarea>
      <textarea name="otp" id="pin">771122</textarea>
      <p>Visible text</p></body>`);
    const json = JSON.stringify(snap(doc));
    for (const secret of ["4111111111111111", "5500005555555559", "771122"]) expect(json).not.toContain(secret);
    expect(json).toContain("Visible text");
  });

  it("does not leak a sensitive field's contents through another element's aria-labelledby", () => {
    const doc = dom(`<body><div contenteditable="true" id="pw" aria-label="password">hunter2</div>
      <button aria-labelledby="pw">Go</button></body>`);
    expect(JSON.stringify(snap(doc))).not.toContain("hunter2");
  });
});

describe("BR-2: payment selects", () => {
  const html = `<body><form>
    <select autocomplete="cc-exp-month" aria-label="Expiry month"><option value="01">01</option><option value="07" selected>07</option></select>
    <select name="plan" aria-label="Plan"><option value="a">A</option><option value="b" selected>B</option></select>
  </form></body>`;

  it("marks cc-* selects as payment and hides the chosen value", () => {
    const s = snap(dom(html));
    const exp = s.elements.find((e) => e.name === "Expiry month")!;
    expect(exp.sensitive).toBe("payment");
    expect(exp.value).toBeUndefined();
    expect(exp.options?.some((o) => o.selected)).toBe(false);
    expect(s.elements.find((e) => e.name === "Plan")).toMatchObject({ value: "b" });
  });

  it("refuses to select in a payment select", async () => {
    const a = new PageAgent(dom(html), { measure, settleMs: 0, token: "t0" });
    const s = a.snapshot();
    const exp = s.elements.find((e) => e.name === "Expiry month")!;
    expect(await code(a.act(s.snapshot_id, { kind: "select", element: exp.id, value: "01" }))).toBe("sensitive_field");
  });
});

describe("BR-3: fields that change type", () => {
  it("keeps a password field redacted after the page flips it to type=text", () => {
    const doc = dom(`<body><input type="password" aria-label="Secret" value="hunter2"></body>`);
    const reg = registry();
    expect(snap(doc, reg).elements[0]!.sensitive).toBe("password");
    doc.querySelector("input")!.setAttribute("type", "text");
    const again = snap(doc, reg);
    expect(again.elements[0]!.sensitive).toBe("password");
    expect(JSON.stringify(again)).not.toContain("hunter2");
  });

  it("refuses to type into a field that became a password field after the snapshot", async () => {
    const doc = dom(`<body><input type="text" aria-label="Search"></body>`);
    const a = new PageAgent(doc, { measure, settleMs: 0, token: "t0" });
    const s = a.snapshot();
    doc.querySelector("input")!.setAttribute("type", "password");
    expect(await code(a.act(s.snapshot_id, { kind: "type", element: s.elements[0]!.id, text: "x" }))).toBe("sensitive_field");
    expect(doc.querySelector("input")!.value).toBe("");
  });
});

describe("BR-4: overlay spoofing", () => {
  it("does not let the page hide its own content from snapshots with the overlay attribute", () => {
    const doc = dom(`<body><div data-nugget-overlay><p>Warning: this is a phishing page</p><button>Pay</button></div></body>`);
    const s = snap(doc);
    expect(s.text).toContain("phishing");
    expect(s.elements.map((e) => e.name)).toContain("Pay");
  });

  it("puts the overlay back when the page removes it, with !important styles", async () => {
    const doc = dom("<body></body>");
    const o = createOverlay(doc, () => undefined);
    o.show();
    const host = doc.querySelector("[data-nugget-overlay]") as HTMLElement;
    expect(host.style.getPropertyPriority("display")).toBe("important");
    host.remove();
    await new Promise((r) => setTimeout(r, 0));
    expect(o.visible).toBe(true);
    o.hide();
    await new Promise((r) => setTimeout(r, 0));
    expect(o.visible).toBe(false);
  });
});

describe("BR-5: router consent and kill switch", () => {
  class Fake implements BrowserDeps {
    granted = new Set(["https://shop.example"]);
    pageUrl = "https://shop.example/page";
    extensionVersion = "0";
    browserName = "test";
    async activeTab() {
      return { id: 1, url: "https://shop.example/page" };
    }
    async getTab(id: number) {
      return { id, url: "https://shop.example/page" };
    }
    async hasPermission(o: string) {
      return this.granted.has(o);
    }
    async inject() {}
    async sendToTab(_t: number, msg: ContentRequest): Promise<ContentResponse> {
      if (msg.nugget === "snapshot") {
        // The tab navigated to another site between the consent check and the read.
        const a = new PageAgent(dom("<body><p>bank balance 1234</p></body>", this.pageUrl), { measure, settleMs: 0 });
        return { ok: true, result: a.snapshot() };
      }
      return { ok: true, result: {} };
    }
    async navigate() {}
  }

  it("drops a snapshot whose document is on a site the user did not grant", async () => {
    const f = new Fake();
    f.pageUrl = "https://bank.example/account";
    const r = JSON.parse(await new Router(f).handleRaw({ id: 1, type: "snapshot" }));
    expect(r).toMatchObject({ error: { code: "permission_required" } });
    expect(JSON.stringify(r)).not.toContain("1234");
  });

  it("refuses snapshots and showing the 'acting' overlay while stopped", async () => {
    const router = new Router(new Fake());
    await router.halt();
    expect(JSON.parse(await router.handleRaw({ id: 1, type: "snapshot" }))).toMatchObject({ error: { code: "halted" } });
    expect(JSON.parse(await router.handleRaw({ id: 2, type: "overlay", active: true }))).toMatchObject({
      error: { code: "halted" },
    });
    expect(JSON.parse(await router.handleRaw({ id: 3, type: "overlay", active: false }))).toMatchObject({ result: {} });
  });
});

describe("BR-6: snapshot size caps", () => {
  it("caps the title and link hrefs so a page cannot blow the frame cap", () => {
    const doc = dom(`<body><a href="https://shop.example/?${"q".repeat(100_000)}">x</a></body>`);
    doc.title = "T".repeat(100_000);
    const s = snap(doc);
    expect(s.title.length).toBeLessThanOrEqual(1000);
    expect(s.elements[0]!.href!.length).toBeLessThanOrEqual(4096);
  });
});
