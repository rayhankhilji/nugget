// @vitest-environment jsdom
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
});
