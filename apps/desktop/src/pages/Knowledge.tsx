import { useMemo, useRef, useState, type KeyboardEvent } from "react";
import { api, events, isMethodNotFound, type EntityKind, type SourceInfo } from "../api";
import { useAsync, useDebounced, useEvent } from "../hooks";
import { useT, type MessageKey } from "../i18n";
import { graphSourceOptions, kindLabel } from "../knowledge";
import { EntityDetailPanel } from "../components/EntityDetail";
import { GraphPanel, KindChips } from "../components/GraphView";
import { Empty, ErrorNote, NotAvailable, PageHeader, Spinner } from "../components/ui";

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
  const sources = useAsync(() => api.sourcesList().catch(() => [] as SourceInfo[]));
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
          s && (
            <p className="muted small knowledge-counts">
              {s.pending_items > 0
                ? t("knowledge.countsWaiting", { entities: s.entities, claims: s.claims, pending: s.pending_items })
                : t("knowledge.counts", { entities: s.entities, claims: s.claims })}
            </p>
          )
        }
      />
      {status.error != null && !isMethodNotFound(status.error) && <ErrorNote error={status.error} onRetry={status.reload} />}

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
        <div id="kb-panel-browse" role="tabpanel" aria-labelledby="kb-tab-browse" className="kb-layout">
          <EntityBrowser selected={selected} onSelect={setSelected} total={s?.entities ?? null} refreshKey={listTick} />
          <div className="kb-detail">
            {detail || <Empty title={t("knowledge.chooseTitle")}>{t("knowledge.chooseBody")}</Empty>}
          </div>
        </div>
      )}
      {tab === "graph" && (
        <div id="kb-panel-graph" role="tabpanel" aria-labelledby="kb-tab-graph" className={selected ? "kb-graph kb-graph-with-detail" : "kb-graph"}>
          <GraphPanel center={center} onCenter={setCenter} selected={selected} onOpen={setSelected} sources={sourceOptions} />
          {selected && <div className="kb-detail kb-detail-side">{detail}</div>}
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
}: {
  selected: string | null;
  onSelect: (id: string) => void;
  /** All entities, from `knowledge.status` (shown when no filter is set). */
  total: number | null;
  refreshKey: number;
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
      <div className="field">
        <label htmlFor="kb-search" className="visually-hidden">
          {t("knowledge.searchLabel")}
        </label>
        <input
          id="kb-search"
          type="search"
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
      <p className="muted small kb-count" aria-live="polite">
        {list.loading && !list.data ? (
          t("knowledge.loading")
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
      <ErrorNote error={list.error} onRetry={list.reload} />
      {list.data && rows.length === 0 && !filtered && (
        <Empty title={t("knowledge.emptyTitle")}>{t("knowledge.emptyBody")}</Empty>
      )}
      <ul className="entity-list" onKeyDown={onListKey} aria-busy={list.loading || undefined}>
        {rows.map((e) => (
          <li key={e.id}>
            <button type="button" className="entity-row" aria-current={e.id === selected ? "true" : undefined} onClick={() => onSelect(e.id)}>
              <span className="entity-row-name">
                <span className={`kind-dot kind-${e.kind}`} aria-hidden="true" />
                {e.name}
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
          <span className="muted small">{t("knowledge.page", { n: page + 1 })}</span>
          <button type="button" className="btn btn-small" onClick={() => setPage(page + 1)} disabled={!hasMore || list.loading}>
            {t("common.next")}
          </button>
          {list.loading && <Spinner />}
        </nav>
      )}
    </section>
  );
}
