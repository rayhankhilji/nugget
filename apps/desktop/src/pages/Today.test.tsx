// @vitest-environment jsdom
import axe from "axe-core";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import type { BriefView, CardsList, CardView, ProactiveLearning } from "../api";
import { TodayPage } from "./Today";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { factorLabel, learnedLine, snoozeUntil, topFactors } from "./today/model";
import { briefNotification, reminderNotification } from "../proactiveNotifications";

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
  it("says there is no feedback yet instead of a usefulness rate before any card is accepted or dismissed", async () => {
    const quiet = { total: 1, by_state: {}, shown: 1, accepted: 0, dismissed: 0, snoozed: 0, precision: 0, by_kind: {}, dismiss_reasons: {}, model_updates: 0 };
    let metrics = quiet;
    setup({ proactive_metrics: () => metrics });
    const { container } = await render(<TodayPage />);
    expect(container.querySelector(".today-metrics")?.textContent).toBe("No feedback yet");
    expect(container.textContent).not.toContain("were useful");

    // Accepting a card gives the rate something to measure.
    metrics = { ...quiet, accepted: 1, precision: 1 };
    await click(byText(container, "button", "Accept"));
    await flush();
    expect(container.querySelector(".today-metrics")?.textContent).toBe("100% of suggestions were useful (1 of 1)");
  });

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

  it("doesn't repeat inbox cards in the brief, even right after one is dismissed", async () => {
    // After the dismiss the inbox is empty; the first brief reload is slower than the list (still has c1).
    let lists = 0;
    let briefs = 0;
    setup({
      cards_list: () => (lists++ === 0 ? list : { ...list, cards: [] }),
      brief_get: () => (briefs++ < 2 ? brief : { ...brief, top: [] }),
    });
    const { container } = await render(<TodayPage />);
    const briefText = () => container.querySelector(".today-brief")?.textContent ?? "";
    // c1 is under "Needs you": the brief's Top leaves it out (c2 is a renewal, still listed).
    expect(briefText()).not.toContain("Ana is waiting");
    expect(briefText()).not.toContain("Top of the list");
    expect(briefText()).toContain("Domain renewal");
    await click(container.querySelector('button[aria-label="Dismiss Reply to Ana about the Q3 plan"]'));
    await click(byText(container, ".today-dismiss button", "Dismiss card"));
    expect(container.querySelector(".today-card")).toBeNull();
    expect(briefText()).not.toContain("Ana is waiting");
  });

  it("shows a short action label but keeps the daemon's full label in the accessible name", async () => {
    setup();
    const { container } = await render(<TodayPage />);
    const draft = byText(container, "button", "Draft reply")!;
    expect(draft.getAttribute("aria-label")).toBe("Draft reply: Draft a reply to Ana");
    expect(draft.getAttribute("title")).toBe("Draft a reply to Ana");
    const archive = byText(container, "button", "Archive thread")!;
    expect(archive.getAttribute("aria-label")).toBe("Archive thread: Archive the thread");
  });

  it("has no accessibility violations", async () => {
    setup();
    await render(<TodayPage />);
    const result = await axe.run(document.body, {
      // Rendered without the app shell's landmarks; the in-app check is in a11y.test.tsx.
      rules: { "color-contrast": { enabled: false }, region: { enabled: false } },
    });
    expect(result.violations.map((v) => v.id)).toEqual([]);
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

  it("dismisses with a reason and free text", async () => {
    const { feedback } = setup();
    const { container } = await render(<TodayPage />);
    await click(container.querySelector('button[aria-label="Dismiss Reply to Ana about the Q3 plan"]'));
    const radios = Array.from(container.querySelectorAll<HTMLInputElement>('.today-dismiss input[type="radio"]'));
    expect(radios.map((r) => r.value)).toEqual(["not_relevant", "already_handled", "wrong_info", "not_important", "other"]);
    await click(radios[4]);
    const input = container.querySelector<HTMLInputElement>('.today-dismiss input[type="text"]')!;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      setter.call(input, "duplicate");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(byText(container, "button", "Dismiss card"));
    expect(feedback.at(-1)).toEqual({ card_id: "c1", feedback: "dismiss", reason: "other", reason_text: "duplicate" });
  });

  it("snoozes until tomorrow morning", async () => {
    const { feedback } = setup();
    const { container } = await render(<TodayPage />);
    await click(container.querySelector('button[aria-label="Snooze Reply to Ana about the Q3 plan"]'));
    await click(byText(container, "button", "Tomorrow morning"));
    const last = feedback.at(-1)!;
    expect(last.feedback).toBe("snooze");
    expect(new Date(last.until as string).getHours()).toBe(8);
  });

  it("shows a disabled state", async () => {
    setup({ cards_list: { cards: [], enabled: false } });
    const { container } = await render(<TodayPage />);
    expect(container.textContent).toContain("Suggestions are turned off");
  });

  it("shows an empty state", async () => {
    setup({ cards_list: { cards: [], enabled: true } });
    const { container } = await render(<TodayPage />);
    expect(container.textContent).toContain("You're all caught up");
  });
});

