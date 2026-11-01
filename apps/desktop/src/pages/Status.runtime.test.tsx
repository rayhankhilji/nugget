// @vitest-environment jsdom
/**
 * KK1: the Status page tells a warming-up local model apart from a failed one, and a failed one
 * offers Retry (runtime.retry) with the server's last line folded into Details.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { RuntimeInfo } from "../api";
import { cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { StatusPage } from "./Status";

beforeEach(installDomShims);
afterEach(cleanup);

const base: RuntimeInfo = { model_id: "qwen3-4b-q4km", role: "chat", state: "ready", provider: "local-chat" };

describe("Status: local model states (KK1)", () => {
  it("shows warm-up progress against the expected load time", async () => {
    mockDaemon({ runtime_status: [{ ...base, state: "starting", state_ms: 12_000, expected_ms: 40_000 }] });
    const { container } = await render(<StatusPage />);
    await flush();
    const card = container.querySelector('[aria-labelledby="runtime-title"]')!;
    expect(card.textContent).toContain("Warming up: 12s of about 40s");
    const bar = card.querySelector("progress")!;
    expect(bar.getAttribute("value")).toBe("30");
    expect(bar.getAttribute("aria-label")).toBe("Loading the model into memory");
    expect(card.querySelector("button")).toBeNull();
  });

  it("explains a failure plainly and retries the role on request", async () => {
    let retried: unknown = null;
    let state: RuntimeInfo = {
      ...base,
      state: "failed",
      error: "The on-device chat model keeps stopping, so Nugget stopped restarting it. Open Status to retry or to see its log.",
      gave_up: true,
      detail: "error: cannot allocate memory",
    };
    const wrapped = mockDaemon({
      runtime_status: () => [state],
      runtime_retry: (args: unknown) => {
        retried = args;
        state = { ...base, state: "starting", state_ms: 0, expected_ms: 20_000 };
        return { started: true };
      },
    });
    const { container } = await render(<StatusPage />);
    await flush();
    const card = container.querySelector('[aria-labelledby="runtime-title"]')!;
    expect(card.textContent).toContain("keeps stopping");
    expect(card.textContent).toContain("Nugget stopped trying after repeated failures.");
    // The technical line stays folded away under Details, with where the full log is.
    const details = card.querySelector("details")!;
    expect(details.querySelector("summary")!.textContent).toBe("Details");
    expect(details.textContent).toContain("error: cannot allocate memory");
    expect(details.textContent).toContain("llama-server-chat.log");
    const retry = Array.from(card.querySelectorAll("button")).find((b) => b.textContent === "Retry")!;
    expect(retry).toBeDefined();
    const before = wrapped.filter((c) => c === "runtime_status").length;
    await click(retry);
    await flush();
    expect(retried).toEqual({ role: "chat" });
    expect(wrapped.filter((c) => c === "runtime_status").length).toBeGreaterThan(before);
    expect(card.textContent).toContain("Warming up");
  });

  it("says when an automatic restart comes next", async () => {
    mockDaemon({
      runtime_status: [{ ...base, state: "failed", error: "The on-device chat model stopped responding; restarting it.", retry_in_ms: 2_500 }],
    });
    const { container } = await render(<StatusPage />);
    await flush();
    const card = container.querySelector('[aria-labelledby="runtime-title"]')!;
    expect(card.textContent).toContain("stopped responding");
    expect(card.textContent).toContain("Trying again automatically in 3s.");
  });
});
