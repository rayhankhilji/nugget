// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import {
  CloudError,
  cloudApi,
  deviceTimeZone,
  type CloudAction,
  type CloudAppInfo,
  type CloudBillingPreview,
  type CloudBrief,
  type CloudSearchResponse,
} from "./cloud";
import { cleanup, mockDaemon } from "./test/render";

afterEach(cleanup);

const action: CloudAction = {
  id: "a1",
  kind: "send_mail",
  toolkit: "gmail",
  status: "pending",
  risk: "send",
  decision: "confirm",
  reason: null,
  summary: "Email ana@example.com: Lunch",
  preview: { fields: [{ label: "To", value: "ana@example.com" }], diff: [] },
  digest: "ab".repeat(32),
  idempotency_key: "k-1",
  created_at: "2026-11-02T10:00:00Z",
  expires_at: "2026-11-02T10:15:00Z",
  decided_at: null,
  executed_at: null,
  result: null,
  error: null,
  snoozed_until: null,
  revision: 1,
  editable: ["to", "cc", "bcc", "subject", "body"],
};

const gmail: CloudAppInfo = {
  slug: "gmail",
  name: "Gmail",
  logo: null,
  category: "mail",
  syncs: true,
  actions: ["send_mail", "reply_mail"],
  sync_mode: "poll",
  connectable: true,
  connected: true,
};

/** Records each command with its args and answers with `reply`. */
function record(reply: Record<string, unknown>) {
  const calls: [string, unknown][] = [];
  mockDaemon(
    Object.fromEntries(
      Object.entries(reply).map(([cmd, r]) => [
        cmd,
        (args: unknown) => {
          calls.push([cmd, args]);
          return r;
        },
      ]),
    ),
  );
  return calls;
}

describe("cloudApi actions", () => {
  it("invokes the shell commands with camelCase args", async () => {
    const calls = record({
      cloud_apps: [gmail],
      cloud_actions_list: [action],
      cloud_action_propose: action,
      cloud_action_preview: action,
      cloud_action_status: { ...action, status: "succeeded" },
      cloud_action_approve: { ...action, status: "executing" },
      cloud_action_cancel: { ...action, status: "cancelled" },
    });
    expect(await cloudApi.apps()).toEqual([gmail]);
    expect(await cloudApi.actions()).toEqual([action]);
    await cloudApi.actions("pending");
    const input = { to: ["ana@example.com"], subject: "Lunch", body: "Noon?" };
    expect(await cloudApi.proposeAction({ kind: "send_mail", toolkit: "gmail", input, idempotencyKey: "k-1" })).toEqual(action);
    await cloudApi.actionPreview("a1");
    expect((await cloudApi.actionStatus("a1")).status).toBe("succeeded");
    expect((await cloudApi.approveAction("a1", action.digest)).status).toBe("executing");
    expect((await cloudApi.cancelAction("a1")).status).toBe("cancelled");

    expect(calls).toEqual([
      ["cloud_apps", {}],
      ["cloud_actions_list", {}],
      ["cloud_actions_list", { status: "pending" }],
      ["cloud_action_propose", { kind: "send_mail", toolkit: "gmail", input, idempotencyKey: "k-1" }],
      ["cloud_action_preview", { id: "a1" }],
      ["cloud_action_status", { id: "a1" }],
      ["cloud_action_approve", { id: "a1", digest: action.digest }],
      ["cloud_action_cancel", { id: "a1" }],
    ]);
  });

  it("edits, snoozes and lists snoozed actions", async () => {
    const edited = { ...action, digest: "cd".repeat(32), revision: 2 };
    const snoozed = { ...action, snoozed_until: "2026-11-02T10:10:00Z" };
    const calls = record({
      cloud_actions_list: [snoozed],
      cloud_action_update: edited,
      cloud_action_snooze: snoozed,
    });
    const e = await cloudApi.updateAction("a1", { subject: "Lunch?" });
    expect([e.revision, e.digest]).toEqual([2, "cd".repeat(32)]);
    expect((await cloudApi.snoozeAction("a1", "2026-11-02T10:10:00Z")).snoozed_until).toBe("2026-11-02T10:10:00Z");
    await cloudApi.snoozeAction("a1", null);
    expect(await cloudApi.actions("pending", { includeSnoozed: true })).toEqual([snoozed]);
    await cloudApi.actions(undefined, { includeSnoozed: true });
    await cloudApi.actions("pending", { includeSnoozed: false });

    expect(calls).toEqual([
      ["cloud_action_update", { id: "a1", input: { subject: "Lunch?" } }],
      ["cloud_action_snooze", { id: "a1", until: "2026-11-02T10:10:00Z" }],
      ["cloud_action_snooze", { id: "a1", until: null }],
      ["cloud_actions_list", { status: "pending", includeSnoozed: true }],
      ["cloud_actions_list", { includeSnoozed: true }],
      ["cloud_actions_list", { status: "pending" }],
    ]);
  });

  it("surfaces the cloud's error code", async () => {
    mockDaemon({
      cloud_action_approve: () => {
        throw { kind: "rpc", message: "The action changed.", code: null, cloud: "digest_mismatch" };
      },
    });
    const e = await cloudApi.approveAction("a1", action.digest).catch((x: unknown) => x);
    expect(e).toBeInstanceOf(CloudError);
    expect((e as CloudError).cloud).toBe("digest_mismatch");
    expect((e as CloudError).message).toBe("The action changed.");
  });
});

