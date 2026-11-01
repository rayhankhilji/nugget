// @vitest-environment jsdom
/** II4: notification / quit preferences, onboarding opt-ins, deep links and shell labels. */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
import { fromTime, PlatformPrefs, toTime } from "../components/AppSettings";
import type { PlatformSettings } from "../api";
import { DesktopOptIns } from "./DesktopOptIns";
import { DeepLinks, fileName, findItemUrl } from "./DeepLinks";
import { SHELL_KEYS, shellLabels } from "./shellLabels";

const defaults = (): PlatformSettings => ({
  notifications: { enabled: false, brief: true, agent: true, quiet: null },
  quit_stops_daemon: false,
  paused_sources: [],
});

function checkbox(root: ParentNode, label: RegExp): HTMLInputElement {
  const el = Array.from(root.querySelectorAll("label")).find((l) => label.test(l.textContent ?? ""));
  const input = el?.querySelector("input");
  if (!input) throw new Error(`no checkbox labelled ${label}`);
  return input;
}

async function click(el: HTMLElement) {
  await act(async () => {
    el.click();
  });
  await flush();
}

function button(root: ParentNode, text: RegExp): HTMLButtonElement {
  const b = Array.from(root.querySelectorAll("button")).find((x) => text.test(x.textContent ?? ""));
  if (!b) throw new Error(`no button ${text}`);
  return b;
}

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("quiet-hour times", () => {
  it("round-trips HH:MM and rejects non-times", () => {
    expect(toTime(22 * 60)).toBe("22:00");
    expect(toTime(7 * 60 + 5)).toBe("07:05");
    expect(fromTime("07:05")).toBe(425);
    expect(fromTime("24:00")).toBeNull();
    expect(fromTime("")).toBeNull();
  });
});

describe("PlatformPrefs", () => {
  it("opts in to notifications, sets quiet hours and the quit behaviour", async () => {
    let saved = defaults();
    const sent: PlatformSettings[] = [];
    mockDaemon({
      platform_settings_get: () => saved,
      platform_settings_set: (a: unknown) => {
        saved = (a as { settings: PlatformSettings }).settings;
        sent.push(saved);
        return saved;
      },
    });
    const { container } = await render(<PlatformPrefs />);
    const notify = checkbox(container, /^Show notifications/);
    expect(notify.checked).toBe(false);
    expect(notify.getAttribute("role")).toBe("switch");
    // Per-kind switches appear only once notifications are on.
    expect(() => checkbox(container, /Agents waiting/)).toThrow();
    await click(notify);
    expect(sent.at(-1)?.notifications.enabled).toBe(true);
    await click(checkbox(container, /Quiet hours/));
    expect(sent.at(-1)?.notifications.quiet).toEqual({ start: 1320, end: 420 });
    const from = container.querySelector<HTMLInputElement>('input[type="time"]');
    expect(from?.value).toBe("22:00");
    expect(container.querySelector(`label[for="${from?.id}"]`)?.textContent).toBe("From");
    await click(checkbox(container, /Also stop the background service/));
    expect(sent.at(-1)?.quit_stops_daemon).toBe(true);
  });

  it("keeps the old state when saving fails", async () => {
    mockDaemon({
      platform_settings_get: defaults,
      platform_settings_set: () => {
        throw { kind: "other", message: "disk full" };
      },
    });
    const { container } = await render(<PlatformPrefs />);
    await click(checkbox(container, /^Show notifications/));
    expect(checkbox(container, /^Show notifications/).checked).toBe(false);
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("disk full");
  });
});

describe("DesktopOptIns", () => {
  it("offers open-at-login and notifications, both off", async () => {
    const calls = mockDaemon({
      autostart_get: () => ({ supported: true, enabled: false }),
      autostart_set: (a: unknown) => ({ supported: true, enabled: (a as { enabled: boolean }).enabled }),
      platform_settings_get: defaults,
      platform_settings_set: (a: unknown) => (a as { settings: PlatformSettings }).settings,
    });
    const { container } = await render(<DesktopOptIns />);
    expect(container.querySelector("section h2")?.textContent).toBe("On this computer");
    const login = checkbox(container, /in the tray when I log in/);
    const notify = checkbox(container, /^Show notifications/);
    expect([login.checked, notify.checked]).toEqual([false, false]);
    await click(login);
    await click(notify);
    expect(calls).toContain("autostart_set");
    expect(calls).toContain("platform_settings_set");
    expect(checkbox(container, /^Show notifications/).checked).toBe(true);
  });
});

