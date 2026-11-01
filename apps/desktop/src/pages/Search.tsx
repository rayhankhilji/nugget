import { useEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import {
  api,
  errorMessage,
  events,
  isCommandMissing,
  isMethodNotFound,
  isOpenableUrl,
  type ChatDone,
  type Citation,
  type ContextResponse,
  type FacetedHit,
  type FacetedResult,
  type Locality,
  type SearchHit,
  type SourceInfo,
} from "../api";
import { useAsync, useEvent, useOpener } from "../hooks";
import { usePendingSearch, usePendingSearchState } from "../hooks.nav";
import { citedNumbers, splitCitations, withheldNotice } from "../citations";
import { Announce, useAnnouncer } from "../components/Announce";
import { GoToPage } from "../components/GoToPage";
import { Highlight, itemFileName, plainSnippet, SourceBadge } from "../components/Provenance";
import { sourceKindOf } from "../recency";
import { EmptyState, ErrorNote, Loading, NotAvailable, PageHeader, Spinner } from "../components/ui";
import { formatDate, formatNumber, useT } from "../i18n";
import { sourceKindLabel } from "../format";
import { addRecent } from "../savedSearches";
import { activeFilters, decode, EMPTY_STATE, encode, hasFilters, normalize, toFilters, type SearchState } from "../searchState";
import { Facets, kindLabel } from "../components/search/Facets";
import { Preview } from "../components/search/Preview";
import { RecentSearchList, SavedSearchList, SaveSearchDialog } from "../components/search/SavedSearches";
import { Timeline } from "../components/search/Timeline";
import "../styles-search.css";

const LIMITS = [10, 25, 50];
const BUDGETS = [1000, 2000, 4000, 8000];

/** A result from `search.faceted` (with date, kind, people) or from plain `search` (without). */
type Hit = SearchHit & Partial<Omit<FacetedHit, keyof SearchHit>>;

export function SearchPage() {
  const t = useT();
  const [query, setQuery] = useState("");
  const [limit, setLimit] = useState(25);
  const [hits, setHits] = useState<Hit[] | null>(null);
  const [searched, setSearched] = useState("");
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [askOpen, setAskOpen] = useState(false);
  /** The search shown: query, filters and, when set, the timeline in view. */
  const [state, setState] = useState<SearchState>(EMPTY_STATE);
  const [facets, setFacets] = useState<FacetedResult["facets"] | null>(null);
  const [total, setTotal] = useState<number | null>(null);
  /** The pool size when the daemon capped its candidates (facet counts may be partial). */
  const [poolLimited, setPoolLimited] = useState<number | null>(null);
  const [moreBusy, setMoreBusy] = useState(false);
  const [selected, setSelected] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);
  const [announcement, announce] = useAnnouncer();
  /** Whether the daemon serves `search.faceted` (unknown until the first search). */
  const faceted = useRef<boolean | null>(null);
  /** Only the latest search may update the page. */
  const seq = useRef(0);
  /** The wanted state, updated at once (before any answer), so quick changes build on each other. */
  const stateRef = useRef<SearchState>(EMPTY_STATE);
  /** The encoded search (without timeline) the shown results answer. */
  const shownKey = useRef<string | null>(null);
  /** Said by a filter control just before its search; repeated with the result count. */
  const lead = useRef("");
  /** The result whose preview is open, to hand focus back when it closes. */
  const returnFocus = useRef<HTMLElement | null>(null);
  const opener = useOpener();
  const inputRef = useRef<HTMLInputElement>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  // Source names and kinds for badges. Optional: older daemons don't have `sources.list` (null).
  const sources = useAsync(() => api.sourcesList().catch(() => null));
  const byId = useMemo(() => new Map((sources.data ?? []).map((s) => [s.id, s])), [sources.data]);
  const sourceName = (id: string) => {
    const s = byId.get(id);
    return s?.name || sourceKindLabel(s?.kind ?? sourceKindOf(id));
  };
  // Nothing to search yet: no sources, or none has indexed an item. Unknown (older daemon) is not "nothing".
  const nothingIndexed = !!sources.data && sources.data.reduce((n, s) => n + s.items, 0) === 0;
  // Sources exist but none has indexed anything yet: indexing is still running (or failing).
  const stillIndexing = nothingIndexed && (sources.data?.length ?? 0) > 0;
  const failing = (sources.data ?? []).filter((s) => s.state === "error" || s.last_error).length;
  const nothingHint = stillIndexing ? t("search.indexingHint") : t("search.noSourcesHint");

  /** Make `next` the wanted state right away (the controls show it while the search runs). */
  const want = (next: SearchState): SearchState => {
    const n = normalize(next);
    stateRef.current = n;
    setState(n);
    return n;
  };

  /**
   * Run `next` (its query and filters); `append` loads the next page of the same search.
   * `record` adds it to recent searches (explicit runs only, not filter changes or paging).
   */
  const runState = async (wanted: SearchState, { append = false, record = false }: { append?: boolean; record?: boolean } = {}) => {
    const next = append ? stateRef.current : want({ ...wanted, timeline: null });
    const q = next.q;
    if (!q) return;
    const mine = ++seq.current;
    const offset = append ? (hits?.length ?? 0) : 0;
    if (append) setMoreBusy(true);
    else setBusy(true);
    setError(null);
    try {
      let result: FacetedResult | null = null;
      if (faceted.current !== false) {
        try {
          result = await api.searchFaceted({ query: q, limit, offset, filters: toFilters(next) });
          faceted.current = true;
        } catch (err) {
          if (!isCommandMissing(err)) throw err;
          faceted.current = false;
        }
      }
      const plain = result ? null : await api.search(q, limit);
      if (mine !== seq.current) return;
      if (result) {
        setHits((prev) => (append ? [...(prev ?? []), ...result.hits] : result.hits));
        setFacets(result.facets);
        setTotal(result.total);
        setPoolLimited(result.pool_limited ? result.pool : null);
      } else {
        setHits(plain);
        setFacets(null);
        setTotal(null);
        setPoolLimited(null);
      }
      if (!append) {
        setSelected(null);
        setSearched(q);
        shownKey.current = encode(next);
        if (record) addRecent(encode(next));
        const n = result ? result.total : (plain?.length ?? 0);
        const active = activeFilters(next);
        const said = active > 0 ? t("searchui.resultsFiltered", { n, query: q, filters: active }) : t("search.resultsFor", { n, query: q });
        // Keep what a control said as it cleared itself ("Filters cleared") in front of the count.
        announce(lead.current ? `${lead.current} ${said}` : said);
        lead.current = "";
      }
    } catch (err) {
      if (mine === seq.current) setError(err);
    } finally {
      if (mine === seq.current) {
        setBusy(false);
        setMoreBusy(false);
      }
    }
  };

  const run = (e?: FormEvent, override?: string) => {
    e?.preventDefault();
    const q = (override ?? query).trim();
    if (!q) return;
    // A new query keeps the current filters (they stay visible and clearable).
    return runState({ ...stateRef.current, q }, { record: true });
  };

  /** Apply an encoded state: a saved or recent search, a timeline requested from elsewhere. */
  const apply = (encoded: string) => {
    const s = decode(encoded);
    if (s.timeline) {
      setSelected(null);
      if (!s.q && !hasFilters(s)) {
        // Only a timeline (the entity page's button): keep the search as it is underneath.
        const next = { ...stateRef.current, timeline: s.timeline };
        stateRef.current = next;
        setState(next);
        return;
      }
      setQuery(s.q);
      stateRef.current = s;
      setState(s);
      return;
    }
    setQuery(s.q);
    inputRef.current?.focus();
    void runState(s, { record: true });
  };

  // A search from the command palette: fill the box and run it, keeping focus in the box.
  usePendingSearch((q) => {
    setQuery(q);
    inputRef.current?.focus();
    void runState({ ...EMPTY_STATE, q }, { record: true });
  });
  usePendingSearchState(apply);

  const openTimeline = (entityId: string) => {
    setSelected(null);
    stateRef.current = { ...stateRef.current, timeline: entityId };
    setState(stateRef.current);
  };
  const closeTimeline = () => {
    const back = { ...stateRef.current, timeline: null };
    stateRef.current = back;
    setState(back);
    // Results for another search (a saved timeline with its own query): run that search.
    if (back.q && shownKey.current !== encode(back)) void runState(back);
    focusBack.current = true;
  };
  // Back on the results: focus their heading (or the box when there are none yet).
  const focusBack = useRef(false);
  useEffect(() => {
    if (!focusBack.current || state.timeline) return;
    focusBack.current = false;
    (headingRef.current ?? inputRef.current)?.focus();
  }, [state.timeline]);

  const select = (i: number, from: HTMLElement) => {
    returnFocus.current = from;
    setSelected(i);
  };
  const closePreview = () => {
    setSelected(null);
    if (returnFocus.current?.isConnected) returnFocus.current.focus();
  };

  // Arrow keys move between results (each result's title is a button).
  const onResultsKey = (e: KeyboardEvent<HTMLOListElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const buttons = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>(".result-select"));
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (i < 0) return;
    e.preventDefault();
    buttons[Math.max(0, Math.min(buttons.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))]?.focus();
  };

  const count = total ?? hits?.length ?? 0;
  const shownState = { ...state, q: state.q || query.trim() };
  const canSave = !!shownState.q || !!state.timeline;
  const preview = selected !== null ? hits?.[selected] : undefined;
  const filtered = hasFilters(state);

  if (state.timeline) {
    return (
      <div className="page">
        <PageHeader title={t("search.title")} description={t("search.description")} actions={<SaveButton disabled={!canSave} onClick={() => setSaving(true)} />} />
        <div aria-live="polite" className={opener.status ? (opener.status.error ? "note note-error" : "note") : "visually-hidden"}>
          {opener.status?.text}
        </div>
        <Timeline entityId={state.timeline} onBack={closeTimeline} onOpen={opener.open} />
        {saving && (
          <SaveSearchDialog state={encode(state)} defaultName="" onClose={() => setSaving(false)} onSaved={(name) => announce(t("searchui.savedAnnounce", { name }))} />
        )}
        <Announce message={announcement} />
      </div>
    );
  }

  return (
    <div className="page">
      <PageHeader
        title={t("search.title")}
        description={t("search.description")}
        actions={
          <>
            <SaveButton disabled={!canSave} onClick={() => setSaving(true)} />
            <button type="button" className="btn btn-ghost" aria-expanded={askOpen} aria-controls="ask-panel" onClick={() => setAskOpen((o) => !o)}>
              {askOpen ? t("search.hideAsk") : t("search.ask")}
            </button>
          </>
        }
      />
      <form className="search-bar" role="search" onSubmit={run}>
        <label htmlFor="search-q" className="visually-hidden">
          {t("search.query")}
        </label>
        <input
          id="search-q"
          ref={inputRef}
          type="search"
          dir="auto"
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

      <SavedSearchList onRun={apply} announce={announce} onEmpty={() => inputRef.current?.focus()} />
      {!query.trim() && <RecentSearchList onRun={apply} announce={announce} onEmpty={() => inputRef.current?.focus()} />}

      <ErrorNote error={error} onRetry={() => void runState({ ...stateRef.current, q: stateRef.current.q || query.trim() })} />
      {failing > 0 && (
        <p className="note" role="status">
          {t("search.failingSources", { n: failing })} <GoToPage page="sources" className="btn btn-ghost btn-small" />
        </p>
      )}

      <div aria-live="polite" className={opener.status ? (opener.status.error ? "note note-error" : "note") : "visually-hidden"}>
        {opener.status?.text}
      </div>
      {/* Each finished search is announced through `announce` (with the filter count, so a
          filter change that keeps the same count is still heard). */}

      {busy && !hits && <Loading label={t("search.searching")} lines={4} />}
      <div className={`sr-body${facets ? " sr-has-facets" : ""}${preview ? " sr-has-preview" : ""}`}>
        {facets && hits && (
          <Facets
            facets={facets}
            state={state}
            sourceName={sourceName}
            onChange={(update) => void runState(update(stateRef.current))}
            onTimeline={openTimeline}
            poolLimited={poolLimited}
            announce={(text) => {
              lead.current = text;
              announce(text);
            }}
          />
        )}
        <div className="sr-results">
          {hits && (
            <h2 id="search-results-heading" className="pg-results-heading" ref={headingRef} tabIndex={-1}>
              {t("search.resultsFor", { n: count, query: searched })}
            </h2>
          )}
          {hits && hits.length === 0 && (
            <EmptyState
              title={t("search.noResults", { query: searched })}
              action={
                filtered ? (
                  <button type="button" className="btn btn-primary" onClick={() => void runState({ ...stateRef.current, sources: [], kinds: [], people: [], from: null, to: null })}>
                    {t("searchui.clearFilters")}
                  </button>
                ) : nothingIndexed ? (
                  <GoToPage page="sources" className="btn btn-primary" />
                ) : undefined
              }
            >
              {filtered ? t("searchui.noResultsFiltered") : nothingIndexed ? nothingHint : t("search.noResultsHint")}
            </EmptyState>
          )}
          {hits && hits.length > 0 && (
            <ol className={busy ? "results pg-stale" : "results"} aria-labelledby="search-results-heading" aria-busy={busy} onKeyDown={onResultsKey}>
              {hits.map((h, i) => {
                const source = byId.get(h.item.source);
                const title = h.title || itemFileName(h.item.key, source?.kind ?? sourceKindOf(h.item.source)) || t("search.untitled");
                return (
                  <li key={`${h.item.source}\u0000${h.item.key}\u0000${i}`} className={selected === i ? "result sr-selected" : "result"}>
                    <div className="result-head">
                      <button
                        type="button"
                        className="result-title result-select"
                        dir="auto"
                        aria-expanded={selected === i}
                        aria-controls={selected === i ? "search-preview" : undefined}
                        onClick={(e) => select(i, e.currentTarget)}
                      >
                        {title}
                      </button>
                      <span className="result-tools">
                        {isOpenableUrl(h.url) && (
                          <button type="button" className="btn btn-small" onClick={() => void opener.open(h.url!, title)} aria-label={t("search.openTitle", { title })}>
                            {t("search.open")}
                          </button>
                        )}
                      </span>
                    </div>
                    {(h.kind || h.date || h.sensitive) && (
                      <p className="small sr-meta">
                        {h.kind && <span className="badge">{kindLabel(t, h.kind)}</span>}
                        {h.date && (
                          <time dateTime={h.date} className="muted">
                            {formatDate(h.date)}
                          </time>
                        )}
                        {h.sensitive && (
                          <span className="badge badge-warn" title={t("search.sensitiveTitle")}>
                            {t("search.sensitive")}
                          </span>
                        )}
                      </p>
                    )}
                    <p className="result-snippet" dir="auto">
                      <Highlight text={plainSnippet(h.snippet)} query={searched} />
                    </p>
                    <p className="muted small result-meta">
                      <SourceBadge sourceId={h.item.source} name={source?.name} kind={source?.kind} itemKey={h.item.key} title={h.title} />
                    </p>
                    {h.people && h.people.length > 0 && (
                      <ul className="chips sr-people" aria-label={t("searchui.peopleIn", { title })}>
                        {h.people.map((p) => (
                          <li key={p.id}>
                            <button type="button" className="chip" onClick={() => openTimeline(p.id)} aria-label={t("searchui.timelineFor", { name: p.name })}>
                              <bdi>{p.name}</bdi>
                            </button>
                          </li>
                        ))}
                      </ul>
                    )}
                  </li>
                );
              })}
            </ol>
          )}
          {hits && total !== null && hits.length < total && (
            <button type="button" className="btn sr-more" disabled={moreBusy || busy} onClick={() => void runState(stateRef.current, { append: true })}>
              {moreBusy ? <Spinner label={t("searchui.loadingMore")} /> : t("searchui.showMore", { shown: hits.length, total })}
            </button>
          )}
          {!hits && !busy && !error &&
            (nothingIndexed ? (
              <EmptyState title={stillIndexing ? t("search.indexingTitle") : t("search.noSourcesTitle")} action={<GoToPage page="sources" className="btn btn-primary" />}>
                {nothingHint}
              </EmptyState>
            ) : (
              <EmptyState title={t("search.typeQuery")}>{t("search.typeQueryHint")}</EmptyState>
            ))}
        </div>
        {preview && (
          <Preview
            item={preview.item}
            chunkId={preview.chunk_id}
            fallbackTitle={preview.title ?? ""}
            query={searched}
            source={byId.get(preview.item.source)}
            onClose={closePreview}
            onTimeline={openTimeline}
            onOpen={opener.open}
          />
        )}
      </div>
      {saving && (
        <SaveSearchDialog
          state={encode(shownState)}
          defaultName={shownState.q}
          onClose={() => setSaving(false)}
          onSaved={(name) => announce(t("searchui.savedAnnounce", { name }))}
        />
      )}
      <Announce message={announcement} />
    </div>
  );
}

