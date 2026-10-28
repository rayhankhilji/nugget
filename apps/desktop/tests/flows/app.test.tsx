// @vitest-environment jsdom
/** App shell: connecting, the offline screen (retry / start), connection events, navigation. */
import { describe, expect, it } from "vitest";
import { EVENT } from "../../src/api";
import {
  click,
  daemon,
  emitConnection,
  emitEvent,
  errors,
  findByRole,
  getByRole,
  queryByRole,
  renderApp,
  setupHarness,
  t,
  waitFor,
  within,
} from "../harness";

setupHarness();

const OFFLINE = { connected: false, socket: "/tmp/nuggetd.sock", error: "connection refused" };
const ONLINE = { connected: true, socket: "/tmp/nuggetd.sock", error: null };

const offlineHeading = () => queryByRole("heading", { name: t("offline.title") });
/** The sidebar's connection status. */
const pill = () => {
  const labels = [t("app.conn.up"), t("app.conn.down"), t("app.conn.pending")];
  return within(getByRole("complementary")).getAllByRole("status").find((s) => labels.includes((s.textContent ?? "").trim()))!;
};
const navButton = (page: string) => within(getByRole("navigation", { name: t("app.navLabel") })).getByRole("button", { name: page });

describe("app shell", () => {
  it("shows the offline screen when the daemon isn't running, and retries", async () => {
    const d = daemon().offline();
    await renderApp({ page: "status" });
    expect(offlineHeading()).not.toBeNull();
    expect(pill().textContent).toContain(t("app.conn.down"));
    // Pages don't call the daemon while it's down.
    expect(d.count("status")).toBe(0);

    // Still down on the first retry, up on the second.
    d.once("connection_info", OFFLINE).once("connection_info", ONLINE);
    await click(getByRole("button", { name: t("offline.tryAgain") }));
    expect(offlineHeading()).not.toBeNull();
    await click(getByRole("button", { name: t("offline.tryAgain") }));
    expect(offlineHeading()).toBeNull();
    expect(pill().textContent).toContain(t("app.conn.up"));
    await waitFor(() => expect(d.count("status")).toBeGreaterThan(0));
    expect(d.count("connection_info")).toBeGreaterThanOrEqual(3);
  });

  it("starts the daemon from the offline screen", async () => {
    const d = daemon({ start_daemon: ONLINE }).offline();
    await renderApp({ page: "sources" });
    const start = d.hang("start_daemon");
    await click(getByRole("button", { name: t("offline.start") }));
    expect(d.callsTo("start_daemon")).toEqual([{}]);
    // Busy while starting: both buttons disabled.
    expect(getByRole("button", { name: new RegExp(t("offline.starting")) })).toHaveProperty("disabled", true);
    expect(getByRole("button", { name: t("offline.tryAgain") })).toHaveProperty("disabled", true);
    await start.resolve(ONLINE);
    expect(offlineHeading()).toBeNull();
    await findByRole("heading", { name: "Documents", level: 2 });
  });

  it("explains a daemon that didn't start", async () => {
    const d = daemon().offline().fail("start_daemon", { kind: "spawn", message: "nuggetd not found on PATH" });
    await renderApp();
    await click(getByRole("button", { name: t("offline.start") }));
    expect(d.count("start_daemon")).toBe(1);
    expect(getByRole("alert").textContent).toContain("nuggetd not found on PATH");
    expect(offlineHeading()).not.toBeNull();
  });

  it("flips to connected (and back) on connection events", async () => {
    daemon().offline();
    await renderApp({ page: "status" });
    expect(offlineHeading()).not.toBeNull();

    await emitConnection(ONLINE);
    expect(offlineHeading()).toBeNull();
    expect(pill().textContent).toContain(t("app.conn.up"));
    await findByRole("heading", { name: t("status.title"), level: 1 });

    await emitConnection(OFFLINE);
    expect(offlineHeading()).not.toBeNull();
    expect(pill().textContent).toContain(t("app.conn.down"));
  });

  it("treats a failing connection check as offline", async () => {
    daemon().fail("connection_info", errors.disconnected("socket closed"));
    await renderApp();
    expect(offlineHeading()).not.toBeNull();
  });

  it("navigates with the sidebar and remembers the page", async () => {
    const d = daemon();
    await renderApp({ page: "status" });
    expect(navButton(t("app.nav.status")).getAttribute("aria-current")).toBe("page");

    await click(navButton(t("app.nav.sources")));
    expect(navButton(t("app.nav.sources")).getAttribute("aria-current")).toBe("page");
    expect(navButton(t("app.nav.status")).getAttribute("aria-current")).toBeNull();
    expect(getByRole("main").getAttribute("aria-label")).toBe(t("app.nav.sources"));
    expect(localStorage.getItem("nugget.page")).toBe("sources");
    await waitFor(() => expect(d.commands).toContain("sources_list"));
  });

  it("follows the tray's navigate event and focuses the page", async () => {
    daemon();
    await renderApp({ page: "status" });
    await emitEvent(EVENT.navigate, "knowledge");
    expect(getByRole("main").getAttribute("aria-label")).toBe(t("app.nav.knowledge"));
    await waitFor(() => expect(document.activeElement).toBe(getByRole("main")));
  });
});
