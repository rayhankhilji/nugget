// @vitest-environment jsdom
/**
 * LL2: KK2's Sources notes for a folder Nugget can't index anything from (nothing indexable,
 * no access, gone) are polite status notes in plain words with the skipped files, and the page
 * has no axe violations with them. The e2e corpus folder always has something to index, so these
 * states are only reachable here.
 */
import axe from "axe-core";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { ScanReport, SourceInfo } from "../api";
import { cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
import { SourcesPage } from "./Sources";

beforeEach(installDomShims);
afterEach(cleanup);

const scan = (over: Partial<ScanReport>): ScanReport => ({
  finished: true,
  found: 0,
  unsupported: 0,
  too_large: 0,
  unreadable: 0,
  roots_missing: 0,
  roots_denied: 0,
  max_file_bytes: 50_000_000,
  ...over,
});
const source = (over: Partial<SourceInfo>): SourceInfo => ({
  id: "files-9",
  kind: "files",
  name: "Scans",
  enabled: true,
  state: "idle",
  items: 0,
  chunks: 0,
  last_sync: null,
  ...over,
});

describe("Sources: folders with nothing to index (LL2)", () => {
  it.each([
    ["nothing indexable", scan({ unsupported: 3, skipped: [{ path: "photo.heic", reason: "unsupported" }] })],
    ["no access", scan({ roots_denied: 1 })],
    ["gone", scan({ roots_missing: 1 })],
  ] as const)("says so plainly, as a status, with no violations: %s", async (_name, report) => {
    mockDaemon({ sources_list: [source({ scan: report })] });
    const { container } = await render(<SourcesPage />);
    await flush();
    const note = container.querySelector(".source-scan")!;
    expect(note).not.toBeNull();
    expect(note.getAttribute("role")).toBe("status");
    expect(note.textContent).toContain("Scans");
    expect(note.textContent).not.toMatch(/undefined|\{"|roots_/);
    const r = await axe.run(document.body, { rules: { "color-contrast": { enabled: false }, region: { enabled: false } } });
    expect(r.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`)).toEqual([]);
  });
});
