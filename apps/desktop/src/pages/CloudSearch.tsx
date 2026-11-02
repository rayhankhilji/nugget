import { useState, type FormEvent } from "react";
import { appName, cloudApi, type CloudSearchResponse } from "../cloud";
import { AppGlyph } from "../components/AppLogo";
import { formatTime } from "../format";
import { useOpener } from "../hooks";
import { useT } from "../i18n";
import { webUrl } from "../components/CitedProse";
import { EmptyState, ErrorNote, PageHeader, Spinner } from "../components/ui";

/**
 * Search over Heed Cloud (`/v1/search`): one box, filters by source app, results with their app
 * and date. Only web links open, in the browser.
 */
export function CloudSearch() {
  const t = useT();
  const opener = useOpener();
  const [query, setQuery] = useState("");
  const [sources, setSources] = useState<string[]>([]);
  const [res, setRes] = useState<CloudSearchResponse | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const run = async (q: string, only: string[]) => {
    if (!q.trim()) return;
    setBusy(true);
    setError(null);
    try {
      setRes(await cloudApi.search({ q: q.trim(), ...(only.length ? { sources: only } : {}), limit: 50 }));
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  const submit = (e: FormEvent) => {
    e.preventDefault();
    void run(query, sources);
  };
  const toggle = (s: string) => {
    const next = sources.includes(s) ? sources.filter((x) => x !== s) : [...sources, s];
    setSources(next);
    void run(res?.query ?? query, next);
  };

  return (
    <div className="page">
      <PageHeader title={t("search.title")} description={t("searchCloud.description")} />
      <form className="search-bar" role="search" onSubmit={submit}>
        <input type="search" aria-label={t("search.query")} placeholder={t("search.placeholder")} value={query} onChange={(e) => setQuery(e.target.value)} />
        <button type="submit" className="btn btn-primary" aria-disabled={busy || undefined}>
          {busy ? <Spinner label={t("search.searching")} /> : t("search.submit")}
        </button>
      </form>
      <ErrorNote error={error} onRetry={() => void run(query, sources)} />
      {res && res.facets.sources.length > 0 && (
        <div className="chips" role="group" aria-label={t("searchCloud.sources")}>
          {res.facets.sources.map((f) => (
            <button key={f.value} type="button" className="chip" aria-pressed={sources.includes(f.value)} onClick={() => toggle(f.value)}>
              {appName(f.value)} <span className="muted">{f.count}</span>
            </button>
          ))}
        </div>
      )}
      {res &&
        (res.results.length === 0 ? (
          <EmptyState title={t("search.noResults", { query: res.query })}>{t("searchCloud.noResultsHint")}</EmptyState>
        ) : (
          <>
            <p className="muted" role="status">
              {t("search.resultsFor", { n: res.total, query: res.query })}
            </p>
            <ol className="results">
              {res.results.map((r) => {
                const title = r.title || t("cloud.ask.untitled");
                const url = webUrl(r.url);
                return (
                  <li key={r.item_id} className="result">
                    <div className="result-head">
                      <span className="result-title" dir="auto">
                        {title}
                      </span>
                      {url && (
                        <button type="button" className="btn btn-small btn-ghost" onClick={() => void opener.open(url, title)} aria-label={t("search.openTitle", { title })}>
                          {t("search.open")}
                        </button>
                      )}
                    </div>
                    <p className="small muted result-meta">
                      <AppGlyph source={r.source} />
                      {appName(r.source)}
                      {r.author && <> · {r.author}</>}
                      {r.occurred_at && <> · {formatTime(r.occurred_at)}</>}
                    </p>
                    <p className="result-snippet" dir="auto">
                      {r.snippet}
                    </p>
                  </li>
                );
              })}
            </ol>
            {opener.status && (
              <p className={opener.status.error ? "note note-error" : "small muted"} role="status">
                {opener.status.text}
              </p>
            )}
          </>
        ))}
    </div>
  );
}