const learning: ProactiveLearning = {
  entries: [
    { scope: "sender", key: "bo@vendor.io", label: null, accepted: 0, dismissed: 3.2, adjustment: -1.1, suppresses: false, last_at: "2026-10-28T09:00:00Z" },
    { scope: "thread", key: "t-9f2", label: "<i>Invoice</i> follow-up", accepted: 0, dismissed: 1, adjustment: -6, suppresses: true, last_at: "2026-10-27T09:00:00Z" },
    { scope: "kind", key: "renewal", label: null, accepted: 2, dismissed: 0, adjustment: 0.6, suppresses: false, last_at: "2026-10-26T09:00:00Z" },
  ],
};

const learnedCard: CardView = {
  ...card,
  state: "seen",
  learned: [
    { scope: "sender", key: "ana@example.com", direction: "less", accepted: 0, dismissed: 3, reason: null, contribution: -0.9 },
    { scope: "kind", key: "unanswered_thread", direction: "more", accepted: 2, dismissed: 0, reason: null, contribution: 0.4 },
    { scope: "domain", key: "example.com", direction: "less", accepted: 0, dismissed: 1, reason: "not_important", contribution: -0.1 },
  ],
};

function setupLearning(extra: Record<string, unknown | ((a: unknown) => unknown)> = {}) {
  const resets: Record<string, unknown>[] = [];
  const r = setup({
    cards_list: { cards: [learnedCard], enabled: true },
    proactive_learning: learning,
    proactive_reset_learning: (a: unknown) => {
      resets.push((a as Req).request);
      return { cleared: 1 };
    },
    ...extra,
  });
  return { ...r, resets };
}

describe("Today learning", () => {
  it("says in the why how feedback moved a card, at most two effects, with counts", async () => {
    setupLearning();
    const { container } = await render(<TodayPage />);
    const lines = Array.from(container.querySelectorAll(".today-learned li span")).map((e) => e.textContent);
    expect(lines).toEqual([
      "Shown less often: you dismissed 3 similar cards from this sender",
      "Shown more often: you accepted 2 similar “Awaiting your reply” cards",
    ]);
  });

  it("treats a card without `learned` (older daemon) as nothing learned and hides the panel without the method", async () => {
    setup();
    const { container } = await render(<TodayPage />);
    expect(container.querySelector(".today-learned")).toBeNull();
    expect(container.querySelector(".today-learning")).toBeNull();
  });

  it("forgets one effect from the card", async () => {
    const { resets, calls } = setupLearning();
    const { container } = await render(<TodayPage />);
    const before = calls.filter((c) => c === "cards_list").length;
    await click(container.querySelector('.today-learned button[aria-label="Forget what Nugget learned about ana@example.com (Sender)"]'));
    expect(resets).toEqual([{ scope: "sender", key: "ana@example.com" }]);
    expect(calls.filter((c) => c === "cards_list").length).toBeGreaterThan(before);
  });

  it("lists what was learned, as plain text, and forgets one entry", async () => {
    const { resets, calls } = setupLearning();
    const { container } = await render(<TodayPage />);
    const panel = container.querySelector(".today-learning")!;
    expect(panel.querySelector("h2")?.textContent).toBe("What Nugget has learned");
    const toggle = byText(panel, "button", "Show 3 things Nugget learned")!;
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    await click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const entries = Array.from(panel.querySelectorAll(".today-learning-entry"));
    expect(entries.length).toBe(3);
    expect(entries[0]!.textContent).toContain("bo@vendor.io");
    expect(entries[0]!.textContent).toContain("Shown less often");
    expect(entries[0]!.textContent).toContain("0 accepted · 3 dismissed");
    expect(entries[1]!.textContent).toContain("<i>Invoice</i> follow-up");
    expect(entries[1]!.textContent).toContain("Hidden");
    expect(panel.querySelector("i")).toBeNull();
    expect(entries[2]!.textContent).toContain("Renewal");
    expect(entries[2]!.textContent).toContain("Shown more often");

    const learningCalls = calls.filter((c) => c === "proactive_learning").length;
    await click(byText(entries[1]!, "button", "Forget"));
    expect(resets).toEqual([{ scope: "thread", key: "t-9f2" }]);
    expect(calls.filter((c) => c === "proactive_learning").length).toBeGreaterThan(learningCalls);
    expect(panel.querySelector('[role="status"]')?.textContent).toBe("Forgot 1 learned adjustment");
  });

  it("asks before resetting everything", async () => {
    const { resets } = setupLearning();
    const { container } = await render(<TodayPage />);
    await click(byText(container, "button", "Reset all learning"));
    let dialog = document.querySelector("dialog");
    expect(dialog?.textContent).toContain("Reset everything Nugget has learned?");
    expect(resets).toEqual([]);
    // Cancel (focused first) leaves everything as it was.
    expect(document.activeElement?.textContent).toBe("Cancel");
    await click(byText(dialog!, "button", "Cancel"));
    expect(document.querySelector("dialog")).toBeNull();
    expect(resets).toEqual([]);

    await click(byText(container, "button", "Reset all learning"));
    dialog = document.querySelector("dialog");
    await click(byText(dialog!, "button", "Reset all"));
    expect(resets).toEqual([{ scope: "all" }]);
    expect(document.querySelector("dialog")).toBeNull();
  });

  it("shows an error when forgetting fails and keeps the dialog open", async () => {
    setupLearning({
      proactive_reset_learning: () => {
        throw { kind: "rpc", message: "store is locked", code: -32000 };
      },
    });
    const { container } = await render(<TodayPage />);
    await click(byText(container, "button", "Reset all learning"));
    await click(byText(document.querySelector("dialog")!, "button", "Reset all"));
    expect(document.querySelector("dialog")?.textContent).toContain("store is locked");

    await click(byText(document.querySelector("dialog")!, "button", "Cancel"));
    await click(container.querySelector('.today-learned button[aria-label="Forget what Nugget learned about ana@example.com (Sender)"]'));
    expect(container.querySelector('.today-learned [role="alert"]')?.textContent).toContain("store is locked");
  });

  it("has no accessibility violations with learning shown", async () => {
    setupLearning();
    const { container } = await render(<TodayPage />);
    await click(byText(container, "button", "Show 3 things Nugget learned"));
    const result = await axe.run(document.body, {
      rules: { "color-contrast": { enabled: false }, region: { enabled: false } },
    });
    expect(result.violations.map((v) => v.id)).toEqual([]);
  });
});

