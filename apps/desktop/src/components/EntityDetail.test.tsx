// @vitest-environment jsdom
/** Entity page: facts grouped by recency, relative dates with absolute tooltips, source icons. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { EntityDetail } from "../api";
import { formatTime } from "../format";
import { entityDetail } from "../test/fixtures";
import { cleanup, installDomShims, mockDaemon, render } from "../test/render";
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

  it("badges an entity and facts known only from sensitive items", async () => {
    const detail: EntityDetail = {
      ...timed,
      sensitive: true,
      claims: [{ ...timed.claims[0]!, sensitive: true }, timed.claims[1]!],
    };
    mockDaemon({ knowledge_entity: detail });
    const { container } = await render(panel());
    const head = container.querySelector(".entity-sub")!;
    expect(head.textContent).toContain("Sensitive");
    expect(head.querySelector("[title]")!.getAttribute("title")).toMatch(/no vault page/);
    const facts = Array.from(container.querySelectorAll("li.fact"));
    expect(facts.map((f) => f.querySelector(".fact-meta")!.textContent!.includes("Sensitive"))).toEqual([true, false]);

    cleanup();
    installDomShims();
    mockDaemon({ knowledge_entity: timed });
    const plain = await render(panel());
    expect(plain.container.textContent).not.toContain("Sensitive");
  });

  it("offers no Restore on a superseded fact and counts current facts only (GG5-14)", async () => {
    const detail: EntityDetail = {
      ...timed,
      claims: [timed.claims[0]!, { ...timed.claims[1]!, status: "superseded" }, { ...timed.claims[1]!, id: 4, status: "rejected" }],
    };
    mockDaemon({ knowledge_entity: detail });
    const { container } = await render(panel());
    expect(container.querySelector("h3.entity-section")!.textContent).toMatch(/\b1\b/);
    expect(container.querySelector("h3.entity-section")!.textContent).not.toMatch(/\b2\b/);
    const superseded = container.querySelector("li.fact-superseded")!;
    expect(superseded.querySelector("button.fact-reject")).toBeNull();
    expect(Array.from(superseded.querySelectorAll("button")).map((b) => b.textContent)).not.toContain("Restore");
    // A rejected fact can still be restored.
    const rejected = container.querySelector("li.fact-retracted")!;
    expect(Array.from(rejected.querySelectorAll("button")).map((b) => b.textContent)).toContain("Restore");
  });
});
