// @vitest-environment jsdom
/**
 * Guidance that names another page ("Add a source on the Sources page") comes with a
 * "Go to <Page>" button that actually takes you there.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { PageId } from "../api";
import { App } from "../App";
import { GoToPage } from "../components/GoToPage";
import { usePendingPage } from "../hooks.nav";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";

beforeEach(() => {
  installDomShims();
  if (!Element.prototype.scrollIntoView) Element.prototype.scrollIntoView = () => {};
});
afterEach(cleanup);

const DONE = JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true });

async function openApp(page: PageId, responses: Record<string, unknown>) {
  mockDaemon(responses);
  localStorage.setItem("nugget.onboarding.v1", DONE);
  localStorage.setItem("nugget.page", page);
  await render(<App />);
  await flush(10);
  return document.querySelector("main")!;
}

const current = () => document.querySelector("nav [aria-current=page]")?.textContent ?? "";

describe("GoToPage", () => {
  it("reaches a usePendingPage listener", async () => {
    const seen: PageId[] = [];
    function Listener() {
      usePendingPage((id) => seen.push(id));
      return null;
    }
    const { container } = await render(
      <>
        <Listener />
        <GoToPage page="models" />
        <GoToPage page="sources" />
      </>,
    );
    await click(byText(container, "button", "Go to Models"));
    await click(byText(container, "button", "Go to Sources"));
    expect(seen).toEqual(["models", "sources"]);
  });
});

describe("page guidance navigates", () => {
  it("Search with no sources: Go to Sources opens the Sources page", async () => {
    const main = await openApp("search", { sources_list: [] });
    expect(main.textContent).toContain("Add a source on the Sources page");
    await click(byText(main, "button", "Go to Sources"));
    await flush(5);
    expect(localStorage.getItem("nugget.page")).toBe("sources");
    expect(current()).toContain("Sources");
    expect(document.querySelector("main")?.textContent).not.toContain("Add a source on the Sources page");
  });

  it("Chat with no models: Go to Models opens the Models page", async () => {
    const main = await openApp("chat", { providers_list: [] });
    expect(main.textContent).toContain("Add one on the Models page");
    await click(byText(main, "button", "Go to Models"));
    await flush(5);
    expect(localStorage.getItem("nugget.page")).toBe("models");
    expect(current()).toContain("Models");
  });

  it("Today with no sources: Go to Sources opens the Sources page", async () => {
    const main = await openApp("today", { sources_list: [], cards_list: { cards: [], last_refresh: null, enabled: true } });
    await click(byText(main, "button", "Go to Sources"));
    await flush(5);
    expect(localStorage.getItem("nugget.page")).toBe("sources");
  });
});