describe("today helpers", () => {
  it("labels factors", () => {
    expect(factorLabel("importance")).toBe("Important");
    expect(factorLabel("kind:renewal")).toBe("Type: Renewal");
    expect(factorLabel("some_new_thing")).toBe("some new thing");
  });

  it("falls back to magnitude when no factor is positive", () => {
    const f = topFactors([
      { name: "a", value: 1, weight: -1, contribution: -1 },
      { name: "b", value: 1, weight: -2, contribution: -2 },
    ]);
    expect(f.map((x) => x.name)).toEqual(["b", "a"]);
  });

  it("phrases learned effects with plurals and reasons", () => {
    const base = { accepted: 0, dismissed: 1, reason: null, contribution: -1 };
    expect(learnedLine({ ...base, scope: "sender", key: "a@b.c", direction: "less" })).toBe(
      "Shown less often: you dismissed 1 similar card from this sender",
    );
    expect(learnedLine({ ...base, scope: "domain", key: "b.c", direction: "less", dismissed: 4, reason: "not_important" })).toBe(
      "Shown less often: you marked 4 similar cards from this domain as not important",
    );
    expect(learnedLine({ ...base, scope: "thread", key: "t", direction: "hidden", reason: "already_handled" })).toBe(
      "Hidden: you marked this thread as already handled",
    );
    expect(learnedLine({ ...base, scope: "kind", key: "renewal", direction: "more", accepted: 2.4 })).toBe(
      "Shown more often: you accepted 2 similar “Renewal” cards",
    );
    // Decayed to nothing, or a scope this app doesn't know: a generic cause.
    expect(learnedLine({ ...base, scope: "sender", key: "a", direction: "less", dismissed: 0.2 })).toBe(
      "Shown less often: based on your earlier feedback",
    );
    expect(learnedLine({ ...base, scope: "project", key: "x", direction: "less" })).toBe("Shown less often: based on your earlier feedback");
    expect(learnedLine({ ...base, scope: "sender", key: "a", direction: "sideways" })).toBeNull();
  });

  it("computes snooze times", () => {
    const wed = new Date(2026, 9, 28, 10, 0);
    expect(new Date(snoozeUntil("later_today", wed)).getHours()).toBe(13);
    const tomorrow = new Date(snoozeUntil("tomorrow", wed));
    expect([tomorrow.getDate(), tomorrow.getHours()]).toEqual([29, 8]);
    const monday = new Date(snoozeUntil("next_week", wed));
    expect([monday.getDay(), monday.getDate()]).toEqual([1, 2]);
  });

  it("composes notifications", () => {
    expect(briefNotification(brief)).toEqual({ title: "Your morning brief: 2 items", body: "2 things need you · 1 meeting today" });
    expect(reminderNotification({ id: "r", text: "  Call\nBo ", at: "2026-10-28T10:00:00Z" }).body).toBe("Call Bo");
  });
});
