// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { BriefView, CardsList, CardView } from "../api";
import { TodayPage } from "./Today";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";

const card: CardView = {
  id: "c1",
  kind: "unanswered_thread",
  title: "Reply to Ana about the Q3 plan",
  detail: "Ana asked for your numbers on Monday.",
  summary: "<b>Ana</b> is waiting on your Q3 numbers",
  people: ["Ana Ruiz"],
  due: "2026-10-29T17:00:00Z",
  score: 2.1,
  rank: 0.9,
  factors: [
    { name: "bias", value: 1, weight: 3, contribution: 3 },
    { name: "urgency", value: 0.9, weight: 1.5, contribution: 1.35 },
    { name: "kind_unanswered_thread", value: 1, weight: 0.8, contribution: 0.8 },
    { name: "freshness", value: 0.2, weight: -1, contribution: -0.2 },
  ],
  evidence: [
    { item: { source: "gmail-1", key: "msg-1" }, quote: "Could you send <script>x</script> the numbers?", url: "https://mail.example.com/m/1" },
    { item: { source: "files-1", key: "a.md" }, quote: null, url: "javascript:alert(1)" },
  ],
  actions: [
    { index: 0, kind: "draft_reply", label: "Draft a reply to Ana", risk: "observe", supported: true },
    { index: 1, kind: "archive_thread", label: "Archive the thread", risk: "edit", supported: false },
  ],
  state: "new",
  sensitive: false,
  created_at: "2026-10-28T08:00:00Z",
};

const list: CardsList = { cards: [card], last_refresh: "2026-10-28T08:00:00Z", enabled: true };

const brief: BriefView = {
  date: "2026-10-28",
  generated_at: "2026-10-28T07:00:00Z",
  top: [{ card_id: "c1", kind: "unanswered_thread", title: card.title, summary: "Ana is waiting", due: null, rank: 0.9, factors: [], actions: ["draft_reply"] }],
  meetings: [
    {
      title: "Vendor sync",
      start: "2026-10-28T15:00:00Z",
      end: "2026-10-28T15:30:00Z",
      all_day: false,
      location: null,
      attendees: ["me@corp.com", "bo@vendor.io"],
      external: ["bo@vendor.io"],
      prep_card: null,
      prep: [],
    },
  ],
  overdue: [],
  renewals: [{ card_id: "c2", kind: "renewal", title: "Domain renewal", summary: "Renews Friday", due: null, rank: 0.5, factors: [], actions: [] }],
  open_cards: 2,
};

type Req = { request: Record<string, unknown> };

