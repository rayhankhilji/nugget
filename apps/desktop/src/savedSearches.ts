/**
 * Saved searches (named, encoded `SearchState`s) and recent searches (the last 10 distinct ones),
 * kept in this device's localStorage only. Every storage access is guarded: without storage the
 * lists are simply empty and saving does nothing lasting.
 */
import { useSyncExternalStore } from "react";
import { decode, encode } from "./searchState";

export interface SavedSearch {
  id: string;
  name: string;
  /** An encoded `SearchState` (see searchState.ts). */
  state: string;
}

export const SAVED_KEY = "nugget.search.saved.v1";
export const RECENT_KEY = "nugget.search.recent.v1";
export const MAX_RECENT = 10;
export const MAX_SAVED = 100;
const MAX_NAME = 120;

function rawOf(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function parse(raw: string | null): unknown {
  try {
    return raw ? (JSON.parse(raw) as unknown) : null;
  } catch {
    return null;
  }
}

function write(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* storage unavailable or full: keep going without it */
  }
}

function remove(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    /* ignore */
  }
}

const listeners = new Set<() => void>();
/** Parsed lists, keyed by the stored text they came from (stable snapshots for React). */
let savedCache: { raw: string | null; list: SavedSearch[] } | null = null;
let recentCache: { raw: string | null; list: string[] } | null = null;

function changed(): void {
  for (const l of listeners) l();
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  const onStorage = (e: StorageEvent) => {
    if (e.key === null || e.key === SAVED_KEY || e.key === RECENT_KEY) changed();
  };
  window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(l);
    window.removeEventListener("storage", onStorage);
  };
}

/** Saved searches, in the order they were saved. Junk entries are dropped. */
export function listSaved(): SavedSearch[] {
  const text = rawOf(SAVED_KEY);
  if (savedCache && savedCache.raw === text) return savedCache.list;
  const raw = parse(text);
  const out: SavedSearch[] = [];
  if (Array.isArray(raw)) {
    for (const r of raw) {
      if (!r || typeof r !== "object") continue;
      const { id, name, state } = r as Record<string, unknown>;
      if (typeof id === "string" && typeof name === "string" && name.trim() && typeof state === "string") {
        out.push({ id, name: name.trim().slice(0, MAX_NAME), state: encode(decode(state)) });
      }
    }
  }
  savedCache = { raw: text, list: out };
  return out;
}

function newId(): string {
  try {
    return crypto.randomUUID();
  } catch {
    return `s${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
  }
}

/** Save `state` under `name`; a search with the same name is replaced. Returns the entry. */
export function saveSearch(name: string, state: string): SavedSearch {
  const clean = name.trim().slice(0, MAX_NAME);
  const encoded = encode(decode(state));
  const list = listSaved();
  const same = list.find((s) => s.name.toLocaleLowerCase() === clean.toLocaleLowerCase());
  const entry: SavedSearch = { id: same?.id ?? newId(), name: clean, state: encoded };
  const next = same ? list.map((s) => (s.id === same.id ? entry : s)) : [...list, entry].slice(-MAX_SAVED);
  write(SAVED_KEY, next);
  changed();
  return entry;
}

export function renameSaved(id: string, name: string): void {
  const clean = name.trim().slice(0, MAX_NAME);
  if (!clean) return;
  write(SAVED_KEY, listSaved().map((s) => (s.id === id ? { ...s, name: clean } : s)));
  changed();
}

export function deleteSaved(id: string): void {
  write(SAVED_KEY, listSaved().filter((s) => s.id !== id));
  changed();
}

/** Recent searches (encoded states), newest first. */
export function listRecent(): string[] {
  const text = rawOf(RECENT_KEY);
  if (recentCache && recentCache.raw === text) return recentCache.list;
  const raw = parse(text);
  const out: string[] = [];
  if (Array.isArray(raw)) {
    for (const r of raw) {
      if (typeof r !== "string") continue;
      const e = encode(decode(r));
      if (decode(e).q && !out.includes(e)) out.push(e);
    }
  }
  recentCache = { raw: text, list: out.slice(0, MAX_RECENT) };
  return recentCache.list;
}

/** Remember a search that ran (only ones with a query); moves a repeat to the front. */
export function addRecent(state: string): void {
  const e = encode({ ...decode(state), timeline: null });
  if (!decode(e).q) return;
  write(RECENT_KEY, [e, ...listRecent().filter((r) => r !== e)].slice(0, MAX_RECENT));
  changed();
}

export function clearRecent(): void {
  remove(RECENT_KEY);
  changed();
}

export function useSavedSearches(): SavedSearch[] {
  return useSyncExternalStore(subscribe, listSaved, listSaved);
}

export function useRecentSearches(): string[] {
  return useSyncExternalStore(subscribe, listRecent, listRecent);
}
