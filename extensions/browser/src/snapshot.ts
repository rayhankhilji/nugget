// DOM snapshot: a flattened accessibility-ish list of interactive elements plus capped page text.
//
// Rules:
// - Password and payment fields are marked `sensitive` and their values are never read.
// - Open shadow roots are traversed; closed ones are invisible to us by design.
// - Iframes: same-origin frames are traversed (the user's per-site consent covers them);
//   cross-origin frames are listed as opaque and never read.
// - Hidden subtrees (`hidden`, `display: none`, `aria-hidden="true"`) are skipped unless
//   `include_hidden` is set; `visibility: hidden` and zero-size elements are listed as not visible.

import { ElementRegistry } from "./registry";
import {
  type ElementInfo,
  type FrameInfo,
  LIMITS,
  type PageSnapshot,
  type Rect,
  type SelectOption,
  type Sensitive,
  type SnapshotOptions,
} from "./protocol";

export interface SnapshotContext {
  registry: ElementRegistry;
  /** Override for layout (jsdom has none). Defaults to `getBoundingClientRect`. */
  measure?: (el: Element) => Rect;
  now?: () => number;
}

const SKIP_TAGS = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "HEAD", "META", "LINK"]);
const BLOCK_TAGS = new Set([
  "ADDRESS", "ARTICLE", "ASIDE", "BLOCKQUOTE", "BR", "DD", "DIV", "DL", "DT", "FIELDSET", "FIGCAPTION",
  "FIGURE", "FOOTER", "FORM", "H1", "H2", "H3", "H4", "H5", "H6", "HEADER", "HR", "LI", "MAIN", "NAV",
  "OL", "P", "PRE", "SECTION", "TABLE", "TR", "UL",
]);
const INTERACTIVE_ROLES = new Set([
  "button", "checkbox", "combobox", "link", "listbox", "menuitem", "menuitemcheckbox", "menuitemradio",
  "option", "radio", "searchbox", "slider", "spinbutton", "switch", "tab", "textbox", "treeitem",
]);
const PASSWORD_AUTOCOMPLETE = new Set(["current-password", "new-password", "one-time-code"]);
const PAYMENT_AUTOCOMPLETE = /^cc-|^transaction-/;
const PAYMENT_HINT = /(card|cc)[-_ ]?(num|number|no\b)|\bcvc|\bcvv|\bcsc\b|security[-_ ]?code|expir|\bexp[-_ ]?(date|month|year|mm|yy)|\bmm ?\/ ?yy|\biban\b|routing[-_ ]?number|account[-_ ]?number/i;
const PASSWORD_HINT = /passw(or)?d|passcode|\bpin\b|\botp\b/i;

export function buildSnapshot(doc: Document, ctx: SnapshotContext, options: SnapshotOptions = {}): PageSnapshot {
  const now = ctx.now ?? Date.now;
  const walker = new Walker(doc, ctx, options);
  walker.walkDocument(doc, undefined);
  const win = doc.defaultView;
  return {
    snapshot_id: ctx.registry.nextSnapshotId(),
    url: doc.URL,
    title: doc.title,
    captured_at_ms: now(),
    viewport: {
      width: win?.innerWidth ?? 0,
      height: win?.innerHeight ?? 0,
      scroll_x: win?.scrollX ?? 0,
      scroll_y: win?.scrollY ?? 0,
    },
    elements: walker.elements,
    elements_truncated: walker.elementsTruncated,
    text: options.include_text === false ? "" : walker.text.trim(),
    text_truncated: walker.textTruncated,
    frames: walker.frames,
  };
}

class Walker {
  elements: ElementInfo[] = [];
  frames: FrameInfo[] = [];
  text = "";
  elementsTruncated = false;
  textTruncated = false;
  private readonly topOrigin: string;

  constructor(
    private readonly topDoc: Document,
    private readonly ctx: SnapshotContext,
    private readonly options: SnapshotOptions,
  ) {
    this.topOrigin = originOf(topDoc);
  }

  walkDocument(doc: Document, frameId: string | undefined): void {
    const root = doc.body ?? doc.documentElement;
    if (root) this.walk(root, frameId, false, false);
  }