function setup(extra: Record<string, unknown | ((a: unknown) => unknown)> = {}) {
  const feedback: Record<string, unknown>[] = [];
  const executes: Record<string, unknown>[] = [];
  const calls = mockDaemon({
    cards_list: list,
    brief_get: brief,
    proactive_metrics: { total: 4, by_state: {}, shown: 4, accepted: 3, dismissed: 1, snoozed: 0, precision: 0.75, by_kind: {}, dismiss_reasons: {}, model_updates: 0 },
    cards_feedback: (a: unknown) => {
      feedback.push((a as Req).request);
      return card;
    },
    cards_preview: {
      card_id: "c1",
      action: 0,
      kind: "draft_reply",
      summary: "Reply to Ana",
      risk: "send",
      target: "mail:drafts",
      diff: [
        { op: "context", text: "To: ana@example.com" },
        { op: "add", text: "Hi Ana, here are the numbers." },
        { op: "remove", text: "old line" },
      ],
      digest: "d1",
    },
    cards_execute: (a: unknown) => {
      const r = (a as Req).request;
      executes.push(r);
      return r.confirmed
        ? { outcome: "executed", detail: "draft-1", artifact: { kind: "draft", text: "Hi Ana, <i>numbers</i>", url: "mailto:ana@example.com?body=Hi" } }
        : { outcome: "needs_confirmation", detail: "this sends on your behalf" };
    },
    ...extra,
  });
  return { calls, feedback, executes };
}

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("Today page", () => {
  it("renders the brief and cards with why and evidence as plain text", async () => {
    const { feedback } = setup();
    const { container } = await render(<TodayPage />);
    const text = container.textContent ?? "";
    expect(text).toContain("Morning brief");
    expect(text).toContain("Vendor sync");
    expect(text).toContain("bo@vendor.io");
    expect(text).toContain("Domain renewal");
    expect(text).toContain("75% of suggestions were useful (3 of 4)");
    // Untrusted markup is shown as text.
    expect(text).toContain("<b>Ana</b> is waiting on your Q3 numbers");
    expect(container.querySelector("b")).toBeNull();
    // Why: positive factors, the intercept hidden, kinds named.
    const why = container.querySelector(".today-why")?.textContent ?? "";
    expect(why).toContain("Due soon");
    expect(why).toContain("Type: Awaiting your reply");
    expect(why).not.toMatch(/bias/i);
    expect(why).not.toContain("Recent");
    // Evidence: quote as text; link only for https.
    await click(byText(container, "button", "Show 2 sources"));
    expect(container.querySelector("script")).toBeNull();
    expect(container.textContent).toContain("Could you send <script>x</script> the numbers?");
    expect(container.querySelectorAll(".evidence button").length).toBe(1);
    // Unsupported action is not actionable and explains why.
    const archive = byText(container, "button", "Archive thread");
    expect(archive?.getAttribute("aria-disabled")).toBe("true");
    expect(archive?.getAttribute("title")).toContain("Not available");
    // New card marked seen exactly once.
    expect(feedback).toEqual([{ card_id: "c1", feedback: "seen" }]);
  });

  it("writes ISO dates inside card and brief text for the reader's locale (LL2, GG7-11)", async () => {
    const dated = { ...card, title: "Domain renews 2026-11-03", summary: "Due 2026-11-03: pay the invoice", detail: "Since 2026-10-01." };
    setup({ cards_list: { ...list, cards: [dated] } });
    const { container } = await render(<TodayPage />);
    await flush();
    const text = container.querySelector(".today-card")!.textContent!;
    expect(text).not.toContain("2026-11-03");
    expect(text).toContain("Nov 3, 2026");
    expect(container.querySelector(".today-card-title")!.textContent).toBe("Domain renews Nov 3, 2026");
    expect(byText(container, "button", "Accept")?.getAttribute("aria-label") ?? "").not.toContain("2026-11-03");
  });

  it("previews, approves, confirms and shows the draft", async () => {
    const { executes, calls } = setup({ open_mailto: null });
    const { container } = await render(<TodayPage />);
    await click(byText(container, "button", "Draft reply"));
    await flush();
    const dialog = document.querySelector("dialog")!;
    expect(dialog.textContent).toContain("This is a preview");
    expect(dialog.querySelector(".today-diff-add")?.textContent).toContain("Hi Ana, here are the numbers.");
    expect(dialog.querySelector(".today-diff-remove")?.textContent).toContain("old line");
    expect(dialog.textContent).toContain("Risk: Sends");
    expect(dialog.textContent).toContain("Goes to: Mail drafts");
    await click(byText(dialog, "button", "Approve"));
    expect(executes[0]).toEqual({ card_id: "c1", action: 0, digest: "d1", confirmed: false });
    expect(dialog.textContent).toContain("Please confirm: this sends on your behalf");
    await click(byText(dialog, "button", "Confirm"));
    expect(executes[1]).toEqual({ card_id: "c1", action: 0, digest: "d1", confirmed: true });
    expect(dialog.querySelector(".today-draft")?.textContent).toBe("Hi Ana, <i>numbers</i>");
    expect(dialog.querySelector("i")).toBeNull();
    // Once the action ran, the dialog no longer says nothing has changed.
    expect(dialog.textContent).not.toContain("This is a preview");
    await click(byText(dialog, "button", "Open in mail app"));
    expect(calls).toContain("open_mailto");
  });

  it("shows the reason when the policy denies", async () => {
    setup({ cards_execute: { outcome: "denied", detail: "sending is blocked" } });
    const { container } = await render(<TodayPage />);
    await click(byText(container, "button", "Draft reply"));
    await flush();
    await click(byText(document.querySelector("dialog")!, "button", "Approve"));
    expect(document.querySelector("dialog")?.textContent).toContain("Not allowed: sending is blocked");
  });
});



