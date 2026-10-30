// @vitest-environment jsdom
/**
 * Empty, daemon-unavailable and failing-connector states for Search, Knowledge, Sources, Status,
 * Today and Models: each says what is going on and what to do next, and healthy pages show no
 * error notes or alerts.
 */
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { CardsList, SourceInfo } from "../api";
import { requestSearch } from "../hooks.nav";
import { RESPONSES, sources as fixtureSources } from "../test/fixtures";
import { cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import { KnowledgePage } from "./Knowledge";
import { ModelsPage } from "./Models";
import { SearchPage } from "./Search";
import { SourcesPage } from "./Sources";
import { StatusPage } from "./Status";
import { TodayPage } from "./Today";

const OFFLINE_TEXT = "Nugget isn't reachable right now.";

/** Every listed command fails as if the daemon had stopped. */
function offline(...cmds: string[]): Record<string, () => never> {
  return Object.fromEntries(
    cmds.map((c) => [
      c,
      () => {
        throw { kind: "not_running", message: "connect ECONNREFUSED /tmp/nuggetd.sock" };
      },
    ]),
  );
}

const failing: SourceInfo = {
  id: "imap-1",
  kind: "imap",
  name: "Home mail",
  enabled: true,
  state: "error",
  items: 12,
  chunks: 40,
  last_sync: "2026-10-28T11:00:00Z",
  last_error: "authentication failed",
  settings: { username: "me@example.com" },
};
const withFailing = [...fixtureSources, failing];

const emptyCards: CardsList = { cards: [], last_refresh: null, enabled: true };
const today = {
  cards_list: emptyCards,
  brief_get: { date: "2026-10-29", generated_at: "2026-10-29T07:00:00Z", top: [], meetings: [], overdue: [], renewals: [], open_cards: 0 },
  proactive_metrics: { total: 0, by_state: {}, shown: 0, accepted: 0, dismissed: 0, snoozed: 0, precision: null, by_kind: {}, dismiss_reasons: {}, model_updates: 0 },
};

function text(): string {
  return document.body.textContent ?? "";
}

function alerts(): string[] {
  return Array.from(document.querySelectorAll(".note-error, [role=alert]")).map((e) => e.textContent ?? "");
}

function retry(): HTMLButtonElement | undefined {
  return Array.from(document.querySelectorAll<HTMLButtonElement>("[role=alert] button")).find((b) => b.textContent === "Retry");
}

async function show(ui: ReactNode) {
  return render(<main>{ui}</main>);
}

beforeEach(() => {
  installDomShims();
  if (!Element.prototype.scrollIntoView) Element.prototype.scrollIntoView = () => {};
});
afterEach(cleanup);

describe("search states", () => {
  it("with no sources, points to Sources", async () => {
    mockDaemon({ sources_list: [] });
    await show(<SearchPage />);
    expect(document.querySelector(".pg-empty-title")?.textContent).toBe("Nothing to search yet");
    expect(text()).toContain("Add a source on the Sources page.");
  });

  it("with sources still indexing, says so instead of asking for a source", async () => {
    mockDaemon({ sources_list: [{ ...fixtureSources[0]!, items: 0, chunks: 0, state: "syncing" }] });
    await show(<SearchPage />);
    expect(document.querySelector(".pg-empty-title")?.textContent).toBe("Still indexing");
    expect(text()).toContain("the Sources page shows progress");
    expect(alerts()).toEqual([]);
  });

  it("warns quietly that results may be incomplete when a source fails", async () => {
    mockDaemon({ sources_list: withFailing });
    await show(<SearchPage />);
    expect(text()).toContain("1 source could not sync, so results may be incomplete. See Sources.");
    expect(alerts()).toEqual([]);
  });

  it("with the daemon stopped, says it isn't reachable and offers Retry", async () => {
    mockDaemon(offline("search", "sources_list"));
    requestSearch("budget");
    await show(<SearchPage />);
    expect(text()).toContain(OFFLINE_TEXT);
    expect(text()).not.toContain("ECONNREFUSED");
    expect(retry()).toBeTruthy();
  });

  it("is healthy with data", async () => {
    mockDaemon();
    await show(<SearchPage />);
    expect(alerts()).toEqual([]);
  });
});

describe("knowledge states", () => {
  const noEntities = { knowledge_entities: [], knowledge_status: { ...(RESPONSES.knowledge_status as object), entities: 0, claims: 0, mentions: 0 } };

  it("with no sources, points to Sources and drops the 'choose' prompt", async () => {
    mockDaemon({ ...noEntities, sources_list: [] });
    await show(<KnowledgePage />);
    expect(document.querySelector(".pg-empty-title")?.textContent).toBe("No entities yet");
    expect(text()).toContain("Add a source on the Sources page.");
    expect(text()).not.toContain("Choose an entity");
  });

  it("while extraction is pending, says it is reading", async () => {
    mockDaemon({ ...noEntities, knowledge_status: { ...noEntities.knowledge_status, pending_items: 3 } });
    await show(<KnowledgePage />);
    expect(text()).toContain("Nugget is reading 3 items. Entities appear here as it goes.");
  });

  it("with the daemon stopped, says it isn't reachable and offers Retry", async () => {
    mockDaemon(offline("knowledge_entities", "knowledge_status", "sources_list"));
    await show(<KnowledgePage />);
    expect(text()).toContain(OFFLINE_TEXT);
    expect(text()).not.toContain("No entities yet");
    expect(retry()).toBeTruthy();
  });

  it("is healthy with data", async () => {
    mockDaemon();
    await show(<KnowledgePage />);
    expect(text()).toContain("Ada Lovelace");
    expect(alerts()).toEqual([]);
  });
});

describe("sources states", () => {
  it("announces loading", async () => {
    mockDaemon({ sources_list: () => new Promise(() => {}) });
    await show(<SourcesPage />);
    const status = Array.from(document.querySelectorAll("[role=status]")).map((e) => e.textContent);
    expect(status.some((s) => s?.includes("Loading sources"))).toBe(true);
  });

  it("when empty, offers Add source right there", async () => {
    mockDaemon({ sources_list: [] });
    await show(<SourcesPage />);
    expect(document.querySelector(".pg-empty-title")?.textContent).toBe("No sources yet");
    const add = document.querySelector<HTMLButtonElement>(".pg-empty-action button");
    expect(add?.textContent).toBe("Add source");
    await click(add);
    expect(document.querySelector("dialog[open]")).toBeTruthy();
  });

  it("explains a failing connector and what to do", async () => {
    mockDaemon({ sources_list: withFailing });
    await show(<SourcesPage />);
    expect(text()).toContain("1 source could not sync. See the note on it below.");
    expect(text()).toContain("The last sync failed: authentication failed");
    expect(text()).toContain("Check its sign-in, password or token, then choose Sync now.");
  });

  it("explains an error state without a recorded message", async () => {
    mockDaemon({ sources_list: [{ ...failing, kind: "files", last_error: null, settings: { roots: ["/x"] } }] });
    await show(<SourcesPage />);
    expect(text()).toContain("The last sync failed.");
    expect(text()).toContain("Check that the source is still there, then choose Sync now.");
  });

  it("turns the health code into a plain reason and fix", async () => {
    const health = { code: "offline", reason: "x", fix: "y", item_errors: 0 };
    mockDaemon({ sources_list: [{ ...failing, last_error: "network error: dns", health }] });
    await show(<SourcesPage />);
    expect(text()).toContain("Nugget could not reach the service behind Home mail.");
    expect(text()).toContain("Check your internet connection, VPN or proxy; Nugget retries automatically.");
  });

  it("reports skipped items on a healthy source", async () => {
    const health = { code: "item_errors", reason: "x", fix: "", item_errors: 2, last_item_error: "bad.pdf: corrupt" };
    mockDaemon({ sources_list: [{ ...failing, state: "idle", last_error: null, health }] });
    await show(<SourcesPage />);
    expect(text()).toContain("some items could not be read and were skipped: 2.");
    expect(text()).toContain("Last skipped item: bad.pdf: corrupt");
  });

  it("with the daemon stopped, says it isn't reachable and offers Retry", async () => {
    mockDaemon(offline("sources_list"));
    await show(<SourcesPage />);
    expect(text()).toContain(OFFLINE_TEXT);
    expect(text()).not.toContain("No sources yet");
    expect(retry()).toBeTruthy();
  });

  it("is healthy with data", async () => {
    mockDaemon();
    await show(<SourcesPage />);
    expect(text()).toContain("Documents");
    expect(alerts()).toEqual([]);
  });
});

describe("status states", () => {
  it("with nothing configured, says what to add", async () => {
    mockDaemon({
      status: { ...(RESPONSES.status as object), providers: [], items: 0, chunks: 0 },
      ingest_status: { ...(RESPONSES.ingest_status as object), sources: [], queue_depth: 0 },
      runtime_status: [],
    });
    await show(<StatusPage />);
    expect(text()).toContain("No AI providers yet");
    expect(text()).toContain("Nothing is being indexed");
    expect(text()).toContain("Add a folder or connect mail on the Sources page to start indexing.");
    expect(text()).toContain("Nothing running on this device");
  });

  it("lists a failing connector with where to fix it", async () => {
    mockDaemon({ ingest_status: { ...(RESPONSES.ingest_status as object), sources: withFailing } });
    await show(<StatusPage />);
    expect(text()).toContain("Home mail");
    expect(text()).toContain("authentication failed");
    expect(text()).toContain("This source could not sync. Fix it on the Sources page, then choose Sync now.");
  });

  it("with the daemon stopped, says it isn't reachable and offers Retry", async () => {
    mockDaemon(offline("status", "ingest_status", "runtime_status", "knowledge_status"));
    await show(<StatusPage />);
    expect(text()).toContain(OFFLINE_TEXT);
    expect(text()).not.toContain("ECONNREFUSED");
    expect(retry()).toBeTruthy();
  });

  it("is healthy with data", async () => {
    // The updater lives in the app shell, not the daemon; answer it so it isn't an error here.
    mockDaemon({ updates_status: null });
    await show(<StatusPage />);
    expect(text()).toContain("Uptime");
    expect(alerts()).toEqual([]);
  });
});

describe("today states", () => {
  it("with no sources, says where cards come from and points to Sources", async () => {
    mockDaemon({ ...today, sources_list: [] });
    await show(<TodayPage />);
    expect(text()).toContain("Nothing to look at yet");
    expect(text()).toContain("Add a source on the Sources page to get started.");
    expect(text()).not.toContain("You're all caught up");
  });

  it("with sources and no cards, is all caught up", async () => {
    mockDaemon(today);
    await show(<TodayPage />);
    expect(text()).toContain("You're all caught up");
    expect(alerts()).toEqual([]);
  });

  it("with the daemon stopped, says it isn't reachable and offers Retry", async () => {
    mockDaemon(offline("cards_list", "brief_get", "proactive_metrics", "sources_list"));
    await show(<TodayPage />);
    expect(text()).toContain(OFFLINE_TEXT);
    expect(text()).not.toContain("You're all caught up");
    expect(text()).not.toContain("Nothing to look at yet");
    expect(retry()).toBeTruthy();
  });
});

describe("models states", () => {
  it("with an empty list, says what to do", async () => {
    mockDaemon({ models_list: [] });
    await show(<ModelsPage />);
    expect(document.querySelector(".pg-empty-title")?.textContent).toBe("No models available");
    expect(text()).toContain("Refresh, or update Nugget to get the latest list.");
  });

  it("with the daemon stopped, says it isn't reachable and offers Retry", async () => {
    mockDaemon(offline("models_list"));
    await show(<ModelsPage />);
    expect(text()).toContain(OFFLINE_TEXT);
    expect(text()).not.toContain("No models available");
    expect(retry()).toBeTruthy();
  });

  it("is healthy with data", async () => {
    mockDaemon();
    await show(<ModelsPage />);
    expect(text()).toContain("qwen3-4b-q4");
    expect(alerts()).toEqual([]);
  });
});
