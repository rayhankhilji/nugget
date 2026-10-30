import { api, type ModelFit, type ModelInfo } from "../api";
import type { AiChoice } from "./machine";

/** Recommended first, then by size (smallest first), then by id. */
export function sortModels(list: readonly ModelInfo[]): ModelInfo[] {
  return [...list].sort(
    (a, b) => Number(b.recommended) - Number(a.recommended) || a.size_bytes - b.size_bytes || a.id.localeCompare(b.id),
  );
}

const SEARCH_TIERS = new Set(["embedding", "rerank"]);

/** A model that only serves search (embeddings, reranking), not chat and drafting. */
export function isSearchModel(m: Pick<ModelInfo, "id" | "tiers">, fit?: Pick<ModelFit, "tiers"> | null): boolean {
  const tiers = m.tiers?.length ? m.tiers : fit?.tiers ?? [];
  if (tiers.length) return tiers.every((t) => SEARCH_TIERS.has(t));
  return /(^|[-_])(embed|rerank)/i.test(m.id);
}

/**
 * What is already set up on an AI path, from the daemon (not from saved progress): an installed
 * chat model for `local`, a provider from the config file for `byok`. `null` when nothing is, or
 * it can't be told.
 */
export async function detectAiSetup(ai: AiChoice | null): Promise<string | null> {
  try {
    if (ai === "local") {
      const m = sortModels(await api.modelsList()).find((x) => x.installed && !isSearchModel(x));
      return m?.id ?? null;
    }
    if (ai === "byok") {
      // The runtime's own models are served as `local-*` providers; anything else was added to
      // the config file (with a key, or a server of the user's).
      const p = (await api.providersList()).find((x) => !x.id.startsWith("local-") && !x.error);
      return p?.id ?? null;
    }
  } catch {
    /* unknown: the step's own actions still count */
  }
  return null;
}
