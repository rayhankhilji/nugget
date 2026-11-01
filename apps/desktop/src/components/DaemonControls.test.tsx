// @vitest-environment jsdom
/** GG6-10 version banner and GG6-20 no-tray controls on the Status page. */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { type ShellInfo, type VersionReport } from "../api";
import { byText, cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import { NoTrayControls, VersionBanner } from "./DaemonControls";

beforeEach(installDomShims);
afterEach(cleanup);

const report = (patch: Partial<VersionReport>): VersionReport => ({
  app_version: "0.2.0",
  daemon_version: "0.1.0",
  state: "in_step",
  reason: null,
  detail: null,
  ...patch,
});

const shell = (patch: Partial<ShellInfo> = {}): ShellInfo => ({
  has_tray: false,
  indexing_paused: false,
  logs: { dir: "/home/me/.local/share/nugget/logs", shell: "…/nugget-desktop.<date>.log", daemon: "…/nuggetd.<date>.log" },
  ...patch,
});

describe("VersionBanner", () => {
  it("tells the user what to do with a service it can't restart, and checks again", async () => {
    let next = report({ state: "mismatch", reason: "not_owned" });
    const calls = mockDaemon({
      daemon_version: () => next,
      daemon_version_recheck: () => next,
    });
    const { container } = await render(<VersionBanner />);
    const alert = container.querySelector('[role="alert"]');
    expect(alert?.textContent).toContain("still version 0.1.0, but this app is 0.2.0");
    expect(alert?.textContent).toContain("started outside the app");
    next = report({ daemon_version: "0.2.0" });
    await click(byText(container, "button", "Check again"));
    expect(calls).toContain("daemon_version_recheck");
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });
});

describe("NoTrayControls", () => {
  it("pauses and resumes indexing from the window and points to the kill switch", async () => {
    let paused = false;
    mockDaemon({
      indexing_pause_toggle: () => {
        paused = !paused;
        return shell({ indexing_paused: paused });
      },
    });
    let info = shell();
    const { container, unmount } = await render(<NoTrayControls info={info} onChange={(s) => (info = s)} />);
    expect(container.textContent).toContain("Closing this window quits Nugget.");
    expect(container.textContent).toContain("kill switch on the Safety page");
    await click(byText(container, "button", "Pause indexing"));
    expect(info.indexing_paused).toBe(true);
    unmount();
    const again = await render(<NoTrayControls info={info} onChange={() => {}} />);
    expect(again.container.textContent).toContain("Indexing is paused.");
    expect(byText(again.container, "button", "Resume indexing")).toBeTruthy();
  });
});
