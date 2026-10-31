// @vitest-environment jsdom
/** Search: query → results with sources → open; empty results; daemon error and retry. */
import { describe, expect, it } from "vitest";
import { openableHits } from "../../src/test/fixtures";
import {
  click,
  daemon,
  errors,
  findByText,
  getByRole,
  main,
  press,
  queryByText,
  renderApp,
  selectOption,
  setupHarness,
  t,
  type,
  waitFor,
  within,
} from "../harness";

setupHarness();

const searchBox = () => getByRole("searchbox", { name: t("search.query") });
const submitButton = () => within(getByRole("search")).getByRole("button", { name: t("search.submit") });

describe("search flow", () => {
  it("types a query, submits it, shows results with their sources and opens one", async () => {
    const d = daemon({ search: openableHits, open_url: "opened" });
    await renderApp({ page: "search" });

    expect(submitButton()).toHaveProperty("disabled", true);
    await type(searchBox(), "  engine notes ");
    expect(submitButton()).toHaveProperty("disabled", false);
    await press(searchBox(), "Enter");

    // The query is trimmed; the limit is the default from the picker.
    await waitFor(() => expect(d.callsTo("search")).toEqual([{ query: "engine notes", limit: 25 }]));
    const results = await findByText("The Analytical Engine weaves algebraic patterns");
    const list = results.closest("ol")!;
    const items = within(list).getAllByRole("listitem");
    expect(items).toHaveLength(2);
    // Results carry their source's name (from sources_list), not just its id.
    expect(items[0]!.textContent).toContain("Engine notes");
    expect(items[0]!.textContent).toContain("Documents");
    expect(items[1]!.textContent).toContain("Work mail");
    // An opaque mail id is not a file name: it stays out of the badge.
    expect(items[1]!.textContent).not.toContain("msg-42");

    await click(within(items[1]!).getByRole("button", { name: t("search.openTitle", { title: "Re: Bernoulli numbers" }) }));
    expect(d.callsTo("open_url")).toEqual([{ url: "https://mail.google.com/mail/u/0/#inbox/msg-42" }]);
    await findByText(t("hooks.open.opened", { label: "Re: Bernoulli numbers" }));
  });

  it("submits with the button and honours the chosen limit", async () => {
    const d = daemon({ search: openableHits });
    await renderApp({ page: "search" });
    const limit = within(getByRole("search")).getByRole("combobox");
    await selectOption(limit, "10");
    await type(searchBox(), "ada");
    await click(submitButton());
    await waitFor(() => expect(d.last("search")).toEqual({ query: "ada", limit: 10 }));
  });

  it("does not offer Open for results without a safe URL", async () => {
    daemon({
      search: [
        { item: { source: "files-1", key: "x.md" }, title: "Plain", snippet: "no url here", score: 0.4 },
        { item: { source: "files-1", key: "y.md" }, title: "Script", snippet: "bad url", score: 0.3, url: "javascript:alert(1)" },
      ],
    });
    await renderApp({ page: "search" });
    await type(searchBox(), "x");
    await press(searchBox(), "Enter");
    await findByText("no url here");
    expect(within(main()).queryAllByRole("button", { name: /^Open / })).toEqual([]);
  });

  it("says when nothing matched", async () => {
    const d = daemon({ search: [] });
    await renderApp({ page: "search" });
    await type(searchBox(), "zebra");
    await press(searchBox(), "Enter");
    await findByText(t("search.noResults", { query: "zebra" }));
    expect(d.count("search")).toBe(1);
    expect(main().querySelector("ol")).toBeNull();
  });

  it("shows a daemon error and retries the same query", async () => {
    const d = daemon({ search: openableHits }).failOnce("search", errors.rpc("index is locked"));
    await renderApp({ page: "search" });
    await type(searchBox(), "engine");
    await press(searchBox(), "Enter");

    const alert = await waitFor(() => getByRole("alert"));
    expect(alert.textContent).toContain("index is locked");
    expect(queryByText("Engine notes")).toBeNull();

    await click(within(alert).getByRole("button", { name: t("common.retry") }));
    await findByText("Engine notes");
    expect(d.callsTo("search")).toEqual([
      { query: "engine", limit: 25 },
      { query: "engine", limit: 25 },
    ]);
    expect(within(main()).queryAllByRole("alert")).toEqual([]);
  });

  it("keeps the old results while a slow search runs, then replaces them", async () => {
    const d = daemon({ search: openableHits });
    await renderApp({ page: "search" });
    await type(searchBox(), "engine");
    await press(searchBox(), "Enter");
    await findByText("Engine notes");

    const slow = d.hang("search");
    await type(searchBox(), "bernoulli");
    await press(searchBox(), "Enter");
    expect(slow.started).toBe(true);
    // While it runs: a busy, disabled submit button; the previous results stay visible.
    expect(within(getByRole("search")).getByRole("button", { name: t("search.searching") })).toHaveProperty("disabled", true);
    expect(queryByText("Engine notes")).not.toBeNull();
    await slow.resolve([{ item: { source: "files-1", key: "b.md" }, title: "Bernoulli", snippet: "numbers", score: 0.7 }]);
    await findByText("Bernoulli");
    expect(queryByText("Engine notes")).toBeNull();
    expect(d.last("search")).toEqual({ query: "bernoulli", limit: 25 });
  });
});
