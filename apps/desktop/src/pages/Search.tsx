import { useMemo, useRef, useState, type FormEvent } from "react";
import {
  api,
  errorMessage,
  events,
  isMethodNotFound,
  isOpenableUrl,
  type ChatDone,
  type Citation,
  type ContextResponse,
  type Locality,
  type SearchHit,
  type SourceInfo,
} from "../api";
import { useAsync, useEvent, useOpener } from "../hooks";
import { splitCitations, withheldNotice } from "../citations";
import { sourceKindLabel } from "../format";
import { SourceIcon } from "../components/SourceIcon";
import { Empty, ErrorNote, NotAvailable, PageHeader, Spinner } from "../components/ui";
import { formatNumber, useT } from "../i18n";

const LIMITS = [10, 25, 50];
const BUDGETS = [1000, 2000, 4000, 8000];

export function SearchPage() {
  const t = useT();
  const [query, setQuery] = useState("");
  const [limit, setLimit] = useState(25);
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [searched, setSearched] = useState("");
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [askOpen, setAskOpen] = useState(false);
  const opener = useOpener();
  // Source names and kinds for badges. Optional: older daemons don't have `sources.list`.
  const sources = useAsync(() => api.sourcesList().catch(() => [] as SourceInfo[]));
  const byId = useMemo(() => new Map((sources.data ?? []).map((s) => [s.id, s])), [sources.data]);

  const run = async (e?: FormEvent) => {
    e?.preventDefault();
    const q = query.trim();
    if (!q) return;
    setBusy(true);
    setError(null);
    try {
      setHits(await api.search(q, limit));
      setSearched(q);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page">
      <PageHeader
        title={t("search.title")}
        description={t("search.description")}
        actions={
          <button type="button" className="btn btn-ghost" aria-expanded={askOpen} aria-controls="ask-panel" onClick={() => setAskOpen((o) => !o)}>
            {askOpen ? t("search.hideAsk") : t("search.ask")}
          </button>
        }
      />
      <form className="search-bar" role="search" onSubmit={run}>
        <label htmlFor="search-q" className="visually-hidden">
          {t("search.query")}
        </label>
        <input
          id="search-q"
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("search.placeholder")}
          autoFocus
        />
        <label className="field-inline">
          <span>{t("search.show")}</span>
          <select value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
            {LIMITS.map((l) => (
              <option key={l} value={l}>
                {formatNumber(l)}
              </option>
            ))}
          </select>
        </label>
        <button type="submit" className="btn btn-primary" disabled={busy || !query.trim()}>
          {busy ? <Spinner label={t("search.searching")} /> : t("search.submit")}
        </button>
      </form>

      {askOpen && <AskPanel initialQuestion={query.trim()} byId={byId} onOpen={opener.open} />}

      <ErrorNote error={error} onRetry={() => void run()} />

      <div aria-live="polite" className={opener.status ? (opener.status.error ? "note note-error" : "note") : "visually-hidden"}>
        {opener.status?.text}
      </div>
      <div aria-live="polite" className="visually-hidden">
        {hits && t("search.resultsFor", { n: hits.length, query: searched })}
      </div>

      {hits && hits.length === 0 && (
        <Empty title={t("search.noResults", { query: searched })}>{t("search.noResultsHint")}</Empty>
      )}
      {hits && hits.length > 0 && (
        <ol className="results">
          {hits.map((h, i) => {
            const title = h.title || h.item.key;
            return (
              <li key={`${h.item.source}\u0000${h.item.key}\u0000${i}`} className="result">
                <div className="result-head">
                  <span className="result-title">{title}</span>
                  <span className="result-tools">
                    <span className="muted small" title={t("search.score")}>
                      {formatNumber(h.score, { minimumFractionDigits: 3, maximumFractionDigits: 3 })}
                    </span>
                    {isOpenableUrl(h.url) && (
                      <button type="button" className="btn btn-small" onClick={() => void opener.open(h.url!, title)} aria-label={t("search.openTitle", { title })}>
                        {t("search.open")}
                      </button>
                    )}
                  </span>
                </div>
                <p className="result-snippet">{h.snippet}</p>
                <p className="muted small result-meta">
                  <SourceBadge id={h.item.source} source={byId.get(h.item.source)} /> <code>{h.item.key}</code>
                </p>
              </li>
            );
          })}
        </ol>
      )}
      {!hits && !error && <Empty title={t("search.typeQuery")}>{t("search.typeQueryHint")}</Empty>}
    </div>
  );
}

function SourceBadge({ id, source }: { id: string; source: SourceInfo | undefined }) {
  const t = useT();
  const kind = source?.kind ?? "";
  return (
    <span className="source-badge" title={source ? t("search.sourceBadge", { kind: sourceKindLabel(kind), id }) : id}>
      <SourceIcon kind={kind} size={13} />
      {source?.name ?? id}
    </span>
  );
}

// ---- Ask with context ----

