import { useState } from "react";
import type { CloudApp } from "../cloud";

/**
 * A white tile with a connected app's logo, or its initial. Only a bundled (`/…`) or inline
 * (`data:`) image can load under the app's CSP; anything else, or a broken image, shows the initial.
 */
export function AppLogo({ app, className = "app-logo" }: { app: Pick<CloudApp, "name" | "logo">; className?: string }) {
  const [broken, setBroken] = useState(false);
  const local = app.logo && (app.logo.startsWith("data:") || app.logo.startsWith("/"));
  return (
    <span className={className} aria-hidden="true">
      {local && !broken ? <img src={app.logo!} alt="" onError={() => setBroken(true)} /> : <span>{app.name.slice(0, 1)}</span>}
    </span>
  );
}
