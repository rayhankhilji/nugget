import { useMemo, useRef, useState, type KeyboardEvent } from "react";
import { api, events, isMethodNotFound, type EntityKind, type SourceInfo } from "../api";
import { useAsync, useDebounced, useEvent } from "../hooks";
import { kindLabel } from "../knowledge";
import { EntityDetailPanel } from "../components/EntityDetail";
import { GraphPanel, KindChips } from "../components/GraphView";
import { Empty, ErrorNote, NotAvailable, PageHeader, Spinner } from "../components/ui";

const PAGE_SIZE = 50;
type Tab = "browse" | "graph";
const TABS: { id: Tab; label: string }[] = [
  { id: "browse", label: "Browse" },
  { id: "graph", label: "Graph" },
];

export function KnowledgePage() {
  const [tab, setTab] = useState<Tab>("browse");
  const [selected, setSelected] = useState<string | null>(null);
  const [center, setCenter] = useState<string | null>(null);
  const [listTick, setListTick] = useState(0);
  const status = useAsync(api.knowledgeStatus);
  const sources = useAsync(() => api.sourcesList().catch(() => [] as SourceInfo[]));
  const sourceNames = useMemo(() => new Map((sources.data ?? []).map((s) => [s.id, s.name])), [sources.data]);
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
    const i = TABS.findIndex((t) => t.id === tab);
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
      onSelect={setSelected}
      onShowInGraph={(id) => {
        setCenter(id);
        setTab("graph");
      }}
      onMerged={(into) => {
        setSelected(into);
        setListTick((t) => t + 1);
        statusReload();
      }}
      onClose={tab === "graph" ? () => setSelected(null) : undefined}
    />
  );

  return (
    <div className="page page-wide">
      <PageHeader
        title="Knowledge"
        description="People, organizations, projects and more that Nugget has learned from your sources. Every fact links back to where it came from."
        actions={
          s && (
            <p className="muted small knowledge-counts">
              {s.entities.toLocaleString()} entities · {s.claims.toLocaleString()} facts
              {s.pending_items > 0 && <> · {s.pending_items.toLocaleString()} items waiting</>}
            </p>
          )
        }
      />
      {status.error != null && !isMethodNotFound(status.error) && <ErrorNote error={status.error} onRetry={status.reload} />}

      <div className="tabs" role="tablist" aria-label="Knowledge views" onKeyDown={onTabKey}>
        {TABS.map((t) => (
          <button
            key={t.id}
            ref={(el) => {
              tabRefs.current[t.id] = el;
            }}
            type="button"
            role="tab"
            id={`kb-tab-${t.id}`}
            aria-selected={tab === t.id}
            aria-controls={`kb-panel-${t.id}`}
            tabIndex={tab === t.id ? 0 : -1}
            className="tab"
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>

      {tab === "browse" && (
        <div id="kb-panel-browse" role="tabpanel" aria-labelledby="kb-tab-browse" className="kb-layout">
          <EntityBrowser selected={selected} onSelect={setSelected} total={s?.entities ?? null} refreshKey={listTick} />
          <div className="kb-detail">
            {detail || <Empty title="Choose an entity">Pick someone or something on the left to see what Nugget knows and why.</Empty>}
          </div>
        </div>
      )}
      {tab === "graph" && (
        <div id="kb-panel-graph" role="tabpanel" aria-labelledby="kb-tab-graph" className={selected ? "kb-graph kb-graph-with-detail" : "kb-graph"}>
          <GraphPanel center={center} onCenter={setCenter} selected={selected} onOpen={setSelected} />
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
        <NotAvailable what="Knowledge" />
      </div>
    );
  }

  return (
    <section className="kb-list" aria-label="Entities">
      <div className="field">
        <label htmlFor="kb-search" className="visually-hidden">
          Search entities
        </label>
        <input
          id="kb-search"
          type="search"
          placeholder="Search people, projects, places…"
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
        label="Filter by kind"
        single
      />
      <p className="muted small kb-count" aria-live="polite">
        {list.loading && !list.data ? (
          "Loading…"
        ) : rows.length === 0 ? (
          filtered ? "No matching entities." : ""
        ) : (
          <>
            {from.toLocaleString()}–{to.toLocaleString()}
            {!filtered && total != null ? ` of ${total.toLocaleString()}` : hasMore ? " and more" : ""}
          </>
        )}
      </p>
      <ErrorNote error={list.error} onRetry={list.reload} />
      {list.data && rows.length === 0 && !filtered && (
        <Empty title="No entities yet">They appear as Nugget indexes your sources: people from mail and calendars, projects and topics from notes.</Empty>
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
                {kindLabel(e.kind)} · {e.claims.toLocaleString()} {e.claims === 1 ? "fact" : "facts"} · {e.mentions.toLocaleString()}{" "}
                {e.mentions === 1 ? "mention" : "mentions"}
              </span>
            </button>
          </li>
        ))}
      </ul>
      {(page > 0 || hasMore) && (
        <nav className="pager" aria-label="Entity pages">
          <button type="button" className="btn btn-small" onClick={() => setPage(page - 1)} disabled={page === 0 || list.loading}>
            Previous
          </button>
          <span className="muted small">Page {page + 1}</span>
          <button type="button" className="btn btn-small" onClick={() => setPage(page + 1)} disabled={!hasMore || list.loading}>
            Next
          </button>
          {list.loading && <Spinner />}
        </nav>
      )}
    </section>
  );
}
