/**
 * Localisation. Messages live in flat JSON catalogs under `./locales` (keys are dotted, e.g.
 * `models.install`), written in the ICU subset described in `./icu.ts`. `en.json` is the source
 * of truth: its keys define `MessageKey`, so a typo in a key is a type error.
 *
 * - `t(key, args)` formats in the current locale and works anywhere (components, helpers).
 * - Components call `useT()` (or `useLocale()`), which re-renders them when the locale changes.
 * - `<Trans k=… tags={{ code: (c) => <code>{c}</code> }} />` renders `<tag>` spans as elements.
 *
 * English is bundled; other catalogs load on demand. A missing key falls back to English.
 */
import { Fragment, useSyncExternalStore, type ReactNode } from "react";
import en from "./locales/en.json";
import { format, formatParts, numberFormat, dateFormat, type Args } from "./icu";
import { pseudoCatalog } from "./pseudo";

export type Messages = typeof en;
export type MessageKey = keyof Messages;
export type Catalog = Partial<Record<MessageKey, string>>;
export type { Args } from "./icu";

export interface LocaleInfo {
  code: string;
  /** The language's own name for itself, shown in the switcher. */
  name: string;
  dir: "ltr" | "rtl";
  /** A partial catalog kept to check layout (not offered unless `?dev`); missing keys show English. */
  stub?: boolean;
  /**
   * A pseudo-locale built from English by `./pseudo.ts` (no catalog file), to spot hard-coded
   * strings and layouts that can't grow. Never detected; offered only in development builds.
   */
  pseudo?: boolean;
}

// i18n-ignore-block: endonyms are shown untranslated, in their own language.
export const LOCALES: readonly LocaleInfo[] = [
  { code: "en", name: "English", dir: "ltr" },
  { code: "es", name: "Español", dir: "ltr" },
  { code: "de", name: "Deutsch", dir: "ltr" },
  { code: "fr", name: "Français", dir: "ltr" },
  { code: "pt-BR", name: "Português (Brasil)", dir: "ltr" },
  { code: "zh-CN", name: "简体中文", dir: "ltr" },
  { code: "ja", name: "日本語", dir: "ltr" },
  { code: "hi", name: "हिन्दी", dir: "ltr" },
  { code: "ar", name: "العربية", dir: "rtl" },
  { code: "en-XA", name: "Pseudo (en-XA)", dir: "ltr", pseudo: true },
];
// i18n-ignore-end

export const DEFAULT_LOCALE = "en";
const STORAGE_KEY = "nugget.locale";

const loaders = import.meta.glob<{ default: Catalog }>(["./locales/*.json", "!./locales/en.json"]);

function loaderFor(code: string) {
  return loaders[`./locales/${code}.json`];
}

/**
 * Best supported locale for a list of BCP 47 tags (most preferred first): an exact match, then
 * the same language (`pt-PT` → `pt-BR`, `zh-TW` → `zh-CN`, `es-MX` → `es`), else English.
 */
export function matchLocale(preferred: readonly string[], available: readonly LocaleInfo[] = LOCALES): string {
  const codes = available.filter((l) => !l.pseudo).map((l) => l.code);
  for (const tag of preferred) {
    let canonical: string;
    try {
      canonical = Intl.getCanonicalLocales(tag)[0] ?? tag;
    } catch {
      continue;
    }
    const exact = codes.find((c) => c.toLowerCase() === canonical.toLowerCase());
    if (exact) return exact;
    const lang = canonical.split("-")[0]!.toLowerCase();
    const same = codes.find((c) => c.split("-")[0]!.toLowerCase() === lang);
    if (same) return same;
  }
  return DEFAULT_LOCALE;
}

/** The OS language, as the webview reports it (WebView2, WKWebView and WebKitGTK follow the OS). */
export function systemLanguages(): string[] {
  if (typeof navigator === "undefined") return [];
  const langs = navigator.languages?.length ? [...navigator.languages] : [];
  if (navigator.language && !langs.includes(navigator.language)) langs.push(navigator.language);
  return langs;
}

export function savedLocale(): string | null {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return v && LOCALES.some((l) => l.code === v) ? v : null;
  } catch {
    return null;
  }
}

/** Saved choice, else the OS language. */
export function detectLocale(): string {
  return savedLocale() ?? matchLocale(systemLanguages(), LOCALES.filter((l) => !l.stub));
}

