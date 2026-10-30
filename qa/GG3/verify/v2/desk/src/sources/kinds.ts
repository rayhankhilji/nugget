/**
 * Connector kinds in the Add source wizard: grouping, how each one connects, and pure builders
 * for the `settings` each kind sends with `sources.add`. Blank optional fields are left out
 * (the daemon's defaults apply), never sent as "" or [].
 */
import { MICROSOFT_KINDS, SAAS_KINDS, type ConnectorKind, type MicrosoftKind, type SaasKind, type SourceSettings } from "../api";
import { splitList } from "../format";
import type { MessageKey } from "../i18n";

export interface KindGroup {
  id: "local" | "google" | "microsoft" | "standard" | "work";
  label: MessageKey;
  kinds: readonly ConnectorKind[];
}

export const KIND_GROUPS: readonly KindGroup[] = [
  { id: "local", label: "addSource.group.local", kinds: ["files", "obsidian"] },
  { id: "google", label: "addSource.group.google", kinds: ["gmail", "gcal", "gdrive"] },
  { id: "microsoft", label: "addSource.group.microsoft", kinds: MICROSOFT_KINDS },
  { id: "standard", label: "addSource.group.standard", kinds: ["imap", "caldav"] },
  { id: "work", label: "addSource.group.work", kinds: SAAS_KINDS },
];

export const isMicrosoftKind = (k: string): k is MicrosoftKind => (MICROSOFT_KINDS as readonly string[]).includes(k);
export const isSaasKind = (k: string): k is SaasKind => (SAAS_KINDS as readonly string[]).includes(k);
export const isGoogleKind = (k: string): k is GoogleKind => (GOOGLE_KINDS as readonly string[]).includes(k);

/**
 * How an existing source (re)connects:
 * - `browser`: `sources.auth.begin` returns a URL to open (Google, Microsoft, Linear with `client_id`)
 * - `device`: same, plus a user code to enter on that page (GitHub with `client_id`)
 * - `password`: a password through `sources.secret.set` (IMAP, CalDAV)
 * - `token`: a pasted API token through `sources.secret.set` (Slack, Notion, GitHub, Linear)
 * - `env`: the token comes from an environment variable (`token_env`); nothing to do here
 * - `none`: local sources
 */
export type ConnectMode = "browser" | "device" | "password" | "token" | "env" | "none";

const hasString = (settings: SourceSettings | undefined, key: string) => {
  const v = settings?.[key];
  return typeof v === "string" && v.trim() !== "";
};

export function connectMode(kind: string, settings?: SourceSettings): ConnectMode {
  if (isGoogleKind(kind) || isMicrosoftKind(kind)) return "browser";
  if (kind === "imap" || kind === "caldav") return "password";
  if (isSaasKind(kind)) {
    if (hasString(settings, "token_env")) return "env";
    if ((kind === "github" || kind === "linear") && hasString(settings, "client_id")) {
      return kind === "github" ? "device" : "browser";
    }
    return "token";
  }
  return "none";
}

// ---- Google ----

export type GoogleKind = "gmail" | "gcal" | "gdrive";
export const GOOGLE_KINDS: readonly GoogleKind[] = ["gmail", "gcal", "gdrive"];

/** Google's product name for the API each kind needs enabled, as shown in the Cloud console. */
export const GOOGLE_API: Record<GoogleKind, string> = {
  gmail: "Gmail API",
  gcal: "Google Calendar API",
  gdrive: "Google Drive API",
};

/** The only scope Google Drive asks for: read-only (the Sheets and Slides APIs accept it for reads). */
export const GDRIVE_SCOPE = "https://www.googleapis.com/auth/drive.readonly";

export interface GoogleForm {
  clientId: string;
  /** Google Drive only. */
  includeSharedWithMe: boolean;
  includeSharedDrives: boolean;
}

export const EMPTY_GOOGLE: GoogleForm = { clientId: "", includeSharedWithMe: false, includeSharedDrives: false };

/** `client_id`, plus Drive's two opt-ins (sent explicitly for `gdrive` only). The secret goes through `sources.secret.set`. */
export function googleSettings(kind: GoogleKind, f: GoogleForm): SourceSettings {
  const out: SourceSettings = { client_id: f.clientId.trim() };
  if (kind === "gdrive") {
    out.include_shared_with_me = f.includeSharedWithMe;
    out.include_shared_drives = f.includeSharedDrives;
  }
  return out;
}

