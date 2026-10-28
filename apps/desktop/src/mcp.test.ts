import { describe, expect, it } from "vitest";
import type { SourceInfo } from "./api";
import { DEFAULT_SCOPE_FORM, scopesFromForm, setupAppLabel, sortSnippets, summarizeScopes, validateScopeForm } from "./mcp";

const src = (id: string, name: string): SourceInfo => ({ id, name, kind: "files", enabled: true, state: "idle", items: 0, chunks: 0 });

describe("scope form", () => {
  it("defaults to all sources, all read-only tools and no sensitive items", () => {
    expect(DEFAULT_SCOPE_FORM.includeSensitive).toBe(false);
    expect(scopesFromForm({ ...DEFAULT_SCOPE_FORM, name: "Cursor" })).toEqual({ sources: null, tools: [], include_sensitive: false });
  });

  it("narrows to chosen sources and tools, de-duplicated", () => {
    expect(
      scopesFromForm({
        ...DEFAULT_SCOPE_FORM,
        allSources: false,
        sources: ["files-1", "files-1", "gmail-1"],
        allTools: false,
        tools: ["search", "get_entity"],
        includeSensitive: true,
      }),
    ).toEqual({ sources: ["files-1", "gmail-1"], tools: ["search", "get_entity"], include_sensitive: true });
  });

  it("validates name and selections", () => {
    expect(validateScopeForm(DEFAULT_SCOPE_FORM).name).toBeDefined();
    expect(validateScopeForm({ ...DEFAULT_SCOPE_FORM, name: "x".repeat(101) }).name).toBeDefined();
    expect(validateScopeForm({ ...DEFAULT_SCOPE_FORM, name: "Claude" })).toEqual({});
    const e = validateScopeForm({ ...DEFAULT_SCOPE_FORM, name: "Claude", allSources: false, allTools: false });
    expect(e.sources).toBeDefined();
    expect(e.tools).toBeDefined();
  });
});

describe("summaries", () => {
  it("describes scopes in words", () => {
    expect(summarizeScopes({ sources: null, tools: [], include_sensitive: false })).toEqual({
      sources: "All sources",
      tools: "All read-only tools",
      sensitive: false,
    });
    expect(
      summarizeScopes({ sources: ["files-1", "gone-1"], tools: ["search", "custom"], include_sensitive: true }, [src("files-1", "Documents")]),
    ).toEqual({ sources: "Documents, gone-1", tools: "Search, custom", sensitive: true });
    expect(summarizeScopes({}).sources).toBe("All sources");
  });

  it("labels and orders setup snippets", () => {
    expect(setupAppLabel("claude-code")).toBe("Claude Code");
    expect(setupAppLabel("zed")).toBe("zed");
    const sorted = sortSnippets([{ app: "generic-stdio" }, { app: "zed" }, { app: "cursor" }, { app: "claude-desktop" }]);
    expect(sorted.map((s) => s.app)).toEqual(["claude-desktop", "cursor", "generic-stdio", "zed"]);
  });
});
