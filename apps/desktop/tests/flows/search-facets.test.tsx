// @vitest-environment jsdom
/**
 * II2 search: facets with counts that rerun the search, the plain-search fallback, saved and
 * recent searches (page, sidebar, palette), the result preview and the timeline.
 */
import axe from "axe-core";
import { describe, expect, it } from "vitest";
import type { FacetedHit, FacetedResult, ItemPreview, Timeline } from "../../src/api";
import { entityDetail, openableHits } from "../../src/test/fixtures";
import { RECENT_KEY, SAVED_KEY } from "../../src/savedSearches";
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
  selectOption,
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

const preview: ItemPreview = {
  item: { source: "files-1", key: "notes/n1.md" },
  title: "Engine note 1",
  kind: "file",
  mime: "text/markdown",
  url: "file:///home/me/notes/n1.md",
  date: "2024-03-02T10:00:00Z",
  date_source: "extracted",
  sensitive: true,
  text: "Full text about the engine and its cards.",
  truncated: false,
  passage: "the engine weaves",
  people: [ada],
  facts: [{ id: 5, subject: ada, predicate: "works_on", text: "Ada works on the engine", status: "active", confidence: 0.9, quote: "Ada at the engine" }],
};

const timeline: Timeline = {
  entity: ada,
  events: [
    { type: "item", at: "2024-01-10T10:00:00Z", date_source: "extracted", item: { source: "files-1", key: "a.md" }, title: "January note", snippet: "jan", kind: "file", url: null, sensitive: false },
    {
      type: "fact",
      at: "2024-01-20T10:00:00Z",
      date_source: "valid_from",
      claim: { id: 5, predicate: "works_on", text: "Ada works on the engine", status: "active", confidence: 0.9, evidence: [{ item: { source: "files-1", key: "a.md" }, title: "January note", quote: "Ada at the engine" }] },
    },
    { type: "item", at: "2024-03-05T10:00:00Z", date_source: "extracted", item: { source: "gmail-1", key: "m1" }, title: "March mail", snippet: "mar", kind: "mail", url: null, sensitive: true },
  ],
  months: [
    { value: "2024-01", count: 2 },
    { value: "2024-03", count: 1 },
  ],
  total: 3,
  truncated: false,
};

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

  it("pages with Show more using the offset", async () => {
    const d = daemon();
    d.once("search_faceted", faceted([hit(1), hit(2)], 3)).once("search_faceted", faceted([hit(3)], 3));
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    await click(getByRole("button", { name: t("searchui.showMore", { shown: 2, total: 3 }) }));
    await findByText("Engine note 3");
    expect(req(d.last("search_faceted"))).toMatchObject({ offset: 2 });
    expect(queryByRole("button", { name: /Show more/ })).toBeNull();
  });

  it("falls back to plain search, without filters, on an older daemon", async () => {
    const d = daemon({ search: openableHits });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("The Analytical Engine weaves algebraic patterns");
    expect(d.count("search_faceted")).toBe(1);
    expect(d.callsTo("search")).toEqual([{ query: "engine", limit: 25 }]);
    expect(main().querySelector("#search-filters")).toBeNull();
    // The next search goes straight to plain search.
    await search("ada");
    await waitFor(() => expect(d.count("search")).toBe(2));
    expect(d.count("search_faceted")).toBe(1);
  });
});

