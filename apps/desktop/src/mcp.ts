/** Helpers for the Connected AI apps page (MCP clients). */
import { MCP_TOOLS, type McpScopes, type SourceInfo } from "./api";
import { t } from "./i18n";

/** Setup apps in display order. Named apps are product names and stay untranslated. */
const SETUP_APP_LABEL: Record<string, () => string> = {
  "claude-desktop": () => "Claude Desktop", // i18n-ignore: product name
  "claude-code": () => "Claude Code", // i18n-ignore: product name
  cursor: () => "Cursor",
  "generic-http": () => t("mcp.app.generic-http"),
  "generic-stdio": () => t("mcp.app.generic-stdio"),
};

export function setupAppLabel(app: string): string {
  return SETUP_APP_LABEL[app]?.() ?? app;
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
  if (!name) errors.name = t("mcp.error.name");
  else if (Array.from(name).length > MAX_CLIENT_NAME) errors.name = t("mcp.error.nameTooLong", { max: MAX_CLIENT_NAME });
  if (!f.allSources && f.sources.length === 0) errors.sources = t("mcp.error.sources");
  if (!f.allTools && f.tools.length === 0) errors.tools = t("mcp.error.tools");
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
  const toolLabels = new Map<string, string>(MCP_TOOLS.map((tool) => [tool.name, t(tool.label)]));
  const list = scopes.sources;
  const tools = scopes.tools ?? [];
  return {
    sources:
      list == null
        ? t("mcp.summary.allSources")
        : list.length === 0
          ? t("mcp.summary.noSources")
          : list.map((id) => names.get(id) ?? id).join(t("common.listSeparator")),
    tools:
      tools.length === 0
        ? t("mcp.summary.allTools")
        : tools.map((name) => toolLabels.get(name) ?? name).join(t("common.listSeparator")),
    sensitive: scopes.include_sensitive === true,
  };
}
