import { describe, expect, it } from "vitest";
import type { ClaimView, EntityDetail, EvidenceView } from "./api";
import {
  attributeRows,
  claimStatusLabel,
  claimText,
  factAction,
  graphSourceOptions,
  isCurrentStatus,
  isDroppedStatus,
  splitProblem,
  splitRequest,
  undoStatus,
  confidenceTone,
  evidenceSummary,
  fileUrl,
  formatConfidence,
  formatEvidence,
  groupClaims,
  joinVaultPath,
  kindLabel,
  MAX_QUOTE_CHARS,
  obsidianUrl,
  predicateLabel,
  truncateText,
  vaultLinks,
} from "./knowledge";

const ev = (over: Partial<EvidenceView> = {}): EvidenceView => ({
  item: { source: "gmail-1", key: "msg-123" },
  chunk_id: 7,
  quote: "Sam  joined\nAcme in March.",
  title: "Re: Offer letter",
  url: "https://mail.google.com/mail/u/0/#inbox/abc",
  ...over,
});

const claim = (over: Partial<ClaimView> = {}): ClaimView => ({
  id: 1,
  subject: { id: "ent_sam", name: "Sam" },
  predicate: "works_at",
  object: { entity: { id: "ent_acme", name: "Acme" } },
  text: "Sam works at Acme.",
  confidence: 0.9,
  status: "active",
  evidence: [ev()],
  ...over,
});

describe("formatEvidence", () => {
  it("collapses whitespace, keeps title and maps the source name", () => {
    const d = formatEvidence(ev(), new Map([["gmail-1", "Work mail"]]));
    expect(d).toEqual({
      title: "Re: Offer letter",
      source: "Work mail",
      quote: "Sam joined Acme in March.",
      truncated: false,
      openUrl: "https://mail.google.com/mail/u/0/#inbox/abc",
    });
  });

  it("falls back to the item key and source id, and hides missing quotes", () => {
    const d = formatEvidence(ev({ title: "  ", quote: null, url: null }));
    expect(d.title).toBe("msg-123");
    expect(d.source).toBe("gmail-1");
    expect(d.quote).toBeNull();
    expect(d.openUrl).toBeNull();
  });

  it("never offers to open URLs open_url would refuse", () => {
    for (const url of ["javascript:alert(1)", "http://example.com", "file://server/share/x", "data:text/html,x"]) {
      expect(formatEvidence(ev({ url })).openUrl, url).toBeNull();
    }
    expect(formatEvidence(ev({ url: "file:///home/me/a.md" })).openUrl).toBe("file:///home/me/a.md");
  });

  it("truncates long quotes at a word boundary", () => {
    const long = "word ".repeat(200);
    const d = formatEvidence(ev({ quote: long }));
    expect(d.truncated).toBe(true);
    expect(d.quote!.endsWith("…")).toBe(true);
    expect(Array.from(d.quote!).length).toBeLessThanOrEqual(MAX_QUOTE_CHARS + 1);
    expect(d.quote).not.toMatch(/\s…$/);
  });

  it("truncateText counts characters, not UTF-16 units", () => {
    const emoji = "😀".repeat(10);
    expect(truncateText(emoji, 10)).toEqual({ text: emoji, truncated: false });
    expect(truncateText(emoji, 5).text).toBe(`${"😀".repeat(5)}…`);
  });

  it("summarises distinct source items", () => {
    expect(evidenceSummary([])).toBe("No sources");
    expect(evidenceSummary([ev()])).toBe("1 source");
    expect(evidenceSummary([ev(), ev({ chunk_id: 8 })])).toBe("1 source");
    expect(evidenceSummary([ev(), ev({ item: { source: "files-1", key: "a.md" } })])).toBe("2 sources");
  });
});

