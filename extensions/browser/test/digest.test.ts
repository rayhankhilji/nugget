import { describe, expect, it } from "vitest";
import { PageAgent } from "../src/agent";
import { formDigest, scopeDigest, sha256Hex } from "../src/digest";
import { parseHostRequest } from "../src/framing";
import { ProtocolError, type PageSnapshot } from "../src/protocol";
import { dom, fixture, measure } from "./helpers";

const enc = (s: string) => new TextEncoder().encode(s);

describe("sha256Hex", () => {
  it("matches the standard test vectors", () => {
    expect(sha256Hex(enc(""))).toBe("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    expect(sha256Hex(enc("abc"))).toBe("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    expect(sha256Hex(enc("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"))).toBe(
      "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
    );
    expect(sha256Hex(enc("a".repeat(1000)))).toBe(sha256Hex(enc("a".repeat(1000))));
  });
});

function setup() {
  const doc = dom(fixture("form.html"));
  const agent = new PageAgent(doc, { measure, settleMs: 0, token: "t0", salt: "s" });
  const snap = agent.snapshot();
  const form = doc.getElementById("signup") as HTMLFormElement;
  return { doc, agent, snap, form };
}

function goId(s: PageSnapshot): string {
  return s.elements.find((e) => e.name === "Create account")!.id;
}

describe("formDigest", () => {
  it("covers hidden inputs, full values, the action and the salt", () => {
    const { doc, form } = setup();
    const base = formDigest(form, "s");
    expect(formDigest(form, "s")).toBe(base);
    expect(formDigest(form, "other")).not.toBe(base);
    (doc.querySelector("input[name=csrf]") as HTMLInputElement).value = "changed";
    expect(formDigest(form, "s")).not.toBe(base);
    (doc.querySelector("input[name=csrf]") as HTMLInputElement).value = "tok";
    expect(formDigest(form, "s")).toBe(base);
    const bio = doc.getElementById("bio") as HTMLTextAreaElement;
    bio.value = "x".repeat(600);
    const long = formDigest(form, "s");
    bio.value = "x".repeat(599) + "y";
    expect(formDigest(form, "s")).not.toBe(long);
    form.setAttribute("action", "https://evil.example/collect");
    expect(formDigest(form, "s")).not.toBe(formDigest(dom(fixture("form.html")).getElementById("signup") as HTMLFormElement, "s"));
  });

  it("is listed per form in the snapshot", () => {
    const { snap, form } = setup();
    expect(snap.forms).toHaveLength(1);
    expect(snap.forms[0]).toMatchObject({ method: "post", digest: formDigest(form, "s") });
    expect(snap.forms[0]!.action).toBe("https://shop.example/signup");
  });
});

describe("act with an expected form", () => {
  it("refuses a submit when the form changed after confirmation, synchronously before dispatch", async () => {
    const { doc, agent, snap, form } = setup();
    const expected = { form: snap.forms[0]!.id, digest: snap.forms[0]!.digest! };
    let submitted = 0;
    form.addEventListener("submit", (e) => {
      submitted += 1;
      e.preventDefault();
    });
    (doc.querySelector("input[name=csrf]") as HTMLInputElement).value = "attacker";
    const err = await agent.act(snap.snapshot_id, { kind: "submit", element: goId(snap) }, expected).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ProtocolError);
    expect((err as ProtocolError).code).toBe("form_changed");
    expect(submitted).toBe(0);
  });

  it("dispatches an unchanged form", async () => {
    const { agent, snap, form } = setup();
    let submitted = 0;
    form.addEventListener("submit", (e) => {
      submitted += 1;
      e.preventDefault();
    });
    const expected = { form: snap.forms[0]!.id, digest: snap.forms[0]!.digest! };
    await agent.act(snap.snapshot_id, { kind: "click", element: goId(snap) }, expected);
    expect(submitted).toBe(1);
  });

  it("parses expect_form from the host", () => {
    const req = parseHostRequest({
      id: 1,
      type: "act",
      snapshot_id: "a.1",
      action: { kind: "submit", element: "n1" },
      expect_form: { form: "n9", digest: "ab" },
    });
    expect(req).toMatchObject({ expect_form: { form: "n9", digest: "ab" } });
    expect(() => parseHostRequest({ id: 1, type: "act", snapshot_id: "a.1", action: { kind: "click", element: "n1" }, expect_form: 3 })).toThrow();
  });
});

