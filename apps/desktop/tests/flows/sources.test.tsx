// @vitest-environment jsdom
/** Sources: list, add-source wizard (cancel and add), sync, sign-in for a source that needs auth. */
import { describe, expect, it } from "vitest";
import { EVENT, type SourceInfo } from "../../src/api";
import { sources as fixtureSources } from "../../src/test/fixtures";
import {
  click,
  daemon,
  emitEvent,
  errors,
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

  it("opens the add-source wizard and cancels it without adding anything", async () => {
    const d = daemon();
    await renderApp({ page: "sources" });
    await click(getByRole("button", { name: t("sources.add") }));

    const dialog = getByRole("dialog", { name: t("addSource.title") });
    // Focus starts on the selected kind.
    expect(document.activeElement).toBe(within(dialog).getByRole("radio", { name: /Folder/ }));
    await click(within(dialog).getByRole("button", { name: t("common.cancel") }));
    expect(queryByRole("dialog")).toBeNull();
    expect(d.count("sources_add")).toBe(0);
  });

  it("closes the wizard with Escape", async () => {
    daemon();
    await renderApp({ page: "sources" });
    await click(getByRole("button", { name: t("sources.add") }));
    await press(document.activeElement, "Escape");
    expect(queryByRole("dialog")).toBeNull();
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

  it("syncs one source by id, and all sources with a null id", async () => {
    const d = daemon({ sources_sync: null });
    await renderApp({ page: "sources" });

    await click(within(row("Documents")).getByRole("button", { name: t("sources.syncNow") }));
    expect(d.callsTo("sources_sync")).toEqual([{ request: { id: "files-1" } }]);
    await findByText(t("sources.announce.syncing", { name: "Documents" }));

    await click(getByRole("button", { name: t("sources.syncAll") }));
    expect(d.last("sources_sync")).toEqual({ request: { id: null } });
  });

  it("shows a sync failure on the row", async () => {
    daemon().fail("sources_sync", errors.rpc("source is busy"));
    await renderApp({ page: "sources" });
    await click(within(row("Documents")).getByRole("button", { name: t("sources.syncNow") }));
    expect(within(row("Documents")).getByRole("alert").textContent).toContain("source is busy");
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

  it("disables a source with its switch", async () => {
    const d = daemon({ sources_update: { ...fixtureSources[0]!, enabled: false, state: "disabled" } });
    await renderApp({ page: "sources" });
    await click(within(row("Documents")).getByRole("switch"));
    expect(d.callsTo("sources_update")).toEqual([{ request: { id: "files-1", enabled: false } }]);
    expect(within(row("Documents")).getByRole("switch")).toHaveProperty("checked", false);
    expect(within(row("Documents")).getByRole("button", { name: t("sources.syncNow") })).toHaveProperty("disabled", true);
  });

  it("shows an empty state when there are no sources", async () => {
    daemon({ sources_list: [] });
    await renderApp({ page: "sources" });
    await findByText(t("sources.empty.title"));
    expect(getByRole("button", { name: t("sources.syncAll") })).toHaveProperty("disabled", true);
  });
});
