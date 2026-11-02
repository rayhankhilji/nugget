// @ts-check
import { defineConfig } from "astro/config";
import sitemap from "@astrojs/sitemap";

// The public URL the site is served from, used for canonical links and the sitemap.
const site = process.env.SITE_URL || "https://heed.pages.dev";

export default defineConfig({
  site,
  output: "static",
  // The smoke test builds a second copy with test settings into its own folder.
  outDir: process.env.ASTRO_OUT_DIR || "./dist",
  trailingSlash: "never",
  build: { format: "file", inlineStylesheets: "always" },
  prefetch: { prefetchAll: false, defaultStrategy: "hover" },
  integrations: [
    sitemap({
      filter: (page) => !/\/(account|checkout)$/.test(page),
    }),
  ],
  vite: {
    // NEXT_PUBLIC_* is the env contract shared with the other Heed deploys; PUBLIC_* is Astro's own.
    envPrefix: ["PUBLIC_", "NEXT_PUBLIC_"],
  },
});
