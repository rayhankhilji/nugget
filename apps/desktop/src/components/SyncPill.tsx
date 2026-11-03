import { useEffect, useRef } from "react";
import { appName, cloudApi } from "../cloud";
import { useCloudSignedIn } from "../cloudSignedIn";
import { useSyncRequested } from "../cloudSync";
import { useAsync, useEvent } from "../hooks";
import { requestPage } from "../hooks.nav";
import { formatNumber, useT } from "../i18n";

/**
 * While Heed Cloud imports an app in the background, a small pill in the sidebar says so from any
 * page, with a 4px sweep. It reads the same connections list as Connections and re-reads it every
 * few seconds only while something is importing or a "Sync now" is in flight. Clicking it opens
 * Connections.
 */
export function SyncPill() {
  const t = useT();
  const signedIn = useCloudSignedIn();
  const list = useAsync(() => (signedIn ? cloudApi.connections().catch(() => null) : Promise.resolve(null)), [signedIn]);
  useEvent(cloudApi.onConnectionsChanged, list.reload);
  const all = list.data?.connections ?? [];
  const syncing = all.filter((c) => c.syncing);
  const reload = list.reload;
  const requested = useSyncRequested();
  const wasRequested = useRef(requested);
  useEffect(() => {
    // A sync just started or ended: read where it is now.
    if (wasRequested.current === requested) return;
    wasRequested.current = requested;
    if (signedIn) reload();
  }, [requested, signedIn, reload]);
  useEffect(() => {
    if (syncing.length === 0 && !requested) return;
    const timer = setInterval(reload, requested ? 1000 : 5000);
    return () => clearInterval(timer);
  }, [syncing.length, requested, reload]);
  if (syncing.length === 0) return null;
  const one = syncing.length === 1 ? syncing[0]! : null;
  const name = one ? (list.data?.available.find((a) => a.slug === one.toolkit)?.name ?? appName(one.toolkit)) : "";
  return (
    <button type="button" className="sync-pill" onClick={() => requestPage("connections")}>
      <span className="sync-pill-text">
        <span>{one ? t("sync.one", { name }) : t("sync.many", { count: syncing.length })}</span>
        <span className="sync-pill-count">
          {one ? t("sync.items", { count: formatNumber(one.items_synced ?? 0) }) : t("sync.done", { done: all.length - syncing.length, total: all.length })}
        </span>
      </span>
      <span className="sync-pill-bar" aria-hidden="true" />
    </button>
  );
}
