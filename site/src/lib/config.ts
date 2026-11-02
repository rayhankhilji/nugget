/** Public configuration, read at build time. Every value here ships to the browser. */
const env = import.meta.env;

const trim = (v: unknown): string => (typeof v === "string" ? v.trim().replace(/\/+$/, "") : "");

export const config = {
  supabaseUrl: trim(env.NEXT_PUBLIC_SUPABASE_URL),
  supabaseAnonKey: typeof env.NEXT_PUBLIC_SUPABASE_ANON_KEY === "string" ? env.NEXT_PUBLIC_SUPABASE_ANON_KEY.trim() : "",
  cloudUrl: trim(env.NEXT_PUBLIC_CLOUD_URL),
  downloadMacArm: trim(env.NEXT_PUBLIC_DOWNLOAD_MAC_ARM_URL),
  downloadMacIntel: trim(env.NEXT_PUBLIC_DOWNLOAD_MAC_INTEL_URL),
} as const;

export const supportEmail = "support@heed.app";
export const privacyEmail = "privacy@heed.app";