const searchResponse: CloudSearchResponse = {
  mode: "hybrid",
  query: "contract",
  dates: { from: null, to: null, phrase: null },
  results: [
    { item_id: "i1", source: "gmail", kind: "email", title: "Signed contract", author: "Dana", url: null, occurred_at: null, snippet: "…", score: 0.9 },
  ],
  total: 1,
  facets: { sources: [{ value: "gmail", count: 1 }], people: [] },
  timeline: { bucket: "month", points: [{ start: "2026-10-01T00:00:00Z", count: 1 }] },
};

describe("cloud search, cards and brief wrappers", () => {
  it("send the device zone and the typed arguments to the shell", async () => {
    const seen: Record<string, unknown> = {};
    const brief: CloudBrief = {
      day: "2026-11-02",
      timezone: "Europe/London",
      prose: "Dana waits [1].",
      citations: [],
      brief: { date: "2026-11-02", generated_at: "2026-11-02T07:00:00Z", top: [], meetings: [], overdue: [], renewals: [], open_cards: 0 },
      created_at: "2026-11-02T07:00:01Z",
    };
    mockDaemon({
      cloud_search: (a: unknown) => ((seen.search = a), searchResponse),
      cloud_cards: (a: unknown) => ((seen.cards = a), { cards: [], last_refresh: null }),
      cloud_card_action: (a: unknown) => ((seen.action = a), null),
      cloud_brief: (a: unknown) => ((seen.brief = a), brief),
    });
    const tz = deviceTimeZone();
    expect((await cloudApi.search({ q: "contract", sources: ["gmail"] })).results[0]?.title).toBe("Signed contract");
    expect(seen.search).toEqual({ request: { q: "contract", sources: ["gmail"], tz } });
    expect((await cloudApi.cards()).cards).toEqual([]);
    expect(seen.cards).toEqual({ tz });
    await cloudApi.dismissCard("abc", "already_handled");
    expect(seen.action).toEqual({ id: "abc", action: "dismiss", reason: "already_handled", note: null });
    await cloudApi.acceptCard("abc", { index: 0 });
    expect(seen.action).toEqual({ id: "abc", action: "accept", index: 0, body: null });
    await cloudApi.snoozeCard("abc", "2026-11-04T09:00:00Z");
    expect(seen.action).toEqual({ id: "abc", action: "snooze", until: "2026-11-04T09:00:00Z" });
    expect((await cloudApi.brief())?.prose).toContain("[1]");
    expect(seen.brief).toEqual({ tz });
  });
});

describe("cloud billing preview", () => {
  it("sends the plan and interval and returns the proration", async () => {
    const preview: CloudBillingPreview = {
      plan: "heed_max",
      interval: "year",
      amount_due: 95100,
      currency: "gbp",
      proration_date: "2026-11-02T10:00:00Z",
    };
    const calls = record({ cloud_billing_preview: preview });
    expect(await cloudApi.billingPreview("heed_max", "year")).toEqual(preview);
    expect(calls).toEqual([["cloud_billing_preview", { plan: "heed_max", interval: "year" }]]);
  });

  it("surfaces no_subscription", async () => {
    mockDaemon({
      cloud_billing_preview: () => {
        throw { kind: "rpc", message: "Subscribe first.", code: null, cloud: "no_subscription" };
      },
    });
    const e = await cloudApi.billingPreview("heed", "month").catch((x: unknown) => x);
    expect((e as CloudError).cloud).toBe("no_subscription");
  });
});
