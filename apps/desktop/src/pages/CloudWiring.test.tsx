// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { BriefView, CardView } from "../api";
import type { CloudAction, CloudBrief, CloudSearchResponse, CloudSession } from "../cloud";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { HomePage, NeedsPage } from "./Today";
import { CloudSearch } from "./CloudSearch";
import { AccountPage } from "./Account";
import { webUrl } from "../components/CitedProse";

beforeEach(installDomShims);
afterEach(cleanup);

const signedIn: CloudSession = { configured: true, signed_in: true, email: "me@example.com" };

const card: CardView = {
  id: "cc1",
  kind: "unanswered_thread",
  title: "Reply to Margaret about Moorland",
  detail: "",
  summary: "Margaret asked about Thursday",
  people: [],
  due: null,
  score: 1,
  rank: 1,
  factors: [],
  evidence: [],
  actions: [{ index: 0, kind: "draft_reply", label: "Draft a reply", risk: "send", supported: true }],
  state: "seen",
  sensitive: false,
  created_at: "2026-11-02T07:00:00Z",
};

const briefView: BriefView = { date: "2026-11-02", generated_at: "2026-11-02T07:00:00Z", top: [], meetings: [], overdue: [], renewals: [], open_cards: 1 };
const cloudBrief: CloudBrief = {
  day: "2026-11-02",
  timezone: "Europe/London",
  prose: "Margaret is waiting on a time for the Moorland sync [1].",
  citations: [{ n: 1, item_id: "i1", title: "Moorland sync", source: "gmail", url: "https://mail.google.com/m/1", occurred_at: null, snippet: "Thursday?" }],
  brief: briefView,
  created_at: "2026-11-02T07:00:00Z",
} as CloudBrief;

const action: CloudAction = {
  id: "a1",
  kind: "reply_mail",
  toolkit: "gmail",
  status: "pending",
  risk: "send",
  decision: "confirm",
  reason: null,
  summary: "Reply to Margaret",
  preview: { fields: [{ label: "To", value: "margaret@example.com" }], diff: [{ field: "Body", before: null, after: "Thursday works." }] },
  digest: "sha-abc",
  idempotency_key: "k1",
  created_at: "2026-11-02T08:00:00Z",
  expires_at: "2026-11-03T08:00:00Z",
  decided_at: null,
  executed_at: null,
  result: null,
  error: null,
};

describe("signed in to Heed Cloud", () => {
  it("Home reads cards and the written brief from the cloud, with citation chips", async () => {
    const calls = mockDaemon({
      cloud_session: signedIn,
      cloud_cards: { cards: [card], last_refresh: null },
      cloud_brief: cloudBrief,
      sources_list: [],
    });
    const { container } = await render(<HomePage />);
    await flush(5);
    expect(calls).toContain("cloud_cards");
    expect(calls).toContain("cloud_brief");
    expect(container.textContent).toContain("Margaret is waiting on a time for the Moorland sync");
    expect(container.querySelector("button.cite")?.getAttribute("aria-label")).toBe("Source 1");
    expect(container.querySelector(".citations")?.textContent).toContain("Moorland sync");
  });

  it("Needs you proposes an action from a card, and nothing runs until Approve with the shown digest", async () => {
    const accepted: unknown[] = [];
    const approved: unknown[] = [];
    let pending: CloudAction[] = [];
    mockDaemon({
      cloud_session: signedIn,
      cloud_cards: { cards: [card], last_refresh: null },
      cloud_brief: null,
      cloud_apps: [],
      sources_list: [],
      proactive_metrics: { total: 0, by_state: {}, shown: 0, accepted: 0, dismissed: 0, snoozed: 0, precision: null, by_kind: {}, dismiss_reasons: {}, model_updates: 0 },
      cloud_cards_seen: null,
      cloud_card_action: (a: unknown) => {
        accepted.push(a);
        pending = [action];
        return { card: null, action };
      },
      cloud_actions_list: () => pending,
      cloud_action_approve: (a: unknown) => {
        approved.push(a);
        pending = [];
        return { ...action, status: "succeeded" };
      },
    });
    const { container } = await render(<NeedsPage />);
    await flush(5);
    await click(Array.from(container.querySelectorAll("button")).find((b) => b.textContent?.startsWith("Draft reply")));
    await flush(5);
    expect(accepted).toEqual([{ id: "cc1", action: "accept", index: 0, body: null }]);
    const queue = container.querySelector(".action-queue")!;
    expect(queue.textContent).toContain("Reply to Margaret");
    expect(queue.textContent).toContain("margaret@example.com");
    expect(queue.textContent).toContain("Thursday works.");
    expect(approved).toEqual([]);
    await click(byText(queue as HTMLElement, "button", "Approve"));
    await flush(5);
    expect(approved).toEqual([{ id: "a1", digest: "sha-abc" }]);
    expect(container.querySelector(".action-queue")).toBeNull();
  });

  it("Search runs against the cloud and only offers web links", async () => {
    const res: CloudSearchResponse = {
      mode: "hybrid",
      query: "moorland",
      dates: { from: null, to: null, phrase: null },
      results: [
        { item_id: "r1", source: "gmail", kind: "mail", title: "Moorland sync", author: "Margaret", url: "https://mail.google.com/m/1", occurred_at: null, snippet: "Thursday?", score: 1 },
        { item_id: "r2", source: "notion", kind: "page", title: "Notes", author: null, url: "javascript:alert(1)", occurred_at: null, snippet: "x", score: 0.5 },
      ],
      total: 2,
      facets: { sources: [{ value: "gmail", count: 1 }], people: [] },
      timeline: { bucket: "day", points: [] },
    } as CloudSearchResponse;
    const requests: unknown[] = [];
    mockDaemon({ cloud_search: (a: unknown) => (requests.push(a), res) });
    const { container } = await render(<CloudSearch />);
    const box = container.querySelector<HTMLInputElement>('input[type="search"]')!;
    const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    setValue.call(box, "moorland");
    box.dispatchEvent(new Event("input", { bubbles: true }));
    await click(byText(container, "button", "Search"));
    await flush(3);
    expect(requests).toHaveLength(1);
    const results = container.querySelectorAll(".result");
    expect(results).toHaveLength(2);
    expect(results[0]!.querySelector("button")?.textContent).toBe("Open");
    expect(results[1]!.querySelector("button")).toBeNull();
    expect(webUrl("file:///etc/passwd")).toBeNull();
    expect(webUrl("https://example.com/a")).toBe("https://example.com/a");
  });

  it("Account deletes the account only after confirming", async () => {
    let deleted = 0;
    mockDaemon({
      cloud_session: signedIn,
      cloud_me: { user_id: "u1", email: "me@example.com", plan: null, embeddings: false, subscription: null, entitlements: null, usage: { month_cost_micros: 0, smart_asks_used: 0 } },
      cloud_billing_plans: { plans: [] },
      cloud_delete_account: () => (deleted++, null),
    });
    const { container } = await render(<AccountPage />);
    await flush(5);
    await click(byText(container, "button", "Delete account…"));
    expect(deleted).toBe(0);
    const dialog = document.querySelector("dialog[open]")!;
    expect(dialog.textContent).toContain("This can't be undone");
    await click(byText(dialog as HTMLElement, "button", "Delete account"));
    await flush(3);
    expect(deleted).toBe(1);
  });
});
