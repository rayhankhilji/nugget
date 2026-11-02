// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatgptStatus } from "../api";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { ChatgptPanel, CHATGPT_POLL_MS } from "./Chatgpt";
import { Onboarding } from "./Onboarding";
import { INITIAL } from "./machine";

beforeEach(() => installDomShims());
afterEach(() => {
  vi.useRealTimers();
  cleanup();
});

const status = (s: Partial<ChatgptStatus>): ChatgptStatus => ({
  state: "signed_out",
  account: null,
  plan_usage: false,
  paused: false,
  error: null,
  models: { light: null, smart: null },
  manage_usage_url: "https://chatgpt.com/settings/usage",
  ...s,
});
const signedIn = (s: Partial<ChatgptStatus> = {}) =>
  status({ state: "signed_in", account: { email: "sam@example.com", name: "Sam" }, plan_usage: true, models: { light: "gpt-4.1-mini", smart: "gpt-4.1" }, ...s });

async function violations(): Promise<string[]> {
  const result = await axe.run(document.body, { rules: { "color-contrast": { enabled: false } } });
  return result.violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

const button = (c: HTMLElement, text: string) => byText(c, "button", text);

describe("Sign in with ChatGPT", () => {
  it("signs in through the browser, polls about once a second, and can be cancelled", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    let current = status({});
    const calls = mockDaemon({
      chatgpt_status: () => current,
      chatgpt_sign_in: () => (current = status({ state: "signing_in" })),
      chatgpt_cancel: () => (current = status({ error: { code: "cancelled", message: "cancelled" } })),
    });
    const { container } = await render(<main><ChatgptPanel /></main>);
    expect(container.querySelector(".chatgpt-fineprint")?.textContent).toBe("Uses your ChatGPT plan. Heed never sees your password.");
    await click(button(container, "Sign in with ChatGPT"));
    expect(calls).toContain("chatgpt_sign_in");
    expect(container.textContent).toContain("Waiting for your browser…");
    const before = calls.filter((c) => c === "chatgpt_status").length;
    await act(async () => {
      vi.advanceTimersByTime(CHATGPT_POLL_MS * 2);
    });
    await flush();
    expect(calls.filter((c) => c === "chatgpt_status").length).toBe(before + 2);
    expect(await violations()).toEqual([]);

    await click(byText(container, ".note button", "Cancel"));
    expect(calls).toContain("chatgpt_cancel");
    expect(container.textContent).not.toContain("Waiting for your browser…");
    expect(container.textContent).toContain("Sign-in cancelled.");
    // Polling stopped.
    const after = calls.filter((c) => c === "chatgpt_status").length;
    await act(async () => {
      vi.advanceTimersByTime(CHATGPT_POLL_MS * 3);
    });
    expect(calls.filter((c) => c === "chatgpt_status").length).toBe(after);
  });

  it("finishes the sign-in: the provider row, the model, and the step set up", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    let current = status({ state: "signing_in" });
    mockDaemon({ chatgpt_status: () => current });
    const ready: string[] = [];
    const { container } = await render(<main><ChatgptPanel onReady={(w) => ready.push(w)} /></main>);
    current = signedIn();
    await act(async () => {
      vi.advanceTimersByTime(CHATGPT_POLL_MS);
    });
    await flush();
    const row = container.querySelector('.provider-status li[data-provider="chatgpt"]')!;
    expect(row.querySelector(".provider-dot")?.getAttribute("data-health")).toBe("ok");
    expect(row.querySelector(".provider-detail")?.textContent).toBe("Signed in as sam@example.com");
    expect(row.querySelector(".provider-model")?.textContent).toBe("gpt-4.1");
    expect(button(container, "Sign out")).toBeDefined();
    expect(ready).toEqual(["ChatGPT"]);
    expect(await violations()).toEqual([]);
  });

  it("plan usage off: says so, signs in again or uses a key, and never switches by itself", async () => {
    let current = signedIn({ plan_usage: false, error: { code: "scope_not_granted", message: "scope" } });
    const calls = mockDaemon({
      chatgpt_status: () => current,
      chatgpt_sign_in: () => (current = status({ state: "signing_in" })),
    });
    const ready: string[] = [];
    const useKey: number[] = [];
    const { container } = await render(<ChatgptPanel onReady={(w) => ready.push(w)} onUseKey={() => useKey.push(1)} />);
    const alert = container.querySelector('[role="alert"]')!;
    expect(alert.textContent).toContain("Your ChatGPT plan usage isn't enabled");
    // One note for it, not a second generic error.
    expect(container.querySelectorAll('[role="alert"]')).toHaveLength(1);
    expect(container.querySelector(".provider-dot")?.hasAttribute("data-health")).toBe(false);
    expect(ready).toEqual([]);
    await click(button(container, "Use an API key instead"));
    expect(useKey).toEqual([1]);
    await click(button(container, "Try again"));
    expect(calls).toContain("chatgpt_sign_in");
  });

  it("usage limit: Manage usage opens ChatGPT's page, Resume resumes, Use another AI goes back", async () => {
    let current = signedIn({ paused: true, error: { code: "usage_limit", message: "limit" } });
    const opened: unknown[] = [];
    const calls = mockDaemon({
      chatgpt_status: () => current,
      chatgpt_resume: () => (current = signedIn()),
      open_url: (a: unknown) => {
        opened.push((a as { url: string }).url);
        return "opened";
      },
    });
    const other: number[] = [];
    const { container } = await render(<ChatgptPanel onOtherAi={() => other.push(1)} />);
    expect(container.querySelector(".provider-dot")?.getAttribute("data-health")).toBe("warn");
    expect(container.textContent).toContain("You've reached your ChatGPT usage limit");
    const manage = button(container, "Manage usage")!;
    expect(manage.classList.contains("btn-primary")).toBe(true);
    // No promise of when the limit resets.
    expect(container.textContent).not.toMatch(/reset|hour|tomorrow/i);
    await click(manage);
    expect(opened).toEqual(["https://chatgpt.com/settings/usage"]);
    await click(button(container, "Use another AI"));
    expect(other).toEqual([1]);
    expect(calls).not.toContain("ai_key_save");
    await click(button(container, "Resume"));
    expect(calls).toContain("chatgpt_resume");
    expect(container.textContent).not.toContain("usage limit");
    expect(container.querySelector(".provider-dot")?.getAttribute("data-health")).toBe("ok");
  });

  it("explains errors from the daemon", async () => {
    const cases: [ChatgptStatus, string][] = [
      [status({ error: { code: "invalid_id_token", message: "x" } }), "ChatGPT's answer couldn't be verified. Try signing in again."],
      [status({ error: { code: "access_denied", message: "x" } }), "The sign-in was turned down in the browser."],
      [status({ error: { code: "reauth_required", message: "x" } }), "Your ChatGPT sign-in has expired. Sign in again."],
      [status({ error: { code: "timeout", message: "x" } }), "The sign-in took too long and was stopped."],
      [status({ error: { code: "something_new", message: "x" } }), "The ChatGPT sign-in didn't work. Try again."],
    ];
    for (const [s, text] of cases) {
      mockDaemon({ chatgpt_status: () => s });
      const { container } = await render(<main><ChatgptPanel /></main>);
      expect(container.querySelector('[role="alert"]')?.textContent).toContain(text);
      cleanup();
    }
  });

  it("cloud_mode: sign-in opens nothing and says Heed Cloud brings its own AI", async () => {
    let current = status({});
    mockDaemon({
      chatgpt_status: () => current,
      // The shell opens nothing when the daemon sends no auth_url, and returns the status.
      chatgpt_sign_in: () => (current = status({ error: { code: "cloud_mode", message: "Heed Cloud is active" } })),
    });
    const { container } = await render(<main><ChatgptPanel /></main>);
    await click(button(container, "Sign in with ChatGPT"));
    expect(container.textContent).toContain("You're using Heed Cloud, which brings its own AI.");
    expect(button(container, "Sign in with ChatGPT")).toBeUndefined();
  });

  it("an older daemon says ChatGPT sign-in needs an update", async () => {
    mockDaemon({
      chatgpt_status: () => {
        throw { kind: "rpc", message: "method not found", code: -32601 };
      },
    });
    const { container } = await render(<main><ChatgptPanel /></main>);
    expect(container.textContent).toContain("can't sign in with ChatGPT yet");
    expect(button(container, "Sign in with ChatGPT")).toBeUndefined();
  });

  it("in onboarding: the row sets up the AI step, and “Use an API key instead” opens the OpenAI key row", async () => {
    mockDaemon({ chatgpt_status: () => signedIn({ plan_usage: false }) });
    const { container } = await render(
      <Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "ai", path: "local", completed: ["welcome"] }} />,
    );
    await click(container.querySelector('input[value="chatgpt"]'));
    expect(container.querySelector('.ai-choice[data-option="chatgpt"] .ai-choice-status')?.textContent).toBe("Opens your browser");
    await click(button(container, "Use an API key instead"));
    expect(container.querySelector<HTMLInputElement>('input[value="openai"]')!.checked).toBe(true);
    cleanup();

    mockDaemon({ chatgpt_status: () => signedIn() });
    const ok = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "ai", path: "local", completed: ["welcome"] }} />);
    await click(ok.container.querySelector('input[value="chatgpt"]'));
    await flush();
    // Set up: Continue no longer skips the step.
    expect(ok.container.querySelector("#ob-continue-hint")).toBeNull();
  });
});
