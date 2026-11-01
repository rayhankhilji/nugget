import { describe, expect, it } from "vitest";
import { DEFAULT_SCOPE_FORM, scopesFromForm, validateScopeForm } from "./mcp";


describe("scope form", () => {
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
