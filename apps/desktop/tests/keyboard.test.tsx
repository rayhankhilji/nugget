// @vitest-environment jsdom
/**
 * Keyboard focus regressions found by the keyboard-only e2e scenario (e2e/keyboard.mjs): Tab
 * wraps inside dialogs instead of leaving the page, the card action dialog keeps focus as its
 * buttons change, and busy buttons keep focus instead of dropping it to the page body.
 */
import { describe, expect, it } from "vitest";
import type { CardsList, CardView, PolicyState } from "../src/api";
import { Dialog, tabStops } from "../src/components/Dialog";
import { ActionDialog } from "../src/pages/today/ActionDialog";
import { SafetyPage } from "../src/pages/Safety";
import { TodayPage } from "../src/pages/Today";
import { click, daemon, getByRole, press, render, setupHarness, t } from "./harness";

setupHarness();

const active = () => document.activeElement as HTMLElement | null;
const dialog = () => document.querySelector<HTMLDialogElement>("dialog[open]")!;

async function renderDialog() {
  await render(
    <Dialog title="Pick" onClose={() => {}}>
      <button type="button" data-autofocus="">
        First
      </button>
      <fieldset>
        <label>
          <input type="radio" name="size" value="s" /> Small
        </label>
        <label>
          <input type="radio" name="size" value="l" defaultChecked /> Large
        </label>
      </fieldset>
      <button type="button" hidden>
        Hidden
      </button>
      <button type="button" tabIndex={-1}>
        Not a stop
      </button>
      <button type="button">Last</button>
    </Dialog>,
  );
}

describe("Dialog keeps Tab inside", () => {
  it("lists one stop per radio group and skips hidden and untabbable elements", async () => {
    await renderDialog();
    const names = tabStops(dialog()).map((el) => (el instanceof HTMLInputElement ? el.value : (el.getAttribute("aria-label") ?? el.textContent)));
    expect(names).toEqual([t("common.close"), "First", "l", "Last"]);
  });

  it("wraps from the last stop to the first, and Shift+Tab from the first to the last", async () => {
    await renderDialog();
    expect(active()?.textContent).toBe("First");

    const last = getByRole("button", { name: "Last" });
    last.focus();
    await press(last, "Tab");
    expect(active()).toBe(getByRole("button", { name: t("common.close") }));

    await press(active(), "Tab", { shift: true });
    expect(active()).toBe(last);
  });

  it("leaves Tab between inner stops to the browser", async () => {
    await renderDialog();
    const first = getByRole("button", { name: "First" });
    const e = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    first.dispatchEvent(e);
    expect(e.defaultPrevented).toBe(false);
  });

  it("brings focus back in when it is outside the dialog", async () => {
    await renderDialog();
    (document.activeElement as HTMLElement).blur();
    const e = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    dialog().dispatchEvent(e);
    expect(e.defaultPrevented).toBe(true);
    expect(active()).toBe(getByRole("button", { name: t("common.close") }));
  });
});

const card: CardView = {
  id: "c1",
  kind: "unanswered_thread",
  title: "Reply to Ana",
  detail: "Ana asked on Monday.",
  people: [],
  score: 1,
  rank: 1,
  factors: [],
  evidence: [],
  actions: [{ index: 0, kind: "draft_reply", label: "Draft a reply to Ana", risk: "edit", supported: true }],
  state: "seen",
  sensitive: false,
  created_at: "2026-10-28T08:00:00Z",
};
const preview = { card_id: "c1", action: 0, kind: "draft_reply", summary: "Draft a reply", risk: "edit", target: "mail:drafts", diff: [], digest: "d1" };

describe("card action dialog focus", () => {
  it("moves focus to Confirm, then to Done, as Approve and Confirm go away", async () => {
    const d = daemon({ cards_preview: preview });
    d.once("cards_execute", { outcome: "needs_confirmation", detail: "drafts need a yes" });
    d.once("cards_execute", { outcome: "executed", artifact: { kind: "draft", text: "Hi Ana" } });
    await render(<ActionDialog card={card} action={card.actions[0]!} onClose={() => {}} />);

    const approve = getByRole("button", { name: t("today.action.approve") });
    approve.focus();
    await click(approve);
    expect(active()).toBe(getByRole("button", { name: t("today.action.confirm") }));

    await click(active());
    expect(d.last("cards_execute")).toMatchObject({ request: { confirmed: true } });
    expect(active()).toBe(getByRole("button", { name: t("common.done") }));
  });

  it("does not move focus the user put elsewhere in the dialog", async () => {
    const d = daemon();
    const loading = d.hang("cards_preview");
    await render(<ActionDialog card={card} action={card.actions[0]!} onClose={() => {}} />);
    const cancel = getByRole("button", { name: t("common.cancel") });
    cancel.focus();
    await loading.resolve(preview);
    expect(getByRole("button", { name: t("today.action.approve") })).toBeTruthy();
    expect(active()).toBe(cancel);
  });
});

describe("Today returns focus to the inbox after an executed action", () => {
  it("focuses the inbox heading once the dialog closes", async () => {
    const list: CardsList = { cards: [card], last_refresh: null, enabled: true };
    const d = daemon({
      cards_list: list,
      brief_get: { date: "2026-10-28", generated_at: "2026-10-28T07:00:00Z", top: [], meetings: [], overdue: [], renewals: [], open_cards: 1 },
      proactive_metrics: { total: 1, by_state: {}, shown: 0, accepted: 0, dismissed: 0, snoozed: 0, by_kind: {}, dismiss_reasons: {}, model_updates: 0 },
      cards_feedback: card,
      cards_preview: preview,
      cards_execute: { outcome: "executed", artifact: { kind: "draft", text: "Hi Ana" } },
    });
    await render(<TodayPage />);
    await click(getByRole("button", { name: "Draft a reply to Ana" }));
    await click(getByRole("button", { name: t("today.action.approve") }));
    d.on("cards_list", { ...list, cards: [] });
    await click(getByRole("button", { name: t("common.done") }));
    expect(document.querySelector("dialog[open]")).toBeNull();
    expect(active()).toBe(getByRole("heading", { name: new RegExp(`^${t("today.inbox")}`) }));
  });
});

describe("the kill switch keeps focus while it works", () => {
  const policy: PolicyState = {
    config: { version: 1, rules: [], disabled_defaults: [], rate_limits: { send: 25, destroy: 25, spend: 3, security: 3 }, confirm_timeout_secs: 120 },
    defaults: [],
    halted: false,
    log: { entries: 0, head: null, chain_ok: true },
  };

  it("is aria-disabled, not disabled, while stopping, and stays focused as it turns into Resume", async () => {
    const d = daemon({ policy_get: policy, policy_approvals: { pending: [], task: [] } });
    const kill = d.hang("policy_kill");
    await render(<SafetyPage />);
    const stop = getByRole("button", { name: t("safety.kill.stop") }) as HTMLButtonElement;
    await click(stop);
    expect(kill.started).toBe(true);
    expect(stop.disabled).toBe(false);
    expect(stop.getAttribute("aria-disabled")).toBe("true");
    expect(active()).toBe(stop);

    // A second press while busy does nothing.
    await click(stop);
    expect(d.count("policy_kill")).toBe(1);

    await kill.resolve({ halted: true });
    expect(stop.textContent).toBe(t("safety.kill.resume"));
    expect(stop.hasAttribute("aria-disabled")).toBe(false);
    expect(active()).toBe(stop);
  });
});