  private walk(node: Node, frameId: string | undefined, inShadow: boolean, invisible: boolean): void {
    if (node.nodeType === 3 /* TEXT_NODE */) {
      if (!invisible) this.appendText(node.nodeValue ?? "");
      return;
    }
    if (node.nodeType !== 1 /* ELEMENT_NODE */) return;
    const el = node as Element;
    if (SKIP_TAGS.has(el.tagName) || el.hasAttribute("data-nugget-overlay")) return;

    const style = el.ownerDocument.defaultView?.getComputedStyle(el);
    const removed =
      el.hasAttribute("hidden") || style?.display === "none" || el.getAttribute("aria-hidden") === "true";
    if (removed && !this.options.include_hidden) return;
    const nowInvisible = invisible || removed || style?.visibility === "hidden";

    if (el.tagName === "IFRAME" || el.tagName === "FRAME") {
      this.visitFrame(el as HTMLIFrameElement, nowInvisible);
      return;
    }

    if (isInteractive(el)) this.addElement(el, frameId, inShadow, nowInvisible);

    const block = BLOCK_TAGS.has(el.tagName);
    if (block) this.appendText("\n");
    // Form controls' contents are their values, which `addElement` handles; don't leak a
    // textarea's text (it may be sensitive) or option labels into the page text.
    if (el.tagName !== "TEXTAREA" && el.tagName !== "SELECT") {
      if (el.shadowRoot) {
        for (const child of Array.from(el.shadowRoot.childNodes)) this.walk(child, frameId, true, nowInvisible);
      }
      for (const child of Array.from(el.childNodes)) this.walk(child, frameId, inShadow, nowInvisible);
    }
    if (block) this.appendText("\n");
  }

  private visitFrame(frame: HTMLIFrameElement, invisible: boolean): void {
    const id = this.ctx.registry.idFor(frame);
    let doc: Document | null = null;
    try {
      doc = frame.contentDocument;
    } catch {
      doc = null;
    }
    // A frame whose `src` points at another origin is cross-origin even while its initial
    // about:blank document is still (accessibly) in place.
    const srcOrigin = originFromSrc(frame, this.topDoc);
    const docOrigin = doc ? originOf(doc) : "null";
    const inherits = doc !== null && /^about:/.test(doc.URL);
    const sameOrigin =
      doc !== null &&
      (srcOrigin === "null" || srcOrigin === this.topOrigin) &&
      (docOrigin === this.topOrigin || inherits);
    this.frames.push({
      id,
      origin: srcOrigin !== "null" ? srcOrigin : sameOrigin ? this.topOrigin : docOrigin,
      same_origin: sameOrigin,
      traversed: sameOrigin && !invisible,
      rect: this.measure(frame),
    });
    if (sameOrigin && doc && !invisible) this.walkDocument(doc, id);
  }

  private addElement(el: Element, frameId: string | undefined, inShadow: boolean, invisible: boolean): void {
    if (this.elements.length >= LIMITS.maxElements) {
      this.elementsTruncated = true;
      return;
    }
    const rect = this.measure(el);
    const sensitive = sensitivity(el);
    const info: ElementInfo = {
      id: this.ctx.registry.idFor(el),
      role: roleOf(el),
      name: cap(accessibleName(el), LIMITS.maxNameChars),
      tag: el.tagName.toLowerCase(),
      disabled: isDisabled(el),
      editable: isEditable(el),
      focused: el.ownerDocument.activeElement === el,
      visible: !invisible && rect.width > 0 && rect.height > 0,
      rect,
      in_shadow: inShadow,
    };
    if (el.tagName === "INPUT") info.input_type = (el as HTMLInputElement).type;
    if (sensitive) {
      info.sensitive = sensitive;
    } else {
      const value = valueOf(el);
      if (value !== undefined) info.value = cap(value, LIMITS.maxValueChars);
    }
    const checked = checkedOf(el);
    if (checked !== undefined) info.checked = checked;
    if (el.tagName === "SELECT") info.options = optionsOf(el as HTMLSelectElement);
    if (el.tagName === "A" && el.hasAttribute("href")) info.href = (el as HTMLAnchorElement).href;
    const form = formOf(el);
    if (form) info.form = this.ctx.registry.idFor(form);
    if (frameId) info.frame = frameId;
    this.elements.push(info);
  }

  private measure(el: Element): Rect {
    const r = this.ctx.measure ? this.ctx.measure(el) : el.getBoundingClientRect();
    return { x: round(r.x), y: round(r.y), width: round(r.width), height: round(r.height) };
  }

