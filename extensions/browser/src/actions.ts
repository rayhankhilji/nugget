// Action validation (against the latest snapshot) and execution in the page.

import { LIMITS, ProtocolError, type BrowserAction, type ElementInfo, type PageSnapshot, type WaitCondition } from "./protocol";
import type { ElementRegistry } from "./registry";
import { accessibleName, roleOf } from "./snapshot";

/** URL schemes `navigate` accepts. Everything else (javascript:, data:, file:, chrome:, ...) is refused. */
export function checkNavigateUrl(raw: string, base?: string): URL {
  let url: URL;
  try {
    url = new URL(raw, base);
  } catch {
    throw new ProtocolError("bad_url", `not a URL: ${raw.slice(0, 200)}`);
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") {
    throw new ProtocolError("bad_url", `scheme ${url.protocol} is not allowed`);
  }
  if (url.username || url.password) throw new ProtocolError("bad_url", "credentials in URLs are not allowed");
  return url;
}

function element(snapshot: PageSnapshot, id: string): ElementInfo {
  const el = snapshot.elements.find((e) => e.id === id);
  if (!el) throw new ProtocolError("unknown_element", `element ${id} is not in snapshot ${snapshot.snapshot_id}`);
  return el;
}

function usable(el: ElementInfo): void {
  if (!el.visible) throw new ProtocolError("not_visible", `element ${el.id} is not visible`);
  if (el.disabled) throw new ProtocolError("disabled", `element ${el.id} is disabled`);
}

/**
 * Check `action` against `latest`, the last snapshot handed to the host. The host must name that
 * snapshot: acting on ids from an older view of the page is refused (`stale_snapshot`).
 * Returns the target element, if the action has one.
 */
export function validateAction(
  action: BrowserAction,
  snapshotId: string,
  latest: PageSnapshot | undefined,
): ElementInfo | undefined {
  if (!latest || latest.snapshot_id !== snapshotId) {
    throw new ProtocolError("stale_snapshot", `snapshot ${snapshotId} is not the latest; take a new snapshot`);
  }
  switch (action.kind) {
    case "click": {
      const el = element(latest, action.element);
      usable(el);
      return el;
    }
    case "type": {
      const el = element(latest, action.element);
      usable(el);
      if (el.sensitive) {
        throw new ProtocolError("sensitive_field", `element ${el.id} is a ${el.sensitive} field; Nugget never types into those`);
      }
      if (!el.editable) throw new ProtocolError("not_editable", `element ${el.id} is not editable`);
      if (action.text.length > LIMITS.maxTypeChars) {
        throw new ProtocolError("bad_request", `text longer than ${LIMITS.maxTypeChars} characters`);
      }
      return el;
    }
    case "select": {
      const el = element(latest, action.element);
      usable(el);
      if (!el.options) throw new ProtocolError("not_editable", `element ${el.id} is not a select`);
      if (!el.options.some((o) => o.value === action.value || o.label === action.value)) {
        throw new ProtocolError("bad_option", `element ${el.id} has no option ${JSON.stringify(action.value)}`);
      }
      return el;
    }
    case "scroll": {
      if (action.element === undefined) return undefined;
      return element(latest, action.element);
    }
    case "navigate":
      checkNavigateUrl(action.url, latest.url);
      return undefined;
    case "submit": {
      const el = element(latest, action.element);
      if (!el.form) throw new ProtocolError("not_editable", `element ${el.id} is not inside a form`);
      if (el.disabled) throw new ProtocolError("disabled", `element ${el.id} is disabled`);
      return el;
    }
    case "wait_for":
      if (action.timeout_ms !== undefined && (action.timeout_ms < 0 || action.timeout_ms > LIMITS.maxWaitMs)) {
        throw new ProtocolError("bad_request", `timeout_ms must be between 0 and ${LIMITS.maxWaitMs}`);
      }
      if ("element_gone" in action.until) element(latest, action.until.element_gone);
      return undefined;
  }
}

function live(registry: ElementRegistry, id: string): Element {
  const el = registry.get(id);
  if (!el) throw new ProtocolError("unknown_element", `element ${id} is no longer in the page`);
  return el;
}

function setNativeValue(el: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement, value: string): void {
  // Use the prototype setter so frameworks that track the value property (React) see the change.
  const win = el.ownerDocument.defaultView!;
  const proto =
    el.tagName === "TEXTAREA" ? win.HTMLTextAreaElement.prototype
    : el.tagName === "SELECT" ? win.HTMLSelectElement.prototype
    : win.HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
  if (setter) setter.call(el, value);
  else el.value = value;
}

