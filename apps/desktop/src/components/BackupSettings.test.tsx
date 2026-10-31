// @vitest-environment jsdom
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { EVENT, type BackupStatus, type ExportResult } from "../api";
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
  it("renders the status: folder, schedule, passphrase, last backup and the backup list", async () => {
    mockDaemon();
    await render(<BackupSettings />);
    const s = section();
    expect(s.querySelector("h2")?.textContent).toBe("Backup and export");
    expect((field(s, "Backup folder") as HTMLInputElement).value).toBe("/home/me/Backups");
    expect((field(s, "Schedule") as HTMLSelectElement).value).toBe("daily");
    expect((field(s, "Backups to keep") as HTMLInputElement).value).toBe("7");
    expect(s.textContent).toContain("Set");
    expect(s.textContent).toContain("1,234 items");
    expect(s.textContent).toContain("Nugget 0.0.1 on Linux");
    expect(byText(s, "button", "Restore…")?.getAttribute("aria-label")).toMatch(/^Restore the backup from /);
    expect(byText(s, "button", "Back up now")?.hasAttribute("disabled")).toBe(false);
    // Plain warnings are visible, not hidden behind a toggle.
    expect(s.textContent).toContain("If you forget this passphrase and lose this computer, your backups can't be opened. Nobody can recover them.");
    expect(s.textContent).toContain("The export is not encrypted.");
  });

  it("asks for setup before Back up now and saves the schedule", async () => {
    const configured: unknown[] = [];
    const empty: BackupStatus = { ...backupStatus, folder: null, passphrase_set: false, last_backup: null, next_due: null };
    mockDaemon({
      backup_status: empty,
      backup_configure: (a: unknown) => {
        configured.push(a);
        return { ...empty, folder: "/mnt/usb/nugget", interval_hours: 168, keep: 4 };
      },
    });
    await render(<BackupSettings />);
    const s = section();
    expect(byText(s, "button", "Back up now")?.hasAttribute("disabled")).toBe(true);
    expect(s.textContent).toContain("Set a backup folder and a passphrase first.");
    expect(s.textContent).toContain("Not set");

    // Empty folder is refused before anything is sent.
    await click(byText(s, "button", "Save backup settings"));
    expect(configured).toEqual([]);
    expect(s.textContent).toContain("Enter a folder for your backups.");
    expect(field(s, "Backup folder").getAttribute("aria-invalid")).toBe("true");

    await type(field(s, "Backup folder"), "/mnt/usb/nugget");
    await type(field(s, "Schedule"), "weekly");
    await type(field(s, "Backups to keep"), "4");
    await click(byText(s, "button", "Save backup settings"));
    expect(configured).toEqual([{ request: { folder: "/mnt/usb/nugget", interval_hours: 168, keep: 4 } }]);
    expect(s.textContent).toContain("Backup settings saved.");
  });

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

  it("backs up now and shows the result in a live region", async () => {
    const calls = mockDaemon({ backup_run: { ...backupInfo, items: 1300, bytes: 1_048_576 } });
    await render(<BackupSettings />);
    const s = section();
    await click(byText(s, "button", "Back up now"));
    await flush();
    expect(calls).toContain("backup_run");
    const done = byText(s, "p", /^Backup finished: 1,300 items/);
    expect(done).toBeTruthy();
    expect(done!.closest("[aria-live]")?.getAttribute("aria-live")).toBe("polite");
  });

  it("shows the last error from the daemon", async () => {
    mockDaemon({ backup_status: { ...backupStatus, last_error: "the folder is not writable" } });
    await render(<BackupSettings />);
    expect(section().textContent).toContain("The last backup failed: the folder is not writable");
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
    expect(d.textContent).toContain("Your current data is set aside in Nugget's data folder, not deleted.");
    await type(field(d, "Backup passphrase"), "correct horse battery");
    await click(byText(d, "button", "Restore and restart"));
    await flush(10);

    expect(restored).toEqual([{ request: { path: backupInfo.path, passphrase: "correct horse battery" } }]);
    expect(order.indexOf("backup_restore")).toBeLessThan(order.indexOf("start_daemon"));
    expect(order.filter((c) => c === "start_daemon")).toHaveLength(1);
    expect(document.querySelector("dialog")).toBeNull();
    expect(s.textContent).toMatch(/Restored\. Nugget is running with the backup from /);
  });

  it("keeps the dialog open and suggests the passphrase when a restore without one fails", async () => {
    const calls = mockDaemon({
      backup_restore: () => {
        throw { kind: "rpc", code: -32602, message: "this backup was made with a different key; its passphrase is needed" };
      },
    });
    await render(<BackupSettings />);
    await click(byText(section(), "button", "Restore…"));
    const d = document.querySelector("dialog")!;
    await click(byText(d, "button", "Restore and restart"));
    await flush();
    expect(calls).not.toContain("start_daemon");
    expect(d.textContent).toContain("The restore didn't finish: this backup was made with a different key");
    expect(d.textContent).toContain("enter its passphrase and try again");
    expect(byText(d, "button", "Try again")).toBeTruthy();
  });

  it("exports to a folder and shows the counts and the path", async () => {
    const exported: unknown[] = [];
    const result: ExportResult = { path: "/tmp/out/nugget-export-20261030T090000Z", items: 1234, entities: 56, claims: 789, audit_entries: 10, egress_rows: 3, vault_files: 42 };
    let release: () => void = () => {};
    mockDaemon({
      export_run: (a: unknown) => {
        exported.push(a);
        return new Promise((r) => (release = () => r(result)));
      },
    });
    await render(<BackupSettings />);
    const s = section();
    await click(byText(s, "button", "Export everything"));
    expect(s.textContent).toContain("Enter a folder to export to.");
    expect(exported).toEqual([]);

    await type(field(s, "Export folder"), "/tmp/out");
    await click(byText(s, "button", "Export everything"));
    expect(exported).toEqual([{ request: { folder: "/tmp/out" } }]);
    await act(async () => {
      await emit(EVENT.notification, { method: "export.progress", params: { phase: "items", done: 500 } });
    });
    await flush();
    expect(s.textContent).toContain("Exporting items: 500");
    await act(async () => release());
    await flush();
    expect(s.querySelector("code")?.textContent).toBe(result.path);
    expect(s.textContent).toContain("Export finished in");
    const counts = Object.fromEntries(Array.from(s.querySelectorAll("dl.stats > div")).map((d) => [d.querySelector("dt")?.textContent, d.querySelector("dd")?.textContent]));
    expect(counts).toMatchObject({ Items: "1,234", Entities: "56", Facts: "789", "Audit entries": "10", "Network log rows": "3", "Vault files": "42" });
  });

  it("says when the daemon has no backups yet", async () => {
    mockDaemon({ backup_status: () => Promise.reject({ kind: "rpc", code: -32601, message: "method not found" }) });
    await render(<BackupSettings />);
    expect(section().textContent).toContain("Not available in this daemon version.");
  });
});

