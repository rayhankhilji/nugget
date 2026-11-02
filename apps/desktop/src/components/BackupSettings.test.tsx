// @vitest-environment jsdom
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { backupInfo, backupStatus } from "../test/fixtures";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { BackupSettings, RecoveryBanner, resetRestoreJob } from "./BackupSettings";

beforeEach(installDomShims);
afterEach(() => {
  cleanup();
  resetRestoreJob();
});

/** The control a visible `<label>` names. */
function field(root: ParentNode, label: string): HTMLInputElement | HTMLSelectElement {
  const l = byText(root, "label", label);
  const el = l && document.getElementById(l.getAttribute("for") ?? "");
  if (!el) throw new Error(`no field labelled ${label}`);
  return el as HTMLInputElement | HTMLSelectElement;
}

async function type(el: HTMLInputElement | HTMLSelectElement, value: string) {
  const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
  await act(async () => {
    Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
  });
}

const section = () => document.querySelector<HTMLElement>('section[aria-labelledby="backup-title"]')!;

describe("BackupSettings", () => {
  it("validates the passphrase before setting it, and never keeps it after", async () => {
    const set: unknown[] = [];
    mockDaemon({
      backup_set_passphrase: (a: unknown) => {
        set.push(a);
        return { ok: true };
      },
    });
    await render(<BackupSettings />);
    const s = section();
    const submit = () => click(byText(s, "button", "Change backup passphrase"));

    await type(field(s, "Passphrase"), "short");
    await type(field(s, "Confirm passphrase"), "short");
    await submit();
    expect(s.textContent).toContain("Use at least 8 characters.");
    expect(set).toEqual([]);

    await type(field(s, "Passphrase"), "correct horse battery");
    await type(field(s, "Confirm passphrase"), "correct horse batery");
    await submit();
    expect(s.textContent).toContain("The two passphrases don't match.");
    expect(set).toEqual([]);

    await type(field(s, "Confirm passphrase"), "correct horse battery");
    await submit();
    expect(set).toEqual([{ request: { passphrase: "correct horse battery" } }]);
    expect(s.textContent).toContain("Backup passphrase set.");
    expect((field(s, "Passphrase") as HTMLInputElement).value).toBe("");
    expect((field(s, "Confirm passphrase") as HTMLInputElement).value).toBe("");
  });

  it("restores: confirms data is set aside, sends the passphrase, then restarts the daemon", async () => {
    const order: string[] = [];
    const restored: unknown[] = [];
    let up = true;
    mockDaemon({
      backup_restore: (a: unknown) => {
        order.push("backup_restore");
        restored.push(a);
        up = false; // the daemon exits by itself after replying
        return { staged: true, restart_required: true };
      },
      connection_info: () => {
        order.push("connection_info");
        return { connected: up, socket: "/tmp/nuggetd.sock", error: up ? null : "not running" };
      },
      start_daemon: () => {
        order.push("start_daemon");
        up = true;
        return { connected: true, socket: "/tmp/nuggetd.sock", error: null };
      },
    });
    await render(<BackupSettings />);
    const s = section();
    await click(byText(s, "button", "Restore…"));
    const d = document.querySelector("dialog")!;
    expect(d.querySelector("h2")?.textContent).toBe("Restore this backup?");
    expect(d.textContent).toContain("Your current data is set aside in Heed's data folder, not deleted.");
    await type(field(d, "Backup passphrase"), "correct horse battery");
    await click(byText(d, "button", "Restore and restart"));
    await flush(10);

    expect(restored).toEqual([{ request: { path: backupInfo.path, passphrase: "correct horse battery" } }]);
    expect(order.indexOf("backup_restore")).toBeLessThan(order.indexOf("start_daemon"));
    expect(order.filter((c) => c === "start_daemon")).toHaveLength(1);
    expect(document.querySelector("dialog")).toBeNull();
    expect(s.textContent).toMatch(/Restored\. Heed is running with the backup from /);
  });
});

describe("RecoveryBanner", () => {
  it("explains the damaged database and restores from the latest backup", async () => {
    const restored: unknown[] = [];
    let up = true;
    mockDaemon({
      backup_status: {
        ...backupStatus,
        recovery: { reason: "database disk image is malformed", db_path: "/home/me/.nugget/data/nugget.db", moved_to: "/home/me/.nugget/data/nugget.db.damaged-20261030", latest_backup: backupInfo },
      },
      backup_restore: (a: unknown) => {
        restored.push(a);
        up = false;
        return { staged: true, restart_required: true };
      },
      connection_info: () => ({ connected: up, socket: "/tmp/nuggetd.sock", error: null }),
      start_daemon: () => ((up = true), { connected: true, socket: "/tmp/nuggetd.sock", error: null }),
    });
    const { container } = await render(<RecoveryBanner />);
    const alert = container.querySelector('[role="alert"]')!;
    expect(alert.textContent).toContain("Heed found its database damaged");
    expect(alert.textContent).toContain("Heed has not written to it");
    expect(alert.querySelector("code")?.textContent).toBe("/home/me/.nugget/data/nugget.db.damaged-20261030");
    expect(alert.textContent).toContain("database disk image is malformed");

    await click(byText(alert, "button", "Restore from latest backup"));
    const d = document.querySelector("dialog")!;
    await click(byText(d, "button", "Restore and restart"));
    await flush(10);
    expect(restored).toEqual([{ request: { path: backupInfo.path } }]);
  });
});
