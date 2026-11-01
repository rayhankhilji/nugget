// @vitest-environment jsdom
/**
 * LL2: the Status page's KK1 states (a warming, a failed and a retried local model; a full or low
 * disk) for assistive technology: no axe violations, Retry named after its model, keyboard focus
 * kept when Retry goes away, state changes announced, and the runtime card never blank or raw
 * while it loads or fails. The e2e daemon runs without a local model, so these states are only
 * reachable here.
 */
import axe from "axe-core";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { RuntimeInfo } from "../api";
import { status as baseStatus } from "../test/fixtures";
import { cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { StatusPage } from "./Status";

beforeEach(installDomShims);
afterEach(cleanup);

const base: RuntimeInfo = { model_id: "local-chat-model", role: "chat", state: "ready", provider: "local-chat" };
const failed: RuntimeInfo = {
  ...base,
  state: "failed",
  error: "The on-device chat model keeps stopping, so Nugget stopped restarting it. Open Status to retry or to see its log.",
  gave_up: true,
  detail: "error: cannot allocate memory",
};

async function violations(): Promise<string[]> {
  // Rendered without the app shell's landmarks; the in-app check is in a11y.test.tsx.
  const r = await axe.run(document.body, { rules: { "color-contrast": { enabled: false }, region: { enabled: false } } });
  return r.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
}

const card = () => document.querySelector('[aria-labelledby="runtime-title"]')!;

describe("Status accessibility (LL2)", () => {
  it.each([
    ["a failed model with Retry and Details", [failed]],
    ["a warming model", [{ ...base, state: "starting", state_ms: 12_000, expected_ms: 40_000 }]],
    ["a model waiting to restart", [{ ...failed, gave_up: false, retry_in_ms: 2_500 }]],
  ] as const)("has no violations with %s", async (_name, runtimes) => {
    mockDaemon({ runtime_status: runtimes });
    await render(<StatusPage />);
    await flush();
    expect(card().textContent).not.toMatch(/undefined|\{"|null/);
    expect(await violations()).toEqual([]);
  });

  it.each([
    ["full", { free_bytes: 120_000_000, level: "full", recent_full_error: true }],
    ["low", { free_bytes: 800_000_000, level: "low" }],
  ] as const)("has no violations with a %s disk", async (_name, disk) => {
    mockDaemon({ status: { ...baseStatus, disk } });
    await render(<StatusPage />);
    await flush();
    expect(document.body.textContent).toContain("disk");
    expect(await violations()).toEqual([]);
  });

  it("names Retry after its model, keeps focus on the row when it goes away, and announces the change", async () => {
    let state: RuntimeInfo = failed;
    mockDaemon({
      runtime_status: () => [state],
      runtime_retry: () => {
        state = { ...base, state: "starting", state_ms: 0, expected_ms: 20_000 };
        return { started: true };
      },
    });
    await render(<StatusPage />);
    await flush();
    const retry = card().querySelector<HTMLButtonElement>('button[aria-label="Retry local-chat-model"]')!;
    expect(retry.textContent).toBe("Retry");
    retry.focus();
    await click(retry);
    await flush();
    expect(card().querySelector('button[aria-label="Retry local-chat-model"]')).toBeNull();
    const row = card().querySelector("tbody th")!;
    expect(document.activeElement).toBe(row);
    // The page's one announcer (shared with indexing announcements).
    const live = document.querySelector('[role="status"][aria-atomic="true"]')!;
    // Starting is shown on the card, not read out (a crash loop would repeat it every cycle).
    expect(live.textContent).toBe("");
    // Ready later: said once, not on every poll.
    state = { ...base, state: "ready" };
    await act(async () => {
      await new Promise((r) => setTimeout(r, 1_100));
    });
    await flush();
    expect(live.textContent).toBe("Local model local-chat-model: Ready");
  });

  it("moves no focus when the user went elsewhere before the model restarted, and says a crash loop's failure once", async () => {
    // A crash loop: failed with an automatic restart pending, so Status polls every second.
    const looping: RuntimeInfo = { ...failed, gave_up: false, retry_in_ms: 4_000 };
    let state: RuntimeInfo = looping;
    let polls = 0;
    mockDaemon({
      runtime_status: () => {
        polls++;
        return [state];
      },
      runtime_retry: () => ({ started: true }),
    });
    await render(<StatusPage />);
    await flush();
    const retry = card().querySelector<HTMLButtonElement>('button[aria-label="Retry local-chat-model"]')!;
    retry.focus();
    await click(retry);
    await flush();
    // Still failed after the retry; the user moves on.
    const elsewhere = document.createElement("button");
    document.body.appendChild(elsewhere);
    elsewhere.focus();
    const tick = async () => {
      await act(async () => {
        await new Promise((r) => setTimeout(r, 1_100));
      });
      await flush();
    };
    state = { ...base, state: "starting", state_ms: 0, expected_ms: 20_000 };
    await tick();
    expect(document.activeElement).toBe(elsewhere);
    const live = document.querySelector('[role="status"][aria-atomic="true"]')!;
    state = looping;
    await tick();
    const first = live.textContent;
    expect(first).toContain("local-chat-model");
    state = { ...base, state: "starting", state_ms: 0, expected_ms: 20_000 };
    await tick();
    live.textContent = "";
    state = looping;
    await tick();
    // The second failure of the loop is not read out again.
    expect(live.textContent).toBe("");
    expect(polls).toBeGreaterThan(3);
    elsewhere.remove();
  });

  it("a Retry that fails keeps focus on Retry and says why", async () => {
    mockDaemon({
      runtime_status: [failed],
      runtime_retry: () => {
        throw { kind: "rpc", message: "The model file is missing. Install it again from Models.", code: -32000 };
      },
    });
    await render(<StatusPage />);
    await flush();
    const retry = card().querySelector<HTMLButtonElement>('button[aria-label="Retry local-chat-model"]')!;
    retry.focus();
    await click(retry);
    await flush();
    expect(document.activeElement).toBe(retry);
    expect(card().querySelector('[role="alert"]')?.textContent).toContain("The model file is missing.");
    expect(await violations()).toEqual([]);
  });

  it("shows a named loading state, then a plain error with Retry, for the runtime card", async () => {
    let fail: (e: unknown) => void = () => {};
    mockDaemon({ runtime_status: () => new Promise((_, reject) => (fail = reject)) });
    await render(<StatusPage />);
    await flush();
    expect(card().querySelector('.pg-loading[role="status"]')?.textContent).toContain("Checking local models");
    expect(await violations()).toEqual([]);
    await act(async () => fail({ kind: "rpc", message: "local runtime unavailable", code: -32000 }));
    await flush();
    const alert = card().querySelector('[role="alert"]')!;
    expect(alert.textContent).toContain("local runtime unavailable");
    expect(Array.from(alert.querySelectorAll("button")).map((b) => b.textContent)).toContain("Retry");
    expect(await violations()).toEqual([]);
  });
});