// ---- Microsoft 365 ----

const GUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Entra application (client) IDs are GUIDs. */
export const isGuid = (s: string) => GUID.test(s.trim());

/** Same rule as the connector: `common`, `organizations`, `consumers`, a tenant GUID or a domain. */
export function isValidTenant(raw: string): boolean {
  const t = raw.trim();
  return (
    t.length > 0 &&
    t.length <= 253 &&
    !/^[.-]/.test(t) &&
    !/[.-]$/.test(t) &&
    !t.includes("..") &&
    /^[A-Za-z0-9.-]+$/.test(t)
  );
}

/** Well-known Outlook folder names (`inbox`, `sentitems`, …) or folder ids. */
export const isOutlookFolder = (s: string) => s.length <= 512 && /^[A-Za-z0-9_=+.-]+$/.test(s);

/** A OneDrive folder path from the drive root; `.` and `..` segments are refused. */
export const isDrivePath = (s: string) =>
  s.length <= 2048 && !/[\u0000-\u001f]/.test(s) && !s.split(/[/\\]/).some((seg) => seg === "." || seg === "..");

export interface MicrosoftForm {
  clientId: string;
  /** Blank: the daemon's default (`common`). */
  tenant: string;
  accountHint: string;
  /** Outlook: comma or newline separated folder names. */
  folders: string;
  /** OneDrive: comma or newline separated paths. */
  includeFolders: string;
  excludeFolders: string;
}

export const EMPTY_MICROSOFT: MicrosoftForm = {
  clientId: "",
  tenant: "",
  accountHint: "",
  folders: "",
  includeFolders: "",
  excludeFolders: "",
};

export function microsoftSettings(kind: MicrosoftKind, f: MicrosoftForm): SourceSettings {
  const out: SourceSettings = { client_id: f.clientId.trim() };
  if (f.tenant.trim()) out.tenant = f.tenant.trim();
  if (f.accountHint.trim()) out.account_hint = f.accountHint.trim();
  if (kind === "outlook") {
    const folders = splitList(f.folders);
    if (folders.length > 0) out.folders = folders;
  }
  if (kind === "onedrive") {
    const include = splitList(f.includeFolders);
    const exclude = splitList(f.excludeFolders);
    if (include.length > 0) out.include_folders = include;
    if (exclude.length > 0) out.exclude_folders = exclude;
  }
  return out;
}

export type MicrosoftErrors = Partial<Record<"clientId" | "tenant" | "folders" | "includeFolders" | "excludeFolders", MessageKey>>;

export function microsoftErrors(kind: MicrosoftKind, f: MicrosoftForm): MicrosoftErrors {
  const e: MicrosoftErrors = {};
  if (!f.clientId.trim()) e.clientId = "addSource.microsoft.clientIdMissing";
  else if (!isGuid(f.clientId)) e.clientId = "addSource.microsoft.clientIdInvalid";
  if (f.tenant.trim() && !isValidTenant(f.tenant)) e.tenant = "addSource.microsoft.tenantInvalid";
  if (kind === "outlook" && !splitList(f.folders).every(isOutlookFolder)) e.folders = "addSource.microsoft.foldersInvalid";
  if (kind === "onedrive") {
    if (!splitList(f.includeFolders).every(isDrivePath)) e.includeFolders = "addSource.microsoft.pathInvalid";
    if (!splitList(f.excludeFolders).every(isDrivePath)) e.excludeFolders = "addSource.microsoft.pathInvalid";
  }
  return e;
}

/** The Microsoft Graph permission each kind asks for (plus `offline_access` and `User.Read`). */
export const MICROSOFT_SCOPE: Record<MicrosoftKind, string> = {
  outlook: "Mail.Read",
  "outlook-calendar": "Calendars.Read",
  onedrive: "Files.Read",
};

// ---- SaaS ----

/** `token`: paste a token. `oauth`: the user's own OAuth app (`client_id`). */
export type SaasMethod = "token" | "oauth";