describe("claims", () => {
  it("formats confidence", () => {
    expect(formatConfidence(0.923)).toBe("92%");
    expect(formatConfidence(1.4)).toBe("100%");
    expect(formatConfidence(-1)).toBe("0%");
    expect(formatConfidence(Number.NaN)).toBe("–");
    expect(confidenceTone(0.95)).toBe("good");
    expect(confidenceTone(0.6)).toBe("neutral");
    expect(confidenceTone(0.2)).toBe("warn");
  });

  it("labels predicates and kinds", () => {
    expect(predicateLabel("works_at")).toBe("works at");
    expect(predicateLabel("reports-to")).toBe("reports to");
    expect(kindLabel("organization")).toBe("Organization");
    expect(kindLabel("spaceship")).toBe("spaceship");
  });

  it("falls back to predicate + object when a fact has no sentence", () => {
    expect(claimText(claim({ text: " " }))).toBe("works at Acme");
    expect(claimText(claim({ text: "", object: { literal: "London" }, predicate: "lives_in" }))).toBe("lives in London");
  });

  it("groups by predicate with current facts first", () => {
    const groups = groupClaims([
      claim({ id: 1, predicate: "works_at", status: "retracted", confidence: 0.99 }),
      claim({ id: 2, predicate: "works_at", status: "active", confidence: 0.5 }),
      claim({ id: 3, predicate: "email", status: "user_asserted", confidence: 1 }),
      claim({ id: 4, predicate: "works_at", status: "active", confidence: 0.8 }),
      claim({ id: 5, predicate: "works_at", status: "superseded", confidence: 0.9 }),
    ]);
    expect(groups.map((g) => g.predicate)).toEqual(["email", "works_at"]);
    expect(groups[1]!.claims.map((c) => c.id)).toEqual([4, 2, 5, 1]);
  });

  it("turns attributes into rows", () => {
    expect(attributeRows({ role: "CTO", start_year: 2021, empty: "", none: null })).toEqual([
      ["role", "CTO"],
      ["start year", "2021"],
    ]);
    expect(attributeRows(null)).toEqual([]);
    expect(attributeRows([1, 2])).toEqual([]);
  });
});

describe("vault links", () => {
  it("joins safe relative paths", () => {
    expect(joinVaultPath("/home/me/Nugget Vault/", "people/Sam Lee.md")).toBe("/home/me/Nugget Vault/people/Sam Lee.md");
    expect(joinVaultPath("C:\\Users\\me\\Vault", "people/Sam.md")).toBe("C:\\Users\\me\\Vault\\people\\Sam.md");
    expect(joinVaultPath("/v", "./a//b.md")).toBe("/v/a/b.md");
  });

  it("refuses paths that leave the vault", () => {
    for (const rel of ["../etc/passwd", "a/../../x.md", "/etc/passwd", "\\\\server\\share", "C:\\x.md", "C:x.md", "", "a/\u0000.md"]) {
      expect(joinVaultPath("/home/me/vault", rel), rel).toBeNull();
    }
  });

  it("builds file and obsidian URLs", () => {
    expect(fileUrl("/home/me/Nugget Vault/people/Sam #1.md")).toBe("file:///home/me/Nugget%20Vault/people/Sam%20%231.md");
    expect(fileUrl("C:\\Users\\me\\a b.md")).toBe("file:///C:/Users/me/a%20b.md");
    expect(fileUrl("relative/a.md")).toBeNull();
    expect(fileUrl("//server/share/a.md")).toBeNull();
    expect(obsidianUrl("/home/me/v/a.md")).toBe("obsidian://open?path=%2Fhome%2Fme%2Fv%2Fa.md");
    expect(obsidianUrl("rel.md")).toBeNull();
  });

  it("vaultLinks combines both and stays within what open_url accepts", () => {
    expect(vaultLinks("/home/me/vault", "people/sam.md")).toEqual({
      file: "file:///home/me/vault/people/sam.md",
      obsidian: "obsidian://open?path=%2Fhome%2Fme%2Fvault%2Fpeople%2Fsam.md",
    });
    expect(vaultLinks(null, "people/sam.md")).toBeNull();
    expect(vaultLinks("/home/me/vault", null)).toBeNull();
    expect(vaultLinks("/home/me/vault", "../x.md")).toBeNull();
  });
});

