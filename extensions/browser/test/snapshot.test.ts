import { describe, expect, it } from "vitest";
import { buildSnapshot } from "../src/snapshot";
import { LIMITS } from "../src/protocol";
import { createOverlay } from "../src/overlay";
import { dom, fixture, measure, registry } from "./helpers";

function snap(doc: Document, reg = registry(), opts = {}) {
  return buildSnapshot(doc, { registry: reg, measure, now: () => 42 }, opts);
}

function byName(s: ReturnType<typeof snap>, name: string) {
  const el = s.elements.find((e) => e.name === name);
  if (!el) throw new Error(`no element named ${name}: ${s.elements.map((e) => e.name).join(", ")}`);
  return el;
}

describe("form snapshot", () => {
  const doc = dom(fixture("form.html"));
  const s = snap(doc);

  it("captures url, title and snapshot id", () => {
    expect(s.url).toBe("https://shop.example/page");
    expect(s.title).toBe("Sign up");
    expect(s.snapshot_id).toBe("t0.1");
    expect(s.captured_at_ms).toBe(42);
  });

  it("derives roles and accessible names", () => {
    expect(byName(s, "Log in")).toMatchObject({ role: "link", tag: "a", href: "https://shop.example/login" });
    expect(byName(s, "Email address")).toMatchObject({ role: "textbox", input_type: "email", value: "ada@example.com", editable: true });
    expect(byName(s, "Full name")).toMatchObject({ role: "textbox", value: "" });
    expect(byName(s, "Create account")).toMatchObject({ role: "button", tag: "button" });
    expect(byName(s, "Custom control")).toMatchObject({ role: "button", tag: "div" });
    expect(byName(s, "I agree to the terms")).toMatchObject({ role: "checkbox", checked: false });
  });

  it("lists select options and the current value", () => {
    const plan = byName(s, "Plan");
    expect(plan.role).toBe("combobox");
    expect(plan.value).toBe("pro");
    expect(plan.options).toEqual([
      { value: "free", label: "Free", selected: false },
      { value: "pro", label: "Pro", selected: true },
    ]);
  });

  it("associates controls with their form", () => {
    const formId = byName(s, "Email address").form;
    expect(formId).toMatch(/^n\d+$/);
    expect(byName(s, "Create account").form).toBe(formId);
    expect(byName(s, "Log in").form).toBeUndefined();
  });

  it("marks password fields and never reads their values", () => {
    const pw = byName(s, "Password");
    expect(pw.sensitive).toBe("password");
    expect(pw.value).toBeUndefined();
    const otp = byName(s, "Code");
    expect(otp.sensitive).toBe("password");
    expect(otp.value).toBeUndefined();
    const json = JSON.stringify(s);
    expect(json).not.toContain("hunter2-secret");
    expect(json).not.toContain("123456");
  });

  it("keeps textarea contents and scripts out of the page text", () => {
    expect(s.text).toContain("Create your account");
    expect(s.text).toContain("Already a member? Log in");
    expect(s.text).not.toContain("My private bio");
    expect(s.text).not.toContain("should not appear");
    // ...but the textarea itself is listed with its value.
    expect(s.elements.find((e) => e.tag === "textarea")?.value).toBe("My private bio");
  });

  it("skips hidden inputs and hidden subtrees, lists visibility:hidden as not visible", () => {
    expect(s.elements.some((e) => e.input_type === "hidden")).toBe(false);
    expect(s.elements.some((e) => e.name === "Hidden")).toBe(false);
    expect(s.elements.some((e) => e.name === "Hidden link")).toBe(false);
    expect(byName(s, "Ghost").visible).toBe(false);
    expect(byName(s, "Create account").visible).toBe(true);
    expect(s.text).not.toContain("Ghost");
  });

  it("includes hidden subtrees on request", () => {
    const all = snap(doc, registry(), { include_hidden: true });
    expect(byName(all, "Hidden").visible).toBe(false);
  });

  it("marks disabled controls", () => {
    expect(byName(s, "Disabled").disabled).toBe(true);
  });

  it("treats contenteditable roots as textboxes", () => {
    const ed = s.elements.find((e) => e.tag === "div" && e.role === "textbox");
    expect(ed).toMatchObject({ editable: true, value: "Draft text" });
    expect(s.elements.filter((e) => e.tag === "b")).toHaveLength(0);
  });

  it("keeps element ids stable across snapshots", () => {
    const reg = registry();
    const a = snap(doc, reg);
    const b = snap(doc, reg);
    expect(b.snapshot_id).toBe("t0.2");
    expect(b.elements.map((e) => e.id)).toEqual(a.elements.map((e) => e.id));
    expect(new Set(a.elements.map((e) => e.id)).size).toBe(a.elements.length);
  });

  it("treats zero-size elements as not visible", () => {
    const s0 = buildSnapshot(doc, { registry: registry(), measure: () => ({ x: 0, y: 0, width: 0, height: 0 }) });
    expect(s0.elements.every((e) => !e.visible)).toBe(true);
  });
});

