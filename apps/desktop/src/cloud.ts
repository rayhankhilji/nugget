/**
 * Heed cloud: typed wrappers for the shell's `cloud_*` commands (`src-tauri/src/cloud.rs`).
 * Tokens never reach the page: sign-in finishes in the shell, which then emits
 * {@link CLOUD_SESSION_EVENT} without any secret.
 */
import { invoke, type InvokeArgs } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ApiError, inTauri, type BriefView, type CardView, type DismissReason } from "./api";
import { t } from "./i18n";

export interface CloudSession {
  /** False when this build has no cloud configuration. */
  configured: boolean;
  signed_in: boolean;
  email: string | null;
}

export interface CloudConnection {
  id: string;
  toolkit: string;
  /** The connection's status: `active`, `initiated`, `failed` or `disabled`. */
  status: string;
  last_synced_at: string | null;
  last_error: string | null;
  /** A sync is running in the cloud now. */
  syncing?: boolean;
  /** The first import has finished. */
  backfill_done?: boolean;
  items_synced?: number;
}

export interface CloudApp {
  slug: string;
  name: string;
  logo: string | null;
  connectable: boolean;
}

export interface CloudConnections {
  connections: CloudConnection[];
  available: CloudApp[];
}

export interface CloudSyncResult {
  toolkit: string;
  added: number;
  updated: number;
  /** Items removed because they were deleted upstream (absent from older servers). */
  deleted?: number;
  error: string | null;
}

export interface CloudCitation {
  n: number;
  item_id: string;
  title: string | null;
  source: string;
  url: string | null;
  occurred_at: string | null;
  snippet: string;
}

export interface CloudAnswer {
  conversation_id: string;
  /** Contains `[n]` markers for {@link CloudCitation.n}. */
  answer: string;
  citations: CloudCitation[];
  usage: { input_tokens: number; output_tokens: number };
  /** The tier that answered (absent from older servers). */
  model_tier?: CloudModelTier | null;
  /** A smart request answered by the fast tier: this month's smart allowance is used up. */
  downgraded?: boolean;
}

export type CloudModelTier = "fast" | "smart";
export type BillingInterval = "month" | "year";

/** What a plan includes; `null` limits are unlimited. */
export interface CloudEntitlements {
  plan: string;
  name: string;
  max_connections: number | null;
  sync_interval_minutes: number;
  history_days: number | null;
  smart_asks_per_month: number;
  default_model: string;
  devices: number;
  ghost_text: boolean;
  needs_you_inbox: boolean;
  daily_brief: boolean;
  actions_with_approval: boolean;
  agent_tasks: boolean;
  computer_use: boolean;
  priority_support: boolean;
}

export interface CloudSubscription {
  plan: string;
  /** Stripe's status: `trialing`, `active`, `past_due`, `canceled`, ... */
  status: string;
  interval: string;
  trial_end: string | null;
  current_period_end: string | null;
  cancel_at_period_end: boolean;
}

export interface CloudMe {
  user_id: string;
  email: string | null;
  plan: string | null;
  embeddings: boolean;
  /** False on a self-hosted server with billing off (everyone is entitled). */
  billing?: boolean;
  subscription: CloudSubscription | null;
  entitlements: CloudEntitlements | null;
  usage: { month_cost_micros: number; smart_asks_used: number };
}

export interface CloudPlan {
  id: string;
  name: string;
  monthly_gbp: number;
  annual_gbp: number;
  trial_days: number;
  entitlements: CloudEntitlements;
}

export interface CloudSearchRequest {
  /** Words; may hold a date phrase ("invoices last week"). Empty browses newest first. */
  q: string;
  sources?: string[];
  /** A name or address: sender, recipients or attendees. */
  person?: string;
  /** RFC 3339 bounds; when unset a date phrase in `q` is used. */
  from?: string;
  to?: string;
  /** 1 to 50 (default 20). */
  limit?: number;
}

