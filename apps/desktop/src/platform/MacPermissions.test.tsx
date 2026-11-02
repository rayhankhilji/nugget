// @vitest-environment jsdom
/** macOS first-run permissions: which panes are missing, onboarding's live status, re-checks. */
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { DoubleTapStatus } from "../api";
import { MacPermissionStatus } from "../onboarding/steps";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { missingPanes } from "./MacPermissions";

const mac: DoubleTapStatus = {
  platform: "macos",
  session: "macos",
  listening: true,
  trigger_error: null,
  accessibility: "denied",
  input_monitoring: "denied",
  full_disk_access: "denied",
  hint: null,
  modifiers: [],
  recommended_trigger: { kind: "double_tap", modifier: "right_command", window_ms: 350 },
};

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("missingPanes", () => {
  it("lists what is off, Full Disk Access only when a source needs it", () => {
    expect(missingPanes(mac, false)).toEqual(["accessibility", "input_monitoring"]);
    expect(missingPanes(mac, true)).toEqual(["accessibility", "input_monitoring", "full_disk_access"]);
    expect(missingPanes({ ...mac, accessibility: "granted", input_monitoring: "granted" }, true)).toEqual(["full_disk_access"]);
    expect(missingPanes({ ...mac, accessibility: "granted", input_monitoring: "granted", full_disk_access: "unknown" }, true)).toEqual([]);
    // Older shells don't report Full Disk Access.
    expect(missingPanes({ ...mac, accessibility: "granted", input_monitoring: "granted", full_disk_access: undefined }, true)).toEqual([]);
  });

  it("asks for nothing off macOS or without a status", () => {
    expect(missingPanes(undefined, true)).toEqual([]);
    expect(missingPanes({ ...mac, platform: "linux" }, true)).toEqual([]);
    expect(missingPanes({ ...mac, platform: "windows" }, true)).toEqual([]);
  });
});

describe("onboarding: MacPermissionStatus", () => {
  it("shows a button per missing pane with first-run steps, and flips once everything is on", async () => {
    let current: DoubleTapStatus = mac;
    const panes: string[] = [];
    mockDaemon({
      doubletap_status: () => current,
      sources_list: [{ id: "m", kind: "imessage", name: "Messages", enabled: true, state: "idle", items: 0 }],
      doubletap_open_privacy_settings: (args: unknown) => (panes.push((args as { pane: string }).pane), true),
    });
    const { container } = await render(<MacPermissionStatus />);
    await flush(10);
    const box = container.querySelector<HTMLElement>(".dt-recover")!;
    expect(box).not.toBeNull();
    // Nothing was asked yet: no "turn it back on".
    expect(box.textContent).toContain("Turn on Nugget in System Settings");
    expect(container.textContent).toContain("Full Disk Access lets Nugget read your Messages");
    await click(byText(box, "button", "Open Accessibility settings"));
    await click(byText(box, "button", "Open Input Monitoring settings"));
    await click(byText(box, "button", "Open Full Disk Access settings"));
    expect(panes).toEqual(["accessibility", "input_monitoring", "full_disk_access"]);

    current = { ...mac, accessibility: "granted", input_monitoring: "granted", full_disk_access: "granted" };
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await flush(10);
    expect(container.querySelector(".dt-recover")).toBeNull();
    expect(container.textContent).toContain("Everything Nugget needs on this Mac is turned on.");
  });

  it("renders nothing off macOS", async () => {
    mockDaemon({ doubletap_status: { ...mac, platform: "linux" }, sources_list: [] });
    const { container } = await render(<MacPermissionStatus />);
    await flush(10);
    expect(container.textContent).toBe("");
  });
});
