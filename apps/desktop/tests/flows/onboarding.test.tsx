// @vitest-environment jsdom
/**
 * Onboarding through the whole app: first launch shows setup, every step can be walked,
 * skipped or revisited, finishing (or skipping) lands in the app and is remembered, and it
 * works with the background service down. Step-level details live in src/onboarding/*.test.
 */
import { describe, expect, it } from "vitest";
import { EVENT } from "../../src/api";
import { loadState } from "../../src/onboarding/machine";
import {
  click,
  daemon,
  emitEvent,
  findByText,
  getAllByRole,
  getByRole,
  queryByRole,
  render,
  renderApp,
  setupHarness,
  t,
  within,
} from "../harness";
import { App } from "../../src/App";

setupHarness();

const heading = () => getByRole("heading", { level: 1 }).textContent;
const button = (name: string) => getByRole("button", { name });
const radio = (value: string) => getAllByRole("radio").find((r) => (r as HTMLInputElement).value === value)!;
const nav = () => queryByRole("navigation", { name: t("app.navLabel") });

describe("onboarding flow", () => {
  it("shows on first launch and walks every step into the app", async () => {
    const d = daemon({ models_install: null });
    await renderApp({ onboarding: true });
    expect(nav()).toBeNull();
    expect(heading()).toBe(t("onboarding.welcome.heading"));
    expect(getByRole("main").textContent).toContain(t("onboarding.stepOf", { n: 1, total: 6 }));

    await click(button(t("onboarding.continue")));
    expect(heading()).toBe(t("onboarding.ai.heading"));
    expect(document.activeElement).toBe(getByRole("heading", { level: 1 }));

    // Local AI: install the recommended model.
    await click(radio("local"));
    const card = await findByText(/qwen3-4b-q4/, { selector: "li" });
    await click(within(card).getByRole("button", { name: t("onboarding.models.install") }));
    expect(d.callsTo("models_install")).toEqual([{ id: "qwen3-4b-q4" }]);
    await emitEvent(EVENT.modelsProgress, { id: "qwen3-4b-q4", downloaded: 2_500_000_000, total: 2_500_000_000, phase: "installed", error: null });
    expect(d.count("models_list")).toBeGreaterThanOrEqual(2);

    await click(button(t("onboarding.continue")));
    expect(heading()).toBe(t("onboarding.source.heading"));
    await click(button(t("onboarding.source.add")));
    expect(getByRole("dialog", { name: t("addSource.title") })).toBeTruthy();
    await click(within(getByRole("dialog")).getByRole("button", { name: t("common.cancel") }));
    expect(queryByRole("dialog")).toBeNull();

    await click(button(t("onboarding.continue")));
    expect(heading()).toBe(t("onboarding.perm.heading"));
    // Back goes one step back without losing progress.
    await click(button(t("onboarding.back")));
    expect(heading()).toBe(t("onboarding.source.heading"));
    await click(button(t("onboarding.continue")));
    await click(button(t("onboarding.continue")));
    expect(heading()).toBe(t("onboarding.trigger.heading"));

    await click(button(t("onboarding.skipStep")));
    expect(heading()).toBe(t("onboarding.done.heading"));
    expect(getByRole("main").textContent).toContain(t("onboarding.done.pending", { count: 1 }));
    expect(loadState()).toMatchObject({ ai: "local", skipped: ["trigger"], finished: false });

    await click(button(t("onboarding.done.start")));
    expect(nav()).not.toBeNull();
    expect(loadState().finished).toBe(true);
    // The first page after setup is the default (chat).
    expect(within(nav()!).getByRole("button", { name: t("app.nav.chat") }).getAttribute("aria-current")).toBe("page");
  });

  it("is not shown again once finished", async () => {
    daemon();
    await renderApp({ onboarding: true });
    await click(button(t("onboarding.skipAll")));
    expect(nav()).not.toBeNull();

    // Next launch.
    document.body.innerHTML = "";
    await render(<App />);
    expect(nav()).not.toBeNull();
    expect(queryByRole("heading", { name: t("onboarding.welcome.heading") })).toBeNull();
  });

  it("can be skipped from any step, recording what was left", async () => {
    daemon();
    await renderApp({ onboarding: true });
    await click(button(t("onboarding.continue")));
    await click(radio("later"));
    await click(button(t("onboarding.continue")));
    expect(heading()).toBe(t("onboarding.source.heading"));
    await click(button(t("onboarding.skipAll")));
    expect(nav()).not.toBeNull();
    expect(loadState()).toMatchObject({ finished: true, ai: "later", completed: ["welcome", "ai"], skipped: ["source", "permissions", "trigger"] });
  });

  it("jumps to a step from the progress list", async () => {
    daemon();
    await renderApp({ onboarding: true });
    const progress = getByRole("navigation", { name: t("onboarding.progressLabel") });
    const steps = within(progress).getAllByRole("button");
    expect(steps).toHaveLength(6);
    expect(steps[0]!.getAttribute("aria-current")).toBe("step");
    await click(steps[3]!);
    expect(heading()).toBe(t("onboarding.perm.heading"));
    expect(steps[3]!.getAttribute("aria-current")).toBe("step");
  });

  it("runs again from the sidebar", async () => {
    daemon();
    await renderApp({ page: "status" });
    await click(button(t("settings.setupAgain")));
    expect(heading()).toBe(t("onboarding.welcome.heading"));
    expect(nav()).toBeNull();
  });

  it("works with the background service down, then shows the offline screen", async () => {
    const d = daemon().offline();
    await renderApp({ onboarding: true });
    expect(d.commands).toContain("connection_info");

    await click(button(t("onboarding.continue")));
    await click(radio("local"));
    expect(getByRole("main").textContent).toContain(t("onboarding.offline"));
    expect(d.count("models_list")).toBe(0);

    await click(button(t("onboarding.continue")));
    expect(heading()).toBe(t("onboarding.source.heading"));
    expect(queryByRole("button", { name: t("onboarding.source.add") })).toBeNull();
    expect(getByRole("main").textContent).toContain(t("onboarding.offline"));

    await click(button(t("onboarding.skipAll")));
    expect(getByRole("heading", { name: t("offline.title") })).toBeTruthy();
    expect(button(t("offline.start"))).toBeTruthy();
  });

  it("catches up when the service comes up mid-setup", async () => {
    daemon().offline();
    await renderApp({ onboarding: true });
    await click(button(t("onboarding.continue")));
    await click(radio("local"));
    expect(getByRole("main").textContent).toContain(t("onboarding.offline"));

    await emitEvent(EVENT.connection, { connected: true, socket: "/tmp/nuggetd.sock", error: null });
    await findByText(/qwen3-4b-q4/, { selector: "li" });
    expect(getByRole("main").textContent).not.toContain(t("onboarding.offline"));
  });
});