export interface SaasForm {
  method: SaasMethod;
  clientId: string;
  /** Slack: channel ids or names. */
  conversations: string;
  includeDms: boolean;
  includeChannels: boolean;
  /** Notion. */
  includeDatabases: boolean;
  /** GitHub: `owner/name`, comma or newline separated. */
  repos: string;
  allActivity: boolean;
  includeDiscussions: boolean;
  /** Linear. */
  include: LinearInclude[];
  teams: string;
}

export type LinearInclude = "assigned" | "created" | "subscribed";
export const LINEAR_INCLUDE: readonly LinearInclude[] = ["assigned", "created", "subscribed"];

export const EMPTY_SAAS: SaasForm = {
  method: "token",
  clientId: "",
  conversations: "",
  includeDms: true,
  includeChannels: true,
  includeDatabases: true,
  repos: "",
  allActivity: false,
  includeDiscussions: true,
  include: [...LINEAR_INCLUDE],
  teams: "",
};

/** Kinds that can connect through the user's own OAuth app instead of a pasted token. */
export const supportsOAuth = (kind: SaasKind) => kind === "github" || kind === "linear";

const usesOAuth = (kind: SaasKind, f: SaasForm) => supportsOAuth(kind) && f.method === "oauth";

export const isRepo = (s: string) => /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(s) && !s.split("/").some((p) => p === "." || p === "..");
export const isTeamKey = (s: string) => /^[A-Za-z0-9_]+$/.test(s);

/** Values that differ from the connector's defaults only; `client_id` only in OAuth mode. */
export function saasSettings(kind: SaasKind, f: SaasForm): SourceSettings {
  const out: SourceSettings = {};
  if (usesOAuth(kind, f) && f.clientId.trim()) out.client_id = f.clientId.trim();
  switch (kind) {
    case "slack": {
      const conversations = splitList(f.conversations);
      if (conversations.length > 0) out.conversations = conversations;
      if (!f.includeDms) out.include_dms = false;
      if (!f.includeChannels) out.include_channels = false;
      break;
    }
    case "notion":
      if (!f.includeDatabases) out.include_databases = false;
      break;
    case "github": {
      const repos = splitList(f.repos);
      if (repos.length > 0) {
        out.repos = repos;
        if (f.allActivity) out.all_activity = true;
      }
      if (!f.includeDiscussions) out.include_discussions = false;
      break;
    }
    case "linear": {
      const include = LINEAR_INCLUDE.filter((i) => f.include.includes(i));
      if (include.length > 0 && include.length < LINEAR_INCLUDE.length) out.include = include;
      const teams = splitList(f.teams);
      if (teams.length > 0) out.teams = teams;
      break;
    }
  }
  return out;
}

export type SaasErrors = Partial<Record<"clientId" | "token" | "conversations" | "repos" | "include" | "teams", MessageKey>>;

/**
 * Form errors. `token` is the pasted value (checked only in token mode, and only when nothing
 * was saved yet); it is read here and never kept.
 */
export function saasErrors(kind: SaasKind, f: SaasForm, token: string, tokenSaved: boolean): SaasErrors {
  const e: SaasErrors = {};
  if (usesOAuth(kind, f)) {
    if (!f.clientId.trim()) e.clientId = "addSource.saas.clientIdMissing";
    else if (/[\s\u0000-\u001f]/.test(f.clientId.trim()) || f.clientId.trim().length > 512) e.clientId = "addSource.saas.clientIdInvalid";
  } else if (!token.trim() && !tokenSaved) {
    e.token = "addSource.saas.tokenMissing";
  } else if (kind === "slack" && token.trim().startsWith("xoxb-")) {
    e.token = "addSource.slack.botToken";
  }
  switch (kind) {
    case "slack":
      if (!f.includeDms && !f.includeChannels && splitList(f.conversations).length === 0) e.conversations = "addSource.slack.nothing";
      break;
    case "github":
      if (!splitList(f.repos).every(isRepo)) e.repos = "addSource.github.reposInvalid";
      break;
    case "linear":
      if (f.include.length === 0) e.include = "addSource.linear.includeMissing";
      if (!splitList(f.teams).every(isTeamKey)) e.teams = "addSource.linear.teamsInvalid";
      break;
  }
  return e;
}