  private appendText(s: string): void {
    if (this.textTruncated || this.options.include_text === false) return;
    const collapsed = s === "\n" ? s : s.replace(/\s+/g, " ");
    if (collapsed === " " || collapsed === "") {
      if (!this.text.endsWith(" ") && !this.text.endsWith("\n") && this.text !== "") this.text += " ";
      return;
    }
    if (collapsed === "\n") {
      if (this.text !== "" && !this.text.endsWith("\n")) this.text = this.text.trimEnd() + "\n";
      return;
    }
    const room = LIMITS.maxTextChars - this.text.length;
    if (collapsed.length > room) {
      this.text += collapsed.slice(0, Math.max(0, room));
      this.textTruncated = true;
      return;
    }
    this.text += collapsed;
  }
}

function round(n: number): number {
  return Math.round(n * 10) / 10;
}

function cap(s: string, n: number): string {
  return s.length > n ? s.slice(0, n) : s;
}

function originOf(doc: Document): string {
  try {
    return new URL(doc.URL).origin;
  } catch {
    return "null";
  }
}

function originFromSrc(frame: HTMLIFrameElement, top: Document): string {
  const src = frame.getAttribute("src");
  if (!src || /^about:/.test(src)) return "null";
  try {
    const url = new URL(src, top.URL);
    return /^https?:$/.test(url.protocol) ? url.origin : "null";
  } catch {
    return "null";
  }
}

export function sensitivity(el: Element): Sensitive | undefined {
  const tag = el.tagName;
  if (tag !== "INPUT" && tag !== "TEXTAREA" && !(el as HTMLElement).isContentEditable) return undefined;
  const type = (el.getAttribute("type") ?? "").toLowerCase();
  if (type === "password") return "password";
  const autocomplete = (el.getAttribute("autocomplete") ?? "").toLowerCase().split(/\s+/);
  if (autocomplete.some((t) => PASSWORD_AUTOCOMPLETE.has(t))) return "password";
  if (autocomplete.some((t) => PAYMENT_AUTOCOMPLETE.test(t))) return "payment";
  const hints = ["name", "id", "aria-label", "placeholder", "data-testid"]
    .map((a) => el.getAttribute(a) ?? "")
    .join(" ");
  if (PAYMENT_HINT.test(hints)) return "payment";
  if (PASSWORD_HINT.test(hints)) return "password";
  return undefined;
}

function isInteractive(el: Element): boolean {
  const role = el.getAttribute("role");
  if (role && INTERACTIVE_ROLES.has(role)) return true;
  switch (el.tagName) {
    case "A":
      return el.hasAttribute("href");
    case "BUTTON":
    case "SELECT":
    case "TEXTAREA":
    case "SUMMARY":
      return true;
    case "INPUT":
      return (el.getAttribute("type") ?? "").toLowerCase() !== "hidden";
    default:
      break;
  }
  if ((el as HTMLElement).isContentEditable && !(el.parentElement as HTMLElement | null)?.isContentEditable) return true;
  const ce = el.getAttribute("contenteditable");
  if (ce !== null && ce !== "false" && !el.parentElement?.closest("[contenteditable]:not([contenteditable=false])")) {
    return true;
  }
  const tabindex = el.getAttribute("tabindex");
  if (tabindex !== null && Number(tabindex) >= 0) return true;
  return el.hasAttribute("onclick");
}

export function roleOf(el: Element): string {
  const explicit = el.getAttribute("role");
  if (explicit) return explicit.split(/\s+/)[0]!;
  switch (el.tagName) {
    case "A":
      return "link";
    case "BUTTON":
    case "SUMMARY":
      return "button";
    case "SELECT":
      return (el as HTMLSelectElement).multiple ? "listbox" : "combobox";
    case "TEXTAREA":
      return "textbox";
    case "INPUT": {
      const type = (el.getAttribute("type") ?? "text").toLowerCase();
      switch (type) {
        case "button":
        case "submit":
        case "reset":
        case "image":
          return "button";
        case "checkbox":
          return "checkbox";
        case "radio":
          return "radio";
        case "range":
          return "slider";
        case "number":
          return "spinbutton";
        case "search":
          return "searchbox";
        default:
          return "textbox";
      }
    }
    default:
      return el.getAttribute("contenteditable") !== null ? "textbox" : "generic";
  }
}

function textOf(el: Element): string {
  return (el.textContent ?? "").replace(/\s+/g, " ").trim();
}

