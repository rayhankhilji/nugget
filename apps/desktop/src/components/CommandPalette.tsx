import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useT } from "../i18n";

/** The Tab key's name on its keycap, the same in every language. */
const TAB_KEY = "Tab";

export interface PaletteItem {
  id: string;
  label: string;
  /** Extra words that should find this item (synonyms, translated). */
  keywords?: string;
  /** Shortcut hint shown at the end of the row, e.g. `Ctrl+1`. */
  hint?: string;
  /** What kind of thing this is: the section it is listed under ("Pages", "Mail"). */
  group: string;
  run: () => void;
  /** A second line under the label (a person's apps and item count). */
  detail?: string;
  /** Show an initial avatar before the label (people). */
  avatar?: boolean;
  /** For search results: where it came from and who wrote it (for `in:` and `from:`). */
  source?: string;
  sourceName?: string;
  author?: string | null;
}

/** `from:` and `in:` filters typed into the palette, and the rest of the query. */
export interface PaletteQuery {
  text: string;
  from: string | null;
  in: string | null;
}

export function parsePaletteQuery(q: string): PaletteQuery {
  let from: string | null = null;
  let inn: string | null = null;
  const rest: string[] = [];
  for (const word of q.split(/\s+/).filter(Boolean)) {
    const m = /^(from|in):(.+)$/i.exec(word);
    if (m && m[1]!.toLowerCase() === "from") from = m[2]!;
    else if (m) inn = m[2]!;
    else rest.push(word);
  }
  return { text: rest.join(" "), from, in: inn };
}

/** Search results that pass the `from:` and `in:` filters. */
export function applyPaletteFilters(items: readonly PaletteItem[], q: PaletteQuery): PaletteItem[] {
  const has = (hay: string | null | undefined, needle: string) => fold(hay ?? "").includes(fold(needle));
  return items.filter((i) => (!q.from || has(i.author, q.from)) && (!q.in || has(i.sourceName, q.in) || has(i.source, q.in)));
}

/** Lower-case and strip accents, so "resume" finds "Résumé". */
function fold(s: string): string {
  return s.normalize("NFD").replace(/\p{M}/gu, "").toLocaleLowerCase();
}

/** `needle`'s characters appear in `hay` in order (loose "fuzzy" match). */
function subsequence(needle: string, hay: string): boolean {
  let i = 0;
  for (const ch of hay) if (ch === needle[i] && ++i === needle.length) return true;
  return needle.length === 0;
}

/** Score one query word against an item; 0 means no match. Label hits beat keyword hits. */
function scoreWord(word: string, label: string, keywords: string): number {
  if (label.startsWith(word)) return 8;
  if (label.split(/[\s\-–—/]+/).some((w) => w.startsWith(word))) return 6;
  if (label.includes(word)) return 4;
  if (keywords.split(/[\s,]+/).some((w) => w.startsWith(word))) return 3;
  if (keywords.includes(word)) return 2;
  if (word.length > 1 && subsequence(word, label)) return 1;
  return 0;
}

/**
 * Items matching every word of `query`, best first (ties keep their original order). An empty
 * query returns everything.
 */
export function filterItems<T extends Pick<PaletteItem, "label" | "keywords">>(items: readonly T[], query: string): T[] {
  const words = fold(query).split(/\s+/).filter(Boolean);
  if (words.length === 0) return [...items];
  const scored: { item: T; score: number; i: number }[] = [];
  items.forEach((item, i) => {
    const label = fold(item.label);
    const keywords = fold(item.keywords ?? "");
    let score = 0;
    for (const w of words) {
      const s = scoreWord(w, label, keywords);
      if (s === 0) return;
      score += s;
    }
    scored.push({ item, score, i });
  });
  return scored.sort((a, b) => b.score - a.score || a.i - b.i).map((s) => s.item);
}

/**
 * ⌘K command palette: a modal dialog holding a combobox and its listbox. Focus stays in the
 * input; arrows move the active option (announced through `aria-activedescendant`), Enter runs it,
 * Escape closes. Mount to open, unmount to close; focus returns to whatever had it before.
 */