// ---- store ----

interface State {
  locale: string;
  catalog: Catalog;
}

let state: State = { locale: DEFAULT_LOCALE, catalog: en };
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

export function localeInfo(code: string = state.locale): LocaleInfo {
  return LOCALES.find((l) => l.code === code) ?? LOCALES[0]!;
}

function applyDocument(code: string) {
  if (typeof document === "undefined") return;
  document.documentElement.lang = code;
  document.documentElement.dir = localeInfo(code).dir;
}

/** Switch locale (loading its catalog) without saving the choice. Unknown codes fall back to English. */
export async function loadLocale(code: string): Promise<void> {
  const info = LOCALES.find((l) => l.code === code);
  let catalog: Catalog = en;
  if (info?.pseudo) {
    catalog = pseudoCatalog(en);
  } else if (info && code !== DEFAULT_LOCALE) {
    const load = loaderFor(code);
    if (load) catalog = (await load()).default;
  }
  state = { locale: info ? code : DEFAULT_LOCALE, catalog };
  applyDocument(state.locale);
  emit();
}

/** Switch locale and remember the choice. `null` goes back to following the OS. */
export async function setLocale(code: string | null): Promise<void> {
  try {
    if (code === null) localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, code);
  } catch {
    /* storage unavailable: the choice lasts this session */
  }
  await loadLocale(code ?? detectLocale());
}

/** Load the detected locale. Call before the first render so the UI doesn't flash English. */
export function initI18n(): Promise<void> {
  return loadLocale(detectLocale());
}

/** Use a catalog directly (tests). */
export function setCatalogForTest(locale: string, catalog: Catalog): void {
  state = { locale, catalog };
  emit();
}

export function currentLocale(): string {
  return state.locale;
}

function message(key: MessageKey): string {
  return state.catalog[key] ?? (en as Catalog)[key] ?? key;
}

/** Format `key` in the current locale. */
export function t(key: MessageKey, args?: Args): string {
  try {
    return format(message(key), state.locale, args);
  } catch {
    // A broken translation must never take down the page: fall back to English.
    return format((en as Catalog)[key] ?? key, DEFAULT_LOCALE, args);
  }
}

export type TFunction = typeof t;

function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}

/** Current locale; re-renders the component when it changes. */
export function useLocale(): LocaleInfo {
  const code = useSyncExternalStore(subscribe, () => state.locale, () => state.locale);
  return localeInfo(code);
}

/** `t`, re-rendering the calling component when the locale changes. */
export function useT(): TFunction {
  useSyncExternalStore(subscribe, () => state, () => state);
  return t;
}

// ---- rich text ----

export type TagRenderer = (children: ReactNode) => ReactNode;

/**
 * Render a message whose text contains `<tag>…</tag>` spans. Each tag must have a renderer in
 * `tags`; arguments are inserted as text, so they can never introduce markup.
 */
export function Trans({ k, args = {}, tags }: { k: MessageKey; args?: Args; tags: Record<string, TagRenderer> }) {
  useT();
  let parts: ReactNode[];
  const build = (src: string, locale: string) =>
    formatParts<ReactNode>(src, locale, args, (tag, children, i) => (
      <Fragment key={i}>{(tags[tag] ?? ((c: ReactNode) => c))(children)}</Fragment>
    ));
  try {
    parts = build(message(k), state.locale);
  } catch {
    parts = build((en as Catalog)[k] ?? k, DEFAULT_LOCALE);
  }
  return <>{parts.map((p, i) => (typeof p === "string" ? <Fragment key={`s${i}`}>{p}</Fragment> : p))}</>;
}

// ---- Intl helpers bound to the current locale ----

export function formatNumber(n: number, opts?: Intl.NumberFormatOptions): string {
  return numberFormat(state.locale, opts).format(n);
}

export function formatDate(d: Date | string | number, opts: Intl.DateTimeFormatOptions = { dateStyle: "medium" }): string {
  const date = d instanceof Date ? d : new Date(d);
  if (Number.isNaN(date.getTime())) return String(d);
  return dateFormat(state.locale, opts).format(date);
}

/** "a, b and c" in the current locale. */
export function formatList(items: readonly string[], type: "conjunction" | "disjunction" = "conjunction"): string {
  return new Intl.ListFormat(state.locale, { style: "long", type }).format(items);
}
