// @vitest-environment jsdom
import axe from "axe-core";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { Onboarding } from "./Onboarding";
import { INITIAL, loadState } from "./machine";
import { cheapestMonthly } from "./Fork";
import type { CloudPlan, CloudSession } from "../cloud";

beforeEach(() => installDomShims());
afterEach(cleanup);

const heading = (c: HTMLElement) => c.querySelector("h1")?.textContent;
const card = (c: HTMLElement, path: "cloud" | "local") => c.querySelector<HTMLButtonElement>(`button.ob-fork-card[data-path="${path}"]`)!;
const foot = (c: HTMLElement, path: "cloud" | "local") => card(c, path).querySelector(".ob-fork-foot")?.textContent;

const signedOut: CloudSession = { configured: true, signed_in: false, email: null };
const plan = (id: string, monthly: number): CloudPlan =>
  ({ id, name: id, monthly_gbp: monthly, annual_gbp: monthly * 10, trial_days: 14, entitlements: {} }) as unknown as CloudPlan;

async function violations(): Promise<string[]> {
  const result = await axe.run(document.body, { rules: { "color-contrast": { enabled: false } } });
  return result.violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

describe("first-run fork", () => {
  it("shows two equal cards, nothing preselected, the cheapest plan's price and the local copy", async () => {
    const calls = mockDaemon({
      cloud_session: () => signedOut,
      cloud_billing_plans: () => ({ plans: [plan("pro", 79), plan("plus", 39), plan("team", 129)] }),
    });
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    expect(heading(container)).toBe("How do you want to run Heed?");
    expect(container.querySelectorAll(".ob-fork > button.ob-fork-card")).toHaveLength(2);
    expect(card(container, "cloud").getAttribute("aria-pressed")).toBe("false");
    expect(card(container, "local").getAttribute("aria-pressed")).toBe("false");
    expect(calls).toContain("cloud_billing_plans");
    expect(foot(container, "cloud")).toBe("14 days free, then from £39 a month");
    expect(card(container, "local").querySelector("p")?.textContent).toBe(
      "Your data stays on this computer. Only the passages a question needs go to the AI you choose, or nothing at all with a local model.",
    );
    expect(foot(container, "local")).toBe("Free");
    // Choosing moves on: no Continue on the fork.
    expect(byText(container, "footer button", "Continue")).toBeUndefined();
    expect(await violations()).toEqual([]);
  });

  it("Heed Cloud leads to the sign-in, and Back returns to the fork with the choice shown", async () => {
    mockDaemon({ cloud_session: () => signedOut, cloud_billing_plans: () => ({ plans: [plan("plus", 39)] }) });
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    await click(card(container, "cloud"));
    expect(heading(container)).toBe("Sign in to Heed Cloud");
    expect(byText(container, "button", "Continue with Google")).toBeDefined();
    expect(loadState()).toMatchObject({ path: "cloud", step: "welcome" });
    await click(byText(container, "footer button", "Back"));
    expect(heading(container)).toBe("How do you want to run Heed?");
  });

  it("Run it yourself leads to the AI choice, with Sign in with ChatGPT offered", async () => {
    mockDaemon({ cloud_session: () => signedOut, cloud_billing_plans: () => ({ plans: [] }) });
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    await click(card(container, "local"));
    expect(heading(container)).toBe("Choose your AI");
    expect(loadState()).toMatchObject({ path: "local", step: "ai", completed: ["welcome"] });
    const options = [...container.querySelectorAll<HTMLInputElement>('.ai-choices input[type="radio"]')].map((r) => r.value);
    expect(options).toEqual(["chatgpt", "local", "ollama", "anthropic", "openai", "openrouter", "compatible", "later"]);
    // Back on the fork, the choice made is shown.
    await click(byText(container, "footer button", "Back"));
    expect(card(container, "local").getAttribute("aria-pressed")).toBe("true");
  });

  it("says only “14-day free trial” when the plans can't be read", async () => {
    mockDaemon({
      cloud_session: () => signedOut,
      cloud_billing_plans: () => {
        throw { kind: "other", message: "offline" };
      },
    });
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    await flush();
    expect(foot(container, "cloud")).toBe("14-day free trial");
  });

  it("without cloud in this build: no plans call, the trial line, and picking Heed Cloud says it's unavailable", async () => {
    const calls = mockDaemon({ cloud_session: () => ({ configured: false, signed_in: false, email: null }) });
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    await flush();
    expect(calls).not.toContain("cloud_billing_plans");
    expect(foot(container, "cloud")).toBe("14-day free trial");
    await click(card(container, "cloud"));
    expect(heading(container)).toBe("How do you want to run Heed?");
    expect(card(container, "cloud").getAttribute("aria-pressed")).toBe("true");
    expect(container.querySelector(".ob-fork-note")?.textContent).toContain("Heed Cloud isn't available in this version of Heed.");
  });

  it("hides Sign in with ChatGPT on the cloud path and when signed in to Heed Cloud", async () => {
    mockDaemon({ cloud_session: () => signedOut });
    const cloudPath = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "ai", path: "cloud", completed: ["welcome", "source"] }} />);
    expect(cloudPath.container.querySelector('input[value="chatgpt"]')).toBeNull();
    expect(cloudPath.container.querySelector('input[value="anthropic"]')).not.toBeNull();
    cleanup();

    mockDaemon({ cloud_session: () => ({ configured: true, signed_in: true, email: "ana@example.com" }) });
    const signedIn = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "ai", completed: ["welcome", "source"] }} />);
    expect(signedIn.container.querySelector('input[value="chatgpt"]')).toBeNull();
  });
});

describe("cheapestMonthly", () => {
  it("takes the lowest monthly price and ignores plans without one", () => {
    expect(cheapestMonthly([plan("a", 79), plan("b", 39)])).toBe(39);
    expect(cheapestMonthly([plan("free", 0)])).toBeNull();
    expect(cheapestMonthly([])).toBeNull();
    expect(cheapestMonthly(undefined)).toBeNull();
  });
});
