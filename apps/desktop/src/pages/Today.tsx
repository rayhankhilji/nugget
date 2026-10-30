/**
 * Today: the morning brief and the card inbox. Each card says why it is here (top score
 * factors, evidence citations) and offers accept / dismiss (with a reason) / snooze, plus
 * actions that run only after a dry-run preview is approved (see `today/ActionDialog`).
 *
 * Everything a card shows comes from indexed mail, calendars and notes: it is rendered as plain
 * text, and links are offered only for `https:` (opened by the OS, never in the webview).
 */
import { createContext, useContext, useEffect, useId, useMemo, useRef, useState } from "react";
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
import { formatRelative, formatTime } from "../format";
import { GoToPage } from "../components/GoToPage";
import { plainSnippet, SourceBadge } from "../components/Provenance";
import { Badge, EmptyState, ErrorNote, Loading, PageHeader, Spinner } from "../components/ui";
import { formatList, formatNumber, useT, type MessageKey } from "../i18n";
import { ActionDialog } from "./today/ActionDialog";
import { LearnedLines, LearningPanel, type Forget } from "./today/Learning";
import {
  SNOOZE_CHOICES,
  cardKindLabel,
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

export function TodayPage() {
  const t = useT();
  const cards = useAsync(() => proactiveApi.cardsList({}), []);
  const brief = useAsync(() => proactiveApi.briefGet(), []);
  const metrics = useAsync(() => proactiveApi.metrics(), []);
  const learning = useAsync(() => learningApi.learning(), []);
  // Only to tell "all caught up" from "nothing connected yet"; unknown (`null`) is not "none".
  const sources = useAsync(() => api.sourcesList().catch(() => null), []);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshError, setRefreshError] = useState<unknown>(null);
  const [status, setStatus] = useState("");
  /** Cards acted on here, hidden until the next list arrives. */
  const [hidden, setHidden] = useState<ReadonlySet<string>>(new Set());
  const [action, setAction] = useState<{ card: CardView; action: CardActionView } | null>(null);
  const seen = useRef(new Set<string>());
  const inboxRef = useRef<HTMLHeadingElement>(null);
  /** Set when an executed action closes its dialog: its card leaves the inbox, so focus goes to the inbox. */
  const focusInbox = useRef(false);

  const reloadAll = () => {
    cards.reload();
    brief.reload();
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
    for (const c of cards.data?.cards ?? []) {
      if (c.state !== "new" || seen.current.has(c.id)) continue;
      seen.current.add(c.id);
      proactiveApi.cardsFeedback({ card_id: c.id, feedback: "seen" }).catch(() => {});
    }
  }, [cards.data]);

  const refresh = async () => {
    // Busy is aria-disabled, not disabled, so the button keeps keyboard focus while it refreshes.
    if (refreshing) return;
    setRefreshing(true);
    setRefreshError(null);
    try {
      await proactiveApi.refresh();
      reloadAll();
    } catch (e) {
      setRefreshError(e);
    } finally {
      setRefreshing(false);
    }
  };

  const feedback = async (card: CardView, request: Omit<CardFeedbackRequest, "card_id">, done: string) => {
    try {
      await proactiveApi.cardsFeedback({ card_id: card.id, ...request });
      setHidden((h) => new Set(h).add(card.id));
      setStatus(done);
      inboxRef.current?.focus();
      cards.reload();
      metrics.reload();
      learning.reload();
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
  const listed = useMemo(() => new Set(visible.map((c) => c.id)), [visible]);
  const sourceMap = useMemo(() => new Map((sources.data ?? []).map((s) => [s.id, s])), [sources.data]);

  return (
    <SourcesContext.Provider value={sourceMap}>
    <div className="page">
      <PageHeader
        title={t("today.title")}
        description={t("today.description")}
        actions={
          <button type="button" className="btn btn-ghost" onClick={() => void refresh()} disabled={disabled} aria-disabled={refreshing || undefined}>
            {refreshing ? <Spinner label={t("today.refreshing")} /> : t("today.refresh")}
          </button>
        }
      />
      <ErrorNote error={refreshError} />
      <p className="visually-hidden" role="status" aria-live="polite">
        {status}
      </p>

      {disabled ? (
        <EmptyState title={t("today.disabledTitle")}>{t("today.disabledBody")}</EmptyState>
      ) : (
        <>
          <BriefSection brief={brief.data} error={brief.error} loading={brief.loading} onRetry={brief.reload} listed={listed} />

          <h2 id="today-inbox" ref={inboxRef} tabIndex={-1}>
            {t("today.inbox")}
            {list && visible.length > 0 && <span className="muted"> · {formatNumber(visible.length)}</span>}
          </h2>
          <ErrorNote error={cards.error} onRetry={cards.reload} />
          {cards.loading && !list && <Loading label={t("today.loading")} lines={3} />}
          {list && visible.length === 0 &&
            (sources.data?.length === 0 ? (
              <EmptyState title={t("today.noSourcesTitle")} action={<GoToPage page="sources" />}>
                {t("today.noSourcesBody")}
              </EmptyState>
            ) : (
              <EmptyState title={t("today.emptyTitle")}>{t("today.emptyBody")}</EmptyState>
            ))}
          {visible.length > 0 && (
            <ul className="today-cards" aria-labelledby="today-inbox">
              {visible.map((c) => (
                <CardItem
                  key={c.id}
                  card={c}
                  onAccept={() => void feedback(c, { feedback: "accept" }, t("today.accepted", { title: c.title }))}
                  onDismiss={(reason, text) =>
                    void feedback(c, { feedback: "dismiss", reason, reason_text: reason === "other" ? text || null : null }, t("today.dismissed", { title: c.title }))
                  }
                  onSnooze={(choice) => {
                    const until = snoozeUntil(choice);
                    void feedback(c, { feedback: "snooze", until }, t("today.snoozed", { title: c.title, until: formatTime(until) }));
                  }}
                  onAction={(a) => setAction({ card: c, action: a })}
                  onForget={forget}
                />
              ))}
            </ul>
          )}
          {metrics.data &&
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
          <LearningPanel
            learning={learning.data}
            error={learning.error}
            loading={learning.loading}
            onRetry={learning.reload}
            onForget={forget}
          />
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

// ---- morning brief ----

function BriefSection({
  brief,
  error,
  loading,
  onRetry,
  listed,
}: {
  brief: BriefView | undefined;
  error: unknown;
  loading: boolean;
  onRetry: () => void;
  /** Cards shown under "Needs you": not repeated in the brief. */
  listed: ReadonlySet<string>;
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
      {empty ? (
        <p className="muted">{listed.size > 0 ? t("today.brief.nothingElse") : t("today.brief.empty")}</p>
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
              {c.title}
            </span>
            {c.summary && (
              <span className="muted small block" dir="auto">
                {c.summary}
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
        {m.title}
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

  return (
    <li className="card today-card" id={cardDomId(card.id)} tabIndex={-1} aria-labelledby={titleId}>
      <div className="card-head">
        <h3 id={titleId} className="card-title today-card-title" dir="auto">
          {card.title}
        </h3>
        <span className="badges">
          <Badge>{cardKindLabel(card.kind)}</Badge>
          {card.state === "new" && <Badge tone="accent">{t("today.new")}</Badge>}
          {card.sensitive && <Badge tone="warn">{t("today.sensitive")}</Badge>}
        </span>
      </div>
      {summary && (
        <p className="today-card-summary" dir="auto">
          {summary}
        </p>
      )}
      {card.detail && card.detail !== summary && (
        <p className="muted today-card-detail" dir="auto">
          {card.detail}
        </p>
      )}
      <p className="muted small today-card-meta">
        {card.due && (
          <span title={formatTime(card.due)}>{t("today.due", { when: formatRelative(card.due, now) })}</span>
        )}
        {people.length > 0 && (
          <span>{t("today.people", { people: formatList(people) })}</span>
        )}
      </p>

      {why.length > 0 && (
        <div className="today-why">
          <span className="small">{t("today.why")}</span>
          <ul className="badges" aria-label={t("today.whyLabel")}>
            {why.map((f) => (
              <li key={f.name}>
                <Badge tone="neutral">{factorLabel(f.name)}</Badge>
              </li>
            ))}
          </ul>
        </div>
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
            {showEvidence && <EvidenceList evidence={card.evidence} label={t("today.evidenceLabel", { title: card.title })} />}
          </div>
        </div>
      )}

      <div className="today-card-footer">
        {card.actions.length > 0 && (
          <div className="today-proposed" role="group" aria-label={t("today.actionsLabel", { title: card.title })}>
            {card.actions.map((a) => {
              const short = ACTION_LABEL[a.kind];
              const label = short ? t(short) : a.label;
              const full = short && a.label !== label ? a.label : undefined;
              return a.supported ? (
                <button key={a.index} type="button" className="btn btn-small" title={full} onClick={() => onAction(a)}>
                  {label}
                </button>
              ) : (
                <button
                  key={a.index}
                  type="button"
                  className="btn btn-small"
                  aria-disabled="true"
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
        <button type="button" className="btn btn-small btn-primary" onClick={onAccept} aria-label={t("today.acceptLabel", { title: card.title })}>
          {t("today.accept")}
        </button>
        <span className="today-card-footer-end">
          <button
            type="button"
            className="btn btn-small btn-ghost"
            aria-expanded={panel === "snooze"}
            onClick={() => setPanel((p) => (p === "snooze" ? "none" : "snooze"))}
            aria-label={t("today.snoozeLabel", { title: card.title })}
          >
            {t("today.snooze")}
          </button>
          <button
            type="button"
            className="btn btn-small btn-danger-ghost"
            aria-expanded={panel === "dismiss"}
            onClick={() => setPanel((p) => (p === "dismiss" ? "none" : "dismiss"))}
            aria-label={t("today.dismissLabel", { title: card.title })}
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
            <input
              ref={i === 0 ? first : undefined}
              type="radio"
              name={name}
              value={r}
              checked={reason === r}
              onChange={() => setReason(r)}
            />
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
          const quote = e.quote ? plainSnippet(e.quote) : "";
          return (
            <li key={`${ev.item.source}\u0000${ev.item.key}\u0000${i}`}>
              {quote ? (
                <blockquote className="evidence-quote" dir="auto" title={e.truncated ? ev.quote ?? undefined : undefined}>
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

