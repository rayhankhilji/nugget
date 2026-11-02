// @ts-check
import { defineConfig } from "astro/config";
import sitemap from "@astrojs/sitemap";
import smartquotes from "./integrations/smartquotes.mjs";

// The public URL the site is served from, used for canonical links and the sitemap. On Vercel,
// VERCEL_PROJECT_PRODUCTION_URL (the production domain, without a scheme) is set automatically.
const site =
  process.env.SITE_URL ||
  (process.env.VERCEL_PROJECT_PRODUCTION_URL ? `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL}` : "https://heed.pages.dev");

export default defineConfig({
  site,
  output: "static",
  // The smoke test builds a second copy with test settings into its own folder.
  outDir: process.env.ASTRO_OUT_DIR || "./dist",
  trailingSlash: "never",
  build: { format: "file", inlineStylesheets: "always" },
  prefetch: { prefetchAll: false, defaultStrategy: "hover" },
  integrations: [
    smartquotes(),
    sitemap({
      filter: (page) => !/\/(account|checkout)$/.test(page),
    }),
  ],
  vite: {
    // NEXT_PUBLIC_* is the env contract shared with the other Heed deploys; PUBLIC_* is Astro's own.
    envPrefix: ["PUBLIC_", "NEXT_PUBLIC_"],
  },
});