function SaveButton({ disabled, onClick }: { disabled: boolean; onClick: () => void }) {
  const t = useT();
  return (
    <button type="button" className="btn btn-ghost" disabled={disabled} onClick={onClick} aria-haspopup="dialog">
      {t("searchui.saveSearch")}
    </button>
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
  const [announcement, announce] = useAnnouncer();

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

  const getContext = async (e?: FormEvent) => {
    e?.preventDefault();
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
    announce(t("announce.answering"));
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

  // The answer streams outside any live region; announce once it is complete (a failure shows
  // as an alert, which announces itself).
  const finished = answer !== null && answer.streamId !== null && answer.done !== null && answer.error === null;
  const finishedText = finished ? answer.text : null;
  useEffect(() => {
    if (finishedText === null) return;
    announce(t("announce.answerReady", { count: citedNumbers(finishedText, answerValid).size }));
    // Once per finished answer: the text and citations are final by then.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [finished, answer?.streamId]);

  return (
    <section id="ask-panel" className="ask-panel" aria-labelledby="ask-title">
      <h2 id="ask-title">{t("search.ask")}</h2>
      <p className="muted small">{t("search.askIntro")}</p>
      <form className="ask-form" onSubmit={getContext}>
        <label htmlFor="ask-q" className="visually-hidden">
          {t("search.question")}
        </label>
        <input id="ask-q" type="text" dir="auto" value={question} onChange={(e) => setQuestion(e.target.value)} placeholder={t("search.questionPlaceholder")} autoFocus />
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

      {isMethodNotFound(error) ? <NotAvailable what={t("search.contextRetrieval")} /> : <ErrorNote error={error} onRetry={() => void getContext()} />}

      {ctx && ctx.citations.length === 0 && <p className="muted">{t("search.nothingRelevant", { query: asked })}</p>}
      {ctx && ctx.citations.length > 0 && (
        <>
          <h3 className="ask-sub">{t("search.context")}</h3>
          <div className="context-text" dir="auto" tabIndex={0} role="region" aria-label={t("search.retrievedContext")}>
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
        <div className="answer" aria-busy={streaming}>
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
            <div className="answer-text" dir="auto">
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
      <Announce message={announcement} />
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
  const title = c.title || itemFileName(c.item.key, source?.kind ?? sourceKindOf(c.item.source)) || t("search.untitled");
  return (
    <li id={`${idPrefix}-${c.n}`} tabIndex={-1} className="citation">
      <span className="cite-n">[{c.n}]</span>
      <span className="citation-body">
        <span className="citation-title">
          <span dir="auto">{title}</span>
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
          <SourceBadge sourceId={c.item.source} name={source?.name} kind={source?.kind} itemKey={c.item.key} title={c.title} />
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
