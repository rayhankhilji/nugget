import { useEffect, useState } from "react";
import { appName, cloudApi, type CloudApp } from "../cloud";

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

let logos: Promise<Map<string, string | null>> | null = null;

/** The cloud's app logos by slug, fetched once per window (empty when signed out or offline). */
function loadLogos(): Promise<Map<string, string | null>> {
  logos ??= cloudApi
    .apps()
    .then((apps) => new Map(apps.map((a) => [a.slug, a.logo])))
    .catch(() => new Map());
  const pending = logos;
  // Nothing yet (signed out, offline): ask again next time rather than keep the empty answer.
  void pending.then((m) => m.size === 0 && logos === pending && (logos = null));
  return pending;
}

/**
 * A small round app glyph for chips and meta lines: the same logo as Connections, or the app's
 * initial when there is none. Decorative; the app's name is always in the text beside it.
 */
export function AppGlyph({ source, className = "app-glyph" }: { source: string; className?: string }) {
  const [logo, setLogo] = useState<string | null>(null);
  const [broken, setBroken] = useState(false);
  useEffect(() => {
    let alive = true;
    void loadLogos().then((m) => alive && setLogo(m.get(source) ?? null));
    return () => {
      alive = false;
    };
  }, [source]);
  const local = logo && (logo.startsWith("data:") || logo.startsWith("/"));
  return (
    <span className={className} aria-hidden="true">
      {local && !broken ? <img src={logo} alt="" onError={() => setBroken(true)} /> : appName(source).slice(0, 1)}
    </span>
  );
}
