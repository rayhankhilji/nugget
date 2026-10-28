// Background service worker (event page on Firefox). Owns the native-messaging port to the
// Nugget host, consent checks and the kill switch. No network access of its own: the only way
// out is the native host.

import type { ContentRequest, ContentResponse } from "./content";
import { HOST_NAME } from "./protocol";
import { type BrowserDeps, Router } from "./router";

declare const NUGGET_E2E: boolean;

const isFirefox = typeof navigator !== "undefined" && navigator.userAgent.includes("Firefox");

const deps: BrowserDeps = {
  async activeTab() {
    const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
    return tab?.id === undefined ? undefined : { id: tab.id, url: tab.url };
  },
  async getTab(id) {
    try {
      const tab = await chrome.tabs.get(id);
      return { id, url: tab.url };
    } catch {
      return undefined;
    }
  },
  hasPermission(origin) {
    return chrome.permissions.contains({ origins: [`${origin}/*`] });
  },
  async inject(tabId) {
    await chrome.scripting.executeScript({ target: { tabId }, files: ["content.js"] });
  },
  sendToTab(tabId, msg: ContentRequest) {
    return chrome.tabs.sendMessage(tabId, msg) as Promise<ContentResponse>;
  },
  navigate(tabId, url) {
    return new Promise<void>((resolve) => {
      const done = () => {
        clearTimeout(timer);
        chrome.tabs.onUpdated.removeListener(listener);
        resolve();
      };
      const listener = (id: number, info: { status?: string }) => {
        if (id === tabId && info.status === "complete") done();
      };
      const timer = setTimeout(done, 15_000);
      chrome.tabs.onUpdated.addListener(listener);
      void chrome.tabs.update(tabId, { url });
    });
  },
  extensionVersion: chrome.runtime.getManifest().version,
  browserName: isFirefox ? "firefox" : "chromium",
};

const router = new Router(deps);
let port: chrome.runtime.Port | undefined;

function connect(): void {
  if (port) return;
  try {
    port = chrome.runtime.connectNative(HOST_NAME);
  } catch {
    port = undefined;
    return;
  }
  port.onMessage.addListener((msg: unknown) => {
    void router.handleRaw(msg).then((reply) => port?.postMessage(JSON.parse(reply)));
  });
  port.onDisconnect.addListener(() => {
    port = undefined;
  });
}

function sendEvent(event: object): void {
  port?.postMessage({ type: "event", ...event });
}

async function stop(reason: string): Promise<void> {
  await router.halt();
  sendEvent({ event: "user_stop", reason });
}

chrome.runtime.onMessage.addListener((msg: unknown, sender, sendResponse) => {
  if (sender.id !== chrome.runtime.id || typeof msg !== "object" || msg === null) return false;
  const m = msg as { nugget?: string; reason?: string };
  switch (m.nugget) {
    case "user_stop":
      void stop(m.reason ?? "user").then(() => sendResponse({ ok: true }));
      return true;
    case "resume":
      // Resuming is a popup-only action: a page cannot un-stop Nugget.
      if (sender.tab) return false;
      void router.resume().then(() => sendResponse({ ok: true }));
      return true;
    case "status":
      if (sender.tab) return false;
      connect();
      sendResponse({ connected: port !== undefined, halted: router.isHalted });
      return false;
    default:
      return false;
  }
});

chrome.permissions.onAdded.addListener((p) => {
  sendEvent({ event: "permission_changed", origins: p.origins ?? [], granted: true });
});
chrome.permissions.onRemoved.addListener((p) => {
  sendEvent({ event: "permission_changed", origins: p.origins ?? [], granted: false });
});

chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);

// End-to-end test hook, compiled in only by `pnpm build:e2e` (never in release builds): lets the
// Playwright test drive the router without a native host.
if (typeof NUGGET_E2E !== "undefined" && NUGGET_E2E) {
  (globalThis as unknown as { nuggetE2E: unknown }).nuggetE2E = {
    handle: (raw: unknown) => router.handleRaw(raw).then((r) => JSON.parse(r)),
  };
} else {
  connect();
}
