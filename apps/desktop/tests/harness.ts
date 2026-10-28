/**
 * Flow-test harness: a scriptable mock daemon plus user-level interactions and accessible
 * queries, layered on src/test/render (no testing-library, no browser). See tests/README.md.
 */
import { act, createElement } from "react";
import { afterEach, beforeEach } from "vitest";
import { mockIPC } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import { EVENT, METHOD_NOT_FOUND, type ConnectionInfo, type PageId } from "../src/api";
import { App } from "../src/App";
import { INITIAL, saveState } from "../src/onboarding/machine";
import { cleanup, flush, installDomShims, render, type Rendered } from "../src/test/render";
import { RESPONSES } from "../src/test/fixtures";

export { flush, render, cleanup } from "../src/test/render";
export { t } from "../src/i18n";

// ---------------------------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------------------------

/** Register the usual beforeEach/afterEach (DOM shims, unmount, clear mocks and storage). */
export function setupHarness(): void {
  beforeEach(() => {
    installDomShims();
    if (!Element.prototype.scrollIntoView) Element.prototype.scrollIntoView = () => {};
  });
  afterEach(cleanup);
}

/** Mark onboarding finished so `<App/>` opens straight into the shell. */
export function skipOnboarding(): void {
  saveState({ ...INITIAL, step: "done", finished: true });
}

/** Render the full app, onboarding skipped (unless asked for) and on `page`. */
export async function renderApp(opts: { page?: PageId; onboarding?: boolean } = {}): Promise<Rendered> {
  if (!opts.onboarding) skipOnboarding();
  if (opts.page) localStorage.setItem("nugget.page", opts.page);
  const r = await render(createElement(App));
  await flush(5);
  return r;
}

// ---------------------------------------------------------------------------------------------
// Mock daemon
// ---------------------------------------------------------------------------------------------

/** Computes a command's answer from its args and how many times it has been called (1-based). */
export type ReplyFn = (args: Record<string, unknown>, n: number) => unknown;
/** What a command answers: a value (deep-copied per call) or a `ReplyFn`. */
export type Reply = unknown;

export interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

/** Errors in the shape the Rust shell rejects with (see ApiError in src/api.ts). */
export const errors = {
  rpc: (message = "Something went wrong", code = -32000) => ({ kind: "rpc", message, code }),
  methodNotFound: () => ({ kind: "rpc", message: "method not found", code: METHOD_NOT_FOUND }),
  notRunning: (message = "Nugget isn't running") => ({ kind: "not_running", message }),
  disconnected: (message = "connection lost") => ({ kind: "disconnected", message }),
};

/** A call held open by `hang()`, settled by the test. Settling flushes React updates. */
export interface Pending {
  /** True once the daemon has received the call. */
  readonly started: boolean;
  resolve(value?: unknown): Promise<void>;
  reject(error?: unknown): Promise<void>;
}

type Step = { kind: "reply"; reply: Reply } | { kind: "fail"; error: unknown } | { kind: "hang"; pending: PendingImpl };

class PendingImpl implements Pending {
  started = false;
  private settle: { resolve: (v: unknown) => void; reject: (e: unknown) => void } | null = null;
  private early: { ok: boolean; value: unknown } | null = null;

  promise(): Promise<unknown> {
    this.started = true;
    return new Promise((resolve, reject) => {
      this.settle = { resolve, reject };
      if (this.early) (this.early.ok ? resolve : reject)(this.early.value);
    });
  }
  async resolve(value?: unknown) {
    await this.finish(true, value);
  }
  async reject(error: unknown = errors.rpc()) {
    await this.finish(false, error);
  }
  private async finish(ok: boolean, value: unknown) {
    await act(async () => {
      if (this.settle) (ok ? this.settle.resolve : this.settle.reject)(value);
      else this.early = { ok, value };
    });
    await flush(3);
  }
}

/**
 * A scriptable daemon behind the mocked Tauri IPC. Every command call is recorded with its
 * arguments. Answers come from (in order): one-shot steps queued with `once`/`failOnce`/`hang`,
 * the persistent reply set with `on`/`fail`, then the shared fixtures. Anything else rejects
 * like an older daemon ("method not found").
 */
export class Daemon {
  readonly calls: Call[] = [];
  private persistent = new Map<string, Step>();
  private queued = new Map<string, Step[]>();
  private counts = new Map<string, number>();

  constructor(responses: Record<string, Reply> = {}) {
    for (const [cmd, r] of Object.entries(responses)) this.on(cmd, r);
    mockIPC((cmd, args) => this.handle(cmd, (args ?? {}) as Record<string, unknown>), { shouldMockEvents: true });
  }

