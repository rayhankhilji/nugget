// @vitest-environment jsdom
/**
 * II2 search: facets with counts that rerun the search, the plain-search fallback, saved and
 * recent searches (page, sidebar, palette), the result preview and the timeline.
 */
import axe from "axe-core";
import { describe, expect, it } from "vitest";
import type { FacetedHit, FacetedResult } from "../../src/api";
import { SAVED_KEY } from "../../src/savedSearches";
import {
  click,
  daemon,
  findByRole,
  findByText,
  getByRole,
  main,
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

async function violations(root: Element = document.body): Promise<string[]> {
  const result = await axe.run(root, { rules: { "color-contrast": { enabled: false } } });
  return result.violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

const ada = { id: "e1", name: "Ada Lovelace", kind: "person" };
const hit = (n: number, extra: Partial<FacetedHit> = {}): FacetedHit => ({
  item: { source: "files-1", key: `notes/n${n}.md` },
  chunk_id: n,
  title: `Engine note ${n}`,
  snippet: `The engine weaves patterns ${n}`,
  score: 1 - n / 100,
  url: null,
  date: "2024-03-02T10:00:00Z",
  date_source: "extracted",
  kind: "file",
  sensitive: false,
  people: [ada],
  ...extra,
});

const faceted = (hits: FacetedHit[], total = hits.length): FacetedResult => ({
  query: "engine",
  hits,
  total,
  pool: 40,
  pool_limited: false,
  facets: {
    sources: [
      { value: "files-1", count: 3 },
      { value: "gmail-1", count: 2 },
    ],
    kinds: [
      { value: "file", count: 3 },
      { value: "mail", count: 2 },
    ],
    people: [{ value: "e1", name: "Ada Lovelace", kind: "person", count: 4 }],
    months: [
      { value: "2024-02", count: 1 },
      { value: "2024-03", count: 4 },
    ],
  },
});



const searchBox = () => getByRole("searchbox", { name: t("search.query") });
const filters = () => main().querySelector<HTMLElement>("#search-filters")!;
const req = (args: Record<string, unknown> | undefined) => (args as { request: Record<string, unknown> }).request;

async function search(q: string) {
  await type(searchBox(), q);
  await press(searchBox(), "Enter");
}

describe("faceted search", () => {
  it("shows facet counts; checking a value reruns the search with the filter; clear removes it", async () => {
    const d = daemon({ search_faceted: faceted([hit(1), hit(2, { kind: "mail", sensitive: true, item: { source: "gmail-1", key: "m2" } })], 2) });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");

    expect(req(d.last("search_faceted"))).toMatchObject({ query: "engine", limit: 25, offset: 0, filters: { sources: [], kinds: [], people: [], from: null, to: null } });
    // JJ2: the time zone goes along, so the daemon dates each day at its own offset.
    expect(req(d.last("search_faceted"))).toMatchObject({ time_zone: Intl.DateTimeFormat().resolvedOptions().timeZone });
    expect(typeof req(d.last("search_faceted")).utc_offset_minutes).toBe("number");
    expect(typeof req(d.last("search_faceted")).utc_offset_minutes).toBe("number");
    expect(d.count("search")).toBe(0);

    // Groups are labelled fieldsets of checkboxes with counts.
    const kind = within(filters()).getByRole("group", { name: t("searchui.facet.kind") });
    const mail = within(kind).getByRole("checkbox", { name: /Mail/ });
    expect(mail.closest("label")!.textContent).toContain("2");
    expect(within(filters()).getByRole("group", { name: t("searchui.facet.source") }).textContent).toContain("Work mail");
    expect(within(filters()).getByRole("group", { name: t("searchui.facet.person") }).textContent).toContain("Ada Lovelace");

    // Results carry kind, date, sensitive badge and people chips.
    const items = within(main().querySelector("ol.results")!).getAllByRole("listitem").filter((li) => li.classList.contains("result"));
    expect(items[1]!.textContent).toContain(t("searchui.kind.mail"));
    expect(items[1]!.textContent).toContain(t("search.sensitive"));
    expect(items[0]!.querySelector("time")?.getAttribute("datetime")).toBe("2024-03-02T10:00:00Z");
    expect(within(items[0]!).getByRole("button", { name: t("searchui.timelineFor", { name: "Ada Lovelace" }) })).toBeTruthy();

    await click(mail);
    await waitFor(() => expect(d.count("search_faceted")).toBe(2));
    expect(req(d.last("search_faceted"))).toMatchObject({ query: "engine", filters: { kinds: ["mail"] } });

    // A month quick pick sets the date range.
    await click(within(filters()).getByRole("button", { name: t("searchui.monthPick", { month: "March 2024", count: 4 }) }));
    await waitFor(() => expect(req(d.last("search_faceted"))).toMatchObject({ filters: { kinds: ["mail"], from: "2024-03-01", to: "2024-03-31" } }));

    expect(await violations()).toEqual([]);

    await click(within(filters()).getByRole("button", { name: t("searchui.clearFilters") }));
    await waitFor(() => expect(req(d.last("search_faceted"))).toMatchObject({ filters: { kinds: [], from: null, to: null } }));
    expect(within(filters()).queryByRole("button", { name: t("searchui.clearFilters") })).toBeNull();
  });
});

describe("saved and recent searches", () => {
  it("saves a search with a name; it shows on the page, in the sidebar and the palette, and runs", async () => {
    const d = daemon({ search_faceted: faceted([hit(1)]) });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    await click(within(filters()).getByRole("checkbox", { name: /Mail/ }));

    await click(getByRole("button", { name: t("searchui.saveSearch") }));
    const dialog = await findByRole("dialog", { name: t("searchui.saveTitle") });
    const name = within(dialog).getByRole("textbox", { name: t("searchui.saveName") });
    expect((name as HTMLInputElement).value).toBe("engine");
    await type(name, "Engine mail");
    await click(within(dialog).getByRole("button", { name: t("searchui.save") }));
    expect(queryByRole("dialog")).toBeNull();
    expect(JSON.parse(localStorage.getItem(SAVED_KEY)!)[0]).toMatchObject({ name: "Engine mail", state: "q=engine&kind=mail" });

    // On the page and in the sidebar.
    const run = t("searchui.runSaved", { name: "Engine mail" });
    expect(within(main()).getByRole("button", { name: run })).toBeTruthy();
    const sidebar = getByRole("complementary", { name: t("app.sidebarLabel") });
    const sideButton = within(sidebar).getByRole("button", { name: run });

    // Running it from the sidebar applies the query and filters.
    await type(searchBox(), "other");
    await click(sideButton);
    await waitFor(() => expect(req(d.last("search_faceted"))).toMatchObject({ query: "engine", filters: { kinds: ["mail"] } }));
    expect((searchBox() as HTMLInputElement).value).toBe("engine");

    // In the palette.
    await press(document.body, "k", { ctrl: true });
    const palette = await findByRole("dialog", { name: t("palette.title") });
    expect(palette.textContent).toContain(t("searchui.paletteSaved", { name: "Engine mail" }));
    await press(document.activeElement, "Escape");

    // Delete.
    await click(within(main()).getByRole("button", { name: t("searchui.deleteSaved", { name: "Engine mail" }) }));
    expect(within(main()).queryByRole("button", { name: run })).toBeNull();
    expect(within(sidebar).queryByRole("button", { name: run })).toBeNull();
  });
});

const liveText = () => Array.from(document.querySelectorAll('[role="status"][aria-live="polite"]')).map((e) => e.textContent?.trim() ?? "");
const said = (text: string) => liveText().some((l) => l.includes(text));

describe("review fixes", () => {
  it("labels only confirmed sensitive results; unsorted ones stay private without the warning (JJ2)", async () => {
    daemon({
      search_faceted: faceted([hit(1, { sensitive: true }), hit(2, { sensitive: true, unsorted: true }), hit(3)]),
    });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    const items = [...main().querySelectorAll("ol.results > li")];
    const badge = (li: Element) => [...li.querySelectorAll(".badge-warn")].map((b) => b.textContent);
    expect(badge(items[0]!)).toEqual([t("search.sensitive")]);
    expect(badge(items[1]!)).toEqual([]);
    expect(badge(items[2]!)).toEqual([]);
  });

  it("renames a saved search, refusing a name another one has (JJ2)", async () => {
    daemon({ search_faceted: faceted([hit(1)]) });
    localStorage.setItem(SAVED_KEY, JSON.stringify([{ id: "a", name: "A", state: "q=a" }, { id: "b", name: "B", state: "q=b" }]));
    await renderApp({ page: "search" });
    const opener = within(main()).getByRole("button", { name: t("searchui.renameSaved", { name: "A" }) });
    await click(opener);
    const dialog = await findByRole("dialog", { name: t("searchui.renameTitle") });
    const input = within(dialog).getByRole("textbox", { name: t("searchui.saveName") }) as HTMLInputElement;
    expect(input.value).toBe("A");
    await type(input, "b");
    expect(within(dialog).getByRole("alert").textContent).toBe(t("searchui.nameTaken", { name: "b" }));
    expect(input.getAttribute("aria-invalid")).toBe("true");
    expect((within(dialog).getByRole("button", { name: t("searchui.rename") }) as HTMLButtonElement).disabled).toBe(true);
    expect(await violations(dialog)).toEqual([]);

    await type(input, "Engine mail");
    await press(input, "Enter");
    await waitFor(() => expect(queryByRole("dialog")).toBeNull());
    expect(within(main()).getByRole("button", { name: t("searchui.runSaved", { name: "Engine mail" }) })).toBeTruthy();
    expect(JSON.parse(localStorage.getItem(SAVED_KEY)!)).toEqual([
      { id: "a", name: "Engine mail", state: "q=a" },
      { id: "b", name: "B", state: "q=b" },
    ]);
    expect(said(t("searchui.renamed", { name: "Engine mail" }))).toBe(true);
  });
});
