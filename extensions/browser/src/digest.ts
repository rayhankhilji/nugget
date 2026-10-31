// A synchronous SHA-256 and the form digest that binds a confirmed submit to the form's exact
// contents. Synchronous on purpose: the check right before dispatching a submit must not yield
// to the page (an `await` would let page scripts rewrite the form in between).

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

/** SHA-256 of `data`, lowercase hex. */
export function sha256Hex(data: Uint8Array): string {
  const h = new Uint32Array([
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ]);
  const bitLen = data.length * 8;
  const padded = new Uint8Array(Math.ceil((data.length + 9) / 64) * 64);
  padded.set(data);
  padded[data.length] = 0x80;
  const view = new DataView(padded.buffer);
  view.setUint32(padded.length - 8, Math.floor(bitLen / 0x100000000));
  view.setUint32(padded.length - 4, bitLen >>> 0);
  const w = new Uint32Array(64);
  for (let off = 0; off < padded.length; off += 64) {
    for (let i = 0; i < 16; i++) w[i] = view.getUint32(off + i * 4);
    for (let i = 16; i < 64; i++) {
      const a = w[i - 15]!, b = w[i - 2]!;
      const s0 = ((a >>> 7) | (a << 25)) ^ ((a >>> 18) | (a << 14)) ^ (a >>> 3);
      const s1 = ((b >>> 17) | (b << 15)) ^ ((b >>> 19) | (b << 13)) ^ (b >>> 10);
      w[i] = (w[i - 16]! + s0 + w[i - 7]! + s1) >>> 0;
    }
    let a = h[0]!, b = h[1]!, c = h[2]!, d = h[3]!, e = h[4]!, f = h[5]!, g = h[6]!, hh = h[7]!;
    for (let i = 0; i < 64; i++) {
      const S1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
      const ch = (e & f) ^ (~e & g);
      const t1 = (hh + S1 + ch + K[i]! + w[i]!) >>> 0;
      const S0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) >>> 0;
      hh = g;
      g = f;
      f = e;
      e = (d + t1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) >>> 0;
    }
    h[0] = (h[0]! + a) >>> 0;
    h[1] = (h[1]! + b) >>> 0;
    h[2] = (h[2]! + c) >>> 0;
    h[3] = (h[3]! + d) >>> 0;
    h[4] = (h[4]! + e) >>> 0;
    h[5] = (h[5]! + f) >>> 0;
    h[6] = (h[6]! + g) >>> 0;
    h[7] = (h[7]! + hh) >>> 0;
  }
  return Array.from(h, (x) => x.toString(16).padStart(8, "0")).join("");
}

/** The page's own window classes for `el` (the content script's isolated world shares the DOM). */
function win(el: Element): typeof globalThis & Window {
  return (el.ownerDocument.defaultView ?? globalThis) as typeof globalThis & Window;
}

/**
 * A form property read through `HTMLFormElement.prototype`, never through the instance: a
 * control named `action`, `elements` or `getAttribute` shadows the instance property ("DOM
 * clobbering", in the isolated world too).
 */
export function formProperty<T>(form: HTMLFormElement, name: string): T | undefined {
  const d = Object.getOwnPropertyDescriptor(win(form).HTMLFormElement.prototype, name);
  return d?.get?.call(form) as T | undefined;
}

/** `Element.prototype` of `el`'s window, looked up once per document. */
const protos = new WeakMap<Document, Element>();
function elementProto(el: Element): Element {
  const doc = el.ownerDocument;
  let p = protos.get(doc);
  if (!p) {
    p = win(el).Element.prototype;
    protos.set(doc, p);
  }
  return p;
}

/** `el.getAttribute(name)` through `Element.prototype`. */
export function attr(el: Element, name: string): string | null {
  return elementProto(el).getAttribute.call(el, name);
}

/** `el.hasAttribute(name)` through `Element.prototype`. */
export function hasAttr(el: Element, name: string): boolean {
  return elementProto(el).hasAttribute.call(el, name);
}