describe("DOM clobbering (second review H1)", () => {
  // jsdom does not implement a form's named-property override, so simulate it on the instance:
  // a page with <input name="action">, <input name="elements"> or <input name="getAttribute">.
  function clobber(form: HTMLFormElement) {
    const input = form.querySelector("input")!;
    Object.defineProperty(form, "action", { value: input, configurable: true });
    Object.defineProperty(form, "method", { value: input, configurable: true });
    Object.defineProperty(form, "elements", { value: [], configurable: true });
    Object.defineProperty(form, "getAttribute", {
      value: () => {
        throw new Error("clobbered");
      },
      configurable: true,
    });
    Object.defineProperty(form, "requestSubmit", { value: input, configurable: true });
  }

  it("snapshots still work and the digest still covers the controls", () => {
    const { doc, agent, form } = setup();
    const clean = formDigest(form, "s");
    clobber(form);
    expect(formDigest(form, "s")).toBe(clean);
    const snap = agent.snapshot();
    expect(snap.forms[0]).toMatchObject({ action: "https://shop.example/signup", method: "post", digest: clean });
    expect(typeof snap.forms[0]!.action).toBe("string");
    (doc.querySelector("input[name=csrf]") as HTMLInputElement).value = "attacker";
    expect(formDigest(form, "s")).not.toBe(clean);
  });

  it("a clobbered requestSubmit does not stop a confirmed submit", async () => {
    const { agent, snap, form } = setup();
    let submitted = 0;
    form.addEventListener("submit", (e) => {
      submitted += 1;
      e.preventDefault();
    });
    clobber(form);
    const expected = { form: snap.forms[0]!.id, digest: snap.forms[0]!.digest! };
    await agent.act(snap.snapshot_id, { kind: "submit", element: goId(snap) }, expected);
    expect(submitted).toBe(1);
  });
});

describe("send scopes (second review H2) and the last-moment check (L2)", () => {
  const html = `<!doctype html><html><body>
    <div role="dialog" aria-label="New message">
      <input aria-label="To" value="bob@ourco.example">
      <div contenteditable="true" aria-label="Message body" id="body">Hi Bob</div>
      <div role="button" tabindex="0" id="send">Send</div>
    </div>
    <div role="button" tabindex="0" id="other">Elsewhere</div>
  </body></html>`;

  function compose() {
    const doc = dom(html);
    const agent = new PageAgent(doc, { measure, settleMs: 0, token: "t0", salt: "s" });
    const snap = agent.snapshot();
    const send = snap.elements.find((e) => e.name === "Send")!.id;
    const scope = snap.scopes.find((s) => s.members.includes(send))!;
    return { doc, agent, snap, send, scope };
  }

  it("lists the dialog around a send with its members and a digest of the body text", () => {
    const { doc, snap, send, scope } = compose();
    expect(scope.members).toContain(send);
    expect(scope.members).toContain(snap.elements.find((e) => e.name === "To")!.id);
    expect(scope.members).not.toContain(snap.elements.find((e) => e.name === "Elsewhere")!.id);
    const dialog = doc.querySelector("[role=dialog]")!;
    expect(scope.digest).toBe(scopeDigest(dialog, "s"));
    doc.getElementById("body")!.textContent = "Wire $9,800 today";
    expect(scopeDigest(dialog, "s")).not.toBe(scope.digest);
  });

  it("refuses a send whose body changed, even from the target's own focus handler", async () => {
    const { doc, agent, snap, send, scope } = compose();
    let clicked = 0;
    const button = doc.getElementById("send")!;
    button.addEventListener("click", () => (clicked += 1));
    // The page rewrites the body when the Send button gets focus: after any early check.
    button.addEventListener("focus", () => {
      doc.getElementById("body")!.textContent = "Wire $9,800 today";
    });
    const expected = { form: scope.id, digest: scope.digest! };
    const err = await agent.act(snap.snapshot_id, { kind: "click", element: send }, undefined, expected).catch((e: unknown) => e);
    expect((err as ProtocolError).code).toBe("form_changed");
    expect(clicked).toBe(0);
  });

  it("sends an unchanged message", async () => {
    const { doc, agent, snap, send, scope } = compose();
    let clicked = 0;
    doc.getElementById("send")!.addEventListener("click", () => (clicked += 1));
    await agent.act(snap.snapshot_id, { kind: "click", element: send }, undefined, { form: scope.id, digest: scope.digest! });
    expect(clicked).toBe(1);
  });
});