describe("RecoveryBanner", () => {
  it("renders nothing when the database is healthy", async () => {
    mockDaemon();
    const { container } = await render(<RecoveryBanner />);
    expect(container.textContent).toBe("");
  });

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
    expect(alert.textContent).toContain("Nugget found its database damaged");
    expect(alert.textContent).toContain("Nugget has not written to it");
    expect(alert.querySelector("code")?.textContent).toBe("/home/me/.nugget/data/nugget.db.damaged-20261030");
    expect(alert.textContent).toContain("database disk image is malformed");

    await click(byText(alert, "button", "Restore from latest backup"));
    const d = document.querySelector("dialog")!;
    await click(byText(d, "button", "Restore and restart"));
    await flush(10);
    expect(restored).toEqual([{ request: { path: backupInfo.path } }]);
  });

  it("says when there is no backup to restore from", async () => {
    mockDaemon({ backup_status: { ...backupStatus, recovery: { reason: "", db_path: "/x/nugget.db", moved_to: null, latest_backup: null } } });
    const { container } = await render(<RecoveryBanner />);
    expect(container.textContent).toContain("/x/nugget.db");
    expect(container.textContent).toContain("No backup was found.");
    expect(byText(container, "button", "Restore from latest backup")).toBeUndefined();
  });
});
