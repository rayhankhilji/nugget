// @vitest-environment jsdom
/**
 * Page states: every page owned by the shell quality pass shows a distinct loading state on first
 * load, an error with a working Retry, and an empty state with a next step, and each of those is
 * clean under axe (colour contrast is checked by eye; jsdom has no layout).
 */
import axe from "axe-core";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, click, flush, installDomShims, mockDaemon, render } from "../src/test/render";
import { RESPONSES } from "../src/test/fixtures";
import { requestSearch } from "../src/hooks.nav";
import { SearchPage } from "../src/pages/Search";
import { StatusPage } from "../src/pages/Status";
import { ModelsPage } from "../src/pages/Models";
import { PrivacyPage } from "../src/pages/Privacy";
import { AppsPage } from "../src/pages/Apps";
import { loadLocale } from "../src/i18n";

async function violations(root: Element = document.body): Promise<string[]> {
  const result = await axe.run(root, { rules: { "color-contrast": { enabled: false } } });
  return result.violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

/** Pages render inside the shell's <main>; give them one so landmark rules apply as in the app. */
function inMain(ui: ReactNode) {
  return <main>{ui}</main>;
}

/** A daemon answer that stays pending until the test releases it. */
function deferred<T>(value: T) {
  let release: () => void = () => {};
  const promise = new Promise<T>((resolve) => {
    release = () => resolve(value);
  });
  return { promise, release };
}

const FAIL = { kind: "rpc", message: "database is locked", code: -32000 };

/** Fails the first call, then answers with `value`. */
function failOnce(value: unknown) {
  let calls = 0;
  return () => {
    calls += 1;
    if (calls === 1) throw FAIL;
    return value;
  };
}

function text(): string {
  return document.body.textContent ?? "";
}

function statusLabels(): string[] {
  return Array.from(document.querySelectorAll('[role="status"]')).map((e) => e.textContent ?? "");
}

function retryButton(): HTMLButtonElement | undefined {
  return Array.from(document.querySelectorAll<HTMLButtonElement>('[role="alert"] button')).find((b) => b.textContent === "Retry");
}

interface Case {
  name: string;
  ui: () => ReactNode;
  /** The command whose answer the page waits on for its first load. */
  command: string;
  loading: string;
  loaded: string;
  empty: { title: string; responses: Record<string, unknown> };
}

const CASES: Case[] = [
  {
    name: "status",
    ui: () => <StatusPage />,
    command: "status",
    loading: "Loading status",
    loaded: "Uptime",
    empty: { title: "No AI providers yet", responses: { status: { ...(RESPONSES.status as object), providers: [] } } },
  },
  {
    name: "models",
    ui: () => <ModelsPage />,
    command: "models_list",
    loading: "Loading models",
    loaded: "qwen3-4b-q4",
    empty: { title: "No models available", responses: { models_list: [] } },
  },
  {
    name: "privacy",
    ui: () => <PrivacyPage />,
    command: "egress_recent",
    loading: "Loading the network ledger",
    loaded: "api.anthropic.com",
    empty: { title: "Nothing has left this device", responses: { egress_recent: [] } },
  },
  {
    name: "apps",
    ui: () => <AppsPage />,
    command: "mcp_clients_list",
    loading: "Loading connected apps",
    loaded: "Revoke",
    empty: { title: "No apps connected", responses: { mcp_clients_list: [] } },
  },
];

beforeEach(() => {
  installDomShims();
});
afterEach(async () => {
  cleanup();
  await loadLocale("en");
});

describe.each(CASES)("$name page", (c) => {
  it("shows a labelled loading state before data, then the data", async () => {
    const d = deferred(RESPONSES[c.command]);
    mockDaemon({ [c.command]: () => d.promise });
    await render(inMain(c.ui()));
    expect(statusLabels().some((s) => s.includes(c.loading))).toBe(true);
    expect(text()).not.toContain(c.loaded);
    expect(await violations()).toEqual([]);
    d.release();
    await flush();
    expect(text()).toContain(c.loaded);
    expect(statusLabels().some((s) => s.includes(c.loading))).toBe(false);
    expect(await violations()).toEqual([]);
  });

  it("shows an error with Retry, and Retry reloads", async () => {
    const calls = mockDaemon({ [c.command]: failOnce(RESPONSES[c.command]) });
    await render(inMain(c.ui()));
    expect(text()).toContain("database is locked");
    expect(text()).not.toContain(c.loaded);
    expect(await violations()).toEqual([]);
    const before = calls.filter((x) => x === c.command).length;
    await click(retryButton());
    await flush();
    expect(calls.filter((x) => x === c.command).length).toBe(before + 1);
    expect(text()).toContain(c.loaded);
    expect(text()).not.toContain("database is locked");
  });

  it("shows an empty state with a next step", async () => {
    mockDaemon(c.empty.responses);
    await render(inMain(c.ui()));
    expect(document.querySelector(".pg-empty-title")?.textContent).toBe(c.empty.title);
    expect(document.querySelector(".pg-empty-body")?.textContent).toBeTruthy();
    expect(await violations()).toEqual([]);
  });
});

describe("models page", () => {
  it("says when nothing is installed yet", async () => {
    const models = (RESPONSES.models_list as { installed: boolean }[]).map((m) => ({ ...m, installed: false }));
    mockDaemon({ models_list: models });
    await render(inMain(<ModelsPage />));
    expect(text()).toContain("No models installed yet");
    expect(text()).toContain("qwen3-4b-q4");
    expect(await violations()).toEqual([]);
  });
});

describe("apps page", () => {
  it("offers Connect from the empty state", async () => {
    mockDaemon({ mcp_clients_list: [] });
    await render(inMain(<AppsPage />));
    const action = document.querySelector(".pg-empty-action button");
    expect(action?.textContent).toBe("Connect an app");
  });
});

describe("search page", () => {
  it("before the first query, invites one", async () => {
    mockDaemon();
    await render(inMain(<SearchPage />));
    expect(document.querySelector(".pg-empty-title")?.textContent).toBe("Type a query to search");
    expect(document.querySelectorAll("h1")).toHaveLength(1);
    expect(await violations()).toEqual([]);
  });

  it("with nothing indexed, points to Sources", async () => {
    mockDaemon({ sources_list: [] });
    await render(inMain(<SearchPage />));
    expect(document.querySelector(".pg-empty-title")?.textContent).toBe("Nothing to search yet");
    expect(text()).toContain("Sources page");
    expect(await violations()).toEqual([]);
  });

  it("runs a search requested by the command palette, with a loading state and announced results", async () => {
    const d = deferred(RESPONSES.search);
    const calls = mockDaemon({ search: () => d.promise });
    requestSearch("Ada");
    await render(inMain(<SearchPage />));
    expect(calls).toContain("search");
    expect(document.querySelector<HTMLInputElement>("#search-q")?.value).toBe("Ada");
    expect(statusLabels().some((s) => s.includes("Searching"))).toBe(true);
    expect(await violations()).toEqual([]);
    d.release();
    await flush();
    expect(text()).toContain("Ada wrote the first program");
    const live = Array.from(document.querySelectorAll('[aria-live="polite"]')).map((e) => e.textContent);
    expect(live).toContain("1 result for Ada");
    expect(document.querySelector("ol.results")?.getAttribute("aria-labelledby")).toBe("search-results-heading");
    expect(await violations()).toEqual([]);
  });

  it("runs a search requested while the page is open", async () => {
    const calls = mockDaemon();
    await render(inMain(<SearchPage />));
    expect(calls).not.toContain("search");
    requestSearch("Notes");
    await flush();
    expect(document.querySelector<HTMLInputElement>("#search-q")?.value).toBe("Notes");
    expect(text()).toContain("Ada wrote the first program");
  });

  it("shows no results with a hint", async () => {
    mockDaemon({ search: [] });
    requestSearch("zebra");
    await render(inMain(<SearchPage />));
    expect(document.querySelector(".pg-empty-title")?.textContent).toBe("No results for “zebra”");
    expect(await violations()).toEqual([]);
  });

  it("shows an error with Retry, and Retry runs the search again", async () => {
    const calls = mockDaemon({ search: failOnce(RESPONSES.search) });
    requestSearch("Ada");
    await render(inMain(<SearchPage />));
    expect(text()).toContain("database is locked");
    expect(await violations()).toEqual([]);
    await click(retryButton());
    await flush();
    expect(calls.filter((x) => x === "search")).toHaveLength(2);
    expect(text()).toContain("Ada wrote the first program");
  });
});
