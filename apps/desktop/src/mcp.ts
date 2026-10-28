/** Helpers for the Connected AI apps page (MCP clients). */
import { MCP_TOOLS, type McpScopes, type SourceInfo } from "./api";

export const SETUP_APP_LABEL: Record<string, string> = {
  "claude-desktop": "Claude Desktop",
  "claude-code": "Claude Code",
  cursor: "Cursor",
  "generic-http": "Other app (HTTP)",
  "generic-stdio": "Other app (stdio)",
};

export function setupAppLabel(app: string): string {
  return SETUP_APP_LABEL[app] ?? app;
}

/** Order snippets are shown in: the named apps first, then generic ones, then anything else. */
export function sortSnippets<T extends { app: string }>(snippets: readonly T[]): T[] {
  const order = Object.keys(SETUP_APP_LABEL);
  const rank = (a: string) => {
    const i = order.indexOf(a);
    return i < 0 ? order.length : i;
  };
  return [...snippets].sort((a, b) => rank(a.app) - rank(b.app));
}

export const MAX_CLIENT_NAME = 100;

/** What the Connect an app form collects. */
export interface ScopeForm {
  name: string;
  allSources: boolean;
  sources: string[];
  allTools: boolean;
  tools: string[];
  /** Off by default. */
  includeSensitive: boolean;
}

export const DEFAULT_SCOPE_FORM: ScopeForm = {
  name: "",
  allSources: true,
  sources: [],
  allTools: true,
  tools: [],
  includeSensitive: false,
};

export type ScopeFormErrors = Partial<Record<"name" | "sources" | "tools", string>>;

export function validateScopeForm(f: ScopeForm): ScopeFormErrors {
  const errors: ScopeFormErrors = {};
  const name = f.name.trim();
  if (!name) errors.name = "Give the app a name, such as “Claude Desktop”.";
  else if (Array.from(name).length > MAX_CLIENT_NAME) errors.name = `Use at most ${MAX_CLIENT_NAME} characters.`;
  if (!f.allSources && f.sources.length === 0) errors.sources = "Choose at least one source, or allow all sources.";
  if (!f.allTools && f.tools.length === 0) errors.tools = "Choose at least one tool, or allow all tools.";
  return errors;
}

/** Wire scopes: `sources: null` means all sources, `tools: []` all read-only tools. */
export function scopesFromForm(f: ScopeForm): McpScopes {
  return {
    sources: f.allSources ? null : [...new Set(f.sources)],
    tools: f.allTools ? [] : [...new Set(f.tools)],
    include_sensitive: f.includeSensitive,
  };
}

export interface ScopeSummary {
  sources: string;
  tools: string;
  sensitive: boolean;
}

export function summarizeScopes(scopes: McpScopes, sources?: readonly SourceInfo[]): ScopeSummary {
  const names = new Map((sources ?? []).map((s) => [s.id, s.name]));
  const toolLabels = new Map(MCP_TOOLS.map((t) => [t.name, t.label]));
  const list = scopes.sources;
  const tools = scopes.tools ?? [];
  return {
    sources:
      list == null
        ? "All sources"
        : list.length === 0
          ? "No sources"
          : list.map((id) => names.get(id) ?? id).join(", "),
    tools: tools.length === 0 ? "All read-only tools" : tools.map((t) => toolLabels.get(t) ?? t).join(", "),
    sensitive: scopes.include_sensitive === true,
  };
}
