/**
 * Saved and recent searches: the sidebar list, the Search page's lists, and the Save dialog.
 * Running one sends its encoded state to the Search page (`requestSearchState`).
 */
import { useId, useState, type FormEvent } from "react";
import { useT } from "../../i18n";
import { clearRecent, deleteSaved, saveSearch, useRecentSearches, useSavedSearches, type SavedSearch } from "../../savedSearches";
import { decode } from "../../searchState";
import { Dialog } from "../Dialog";

/** Sidebar: the saved searches under the nav (nothing when there are none). */
export function SavedSearchNav({ onRun }: { onRun: (s: SavedSearch) => void }) {
  const t = useT();
  const saved = useSavedSearches();
  const id = useId();
  if (saved.length === 0) return null;
  return (
    <div className="nav-saved">
      <p className="nav-group-label" id={id}>
        {t("searchui.saved")}
      </p>
      <ul aria-labelledby={id}>
        {saved.map((s) => (
          <li key={s.id}>
            <button type="button" className="nav-item nav-saved-item" onClick={() => onRun(s)} aria-label={t("searchui.runSaved", { name: s.name })}>
              <bdi>{s.name}</bdi>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** Search page: saved searches (run, delete). */
export function SavedSearchList({ onRun }: { onRun: (encoded: string) => void }) {
  const t = useT();
  const saved = useSavedSearches();
  if (saved.length === 0) return null;
  return (
    <section className="sr-saved" aria-labelledby="search-saved-heading">
      <h2 id="search-saved-heading" className="sr-list-title">
        {t("searchui.saved")}
      </h2>
      <ul className="sr-chip-list">
        {saved.map((s) => (
          <li key={s.id} className="sr-saved-item">
            <button type="button" className="chip" onClick={() => onRun(s.state)} aria-label={t("searchui.runSaved", { name: s.name })}>
              <bdi>{s.name}</bdi>
            </button>
            <button type="button" className="btn btn-ghost btn-small" onClick={() => deleteSaved(s.id)} aria-label={t("searchui.deleteSaved", { name: s.name })}>
              {t("searchui.delete")}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** Search page, empty box: recent searches and Clear recent. */
export function RecentSearchList({ onRun }: { onRun: (encoded: string) => void }) {
  const t = useT();
  const recent = useRecentSearches();
  if (recent.length === 0) return null;
  return (
    <section className="sr-recent" aria-labelledby="search-recent-heading">
      <div className="sr-list-head">
        <h2 id="search-recent-heading" className="sr-list-title">
          {t("searchui.recent")}
        </h2>
        <button type="button" className="btn btn-ghost btn-small" onClick={clearRecent}>
          {t("searchui.clearRecent")}
        </button>
      </div>
      <ul className="sr-chip-list">
        {recent.map((r) => {
          const s = decode(r);
          return (
            <li key={r}>
              <button type="button" className="chip" onClick={() => onRun(r)} aria-label={t("searchui.runRecent", { query: s.q })}>
                <bdi>{s.q}</bdi>
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

/** Asks for a name and saves `state` under it. */
export function SaveSearchDialog({ state, defaultName, onClose, onSaved }: { state: string; defaultName: string; onClose: () => void; onSaved: (name: string) => void }) {
  const t = useT();
  const [name, setName] = useState(defaultName);
  const inputId = useId();
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const n = name.trim();
    if (!n) return;
    saveSearch(n, state);
    onSaved(n);
    onClose();
  };
  return (
    <Dialog title={t("searchui.saveTitle")} description={t("searchui.saveIntro")} onClose={onClose}>
      <form onSubmit={submit} className="sr-save-form">
        <label htmlFor={inputId}>{t("searchui.saveName")}</label>
        <input id={inputId} type="text" dir="auto" value={name} maxLength={120} onChange={(e) => setName(e.target.value)} data-autofocus required />
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose}>
            {t("common.cancel")}
          </button>
          <button type="submit" className="btn btn-primary" disabled={!name.trim()}>
            {t("searchui.save")}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