  /** Always answer `cmd` with `reply`. */
  on(cmd: string, reply: ReplyFn): this;
  on(cmd: string, reply: Reply): this;
  on(cmd: string, reply: Reply): this {
    this.persistent.set(cmd, { kind: "reply", reply });
    return this;
  }
  /** Answer only the next call of `cmd` with `reply` (queue several for a sequence). */
  once(cmd: string, reply: ReplyFn): this;
  once(cmd: string, reply: Reply): this;
  once(cmd: string, reply: Reply): this {
    return this.enqueue(cmd, { kind: "reply", reply });
  }
  /** Always reject `cmd`. */
  fail(cmd: string, error: unknown = errors.rpc()): this {
    this.persistent.set(cmd, { kind: "fail", error });
    return this;
  }
  /** Reject only the next call of `cmd`; later calls answer as before. */
  failOnce(cmd: string, error: unknown = errors.rpc()): this {
    return this.enqueue(cmd, { kind: "fail", error });
  }
  /** Hold the next call of `cmd` open until the test settles it. */
  hang(cmd: string): Pending {
    const pending = new PendingImpl();
    this.enqueue(cmd, { kind: "hang", pending });
    return pending;
  }
  /** Forget a persistent reply (fall back to fixtures). */
  reset(cmd: string): this {
    this.persistent.delete(cmd);
    this.queued.delete(cmd);
    return this;
  }
  /** The daemon is not running: `connection_info` reports it disconnected. */
  offline(socket = "/tmp/nuggetd.sock"): this {
    this.on("connection_info", { connected: false, socket, error: "connection refused" } satisfies ConnectionInfo);
    return this;
  }

  /** Arguments of every call to `cmd`, oldest first. */
  callsTo(cmd: string): Record<string, unknown>[] {
    return this.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
  }
  /** Arguments of the most recent call to `cmd`, or undefined. */
  last(cmd: string): Record<string, unknown> | undefined {
    return this.callsTo(cmd).at(-1);
  }
  count(cmd: string): number {
    return this.callsTo(cmd).length;
  }
  /** Command names called, in order (handy for `toContain`). */
  get commands(): string[] {
    return this.calls.map((c) => c.cmd);
  }
  /** Forget recorded calls (e.g. after the initial page load). */
  clearCalls(): void {
    this.calls.length = 0;
  }

  private enqueue(cmd: string, step: Step): this {
    const q = this.queued.get(cmd) ?? [];
    q.push(step);
    this.queued.set(cmd, q);
    return this;
  }

  private handle(cmd: string, args: Record<string, unknown>): unknown {
    this.calls.push({ cmd, args });
    const n = (this.counts.get(cmd) ?? 0) + 1;
    this.counts.set(cmd, n);
    const step = this.queued.get(cmd)?.shift() ?? this.persistent.get(cmd);
    if (step) return run(step, args, n);
    if (cmd in RESPONSES) return structuredClone(RESPONSES[cmd]);
    throw errors.methodNotFound();
  }
}

function run(step: Step, args: Record<string, unknown>, n: number): unknown {
  switch (step.kind) {
    case "fail":
      throw step.error;
    case "hang":
      return step.pending.promise();
    case "reply": {
      const r = step.reply;
      if (typeof r === "function") return (r as ReplyFn)(args, n);
      return r === undefined ? null : structuredClone(r);
    }
  }
}

/** Install a fresh mock daemon (fixtures from src/test/fixtures.ts, plus `responses`). */
export function daemon(responses: Record<string, Reply> = {}): Daemon {
  return new Daemon(responses);
}

/** Emit a Tauri event (as the Rust shell would) and let React settle. */
export async function emitEvent(name: string, payload?: unknown): Promise<void> {
  await act(async () => {
    await emit(name, payload);
  });
  await flush(3);
}

/** Shorthand for the shell's connection event. */
export function emitConnection(info: ConnectionInfo): Promise<void> {
  return emitEvent(EVENT.connection, info);
}

// ---------------------------------------------------------------------------------------------
// Interactions
// ---------------------------------------------------------------------------------------------

function setNativeValue(el: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement, value: string) {
  const proto = Object.getPrototypeOf(el) as object;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
  if (setter) setter.call(el, value);
  else el.value = value;
}

/**
 * Type into an input or textarea one character at a time, like a user (React sees each
 * change). Replaces the current value unless `{ append: true }`.
 */
