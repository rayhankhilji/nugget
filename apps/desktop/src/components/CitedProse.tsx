import { useId, useMemo, useState } from "react";
import { appName, type CloudCitation } from "../cloud";
import { splitCitations } from "../citations";
import { formatTime } from "../format";
import { useOpener } from "../hooks";
import { useT } from "../i18n";

/** Only web links are offered; anything else from a source (file:, javascript:, app schemes) is not. */
export function webUrl(url: string | null | undefined): string | null {
  if (!url) return null;
  try {
    const u = new URL(url);
    return u.protocol === "https:" || u.protocol === "http:" ? u.href : null;
  } catch {
    return null;
  }
}

/**
 * Cloud-written text with citation chips, then its sources. A chip names the app its source came
 * from ("Gmail"), or shows the bare number when the source names none. After the text, an
 * "N sources" stack opens the list; a chip opens it at its source. A source with a web link opens
 * in the browser.
 */
export function CitedProse({ text, citations, sourcesHeading = true }: { text: string; citations: CloudCitation[]; sourcesHeading?: boolean }) {
  const t = useT();
  const prefix = useId();
  const opener = useOpener();
  const [focused, setFocused] = useState<number | null>(null);
  const [open, setOpen] = useState(false);
  const valid = useMemo(() => new Set(citations.map((c) => c.n)), [citations]);
  const byN = useMemo(() => new Map(citations.map((c) => [c.n, c])), [citations]);
  const apps = useMemo(() => [...new Set(citations.map((c) => c.source).filter(Boolean))], [citations]);
  const cite = (n: number) => {
    setFocused(n);
    setOpen(true);
    // After the list is shown.
    requestAnimationFrame(() => document.getElementById(`${prefix}-${n}`)?.focus());
  };
  return (
    <div className="cited">
      {text
        .split(/\n{2,}/)
        .filter((p) => p.trim())
        .map((para, k) => (
          <p key={k} className="cited-text" dir="auto">
            {splitCitations(para, valid).map((p, i) =>
              p.kind === "text" ? (
                <span key={i}>{p.text}</span>
              ) : (
                <CiteChip key={i} n={p.n} source={byN.get(p.n)?.source ?? null} author={byN.get(p.n)?.author ?? null} onClick={() => cite(p.n)} />
              ),
            )}
          </p>
        ))}
      {citations.length > 0 && (
        <>
          {sourcesHeading && <h3 className="visually-hidden">{t("cloud.ask.sources")}</h3>}
          <button type="button" className="sources-stack" aria-expanded={open} aria-controls={`${prefix}-list`} onClick={() => setOpen((o) => !o)}>
            <span className="sources-stack-glyphs" aria-hidden="true">
              {apps.slice(0, 3).map((a) => (
                <span key={a}>{appName(a).slice(0, 1)}</span>
              ))}
            </span>
            {t("cite.sources", { count: citations.length })}
          </button>
          <ol className="citations" id={`${prefix}-list`} hidden={!open}>
            {citations.map((c) => {
              const title = c.title || t("cloud.ask.untitled");
              const url = webUrl(c.url);
              return (
                <li key={c.n} id={`${prefix}-${c.n}`} tabIndex={-1} className="citation" aria-current={focused === c.n ? "true" : undefined}>
                  <span className="cite-n">{c.n}</span>
                  <span className="citation-body">
                    <span className="citation-title" dir="auto">
                      {title}
                    </span>
                    <span className="small muted">
                      {appName(c.source)}
                      {c.occurred_at && <> · {formatTime(c.occurred_at)}</>}
                    </span>
                  </span>
                  {url && (
                    <button type="button" className="btn btn-small btn-ghost" onClick={() => void opener.open(url, title)} aria-label={t("cloud.ask.openLabel", { title })}>
                      {t("cloud.ask.open")}
                    </button>
                  )}
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
      )}
    </div>
  );
}

/** A citation chip: who and where ("Jane · Gmail"), the app alone, or the bare number. */
function CiteChip({ n, source, author, onClick }: { n: number; source: string | null; author: string | null; onClick: () => void }) {
  const t = useT();
  if (!source) {
    return (
      <button type="button" className="cite" onClick={onClick} aria-label={t("cloud.ask.citationLabel", { n })}>
        {n}
      </button>
    );
  }
  const app = appName(source);
  const name = author ? `${author} · ${app}` : app;
  return (
    <button type="button" className="cite cite-app" onClick={onClick} aria-label={t("cite.label", { name, n })}>
      <span className="cite-glyph" aria-hidden="true">
        {app.slice(0, 1)}
      </span>
      {name}
    </button>
  );
}