describe("DeepLinks", () => {
  it("navigates to a linked page without asking", async () => {
    const went: string[] = [];
    mockDaemon({ deeplink_take: () => ({ kind: "page", page: "agent" }) });
    const { container } = await render(<DeepLinks pages={["today", "agent"]} go={(p) => went.push(p)} />);
    expect(went).toEqual(["agent"]);
    expect(container.querySelector("dialog")).toBeNull();
  });

  it("ignores a page the app doesn't have", async () => {
    const went: string[] = [];
    mockDaemon({ deeplink_take: () => ({ kind: "page", page: "nope" }) });
    await render(<DeepLinks pages={["today"]} go={(p) => went.push(p)} />);
    expect(went).toEqual([]);
  });

  it("asks before opening an item, and opens nothing on Cancel", async () => {
    const calls = mockDaemon({
      deeplink_take: () => ({ kind: "item", source: "notes", key: "plan.md" }),
      search: () => [{ item: { source: "notes", key: "plan.md" }, title: "Plan", snippet: "", score: 1, url: "file:///n/plan.md" }],
      open_url: () => "opened",
    });
    const { container } = await render(<DeepLinks pages={["today"]} go={() => {}} />);
    const dialog = container.ownerDocument.querySelector("dialog");
    expect(dialog?.textContent).toContain("“plan.md” from notes");
    expect(calls).not.toContain("search");
    await click(button(container.ownerDocument, /^Cancel$/));
    expect(calls).not.toContain("open_url");
    expect(container.ownerDocument.querySelector("dialog")).toBeNull();
  });

  it("opens the confirmed item through open_url", async () => {
    const opened: unknown[] = [];
    mockDaemon({
      deeplink_take: () => ({ kind: "item", source: "notes", key: "plan.md" }),
      search: () => [
        { item: { source: "other", key: "plan.md" }, title: "x", snippet: "", score: 2, url: "file:///other" },
        { item: { source: "notes", key: "plan.md" }, title: "Plan", snippet: "", score: 1, url: "file:///n/plan.md" },
      ],
      open_url: (a: unknown) => {
        opened.push(a);
        return "opened";
      },
    });
    const { container } = await render(<DeepLinks pages={["today"]} go={() => {}} />);
    await click(button(container.ownerDocument, /^Open$/));
    expect(opened).toEqual([{ url: "file:///n/plan.md" }]);
  });

  it("asks before handing a plugin file to the Plugins page", async () => {
    const went: string[] = [];
    mockDaemon({ deeplink_take: () => ({ kind: "plugin", path: "/home/a/Downloads/hello.nuggetplugin" }) });
    const { container } = await render(<DeepLinks pages={["plugins"]} go={(p) => went.push(p)} />);
    expect(container.ownerDocument.querySelector("dialog")?.textContent).toContain("hello.nuggetplugin");
    expect(went).toEqual([]);
    await click(button(container.ownerDocument, /^Continue$/));
    expect(went).toEqual(["plugins"]);
  });

  it("finds item URLs by exact reference and names files on any OS", async () => {
    mockDaemon({ search: () => [{ item: { source: "s", key: "k2" }, title: null, snippet: "", score: 1, url: "u" }] });
    expect(await findItemUrl("s", "k")).toBeNull();
    expect(fileName("C:\\Users\\a\\p.nuggetplugin")).toBe("p.nuggetplugin");
    expect(fileName("/tmp/p.nuggetplugin")).toBe("p.nuggetplugin");
  });
});

describe("shell labels", () => {
  it("cover exactly the shell's own keys", () => {
    const rust = readFileSync(resolve(process.cwd(), "src-tauri/src/platform.rs"), "utf8");
    const keys = [...rust.matchAll(/\(\s*"(shell\.[a-zA-Z.]+)",/g)].map((m) => m[1]);
    expect([...SHELL_KEYS].sort()).toEqual([...new Set(keys)].sort());
    expect(shellLabels()["shell.tray.quit"]).toBe("Quit Nugget");
  });

  it("are sent to the shell", async () => {
    const sent: unknown[] = [];
    mockDaemon({ shell_labels_set: (a: unknown) => (sent.push(a), 0) });
    const { useShellLabels } = await import("./shellLabels");
    function Probe() {
      useShellLabels();
      return null;
    }
    await render(<Probe />);
    expect(Object.keys((sent[0] as { labels: Record<string, string> }).labels)).toHaveLength(SHELL_KEYS.length);
  });
});
