import type { AiKeyKind } from "../api";

/**
 * One-screen provider presets: each sets the API kind, address, a default model per tier
 * (`model` for chat, Ask, Double Tap and the agent; `lightModel` for quick tasks) and where to
 * get a key. Endpoints and model ids were checked against each provider's own docs (see the
 * Models guide). Triage never uses these: it stays on this computer.
 */
export interface Preset {
  id: PresetId;
  /** Brand name (not translated); `null` for the custom entry, which has a message instead. */
  label: string | null;
  api: AiKeyKind;
  /** Prefilled address; empty when the user must enter one. */
  baseUrl: string;
  model: string;
  lightModel: string;
  /** Where to create a key; `null` when there is no single page. */
  keyUrl: string | null;
  keyExample: string;
}

export type PresetId = "anthropic" | "openai" | "gemini" | "openrouter" | "groq" | "mistral" | "deepseek" | "together" | "compatible";

// i18n-ignore-block: brand names, URLs and model ids are code, not prose.
export const PRESETS: readonly Preset[] = [
  { id: "anthropic", label: "Anthropic", api: "anthropic", baseUrl: "https://api.anthropic.com", model: "claude-opus-5-5", lightModel: "claude-haiku-5-5", keyUrl: "https://platform.claude.com/settings/keys", keyExample: "sk-ant-…" },
  { id: "openai", label: "OpenAI", api: "openai_responses", baseUrl: "https://api.openai.com/v1", model: "gpt-6-astra", lightModel: "gpt-6-luna", keyUrl: "https://platform.openai.com/api-keys", keyExample: "sk-…" },
  { id: "gemini", label: "Google Gemini", api: "openai_compatible", baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai", model: "gemini-4-argon", lightModel: "gemini-3.5-flash-lite", keyUrl: "https://aistudio.google.com/apikey", keyExample: "AIza…" },
  { id: "openrouter", label: "OpenRouter", api: "openai_compatible", baseUrl: "https://openrouter.ai/api/v1", model: "anthropic/claude-sonnet-5.5", lightModel: "google/gemini-3.8-flash", keyUrl: "https://openrouter.ai/keys", keyExample: "sk-or-…" },
  { id: "groq", label: "Groq", api: "openai_compatible", baseUrl: "https://api.groq.com/openai/v1", model: "openai/gpt-oss-120b", lightModel: "llama-3.1-8b-instant", keyUrl: "https://console.groq.com/keys", keyExample: "gsk_…" },
  { id: "mistral", label: "Mistral", api: "openai_compatible", baseUrl: "https://api.mistral.ai/v1", model: "mistral-medium-latest", lightModel: "mistral-small-latest", keyUrl: "https://console.mistral.ai/api-keys", keyExample: "…" },
  { id: "deepseek", label: "DeepSeek", api: "openai_compatible", baseUrl: "https://api.deepseek.com", model: "deepseek-v4-pro", lightModel: "deepseek-flash", keyUrl: "https://platform.deepseek.com/api_keys", keyExample: "sk-…" },
  { id: "together", label: "Together AI", api: "openai_compatible", baseUrl: "https://api.together.ai/v1", model: "deepseek-ai/DeepSeek-V4-Pro-0813", lightModel: "Qwen/Qwen3.8-Flash", keyUrl: "https://api.together.ai/settings/projects/~current/api-keys", keyExample: "…" },
  { id: "compatible", label: null, api: "openai_compatible", baseUrl: "", model: "", lightModel: "", keyUrl: null, keyExample: "sk-…" },
];

/** Model placeholder for the custom entry. */
export const CUSTOM_MODEL_EXAMPLE = "qwen3:4b";
// i18n-ignore-end

export function preset(id: PresetId): Preset {
  return PRESETS.find((p) => p.id === id) ?? PRESETS[PRESETS.length - 1]!;
}

/** Whether `url` is a server on this computer (which needs no key). */
export function isLoopback(url: string): boolean {
  try {
    const h = new URL(url.trim()).hostname.replace(/^\[|\]$/g, "").toLowerCase();
    return h === "localhost" || h === "::1" || /^127\.\d+\.\d+\.\d+$/.test(h);
  } catch {
    return false;
  }
}
