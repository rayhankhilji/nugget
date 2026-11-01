// @vitest-environment jsdom
import axe from "axe-core";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { SearchUpgradeStatus } from "../api";
import { byText, cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import { SearchUpgrade } from "./Models";

beforeEach(() => installDomShims());
afterEach(cleanup);

const offered: SearchUpgradeStatus = { offer: { from: "bge-small-en", to: "multilingual-e5-small", download_bytes: 120_000_000 }, job: null };
const running: SearchUpgradeStatus = { offer: null, job: { to: "multilingual-e5-small", phase: "embedding", done: 0, total: 10, error: null } };
const none: SearchUpgradeStatus = { offer: null, job: null };

async function violations(): Promise<string[]> {
  const result = await axe.run(document.body, { rules: { "color-contrast": { enabled: false } } });
  return result.violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

describe("multilingual search upgrade (Models page)", () => {
  it("offers the upgrade: Upgrade starts it, Not now dismisses it for good", async () => {
    const calls = mockDaemon({ search_upgrade_status: offered, search_upgrade_start: running });
    const { container } = await render(<SearchUpgrade />);
    expect(container.textContent).toContain("Upgrade search to multilingual");
    expect(container.textContent).toContain("re-indexes in the background");
    expect(await violations()).toEqual([]);

    await click(byText(container, "button", "Upgrade"));
    expect(calls).toContain("search_upgrade_start");
    expect(container.textContent).toContain("Search keeps working meanwhile.");
    expect(container.querySelector("progress")).not.toBeNull();
    cleanup();

    const again = mockDaemon({ search_upgrade_status: offered, search_upgrade_dismiss: none });
    const second = await render(<SearchUpgrade />);
    await click(byText(second.container, "button", "Not now"));
    expect(again).toContain("search_upgrade_dismiss");
    expect(second.container.querySelector("section")).toBeNull();
  });

  it("shows nothing on an older daemon", async () => {
    mockDaemon();
    const { container } = await render(<SearchUpgrade />);
    expect(container.innerHTML).toBe("");
  });
});