export function accessibleName(el: Element): string {
  const doc = el.ownerDocument;
  const labelledby = el.getAttribute("aria-labelledby");
  if (labelledby) {
    const root = el.getRootNode() as Document | ShadowRoot;
    const parts = labelledby
      .split(/\s+/)
      .map((id) => root.getElementById?.(id) ?? doc.getElementById(id))
      .filter((x): x is HTMLElement => x !== null)
      .map(textOf);
    const joined = parts.join(" ").trim();
    if (joined) return joined;
  }
  const aria = el.getAttribute("aria-label")?.trim();
  if (aria) return aria;
  const labels = (el as HTMLInputElement).labels;
  if (labels && labels.length > 0) {
    const t = Array.from(labels).map(labelText).join(" ").trim();
    if (t) return t;
  }
  if (el.tagName === "INPUT") {
    const type = (el.getAttribute("type") ?? "").toLowerCase();
    if (["button", "submit", "reset"].includes(type)) {
      return (el as HTMLInputElement).value || (type === "submit" ? "Submit" : type === "reset" ? "Reset" : "");
    }
    if (type === "image") return el.getAttribute("alt") ?? "";
  }
  if (el.tagName !== "INPUT" && el.tagName !== "TEXTAREA" && el.tagName !== "SELECT") {
    const t = textOf(el);
    if (t) return t;
    const img = el.querySelector("img[alt]");
    if (img) return img.getAttribute("alt")!.trim();
  }
  return (el.getAttribute("placeholder") ?? el.getAttribute("title") ?? "").trim();
}

/** A label's text without the values of controls nested inside it. */
function labelText(label: HTMLLabelElement): string {
  let out = "";
  const visit = (n: Node) => {
    if (n.nodeType === 3) out += n.nodeValue ?? "";
    else if (n.nodeType === 1 && !["INPUT", "SELECT", "TEXTAREA"].includes((n as Element).tagName)) {
      n.childNodes.forEach(visit);
    }
  };
  visit(label);
  return out.replace(/\s+/g, " ").trim();
}

function isDisabled(el: Element): boolean {
  if ((el as HTMLButtonElement).disabled === true) return true;
  if (el.getAttribute("aria-disabled") === "true") return true;
  return el.closest("fieldset[disabled]") !== null && el.tagName !== "LEGEND";
}

export function isEditable(el: Element): boolean {
  if (el.tagName === "TEXTAREA") return !(el as HTMLTextAreaElement).readOnly;
  if (el.tagName === "INPUT") {
    const type = (el.getAttribute("type") ?? "text").toLowerCase();
    const textual = ["text", "search", "email", "url", "tel", "number", "password", "date", "time",
      "datetime-local", "month", "week", ""].includes(type);
    return textual && !(el as HTMLInputElement).readOnly;
  }
  const ce = el.getAttribute("contenteditable");
  return (el as HTMLElement).isContentEditable || (ce !== null && ce !== "false");
}

function valueOf(el: Element): string | undefined {
  if (el.tagName === "INPUT") {
    const type = (el.getAttribute("type") ?? "").toLowerCase();
    if (["checkbox", "radio", "button", "submit", "reset", "image", "file"].includes(type)) return undefined;
    return (el as HTMLInputElement).value;
  }
  if (el.tagName === "TEXTAREA") return (el as HTMLTextAreaElement).value;
  if (el.tagName === "SELECT") return (el as HTMLSelectElement).value;
  if (el.getAttribute("contenteditable") !== null && el.getAttribute("contenteditable") !== "false") {
    return textOf(el);
  }
  return undefined;
}

function checkedOf(el: Element): boolean | undefined {
  if (el.tagName === "INPUT") {
    const type = (el.getAttribute("type") ?? "").toLowerCase();
    if (type === "checkbox" || type === "radio") return (el as HTMLInputElement).checked;
  }
  const aria = el.getAttribute("aria-checked");
  if (aria === "true") return true;
  if (aria === "false") return false;
  return undefined;
}

function optionsOf(select: HTMLSelectElement): SelectOption[] {
  return Array.from(select.options)
    .slice(0, LIMITS.maxOptions)
    .map((o) => ({ value: cap(o.value, LIMITS.maxValueChars), label: cap(textOf(o), LIMITS.maxNameChars), selected: o.selected }));
}

function formOf(el: Element): HTMLFormElement | null {
  if (el.tagName === "FORM") return null;
  const owner = (el as HTMLInputElement).form;
  if (owner) return owner;
  return el.closest("form");
}
