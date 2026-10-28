import { describe, expect, it } from "vitest";
import { PageAgent } from "../src/agent";
import { checkNavigateUrl, validateAction } from "../src/actions";
import { ProtocolError, type BrowserAction, type PageSnapshot } from "../src/protocol";
import { dom, fixture, measure } from "./helpers";

function agent(html = fixture("form.html")) {
  const doc = dom(html);
  return { doc, agent: new PageAgent(doc, { measure, settleMs: 0, token: "t0" }) };
}

function id(s: PageSnapshot, name: string): string {
  const el = s.elements.find((e) => e.name === name);
  if (!el) throw new Error(`no ${name}`);
  return el.id;
}

function code(f: () => unknown): string | undefined {
  try {
    f();
  } catch (e) {
    if (e instanceof ProtocolError) return e.code;
    throw e;
  }
  return undefined;
}

async function codeAsync(p: Promise<unknown>): Promise<string | undefined> {
  try {
    await p;
  } catch (e) {
    if (e instanceof ProtocolError) return e.code;
    throw e;
  }
  return undefined;
}

describe("validateAction", () => {
  const { agent: a } = agent();
  const s = a.snapshot();
  const v = (action: BrowserAction, snapshotId = s.snapshot_id) => code(() => validateAction(action, snapshotId, s));

  it("accepts valid actions", () => {
    expect(v({ kind: "click", element: id(s, "Create account") })).toBeUndefined();
    expect(v({ kind: "type", element: id(s, "Email address"), text: "x@y.z" })).toBeUndefined();
    expect(v({ kind: "select", element: id(s, "Plan"), value: "free" })).toBeUndefined();
    expect(v({ kind: "select", element: id(s, "Plan"), value: "Pro" })).toBeUndefined();
    expect(v({ kind: "submit", element: id(s, "Email address") })).toBeUndefined();
    expect(v({ kind: "scroll" })).toBeUndefined();
    expect(v({ kind: "navigate", url: "/next" })).toBeUndefined();
    expect(v({ kind: "wait_for", until: { text_present: "x" }, timeout_ms: 1000 })).toBeUndefined();
  });

  it("refuses ids from a stale or missing snapshot", () => {
    expect(v({ kind: "click", element: id(s, "Create account") }, "t0.0")).toBe("stale_snapshot");
    expect(code(() => validateAction({ kind: "scroll" }, "t0.1", undefined))).toBe("stale_snapshot");
  });

  it("refuses unknown elements", () => {
    expect(v({ kind: "click", element: "n9999" })).toBe("unknown_element");
    expect(v({ kind: "wait_for", until: { element_gone: "n9999" } })).toBe("unknown_element");
  });

  it("refuses invisible and disabled targets", () => {
    expect(v({ kind: "click", element: id(s, "Ghost") })).toBe("not_visible");
    expect(v({ kind: "click", element: id(s, "Disabled") })).toBe("disabled");
  });

  it("never types into password or payment fields", () => {
    expect(v({ kind: "type", element: id(s, "Password"), text: "x" })).toBe("sensitive_field");
    const { agent: pay } = agent(fixture("checkout.html"));
    const ps = pay.snapshot();
    expect(code(() => validateAction({ kind: "type", element: id(ps, "Card number"), text: "4" }, ps.snapshot_id, ps))).toBe("sensitive_field");
  });

  it("refuses typing into non-editable elements and unknown options", () => {
    expect(v({ kind: "type", element: id(s, "Create account"), text: "x" })).toBe("not_editable");
    expect(v({ kind: "type", element: id(s, "Email address"), text: "x".repeat(10_001) })).toBe("bad_request");
    expect(v({ kind: "select", element: id(s, "Plan"), value: "enterprise" })).toBe("bad_option");
    expect(v({ kind: "select", element: id(s, "Email address"), value: "a" })).toBe("not_editable");
  });

  it("refuses submit outside a form and out-of-range waits", () => {
    expect(v({ kind: "submit", element: id(s, "Log in") })).toBe("not_editable");
    expect(v({ kind: "wait_for", until: { text_present: "x" }, timeout_ms: 60_000 })).toBe("bad_request");
  });

  it("only navigates to http(s) URLs without credentials", () => {
    for (const bad of ["javascript:alert(1)", "data:text/html,hi", "file:///etc/passwd", "chrome://settings", "https://u:p@x.example/"]) {
      expect(code(() => checkNavigateUrl(bad))).toBe("bad_url");
    }
    expect(checkNavigateUrl("/a", "https://shop.example/x").href).toBe("https://shop.example/a");
  });
});

