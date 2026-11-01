// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { SourceInfo, SourceSpec } from "../api";
import { t } from "../i18n";
import { byText, cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import { MessagingForm } from "./MessagingForm";

beforeEach(installDomShims);
afterEach(cleanup);

/** The control a visible `<label>` names (its text starts with `label`; optional fields add a suffix). */
function field(label: string): HTMLInputElement {
  const l = Array.from(document.querySelectorAll("label")).find((e) => e.textContent?.startsWith(label) && e.htmlFor);
  const el = l && document.getElementById(l.htmlFor);
  if (!el) throw new Error(`no field labelled ${label}`);
  return el as HTMLInputElement;
}

/** The checkbox inside the `.check` label with this text. */
function checkbox(label: string): HTMLInputElement {
  const l = byText(document, "label.check", new RegExp(`^${label.replace(/[()]/g, "\\$&")}`));
  const el = l?.querySelector("input");
  if (!el) throw new Error(`no checkbox ${label}`);
  return el;
}

async function type(el: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

const submit = () => click(document.querySelector('button[type="submit"]'));

function added(kind: string, name: string): SourceInfo {
  return { id: `${kind}-1`, kind, name, enabled: true, state: "syncing", items: 0, chunks: 0, last_sync: null };
}

/** Records each `sources_add` request; answers with a new source, or throws `fail` when given. */
function daemon(fail?: { kind: string; message: string }) {
  const requests: SourceSpec[] = [];
  const calls = mockDaemon({
    sources_add: (args: unknown) => {
      const r = (args as { request: SourceSpec }).request;
      requests.push(r);
      if (fail) throw fail;
      return added(r.kind, r.name);
    },
    pick_folders: ["/Users/me/Exports/WhatsApp"],
  });
  return { requests, calls };
}

const noop = () => {};

describe("MessagingForm", () => {
  it("iMessage on a Mac: shows the Full Disk Access steps and adds with the options", async () => {
    const { requests } = daemon();
    let got: SourceInfo | null = null;
    let closed = false;
    const { container } = await render(
      <MessagingForm kind="imessage" os="macos" onBack={noop} onClose={() => (closed = true)} onAdded={(s) => (got = s)} />,
    );
    const steps = container.querySelectorAll(".messaging-fda li");
    expect(steps).toHaveLength(5);
    expect(steps[1]?.textContent).toContain("Full Disk Access");
    expect(container.textContent).toContain(t("addSource.imessage.copyNote"));
    expect(checkbox(t("addSource.messages.private")).checked).toBe(true);
    expect(checkbox(t("addSource.imessage.contacts")).checked).toBe(true);
    // Focus starts on the first option.
    expect(document.activeElement).toBe(checkbox(t("addSource.imessage.contacts")));

    await click(checkbox(t("addSource.messages.groups")));
    await type(field(t("addSource.messages.since")), "30");
    await submit();

    expect(requests).toEqual([
      { id: null, kind: "imessage", name: "iMessage", settings: { private: true, include_groups: false, since_days: 30, contacts: true } },
    ]);
    expect(got).not.toBeNull();
    expect(closed).toBe(true);
  });

  it("iMessage elsewhere: says it needs a Mac and never calls the daemon", async () => {
    const { calls } = daemon();
    const { container } = await render(<MessagingForm kind="imessage" os="windows" onBack={noop} onClose={noop} onAdded={noop} />);
    expect(container.textContent).toContain(t("addSource.imessage.macOnly"));
    expect(container.querySelector(".messaging-fda")).toBeNull();
    const add = container.querySelector<HTMLButtonElement>('button[type="submit"]')!;
    expect(add.getAttribute("aria-disabled")).toBe("true");
    expect(document.getElementById(add.getAttribute("aria-describedby")!)?.textContent).toBe(t("addSource.imessage.macOnlyShort"));
    await submit();
    expect(calls).not.toContain("sources_add");
  });

  it("iMessage: the Full Disk Access error comes back with the steps", async () => {
    const message =
      "Nugget needs Full Disk Access to read your Messages. Open System Settings → Privacy & Security → Full Disk Access and turn on Nugget, then try again.";
    daemon({ kind: "rpc", message });
    let got: SourceInfo | null = null;
    const { container } = await render(<MessagingForm kind="imessage" os="macos" onBack={noop} onClose={noop} onAdded={(s) => (got = s)} />);
    await submit();
    const alert = container.querySelector('[role="alert"]');
    expect(alert?.textContent).toContain(message);
    expect(alert?.querySelectorAll("li")).toHaveLength(5);
    expect(got).toBeNull();
  });

  it("iMessage: after granting access, Add again adds the source", async () => {
    const message = "Nugget needs Full Disk Access to read your Messages.";
    const requests: SourceSpec[] = [];
    mockDaemon({
      sources_add: (args: unknown) => {
        const r = (args as { request: SourceSpec }).request;
        requests.push(r);
        if (requests.length === 1) throw { kind: "unauthorized", message };
        return added(r.kind, r.name);
      },
    });
    let got: SourceInfo | null = null;
    const { container } = await render(<MessagingForm kind="imessage" os="macos" onBack={noop} onClose={noop} onAdded={(s) => (got = s)} />);
    const steps = container.querySelectorAll(".help .messaging-fda li");
    expect(steps[steps.length - 1]?.textContent).toBe(t("addSource.imessage.fdaStep5").replace(/<\/?b>/g, ""));
    await submit();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(message);
    await submit();
    expect(requests).toHaveLength(2);
    expect(requests[1]).toEqual(requests[0]);
    expect(got).not.toBeNull();
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });

  it("a failed folder picker is announced without marking the field invalid", async () => {
    mockDaemon({
      pick_folders: () => {
        throw { kind: "internal", message: "no dialog" };
      },
    });
    const { container } = await render(<MessagingForm kind="telegram" onBack={noop} onClose={noop} onAdded={noop} />);
    await click(byText(container, "button", t("addSource.messages.browse")));
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("no dialog");
    expect(field(t("addSource.telegram.path")).getAttribute("aria-invalid")).toBeNull();
  });

  it("WhatsApp: export steps, a folder from the picker, your name, private by default", async () => {
    const { requests, calls } = daemon();
    const { container } = await render(<MessagingForm kind="whatsapp" onBack={noop} onClose={noop} onAdded={noop} />);
    expect(container.querySelectorAll(".help ol li")).toHaveLength(4);
    expect(container.textContent).toContain("Export chat");

    // Adding without a folder points at the field, which is already invalid when it gets focus.
    const folder = field(t("addSource.whatsapp.folder"));
    let invalidOnFocus: string | null = null;
    folder.addEventListener("focus", () => (invalidOnFocus = folder.getAttribute("aria-invalid")));
    await submit();
    expect(invalidOnFocus).toBe("true");
    expect(folder.getAttribute("aria-invalid")).toBe("true");
    expect(document.getElementById(folder.getAttribute("aria-describedby")!.split(" ").pop()!)?.textContent).toBe(
      t("addSource.messages.whatsappFolderMissing"),
    );
    expect(document.activeElement).toBe(folder);
    expect(requests).toHaveLength(0);

    await click(byText(container, "button", t("addSource.messages.browse")));
    expect(calls).toContain("pick_folders");
    expect(folder.value).toBe("/Users/me/Exports/WhatsApp");
    await type(field(t("addSource.whatsapp.me")), "Ana Souza");
    await submit();
    expect(requests).toEqual([
      {
        id: null,
        kind: "whatsapp",
        name: "WhatsApp",
        settings: { private: true, include_groups: true, folder: "/Users/me/Exports/WhatsApp", me: "Ana Souza" },
      },
    ]);
  });

  it("Telegram: a typed export path, a bad number of days is refused", async () => {
    const { requests } = daemon();
    const { container } = await render(<MessagingForm kind="telegram" onBack={noop} onClose={noop} onAdded={noop} />);
    expect(container.textContent).toContain("Machine-readable JSON");
    await type(field(t("addSource.telegram.path")), "/home/me/Downloads/Telegram Desktop");
    await type(field(t("addSource.messages.since")), "0");
    await submit();
    expect(field(t("addSource.messages.since")).getAttribute("aria-invalid")).toBe("true");
    expect(requests).toHaveLength(0);

    await type(field(t("addSource.messages.since")), "");
    await click(checkbox(t("addSource.messages.private")));
    await submit();
    expect(requests).toEqual([
      { id: null, kind: "telegram", name: "Telegram", settings: { private: false, include_groups: true, path: "/home/me/Downloads/Telegram Desktop" } },
    ]);
  });
});

describe("Sources page", () => {
  it("shows the Full Disk Access steps under an iMessage source that lacks it", async () => {
    const { SourcesPage } = await import("../pages/Sources");
    const fda = "Nugget needs Full Disk Access to read your Messages. Open System Settings → Privacy & Security → Full Disk Access and turn on Nugget, then try again.";
    const broken: SourceInfo = { ...added("imessage", "iMessage"), id: "im-1", state: "error", last_error: fda };
    const fine: SourceInfo = { ...added("whatsapp", "WhatsApp"), id: "wa-1", state: "idle", last_error: "Folder /x not found" };
    mockDaemon({ sources_list: [broken, fine] });
    const { container } = await render(<SourcesPage />);
    const rows = Array.from(container.querySelectorAll("li")).filter((li) => li.querySelector("h2"));
    const im = rows.find((li) => li.querySelector("h2")?.textContent === "iMessage")!;
    const wa = rows.find((li) => li.querySelector("h2")?.textContent === "WhatsApp")!;
    const steps = im.querySelectorAll(".messaging-fda li");
    expect(steps).toHaveLength(5);
    // Not "press Add": on the Sources page the way back is Sync now.
    expect(steps[4]?.textContent).toBe(t("addSource.imessage.fdaStep5Later").replace(/<\/?b>/g, ""));
    expect(wa.querySelector(".messaging-fda")).toBeNull();
  });
});

describe("MessagingForm states (LL2)", () => {
  it("keeps focus on Add while saving, then says a shell failure plainly with no axe violations", async () => {
    let fail: (e: unknown) => void = () => {};
    const requests: unknown[] = [];
    mockDaemon({
      pick_folders: ["/Users/me/Exports/WhatsApp"],
      sources_add: (args: unknown) => {
        requests.push(args);
        return new Promise((_, reject) => (fail = reject));
      },
    });
    const { container } = await render(<MessagingForm kind="whatsapp" onBack={noop} onClose={noop} onAdded={noop} />);
    await click(byText(container, "button", t("addSource.messages.browse")));
    const add = container.querySelector<HTMLButtonElement>('button[type="submit"]')!;
    add.focus();
    await submit();
    // Busy: still focused and focusable (aria-disabled), and a second press sends nothing more.
    expect(add.disabled).toBe(false);
    expect(add.getAttribute("aria-disabled")).toBe("true");
    expect(document.activeElement).toBe(add);
    await submit();
    expect(requests).toHaveLength(1);
    await act(async () => fail({ kind: "io", message: "Broken pipe (os error 32)" }));
    const alert = container.querySelector('[role="alert"]')!;
    expect(alert.textContent).toBe(t("common.error.generic"));
    expect(document.activeElement).toBe(add);
    expect(add.getAttribute("aria-disabled")).toBeNull();
    const r = await axe.run(container, { rules: { "color-contrast": { enabled: false }, region: { enabled: false } } });
    expect(r.violations.map((v) => v.id)).toEqual([]);
  });

  it("shows the daemon's own refusal (a duplicate folder) as it is", async () => {
    mockDaemon({
      pick_folders: ["/Users/me/Exports/WhatsApp"],
      sources_add: () => {
        throw { kind: "rpc", message: "This folder is already a source: WhatsApp.", code: -32602 };
      },
    });
    const { container } = await render(<MessagingForm kind="whatsapp" onBack={noop} onClose={noop} onAdded={noop} />);
    await click(byText(container, "button", t("addSource.messages.browse")));
    await submit();
    expect(container.querySelector('[role="alert"]')?.textContent).toBe("This folder is already a source: WhatsApp.");
  });
});
