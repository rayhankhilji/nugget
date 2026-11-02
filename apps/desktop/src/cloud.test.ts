// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { CloudError, cloudApi, type CloudAction, type CloudAppInfo } from "./cloud";
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
