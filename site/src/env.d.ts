/// <reference types="astro/client" />

interface ImportMetaEnv {
  readonly NEXT_PUBLIC_SUPABASE_URL?: string;
  readonly NEXT_PUBLIC_SUPABASE_ANON_KEY?: string;
  readonly NEXT_PUBLIC_CLOUD_URL?: string;
  readonly NEXT_PUBLIC_DOWNLOAD_MAC_ARM_URL?: string;
  readonly NEXT_PUBLIC_DOWNLOAD_MAC_INTEL_URL?: string;
}
interface ImportMeta {
  readonly env: ImportMetaEnv;
}
