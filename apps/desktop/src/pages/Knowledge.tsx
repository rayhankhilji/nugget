import { useMemo, useRef, useState, type KeyboardEvent } from "react";
import { api, events, isMethodNotFound, type EntityKind, type SourceInfo } from "../api";
import { useAsync, useDebounced, useEvent } from "../hooks";
import { useT, type MessageKey } from "../i18n";
import { graphSourceOptions, kindLabel } from "../knowledge";
import { EntityDetailPanel } from "../components/EntityDetail";
import { GraphPanel, KindChips } from "../components/GraphView";
import { GoToPage } from "../components/GoToPage";
import { Empty, EmptyState, ErrorNote, Loading, NotAvailable, PageHeader, Spinner } from "../components/ui";

const PAGE_SIZE = 50;
type Tab = "browse" | "graph";
const TABS: { id: Tab; label: MessageKey }[] = [
  { id: "browse", label: "knowledge.tab.browse" },
  { id: "graph", label: "knowledge.tab.graph" },
];

export function KnowledgePage() {
  const t = useT();
  const [tab, setTab] = useState<Tab>("browse");
  const [selected, setSelected] = useState<string | null>(null);
  const [center, setCenter] = useState<string | null>(null);
  const [listTick, setListTick] = useState(0);
  const status = useAsync(api.knowledgeStatus);
  // `null` when the list can't be had (older daemon, offline): unknown, not "no sources".
  const sources = useAsync(() => api.sourcesList().catch((): SourceInfo[] | null => null));
  const sourceNames = useMemo(() => new Map((sources.data ?? []).map((s) => [s.id, s.name])), [sources.data]);
  const sourceKinds = useMemo(() => new Map((sources.data ?? []).map((s) => [s.id, s.kind])), [sources.data]);
  const sourceOptions = useMemo(() => graphSourceOptions(sources.data ?? []), [sources.data]);
  const tabRefs = useRef<Record<Tab, HTMLButtonElement | null>>({ browse: null, graph: null });

  // Counts move with extraction; refresh them now and then rather than on every burst.
  const statusReload = status.reload;
  const lastStatus = useRef(0);
  useEvent(events.onKnowledgeChanged, () => {
    if (Date.now() - lastStatus.current > 5000) {
      lastStatus.current = Date.now();
      statusReload();
    }
  });

  const vaultRoot = status.data?.vault.path ?? null;
  // One page-level error: when the status fails, the list's own error (usually the same) is not repeated.
  const pageError = status.error != null && !isMethodNotFound(status.error);
  const s = status.data;

  const onTabKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight" && e.key !== "Home" && e.key !== "End") return;
    e.preventDefault();
    const i = TABS.findIndex((x) => x.id === tab);
    const next =
      e.key === "Home" ? 0 : e.key === "End" ? TABS.length - 1 : (i + (e.key === "ArrowRight" ? 1 : TABS.length - 1)) % TABS.length;
    const id = TABS[next]!.id;
    setTab(id);
    tabRefs.current[id]?.focus();
  };

  const detail = selected && (
    <EntityDetailPanel
      id={selected}
      vaultRoot={vaultRoot}
      sourceNames={sourceNames}
      sourceKinds={sourceKinds}
      onSelect={setSelected}
      onShowInGraph={(id) => {
        setCenter(id);
        setTab("graph");
      }}
      onMerged={(into) => {
        setSelected(into);
        setListTick((n) => n + 1);
        statusReload();
      }}
      onClose={tab === "graph" ? () => setSelected(null) : undefined}
    />
  );

  return (
    <div className="page page-wide">
      <PageHeader
        title={t("knowledge.title")}
        description={t("knowledge.description")}
        actions={
          // The graph tab has its own count line.
          s &&
          tab === "browse" && (
            <p className="muted small knowledge-counts">
              {s.pending_items > 0
                ? t("knowledge.countsWaiting", { entities: s.entities, claims: s.claims, pending: s.pending_items })
                : t("knowledge.counts", { entities: s.entities, claims: s.claims })}
            </p>
          )
        }
      />
      {pageError && (
        <ErrorNote
          error={status.error}
          onRetry={() => {
            status.reload();
            setListTick((n) => n + 1);
          }}
        />
      )}

      <div className="tabs" role="tablist" aria-label={t("knowledge.tabsLabel")} onKeyDown={onTabKey}>
        {TABS.map((x) => (
          <button
            key={x.id}
            ref={(el) => {
              tabRefs.current[x.id] = el;
            }}
            type="button"
            role="tab"
            id={`kb-tab-${x.id}`}
            aria-selected={tab === x.id}
            aria-controls={`kb-panel-${x.id}`}
            tabIndex={tab === x.id ? 0 : -1}
            className="tab"
            onClick={() => setTab(x.id)}
          >
            {t(x.label)}
          </button>
        ))}
      </div>

      {tab === "browse" && (
        <div id="kb-panel-browse" role="tabpanel" aria-labelledby="kb-tab-browse" className="kb-panel">
          <div className={selected ? "kb-layout kb-layout-selected" : "kb-layout"}>
            <EntityBrowser
              selected={selected}
              onSelect={setSelected}
              total={s?.entities ?? null}
              refreshKey={listTick}
              noSources={sources.data?.length === 0}
              pending={s?.pending_items ?? 0}
              hideError={pageError}
            />
            {(detail || (s && s.entities > 0)) && (
              <div className="kb-detail">
                {detail || <Empty title={t("knowledge.chooseTitle")}>{t("knowledge.chooseBody")}</Empty>}
              </div>
            )}
          </div>
        </div>
      )}
      {tab === "graph" && (
        <div id="kb-panel-graph" role="tabpanel" aria-labelledby="kb-tab-graph" className="kb-panel">
          <div className={selected ? "kb-graph kb-graph-with-detail" : "kb-graph"}>
            <GraphPanel center={center} onCenter={setCenter} selected={selected} onOpen={setSelected} sources={sourceOptions} />
            {selected && <div className="kb-detail kb-detail-side">{detail}</div>}
          </div>
        </div>
      )}
    </div>
  );
}

