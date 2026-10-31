import { beforeEach, describe, expect, it } from "vitest";
import type { ContentRequest, ContentResponse } from "../src/content";
import { PageAgent } from "../src/agent";
import { type BrowserDeps, Router } from "../src/router";
import { dom, fixture, measure } from "./helpers";

class FakeBrowser implements BrowserDeps {
  granted = new Set<string>();
  url = "https://shop.example/page";
  agent = new PageAgent(dom(fixture("form.html")), { measure, settleMs: 0, token: "t0" });
  injected = 0;
  navigations: string[] = [];
  extensionVersion = "0.0.1";
  browserName = "test";

  async activeTab() {
    return { id: 7, url: this.url };
  }
  async getTab(id: number) {
    return id === 7 ? { id, url: this.url } : undefined;
  }
  async hasPermission(origin: string) {
    return this.granted.has(origin);
  }
  async inject() {
    this.injected++;
  }
  async sendToTab(_tab: number, msg: ContentRequest): Promise<ContentResponse> {
    try {
      switch (msg.nugget) {
        case "snapshot":
          return { ok: true, result: this.agent.snapshot(msg.options) };
        case "act":
          return { ok: true, result: await this.agent.act(msg.snapshot_id, msg.action) };
        case "halt":
          this.agent.halt();
          return { ok: true, result: {} };
        case "resume":
          this.agent.resume();
          return { ok: true, result: {} };
        default:
          return { ok: true, result: {} };
      }
    } catch (e) {
      const err = e as { code?: string; message: string };
      return { ok: false, code: err.code ?? "internal", message: err.message };
    }
  }
  async navigate(_tab: number, url: string) {
    this.navigations.push(url);
    this.url = url;
    this.agent = new PageAgent(dom("<body><h1>Next</h1><button>Ok</button></body>", url), { measure, settleMs: 0, token: "t1" });
  }
}

let fake: FakeBrowser;
let router: Router;
const call = async (raw: unknown) => JSON.parse(await router.handleRaw(raw));

beforeEach(() => {
  fake = new FakeBrowser();
  router = new Router(fake);
});

describe("router", () => {
  it("answers hello without any site access", async () => {
    expect(await call({ id: 1, type: "hello", protocol: 1 })).toEqual({
      type: "reply",
      id: 1,
      result: { protocol: 1, extension_version: "0.0.1", browser: "test" },
    });
  });

  it("requires a per-site grant before reading a page", async () => {
    const r = await call({ id: 2, type: "snapshot" });
    expect(r).toMatchObject({ id: 2, error: { code: "permission_required" } });
    expect(fake.injected).toBe(0);
    fake.granted.add("https://shop.example");
    const ok = await call({ id: 3, type: "snapshot" });
    expect(ok.result.title).toBe("Sign up");
    expect(fake.injected).toBe(1);
  });

  it("snapshots then acts and verifies", async () => {
    fake.granted.add("https://shop.example");
    const snap = (await call({ id: 1, type: "snapshot" })).result;
    const tos = snap.elements.find((e: { name: string }) => e.name === "I agree to the terms").id;
    const r = await call({ id: 2, type: "act", snapshot_id: snap.snapshot_id, action: { kind: "click", element: tos } });
    expect(r.result.diff.changed).toContainEqual({ id: tos, fields: expect.arrayContaining(["checked"]) });
  });

  it("passes content errors through with their code", async () => {
    fake.granted.add("https://shop.example");
    await call({ id: 1, type: "snapshot" });
    const r = await call({ id: 2, type: "act", snapshot_id: "zz.1", action: { kind: "click", element: "n1" } });
    expect(r).toMatchObject({ id: 2, error: { code: "stale_snapshot" } });
  });

  it("navigates only to granted http(s) sites", async () => {
    fake.granted.add("https://shop.example");
    expect(await call({ id: 1, type: "act", snapshot_id: "", action: { kind: "navigate", url: "https://bank.example/" } }))
      .toMatchObject({ error: { code: "permission_required" } });
    expect(await call({ id: 2, type: "act", snapshot_id: "", action: { kind: "navigate", url: "javascript:alert(1)" } }))
      .toMatchObject({ error: { code: "bad_url" } });
    expect(fake.navigations).toEqual([]);
    const before = (await call({ id: 3, type: "snapshot" })).result;
    const r = await call({ id: 4, type: "act", snapshot_id: before.snapshot_id, action: { kind: "navigate", url: "/next" } });
    expect(fake.navigations).toEqual(["https://shop.example/next"]);
    expect(r.result.diff.url_changed).toEqual({ from: "https://shop.example/page", to: "https://shop.example/next" });
    expect(r.result.before_snapshot_id).toBe(before.snapshot_id);
  });

  it("halts on the kill switch until the user resumes", async () => {
    fake.granted.add("https://shop.example");
    const snap = (await call({ id: 1, type: "snapshot" })).result;
    await call({ id: 2, type: "halt" });
    expect(router.isHalted).toBe(true);
    expect(await call({ id: 3, type: "act", snapshot_id: snap.snapshot_id, action: { kind: "scroll" } }))
      .toMatchObject({ error: { code: "halted" } });
    await router.resume();
    const again = (await call({ id: 4, type: "snapshot" })).result;
    expect((await call({ id: 5, type: "act", snapshot_id: again.snapshot_id, action: { kind: "scroll" } })).result).toBeDefined();
  });

  it("lifts a halt when the host relays the desktop's Resume", async () => {
    fake.granted.add("https://shop.example");
    await call({ id: 1, type: "halt" });
    expect(router.isHalted).toBe(true);
    expect(await call({ id: 2, type: "snapshot" })).toMatchObject({ error: { code: "halted" } });
    expect(await call({ id: 3, type: "resume" })).toMatchObject({ id: 3, result: {} });
    expect(router.isHalted).toBe(false);
    expect((await call({ id: 4, type: "snapshot" })).result).toBeDefined();
  });

  it("replies with an error to malformed and oversized requests", async () => {
    expect(await call({ id: 9, type: "nope" })).toMatchObject({ id: 9, error: { code: "bad_request" } });
    expect(await call("garbage")).toMatchObject({ id: 0, error: { code: "bad_request" } });
    const big = { id: 10, type: "snapshot", pad: "x".repeat(1024 * 1024) };
    expect(await call(big)).toMatchObject({ id: 10, error: { code: "too_large" } });
  });
});
