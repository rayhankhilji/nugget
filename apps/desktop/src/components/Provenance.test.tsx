// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { Highlight, itemFileName, nameAddsInfo, plainSnippet, SourceBadge } from "./Provenance";
import { fitNodes } from "./GraphCanvas";
import { cleanup, render } from "../test/render";
import type { SimNode } from "../graph";

afterEach(() => cleanup());

describe("plainSnippet", () => {
  it("strips Markdown and wiki syntax", () => {
    expect(plainSnippet("# Q4 budget\nSee [[Tidewater Billing]] and [[Ada Wexford|Ada]]. #budget")).toBe(
      "Q4 budget See Tidewater Billing and Ada. budget",
    );
    expect(plainSnippet("**Bold** and *soft* and `code` and ~~gone~~ [link](https://x.test/a)")).toBe("Bold and soft and code and gone link");
    expect(plainSnippet("Intro\n- first\n> quoted\n2. second")).toBe("Intro first quoted second");
    expect(plainSnippet("---\ntitle: x\n---\nBody")).toBe("title: x Body");
  });

  it("keeps real content", () => {
    for (const keep of [
      "Edit __init__.py and __main__.py",
      "Fixed in #123 and PR #45",
      "Press # to dial",
      "2023. Revenue grew",
      "> 50% of users churned",
      "- 5°C tonight",
      "1) step one",
      "snake_case_name and 2 * 3 * 4",
    ]) {
      expect(plainSnippet(keep)).toBe(keep);
    }
    expect(plainSnippet("Low tonight:\n- 5°C")).toBe("Low tonight: - 5°C");
    expect(plainSnippet("Bold __word__.")).toBe("Bold word.");
  });

  it("leaves nothing for a rule-only quote", () => {
    expect(plainSnippet("---")).toBe("");
    expect(plainSnippet("***")).toBe("");
  });
});

describe("Highlight", () => {
  it("marks query words case-insensitively and survives regex characters", async () => {
    const { container } = await render(
      <p>
        <Highlight text="Tidewater budget (b) c++ [x] $5 a/b budgets" query="BUDGET (b) c++ [x] $5 a/b" />
      </p>,
    );
    const marks = Array.from(container.querySelectorAll("mark")).map((m) => m.textContent);
    expect(marks).toEqual(["budget", "(b)", "c++", "[x]", "$5", "a/b", "budget"]);
    expect(container.textContent).toBe("Tidewater budget (b) c++ [x] $5 a/b budgets");
  });

  it("returns the text alone for a query without words", async () => {
    const { container } = await render(
      <p>
        <Highlight text="a.b" query=" . " />
      </p>,
    );
    expect(container.querySelector("mark")).toBeNull();
  });
});

describe("item names", () => {
  it("shows file names for file sources and real extensions only", () => {
    expect(itemFileName("0:mail/mail-036.eml", "files")).toBe("mail-036.eml");
    expect(itemFileName("notes/Plan", "obsidian")).toBe("Plan");
    expect(itemFileName("C123/thread/1700000000.000200", "slack")).toBeNull();
    expect(itemFileName("inbox/AAMkADg3NGU0", "outlook")).toBeNull();
    expect(itemFileName("issue/8a6f2c1e-1b2c-4d5e-9f00-123456789abc", "linear")).toBeNull();
    expect(itemFileName("drive/Budget.xlsx", "gdrive")).toBe("Budget.xlsx");
    expect(itemFileName("x/report.pdf", "unknown")).toBe("report.pdf");
  });

  it("adds the file name only when the title doesn't already say it", () => {
    expect(nameAddsInfo("mail-036.eml", "Q4 budget")).toBe(true);
    expect(nameAddsInfo("engine.md", "Engine")).toBe(false);
    expect(nameAddsInfo(null, "x")).toBe(false);
  });

  it("renders a badge with the source name and never an opaque id", async () => {
    const { container } = await render(
      <>
        <SourceBadge sourceId="files-1" name="Fixture corpus" kind="files" itemKey="0:mail/mail-036.eml" title="Q4 budget" />
        <SourceBadge sourceId="slack-1" kind="slack" itemKey="C123/thread/1700000000.000200" title={null} />
      </>,
    );
    const [file, slack] = Array.from(container.querySelectorAll(".source-badge"));
    expect(file!.textContent).toContain("Fixture corpus");
    expect(file!.textContent).toContain("mail-036.eml");
    expect(file!.textContent).not.toContain("0:");
    expect(slack!.textContent).not.toContain("1700000000");
    expect(slack!.textContent).not.toContain("slack-1");
  });
});

describe("fitNodes", () => {
  const node = (id: string, degree: number) => ({ id, degree }) as unknown as SimNode;

  it("frames connected entities when nearly all are connected", () => {
    const nodes = [...Array.from({ length: 18 }, (_, i) => node(`n${i}`, 2)), node("lone1", 0), node("lone2", 0)];
    expect(fitNodes(nodes).map((n) => n.id)).not.toContain("lone1");
  });

  it("frames everything when many are unconnected or the graph is tiny", () => {
    const nodes = [node("a", 1), node("b", 1), node("c", 0), node("d", 0)];
    expect(fitNodes(nodes)).toBe(nodes);
    const tiny = [node("a", 1), node("b", 1), node("c", 0)];
    expect(fitNodes(tiny)).toBe(tiny);
  });
});

describe("merge: telling same-name entities apart", () => {
  it("uses another alias, then an identifier, then the end of the id", async () => {
    const { distinguishing } = await import("./MergeDialog");
    expect(distinguishing("Ada", "ent_abc123456", { aliases: ["Ada", "Ada L."], identifiers: [] })).toBe("Ada L.");
    expect(distinguishing("Ada", "ent_abc123456", { aliases: ["ada"], identifiers: [["email", "ada@x.test"]] })).toBe("ada@x.test");
    expect(distinguishing("Ada", "ent_abc123456", null)).toBe("#123456");
  });
});
