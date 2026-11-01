import { StrictMode, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { initI18n } from "./i18n";

/** Render a window's root into `#root` once the user's language is loaded (so it doesn't flash English; a failed load still renders). */
export function mount(app: ReactNode): void {
  const root = document.getElementById("root");
  if (!root) throw new Error("missing #root");
  void initI18n()
    .catch(() => {})
    .finally(() => createRoot(root).render(<StrictMode>{app}</StrictMode>));
}