function fire(el: Element, type: string): void {
  const win = el.ownerDocument.defaultView!;
  el.dispatchEvent(new win.Event(type, { bubbles: true, composed: true }));
}

function scrollIntoViewIfPossible(el: Element): void {
  if (typeof (el as HTMLElement).scrollIntoView === "function") {
    (el as HTMLElement).scrollIntoView({ block: "center", inline: "nearest" });
  }
}

/** Perform an already validated action. `navigate` is handled by the background worker. */
export async function performAction(registry: ElementRegistry, doc: Document, action: BrowserAction): Promise<void> {
  switch (action.kind) {
    case "click": {
      const el = live(registry, action.element) as HTMLElement;
      scrollIntoViewIfPossible(el);
      el.focus?.();
      el.click();
      return;
    }
    case "type": {
      const el = live(registry, action.element) as HTMLElement;
      scrollIntoViewIfPossible(el);
      el.focus?.();
      if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") {
        const input = el as HTMLInputElement;
        const next = action.clear === false ? input.value + action.text : action.text;
        setNativeValue(input, next);
      } else {
        el.textContent = action.clear === false ? (el.textContent ?? "") + action.text : action.text;
      }
      fire(el, "input");
      fire(el, "change");
      return;
    }
    case "select": {
      const el = live(registry, action.element) as HTMLSelectElement;
      const opt = Array.from(el.options).find((o) => o.value === action.value)
        ?? Array.from(el.options).find((o) => (o.textContent ?? "").trim() === action.value);
      if (!opt) throw new ProtocolError("bad_option", `no option ${JSON.stringify(action.value)}`);
      el.focus?.();
      setNativeValue(el, opt.value);
      fire(el, "input");
      fire(el, "change");
      return;
    }
    case "scroll": {
      const win = doc.defaultView!;
      const dx = action.dx ?? 0;
      const dy = action.dy ?? (action.dx === undefined ? Math.round((win.innerHeight || 800) * 0.8) : 0);
      if (action.element !== undefined) {
        const el = live(registry, action.element);
        if (action.dx === undefined && action.dy === undefined) scrollIntoViewIfPossible(el);
        else if (typeof el.scrollBy === "function") el.scrollBy(dx, dy);
      } else if (typeof win.scrollBy === "function") {
        try {
          win.scrollBy(dx, dy);
        } catch {
          // jsdom does not implement scrolling; nothing to do.
        }
      }
      return;
    }
    case "submit": {
      const el = live(registry, action.element);
      const form = (el as HTMLInputElement).form ?? el.closest("form");
      if (!form) throw new ProtocolError("not_editable", "element is not inside a form");
      const submitter =
        el.tagName === "BUTTON" || (el.tagName === "INPUT" && ["submit", "image"].includes((el as HTMLInputElement).type))
          ? (el as HTMLButtonElement)
          : undefined;
      if (typeof form.requestSubmit === "function") form.requestSubmit(submitter);
      else form.submit();
      return;
    }
    case "wait_for":
      await waitFor(registry, doc, action.until, action.timeout_ms ?? 5000);
      return;
    case "navigate":
      throw new ProtocolError("internal", "navigate is performed by the background worker");
  }
}

export function conditionMet(registry: ElementRegistry, doc: Document, until: WaitCondition): boolean {
  if ("text_present" in until) {
    return (doc.body?.textContent ?? "").toLowerCase().includes(until.text_present.toLowerCase());
  }
  if ("url_contains" in until) return doc.URL.includes(until.url_contains);
  if ("element_gone" in until) return registry.get(until.element_gone) === undefined;
  const { role, name } = until.element_present;
  const want = name?.toLowerCase();
  const all = collect(doc.body);
  return all.some((el) => (role === undefined || roleOf(el) === role) &&
    (want === undefined || accessibleName(el).toLowerCase().includes(want)));
}

function collect(root: Element | null): Element[] {
  const out: Element[] = [];
  const visit = (el: Element) => {
    out.push(el);
    if (el.shadowRoot) for (const c of Array.from(el.shadowRoot.children)) visit(c);
    for (const c of Array.from(el.children)) visit(c);
  };
  if (root) visit(root);
  return out;
}

async function waitFor(registry: ElementRegistry, doc: Document, until: WaitCondition, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + Math.min(timeoutMs, LIMITS.maxWaitMs);
  for (;;) {
    if (conditionMet(registry, doc, until)) return;
    if (Date.now() >= deadline) throw new ProtocolError("timeout", "condition not met before the timeout");
    await new Promise((r) => setTimeout(r, 100));
  }
}
