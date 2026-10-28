// Content script, injected on demand (chrome.scripting) only into tabs whose site the user has
// granted. Talks only to our own background worker.

import { PageAgent } from "./agent";
import { createOverlay } from "./overlay";
import { ProtocolError, type BrowserAction, type SnapshotOptions } from "./protocol";

export type ContentRequest =
  | { nugget: "snapshot"; options?: SnapshotOptions }
  | { nugget: "act"; snapshot_id: string; action: BrowserAction }
  | { nugget: "overlay"; active: boolean; label?: string }
  | { nugget: "halt" }
  | { nugget: "resume" }
  | { nugget: "ping" };

export type ContentResponse = { ok: true; result: unknown } | { ok: false; code: string; message: string };

declare global {
  interface Window {
    __nuggetAgent?: true;
  }
}

if (!window.__nuggetAgent) {
  window.__nuggetAgent = true;
  const agent = new PageAgent(document);
  const overlay = createOverlay(document, () => {
    agent.halt();
    overlay.show("Nugget stopped");
    void chrome.runtime.sendMessage({ nugget: "user_stop", reason: "overlay stop button" });
  });

  const handle = async (req: ContentRequest): Promise<unknown> => {
    switch (req.nugget) {
      case "ping":
        return { ready: true };
      case "snapshot":
        return agent.snapshot(req.options);
      case "act":
        return agent.act(req.snapshot_id, req.action);
      case "overlay":
        if (req.active) overlay.show(req.label);
        else overlay.hide();
        return {};
      case "halt":
        agent.halt();
        overlay.show("Nugget stopped");
        return {};
      case "resume":
        agent.resume();
        overlay.hide();
        return {};
    }
  };

  chrome.runtime.onMessage.addListener((msg: unknown, sender, sendResponse) => {
    // Only our own extension's background worker may drive the page.
    if (sender.id !== chrome.runtime.id || sender.tab !== undefined) return false;
    if (typeof msg !== "object" || msg === null || !("nugget" in msg)) return false;
    handle(msg as ContentRequest).then(
      (result) => sendResponse({ ok: true, result } satisfies ContentResponse),
      (e: unknown) =>
        sendResponse({
          ok: false,
          code: e instanceof ProtocolError ? e.code : "internal",
          message: e instanceof Error ? e.message : String(e),
        } satisfies ContentResponse),
    );
    return true;
  });
}
