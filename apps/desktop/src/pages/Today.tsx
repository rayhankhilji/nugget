/**
 * Home and Needs you share this file. Home: the ask composer and the morning brief. Needs you:
 * the card inbox as a list with a reading pane. Each card says why it is here (top score
 * factors, evidence citations) and offers accept / dismiss (with a reason) / snooze, plus
 * actions that run only after a dry-run preview is approved (see `today/ActionDialog`).
 *
 * Everything a card shows comes from indexed mail, calendars and notes: it is rendered as plain
 * text, and links are offered only for `https:` (opened by the OS, never in the webview).
 */
import { Fragment, createContext, useContext, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import {
  api,
  DISMISS_REASONS,
  errorMessage,
  learningApi,
  proactiveApi,
  proactiveEvents,
  type BriefCardView,
  type BriefMeetingView,
  type BriefView,
  type CardActionView,
  type CardEvidence,
  type CardFeedbackRequest,
  type CardView,
  type DismissReason,
  type ResetLearningRequest,
  type SourceInfo,
} from "../api";
import { useAsync, useEvent, useNow, useOpener } from "../hooks";
import { comboAria, comboLabel, hasMod, isComposing, isTypingTarget, letterOf, type Combo } from "../hooks.shortcuts";
import { formatDatesInText, formatRelative, formatTime } from "../format";
import { GoToPage } from "../components/GoToPage";
import { SourceBadge } from "../components/Provenance";
import { Badge, EmptyState, ErrorNote, Loading, PageHeader, Spinner } from "../components/ui";
import { formatList, formatNumber, useT, type MessageKey } from "../i18n";
import { ActionDialog } from "./today/ActionDialog";
import { ActionQueue } from "./today/ActionQueue";
import { BriefProse } from "./today/BriefProse";
import { cloudApi, type CloudBrief } from "../cloud";
import { useCloudSignedIn } from "../cloudSignedIn";
import { HomeComposer } from "./today/HomeComposer";
import { LearnedLines, LearningPanel, type Forget } from "./today/Learning";
import {
  SNOOZE_CHOICES,
  cardKindLabel,
  localText,
  evidenceLine,
  factorLabel,
  formatBriefDate,
  formatClock,
  snoozeUntil,
  topFactors,
  type SnoozeChoice,
} from "./today/model";
import "./today/today.css";

/** Source id → source, for evidence badges (empty while unknown). */
const SourcesContext = createContext<ReadonlyMap<string, SourceInfo>>(new Map());

/** Short verb labels for known action kinds; the daemon's full label stays in the tooltip. */
const ACTION_LABEL: Record<string, MessageKey> = {
  draft_reply: "today.actionKind.draft_reply",
  create_event: "today.actionKind.create_event",
  reminder: "today.actionKind.reminder",
  archive_thread: "today.actionKind.archive_thread",
  research_note: "today.actionKind.research_note",
};

export function HomePage() {
  return <TodayPage view="home" />;
}

export function NeedsPage() {
  return <TodayPage view="needs" />;
}

function greetingKey(hour: number): MessageKey {
  if (hour < 12) return "home.greeting.morning";
  if (hour < 18) return "home.greeting.afternoon";
  return "home.greeting.evening";
}

function TodayPage({ view }: { view: "home" | "needs" }) {
  const t = useT();
  const now = useNow();
  // Signed in to Heed Cloud: cards and the brief come from the cloud service, else from this device.
  const cloud = useCloudSignedIn();
  const cards = useAsync(() => (cloud ? cloudApi.cards().then((c) => ({ enabled: true, cards: c.cards })) : proactiveApi.cardsList({})), [cloud]);
  const cloudBrief = useAsync(() => (cloud ? cloudApi.brief() : Promise.resolve(null)), [cloud]);
  const localBrief = useAsync(() => (cloud ? Promise.resolve(undefined) : proactiveApi.briefGet()), [cloud]);
  const brief = cloud ? { ...cloudBrief, data: cloudBrief.data?.brief ?? undefined } : localBrief;
  /** Cloud actions proposed from cards wait in Needs you for approval; bumped to reload them. */
  const [actionsTick, setActionsTick] = useState(0);
  const metrics = useAsync(() => proactiveApi.metrics(), []);
  const learning = useAsync(() => learningApi.learning(), []);
  // Only to tell "all caught up" from "nothing connected yet"; unknown (`null`) is not "none".
  const sources = useAsync(() => api.sourcesList().catch(() => null), []);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshError, setRefreshError] = useState<unknown>(null);
  const [status, setStatus] = useState("");
  /** Cards acted on here, hidden until the next list arrives. */
  const [hidden, setHidden] = useState<ReadonlySet<string>>(new Set());
  /** Cards acted on this visit: kept out of the brief even if its copy is older than the action. */
  const [acted, setActed] = useState<ReadonlySet<string>>(new Set());
  const [action, setAction] = useState<{ card: CardView; action: CardActionView } | null>(null);
  const seen = useRef(new Set<string>());
  const inboxRef = useRef<HTMLHeadingElement>(null);
  /** Set when an executed action closes its dialog: its card leaves the inbox, so focus goes to the inbox. */
  const focusInbox = useRef(false);

  const reloadAll = () => {
    cards.reload();
    cloudBrief.reload();
    localBrief.reload();
    metrics.reload();
    learning.reload();
  };

  useEvent(proactiveEvents.onCardsChanged, reloadAll);

  // Runs after the dialog has handed focus back to the card's button, which is about to go.
  useEffect(() => {
    if (action || !focusInbox.current) return;
    focusInbox.current = false;
    inboxRef.current?.focus();
  }, [action]);

  // A fresh list replaces the optimistic hiding.
  useEffect(() => setHidden(new Set()), [cards.data]);

  // Mark newly displayed cards as seen, once per card.
  useEffect(() => {
    if (view !== "needs") return;
    for (const c of cards.data?.cards ?? []) {
      if (c.state !== "new" || seen.current.has(c.id)) continue;
      seen.current.add(c.id);
      sendFeedback(cloud, c, { feedback: "seen" }).catch(() => {});
    }
  }, [cards.data, view, cloud]);

  const refresh = async () => {
    // Busy is aria-disabled, not disabled, so the button keeps keyboard focus while it refreshes.
    if (refreshing) return;
    setRefreshing(true);
    setRefreshError(null);
    try {
      if (cloud) await cloudApi.refreshCards();
      else await proactiveApi.refresh();
      reloadAll();
    } catch (e) {
      setRefreshError(e);
    } finally {
      setRefreshing(false);
    }
  };

  const feedback = async (card: CardView, request: Omit<CardFeedbackRequest, "card_id">, done: string) => {
    try {
      await sendFeedback(cloud, card, request);
      // In the cloud, accepting a card proposes its action: it waits for approval below.
      if (cloud && request.feedback === "accept") setActionsTick((n) => n + 1);
      setHidden((h) => new Set(h).add(card.id));
      setActed((h) => new Set(h).add(card.id));
      setStatus(done);
      inboxRef.current?.focus();
      cards.reload();
      // The brief lists cards too: refresh it so an acted-on card doesn't linger there.
      brief.reload();
      metrics.reload();
      learning.reload();
    } catch (e) {
      setStatus(t("today.feedbackFailed", { error: errorMessage(e) }));
    }
  };

  /** Cloud: accepting a card with one of its actions proposes that action; it waits for approval below. */
  const proposeFromCard = async (card: CardView, a: CardActionView) => {
    try {
      await cloudApi.acceptCard(card.id, { index: a.index });
      setHidden((h) => new Set(h).add(card.id));
      setActed((h) => new Set(h).add(card.id));
      setActionsTick((n) => n + 1);
      setStatus(t("needs.actions.proposed", { title: localText(card.title_text, card.title) }));
      cards.reload();
    } catch (e) {
      setStatus(t("today.feedbackFailed", { error: errorMessage(e) }));
    }
  };

  /** Forget what feedback taught (one sender/domain/kind/thread, or all); throws on failure. */
  const forget: Forget = async (request: ResetLearningRequest) => {
    const r = await learningApi.resetLearning(request);
    // The daemon also emits `cards.changed`; refetch now in case it is not delivered.
    cards.reload();
    learning.reload();
    return r;
  };

  const list = cards.data;
  const visible = (list?.cards ?? []).filter((c) => !hidden.has(c.id));
  const disabled = list !== undefined && !list.enabled;
  // Cards in the inbox (shown, or just acted on and hidden until the next list): not repeated in the brief.
  const listed = useMemo(() => new Set([...(list?.cards ?? []).map((c) => c.id), ...hidden, ...acted]), [list, hidden, acted]);
  const sourceMap = useMemo(() => new Map((sources.data ?? []).map((s) => [s.id, s])), [sources.data]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected = visible.find((c) => c.id === selectedId) ?? visible[0];
  const refreshButton = (
    <button type="button" className="btn btn-ghost" onClick={() => void refresh()} disabled={disabled} aria-disabled={refreshing || undefined}>
      {refreshing ? <Spinner label={t("today.refreshing")} /> : t("today.refresh")}
    </button>
  );

  if (view === "home") {
    const meetings = brief.data?.meetings ?? [];
    const summary = [
      formatBriefDate(brief.data?.date ?? localDate(now)),
      meetings.length > 0 ? t("home.meetings", { count: meetings.length }) : null,
      visible.length > 0 ? t("home.needsYou", { count: visible.length }) : null,
    ]
      .filter(Boolean)
      .join(" · ");
    return (
      <SourcesContext.Provider value={sourceMap}>
        <div className="page page-home">
          <PageHeader title={t(greetingKey(new Date(now).getHours()))} description={summary} actions={refreshButton} />
          <ErrorNote error={refreshError} />
          <HomeComposer suggestions={suggestions(t, now, meetings, visible)} />
          {disabled ? (
            <EmptyState title={t("today.disabledTitle")}>{t("today.disabledBody")}</EmptyState>
          ) : (
            <BriefSection
              brief={brief.data}
              error={brief.error}
              loading={brief.loading}
              onRetry={brief.reload}
              listed={listed}
              inbox={visible.length}
              home
              prose={cloud ? (cloudBrief.data ?? null) : null}
            />
          )}
        </div>
      </SourcesContext.Provider>
    );
  }

  return (
    <SourcesContext.Provider value={sourceMap}>
      <div className="page page-needs">
        <PageHeader title={t("today.inbox")} description={t("needs.description")} actions={refreshButton} />
        <ErrorNote error={refreshError} />
        <p className="visually-hidden" role="status" aria-live="polite">
          {status}
        </p>

        {disabled ? (
          <EmptyState title={t("today.disabledTitle")}>{t("today.disabledBody")}</EmptyState>
        ) : (
          <>
            <h2 id="today-inbox" className="visually-hidden" ref={inboxRef} tabIndex={-1}>
              {t("today.inbox")}
              {list && visible.length > 0 && <span> · {formatNumber(visible.length)}</span>}
            </h2>
            <ErrorNote error={cards.error} onRetry={cards.reload} />
            {cards.loading && !list && <Loading label={t("today.loading")} lines={3} />}
            {list &&
              visible.length === 0 &&
              (sources.data?.length === 0 ? (
                <EmptyState title={t("today.noSourcesTitle")} action={<GoToPage page="sources" className="btn btn-primary" />}>
                  {t("today.noSourcesBody")}
                </EmptyState>
              ) : (
                <EmptyState title={t("today.emptyTitle")}>{t("today.emptyBody")}</EmptyState>
              ))}
            {visible.length > 0 && selected && (
              <div className="needs-split" onKeyDown={onNeedsKey}>
                <InboxList cards={visible} selected={selected.id} onSelect={setSelectedId} />
                <ul className="today-cards needs-pane" aria-labelledby="today-inbox">
                  {[selected].map((c) => (
                    <CardItem
                      key={c.id}
                      card={c}
                      onAccept={() => void feedback(c, { feedback: "accept" }, t("today.accepted", { title: localText(c.title_text, c.title) }))}
                      onDismiss={(reason, text) =>
                        void feedback(
                          c,
                          { feedback: "dismiss", reason, reason_text: reason === "other" ? text || null : null },
                          t("today.dismissed", { title: localText(c.title_text, c.title) }),
                        )
                      }
                      onSnooze={(choice) => {
                        const until = snoozeUntil(choice);
                        void feedback(
                          c,
                          { feedback: "snooze", until },
                          t("today.snoozed", { title: localText(c.title_text, c.title), until: formatTime(until) }),
                        );
                      }}
                      onAction={(a) => (cloud ? void proposeFromCard(c, a) : setAction({ card: c, action: a }))}
                      onForget={forget}
                    />
                  ))}
                </ul>
              </div>
            )}
            {cloud && <ActionQueue tick={actionsTick} onDone={setStatus} />}
          {!cloud &&
            metrics.data &&
              // A rate means something only once a card has been accepted or dismissed.
              (metrics.data.precision != null && metrics.data.accepted + metrics.data.dismissed > 0 ? (
                <p className="muted small today-metrics">
                  {t("today.metrics", {
                    precision: formatNumber(metrics.data.precision, { style: "percent", maximumFractionDigits: 0 }),
                    accepted: metrics.data.accepted,
                    shown: metrics.data.shown,
                  })}
                </p>
              ) : metrics.data.shown > 0 ? (
                <p className="muted small today-metrics">{t("today.noFeedback")}</p>
              ) : null)}
            {!cloud && <LearningPanel learning={learning.data} error={learning.error} loading={learning.loading} onRetry={learning.reload} onForget={forget} />}
          </>
        )}

        {action && (
          <ActionDialog
            card={action.card}
            action={action.action}
            onClose={(executed) => {
              focusInbox.current = executed;
              setAction(null);
              if (executed) reloadAll();
            }}
          />
        )}
      </div>
    </SourcesContext.Provider>
  );
}

// ---- where cards live ----

/** Card feedback, to Heed Cloud when signed in or to the local background service. */
async function sendFeedback(cloud: boolean, card: CardView, r: Omit<CardFeedbackRequest, "card_id">): Promise<void> {
  if (!cloud) {
    await proactiveApi.cardsFeedback({ card_id: card.id, ...r });
    return;
  }
  if (r.feedback === "accept") await cloudApi.acceptCard(card.id);
  else if (r.feedback === "dismiss") await cloudApi.dismissCard(card.id, r.reason ?? "other", r.reason_text ?? undefined);
  else if (r.feedback === "snooze") await cloudApi.snoozeCard(card.id, r.until ?? snoozeUntil("tomorrow"));
  else if (r.feedback === "seen") await cloudApi.cardsSeen([card.id]);
}

// ---- home ----

/** Today's local date as `YYYY-MM-DD`. */
function localDate(now: number): string {
  const d = new Date(now);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** Three questions to start from: the next meeting and the newest card when there are any, then general ones. */
function suggestions(t: ReturnType<typeof useT>, now: number, meetings: BriefMeetingView[], cards: CardView[]): string[] {
  const out: string[] = [];
  const next = meetings.find((m) => !m.all_day && Date.parse(m.start) > now);
  if (next) out.push(t("home.suggest.prep", { title: next.title }));
  const card = cards[0];
  if (card) out.push(t("home.suggest.card", { title: localText(card.title_text, card.title) }));
  for (const k of ["home.suggest.promised", "home.suggest.week", "home.suggest.changed"] as const) out.push(t(k));
  return out.slice(0, 3);
}

// ---- keyboard ----

/** ⌘/Ctrl+Enter runs the selected card's main action; E accepts it and H opens Snooze. */
const PRIMARY_KEY: Combo = { mod: true, key: "Enter" };

/** A keycap hint right after its button (the button's aria-keyshortcuts says it to screen readers). */
function Keycap({ combo }: { combo: Combo }) {
  return (
    <kbd className="keycap" aria-hidden="true" dir="ltr">
      {combo.key === "Enter" ? comboLabel({ ...combo, key: "↵" }) : comboLabel(combo)}
    </kbd>
  );
}

/** Needs you's shortcuts, while focus is in the list or the pane and not in a text field. */
function onNeedsKey(e: KeyboardEvent<HTMLDivElement>) {
  const k = e.nativeEvent;
  if (e.defaultPrevented || isComposing(k) || isTypingTarget(e.target)) return;
  const plain = !hasMod(k) && !e.altKey;
  const which = hasMod(k) && e.key === "Enter" ? "primary" : plain && letterOf(k) === "e" ? "accept" : plain && letterOf(k) === "h" ? "snooze" : null;
  if (!which) return;
  const button = e.currentTarget.querySelector<HTMLButtonElement>(`.needs-pane [data-shortcut="${which}"]:not([aria-disabled="true"])`);
  if (!button) return;
  e.preventDefault();
  button.click();
}

// ---- inbox list ----

/** The cards as a dense list; the selected one opens in the pane beside it. Arrow keys move. */
function InboxList({ cards, selected, onSelect }: { cards: CardView[]; selected: string; onSelect: (id: string) => void }) {
  const t = useT();
  const now = useNow();
  const onKey = (e: KeyboardEvent<HTMLUListElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const i = cards.findIndex((c) => c.id === selected);
    const next = cards[(i + (e.key === "ArrowDown" ? 1 : cards.length - 1)) % cards.length];
    if (!next) return;
    onSelect(next.id);
    e.currentTarget.querySelector<HTMLButtonElement>(`[data-card="${CSS.escape(next.id)}"]`)?.focus();
  };
  return (
    <ul className="needs-list" aria-label={t("needs.listLabel")} onKeyDown={onKey}>
      {cards.map((c) => (
        <li key={c.id}>
          <button type="button" className="needs-item" data-card={c.id} aria-current={c.id === selected ? "true" : undefined} onClick={() => onSelect(c.id)}>
            {c.state === "new" && <span className="needs-dot" aria-label={t("today.new")} />}
            <span className="needs-item-kind">
              {cardKindLabel(c.kind)}
              {c.due && <> · {formatRelative(c.due, now)}</>}
            </span>
            <span className="needs-item-title" dir="auto">
              {localText(c.title_text, c.title)}
            </span>
            {c.summary && (
              <span className="needs-item-summary" dir="auto">
                {c.summary}
              </span>
            )}
          </button>
        </li>
      ))}
    </ul>
  );
}

// ---- morning brief ----

function BriefSection({
  brief,
  error,
  loading,
  onRetry,
  listed,
  inbox,
  home = false,
  prose = null,
}: {
  brief: BriefView | undefined;
  error: unknown;
  loading: boolean;
  onRetry: () => void;
  /** Cards shown under "Needs you": not repeated in the brief. */
  listed: ReadonlySet<string>;
  /** Cards showing under "Needs you". */
  inbox: number;
  /** On Home: an empty brief gets a calm empty state rather than a line. */
  home?: boolean;
  /** The cloud's written brief, with citations, shown above the lists. */
  prose?: CloudBrief | null;
}) {
  const t = useT();
  const headingId = useId();
  if (!brief) {
    return (
      <section aria-labelledby={headingId} className="today-brief">
        <h2 id={headingId}>{t("today.brief.title")}</h2>
        <ErrorNote error={error} onRetry={onRetry} />
        {loading && <Loading label={t("today.brief.loading")} lines={2} />}
      </section>
    );
  }
  const top = brief.top.filter((c) => !listed.has(c.card_id));
  const empty = top.length + brief.meetings.length + brief.overdue.length + brief.renewals.length === 0;
  return (
    <section aria-labelledby={headingId} className="today-brief">
      <h2 id={headingId}>{t("today.brief.titleDate", { date: formatBriefDate(brief.date) })}</h2>
      {prose?.prose && <BriefProse brief={prose} />}
      {empty && home && prose?.prose ? null : empty && home ? (
        <div className="home-empty">
          <span className="home-empty-ring" aria-hidden="true" />
          <p className="home-empty-title">{inbox > 0 ? t("today.brief.nothingElse") : t("today.brief.empty")}</p>
          <p className="muted">{t("home.empty.body")}</p>
        </div>
      ) : empty ? (
        <p className="muted">{inbox > 0 ? t("today.brief.nothingElse") : t("today.brief.empty")}</p>
      ) : (
        <div className="today-brief-grid">
          <BriefList title={t("today.brief.top")} items={top} />
          {brief.meetings.length > 0 && (
            <div className="today-brief-block">
              <h3>{t("today.brief.meetings")}</h3>
              <ul className="today-brief-list">
                {brief.meetings.map((m, i) => (
                  <MeetingItem key={`${m.start}-${i}`} meeting={m} />
                ))}
              </ul>
            </div>
          )}
          <BriefList title={t("today.brief.overdue")} items={brief.overdue} />
          <BriefList title={t("today.brief.renewals")} items={brief.renewals} />
        </div>
      )}
    </section>
  );
}

function BriefList({ title, items }: { title: string; items: BriefCardView[] }) {
  const t = useT();
  const now = useNow();
  if (items.length === 0) return null;
  return (
    <div className="today-brief-block">
      <h3>{title}</h3>
      <ul className="today-brief-list">
        {items.map((c) => (
          <li key={c.card_id}>
            <span className="today-brief-title" dir="auto">
              {localText(c.title_text, c.title)}
            </span>
            {c.summary && (
              <span className="muted small block" dir="auto">
                {localText(c.summary_text, c.summary)}
              </span>
            )}
            {c.due && (
              <span className="muted small" title={formatTime(c.due)}>
                {t("today.due", { when: formatRelative(c.due, now) })}
              </span>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

function MeetingItem({ meeting: m }: { meeting: BriefMeetingView }) {
  const t = useT();
  const when = m.all_day ? t("today.brief.allDay") : m.end ? `${formatClock(m.start)} – ${formatClock(m.end)}` : formatClock(m.start);
  const jump = () => {
    const el = m.prep_card ? document.getElementById(cardDomId(m.prep_card)) : null;
    el?.focus();
    el?.scrollIntoView?.({ block: "center" });
  };
  return (
    <li>
      <span className="today-brief-title" dir="auto">
        {formatDatesInText(m.title)}
      </span>
      <span className="muted small block">
        {when}
        {m.location && (
          <>
            {" · "}
            <bdi>{m.location}</bdi>
          </>
        )}
      </span>
      {m.external.length > 0 && (
        <span className="small block">
          <Badge tone="accent">{t("today.brief.external")}</Badge> <bdi>{formatList(m.external)}</bdi>
        </span>
      )}
      {m.prep.length > 0 && <EvidenceList evidence={m.prep} label={t("today.brief.prepLabel", { title: m.title })} />}
      {m.prep_card && (
        <button type="button" className="btn btn-ghost btn-small" onClick={jump}>
          {t("today.brief.prepCard")}
        </button>
      )}
    </li>
  );
}

// ---- card inbox ----

function cardDomId(id: string): string {
  return `today-card-${id.replace(/[^A-Za-z0-9_-]/g, "_")}`;
}

function CardItem({
  card,
  onAccept,
  onDismiss,
  onSnooze,
  onAction,
  onForget,
}: {
  card: CardView;
  onAccept: () => void;
  onDismiss: (reason: DismissReason, text: string) => void;
  onSnooze: (choice: SnoozeChoice) => void;
  onAction: (a: CardActionView) => void;
  onForget: Forget;
}) {
  const t = useT();
  const now = useNow();
  const titleId = useId();
  const unsupportedId = useId();
  const evidenceId = useId();
  const [panel, setPanel] = useState<"none" | "dismiss" | "snooze">("none");
  const [showEvidence, setShowEvidence] = useState(false);
  const why = topFactors(card.factors);
  const summary = card.summary?.trim();
  const people = card.people ?? [];
  const title = localText(card.title_text, card.title);
  const primaryAction = card.actions.find((a) => a.supported);

  return (
    <li className="card today-card" id={cardDomId(card.id)} tabIndex={-1} aria-labelledby={titleId}>
      <div className="card-head">
        <h3 id={titleId} className="card-title today-card-title" dir="auto">
          {title}
        </h3>
        <span className="badges">
          <Badge>{cardKindLabel(card.kind)}</Badge>
          {card.state === "new" && <Badge tone="accent">{t("today.new")}</Badge>}
          {card.sensitive && <Badge tone="warn">{t("today.sensitive")}</Badge>}
        </span>
      </div>
      {summary && (
        <p className="today-card-summary" dir="auto">
          {formatDatesInText(summary)}
        </p>
      )}
      {card.detail && card.detail !== summary && (
        <p className="muted today-card-detail" dir="auto">
          {localText(card.detail_text, card.detail)}
        </p>
      )}
      <p className="muted small today-card-meta">
        {card.due && <span title={formatTime(card.due)}>{t("today.due", { when: formatRelative(card.due, now) })}</span>}
        {people.length > 0 && <span>{t("today.people", { people: formatList(people) })}</span>}
      </p>

      {why.length > 0 && (
        <p className="today-why small" aria-label={t("today.whyLabel")}>
          <span className="today-why-label">{t("today.why")}</span> {why.map((f) => factorLabel(f.name)).join(" · ")}
        </p>
      )}
      <LearnedLines card={card} onForget={onForget} />

      {card.evidence.length > 0 && (
        <div>
          <button
            type="button"
            className="btn btn-ghost btn-small"
            aria-expanded={showEvidence}
            aria-controls={evidenceId}
            onClick={() => setShowEvidence((s) => !s)}
          >
            {t("today.evidence", { n: card.evidence.length })}
          </button>
          <div id={evidenceId} hidden={!showEvidence}>
            {showEvidence && <EvidenceList evidence={card.evidence} label={t("today.evidenceLabel", { title })} />}
          </div>
        </div>
      )}

      <div className="today-card-footer">
        {card.actions.length > 0 && (
          <div className="today-proposed" role="group" aria-label={t("today.actionsLabel", { title })}>
            {card.actions.map((a) => {
              const isPrimary = a === primaryAction;
              const short = ACTION_LABEL[a.kind];
              const long = localText(a.label_text, a.label);
              const label = short ? t(short) : long;
              const full = short && long !== label ? long : undefined;
              return a.supported ? (
                <Fragment key={a.index}>
                  <button
                    type="button"
                    className={isPrimary ? "btn btn-small btn-primary" : "btn btn-small btn-ghost"}
                    title={full}
                    aria-label={full ? t("today.actionFullName", { label, full }) : undefined}
                    aria-keyshortcuts={isPrimary ? comboAria(PRIMARY_KEY) : undefined}
                    data-shortcut={isPrimary ? "primary" : undefined}
                    onClick={() => onAction(a)}
                  >
                    {label}
                  </button>
                  {isPrimary && <Keycap combo={PRIMARY_KEY} />}
                </Fragment>
              ) : (
                <button
                  key={a.index}
                  type="button"
                  className="btn btn-small btn-ghost"
                  aria-disabled="true"
                  aria-label={full ? t("today.actionFullName", { label, full }) : undefined}
                  aria-describedby={unsupportedId}
                  title={full ? `${full}. ${t("today.unsupported")}` : t("today.unsupported")}
                  onClick={(e) => e.preventDefault()}
                >
                  {label}
                </button>
              );
            })}
          </div>
        )}
        <button
          type="button"
          className={primaryAction ? "btn btn-small btn-ghost" : "btn btn-small btn-primary"}
          onClick={onAccept}
          aria-label={t("today.acceptLabel", { title })}
          aria-keyshortcuts="E"
          data-shortcut="accept"
        >
          {t("today.accept")}
        </button>
        <Keycap combo={{ key: "E" }} />
        <span className="today-card-footer-end">
          <button
            type="button"
            className="btn btn-small btn-ghost"
            aria-expanded={panel === "snooze"}
            onClick={() => setPanel((p) => (p === "snooze" ? "none" : "snooze"))}
            aria-label={t("today.snoozeLabel", { title })}
            aria-keyshortcuts="H"
            data-shortcut="snooze"
          >
            {t("today.snooze")}
          </button>
          <Keycap combo={{ key: "H" }} />
          <button
            type="button"
            className="btn btn-small btn-danger-ghost"
            aria-expanded={panel === "dismiss"}
            onClick={() => setPanel((p) => (p === "dismiss" ? "none" : "dismiss"))}
            aria-label={t("today.dismissLabel", { title })}
          >
            {t("today.dismiss")}
          </button>
        </span>
      </div>
      {card.actions.some((a) => !a.supported) && (
        <p id={unsupportedId} className="muted small today-unsupported">
          {t("today.unsupported")}
        </p>
      )}

      {panel === "snooze" && (
        <div className="row today-row-start" role="group" aria-label={t("today.snoozeUntil")}>
          {SNOOZE_CHOICES.map((c) => (
            <button key={c} type="button" className="btn btn-small btn-ghost" onClick={() => onSnooze(c)}>
              {t(`today.snoozeChoice.${c}`)}
            </button>
          ))}
        </div>
      )}
      {panel === "dismiss" && <DismissForm onCancel={() => setPanel("none")} onDismiss={onDismiss} />}
    </li>
  );
}

function DismissForm({ onCancel, onDismiss }: { onCancel: () => void; onDismiss: (reason: DismissReason, text: string) => void }) {
  const t = useT();
  const [reason, setReason] = useState<DismissReason>("not_relevant");
  const [text, setText] = useState("");
  const name = useId();
  const textId = useId();
  const first = useRef<HTMLInputElement>(null);
  useEffect(() => first.current?.focus(), []);
  return (
    <form
      className="today-dismiss"
      onSubmit={(e) => {
        e.preventDefault();
        onDismiss(reason, text.trim());
      }}
    >
      <fieldset>
        <legend>{t("today.dismissWhy")}</legend>
        {DISMISS_REASONS.map((r, i) => (
          <label key={r} className="check">
            <input ref={i === 0 ? first : undefined} type="radio" name={name} value={r} checked={reason === r} onChange={() => setReason(r)} />
            {t(`today.dismissReason.${r}`)}
          </label>
        ))}
        {reason === "other" && (
          <div className="field">
            <label htmlFor={textId}>{t("today.dismissOtherLabel")}</label>
            <input id={textId} type="text" maxLength={500} value={text} onChange={(e) => setText(e.target.value)} />
          </div>
        )}
      </fieldset>
      <div className="row today-row-start">
        <button type="submit" className="btn btn-small btn-danger">
          {t("today.dismissConfirm")}
        </button>
        <button type="button" className="btn btn-small btn-ghost" onClick={onCancel}>
          {t("common.cancel")}
        </button>
      </div>
    </form>
  );
}

function EvidenceList({ evidence, label }: { evidence: CardEvidence[]; label: string }) {
  const t = useT();
  const opener = useOpener();
  const sources = useContext(SourcesContext);
  return (
    <>
      <ul className="evidence" aria-label={label}>
        {evidence.map((ev, i) => {
          const e = evidenceLine(ev);
          const source = sources.get(ev.item.source);
          const quote = e.quote;
          return (
            <li key={`${ev.item.source}\u0000${ev.item.key}\u0000${i}`}>
              {quote ? (
                <blockquote className="evidence-quote" dir="auto" title={e.truncated ? e.full : undefined}>
                  {quote}
                </blockquote>
              ) : (
                <p className="muted small">{t("today.noQuote")}</p>
              )}
              <div className="evidence-source">
                <cite className="muted small">
                  <SourceBadge sourceId={ev.item.source} name={source?.name} kind={source?.kind} itemKey={ev.item.key} />
                </cite>
                {e.openUrl && (
                  <button type="button" className="btn btn-small" onClick={() => void opener.open(e.openUrl!, source?.name ?? e.source)}>
                    {t("today.openSource")}
                  </button>
                )}
              </div>
            </li>
          );
        })}
      </ul>
      {opener.status && (
        <p className={opener.status.error ? "field-error" : "muted small"} role="status">
          {opener.status.text}
        </p>
      )}
    </>
  );
}