describe("PageAgent.act with verification diff", () => {
  it("types into a field and reports the value change", async () => {
    const { doc, agent: a } = agent();
    const s = a.snapshot();
    const email = id(s, "Email address");
    let inputs = 0;
    doc.getElementById("email")!.addEventListener("input", () => inputs++);
    const r = await a.act(s.snapshot_id, { kind: "type", element: email, text: "grace@example.com" });
    expect((doc.getElementById("email") as HTMLInputElement).value).toBe("grace@example.com");
    expect(inputs).toBe(1);
    expect(r.before_snapshot_id).toBe(s.snapshot_id);
    expect(r.snapshot.snapshot_id).not.toBe(s.snapshot_id);
    expect(r.diff.changed).toContainEqual({ id: email, fields: expect.arrayContaining(["value"]) });
  });

  it("appends when clear is false", async () => {
    const { doc, agent: a } = agent();
    const s = a.snapshot();
    await a.act(s.snapshot_id, { kind: "type", element: id(s, "Email address"), text: ".uk", clear: false });
    expect((doc.getElementById("email") as HTMLInputElement).value).toBe("ada@example.com.uk");
  });

  it("clicks a checkbox", async () => {
    const { agent: a } = agent();
    const s = a.snapshot();
    const tos = id(s, "I agree to the terms");
    const r = await a.act(s.snapshot_id, { kind: "click", element: tos });
    expect(r.snapshot.elements.find((e) => e.id === tos)?.checked).toBe(true);
    expect(r.diff.changed.find((c) => c.id === tos)?.fields).toContain("checked");
  });

  it("selects by value or label", async () => {
    const { doc, agent: a } = agent();
    const s = a.snapshot();
    const r = await a.act(s.snapshot_id, { kind: "select", element: id(s, "Plan"), value: "Free" });
    expect((doc.getElementById("plan") as HTMLSelectElement).value).toBe("free");
    expect(r.diff.changed.find((c) => c.id === id(s, "Plan"))?.fields).toContain("value");
  });

  it("submits the owning form", async () => {
    const { doc, agent: a } = agent();
    let submitted = 0;
    doc.getElementById("signup")!.addEventListener("submit", (e) => {
      e.preventDefault();
      submitted++;
    });
    const s = a.snapshot();
    await a.act(s.snapshot_id, { kind: "submit", element: id(s, "Email address") });
    expect(submitted).toBe(1);
  });

  it("reports added and removed elements", async () => {
    const { doc, agent: a } = agent(`<body><button id="b">Show more</button><button id="x">Remove me</button></body>`);
    doc.getElementById("b")!.addEventListener("click", () => {
      doc.getElementById("x")!.remove();
      doc.body.insertAdjacentHTML("beforeend", "<a href='/more'>More results</a>");
    });
    const s = a.snapshot();
    const r = await a.act(s.snapshot_id, { kind: "click", element: id(s, "Show more") });
    expect(r.diff.removed).toEqual([id(s, "Remove me")]);
    expect(r.diff.added).toEqual([id(r.snapshot, "More results")]);
    expect(r.diff.text_changed).toBe(true);
  });

  it("waits for an element to appear", async () => {
    const { doc, agent: a } = agent(`<body><p>Loading</p></body>`);
    setTimeout(() => doc.body.insertAdjacentHTML("beforeend", "<button>Continue</button>"), 150);
    const s = a.snapshot();
    const r = await a.act(s.snapshot_id, { kind: "wait_for", until: { element_present: { role: "button", name: "continue" } }, timeout_ms: 2000 });
    expect(r.diff.added).toHaveLength(1);
  });

  it("times out waiting", async () => {
    const { agent: a } = agent(`<body><p>Loading</p></body>`);
    const s = a.snapshot();
    expect(await codeAsync(a.act(s.snapshot_id, { kind: "wait_for", until: { text_present: "never" }, timeout_ms: 200 }))).toBe("timeout");
  });

  it("refuses to act after the old snapshot is superseded", async () => {
    const { agent: a } = agent();
    const s1 = a.snapshot();
    a.snapshot();
    expect(await codeAsync(a.act(s1.snapshot_id, { kind: "click", element: id(s1, "Create account") }))).toBe("stale_snapshot");
  });

  it("refuses elements removed from the page since the snapshot", async () => {
    const { doc, agent: a } = agent();
    const s = a.snapshot();
    doc.getElementById("go")!.remove();
    expect(await codeAsync(a.act(s.snapshot_id, { kind: "click", element: id(s, "Create account") }))).toBe("unknown_element");
  });

  it("refuses everything once halted", async () => {
    const { agent: a } = agent();
    const s = a.snapshot();
    a.halt();
    expect(await codeAsync(a.act(s.snapshot_id, { kind: "scroll" }))).toBe("halted");
    a.resume();
    expect(await codeAsync(a.act(s.snapshot_id, { kind: "scroll" }))).toBeUndefined();
  });
});
