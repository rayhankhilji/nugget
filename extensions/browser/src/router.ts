// Routes validated host requests to the right tab. Browser APIs are injected so the routing,
// consent and kill-switch rules are unit-testable without a browser.

import { checkNavigateUrl } from "./actions";
import type { ContentRequest, ContentResponse } from "./content";
import { diffSnapshots } from "./diff";
import { encodeForHost, parseHostRequest, utf8Length } from "./framing";
import {
  type ActResult,
  type ExtensionMessage,
  type HelloResult,
  MAX_FROM_HOST_BYTES,
  type PageSnapshot,
  PROTOCOL_VERSION,
  ProtocolError,
} from "./protocol";

export interface TabInfo {
  id: number;
  /** Undefined when we lack host permission for the tab's site. */
  url?: string;
}

export interface BrowserDeps {
  activeTab(): Promise<TabInfo | undefined>;
  getTab(id: number): Promise<TabInfo | undefined>;
  /** Whether the user granted Nugget access to `origin` (e.g. `https://example.com`). */
  hasPermission(origin: string): Promise<boolean>;
  inject(tabId: number): Promise<void>;
  sendToTab(tabId: number, msg: ContentRequest): Promise<ContentResponse>;
  /** Navigate and resolve once the new document has loaded (or the timeout passes). */
  navigate(tabId: number, url: string): Promise<void>;
  extensionVersion: string;
  browserName: string;
}

export class Router {
  private halted = false;
  private readonly latest = new Map<number, PageSnapshot>();

  constructor(private readonly deps: BrowserDeps) {}

  get isHalted(): boolean {
    return this.halted;
  }

  /** The user pressed Stop (overlay or popup), or the host sent `halt`. */
  async halt(): Promise<void> {
    this.halted = true;
    const tab = await this.deps.activeTab();
    if (tab) await this.deps.sendToTab(tab.id, { nugget: "halt" }).catch(() => undefined);
  }

  /** Only the user can resume, from the popup. */
  async resume(): Promise<void> {
    this.halted = false;
    const tab = await this.deps.activeTab();
    if (tab) await this.deps.sendToTab(tab.id, { nugget: "resume" }).catch(() => undefined);
  }

  /** Handle one raw message from the native host; always produces exactly one reply. */
  async handleRaw(raw: unknown): Promise<string> {
    let id = 0;
    try {
      if (typeof raw === "object" && raw !== null && typeof (raw as { id?: unknown }).id === "number") {
        id = (raw as { id: number }).id;
      }
      if (utf8Length(JSON.stringify(raw)) > MAX_FROM_HOST_BYTES) {
        throw new ProtocolError("too_large", "request exceeds the size cap");
      }
      const result = await this.handle(raw);
      return this.encode({ type: "reply", id, result });
    } catch (e) {
      const err = e instanceof ProtocolError ? e : new ProtocolError("internal", e instanceof Error ? e.message : String(e));
      return this.encode({ type: "reply", id, error: { code: err.code, message: err.message } });
    }
  }

  private encode(msg: ExtensionMessage): string {
    try {
      return encodeForHost(msg);
    } catch (e) {
      const id = msg.type === "reply" ? msg.id : 0;
      const message = e instanceof Error ? e.message : String(e);
      return encodeForHost({ type: "reply", id, error: { code: "too_large", message } });
    }
  }

  private async handle(raw: unknown): Promise<unknown> {
    const req = parseHostRequest(raw);
    switch (req.type) {
      case "hello":
        return {
          protocol: PROTOCOL_VERSION,
          extension_version: this.deps.extensionVersion,
          browser: this.deps.browserName,
        } satisfies HelloResult;
      case "halt":
        await this.halt();
        return {};
      case "snapshot": {
        const tab = await this.grantedTab(req.tab_id);
        const snap = (await this.toTab(tab.id, { nugget: "snapshot", options: req.options })) as PageSnapshot;
        this.latest.set(tab.id, snap);
        return snap;
      }
      case "overlay": {
        const tab = await this.grantedTab(req.tab_id);
        return this.toTab(tab.id, { nugget: "overlay", active: req.active, label: req.label });
      }
      case "act": {
        if (this.halted) throw new ProtocolError("halted", "Nugget was stopped by the user");
        if (req.action.kind === "navigate") return this.navigate(req.tab_id, req.action.url);
        const tab = await this.grantedTab(req.tab_id);
        const result = (await this.toTab(tab.id, { nugget: "act", snapshot_id: req.snapshot_id, action: req.action })) as ActResult;
        this.latest.set(tab.id, result.snapshot);
        return result;
      }
    }
  }

  private async navigate(tabId: number | undefined, rawUrl: string): Promise<ActResult> {
    const tab = await this.tab(tabId);
    const url = checkNavigateUrl(rawUrl, tab.url);
    // Nugget only takes the user to sites they have opted in to.
    if (!(await this.deps.hasPermission(url.origin))) {
      throw new ProtocolError("permission_required", `Nugget has no access to ${url.origin}; grant it from the toolbar button`);
    }
    const before = this.latest.get(tab.id);
    await this.deps.navigate(tab.id, url.href);
    const snapshot = (await this.toTab(tab.id, { nugget: "snapshot" })) as PageSnapshot;
    this.latest.set(tab.id, snapshot);
    const base: PageSnapshot = before ?? { ...snapshot, url: tab.url ?? "", title: "", elements: [], text: "" };
    return { before_snapshot_id: before?.snapshot_id ?? "", snapshot, diff: diffSnapshots(base, snapshot) };
  }

  private async tab(tabId: number | undefined): Promise<TabInfo> {
    const tab = tabId === undefined ? await this.deps.activeTab() : await this.deps.getTab(tabId);
    if (!tab) throw new ProtocolError("no_tab", "no such tab");
    return tab;
  }

  private async grantedTab(tabId: number | undefined): Promise<TabInfo> {
    const tab = await this.tab(tabId);
    let origin: string | undefined;
    try {
      origin = tab.url ? new URL(tab.url).origin : undefined;
    } catch {
      origin = undefined;
    }
    if (!origin || !/^https?:/.test(origin) || !(await this.deps.hasPermission(origin))) {
      throw new ProtocolError("permission_required", "Nugget has no access to this site; grant it from the toolbar button");
    }
    return tab;
  }

  private async toTab(tabId: number, msg: ContentRequest): Promise<unknown> {
    await this.deps.inject(tabId);
    const res = await this.deps.sendToTab(tabId, msg);
    if (!res) throw new ProtocolError("internal", "no response from the page");
    if (!res.ok) throw new ProtocolError(res.code as ProtocolError["code"], res.message);
    return res.result;
  }
}