export interface CloudSearchResult {
  item_id: string;
  source: string;
  /** `email`, `event` or `doc`. */
  kind: string;
  title: string | null;
  author: string | null;
  url: string | null;
  occurred_at: string | null;
  /** Untrusted source text: render as plain text. */
  snippet: string;
  score: number;
}

export interface CloudFacetCount {
  value: string;
  count: number;
}

export interface CloudSearchResponse {
  /** `hybrid`, `text` (no embeddings) or `browse` (no words). */
  mode: "hybrid" | "text" | "browse";
  /** The words searched, after any date phrase was taken out. */
  query: string;
  dates: { from: string | null; to: string | null; phrase: string | null };
  results: CloudSearchResult[];
  /** Items matching the words and filters (facets and the timeline count these). */
  total: number;
  facets: { sources: CloudFacetCount[]; people: CloudFacetCount[] };
  timeline: { bucket: "day" | "month"; points: { start: string; count: number }[] };
}

/** "Needs you": the daemon's card shape (`CardView`). An action is `supported` when the cloud can
 * propose it (its app is connected); accepting the card then proposes it ({@link CloudCardOutcome}). */
export interface CloudCards {
  cards: CardView[];
  last_refresh: string | null;
  /** Only from {@link cloudApi.refreshCards}. */
  report?: unknown;
}

/** A card after an action; `action` is the proposal accepting it made: show its preview and
 * approve it with {@link cloudApi.approveAction} and its `digest`. Nothing is sent before that. */
export interface CloudCardOutcome {
  card: CardView | null;
  action?: CloudAction | null;
}

export interface CloudBrief {
  /** The user's local date, `YYYY-MM-DD`. */
  day: string;
  timezone: string;
  /** `[n]` markers refer to `citations`; null without a model or AI allowance. */
  prose: string | null;
  citations: CloudCitation[];
  brief: BriefView;
  created_at: string;
}

/** This device's IANA zone (e.g. `Europe/London`), sent so "today" is the user's. */
export function deviceTimeZone(): string | undefined {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || undefined;
  } catch {
    return undefined;
  }
}

/** Display names for the cloud's app catalogue (crates/nugget-cloud/src/apps); anything else is title-cased. */
const APP_NAMES: Record<string, string> = {
  gmail: "Gmail",
  googlecalendar: "Google Calendar",
  outlook: "Outlook",
  slack: "Slack",
  microsoft_teams: "Microsoft Teams",
  notion: "Notion",
  googledrive: "Google Drive",
};

export function appName(slug: string): string {
  return APP_NAMES[slug] ?? slug.charAt(0).toUpperCase() + slug.slice(1);
}

export type CloudAppCategory = "mail" | "calendar" | "chat" | "docs" | "files";

/** An app in the catalogue (`cloud_apps`): what it syncs, which actions it can take. */
export interface CloudAppInfo {
  slug: string;
  name: string;
  /** A `data:` URI, safe under the desktop CSP (`img-src 'self' data:`). */
  logo: string | null;
  /** The remote logo URL (for the website, not the desktop). */
  logo_url?: string;
  /** One of {@link CloudAppCategory}; other strings are possible from a newer server. */
  category: CloudAppCategory | (string & {});
  syncs: boolean;
  /** Action kinds this app supports (see {@link CloudActionKind}). */
  actions: string[];
  sync_mode: "poll" | "trigger" | (string & {});
  connectable: boolean;
  connected: boolean;
}

export type CloudActionKind =
  | "send_mail"
  | "reply_mail"
  | "create_event"
  | "update_event"
  | "post_message"
  | "create_page";

export type CloudActionStatus =
  | "pending"
  | "executing"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "expired"
  | "denied"
  /** A status this build doesn't know (from a newer server). */
  | "unknown";

export interface CloudActionPreviewField {
  label: string;
  value: string;
}

