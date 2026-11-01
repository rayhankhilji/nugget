/**
 * Saved and recent searches: the sidebar list, the Search page's lists, and the Save dialog.
 * Running one sends its encoded state to the Search page (`requestSearchState`).
 */
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { useT } from "../../i18n";
import { clearRecent, deleteSaved, saveSearch, useRecentSearches, useSavedSearches, type SavedSearch } from "../../savedSearches";
import { activeFilters, decode } from "../../searchState";
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

interface ListProps {
  onRun: (encoded: string) => void;
  announce?: (text: string) => void;
  /** The list went away (last one deleted, cleared): put focus somewhere sensible. */
  onEmpty?: () => void;
}

/** Search page: saved searches (run, delete). Deleting moves focus to a neighbour. */
export function SavedSearchList({ onRun, announce, onEmpty }: ListProps) {
  const t = useT();
  const saved = useSavedSearches();
  const listRef = useRef<HTMLUListElement>(null);
  /** The saved search to focus after a delete re-renders the list. */
  const focusNext = useRef<string | null>(null);
  useEffect(() => {
    const id = focusNext.current;
    if (id === null) return;
    focusNext.current = null;
    Array.from(listRef.current?.querySelectorAll<HTMLElement>("[data-saved-id]") ?? [])
      .find((el) => el.dataset.savedId === id)
      ?.focus();
  }, [saved]);
  const remove = (i: number) => {
    const s = saved[i]!;
    const neighbour = saved[i + 1] ?? saved[i - 1];
    focusNext.current = neighbour?.id ?? null;
    deleteSaved(s.id);
    announce?.(t("searchui.savedDeleted", { name: s.name }));
    if (!neighbour) onEmpty?.();
  };
  if (saved.length === 0) return null;
  return (
    <section className="sr-saved" aria-labelledby="search-saved-heading">
      <h2 id="search-saved-heading" className="sr-list-title">
        {t("searchui.saved")}
      </h2>
      <ul className="sr-chip-list" ref={listRef}>
        {saved.map((s, i) => (
          <li key={s.id} className="sr-saved-item">
            <button type="button" className="chip" data-saved-id={s.id} onClick={() => onRun(s.state)} aria-label={t("searchui.runSaved", { name: s.name })}>
              <bdi>{s.name}</bdi>
            </button>
            <button type="button" className="btn btn-ghost btn-small" onClick={() => remove(i)} aria-label={t("searchui.deleteSaved", { name: s.name })}>
              {t("searchui.delete")}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** Search page, empty box: recent searches and Clear recent. */
export function RecentSearchList({ onRun, announce, onEmpty }: ListProps) {
  const t = useT();
  const recent = useRecentSearches();
  if (recent.length === 0) return null;
  return (
    <section className="sr-recent" aria-labelledby="search-recent-heading">
      <div className="sr-list-head">
        <h2 id="search-recent-heading" className="sr-list-title">
          {t("searchui.recent")}
        </h2>
        <button
          type="button"
          className="btn btn-ghost btn-small"
          onClick={() => {
            clearRecent();
            announce?.(t("searchui.recentCleared"));
            onEmpty?.();
          }}
        >
          {t("searchui.clearRecent")}
        </button>
      </div>
      <ul className="sr-chip-list">
        {recent.map((r) => {
          const s = decode(r);
          const n = activeFilters(s);
          // The same query with other filters must not look like a duplicate.
          const label = n > 0 ? t("searchui.recentWithFilters", { query: s.q, n }) : s.q;
          return (
            <li key={r}>
              <button type="button" className="chip" onClick={() => onRun(r)} aria-label={t("searchui.runRecent", { query: label })}>
                <bdi>{label}</bdi>
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
