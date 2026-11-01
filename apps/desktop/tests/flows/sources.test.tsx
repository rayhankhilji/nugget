// @vitest-environment jsdom
/** Sources: list, add-source wizard (cancel and add), sync, sign-in for a source that needs auth. */
import { describe, expect, it } from "vitest";
import { EVENT, type SourceInfo } from "../../src/api";
import { sources as fixtureSources } from "../../src/test/fixtures";
import {
  click,
  daemon,
  emitEvent,
  findByRole,
  findByText,
  getByRole,
  press,
  queryByRole,
  renderApp,
  setupHarness,
  t,
  type,
  waitFor,
  within,
} from "../harness";

setupHarness();

/** The list item for the source with this name. */
const row = (name: string) => getByRole("heading", { name, level: 2 }).closest("li")!;

describe("sources flow", () => {
  it("lists every source with its state and counts", async () => {
    const d = daemon();
    await renderApp({ page: "sources" });
    expect(d.count("sources_list")).toBe(1);

    const docs = within(row("Documents"));
    expect(row("Documents").textContent).toContain(t("sources.items", { n: 1234 }));
    expect(docs.getByRole("switch", { name: /Enabled/ })).toHaveProperty("checked", true);
    expect(row("Work mail").textContent).toContain(t("sources.needsSignIn"));
    expect(row("Work mail").textContent).toContain(t("sources.neverSynced"));
  });

  it("adds a folder through the wizard and shows it in the list", async () => {
    const added: SourceInfo = { id: "files-2", kind: "files", name: "Projects", enabled: true, state: "syncing", items: 0, chunks: 0, last_sync: null };
    const d = daemon({ sources_add: added });
    // The list refetched after adding includes the new source.
    d.once("sources_list", fixtureSources).on("sources_list", [...fixtureSources, added]);
    await renderApp({ page: "sources" });

    await click(getByRole("button", { name: t("sources.add") }));
    await click(within(getByRole("dialog")).getByRole("button", { name: t("common.next") }));
    const dialog = getByRole("dialog", { name: t("addSource.addTitle.files") });
    const path = within(dialog).getByRole("textbox", { name: t("addSource.folder.pathLabel") });
    await type(path, "relative/path");
    await press(path, "Enter");
    expect(within(dialog).getByRole("alert").textContent).toBe(t("addSource.folder.fullPath"));
    await type(path, "/home/me/Projects");
    await press(path, "Enter");
    expect(within(dialog).getByRole("button", { name: t("addSource.folder.removePath", { path: "/home/me/Projects" }) })).toBeTruthy();

    await click(within(dialog).getByRole("button", { name: t("addSource.folder.submit") }));
    expect(d.callsTo("sources_add")).toEqual([
      { request: { id: null, kind: "files", name: "Projects", settings: expect.objectContaining({ roots: ["/home/me/Projects"] }) } },
    ]);
    expect(queryByRole("dialog")).toBeNull();
    await findByRole("heading", { name: "Projects", level: 2 });
  });

  it("adds Google Drive with its opt-ins, saves the secret and starts sign-in", async () => {
    const added: SourceInfo = { id: "gdrive-1", kind: "gdrive", name: "Google Drive", enabled: true, state: "needs_auth", items: 0, chunks: 0, last_sync: null };
    const url = "https://accounts.google.com/o/oauth2/v2/auth?client_id=x";
    const d = daemon({ sources_add: added, sources_secret_set: null, sources_auth_begin: { url }, open_url: "opened" });
    await renderApp({ page: "sources" });

    await click(getByRole("button", { name: t("sources.add") }));
    await click(within(getByRole("dialog")).getByRole("radio", { name: /Google Drive/ }));
    await click(within(getByRole("dialog")).getByRole("button", { name: t("common.next") }));
    const dialog = getByRole("dialog", { name: t("addSource.addTitle.gdrive") });
    await type(within(dialog).getByRole("textbox", { name: t("addSource.google.clientId") }), "1234-abc.apps.googleusercontent.com");
    const secret = dialog.querySelector<HTMLInputElement>('input[type="password"]')!;
    await type(secret, "s3cret");
    await click(within(dialog).getByRole("checkbox", { name: new RegExp(t("addSource.gdrive.sharedWithMe")) }));
    await click(within(dialog).getByRole("button", { name: t("addSource.google.submit") }));

    expect(d.callsTo("sources_add")).toEqual([
      {
        request: {
          id: null,
          kind: "gdrive",
          name: "Google Drive",
          settings: { client_id: "1234-abc.apps.googleusercontent.com", include_shared_with_me: true, include_shared_drives: false },
        },
      },
    ]);
    expect(d.callsTo("sources_secret_set")).toEqual([{ request: { id: "gdrive-1", secret: "s3cret" } }]);
    expect(d.callsTo("sources_auth_begin")).toEqual([{ request: { id: "gdrive-1" } }]);
    expect(d.callsTo("open_url")).toEqual([{ url }]);
  });

  it("offers sign-in for a source that needs auth and opens the browser", async () => {
    const url = "https://accounts.google.com/o/oauth2/v2/auth?client_id=x";
    const d = daemon({ sources_auth_begin: { url }, open_url: "opened" });
    await renderApp({ page: "sources" });

    expect(within(row("Documents")).queryByRole("button", { name: t("sources.signIn") })).toBeNull();
    await click(within(row("Work mail")).getByRole("button", { name: t("sources.signIn") }));
    expect(d.callsTo("sources_auth_begin")).toEqual([{ request: { id: "gmail-1" } }]);
    expect(d.callsTo("open_url")).toEqual([{ url }]);
    await findByText(t("sources.finishSignIn"));

    // Sign-in completes: the daemon reports the source as watching.
    await emitEvent(EVENT.sourcesChanged, { sources: [{ ...fixtureSources[1]!, state: "watching", items: 10 }] });
    await waitFor(() => expect(row("Work mail").textContent).not.toContain(t("sources.needsSignIn")));
    expect(within(row("Work mail")).getByRole("button", { name: t("sources.signInAgain") })).toBeTruthy();
  });
});
