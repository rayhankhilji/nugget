import { useId, useMemo, useState } from "react";
import type { CloudCitation } from "../cloud";
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
 * Cloud-written text with `[n]` citation chips, then its sources. A chip moves focus to its
 * source; a source with a web link opens it in the browser.
 */
export function CitedProse({ text, citations, sourcesHeading = true }: { text: string; citations: CloudCitation[]; sourcesHeading?: boolean }) {
  const t = useT();
  const prefix = useId();
  const opener = useOpener();
  const [focused, setFocused] = useState<number | null>(null);
  const valid = useMemo(() => new Set(citations.map((c) => c.n)), [citations]);
  const cite = (n: number) => {
    setFocused(n);
    document.getElementById(`${prefix}-${n}`)?.focus();
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
                <button key={i} type="button" className="cite" onClick={() => cite(p.n)} aria-label={t("cloud.ask.citationLabel", { n: p.n })}>
                  {p.n}
                </button>
              ),
            )}
          </p>
        ))}
      {citations.length > 0 && (
        <>
          {sourcesHeading && <h3 className="cited-heading">{t("cloud.ask.sources")}</h3>}
          <ol className="citations">
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
                      {c.source}
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
