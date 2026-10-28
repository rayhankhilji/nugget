import { describe, expect, it } from "vitest";
import {
  ApiError,
  isIngestProgress,
  isMethodNotFound,
  isOpenableUrl,
  isSourceInfo,
  isSourceKind,
  isSourceState,
  mergeSources,
  METHOD_NOT_FOUND,
  sourcesFromChange,
  type SourceInfo,
} from "./api";

const source = (over: Partial<SourceInfo> = {}): SourceInfo => ({
  id: "files-1",
  kind: "files",
  name: "Documents",
  enabled: true,
  state: "watching",
  items: 12,
  chunks: 40,
  last_sync: "2026-10-28T09:00:00Z",
  last_error: null,
  settings: { roots: ["/home/me/Documents"] },
  ...over,
});

describe("isSourceState / isSourceKind", () => {
  it("accepts exactly the protocol's snake_case values", () => {
    for (const s of ["idle", "syncing", "watching", "needs_auth", "disabled", "error"]) expect(isSourceState(s)).toBe(true);
    for (const s of ["NeedsAuth", "needsAuth", "", "ready", 1, null, undefined]) expect(isSourceState(s)).toBe(false);
    expect(isSourceKind("gcal")).toBe(true);
    expect(isSourceKind("dropbox")).toBe(false);
  });
});

describe("isSourceInfo", () => {
  it("accepts a full SourceInfo and one with optional fields omitted", () => {
    expect(isSourceInfo(source())).toBe(true);
    const { last_sync: _a, last_error: _b, settings: _c, ...minimal } = source();
    expect(isSourceInfo(minimal)).toBe(true);
  });

  it("rejects wrong shapes", () => {
    expect(isSourceInfo(null)).toBe(false);
    expect(isSourceInfo([source()])).toBe(false);
    expect(isSourceInfo({ ...source(), state: "Watching" })).toBe(false);
    expect(isSourceInfo({ ...source(), items: -1 })).toBe(false);
    expect(isSourceInfo({ ...source(), items: "12" })).toBe(false);
    expect(isSourceInfo({ ...source(), enabled: "yes" })).toBe(false);
    expect(isSourceInfo({ ...source(), last_sync: 1700000000 })).toBe(false);
    expect(isSourceInfo({ ...source(), settings: ["x"] })).toBe(false);
    const { id: _id, ...noId } = source();
    expect(isSourceInfo(noId)).toBe(false);
  });
});

describe("isIngestProgress", () => {
  it("validates notification payloads", () => {
    expect(isIngestProgress({ source: "files-1", phase: "indexing", done: 3, total: 10 })).toBe(true);
    expect(isIngestProgress({ source: "files-1", phase: "scanning", done: 0 })).toBe(true);
    expect(isIngestProgress({ source: "files-1", phase: "error", done: 0, total: null, error: "boom" })).toBe(true);
    expect(isIngestProgress({ source: "files-1", phase: "paused", done: 0 })).toBe(false);
    expect(isIngestProgress({ source: "files-1", phase: "indexing", done: Number.NaN })).toBe(false);
    expect(isIngestProgress({ phase: "indexing", done: 1 })).toBe(false);
    expect(isIngestProgress("indexing")).toBe(false);
  });
});

describe("sourcesFromChange", () => {
  it("extracts sources from the payload shapes the daemon might send", () => {
    expect(sourcesFromChange(source())).toEqual([source()]);
    expect(sourcesFromChange([source(), source({ id: "gmail-1", kind: "gmail" })])).toHaveLength(2);
    expect(sourcesFromChange({ sources: [source()] })).toEqual([source()]);
  });

  it("returns null (refetch) for anything else", () => {
    for (const p of [null, undefined, {}, [], { id: "files-1" }, [source(), { id: "x" }], "files-1"]) {
      expect(sourcesFromChange(p)).toBeNull();
    }
  });
});

describe("mergeSources", () => {
  it("replaces by id in place and appends new ones", () => {
    const a = source({ id: "a" });
    const b = source({ id: "b" });
    const b2 = source({ id: "b", state: "syncing" });
    const c = source({ id: "c" });
    expect(mergeSources([a, b], [b2, c]).map((s) => [s.id, s.state])).toEqual([
      ["a", "watching"],
      ["b", "syncing"],
      ["c", "watching"],
    ]);
  });
});

describe("method not found", () => {
  it("is recognised from the serialized command error", () => {
    const wire = { kind: "rpc", message: "method not found: sources.list", code: METHOD_NOT_FOUND };
    expect(isMethodNotFound(wire)).toBe(true);
    expect(ApiError.from(wire).isMethodNotFound).toBe(true);
    expect(isMethodNotFound({ kind: "rpc", message: "nope", code: -32001 })).toBe(false);
    expect(isMethodNotFound({ kind: "not_running", message: "down", code: null })).toBe(false);
    expect(isMethodNotFound(null)).toBe(false);
    expect(isMethodNotFound(new Error("x"))).toBe(false);
  });
});

describe("isOpenableUrl", () => {
  it("allows https, obsidian and local file URLs", () => {
    for (const u of [
      "https://mail.google.com/mail/u/0/#inbox/abc",
      "HTTPS://example.com/",
      "obsidian://open?vault=Notes&file=Daily%2F2026-10-28",
      "file:///home/me/Notes/a%20b.md",
      "file://localhost/tmp/x.txt",
      "file:///C:/Users/me/a.md",
    ]) {
      expect(isOpenableUrl(u), u).toBe(true);
    }
  });

  it("refuses script, data and other schemes", () => {
    for (const u of [
      "javascript:alert(1)",
      "JavaScript:alert(1)",
      " javascript:alert(1)",
      "java\tscript:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "vbscript:msgbox(1)",
      "http://example.com/",
      "mailto:me@example.com",
      "smb://server/share",
      "ms-msdt:/id",
      "blob:https://example.com/x",
      "about:blank",
    ]) {
      expect(isOpenableUrl(u), u).toBe(false);
    }
  });

  it("refuses network shares, malformed and oversized input", () => {
    for (const u of [
      "file://server/share/doc.txt",
      "file:////server/share/doc.txt",
      "https://",
      "not a url",
      "/etc/passwd",
      "https://example.com/a b",
      "https://example.com/\n",
      "",
      null,
      undefined,
      `https://example.com/${"a".repeat(9000)}`,
    ]) {
      expect(isOpenableUrl(u), String(u).slice(0, 40)).toBe(false);
    }
  });
});