/** `el.closest(selector)` through `Element.prototype`. */
export function closestOf(el: Element | null, selector: string): Element | null {
  return el ? elementProto(el).closest.call(el, selector) : null;
}

function controlParts(el: Element, parts: string[]): void {
  const input = el as HTMLInputElement;
  const type = String(input.type ?? attr(el, "type") ?? "").toLowerCase();
  parts.push(el.tagName, type, String(input.name ?? ""), String(attr(el, "id") ?? ""), String(Boolean(input.disabled)));
  for (const a of ["formaction", "formmethod", "formenctype", "formtarget", "form"]) {
    parts.push(attr(el, a) ?? "");
  }
  if (el.tagName === "SELECT") {
    parts.push(
      Array.from((el as HTMLSelectElement).options ?? [])
        .filter((o) => o.selected)
        .map((o) => o.value)
        .join("\u0000"),
    );
  } else if (type === "checkbox" || type === "radio") {
    parts.push(String(input.checked), String(input.value));
  } else if (type === "file") {
    parts.push(Array.from(input.files ?? [], (f) => `${f.name}:${f.size}`).join("\u0000"));
  } else if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") {
    parts.push(String(input.value));
  } else {
    // A contenteditable region or role=textbox: its text is what it sends.
    parts.push(el.textContent ?? "");
  }
}

function hashParts(parts: string[]): string {
  const text = parts.map((p) => `${p.length}:${p}`).join("|");
  return sha256Hex(new TextEncoder().encode(text));
}

/**
 * Digest of everything `form` would send and where: its action, method, encoding and target,
 * and for every control in `form.elements` (hidden ones, ones past the snapshot's caps and
 * values longer than the snapshot keeps included) its tag, type, name, id, disabled flag,
 * override attributes (`formaction`, ...) and full current value, checked state, selected
 * options or chosen files. Read through the prototypes, so controls named like form properties
 * cannot hide anything. Salted with a per-document secret, so the digest says nothing about the
 * values (a PIN) to whoever reads the snapshot; only equality is meaningful. Throws if the form
 * cannot be read; callers treat that as "no digest" (snapshot) or "changed" (act).
 *
 * Not covered: `<input type=image>` (never in `form.elements`; it sends only click
 * coordinates) and form-associated custom elements, whose submitted value is not readable.
 */
export function formDigest(form: HTMLFormElement, salt: string): string {
  const elements = formProperty<HTMLFormControlsCollection>(form, "elements");
  if (!elements) throw new Error("the form's controls cannot be read");
  const parts: string[] = [
    salt,
    String(formProperty<string>(form, "action") ?? ""),
    (attr(form, "method") ?? "get").toLowerCase(),
    String(formProperty<string>(form, "enctype") ?? ""),
    String(formProperty<string>(form, "target") ?? ""),
  ];
  for (const el of Array.from(elements)) controlParts(el, parts);
  return hashParts(parts);
}

/** What a send outside a native form submit carries: the nearest dialog, form or region. */
export const SCOPE_SELECTOR =
  'dialog, [role="dialog"], [role="alertdialog"], form, [role="form"], [role="region"], section[aria-label], section[aria-labelledby]';

const SCOPE_FIELDS = 'input, textarea, select, [contenteditable]:not([contenteditable="false"]), [role="textbox"]';

/**
 * Digest of the editable contents of a send scope (a compose dialog, a region): every input,
 * text area, select, contenteditable region and role=textbox inside it, with full values and
 * text. Same salting and caveats as [`formDigest`].
 */
export function scopeDigest(scope: Element, salt: string): string {
  const parts: string[] = [salt, scope.tagName, attr(scope, "role") ?? ""];
  const all = win(scope).Element.prototype.querySelectorAll.call(scope, SCOPE_FIELDS);
  for (const el of Array.from(all)) {
    // A contenteditable inside another counts once, with the outer one's text.
    if (el.parentElement && closestOf(el.parentElement, '[contenteditable]:not([contenteditable="false"])')) continue;
    controlParts(el, parts);
  }
  return hashParts(parts);
}
