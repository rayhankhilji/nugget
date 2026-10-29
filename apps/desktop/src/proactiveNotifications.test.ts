// @vitest-environment jsdom
import { emit } from "@tauri-apps/api/event";
import { afterEach, describe, expect, it } from "vitest";
import { EVENT, PROACTIVE_NOTIFICATION, type BriefView } from "./api";
import { startProactiveNotifications } from "./proactiveNotifications";
import { cleanup, flush, mockDaemon } from "./test/render";

const brief: BriefView = {
  date: "2026-10-28",
  generated_at: "2026-10-28T07:00:00Z",
  top: [{ card_id: "c1", kind: "unanswered_thread", title: "Reply to Ana", summary: "", due: null, rank: 0.9, factors: [], actions: [] }],
  meetings: [],
  overdue: [],
  renewals: [],
  open_cards: 1,
};

describe("proactive notifications", () => {
  afterEach(cleanup);

  it("ask the shell to open Today when the brief or a reminder notification is clicked", async () => {
    const shown: unknown[] = [];
    mockDaemon({
      notify: (args: unknown) => {
        shown.push(args);
        return null;
      },
    });
    const stop = startProactiveNotifications();
    await flush();
    await emit(EVENT.notification, { method: PROACTIVE_NOTIFICATION.briefReady, params: brief });
    await emit(EVENT.notification, {
      method: PROACTIVE_NOTIFICATION.reminderDue,
      params: { id: "r1", text: "Call Bo", at: "2026-10-28T10:00:00Z" },
    });
    // Other daemon notifications show nothing.
    await emit(EVENT.notification, { method: "cards.changed", params: {} });
    await flush();
    expect(shown).toEqual([
      expect.objectContaining({ title: "Your morning brief: 1 item", openToday: true }),
      expect.objectContaining({ body: "Call Bo", openToday: true }),
    ]);

    stop();
    await emit(EVENT.notification, { method: PROACTIVE_NOTIFICATION.briefReady, params: brief });
    await flush();
    expect(shown).toHaveLength(2);
  });
});
