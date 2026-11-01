import { useCallback, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { api, events, isMethodNotFound, type EntityKind, type EntitySummaryView, type SourceInfo } from "../api";
import { useAsync, useDebounced, useEvent } from "../hooks";
import { useT, type MessageKey } from "../i18n";
import { graphSourceOptions, kindLabel } from "../knowledge";
import { EntityDetailPanel } from "../components/EntityDetail";
import { GraphPanel, KindChips } from "../components/GraphView";
import { GoToPage } from "../components/GoToPage";
import { Empty, EmptyState, ErrorNote, Loading, NotAvailable, PageHeader } from "../components/ui";
import { usePagedList } from "../components/usePagedList";
import { VirtualList } from "../components/VirtualList";

/** Entities fetched per request as the list scrolls. */
const PAGE_SIZE = 50;
const entityKey = (e: EntitySummaryView) => e.id;
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
  // Pages load as the list scrolls (II5); a new filter or a merge starts over from the top.
  const fetchPage = useCallback(
    (offset: number, limit: number) => api.knowledgeEntities({ text: query || null, kind, limit, offset }),
    [query, kind],
  );
  const list = usePagedList(fetchPage, `${query}\u0000${kind ?? ""}\u0000${refreshKey}`, { pageSize: PAGE_SIZE, itemKey: entityKey });

  // Keep the list fresh while extraction runs, without refetching on every event (and without
  // pulling a long scrolled-through list from under the reader: see `refresh`).
  const { refresh } = list;
  const last = useRef(0);
  useEvent(events.onKnowledgeChanged, () => {
    if (Date.now() - last.current > 3000) {
      last.current = Date.now();
      refresh();
    }
  });

  const rows = list.items ?? [];
  const hasMore = list.hasMore;
  const filtered = Boolean(query || kind);
  // Nothing extracted at all: no point offering search and kind filters.
  // Only when the settled, unfiltered list is empty: never while a query or kind is set, typed but
  // not yet debounced, or reloading (the stale result of an old query would unmount the field).
  const nothing = !filtered && !text.trim() && !kind && !list.loading && list.items !== undefined && rows.length === 0;
  const firstLoad = list.loading && !list.items;
  // Once the filters have been offered they stay mounted, so focus in them is never dropped.
  const filtersShown = useRef(false);
  const showFilters = filtersShown.current || (!nothing && !(firstLoad && !filtered) && !(list.error != null && !list.items));
  filtersShown.current = showFilters;
  // "of N" from the status count, but never fewer than the list itself holds.
  const known = !filtered && total != null && total > 0 ? Math.max(total, rows.length + (hasMore ? 1 : 0)) : null;

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
        ) : known != null ? (
          t("knowledge.rangeOf", { from: 1, to: rows.length, total: known })
        ) : hasMore ? (
          t("knowledge.rangeMore", { from: 1, to: rows.length })
        ) : (
          t("knowledge.range", { from: 1, to: rows.length })
        )}
      </p>
      {firstLoad && <Loading label={t("knowledge.loading")} lines={6} />}
      {!hideError && <ErrorNote error={list.error} onRetry={list.reload} />}
      {list.items && rows.length === 0 && !filtered && (
        <EmptyState title={t("knowledge.emptyTitle")} action={noSources ? <GoToPage page="sources" className="btn btn-primary" /> : undefined}>
          {noSources ? t("knowledge.emptyNoSources") : pending > 0 ? t("knowledge.emptyPending", { n: pending }) : t("knowledge.emptyBody")}
        </EmptyState>
      )}
      <VirtualList
        as="ul"
        className="entity-list"
        aria-busy={list.loading || undefined}
        items={rows}
        itemKey={entityKey}
        estimateSize={50}
        focus="inner"
        total={known ?? undefined}
        hasMore={hasMore}
        loadingMore={list.loadingMore}
        onEndReached={list.loadMore}
        renderItem={(e, { tabIndex }) => (
          <button
            type="button"
            className="entity-row"
            data-vl-focus=""
            tabIndex={tabIndex}
            aria-current={e.id === selected ? "true" : undefined}
            onClick={() => onSelect(e.id)}
          >
            <span className="entity-row-name">
              <span className={`kind-dot kind-${e.kind}`} aria-hidden="true" />
              <span dir="auto">{e.name}</span>
            </span>
            <span className="muted small">{t("knowledge.rowMeta", { kind: kindLabel(e.kind), claims: e.claims, mentions: e.mentions })}</span>
          </button>
        )}
      />
    </section>
  );
}
