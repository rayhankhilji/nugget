import { createClient, type SupabaseClient, type Session } from "@supabase/supabase-js";
import { config } from "./config";

let client: SupabaseClient | null = null;

/** The browser Supabase client, or null when the site was built without Supabase settings. */
export function supabase(): SupabaseClient | null {
  if (!config.supabaseUrl || !config.supabaseAnonKey) return null;
  client ??= createClient(config.supabaseUrl, config.supabaseAnonKey, {
    auth: { persistSession: true, autoRefreshToken: true, detectSessionInUrl: true, flowType: "implicit" },
  });
  return client;
}

export async function currentSession(): Promise<Session | null> {
  const sb = supabase();
  if (!sb) return null;
  const { data } = await sb.auth.getSession();
  return data.session;
}

/** Only same-site paths are allowed as a post-sign-in destination. */
export function safeNext(raw: string | null, fallback = "/account"): string {
  if (!raw || !raw.startsWith("/") || raw.startsWith("//") || raw.includes("\\")) return fallback;
  return raw;
}