describe("payment fields", () => {
  const s = snap(dom(fixture("checkout.html")));

  it("marks card, cvc, expiry, cardholder and iban as payment and omits values", () => {
    for (const name of ["Card number", "CVC", "Name on card", "IBAN"]) {
      expect(byName(s, name)).toMatchObject({ sensitive: "payment" });
      expect(byName(s, name).value).toBeUndefined();
    }
    const exp = s.elements.find((e) => e.name === "MM/YY");
    expect(exp?.sensitive).toBe("payment");
    const json = JSON.stringify(s);
    for (const secret of ["4111111111111111", "DE89370400440532013000", "12/30", "Ada L"]) {
      expect(json).not.toContain(secret);
    }
  });

  it("does not over-mark ordinary fields", () => {
    expect(byName(s, "Coupon")).toMatchObject({ value: "SAVE10" });
    expect(byName(s, "Coupon").sensitive).toBeUndefined();
  });
});

describe("shadow DOM", () => {
  it("traverses open shadow roots and not closed ones", () => {
    const doc = dom("<body><div id=open></div><div id=closed></div></body>");
    const open = doc.getElementById("open")!.attachShadow({ mode: "open" });
    open.innerHTML = `<label for="q">Search</label><input id="q" type="search" value="cats"><button>Go</button>`;
    const closed = doc.getElementById("closed")!.attachShadow({ mode: "closed" });
    closed.innerHTML = `<button>Secret button</button>`;
    const s = snap(doc);
    expect(byName(s, "Search")).toMatchObject({ role: "searchbox", value: "cats", in_shadow: true });
    expect(byName(s, "Go").in_shadow).toBe(true);
    expect(s.elements.some((e) => e.name === "Secret button")).toBe(false);
  });

  it("resolves aria-labelledby inside the shadow root", () => {
    const doc = dom("<body><div id=host></div></body>");
    const root = doc.getElementById("host")!.attachShadow({ mode: "open" });
    root.innerHTML = `<span id="lbl">Quantity</span><input aria-labelledby="lbl" type="number" value="2">`;
    expect(byName(snap(doc), "Quantity")).toMatchObject({ role: "spinbutton", value: "2" });
  });
});

describe("iframe policy", () => {
  it("traverses same-origin frames and lists cross-origin frames as opaque", () => {
    const doc = dom(`<body><iframe id="same"></iframe><iframe id="other" src="https://pay.thirdparty.example/widget"></iframe></body>`);
    const same = doc.getElementById("same") as HTMLIFrameElement;
    same.contentDocument!.body.innerHTML = `<button>Inside same</button><p>frame text</p>`;
    const other = doc.getElementById("other") as HTMLIFrameElement;
    other.contentDocument?.body?.insertAdjacentHTML("beforeend", "<button>Inside other</button>");

    const s = snap(doc);
    expect(s.frames).toHaveLength(2);
    const [f1, f2] = s.frames;
    expect(f1).toMatchObject({ same_origin: true, traversed: true, origin: "https://shop.example" });
    expect(f2).toMatchObject({ same_origin: false, traversed: false, origin: "https://pay.thirdparty.example" });
    expect(byName(s, "Inside same").frame).toBe(f1!.id);
    expect(s.elements.some((e) => e.name === "Inside other")).toBe(false);
    expect(s.text).toContain("frame text");
  });
});

describe("caps", () => {
  it("caps the number of elements", () => {
    const doc = dom(`<body>${"<button>b</button>".repeat(LIMITS.maxElements + 10)}</body>`);
    const s = snap(doc);
    expect(s.elements).toHaveLength(LIMITS.maxElements);
    expect(s.elements_truncated).toBe(true);
  });

  it("caps text and long names", () => {
    const long = "word ".repeat(LIMITS.maxTextChars);
    const doc = dom(`<body><p>${long}</p><button>${"x".repeat(500)}</button></body>`);
    const s = snap(doc);
    expect(s.text.length).toBeLessThanOrEqual(LIMITS.maxTextChars);
    expect(s.text_truncated).toBe(true);
    expect(s.elements[0]!.name).toHaveLength(LIMITS.maxNameChars);
  });

  it("can omit text", () => {
    const s = snap(dom("<body><p>hello</p></body>"), registry(), { include_text: false });
    expect(s.text).toBe("");
  });
});

describe("overlay", () => {
  it("is excluded from snapshots and calls onStop", () => {
    const doc = dom("<body><button>Page</button></body>");
    let stopped = 0;
    const overlay = createOverlay(doc, () => stopped++);
    overlay.show();
    expect(overlay.visible).toBe(true);
    const s = snap(doc);
    expect(s.elements.map((e) => e.name)).toEqual(["Page"]);
    expect(s.text).not.toContain("Nugget");
    overlay.hide();
    expect(overlay.visible).toBe(false);
    expect(stopped).toBe(0);
  });
});