describe("fact decisions", () => {
  it("rejects current facts, withdraws the user's own and restores the rest", () => {
    expect(factAction(claim())).toEqual({ action: "reject", decision: "rejected", undo: "active", next: "rejected" });
    // Undo of a vault assertion: the sources decide what is left.
    expect(factAction(claim({ status: "user_asserted", evidence: [] }))).toEqual({
      action: "unassert",
      decision: "unasserted",
      undo: "user_asserted",
      next: "retracted",
    });
    expect(factAction(claim({ status: "user_asserted" })).next).toBe("active");
    for (const status of ["rejected", "retracted", "superseded"] as const) {
      expect(factAction(claim({ status }))).toEqual({ action: "restore", decision: "active", undo: null, next: "active" });
    }
    // A rejected fact the user wrote comes back as theirs.
    expect(factAction(claim({ status: "rejected", evidence: [] })).next).toBe("user_asserted");
  });

  it("predicts the status an undo leads to", () => {
    expect(undoStatus("active", claim())).toBe("active");
    expect(undoStatus("active", claim({ evidence: [] }))).toBe("user_asserted");
    expect(undoStatus("user_asserted", claim({ evidence: [] }))).toBe("user_asserted");
    expect(undoStatus("rejected", claim())).toBe("rejected");
    expect(undoStatus("unasserted", claim({ evidence: [] }))).toBe("retracted");
  });

  it("classifies and labels statuses", () => {
    expect(isCurrentStatus("active") && isCurrentStatus("user_asserted")).toBe(true);
    expect(isCurrentStatus("rejected") || isCurrentStatus("superseded")).toBe(false);
    expect(isDroppedStatus("rejected") && isDroppedStatus("retracted")).toBe(true);
    expect(isDroppedStatus("superseded") || isDroppedStatus("active")).toBe(false);
    expect(claimStatusLabel("rejected")).toBe("Rejected");
    expect(claimStatusLabel("retracted")).toBe("No longer supported");
    expect(claimStatusLabel("brand_new")).toBe("brand_new");
  });

  it("sorts rejected facts last", () => {
    const [g] = groupClaims([claim({ id: 1, status: "rejected" }), claim({ id: 2, status: "superseded" }), claim({ id: 3 })]);
    expect(g!.claims.map((c) => c.id)).toEqual([3, 2, 1]);
  });
});

describe("entity split", () => {
  const entity: Pick<EntityDetail, "id" | "name" | "aliases" | "identifiers" | "claims"> = {
    id: "ent_sam",
    name: "Sam",
    aliases: ["Sam", "Sammy", "Samantha"],
    identifiers: [
      ["email", "sam@acme.com"],
      ["email", "sam@home.net"],
    ],
    claims: [claim({ id: 1 }), claim({ id: 2 }), claim({ id: 3 })],
  };

  it("needs a new name and at least one fact", () => {
    const draft = { name: "Sam Lee", claims: [2], aliases: [], identifiers: [] };
    expect(splitProblem(entity, draft)).toBeNull();
    expect(splitProblem(entity, { ...draft, name: "  " })).toBe("split.needName");
    expect(splitProblem(entity, { ...draft, name: " sam " })).toBe("split.sameName");
    expect(splitProblem(entity, { ...draft, claims: [] })).toBe("split.needFacts");
  });

  it("builds a request with only details the entity has", () => {
    expect(
      splitRequest(entity, {
        name: "  Sam   Lee ",
        claims: [3, 2, 3, 99],
        aliases: ["Sammy", "Nobody"],
        identifiers: [
          ["email", "sam@home.net"],
          ["email", "x@y.z"],
        ],
      }),
    ).toEqual({
      entity: "ent_sam",
      claims: [2, 3],
      name: "Sam Lee",
      aliases: ["Sammy"],
      identifiers: [["email", "sam@home.net"]],
    });
  });
});

describe("graph source filter", () => {
  it("lists sources by name, falling back to the id", () => {
    expect(
      graphSourceOptions([
        { id: "mail-1", name: "Work mail" },
        { id: "notes-1", name: " " },
        { id: "cal-1", name: "Calendar" },
      ]),
    ).toEqual([
      { id: "cal-1", name: "Calendar" },
      { id: "notes-1", name: "notes-1" },
      { id: "mail-1", name: "Work mail" },
    ]);
  });
});
