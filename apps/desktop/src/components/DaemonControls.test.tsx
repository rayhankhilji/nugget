// @vitest-environment jsdom
/** GG6-10 version banner and GG6-20 no-tray controls on the Status page. */
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { DAEMON_VERSION_EVENT, type ShellInfo, type VersionReport } from "../api";
import { StatusPage } from "../pages/Status";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
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
  it("says nothing while the versions match", async () => {
    mockDaemon({ daemon_version: report({ daemon_version: "0.2.0" }) });
    const { container } = await render(<VersionBanner />);
    expect(container.textContent).toBe("");
  });

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

  it("follows the shell's reports: restarting, then a failed service restart", async () => {
    mockDaemon({ daemon_version: report({}) });
    const { container } = await render(<VersionBanner />);
    await act(() => emit(DAEMON_VERSION_EVENT, report({ state: "restarting" })));
    await flush();
    expect(container.querySelector('[role="status"]')?.textContent).toContain("Restarting it on the new version");
    await act(() =>
      emit(DAEMON_VERSION_EVENT, report({ state: "mismatch", reason: "service_restart_failed", detail: "unit not installed" })),
    );
    await flush();
    const text = container.querySelector('[role="alert"]')?.textContent ?? "";
    expect(text).toContain("restarting the login service didn't work: unit not installed");
    expect(text).toContain("Log out and back in");
  });
});

describe("VersionBanner, newer service", () => {
  it("asks to update the app instead of downgrading the service", async () => {
    mockDaemon({ daemon_version: report({ app_version: "0.1.0", daemon_version: "0.2.0", state: "mismatch", reason: "daemon_newer" }) });
    const { container } = await render(<VersionBanner />);
    const text = container.querySelector('[role="alert"]')?.textContent ?? "";
    expect(text).toContain("version 0.2.0, newer than this app (0.1.0)");
    expect(text).toContain("Update this app");
  });
});

describe("NoTrayControls", () => {
  it("is hidden when there is a tray", async () => {
    mockDaemon({});
    const { container } = await render(<NoTrayControls info={shell({ has_tray: true })} onChange={() => {}} />);
    expect(container.textContent).toBe("");
  });

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

describe("Status page", () => {
  it("shows the no-tray controls and where the logs are", async () => {
    mockDaemon({ shell_info: shell(), daemon_version: report({ daemon_version: "0.2.0" }) });
    const { container } = await render(<StatusPage />);
    expect(container.querySelector("#no-tray-title")?.textContent).toBe("Background controls");
    expect(container.textContent).toContain("Logs: /home/me/.local/share/nugget/logs");
  });

  it("leaves them out with a tray", async () => {
    mockDaemon({ shell_info: shell({ has_tray: true }) });
    const { container } = await render(<StatusPage />);
    expect(container.querySelector("#no-tray-title")).toBeNull();
  });
});
