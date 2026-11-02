/**
 * The apps Heed can connect, generated from the cloud's catalogue
 * (crates/nugget-cloud/src/apps/mod.rs) by scripts/sync-apps.mjs. Don't edit apps.json by hand.
 */
import apps from "../data/apps.json";

export interface App {
  slug: string;
  name: string;
}
export const APPS: App[] = apps;

/** How the site labels an app. Docs come in through Drive. */
export const label = (a: App) => (a.slug === "googledrive" ? "Google Drive (incl. Docs)" : a.name);

/** "Gmail, Google Calendar, … and Notion" */
export const appList = () => {
  const names = APPS.map(label);
  return names.length > 1 ? `${names.slice(0, -1).join(", ")} and ${names.at(-1)}` : (names[0] ?? "");
};
