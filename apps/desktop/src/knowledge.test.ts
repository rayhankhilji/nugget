import { describe, expect, it } from "vitest";
import type { ClaimView, EvidenceView } from "./api";
import {
  factAction,
  formatEvidence,
  joinVaultPath,
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
  it("never offers to open URLs open_url would refuse", () => {
    for (const url of ["javascript:alert(1)", "http://example.com", "file://server/share/x", "data:text/html,x"]) {
      expect(formatEvidence(ev({ url })).openUrl, url).toBeNull();
    }
    expect(formatEvidence(ev({ url: "file:///home/me/a.md" })).openUrl).toBe("file:///home/me/a.md");
  });
});

describe("vault links", () => {
  it("refuses paths that leave the vault", () => {
    for (const rel of ["../etc/passwd", "a/../../x.md", "/etc/passwd", "\\\\server\\share", "C:\\x.md", "C:x.md", "", "a/\u0000.md"]) {
      expect(joinVaultPath("/home/me/vault", rel), rel).toBeNull();
    }
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
    expect(factAction(claim({ status: "user_asserted" }))?.next).toBe("active");
    for (const status of ["rejected", "retracted"] as const) {
      expect(factAction(claim({ status }))).toEqual({ action: "restore", decision: "active", undo: null, next: "active" });
    }
    // GG5-14: a superseded fact is history; "Restore" would make two values current with no Undo.
    expect(factAction(claim({ status: "superseded" }))).toBeNull();
    // A rejected fact the user wrote comes back as theirs.
    expect(factAction(claim({ status: "rejected", evidence: [] }))?.next).toBe("user_asserted");
  });
});