export async function type(el: Element | null | undefined, text: string, opts: { append?: boolean } = {}): Promise<void> {
  if (!(el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement)) throw new Error("type: not an input or textarea");
  await act(async () => {
    el.focus();
    let value = opts.append ? el.value : "";
    if (!opts.append) {
      setNativeValue(el, "");
      el.dispatchEvent(new Event("input", { bubbles: true }));
    }
    for (const ch of text) {
      value += ch;
      setNativeValue(el, value);
      el.dispatchEvent(new Event("input", { bubbles: true }));
    }
  });
  await flush(2);
}

/** Clear an input or textarea. */
export function clear(el: Element | null | undefined): Promise<void> {
  return type(el, "");
}

/** Choose an option of a `<select>` by value. */
export async function selectOption(el: Element | null | undefined, value: string): Promise<void> {
  if (!(el instanceof HTMLSelectElement)) throw new Error("selectOption: not a select");
  await act(async () => {
    setNativeValue(el, value);
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await flush(2);
}

export interface Mods {
  ctrl?: boolean;
  meta?: boolean;
  shift?: boolean;
  alt?: boolean;
}

/**
 * Press a key on `el` (default: the focused element): keydown then keyup. Like a browser,
 * Enter in a form field submits the form and Escape cancels an open dialog, unless a handler
 * prevented the default.
 */
export async function press(el: Element | null | undefined, key: string, mods: Mods = {}): Promise<void> {
  const target = (el ?? document.activeElement ?? document.body) as HTMLElement;
  const init: KeyboardEventInit = {
    key,
    bubbles: true,
    cancelable: true,
    ctrlKey: !!mods.ctrl,
    metaKey: !!mods.meta,
    shiftKey: !!mods.shift,
    altKey: !!mods.alt,
  };
  await act(async () => {
    const proceed = target.dispatchEvent(new KeyboardEvent("keydown", init));
    if (proceed && key === "Enter" && target instanceof HTMLInputElement && target.form) {
      target.form.requestSubmit();
    }
    if (proceed && key === "Escape") {
      const dialog = target.closest("dialog");
      if (dialog?.open) {
        const cancel = new Event("cancel", { cancelable: true });
        if (dialog.dispatchEvent(cancel)) dialog.close();
      }
    }
    target.dispatchEvent(new KeyboardEvent("keyup", init));
  });
  await flush(2);
}

/** Click an element (focusing it first, as a pointer would). */
export async function click(el: Element | null | undefined): Promise<void> {
  if (!el) throw new Error("click: element not found");
  if ((el as HTMLButtonElement).disabled) throw new Error(`click: element is disabled: ${describe(el)}`);
  await act(async () => {
    (el as HTMLElement).focus?.();
    (el as HTMLElement).click();
  });
  await flush(2);
}

/** Submit a form (as Enter or a submit button would). */
export async function submit(form: Element | null | undefined): Promise<void> {
  if (!(form instanceof HTMLFormElement)) throw new Error("submit: not a form");
  await act(async () => form.requestSubmit());
  await flush(2);
}

// ---------------------------------------------------------------------------------------------
// Accessible queries (a small subset of ARIA, enough for this app)
// ---------------------------------------------------------------------------------------------

const NAME_FROM_CONTENT = new Set([
  "button", "link", "heading", "tab", "option", "cell", "columnheader", "rowheader", "checkbox", "radio",
  "switch", "menuitem", "menuitemradio", "menuitemcheckbox", "treeitem", "tooltip", "row",
]);

/** The element's ARIA role: explicit, else implicit from its tag. */
export function roleOf(el: Element): string | null {
  const explicit = el.getAttribute("role")?.trim().split(/\s+/)[0];
  if (explicit) return explicit;
  const tag = el.tagName.toLowerCase();
  switch (tag) {
    case "button":
      return "button";
    case "a":
      return el.hasAttribute("href") ? "link" : null;
    case "h1": case "h2": case "h3": case "h4": case "h5": case "h6":
      return "heading";
    case "input": {
      const type = (el.getAttribute("type") ?? "text").toLowerCase();
      if (["button", "submit", "reset", "image"].includes(type)) return "button";
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      if (type === "search") return "searchbox";
      if (type === "range") return "slider";
      if (type === "number") return "spinbutton";
      if (type === "hidden" || type === "file" || type === "color" || type === "password") return null;
      return "textbox";
    }
    case "textarea":
      return "textbox";
    case "select":
      return (el as HTMLSelectElement).multiple || (el as HTMLSelectElement).size > 1 ? "listbox" : "combobox";
    case "option":
      return "option";
    case "ul": case "ol":
      return "list";
    case "li":
      return "listitem";
    case "nav":
      return "navigation";
    case "main":
      return "main";
    case "aside":
      return "complementary";
    case "dialog":
      return "dialog";
    case "table":
      return "table";
    case "tr":
      return "row";
    case "td":
      return "cell";
    case "th":
      return el.getAttribute("scope") === "row" ? "rowheader" : "columnheader";
    case "progress":
      return "progressbar";
    case "fieldset":
      return "group";
    case "form":
      return "form";
    case "section":
      return el.hasAttribute("aria-label") || el.hasAttribute("aria-labelledby") ? "region" : null;
    default:
      return null;
  }
}

/** Hidden from assistive tech (aria-hidden, `hidden`, or inside a closed dialog/details). */
export function isHidden(el: Element): boolean {
  for (let e: Element | null = el; e; e = e.parentElement) {
    if (e.getAttribute("aria-hidden") === "true" || e.hasAttribute("hidden")) return true;
    if (e.tagName === "DIALOG" && !(e as HTMLDialogElement).open) return true;
    if (e !== el && e.tagName === "DETAILS" && !(e as HTMLDetailsElement).open && el.closest("summary") === null) return true;
  }
  return false;
}

function norm(s: string): string {
  return s.replace(/\s+/g, " ").trim();
}

function textAlternative(node: Node, root: boolean): string {
  if (node.nodeType === Node.TEXT_NODE) return node.textContent ?? "";
  if (!(node instanceof Element)) return "";
  if (node.getAttribute("aria-hidden") === "true" || node.hasAttribute("hidden")) return "";
  if (!root) {
    const label = node.getAttribute("aria-label");
    if (label) return ` ${label} `;
    if (node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement) return ` ${node.value} `;
    if (node instanceof HTMLSelectElement) return ` ${node.selectedOptions[0]?.textContent ?? ""} `;
  }
  if (node.tagName === "IMG") return node.getAttribute("alt") ?? "";
  const block = /^(DIV|P|LI|H[1-6]|TR|TD|TH|SECTION|UL|OL|BR)$/.test(node.tagName);
  const inner = Array.from(node.childNodes).map((c) => textAlternative(c, false)).join("");
  return block ? ` ${inner} ` : inner;
}

/** A simplified accessible-name computation (aria-labelledby, aria-label, labels, content, title). */
export function accessibleName(el: Element): string {
  const labelledby = el.getAttribute("aria-labelledby");
  if (labelledby) {
    const parts = labelledby.split(/\s+/).map((id) => el.ownerDocument.getElementById(id));
    const text = parts.map((p) => (p ? textAlternative(p, true) : "")).join(" ");
    if (norm(text)) return norm(text);
  }
  const label = el.getAttribute("aria-label");
  if (label && norm(label)) return norm(label);
  if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement) {
    const labels = Array.from(el.labels ?? []);
    if (labels.length) {
      // The control's own value is not part of its label.
      const text = labels.map((l) => Array.from(l.childNodes).filter((c) => c !== el).map((c) => textAlternative(c, false)).join("")).join(" ");
      if (norm(text)) return norm(text);
    }
    const ph = el.getAttribute("placeholder");
    if (ph) return norm(ph);
  }
  const role = roleOf(el);
  if (role && NAME_FROM_CONTENT.has(role)) {
    const text = norm(textAlternative(el, true));
    if (text) return text;
  }
  if (el.tagName === "FIELDSET") {
    const legend = el.querySelector(":scope > legend");
    if (legend) return norm(textAlternative(legend, true));
  }
  return norm(el.getAttribute("title") ?? "");
}

export type TextMatch = string | RegExp;

function matches(actual: string, expected: TextMatch): boolean {
  return typeof expected === "string" ? actual === norm(expected) : expected.test(actual);
}

export interface RoleOptions {
  name?: TextMatch;
  /** Search here instead of the whole document. */
  root?: ParentNode;
  /** Include elements hidden from assistive tech. */
  hidden?: boolean;
  /** Heading level. */
  level?: number;
}

export function queryAllByRole(role: string, opts: RoleOptions = {}): HTMLElement[] {
  const root = opts.root ?? document.body;
  return Array.from(root.querySelectorAll<HTMLElement>("*")).filter((el) => {
    if (roleOf(el) !== role) return false;
    if (!opts.hidden && isHidden(el)) return false;
    if (opts.level != null && Number(el.getAttribute("aria-level") ?? el.tagName.slice(1)) !== opts.level) return false;
    if (opts.name != null && !matches(accessibleName(el), opts.name)) return false;
    return true;
  });
}

function describe(el: Element): string {
  const name = accessibleName(el);
  return `${roleOf(el) ?? el.tagName.toLowerCase()}${name ? ` "${name}"` : ""}`;
}

function roleSummary(role: string, opts: RoleOptions): string {
  const all = queryAllByRole(role, { root: opts.root, hidden: opts.hidden });
  return all.length ? all.map(describe).join(", ") : "(none)";
}

export function queryByRole(role: string, opts: RoleOptions = {}): HTMLElement | null {
  const all = queryAllByRole(role, opts);
  if (all.length > 1) throw new Error(`Found ${all.length} elements with role "${role}"${opts.name ? ` and name ${String(opts.name)}` : ""}`);
  return all[0] ?? null;
}

export function getAllByRole(role: string, opts: RoleOptions = {}): HTMLElement[] {
  const all = queryAllByRole(role, opts);
  if (!all.length) throw new Error(`No element with role "${role}"${opts.name ? ` and name ${String(opts.name)}` : ""}. Present: ${roleSummary(role, opts)}`);
  return all;
}

export function getByRole(role: string, opts: RoleOptions = {}): HTMLElement {
  const all = getAllByRole(role, opts);
  if (all.length > 1) throw new Error(`Found ${all.length} elements with role "${role}"${opts.name ? ` and name ${String(opts.name)}` : ""}: ${all.map(describe).join(", ")}`);
  return all[0]!;
}

export interface TextOptions {
  root?: ParentNode;
  /** Only elements matching this selector. */
  selector?: string;
}

/** Innermost elements whose whole (normalised) text matches. */
export function queryAllByText(text: TextMatch, opts: TextOptions = {}): HTMLElement[] {
  const root = opts.root ?? document.body;
  const candidates = Array.from(root.querySelectorAll<HTMLElement>(opts.selector ?? "*")).filter(
    (el) => el.tagName !== "SCRIPT" && el.tagName !== "STYLE" && matches(norm(el.textContent ?? ""), text),
  );
  return candidates.filter((el) => !candidates.some((other) => other !== el && el.contains(other)));
}

export function queryByText(text: TextMatch, opts: TextOptions = {}): HTMLElement | null {
  return queryAllByText(text, opts)[0] ?? null;
}

export function getByText(text: TextMatch, opts: TextOptions = {}): HTMLElement {
  const el = queryByText(text, opts);
  if (!el) throw new Error(`No element with text ${String(text)}`);
  return el;
}

/** Retry `check` (flushing React and timers in between) until it stops throwing or times out. */
export async function waitFor<T>(check: () => T, { timeout = 1500, interval = 20 } = {}): Promise<T> {
  const until = Date.now() + timeout;
  for (;;) {
    try {
      return check();
    } catch (e) {
      if (Date.now() > until) throw e;
    }
    await act(async () => {
      await new Promise((r) => setTimeout(r, interval));
    });
    await flush(1);
  }
}

export function findByRole(role: string, opts: RoleOptions & { timeout?: number } = {}): Promise<HTMLElement> {
  return waitFor(() => getByRole(role, opts), { timeout: opts.timeout });
}

export function findAllByRole(role: string, opts: RoleOptions & { timeout?: number } = {}): Promise<HTMLElement[]> {
  return waitFor(() => getAllByRole(role, opts), { timeout: opts.timeout });
}

export function findByText(text: TextMatch, opts: TextOptions & { timeout?: number } = {}): Promise<HTMLElement> {
  return waitFor(() => getByText(text, opts), { timeout: opts.timeout });
}

/** All the queries above, scoped to `root`. */
export function within(root: ParentNode) {
  return {
    getByRole: (role: string, o: RoleOptions = {}) => getByRole(role, { ...o, root }),
    getAllByRole: (role: string, o: RoleOptions = {}) => getAllByRole(role, { ...o, root }),
    queryByRole: (role: string, o: RoleOptions = {}) => queryByRole(role, { ...o, root }),
    queryAllByRole: (role: string, o: RoleOptions = {}) => queryAllByRole(role, { ...o, root }),
    findByRole: (role: string, o: RoleOptions & { timeout?: number } = {}) => findByRole(role, { ...o, root }),
    getByText: (text: TextMatch, o: TextOptions = {}) => getByText(text, { ...o, root }),
    queryByText: (text: TextMatch, o: TextOptions = {}) => queryByText(text, { ...o, root }),
    findByText: (text: TextMatch, o: TextOptions & { timeout?: number } = {}) => findByText(text, { ...o, root }),
  };
}

/** The app's main landmark (where pages render). */
export function main(): HTMLElement {
  return getByRole("main");
}
