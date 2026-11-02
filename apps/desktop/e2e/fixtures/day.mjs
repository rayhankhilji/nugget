// A believable working day for the brand screenshots (e2e/brand-screens.mjs): one person at a small
// design studio, signed in to Heed Cloud, on Tuesday 3 November 2026 at 08:14 in London.
// Every name, company, subject and address here is fictional.

export const NOW = "2026-11-03T08:14:00Z";
export const TIMEZONE = "Europe/London";
export const ME = "sam@okafor.studio";

const at = (hhmm, day = "2026-11-03") => `${day}T${hhmm}:00Z`;
const ago = (mins) => new Date(Date.parse(NOW) - mins * 60_000).toISOString();

/** Generic monochrome glyphs (an envelope, a calendar, a hash…), drawn here: never the apps' own marks. */
const glyph = (body, color) =>
  `data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="${color}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`)}`;
const MAIL = '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="m3 7 9 6 9-6"/>';
const CAL = '<rect x="3" y="5" width="18" height="16" rx="2"/><path d="M3 10h18M8 3v4M16 3v4"/>';
const HASH = '<path d="M5 9h15M4 15h15M10 3 8 21M16 3l-2 18"/>';
const PEOPLE = '<circle cx="9" cy="8" r="3.5"/><path d="M3 20c0-3.3 2.7-6 6-6s6 2.7 6 6"/><path d="M16 4.5a3.5 3.5 0 0 1 0 7M18 14c1.8.8 3 2.6 3 4.7"/>';
const PAGE = '<path d="M6 3h9l4 4v14H6z"/><path d="M14 3v5h5M9 13h7M9 17h5"/>';
const DRIVE = '<path d="M8 4h8l6 10-4 6H6l-4-6z"/><path d="M8 4l6 10h8M2 14h12l-4 6"/>';

export const APPS = [
  { slug: "gmail", name: "Gmail", logo: glyph(MAIL, "#C5221F"), connectable: true },
  { slug: "googlecalendar", name: "Google Calendar", logo: glyph(CAL, "#1A73E8"), connectable: true },
  { slug: "outlook", name: "Outlook", logo: glyph(MAIL, "#0F6CBD"), connectable: true },
  { slug: "slack", name: "Slack", logo: glyph(HASH, "#4A154B"), connectable: true },
  { slug: "microsoft_teams", name: "Microsoft Teams", logo: glyph(PEOPLE, "#5B5FC7"), connectable: true },
  { slug: "notion", name: "Notion", logo: glyph(PAGE, "#191919"), connectable: true },
  { slug: "googledrive", name: "Google Drive", logo: glyph(DRIVE, "#188038"), connectable: true },
];

export const CONNECTIONS = {
  connections: [
    { id: "c1", toolkit: "gmail", status: "ACTIVE", last_synced_at: ago(3), last_error: null, backfill_done: true, items_synced: 18_406 },
    { id: "c2", toolkit: "googlecalendar", status: "ACTIVE", last_synced_at: ago(3), last_error: null, backfill_done: true, items_synced: 2_214 },
    { id: "c3", toolkit: "slack", status: "ACTIVE", last_synced_at: ago(1), last_error: null, syncing: true, backfill_done: false, items_synced: 6_380 },
    { id: "c4", toolkit: "notion", status: "ACTIVE", last_synced_at: ago(42), last_error: null, backfill_done: true, items_synced: 911 },
  ],
  available: APPS,
};

const ev = (source, key, quote) => ({ item: { source, key }, quote, url: null });
const card = (id, kind, title, summary, extra = {}) => ({
  id,
  kind,
  title,
  detail: "",
  summary,
  people: [],
  due: null,
  score: 1,
  rank: 1,
  factors: [],
  evidence: [],
  actions: [],
  state: "seen",
  sensitive: false,
  created_at: ago(90),
  ...extra,
});

export const CARDS = [
  card("k1", "unanswered_thread", "Reply to Priya Raman about the Halcyon launch date", "Priya asked on Friday whether 14 November still holds for the packaging launch. Print proofs land Thursday.", {
    people: ["Priya Raman"],
    due: at("12:00"),
    rank: 1,
    evidence: [ev("gmail", "m-4471", "Does the 14th still hold on your side? Retail need a yes by Tuesday lunch.")],
    actions: [{ index: 0, kind: "draft_reply", label: "Draft a reply", risk: "send", supported: true }],
  }),
  card("k2", "meeting_prep", "Prep for the Harbourline quarterly review at 10:30", "The deck is in Drive. Jonas left three comments on slide 7 last night about the Q4 retainer numbers.", {
    people: ["Jonas Lindqvist", "Mei Tanaka"],
    due: at("10:30"),
    rank: 2,
    evidence: [ev("googledrive", "d-qbr", "Slide 7: can we show retainer hours against plan, not just spend?")],
    actions: [{ index: 0, kind: "open", label: "Open the deck", risk: "read", supported: true }],
  }),
  card("k3", "commitment", "Send Tom Becker the revised statement of work", "You told Tom on Thursday you would send it by Monday. The draft is in Notion with two open questions.", {
    people: ["Tom Becker"],
    due: at("17:00", "2026-11-02"),
    rank: 3,
    evidence: [ev("slack", "s-7781", "I'll get the revised SOW over by Monday, promise.")],
    actions: [{ index: 0, kind: "draft_reply", label: "Draft the email", risk: "send", supported: true }],
  }),
  card("k4", "follow_up", "Chase Aiyana Brooks for the signed NDA", "You sent it six days ago and haven't heard back. The Kestrel kickoff is on Monday.", {
    people: ["Aiyana Brooks"],
    rank: 4,
    evidence: [ev("gmail", "m-4390", "Attached is the mutual NDA ahead of the Kestrel kickoff.")],
    actions: [{ index: 0, kind: "draft_reply", label: "Draft a nudge", risk: "send", supported: true }],
  }),
  card("k5", "renewal", "Figma renews in 3 days: £1,140 a year", "12 seats. Four people haven't opened Figma since September.", {
    due: at("09:00", "2026-11-06"),
    rank: 5,
    evidence: [ev("gmail", "m-4402", "Your Figma Professional plan renews on 6 November.")],
  }),
];

export const BRIEF = {
  day: "2026-11-03",
  timezone: TIMEZONE,
  prose:
    "Three people are waiting on you this morning. Priya needs a yes on the 14 November launch by lunch [1], and Tom is still waiting for the revised SOW you promised for Monday [2]. The Harbourline review is at 10:30; Jonas wants retainer hours shown against plan on slide 7 [3]. Your afternoon is clear after 15:00.",
  citations: [
    { n: 1, item_id: "m-4471", title: "Re: Halcyon launch timing", source: "gmail", url: "https://mail.google.com/mail/u/0/#inbox/4471", occurred_at: ago(3 * 24 * 60), snippet: "Does the 14th still hold on your side?", author: "Priya Raman" },
    { n: 2, item_id: "s-7781", title: "#halcyon-sow", source: "slack", url: "https://okafor.slack.com/archives/C7781", occurred_at: ago(5 * 24 * 60), snippet: "I'll get the revised SOW over by Monday", author: "Tom Becker" },
    { n: 3, item_id: "d-qbr", title: "Harbourline Q4 review", source: "googledrive", url: "https://docs.google.com/presentation/d/qbr", occurred_at: ago(11 * 60), snippet: "Slide 7: retainer hours against plan", author: "Jonas Lindqvist" },
  ],
  brief: {
    date: "2026-11-03",
    generated_at: ago(14),
    top: [],
    meetings: [
      { title: "Harbourline quarterly review", start: at("10:30"), end: at("11:30"), all_day: false, location: "Google Meet", attendees: ["Jonas Lindqvist", "Mei Tanaka"], external: ["Jonas Lindqvist"], prep: [] },
      { title: "Studio stand-up", start: at("13:00"), end: at("13:15"), all_day: false, location: null, attendees: ["Leah", "Marco", "Dev"], external: [], prep: [] },
      { title: "Halcyon print check", start: at("14:00"), end: at("14:45"), all_day: false, location: "Pressworks, Hackney", attendees: ["Priya Raman"], external: ["Priya Raman"], prep: [] },
    ],
    overdue: [],
    renewals: [],
    open_cards: 5,
  },
  created_at: ago(14),
};

const action = (id, kind, toolkit, summary, fields, body, mins) => ({
  id,
  kind,
  toolkit,
  status: "pending",
  risk: "send",
  decision: "confirm",
  reason: null,
  summary,
  preview: { fields, diff: body ? [{ field: "Body", before: null, after: body }] : [] },
  digest: `sha-${id}`,
  idempotency_key: `k-${id}`,
  created_at: ago(mins),
  expires_at: new Date(Date.parse(NOW) + 22 * 3600_000).toISOString(),
  decided_at: null,
  executed_at: null,
  result: null,
  error: null,
  snoozed_until: null,
  revision: 1,
  editable: EDITABLE[kind] ?? [],
});

const EDITABLE = {
  send_mail: ["to", "cc", "bcc", "subject", "body"],
  reply_mail: ["to", "body"],
  create_event: ["title", "start", "end", "timezone", "attendees", "location", "description"],
  update_event: ["title", "start", "end", "timezone", "attendees", "location", "description"],
  post_message: ["text"],
  create_page: ["title", "markdown"],
};

export const ACTIONS = [
  action(
    "a1",
    "reply_mail",
    "gmail",
    "Reply to Priya Raman: 14 November still holds",
    [
      { label: "To", value: "priya.raman@halcyonfoods.co.uk" },
      { label: "Subject", value: "Re: Halcyon launch timing" },
    ],
    "Hi Priya, yes, the 14th still holds. Print proofs land Thursday and I'll send photos the moment they're in. Sam",
    6,
  ),
  action(
    "a2",
    "create_event",
    "googlecalendar",
    "Book 30 minutes with Tom Becker to walk through the SOW",
    [
      { label: "When", value: "Wed 4 Nov, 15:00–15:30" },
      { label: "Guests", value: "tom.becker@kestrel.io" },
    ],
    null,
    9,
  ),
  action(
    "a3",
    "post_message",
    "slack",
    "Tell #harbourline the review deck is final",
    [{ label: "Channel", value: "#harbourline" }],
    "Deck for the 10:30 is final: retainer hours now sit against plan on slide 7.",
    12,
  ),
];

const ENT = {
  plan: "heed",
  name: "Heed",
  max_connections: 10,
  sync_interval_minutes: 5,
  history_days: null,
  smart_asks_per_month: 300,
  default_model: "smart",
  devices: 3,
  ghost_text: true,
  needs_you_inbox: true,
  daily_brief: true,
  actions_with_approval: true,
  agent_tasks: false,
  computer_use: false,
  priority_support: false,
};

export const ME_VIEW = {
  user_id: "u-sam",
  email: ME,
  plan: "heed",
  embeddings: true,
  subscription: { plan: "heed", status: "trialing", interval: "month", trial_end: "2026-11-12T00:00:00Z", current_period_end: "2026-11-12T00:00:00Z", cancel_at_period_end: false },
  entitlements: ENT,
  usage: { month_cost_micros: 2_140_000, smart_asks_used: 212 },
};

export const PLANS = {
  plans: [
    { id: "heed", name: "Heed", monthly_gbp: 39, annual_gbp: 390, trial_days: 14, entitlements: ENT },
    { id: "heed_max", name: "Heed Max", monthly_gbp: 99, annual_gbp: 990, trial_days: 14, entitlements: { ...ENT, plan: "heed_max", name: "Heed Max", smart_asks_per_month: 900, agent_tasks: true, computer_use: true, priority_support: true } },
  ],
};

export const SEARCH = {
  mode: "hybrid",
  query: "harbourline retainer",
  dates: { from: null, to: null, phrase: null },
  results: [
    { item_id: "d-qbr", source: "googledrive", kind: "doc", title: "Harbourline Q4 review", author: "Jonas Lindqvist", url: "https://docs.google.com/presentation/d/qbr", occurred_at: ago(11 * 60), snippet: "Slide 7: can we show retainer hours against plan, not just spend? Q4 is tracking 18% over on design QA.", score: 0.92 },
    { item_id: "m-4302", source: "gmail", kind: "mail", title: "Retainer for 2027", author: "Mei Tanaka", url: "https://mail.google.com/mail/u/0/#inbox/4302", occurred_at: ago(6 * 24 * 60), snippet: "We'd like to keep the retainer at 60 hours a month and add a quarterly brand audit.", score: 0.88 },
    { item_id: "s-7710", source: "slack", kind: "message", title: "#harbourline", author: "Marco", url: "https://okafor.slack.com/archives/C7710", occurred_at: ago(2 * 24 * 60), snippet: "Logged 71 hours on Harbourline in October; the overage was all packaging QA.", score: 0.81 },
    { item_id: "n-221", source: "notion", kind: "page", title: "Harbourline: account plan", author: "Sam Okafor", url: "https://www.notion.so/okafor/harbourline", occurred_at: ago(20 * 24 * 60), snippet: "Retainer renews 1 January. Levers: audit add-on, rate review, fewer ad hoc requests.", score: 0.74 },
    { item_id: "e-88", source: "googlecalendar", kind: "event", title: "Harbourline quarterly review", author: null, url: null, occurred_at: at("10:30"), snippet: "Jonas Lindqvist, Mei Tanaka · Google Meet", score: 0.7 },
  ],
  total: 5,
  facets: {
    sources: [
      { value: "gmail", count: 1 },
      { value: "slack", count: 1 },
      { value: "googledrive", count: 1 },
      { value: "notion", count: 1 },
      { value: "googlecalendar", count: 1 },
    ],
    people: [
      { value: "Jonas Lindqvist", count: 2 },
      { value: "Mei Tanaka", count: 2 },
      { value: "Marco", count: 1 },
    ],
  },
  timeline: { bucket: "day", points: [] },
};

/** The quick-ask answer, streamed through the chat events. */
export const QUICK_ASK = {
  question: "When is the Halcyon launch and what's left?",
  started: {
    stream_id: "qa-1",
    provider: "Heed Cloud",
    locality: "remote",
    citations: [
      { n: 1, item: { source: "gmail", key: "m-4471" }, chunk_id: 1, title: "Re: Halcyon launch timing", url: "https://mail.google.com/mail/u/0/#inbox/4471", date: ago(3 * 24 * 60) },
      { n: 2, item: { source: "notion", key: "n-310" }, chunk_id: 2, title: "Halcyon: launch checklist", url: "https://www.notion.so/okafor/halcyon", date: ago(26 * 60) },
      { n: 3, item: { source: "googlecalendar", key: "e-91" }, chunk_id: 3, title: "Halcyon print check", url: null, date: at("14:00") },
    ],
    excluded_sensitive: 0,
  },
  answer:
    "The Halcyon packaging launch is set for Friday 14 November [1]. Three things are left: confirming the date with Priya (she needs a yes by lunch today) [1], signing off the print proofs at today's 14:00 check at Pressworks [3], and the retailer shelf mock-ups, which are still marked in progress on the checklist [2].",
};

/** Every Heed Cloud command the app calls, answered from the fixture. */
export function cloudFixture() {
  let actions = [...ACTIONS];
  return {
    cloud_session: () => ({ configured: true, signed_in: true, email: ME }),
    cloud_cards: () => ({ cards: CARDS, last_refresh: ago(4) }),
    cloud_cards_refresh: () => ({ cards: CARDS, last_refresh: NOW }),
    cloud_cards_seen: () => null,
    cloud_brief: () => BRIEF,
    cloud_apps: () => APPS.map((a) => ({ ...a, category: "mail", syncs: true, actions: [], sync_mode: "poll", connected: CONNECTIONS.connections.some((c) => c.toolkit === a.slug) })),
    cloud_connections_list: () => CONNECTIONS,
    cloud_actions_list: () => actions,
    cloud_action_approve: (a) => {
      actions = actions.filter((x) => x.id !== a?.id);
      return { ...ACTIONS[0], status: "succeeded" };
    },
    cloud_action_cancel: (a) => {
      actions = actions.filter((x) => x.id !== a?.id);
      return null;
    },
    cloud_me: () => ME_VIEW,
    cloud_billing_plans: () => PLANS,
    cloud_billing_preview: () => ({ plan: "heed_max", interval: "month", amount_due: 0, currency: "gbp", proration_date: NOW }),
    cloud_search: () => SEARCH,
    retrieve_ask: () => QUICK_ASK.started,
  };
}
