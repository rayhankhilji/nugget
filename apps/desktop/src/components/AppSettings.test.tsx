// @vitest-environment jsdom
/** The desktop settings on the Status page: updates, start-at-login and open-at-login. */
import axe from "axe-core";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
import { AppSettings } from "./AppSettings";
import { UpdateSettings } from "./UpdateSettings";

const updateStatus = {
  current_version: "0.0.1",
  configured: true,
  settings: { channel: "stable", auto_install: false, start_at_login: true, autostart: false },
  available: { version: "0.0.2", notes: "Faster search", date: 1_790_000_000 },
  staged: false,
  busy: false,
  last_checked: 1_790_000_100,
  last_error: null,
};

function checkbox(root: ParentNode, label: RegExp): HTMLInputElement {
  const el = Array.from(root.querySelectorAll("label")).find((l) => label.test(l.textContent ?? ""));
  const input = el?.querySelector("input");
  if (!input) throw new Error(`no checkbox labelled ${label}`);
  return input;
}

async function toggle(input: HTMLInputElement) {
  await act(async () => {
    input.click();
  });
  await flush();
}

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("AppSettings", () => {
  it("turns open-at-login on and reflects the shell's answer", async () => {
    let enabled = false;
    const sent: unknown[] = [];
    const calls = mockDaemon({
      autostart_get: () => ({ supported: true, enabled }),
      autostart_set: (args: unknown) => {
        sent.push(args);
        enabled = (args as { enabled: boolean }).enabled;
        return { supported: true, enabled };
      },
    });
    const { container } = await render(<AppSettings />);
    const box = checkbox(container, /in the tray when I log in/);
    expect(box.checked).toBe(false);
    expect(box.getAttribute("aria-describedby")).toBeTruthy();
    await toggle(box);
    expect(calls).toContain("autostart_set");
    expect(sent).toEqual([{ enabled: true }]);
    expect(checkbox(container, /in the tray when I log in/).checked).toBe(true);
  });

  it("shows the shell's error and keeps the old state", async () => {
    mockDaemon({
      autostart_set: () => {
        throw { kind: "other", message: "couldn't write the entry", code: null };
      },
    });
    const { container } = await render(<AppSettings />);
    const box = checkbox(container, /in the tray when I log in/);
    await toggle(box);
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("couldn't write the entry");
    expect(checkbox(container, /in the tray when I log in/).checked).toBe(false);
  });

  it("says when the platform has no login entry", async () => {
    mockDaemon({ autostart_get: { supported: false, enabled: false } });
    const { container } = await render(<AppSettings />);
    expect(container.querySelector("input")).toBeNull();
    expect(container.textContent).toContain("isn't available on this system");
  });
});

describe("UpdateSettings", () => {
  it("renders translated, accessible settings with the app section", async () => {
    mockDaemon({ updates_status: updateStatus });
    const { container } = await render(<UpdateSettings />);
    expect(container.textContent).toContain("Nugget 0.0.2 is available");
    expect(container.textContent).toContain("Install 0.0.2 and restart");
    expect(container.textContent).toContain("Faster search");
    expect(checkbox(container, /background service at login/).checked).toBe(true);
    expect(checkbox(container, /in the tray when I log in/).checked).toBe(false);
    const result = await axe.run(container, { rules: { "color-contrast": { enabled: false } } });
    expect(result.violations.map((v) => v.id)).toEqual([]);
  });
});