function EntityBrowser({
  selected,
  onSelect,
  total,
  refreshKey,
  noSources,
  pending,
  hideError,
}: {
  selected: string | null;
  onSelect: (id: string) => void;
  /** All entities, from `knowledge.status` (shown when no filter is set). */
  total: number | null;
  refreshKey: number;
  /** Known to have no sources at all (so nothing can be extracted yet). */
  noSources: boolean;
  /** Items still waiting for extraction. */
  pending: number;
  /** The page already shows an error (the list's would repeat it). */
  hideError?: boolean;
}) {
  const t = useT();
  const [text, setText] = useState("");
  const query = useDebounced(text.trim(), 250);
  const [kind, setKind] = useState<EntityKind | null>(null);
  // The page resets whenever the filter changes (without an extra fetch for the old page).
  const filterKey = `${query}\u0000${kind ?? ""}`;
  const [paging, setPaging] = useState({ key: filterKey, page: 0 });
  const page = paging.key === filterKey ? paging.page : 0;
  const setPage = (p: number) => setPaging({ key: filterKey, page: p });

  const list = useAsync(
    () => api.knowledgeEntities({ text: query || null, kind, limit: PAGE_SIZE + 1, offset: page * PAGE_SIZE }),
    [query, kind, page, refreshKey],
  );

  // Keep the list fresh while extraction runs, without refetching on every event.
  const { reload } = list;
  const last = useRef(0);
  useEvent(events.onKnowledgeChanged, () => {
    if (Date.now() - last.current > 3000) {
      last.current = Date.now();
      reload();
    }
  });

  const rows = (list.data ?? []).slice(0, PAGE_SIZE);
  const hasMore = (list.data?.length ?? 0) > PAGE_SIZE;
  const filtered = Boolean(query || kind);
  // Nothing extracted at all: no point offering search and kind filters.
  // Only when the settled, unfiltered list is empty: never while a query or kind is set, typed but
  // not yet debounced, or reloading (the stale result of an old query would unmount the field).
  const nothing =
    !filtered && !text.trim() && !kind && page === 0 && !list.loading && list.data !== undefined && rows.length === 0;
  const firstLoad = list.loading && !list.data;
  // Once the filters have been offered they stay mounted, so focus in them is never dropped.
  const filtersShown = useRef(false);
  const showFilters = filtersShown.current || (!nothing && !(firstLoad && !filtered) && !(list.error != null && !list.data));
  filtersShown.current = showFilters;
  // "of N" from the status count, but never fewer pages than the list itself shows.
  const pages =
    !filtered && total != null && total > 0 ? Math.max(Math.ceil(total / PAGE_SIZE), page + 1 + (hasMore ? 1 : 0)) : null;
  const from = page * PAGE_SIZE + 1;
  const to = page * PAGE_SIZE + rows.length;

  const onListKey = (e: KeyboardEvent<HTMLUListElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const buttons = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>("button.entity-row"));
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (i < 0) return;
    e.preventDefault();
    buttons[Math.min(buttons.length - 1, Math.max(0, i + (e.key === "ArrowDown" ? 1 : -1)))]?.focus();
  };

  if (isMethodNotFound(list.error)) {
    return (
      <div className="kb-list">
        <NotAvailable what={t("knowledge.title")} />
      </div>
    );
  }

  return (
    <section className="kb-list" aria-label={t("knowledge.entitiesLabel")}>
      {showFilters && (
        <>
      <div className="field">
        <label htmlFor="kb-search" className="visually-hidden">
          {t("knowledge.searchLabel")}
        </label>
        <input
          id="kb-search"
          type="search"
          dir="auto"
          placeholder={t("knowledge.searchPlaceholder")}
          value={text}
          onChange={(e) => setText(e.target.value)}
          autoComplete="off"
          spellCheck={false}
        />
      </div>
      <KindChips
        selected={kind ? [kind] : []}
        onToggle={(k) => setKind((cur) => (cur === k ? null : k))}
        onClear={() => setKind(null)}
        label={t("knowledge.filterByKind")}
        single
      />
        </>
      )}
      <p className="muted small kb-count" aria-live="polite">
        {firstLoad ? (
          ""
        ) : rows.length === 0 ? (
          filtered ? t("knowledge.noMatches") : ""
        ) : !filtered && total != null ? (
          t("knowledge.rangeOf", { from, to, total })
        ) : hasMore ? (
          t("knowledge.rangeMore", { from, to })
        ) : (
          t("knowledge.range", { from, to })
        )}
      </p>
      {firstLoad && <Loading label={t("knowledge.loading")} lines={6} />}
      {!hideError && <ErrorNote error={list.error} onRetry={list.reload} />}
      {list.data && rows.length === 0 && !filtered && (
        <EmptyState title={t("knowledge.emptyTitle")} action={noSources ? <GoToPage page="sources" className="btn btn-primary" /> : undefined}>
          {noSources ? t("knowledge.emptyNoSources") : pending > 0 ? t("knowledge.emptyPending", { n: pending }) : t("knowledge.emptyBody")}
        </EmptyState>
      )}
      <ul className="entity-list" onKeyDown={onListKey} aria-busy={list.loading || undefined}>
        {rows.map((e) => (
          <li key={e.id}>
            <button type="button" className="entity-row" aria-current={e.id === selected ? "true" : undefined} onClick={() => onSelect(e.id)}>
              <span className="entity-row-name">
                <span className={`kind-dot kind-${e.kind}`} aria-hidden="true" />
                <span dir="auto">{e.name}</span>
              </span>
              <span className="muted small">
                {t("knowledge.rowMeta", { kind: kindLabel(e.kind), claims: e.claims, mentions: e.mentions })}
              </span>
            </button>
          </li>
        ))}
      </ul>
      {(page > 0 || hasMore) && (
        <nav className="pager" aria-label={t("knowledge.pagesLabel")}>
          <button type="button" className="btn btn-small" onClick={() => setPage(page - 1)} disabled={page === 0 || list.loading}>
            {t("knowledge.previous")}
          </button>
          <span className="muted small">{pages ? t("knowledge.pageOf", { n: page + 1, total: pages }) : t("knowledge.page", { n: page + 1 })}</span>
          <button type="button" className="btn btn-small" onClick={() => setPage(page + 1)} disabled={!hasMore || list.loading}>
            {t("common.next")}
          </button>
          {list.loading && <Spinner />}
        </nav>
      )}
    </section>
  );
}