describe("plain search paging (LL1)", () => {
  /** A plain `search` hit (no date, kind or people). */
  const plainHit = (n: number) => {
    const { item, chunk_id, title, snippet, score, url } = hit(n);
    return { item, chunk_id, title, snippet, score, url };
  };

  it("pages by cursor with search.page when the daemon has no faceted search", async () => {
    const d = daemon({
      search_page: (args: Record<string, unknown>) => {
        const r = req(args) as { cursor?: string };
        if (!r.cursor) return { items: [plainHit(1), plainHit(2)], next_cursor: "s1.2.tag" };
        // The ranking shifted between pages and repeats hit 2: it is listed once.
        return { items: [plainHit(2), plainHit(3)], next_cursor: null };
      },
    });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    expect(req(d.last("search_page"))).toEqual({ query: "engine", limit: 25 });
    await click(getByRole("button", { name: t("searchui.showMorePlain", { shown: 2 }) }));
    await findByText("Engine note 3");
    expect(req(d.last("search_page"))).toEqual({ query: "engine", limit: 25, cursor: "s1.2.tag" });
    expect(main().querySelectorAll(".result-select")).toHaveLength(3);
    // The end: no more button; focus is on the first new result and the count is announced.
    expect(queryByRole("button", { name: /Show more/ })).toBeNull();
    expect(document.activeElement?.textContent).toContain("Engine note 3");
    const said = () => Array.from(document.querySelectorAll('[role="status"]'), (e) => e.textContent ?? "");
    await waitFor(() => expect(said().some((s) => s.includes(t("searchui.loadedMore", { n: 3 })))).toBe(true));
    expect(d.count("search")).toBe(0);
    expect(await violations()).toEqual([]);
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

  it("a saved search from the palette opens Search from another page", async () => {
    localStorage.setItem(SAVED_KEY, JSON.stringify([{ id: "s1", name: "Ada notes", state: "q=ada&person=e1" }]));
    const d = daemon({ search_faceted: faceted([hit(1)]) });
    await renderApp({ page: "status" });
    await press(document.body, "k", { ctrl: true });
    const palette = await findByRole("dialog", { name: t("palette.title") });
    await type(within(palette).getByRole("combobox"), "Ada notes");
    await press(document.activeElement, "Enter");
    await waitFor(() => expect(req(d.last("search_faceted"))).toMatchObject({ query: "ada", filters: { people: ["e1"] } }));
    expect(main().getAttribute("aria-label")).toBe(t("app.nav.search"));
  });

  it("shows recent searches when the box is empty, runs one, and clears them", async () => {
    const d = daemon({ search_faceted: faceted([hit(1)]) });
    await renderApp({ page: "search" });
    expect(queryByRole("heading", { name: t("searchui.recent") })).toBeNull();
    await search("engine");
    await findByText("Engine note 1");
    await type(searchBox(), "");
    const recent = await findByRole("region", { name: t("searchui.recent") });
    await click(within(recent).getByRole("button", { name: t("searchui.runRecent", { query: "engine" }) }));
    await waitFor(() => expect(d.count("search_faceted")).toBe(2));
    await type(searchBox(), "");
    await click(within(getByRole("region", { name: t("searchui.recent") })).getByRole("button", { name: t("searchui.clearRecent") }));
    expect(queryByRole("region", { name: t("searchui.recent") })).toBeNull();
    expect(localStorage.getItem(RECENT_KEY)).toBeNull();
  });
});

describe("result preview", () => {
  it("opens from a result, shows text with matches, facts and people, and Escape returns focus", async () => {
    const d = daemon({ search_faceted: faceted([hit(1), hit(2)]), search_preview: preview, open_url: "opened" });
    await renderApp({ page: "search" });
    await search("engine");
    const title = (await findByText("Engine note 1")).closest("button")!;
    await click(title);
    const panel = await findByRole("region", { name: "Engine note 1" });
    expect(req(d.last("search_preview"))).toMatchObject({ item: { source: "files-1", key: "notes/n1.md" }, chunk_id: 1 });
    await waitFor(() => expect(panel.textContent).toContain("Full text about the engine"));
    expect(panel.querySelectorAll("mark").length).toBeGreaterThan(0);
    expect(panel.textContent).toContain("Ada works on the engine");
    expect(panel.textContent).toContain(t("search.sensitive"));
    expect(document.activeElement?.id).toBe("search-preview-title");
    expect(title.getAttribute("aria-expanded")).toBe("true");
    expect(await violations()).toEqual([]);

    await click(within(panel).getByRole("button", { name: t("search.openTitle", { title: "Engine note 1" }) }));
    expect(d.callsTo("open_url")).toEqual([{ url: "file:///home/me/notes/n1.md" }]);

    await press(within(panel).getByRole("button", { name: t("searchui.closePreview") }), "Escape");
    expect(queryByRole("region", { name: "Engine note 1" })).toBeNull();
    expect(document.activeElement).toBe(title);

    // Arrow keys move between results.
    await press(title, "ArrowDown");
    expect(document.activeElement?.textContent).toBe("Engine note 2");
  });
});

describe("timeline", () => {
  it("opens from a person chip, groups by month, jumps to a month and goes back", async () => {
    const d = daemon({ search_faceted: faceted([hit(1)]), search_timeline: timeline });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    await click(within(main().querySelector("ol.results")!).getByRole("button", { name: t("searchui.timelineFor", { name: "Ada Lovelace" }) }));

    const heading = await findByRole("heading", { name: t("searchui.timelineTitle", { name: "Ada Lovelace" }) });
    expect(req(d.last("search_timeline"))).toMatchObject({ entity: "e1" });
    await waitFor(() => expect(document.activeElement).toBe(heading));
    const months = main().querySelectorAll(".sr-tl-month");
    expect(months).toHaveLength(2);
    expect(months[0]!.querySelector("h3")!.textContent).toBe("January 2024");
    expect(months[0]!.querySelectorAll("li.sr-tl-item")).toHaveLength(1);
    expect(months[0]!.querySelectorAll("li.sr-tl-fact")).toHaveLength(1);
    expect(months[1]!.textContent).toContain(t("search.sensitive"));
    expect(await violations()).toEqual([]);

    const jump = getByRole("combobox", { name: t("searchui.jumpToMonth") });
    await selectOption(jump, "2024-03");
    expect(document.activeElement?.id).toBe("timeline-month-2024-03");

    await click(getByRole("button", { name: t("searchui.backToResults") }));
    await findByText("Engine note 1");
    expect(document.activeElement?.id).toBe("search-results-heading");
  });

  it("refetches a window ending with a month outside a truncated timeline", async () => {
    const d = daemon();
    d.once("search_timeline", { ...timeline, events: timeline.events.slice(2), truncated: true, total: 3 }).once("search_timeline", { ...timeline, events: timeline.events.slice(0, 2), truncated: false, months: [{ value: "2024-01", count: 2 }], total: 2 });
    localStorage.setItem(SAVED_KEY, JSON.stringify([{ id: "s1", name: "Ada timeline", state: "timeline=e1" }]));
    await renderApp({ page: "search" });
    await click(within(main()).getByRole("button", { name: t("searchui.runSaved", { name: "Ada timeline" }) }));
    await findByText(t("searchui.timelineTruncated", { shown: 1, total: 3 }));
    await selectOption(getByRole("combobox", { name: t("searchui.jumpToMonth") }), "2024-01");
    await waitFor(() => expect(req(d.last("search_timeline"))).toMatchObject({ entity: "e1", to: "2024-01-31" }));
    await waitFor(() => expect(document.activeElement?.id).toBe("timeline-month-2024-01"));
    // The jump list still offers every month.
    expect(getByRole("combobox", { name: t("searchui.jumpToMonth") }).querySelectorAll("option")).toHaveLength(3);
  });

  it("opens from the entity page's Timeline button", async () => {
    const d = daemon({ search_timeline: timeline, knowledge_entity: entityDetail });
    await renderApp({ page: "knowledge" });
    await click(await findByRole("button", { name: /^Ada Lovelace/ }));
    const button = await findByRole("button", { name: t("searchui.timelineFor", { name: "Ada Lovelace" }) });
    await click(button);
    await findByRole("heading", { name: t("searchui.timelineTitle", { name: "Ada Lovelace" }) });
    expect(req(d.last("search_timeline"))).toMatchObject({ entity: "e1" });
    expect(main().getAttribute("aria-label")).toBe(t("app.nav.search"));
  });
});

/** Set an input's value the way a browser does (React sees it) and fire input. */
async function setValue(el: HTMLInputElement, value: string) {
  const { act } = await import("react");
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

const liveText = () => Array.from(document.querySelectorAll('[role="status"][aria-live="polite"]')).map((e) => e.textContent?.trim() ?? "");
const said = (text: string) => liveText().some((l) => l.includes(text));

describe("review fixes", () => {
  it("two quick toggles both reach the final request, and a late older answer doesn't win", async () => {
    const d = daemon();
    d.once("search_faceted", faceted([hit(1)]));
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");

    const first = d.hang("search_faceted");
    const second = d.hang("search_faceted");
    await click(within(filters()).getByRole("checkbox", { name: /^Mail/ }));
    // The box stays ticked while its search runs.
    expect((within(filters()).getByRole("checkbox", { name: /^Mail/ }) as HTMLInputElement).checked).toBe(true);
    await click(within(filters()).getByRole("checkbox", { name: /^Work mail/ }));
    expect(req(d.last("search_faceted"))).toMatchObject({ filters: { kinds: ["mail"], sources: ["gmail-1"] } });

    await second.resolve(faceted([hit(7)]));
    await findByText("Engine note 7");
    await first.resolve(faceted([hit(8)]));
    expect(main().textContent).not.toContain("Engine note 8");
    expect((within(filters()).getByRole("checkbox", { name: /^Mail/ }) as HTMLInputElement).checked).toBe(true);
    expect((within(filters()).getByRole("checkbox", { name: /^Work mail/ }) as HTMLInputElement).checked).toBe(true);
  });

  it("Retry after a failed filter change reruns the new filters", async () => {
    const d = daemon({ search_faceted: faceted([hit(1)]) });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    d.failOnce("search_faceted", { kind: "rpc", message: "index is locked", code: -32000 });
    await click(within(filters()).getByRole("checkbox", { name: /^Mail/ }));
    const alert = await findByRole("alert");
    await click(within(alert).getByRole("button", { name: t("common.retry") }));
    await waitFor(() => expect(req(d.last("search_faceted"))).toMatchObject({ filters: { kinds: ["mail"] } }));
    expect(d.count("search_faceted")).toBe(3);
  });

  it("names facet checkboxes with their count", async () => {
    daemon({ search_faceted: faceted([hit(1)]) });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    expect(within(filters()).getByRole("checkbox", { name: "Mail, 2 results" })).toBeTruthy();
    expect(within(filters()).getByRole("checkbox", { name: "Ada Lovelace, 4 results" })).toBeTruthy();
    expect(within(filters()).getByRole("group", { name: "People and projects" })).toBeTruthy();
  });

  it("a timeline-only request keeps the search underneath; Back returns to it intact", async () => {
    const { requestSearchState } = await import("../../src/hooks.nav");
    const { encode } = await import("../../src/searchState");
    const d = daemon({ search_faceted: faceted([hit(1)]), search_timeline: timeline });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    await click(within(filters()).getByRole("checkbox", { name: /^Mail/ }));
    const before = d.count("search_faceted");
    await (await import("react")).act(async () => requestSearchState(encode({ timeline: "e1" })));
    await findByRole("heading", { name: t("searchui.timelineTitle", { name: "Ada Lovelace" }) });
    await click(getByRole("button", { name: t("searchui.backToResults") }));
    await findByText("Engine note 1");
    expect((searchBox() as HTMLInputElement).value).toBe("engine");
    expect((within(filters()).getByRole("checkbox", { name: /^Mail/ }) as HTMLInputElement).checked).toBe(true);
    // Same search: no rerun needed.
    expect(d.count("search_faceted")).toBe(before);
    await click(getByRole("button", { name: t("searchui.saveSearch") }));
    const dialog = await findByRole("dialog", { name: t("searchui.saveTitle") });
    expect((within(dialog).getByRole("textbox", { name: t("searchui.saveName") }) as HTMLInputElement).value).toBe("engine");
  });

  it("applies typed dates only when complete, debounced, and swaps a reversed range", async () => {
    const d = daemon({ search_faceted: faceted([hit(1)]) });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    const date = within(filters()).getByRole("group", { name: t("searchui.facet.date") });
    const [from, to] = Array.from(date.querySelectorAll<HTMLInputElement>('input[type="date"]'));
    const n = d.count("search_faceted");
    await setValue(from!, "0002-01-01");
    await new Promise((r) => setTimeout(r, 500));
    expect(d.count("search_faceted")).toBe(n);
    await setValue(from!, "2024-06-01");
    expect(d.count("search_faceted")).toBe(n);
    await waitFor(() => expect(d.count("search_faceted")).toBe(n + 1));
    expect(req(d.last("search_faceted"))).toMatchObject({ filters: { from: "2024-06-01", to: null } });
    await setValue(to!, "2024-01-31");
    await press(to!, "Enter");
    await waitFor(() => expect(req(d.last("search_faceted"))).toMatchObject({ filters: { from: "2024-01-31", to: "2024-06-01" } }));
  });

  it("keeps focus and announces when Clear filters, Clear dates, Delete and Clear recent remove themselves", async () => {
    const d = daemon({ search_faceted: faceted([hit(1)]) });
    localStorage.setItem(SAVED_KEY, JSON.stringify([{ id: "a", name: "A", state: "q=a" }, { id: "b", name: "B", state: "q=b" }]));
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    await click(within(filters()).getByRole("button", { name: t("searchui.monthPick", { month: "March 2024", count: 4 }) }));
    await click(within(filters()).getByRole("button", { name: t("searchui.clearDates") }));
    expect(document.activeElement?.textContent).toBe(t("searchui.filters"));
    expect(said(t("searchui.datesCleared"))).toBe(true);
    await click(within(filters()).getByRole("checkbox", { name: /^Mail/ }));
    await click(within(filters()).getByRole("button", { name: t("searchui.clearFilters") }));
    expect(document.activeElement?.textContent).toBe(t("searchui.filters"));
    expect(said(t("searchui.filtersCleared"))).toBe(true);
    expect(d.count("search_faceted")).toBeGreaterThan(1);

    await click(within(main()).getByRole("button", { name: t("searchui.deleteSaved", { name: "A" }) }));
    expect(document.activeElement?.getAttribute("aria-label")).toBe(t("searchui.runSaved", { name: "B" }));
    expect(said(t("searchui.savedDeleted", { name: "A" }))).toBe(true);
    await click(within(main()).getByRole("button", { name: t("searchui.deleteSaved", { name: "B" }) }));
    expect(document.activeElement).toBe(searchBox());

    await type(searchBox(), "");
    await click(within(getByRole("region", { name: t("searchui.recent") })).getByRole("button", { name: t("searchui.clearRecent") }));
    expect(document.activeElement).toBe(searchBox());
    expect(said(t("searchui.recentCleared"))).toBe(true);
  });

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

  it("records recents only for explicit runs and labels filtered ones", async () => {
    daemon({ search_faceted: faceted([hit(1)]) });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    await click(within(filters()).getByRole("checkbox", { name: /^Mail/ }));
    expect(JSON.parse(localStorage.getItem(RECENT_KEY)!)).toEqual(["q=engine"]);
    await press(searchBox(), "Enter");
    await waitFor(() => expect(JSON.parse(localStorage.getItem(RECENT_KEY)!)).toEqual(["q=engine&kind=mail", "q=engine"]));
    await type(searchBox(), "");
    const recent = getByRole("region", { name: t("searchui.recent") });
    expect(within(recent).getByRole("button", { name: t("searchui.runRecent", { query: t("searchui.recentWithFilters", { query: "engine", n: 1 }) }) })).toBeTruthy();
    expect(within(recent).getByRole("button", { name: t("searchui.runRecent", { query: "engine" }) })).toBeTruthy();
  });

  it("notes a limited pool and announces each finished search with its filters", async () => {
    daemon({ search_faceted: { ...faceted([hit(1)]), pool_limited: true, pool: 300 } });
    await renderApp({ page: "search" });
    await search("engine");
    await findByText("Engine note 1");
    expect(filters().textContent).toContain(t("searchui.poolLimited", { pool: 300 }));
    expect(liveText()).toContain(t("search.resultsFor", { n: 1, query: "engine" }));
    await click(within(filters()).getByRole("checkbox", { name: /^Mail/ }));
    await waitFor(() => expect(liveText()).toContain(t("searchui.resultsFiltered", { n: 1, query: "engine", filters: 1 })));
  });

  it("resets Jump to month after Show the latest events", async () => {
    const d = daemon();
    d.once("search_timeline", { ...timeline, events: timeline.events.slice(2), truncated: true, total: 3 }).on("search_timeline", { ...timeline, events: timeline.events.slice(0, 2), months: [{ value: "2024-01", count: 2 }], total: 2 });
    localStorage.setItem(SAVED_KEY, JSON.stringify([{ id: "s1", name: "Ada timeline", state: "timeline=e1" }]));
    await renderApp({ page: "search" });
    await click(within(main()).getByRole("button", { name: t("searchui.runSaved", { name: "Ada timeline" }) }));
    const jump = () => getByRole("combobox", { name: t("searchui.jumpToMonth") }) as HTMLSelectElement;
    await findByText(t("searchui.timelineTruncated", { shown: 1, total: 3 }));
    await selectOption(jump(), "2024-01");
    await click(await findByRole("button", { name: t("searchui.timelineLatest") }));
    expect(jump().value).toBe("");
  });
});
