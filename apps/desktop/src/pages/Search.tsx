import { useState, type FormEvent } from "react";
import { api, type SearchHit } from "../api";
import { Empty, ErrorNote, PageHeader, Spinner } from "../components/ui";

const LIMITS = [10, 25, 50];

export function SearchPage() {
  const [query, setQuery] = useState("");
  const [limit, setLimit] = useState(25);
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [searched, setSearched] = useState("");
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);

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
      <PageHeader title="Search" description="Full-text and semantic search over everything Nugget has indexed." />
      <form className="search-bar" role="search" onSubmit={run}>
        <label htmlFor="search-q" className="visually-hidden">
          Search query
        </label>
        <input
          id="search-q"
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search your notes, mail, files…"
          autoFocus
        />
        <label className="field-inline">
          <span>Show</span>
          <select value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
            {LIMITS.map((l) => (
              <option key={l} value={l}>
                {l}
              </option>
            ))}
          </select>
        </label>
        <button type="submit" className="btn btn-primary" disabled={busy || !query.trim()}>
          {busy ? <Spinner label="Searching" /> : "Search"}
        </button>
      </form>

      <ErrorNote error={error} onRetry={() => void run()} />

      <div aria-live="polite" className="visually-hidden">
        {hits && `${hits.length} result${hits.length === 1 ? "" : "s"} for ${searched}`}
      </div>

      {hits && hits.length === 0 && (
        <Empty title={`No results for “${searched}”`}>
          Try different words. Sources are indexed in the background, so new items can take a minute to appear.
        </Empty>
      )}
      {hits && hits.length > 0 && (
        <ol className="results">
          {hits.map((h, i) => (
            <li key={`${h.item.source}\u0000${h.item.key}\u0000${i}`} className="result">
              <div className="result-head">
                <span className="result-title">{h.title || h.item.key}</span>
                <span className="muted small" title="Relevance score">
                  {h.score.toFixed(3)}
                </span>
              </div>
              <p className="result-snippet">{h.snippet}</p>
              <p className="muted small">
                {h.item.source} · <code>{h.item.key}</code>
              </p>
            </li>
          ))}
        </ol>
      )}
      {!hits && !error && <Empty title="Type a query to search">Results come only from this device.</Empty>}
    </div>
  );
}
