import { describe, expect, it } from "vitest";
import { PageAgent } from "../src/agent";
import { formDigest, sha256Hex } from "../src/digest";
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
    const expected = { form: snap.forms[0]!.id, digest: snap.forms[0]!.digest };
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
    const expected = { form: snap.forms[0]!.id, digest: snap.forms[0]!.digest };
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
