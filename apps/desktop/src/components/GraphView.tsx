import { useEffect, useId, useMemo, useRef, useState, type FormEvent } from "react";
import { api, ENTITY_KINDS, events, type EntityKind, type EntitySummaryView, type GraphView as GraphData } from "../api";
import { hashSeed, MAX_RENDER_NODES, prepareGraph, type PreparedGraph } from "../graph";
import { useAsync, useEvent } from "../hooks";
import { Trans, useT } from "../i18n";
import { kindLabel, kindPlural } from "../knowledge";
import { GraphCanvas } from "./GraphCanvas";
import { Badge, Empty, ErrorNote, Spinner } from "./ui";

const DEPTHS = [1, 2, 3];
/** Ask the daemon for a few more than we draw, so truncation is reported honestly. */
const REQUEST_LIMIT = MAX_RENDER_NODES + 1;
const LIST_PAGE = 200;

/**
 * The graph view: an overview of the most connected entities, or the neighbourhood of one
 * (`center`), filtered by kind and depth. Canvas or list (the accessible alternative).
 */
export function GraphPanel({
  center,
  onCenter,
  selected,
  onOpen,
  sources = [],
}: {
  center: string | null;
  onCenter: (id: string | null) => void;
  selected: string | null;
  onOpen: (id: string) => void;
  /** Sources the graph can be filtered by (`{id, name}`, in display order). */
  sources?: readonly { id: string; name: string }[];
}) {
  const t = useT();
  const [depth, setDepth] = useState(2);
  const [source, setSource] = useState<string | null>(null);
  const sourceId = useId();
  const [kinds, setKinds] = useState<EntityKind[]>([]);
  const [mode, setMode] = useState<"canvas" | "list">("canvas");
  const [focused, setFocused] = useState<string | null>(null);
  const depthId = useId();
  const kindsKey = kinds.join(",");

  const loaded = useAsync<GraphData>(
    () => api.knowledgeGraph({ center, depth, limit: REQUEST_LIMIT, kinds, source }),
    [center, depth, kindsKey, source],
  );
  const graph = useMemo<PreparedGraph | null>(
    () => (loaded.data ? prepareGraph(loaded.data, { center, maxNodes: MAX_RENDER_NODES }) : null),
    [loaded.data, center],
  );
  useEffect(() => setFocused(null), [graph]);

  // Refetch when entities in view change (coalesced: extraction emits bursts).
  const { reload } = loaded;
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const shown = useMemo(() => new Set(graph?.nodes.map((n) => n.id) ?? []), [graph]);
  useEvent(events.onKnowledgeChanged, (c) => {
    // The overview may gain new entities from any change; a neighbourhood only from its own.
    const relevant = center === null || c.entities.length === 0 || c.entities.some((id) => shown.has(id));
    if (!relevant) return;
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(reload, 1500);
  });

  const toggleKind = (k: EntityKind) => setKinds((ks) => (ks.includes(k) ? ks.filter((x) => x !== k) : [...ks, k].sort()));
  const centerName = center ? (graph?.nodes.find((n) => n.id === center)?.name ?? center) : null;

  return (
    <div className="graph-panel">
      <div className="graph-controls">
        <CenterPicker onPick={(id) => onCenter(id)} />
        {center && (
          <span className="graph-centered">
            <Trans k="graph.centredOn" args={{ name: centerName }} tags={{ b: (c) => <b>{c}</b> }} />{" "}
            <button type="button" className="btn btn-ghost btn-small" onClick={() => onCenter(null)}>
              {t("graph.showOverview")}
            </button>
          </span>
        )}
        <label className="field-inline" htmlFor={depthId}>
          {t("graph.depth")}
          <select id={depthId} value={depth} onChange={(e) => setDepth(Number(e.target.value))} disabled={!center}>
            {DEPTHS.map((d) => (
              <option key={d} value={d}>
                {t("graph.hops", { n: d })}
              </option>
            ))}
          </select>
        </label>
        {sources.length > 0 && (
          <label className="field-inline" htmlFor={sourceId}>
            {t("graph.source")}
            <select id={sourceId} value={source ?? ""} onChange={(e) => setSource(e.target.value || null)}>
              <option value="">{t("graph.allSources")}</option>
              {sources.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </select>
          </label>
        )}
        <div className="segmented" role="group" aria-label={t("graph.displayLabel")}>
          <button type="button" aria-pressed={mode === "canvas"} onClick={() => setMode("canvas")}>
            {t("graph.modeGraph")}
          </button>
          <button type="button" aria-pressed={mode === "list"} onClick={() => setMode("list")}>
            {t("graph.modeList")}
          </button>
        </div>
      </div>
      <KindChips selected={kinds} onToggle={toggleKind} onClear={() => setKinds([])} label={t("graph.showKinds")} />

      <div className="graph-status" aria-live="polite">
        {graph && (
          <span className="muted small">
            {t("graph.counts", { nodes: graph.nodes.length, links: graph.links.length })}
          </span>
        )}
        {graph?.truncated && (
          <Badge tone="warn">
            {t("graph.truncated")}
            <span className="visually-hidden">{t("graph.truncatedDetail", { n: graph.nodes.length })}</span>
          </Badge>
        )}
        {graph?.truncated && (
          <span className="muted small" aria-hidden="true">
            {t("graph.truncatedHint")}
          </span>
        )}
        {loaded.loading && <Spinner label={t("graph.loading")} />}
      </div>

      <ErrorNote error={loaded.error} onRetry={loaded.reload} />
      {graph && graph.nodes.length === 0 && (
        <Empty title={t("graph.emptyTitle")}>
          {source ? t("graph.emptySource") : kinds.length > 0 ? t("graph.emptyFiltered") : t("graph.emptyNone")}
        </Empty>
      )}
      {graph && graph.nodes.length > 0 && mode === "canvas" && (
        <GraphCanvas
          graph={graph}
          seed={hashSeed(center ?? "overview")}
          selected={selected}
          focused={focused}
          onFocusChange={setFocused}
          onOpen={onOpen}
          onCenter={(id) => onCenter(id)}
        />
      )}
      {graph && graph.nodes.length > 0 && mode === "list" && (
        <GraphList graph={graph} selected={selected} onOpen={onOpen} onCenter={(id) => onCenter(id)} />
      )}
    </div>
  );
}

/** Toggle chips for entity kinds (multi-select; none selected means all). */
export function KindChips({
  selected,
  onToggle,
  onClear,
  label,
  single = false,
}: {
  selected: readonly string[];
  onToggle: (k: EntityKind) => void;
  onClear: () => void;
  label: string;
  /** Radio-like: one kind at a time. */
  single?: boolean;
}) {
  const t = useT();
  return (
    <div className="chips" role="group" aria-label={label}>
      <button type="button" className="chip" aria-pressed={selected.length === 0} onClick={onClear}>
        {t("graph.all")}
      </button>
      {ENTITY_KINDS.map((k) => (
        <button
          key={k}
          type="button"
          className="chip"
          aria-pressed={selected.includes(k)}
          onClick={() => onToggle(k)}
          title={single ? t("graph.only", { kind: k }) : undefined}
        >
          <span className={`kind-dot kind-${k}`} aria-hidden="true" />
          {kindPlural(k)}
        </button>
      ))}
    </div>
  );
}

/** Search box that picks an entity to centre the graph on. */
function CenterPicker({ onPick }: { onPick: (id: string) => void }) {
  const t = useT();
  const [text, setText] = useState("");
  const [results, setResults] = useState<EntitySummaryView[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const inputId = useId();
  const listId = useId();

  const search = async (e: FormEvent) => {
    e.preventDefault();
    const q = text.trim();
    if (!q) return;
    setBusy(true);
    setError(null);
    try {
      setResults(await api.knowledgeEntities({ text: q, limit: 8 }));
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="center-picker">
      <form onSubmit={search} role="search" className="inline-row">
        <label htmlFor={inputId} className="visually-hidden">
          {t("graph.centreLabel")}
        </label>
        <input
          id={inputId}
          type="search"
          placeholder={t("graph.centrePlaceholder")}
          value={text}
          onChange={(e) => {
            setText(e.target.value);
            if (!e.target.value) setResults(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape" && results) {
              e.preventDefault();
              setResults(null);
            }
          }}
          aria-controls={results ? listId : undefined}
          autoComplete="off"
        />
        <button type="submit" className="btn btn-small" disabled={busy || !text.trim()}>
          {busy ? <Spinner label={t("graph.searching")} /> : t("graph.find")}
        </button>
      </form>
      <ErrorNote error={error} />
      {results && (
        <ul id={listId} className="center-results" aria-label={t("graph.centreResults")}>
          {results.length === 0 && <li className="muted small">{t("graph.noMatch")}</li>}
          {results.map((r) => (
            <li key={r.id}>
              <button
                type="button"
                className="btn btn-ghost btn-small"
                onClick={() => {
                  onPick(r.id);
                  setResults(null);
                  setText("");
                }}
              >
                {r.name} <span className="muted">{kindLabel(r.kind)}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** The accessible alternative to the canvas: every drawn entity in a table. */
function GraphList({
  graph,
  selected,
  onOpen,
  onCenter,
}: {
  graph: PreparedGraph;
  selected: string | null;
  onOpen: (id: string) => void;
  onCenter: (id: string) => void;
}) {
  const t = useT();
  const [limit, setLimit] = useState(LIST_PAGE);
  const links = useMemo(() => {
    const m = new Map<string, string[]>();
    const name = new Map(graph.nodes.map((n) => [n.id, n.name]));
    for (const l of graph.links) {
      const a = typeof l.source === "string" ? l.source : l.source.id;
      const b = typeof l.target === "string" ? l.target : l.target.id;
      m.set(a, [...(m.get(a) ?? []), `${l.predicates.join(", ")} ${name.get(b) ?? b}`]);
      m.set(b, [...(m.get(b) ?? []), `${name.get(a) ?? a} ${l.predicates.join(", ")}`]);
    }
    return m;
  }, [graph]);
  const rows = graph.nodes.slice(0, limit);
  return (
    <div className="table-wrap" tabIndex={0} role="region" aria-label={t("graph.listLabel")}>
      <table>
        <thead>
          <tr>
            <th scope="col">{t("graph.col.entity")}</th>
            <th scope="col">{t("graph.col.kind")}</th>
            <th scope="col" className="num">
              {t("graph.col.hops")}
            </th>
            <th scope="col">{t("graph.col.connections")}</th>
            <th scope="col">
              <span className="visually-hidden">{t("graph.col.actions")}</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {rows.map((n) => {
            const conns = links.get(n.id) ?? [];
            return (
              <tr key={n.id} aria-current={n.id === selected ? "true" : undefined}>
                <th scope="row">{n.name}</th>
                <td>{kindLabel(n.kind)}</td>
                <td className="num">{n.id === graph.center ? "–" : n.depth}</td>
                <td className="small">
                  {conns.slice(0, 3).join("; ")}
                  {conns.length > 3 && <span className="muted"> {t("graph.andMore", { n: conns.length - 3 })}</span>}
                </td>
                <td className="nowrap">
                  <button type="button" className="btn btn-small" onClick={() => onOpen(n.id)} aria-label={t("graph.openLabel", { name: n.name })}>
                    {t("graph.open")}
                  </button>{" "}
                  {n.id !== graph.center && (
                    <button type="button" className="btn btn-ghost btn-small" onClick={() => onCenter(n.id)} aria-label={t("graph.centreOnLabel", { name: n.name })}>
                      {t("graph.centre")}
                    </button>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {graph.nodes.length > limit && (
        <p className="list-more">
          <button type="button" className="btn btn-ghost btn-small" onClick={() => setLimit((l) => l + LIST_PAGE)}>
            {t("graph.showMore", { n: Math.min(LIST_PAGE, graph.nodes.length - limit), total: graph.nodes.length })}
          </button>
        </p>
      )}
    </div>
  );
}
