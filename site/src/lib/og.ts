/** Per-page social cards (1200x630), rendered at build time by src/pages/og/[slug].png.ts. */
export const OG_PAGES: Record<string, { label: string; line: string }> = {
  home: { label: "Heed for Mac", line: "Everything that needs you. Nothing that doesn't." },
  pricing: { label: "Pricing", line: "Free to run yourself. Easier in the cloud." },
  download: { label: "Download", line: "Heed for Mac. Free and open source." },
  privacy: { label: "Privacy policy", line: "Your data, in plain English." },
  terms: { label: "Terms of service", line: "The terms, in plain English." },
  account: { label: "Account", line: "Everything that needs you. Nothing that doesn't." },
  "self-host": { label: "Docs · Self-hosting", line: "Run Heed Cloud on your own machine." },
};

/** Which card a page uses, from its path. */
export function ogSlug(pathname: string): string {
  const p = pathname.replace(/(\/index)?\.html$/, "").replace(/\/$/, "") || "/";
  if (p === "/") return "home";
  const last = p.split("/").pop() ?? "";
  return last in OG_PAGES ? last : "home";
}