type Answer = {
  streamId: string | null;
  provider: string | null;
  locality: Locality | null;
  /** The passages the model was given (they can differ from the preview, e.g. for a cloud model). */
  citations: Citation[];
  excludedSensitive: number;
  text: string;
  done: ChatDone | null;
  error: string | null;
};

const NO_ANSWER: Omit<Answer, "error"> = {
  streamId: null,
  provider: null,
  locality: null,
  citations: [],
  excludedSensitive: 0,
  text: "",
  done: null,
};

function AskPanel({
  initialQuestion,
  byId,
  onOpen,
}: {
  initialQuestion: string;
  byId: Map<string, SourceInfo>;
  onOpen: (url: string, label: string) => Promise<void>;
}) {
  const t = useT();
  const [question, setQuestion] = useState(initialQuestion);
  const [budget, setBudget] = useState(2000);
  const [ctx, setCtx] = useState<ContextResponse | null>(null);
  const [asked, setAsked] = useState("");
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [answer, setAnswer] = useState<Answer | null>(null);
  // Chat events can arrive before `chat` returns the stream id; buffer them by id.
  const streams = useRef(new Map<string, { text: string; done: ChatDone | null }>());
  const listRef = useRef<HTMLOListElement>(null);

  const valid = useMemo(() => new Set((ctx?.citations ?? []).map((c) => c.n)), [ctx]);
  const byN = useMemo(() => new Map((ctx?.citations ?? []).map((c) => [c.n, c])), [ctx]);
  // The answer cites the context it was given, which is numbered on its own.
  const answerValid = useMemo(() => new Set((answer?.citations ?? []).map((c) => c.n)), [answer?.citations]);
  const answerByN = useMemo(() => new Map((answer?.citations ?? []).map((c) => [c.n, c])), [answer?.citations]);

  const sync = (id: string) => {
    const s = streams.current.get(id);
    if (!s) return;
    setAnswer((a) => (a && a.streamId === id ? { ...a, text: s.text, done: s.done, error: s.done?.error ?? null } : a));
  };
  useEvent(events.onChatDelta, (d) => {
    const s = streams.current.get(d.stream_id) ?? { text: "", done: null };
    s.text += d.text;
    streams.current.set(d.stream_id, s);
    sync(d.stream_id);
  });
  useEvent(events.onChatDone, (d) => {
    const s = streams.current.get(d.stream_id) ?? { text: "", done: null };
    s.done = d;
    streams.current.set(d.stream_id, s);
    sync(d.stream_id);
  });

  const getContext = async (e: FormEvent) => {
    e.preventDefault();
    const q = question.trim();
    if (!q) return;
    setBusy(true);
    setError(null);
    setAnswer(null);
    try {
      setCtx(await api.retrieveContext({ query: q, budget_tokens: budget }));
      setAsked(q);
    } catch (err) {
      setCtx(null);
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const ask = async () => {
    if (!ctx) return;
    streams.current.clear();
    setAnswer({ ...NO_ANSWER, error: null });
    try {
      // The daemon picks the model first and packs the context for it: sensitive passages are
      // withheld from cloud models, and the cited items are recorded in the egress ledger.
      const started = await api.retrieveAsk({ query: asked, budget_tokens: budget, provider: null, tier: "synthesis" });
      const s = streams.current.get(started.stream_id);
      setAnswer({
        streamId: started.stream_id,
        provider: started.provider,
        locality: started.locality,
        citations: started.citations,
        excludedSensitive: started.excluded_sensitive ?? 0,
        text: s?.text ?? "",
        done: s?.done ?? null,
        error: s?.done?.error ?? null,
      });
    } catch (err) {
      setAnswer({ ...NO_ANSWER, error: errorMessage(err) });
    }
  };

  const openOrFocus = (c: Citation | undefined, list: HTMLOListElement | null, prefix: string) => {
    if (!c) return;
    if (isOpenableUrl(c.url)) {
      void onOpen(c.url, c.title || c.item.key);
    } else {
      list?.querySelector<HTMLElement>(`#${prefix}-${c.n}`)?.focus();
    }
  };
  const cite = (n: number) => openOrFocus(byN.get(n), listRef.current, "cite");
  const answerListRef = useRef<HTMLOListElement>(null);
  const answerCite = (n: number) => openOrFocus(answerByN.get(n), answerListRef.current, "answer-cite");
  const withheld = answer?.provider ? withheldNotice(answer.excludedSensitive, answer.provider) : null;

  const streaming = answer !== null && answer.done === null && answer.error === null;

  return (
    <section id="ask-panel" className="ask-panel" aria-labelledby="ask-title">
      <h2 id="ask-title">{t("search.ask")}</h2>
      <p className="muted small">{t("search.askIntro")}</p>
      <form className="ask-form" onSubmit={getContext}>
        <label htmlFor="ask-q" className="visually-hidden">
          {t("search.question")}
        </label>
        <input id="ask-q" type="text" value={question} onChange={(e) => setQuestion(e.target.value)} placeholder={t("search.questionPlaceholder")} autoFocus />
        <label className="field-inline">
          <span>{t("search.budget")}</span>
          <select value={budget} onChange={(e) => setBudget(Number(e.target.value))}>
            {BUDGETS.map((b) => (
              <option key={b} value={b}>
                {t("search.budgetTokens", { n: b })}
              </option>
            ))}
          </select>
        </label>
        <button type="submit" className="btn" disabled={busy || !question.trim()}>
          {busy ? <Spinner label={t("search.findingContext")} /> : t("search.findContext")}
        </button>
      </form>

      {isMethodNotFound(error) ? <NotAvailable what={t("search.contextRetrieval")} /> : <ErrorNote error={error} />}

      {ctx && ctx.citations.length === 0 && <p className="muted">{t("search.nothingRelevant", { query: asked })}</p>}
      {ctx && ctx.citations.length > 0 && (
        <>
          <h3 className="ask-sub">{t("search.context")}</h3>
          <div className="context-text" tabIndex={0} role="region" aria-label={t("search.retrievedContext")}>
            <CitedText text={ctx.text} valid={valid} byN={byN} onCite={cite} />
          </div>
          <h3 className="ask-sub">{t("search.citations")}</h3>
          <ol className="citations" ref={listRef}>
            {ctx.citations.map((c) => (
              <CitationItem key={c.n} c={c} source={byId.get(c.item.source)} onOpen={onOpen} />
            ))}
          </ol>
          <div className="ask-actions">
            <button type="button" className="btn btn-primary" onClick={() => void ask()} disabled={streaming}>
              {streaming ? <Spinner label={t("search.answering")} /> : answer ? t("search.askAgain") : t("search.askWithContext")}
            </button>
            <span className="muted small">{t("search.askNote")}</span>
          </div>
        </>
      )}

      {answer && (
        <div className="answer" aria-live="polite" aria-busy={streaming}>
          <h3 className="ask-sub">
            {t("search.answer")}
            {answer.provider && (
              <span className="muted small">
                {" "}
                {t("search.answerProvider", { provider: answer.provider, locality: answer.locality ?? "" })}
              </span>
            )}
          </h3>
          {withheld && (
            <p className="note" role="status">
              {withheld}
            </p>
          )}
          {answer.error ? (
            <p className="note note-error" role="alert">
              {answer.error}
            </p>
          ) : (
            <div className="answer-text">
              {answer.text ? (
                <CitedText text={answer.text} valid={answerValid} byN={answerByN} onCite={answerCite} />
              ) : (
                <Spinner label={t("search.waitingModel")} />
              )}
            </div>
          )}
          {answer.citations.length > 0 && (
            <>
              <h4 className="ask-sub">{t("search.passagesSent")}</h4>
              <ol className="citations" ref={answerListRef}>
                {answer.citations.map((c) => (
                  <CitationItem key={c.n} c={c} idPrefix="answer-cite" source={byId.get(c.item.source)} onOpen={onOpen} />
                ))}
              </ol>
            </>
          )}
        </div>
      )}
    </section>
  );
}

function CitedText({
  text,
  valid,
  byN,
  onCite,
}: {
  text: string;
  valid: ReadonlySet<number>;
  byN: Map<number, Citation>;
  onCite: (n: number) => void;
}) {
  const t = useT();
  const parts = useMemo(() => splitCitations(text, valid), [text, valid]);
  return (
    <>
      {parts.map((p, i) =>
        p.kind === "text" ? (
          <span key={i}>{p.text}</span>
        ) : (
          <button
            key={i}
            type="button"
            className="cite"
            onClick={() => onCite(p.n)}
            aria-label={t("search.citationLabel", { n: p.n, title: byN.get(p.n)?.title || byN.get(p.n)?.item.key || "" })}
            title={byN.get(p.n)?.title ?? undefined}
          >
            {p.raw}
          </button>
        ),
      )}
    </>
  );
}

function CitationItem({
  c,
  source,
  onOpen,
  idPrefix = "cite",
}: {
  c: Citation;
  source: SourceInfo | undefined;
  onOpen: (url: string, label: string) => Promise<void>;
  idPrefix?: string;
}) {
  const t = useT();
  const title = c.title || c.item.key;
  return (
    <li id={`${idPrefix}-${c.n}`} tabIndex={-1} className="citation">
      <span className="cite-n">[{c.n}]</span>
      <span className="citation-body">
        <span className="citation-title">
          {title}
          {c.sensitive && (
            <>
              {" "}
              <span className="badge badge-warn" title={t("search.sensitiveTitle")}>
                {t("search.sensitive")}
              </span>
            </>
          )}
        </span>
        <span className="muted small">
          <SourceBadge id={c.item.source} source={source} />
        </span>
      </span>
      {isOpenableUrl(c.url) && (
        <button type="button" className="btn btn-small" onClick={() => void onOpen(c.url!, title)} aria-label={t("search.openTitle", { title })}>
          {t("search.open")}
        </button>
      )}
    </li>
  );
}
