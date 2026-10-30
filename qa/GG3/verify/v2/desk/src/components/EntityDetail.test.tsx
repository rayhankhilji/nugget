// @vitest-environment jsdom
/** Entity page: facts grouped by recency, relative dates with absolute tooltips, source icons. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { EntityDetail } from "../api";
import { formatTime } from "../format";
import { formatDate } from "../i18n";
import { entityDetail } from "../test/fixtures";
import { byText, cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import { EntityDetailPanel } from "./EntityDetail";

const now = new Date(2026, 9, 28, 15, 0, 0);
const daysAgo = (n: number) => new Date(now.getTime() - n * 86_400_000).toISOString();

const timed: EntityDetail = {
  ...entityDetail,
  claims: [
    {
      ...entityDetail.claims[0]!,
      evidence: [
        { item: { source: "mail", key: "m1" }, quote: "Ada at the engine", title: "Re: engine", url: null, observed_at: daysAgo(3) },
        { item: { source: "files-1", key: "notes/engine.md" }, quote: "Ada's notes", title: "Engine notes", url: null },
      ],
    },
    {
      id: 2,
      subject: { id: "e1", name: "Ada Lovelace" },
      predicate: "born_in",
      object: { literal: "London" },
      text: "Ada Lovelace was born in London",
      confidence: 0.8,
      status: "active",
      evidence: [],
      updated_at: daysAgo(400),
    },
  ],
};

const panel = (sourceKinds?: ReadonlyMap<string, string>) => (
  <EntityDetailPanel
    id="e1"
    vaultRoot={null}
    sourceNames={new Map([["mail", "Work mail"]])}
    sourceKinds={sourceKinds}
    onSelect={() => {}}
    onMerged={() => {}}
  />
);

beforeEach(() => {
  installDomShims();
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(now);
});
afterEach(() => {
  vi.useRealTimers();
  cleanup();
});

describe("EntityDetailPanel facts", () => {
  it("groups facts by recency under headings, with relative dates", async () => {
    mockDaemon({ knowledge_entity: timed });
    const { container } = await render(panel(new Map([["mail", "imap"]])));
    const headings = Array.from(container.querySelectorAll("h4")).map((h) => h.textContent);
    expect(headings).toEqual(["This week", "Earlier"]);
    // Each group is a list of facts.
    expect(container.querySelectorAll(".fact-group-recency > ul.facts > li.fact").length).toBe(2);

    const times = Array.from(container.querySelectorAll(".fact-seen time"));
    expect(times.map((t) => t.textContent)).toEqual(["3 days ago", "last year"]);
    expect(times[0]!.getAttribute("dateTime")).toBe(daysAgo(3));
    expect(times[0]!.getAttribute("title")).toBe(formatTime(daysAgo(3)));
    expect(container.querySelector(".fact-seen")!.textContent).toContain("Last seen");
  });

  it("shows a source icon and date for each evidence item", async () => {
    mockDaemon({ knowledge_entity: timed });
    const { container } = await render(panel(new Map([["mail", "imap"]])));
    await click(byText(container, "button", /^Why\?/));
    const items = Array.from(container.querySelectorAll("ul.evidence > li"));
    expect(items.length).toBe(2);
    expect(items[0]!.querySelector("svg.source-icon-imap")).toBeTruthy();
    expect(items[0]!.querySelector(".evidence-source time")?.textContent).toBe("3 days ago");
    expect(items[0]!.querySelector(".evidence-title")?.textContent).toBe("Re: engine");
    // Not in the sources list: the kind is guessed from the id; no time when none is known.
    expect(items[1]!.querySelector("svg.source-icon-files")).toBeTruthy();
    expect(items[1]!.querySelector("time")).toBeNull();
    // Mentions get icons too.
    expect(container.querySelector(".mentions svg.source-icon-files")).toBeTruthy();
  });

  it("shows ISO dates inside fact text in the reader's locale, keeping the stored text", async () => {
    const at = "2026-11-10T13:00:00Z";
    const stored = `Ada attended Wren Mobile usability test on ${at}, due 2026-11-12T00:00:00Z`;
    const detail: EntityDetail = { ...timed, claims: [{ ...timed.claims[1]!, id: 3, text: stored }] };
    mockDaemon({ knowledge_entity: detail });
    const { container } = await render(panel());
    const fact = container.querySelector(".fact-text")!.textContent;
    expect(fact).toBe(
      `Ada attended Wren Mobile usability test on ${formatDate(at, { dateStyle: "medium", timeStyle: "short" })}, due Nov 12, 2026`,
    );
    expect(fact).not.toContain("T13:00");
    expect(detail.claims[0]!.text).toBe(stored);
  });

  it("falls back to grouping by predicate when the daemon sends no timestamps", async () => {
    mockDaemon({ knowledge_entity: entityDetail });
    const { container } = await render(panel());
    expect(Array.from(container.querySelectorAll("h4")).map((h) => h.textContent)).toEqual(["works on"]);
    expect(container.querySelector(".fact-group-recency")).toBeNull();
    expect(container.querySelector("time")).toBeNull();
  });
});
