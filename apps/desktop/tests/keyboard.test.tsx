// @vitest-environment jsdom
/**
 * Keyboard focus regressions found by the keyboard-only e2e scenario (e2e/keyboard.mjs): Tab
 * wraps inside dialogs instead of leaving the page, the card action dialog keeps focus as its
 * buttons change, and busy buttons keep focus instead of dropping it to the page body.
 */
import { describe, expect, it } from "vitest";
import type { PolicyState } from "../src/api";
import { Dialog } from "../src/components/Dialog";
import { SafetyPage } from "../src/pages/Safety";
import { click, daemon, getByRole, press, render, setupHarness, t } from "./harness";

setupHarness();

const active = () => document.activeElement as HTMLElement | null;

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