export function CommandPalette({
  items,
  queryItem,
  search,
  onAsk,
  onClose,
}: {
  items: readonly PaletteItem[];
  /** An extra item built from the typed text (e.g. "Search for …"), listed after the matches. */
  queryItem?: (query: string) => PaletteItem | null;
  /** Live results for the typed text (filters removed), grouped by `group`. */
  search?: (text: string) => Promise<PaletteItem[]>;
  /** Tab: ask Heed the typed question instead. */
  onAsk?: (question: string) => void;
  onClose: () => void;
}) {
  const t = useT();
  const ref = useRef<HTMLDialogElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const baseId = useId();
  const listId = `${baseId}-list`;
  const titleId = `${baseId}-title`;
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  const parsed = useMemo(() => parsePaletteQuery(query), [query]);
  const filtering = parsed.from != null || parsed.in != null;
  const [live, setLive] = useState<{ text: string; items: PaletteItem[] }>({ text: "", items: [] });
  useEffect(() => {
    const text = parsed.text.trim();
    if (!search || (!text && !filtering) || text.length < 2) {
      setLive({ text: "", items: [] });
      return;
    }
    let alive = true;
    const timer = setTimeout(() => {
      search(text).then(
        (found) => alive && setLive({ text, items: found }),
        () => alive && setLive({ text, items: [] }),
      );
    }, 200);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [search, parsed.text, filtering]);
  // Pages and actions match the words; filters only make sense for search results.
  const matches = useMemo(() => (filtering ? [] : filterItems(items, parsed.text)), [items, parsed.text, filtering]);
  const found = useMemo(() => applyPaletteFilters(live.items, parsed), [live.items, parsed]);
  const extra = parsed.text.trim() ? queryItem?.(query.trim()) ?? null : null;
  const results = [...matches, ...found, ...(extra ? [extra] : [])];
  // Sections in order of first appearance; options stay in `results` order for the arrows.
  const sections = useMemo(() => {
    const out: { group: string; items: { item: PaletteItem; i: number }[] }[] = [];
    results.forEach((item, i) => {
      const last = out[out.length - 1];
      if (last && last.group === item.group) last.items.push({ item, i });
      else out.push({ group: item.group, items: [{ item, i }] });
    });
    return out;
  }, [results]);
  const removeFilter = (kind: "from" | "in") => {
    setQuery(query.split(/\s+/).filter((w) => !new RegExp(`^${kind}:`, "i").test(w)).join(" ") + (query.endsWith(" ") ? " " : ""));
    setActive(0);
    inputRef.current?.focus();
  };
  const activeIndex = results.length ? Math.min(active, results.length - 1) : -1;
  const optionId = (i: number) => `${baseId}-opt-${i}`;

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (!el.open) el.showModal();
    inputRef.current?.focus();
    // The platform's own Escape handling: let React state decide instead.
    const onCancel = (e: Event) => {
      e.preventDefault();
      onCloseRef.current();
    };
    el.addEventListener("cancel", onCancel);
    return () => {
      el.removeEventListener("cancel", onCancel);
      // Restore focus only if the chosen command didn't already move it somewhere useful.
      const active = document.activeElement;
      const lost = !active || active === document.body || el.contains(active);
      if (el.open) el.close();
      if (lost && opener?.isConnected) opener.focus();
    };
  }, []);

  // Keep the active option in view while arrowing through a long list.
  useEffect(() => {
    if (activeIndex < 0) return;
    document.getElementById(optionId(activeIndex))?.scrollIntoView?.({ block: "nearest" });
  }, [activeIndex]);

  const runAt = (i: number) => {
    const item = results[i];
    if (!item) return;
    onClose();
    item.run();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (e.nativeEvent.isComposing) return;
    const n = results.length;
    switch (e.key) {
      case "ArrowDown":
        if (n) setActive((activeIndex + 1) % n);
        break;
      case "ArrowUp":
        if (n) setActive((activeIndex - 1 + n) % n);
        break;
      case "Home":
        if (!n || !e.ctrlKey) return;
        setActive(0);
        break;
      case "End":
        if (!n || !e.ctrlKey) return;
        setActive(n - 1);
        break;
      case "Enter":
        runAt(activeIndex);
        break;
      case "Escape":
        onClose();
        break;
      case "Tab":
        // The input is the only stop; with a question typed, Tab asks Heed instead.
        if (onAsk && !e.shiftKey && parsed.text.trim()) {
          onClose();
          onAsk(parsed.text.trim());
        }
        break;
      default:
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  };

  return (
    <dialog ref={ref} className="dialog palette" role="dialog" aria-modal="true" aria-labelledby={titleId} onKeyDown={onKeyDown}>
      <h2 id={titleId} className="visually-hidden">
        {t("palette.title")}
      </h2>
      <div className="palette-input">
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
          <circle cx="7" cy="7" r="4.8" fill="none" stroke="currentColor" strokeWidth="1.6" />
          <path d="M10.6 10.6L14 14" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
        </svg>
        <input
          ref={inputRef}
          type="text"
          dir="auto"
          role="combobox"
          aria-expanded="true"
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={activeIndex >= 0 ? optionId(activeIndex) : undefined}
          aria-label={t("palette.inputLabel")}
          placeholder={t("palette.placeholder")}
          autoComplete="off"
          spellCheck={false}
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setActive(0);
          }}
        />
      </div>
      {(parsed.from || parsed.in) && (
        <div className="palette-filters">
          {(["from", "in"] as const).map((k) =>
            parsed[k] ? (
              <button key={k} type="button" className="chip palette-filter" onMouseDown={(e) => e.preventDefault()} onClick={() => removeFilter(k)} aria-label={t("palette.filter.remove", { label: t(`palette.filter.${k}`, { value: parsed[k]! }) })}>
                {t(`palette.filter.${k}`, { value: parsed[k]! })}
                <span aria-hidden="true">×</span>
              </button>
            ) : null,
          )}
        </div>
      )}
      {results.length === 0 && <p className="palette-empty muted">{t("palette.empty")}</p>}
      <div id={listId} role="listbox" aria-label={t("palette.resultsLabel")} className="palette-list">
        {sections.map((sec, k) => (
          <div key={`${sec.group}-${k}`} role="group" aria-labelledby={`${baseId}-sec-${k}`} className="palette-section">
            <div id={`${baseId}-sec-${k}`} role="presentation" className="palette-section-head">
              {sec.group}
            </div>
            {sec.items.map(({ item, i }) => (
              <div
                key={item.id}
                id={optionId(i)}
                role="option"
                aria-selected={i === activeIndex}
                className="palette-option"
                onMouseMove={() => i !== activeIndex && setActive(i)}
                // Keep focus in the input so the combobox keeps working after a click.
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => runAt(i)}
              >
                {item.avatar && (
                  <span className="palette-avatar" aria-hidden="true">
                    {item.label.slice(0, 1).toLocaleUpperCase()}
                  </span>
                )}
                <span className="palette-text">
                  <span className="palette-label">{item.label}</span>
                  {item.detail && <span className="palette-detail">{item.detail}</span>}
                </span>
                <span className="palette-meta">
                  {item.sourceName && <span className="palette-source">{item.sourceName}</span>}
                  {item.hint && <kbd dir="ltr">{item.hint}</kbd>}
                  {i === activeIndex && (
                    <span className="palette-open" aria-hidden="true">
                      {t("palette.open")} ↵
                    </span>
                  )}
                </span>
              </div>
            ))}
          </div>
        ))}
      </div>
      <p className="visually-hidden" role="status" aria-live="polite">
        {query.trim() ? t("palette.count", { count: results.length }) : ""}
      </p>
      <p className="palette-footer muted small" aria-hidden="true">
        <kbd dir="ltr">↑↓</kbd> {t("palette.hint.move")} · <kbd dir="ltr">{t("shortcuts.key.enter")}</kbd> {t("palette.hint.choose")}
        {onAsk && (
          <>
            {" "}
            · <kbd dir="ltr">{TAB_KEY}</kbd> {t("palette.hint.ask")}
          </>
        )}{" "}
        · <kbd dir="ltr">{t("shortcuts.key.esc")}</kbd> {t("palette.hint.close")}
        {query.trim() && <span className="palette-count">{t("palette.count", { count: results.length })}</span>}
      </p>
    </dialog>
  );
}