export interface CloudActionPreviewDiff {
  field: string;
  before: string | null;
  after: string | null;
}

export interface CloudActionPreview {
  fields: CloudActionPreviewField[];
  diff: CloudActionPreviewDiff[];
}

/** A proposed write to a connected app; nothing is sent until it is approved. */
export interface CloudAction {
  id: string;
  kind: CloudActionKind;
  toolkit: string;
  status: CloudActionStatus;
  /** e.g. `send`. */
  risk: string;
  /** The policy's verdict. */
  decision: "confirm" | "deny";
  reason: string | null;
  summary: string;
  preview: CloudActionPreview;
  /** Hex SHA-256 of what will run; {@link cloudApi.approveAction} must echo it. */
  digest: string;
  idempotency_key: string;
  created_at: string;
  expires_at: string;
  decided_at: string | null;
  executed_at: string | null;
  result: unknown;
  error: string | null;
}

export interface ProposeCloudAction {
  kind: CloudActionKind;
  toolkit: string;
  /** The kind's input object, e.g. send_mail `{to, cc?, bcc?, subject, body}`. */
  input: Record<string, unknown>;
  /** 1-128 chars of `[A-Za-z0-9_\-:.]`; reusing one returns the same action. */
  idempotencyKey: string;
}

/** Sign in with Apple is not in M1 (no Apple Developer account yet). */
export type SignInProvider = "google";

/** `nugget:cloud-session`: the session changed. */
export interface CloudSessionEvent {
  signed_in: boolean;
  error: string | null;
}

export const CLOUD_SESSION_EVENT = "nugget:cloud-session";
/** No payload: Composio sent the browser back; reload the connections. */
export const CLOUD_CONNECTIONS_EVENT = "nugget:cloud-connections";
/** No payload: Stripe Checkout sent the browser back; reload the account and plans. */
export const CLOUD_BILLING_EVENT = "nugget:cloud-billing";

/** A cloud error: the shell's `ApiError` plus the cloud's own code (`not_signed_in`, ...). */
export class CloudError extends ApiError {
  readonly cloud: string | null;
  constructor(err: unknown) {
    const base = ApiError.from(err);
    super({ kind: base.kind, message: base.message, code: base.code });
    const c = typeof err === "object" && err !== null ? (err as { cloud?: unknown }).cloud : undefined;
    this.cloud = typeof c === "string" ? c : null;
  }
  get notSignedIn(): boolean {
    return this.cloud === "not_signed_in";
  }
}

async function call<T>(command: string, args?: InvokeArgs): Promise<T> {
  if (!inTauri()) throw new ApiError({ kind: "unavailable", message: t("common.notInApp") });
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw new CloudError(e);
  }
}

async function on<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  return listen<T>(event, (e) => handler(e.payload));
}

function isSessionEvent(p: unknown): p is CloudSessionEvent {
  return typeof p === "object" && p !== null && typeof (p as CloudSessionEvent).signed_in === "boolean";
}

