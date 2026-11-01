// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { Highlight, plainSnippet, SourceBadge } from "./Provenance";
import { cleanup, render } from "../test/render";

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
});

describe("item names", () => {
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