export const cloudApi = {
  session: () => call<CloudSession>("cloud_session"),
  /** Opens the browser; the sign-in finishes when the link comes back ({@link CLOUD_SESSION_EVENT}). */
  signInWith: (provider: SignInProvider) => call<void>("cloud_sign_in", { provider }),
  /** Emails a magic link; the sign-in finishes when it is opened on this computer. */
  signInEmail: (email: string) => call<void>("cloud_sign_in", { email }),
  signOut: () => call<{ revoked: boolean }>("cloud_sign_out"),
  connections: () => call<CloudConnections>("cloud_connections_list"),
  /** Opens the app's consent page in the browser. */
  connect: (toolkit: string) => call<void>("cloud_connect", { toolkit }),
  disconnect: (id: string) => call<void>("cloud_disconnect", { id }),
  sync: () => call<{ synced: CloudSyncResult[]; running?: boolean }>("cloud_sync"),
  ask: (question: string, conversationId?: string | null, model?: CloudModelTier) =>
    call<CloudAnswer>("cloud_ask", { question, ...(conversationId ? { conversationId } : {}), ...(model ? { model } : {}) }),
  me: () => call<CloudMe>("cloud_me"),
  plans: () => call<{ plans: CloudPlan[] }>("cloud_billing_plans"),
  /** Opens Stripe Checkout in the browser; it comes back with {@link CLOUD_BILLING_EVENT}. */
  checkout: (plan: string, interval: BillingInterval) => call<void>("cloud_billing_checkout", { plan, interval }),
  /** Opens the Stripe customer portal in the browser. */
  portal: () => call<void>("cloud_billing_portal"),
  /** Deletes the account and all its cloud data, then signs this computer out. Not undoable. */
  deleteAccount: () => call<void>("cloud_delete_account"),
  search: (request: CloudSearchRequest) => call<CloudSearchResponse>("cloud_search", { request: { ...request, tz: deviceTimeZone() } }),
  cards: () => call<CloudCards>("cloud_cards", { tz: deviceTimeZone() }),
  /** Runs knowledge and cards now (slow: seconds). */
  refreshCards: () => call<CloudCards>("cloud_cards_refresh", { tz: deviceTimeZone() }),
  cardsSeen: (ids: string[]) => call<void>("cloud_cards_seen", { ids }),
  /** Accepts the card and proposes its action (`index`, else the first supported one); `body` is
   * the reply text when the user wrote it (required for sensitive cards). */
  acceptCard: (id: string, opts?: { index?: number; body?: string }) =>
    call<CloudCardOutcome>("cloud_card_action", { id, action: "accept", index: opts?.index ?? null, body: opts?.body ?? null }),
  doneCard: (id: string) => call<CloudCardOutcome>("cloud_card_action", { id, action: "done" }),
  /** `note` is kept for `other`. */
  dismissCard: (id: string, reason: DismissReason, note?: string) =>
    call<CloudCardOutcome>("cloud_card_action", { id, action: "dismiss", reason, note: note ?? null }),
  /** `until`: RFC 3339, within the next year. */
  snoozeCard: (id: string, until: string) => call<CloudCardOutcome>("cloud_card_action", { id, action: "snooze", until }),
  /** Today's brief from 07:00 local (generated once), else the latest earlier one, else null. */
  brief: () => call<CloudBrief | null>("cloud_brief", { tz: deviceTimeZone() }),
  apps: () => call<CloudAppInfo[]>("cloud_apps"),
  actions: (status?: CloudActionStatus) =>
    call<CloudAction[]>("cloud_actions_list", status ? { status } : {}),
  proposeAction: ({ kind, toolkit, input, idempotencyKey }: ProposeCloudAction) =>
    call<CloudAction>("cloud_action_propose", { kind, toolkit, input, idempotencyKey }),
  /** A dry run: what the action would send, with no side effects. */
  actionPreview: (id: string) => call<CloudAction>("cloud_action_preview", { id }),
  actionStatus: (id: string) => call<CloudAction>("cloud_action_status", { id }),
  approveAction: (id: string, digest: string) => call<CloudAction>("cloud_action_approve", { id, digest }),
  cancelAction: (id: string) => call<CloudAction>("cloud_action_cancel", { id }),
  /** Malformed payloads are dropped. */
  onSession: (h: (e: CloudSessionEvent) => void) =>
    on<unknown>(CLOUD_SESSION_EVENT, (p) => {
      if (isSessionEvent(p)) h({ signed_in: p.signed_in, error: typeof p.error === "string" ? p.error : null });
    }),
  onConnectionsChanged: (h: (payload: unknown) => void) => on<unknown>(CLOUD_CONNECTIONS_EVENT, h),
  onBillingChanged: (h: (payload: unknown) => void) => on<unknown>(CLOUD_BILLING_EVENT, h),
};
