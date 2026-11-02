import { useCallback, useEffect, useRef, useState, type Dispatch } from "react";
import { cloudApi, type CloudApp, type CloudConnections, type CloudSession, type CloudSyncResult } from "../cloud";
import { useAsync, useEvent } from "../hooks";
import { ErrorNote, Loading } from "../components/ui";
import { AppLogo } from "../components/AppLogo";
import { useT } from "../i18n";
import type { Action } from "./machine";

/** How often the connect grid looks again while an app is authorising in the browser. */
export const CONNECT_POLL_MS = 3000;
/** After this long an unfinished consent page no longer counts as authorising (it was abandoned). */
export const AUTHORISE_TIMEOUT_MS = 120_000;

export interface CloudState {
  /** `null` while the first check is in flight. A build or shell without cloud reads as not configured. */
  session: CloudSession | null;
  /** The last sign-in attempt that came back failed (the shell's error). */
  failure: string | null;
  reload: () => void;
}

const UNAVAILABLE: CloudSession = {
  configured: false,
  signed_in: false,
  email: null,
};

/**
 * The Heed account, for the sign-in and connect steps. Re-checked when the shell says the session
 * changed; `onSignedIn` runs when a sign-in finishes while onboarding is open.
 */
export function useCloudSession(onSignedIn: () => void): CloudState {
  const s = useAsync(cloudApi.session);
  const [failure, setFailure] = useState<string | null>(null);
  const signedIn = useRef(onSignedIn);
  signedIn.current = onSignedIn;
  useEvent(cloudApi.onSession, (e) => {
    setFailure(e.signed_in ? null : e.error);
    s.reload();
    if (e.signed_in) signedIn.current();
  });
  const session = s.data ?? (s.error ? UNAVAILABLE : null);
  return { session, failure, reload: s.reload };
}

type TileState = "connected" | "authorising" | "idle" | "failed" | "soon";

/**
 * One-click cloud app connections, as a grid of tiles: Connected, Authorising (the consent page is
 * open in the browser) or Connect. Connecting an app records it as set up, and the first sync
 * starts straight away so the first answer has something to read.
 */
export function ConnectApps({ dispatch, announce }: { dispatch: Dispatch<Action>; announce: (text: string) => void }) {
  const t = useT();
  const list = useAsync(cloudApi.connections);
  useEvent(cloudApi.onConnectionsChanged, list.reload);
  const [clicked, setClicked] = useState<string[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [synced, setSynced] = useState<CloudSyncResult[] | null>(null);

  const data: CloudConnections | undefined = list.data;
  const status = new Map(data?.connections.map((c) => [c.toolkit, c]) ?? []);
  const stateOf = (a: CloudApp): TileState => {
    const c = status.get(a.slug);
    if (c?.status === "ACTIVE") return "connected";
    if (c?.status === "INITIATED" || clicked.includes(a.slug)) return "authorising";
    if (c && (c.status === "FAILED" || c.status === "EXPIRED")) return "failed";
    return a.connectable ? "idle" : "soon";
  };
  const apps = data?.available ?? [];
  const connectedNames = apps.filter((a) => stateOf(a) === "connected").map((a) => a.name);
  const connectedKey = connectedNames.join("\u0000");
  const authorising = apps.some((a) => stateOf(a) === "authorising");

  useEffect(() => {
    if (connectedKey) dispatch({ type: "appsConnected", names: connectedKey.split("\u0000") });
  }, [connectedKey, dispatch]);

  // Announce an app that finished connecting on this visit, then sync so the first answer has data.
  const known = useRef<Set<string> | null>(null);
  const runSync = useCallback(async () => {
    setSyncing(true);
    try {
      setSynced((await cloudApi.sync()).synced);
    } catch (e) {
      setError(e);
    } finally {
      setSyncing(false);
    }
  }, []);
  useEffect(() => {
    if (!data) return;
    const now = new Set(connectedNames);
    if (known.current === null) {
      known.current = now;
      return;
    }
    const fresh = connectedNames.filter((n) => !known.current!.has(n));
    known.current = now;
    if (fresh.length === 0) return;
    for (const name of fresh) announce(t("onboarding.connect.connectedAnnounce", { name }));
    setNotice(null);
    void runSync();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data, connectedKey]);

  // The browser sends no word back on every platform: look again while something is authorising.
  useEffect(() => {
    if (!authorising) return;
    const id = window.setInterval(list.reload, CONNECT_POLL_MS);
    return () => window.clearInterval(id);
  }, [authorising, list.reload]);

  const connect = async (app: CloudApp) => {
    setBusy(app.slug);
    setError(null);
    setNotice(null);
    try {
      await cloudApi.connect(app.slug);
      setClicked((c) => (c.includes(app.slug) ? c : [...c, app.slug]));
      window.setTimeout(() => setClicked((c) => c.filter((x) => x !== app.slug)), AUTHORISE_TIMEOUT_MS);
      setNotice(t("cloud.connectInBrowser", { name: app.name }));
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  };

  const nameOf = (slug: string) => apps.find((a) => a.slug === slug)?.name ?? slug;

  return (
    <section className="ob-connect" aria-label={t("cloud.available.title")}>
      <ErrorNote error={list.error} onRetry={list.reload} />
      <ErrorNote error={error} />
      {list.loading && !data && <Loading lines={2} />}
      {data && (
        <ul className="ob-apps">
          {apps.map((a) => {
            const state = stateOf(a);
            const conn = status.get(a.slug);
            const body = (
              <>
                <AppLogo className="ob-app-logo" app={a} />
                <span className="ob-app-name">{a.name}</span>
                <span className="ob-app-state">
                  {state === "connected" ? (
                    t("cloud.connected")
                  ) : state === "authorising" ? (
                    <>
                      <span className="spinner" aria-hidden="true" /> {t("onboarding.connect.authorising")}
                    </>
                  ) : state === "soon" ? (
                    t("cloud.comingSoon")
                  ) : busy === a.slug ? (
                    <span className="spinner" aria-hidden="true" />
                  ) : (
                    t("cloud.connect")
                  )}
                </span>
                {state === "connected" && <Check />}
              </>
            );
            return (
              <li key={a.slug} className={`ob-app ob-app-${state}`} data-state={state}>
                {state === "idle" || state === "failed" || state === "authorising" ? (
                  <button
                    type="button"
                    className="ob-app-tile"
                    disabled={busy !== null}
                    aria-busy={state === "authorising" || undefined}
                    aria-label={t("cloud.connectLabel", { name: a.name })}
                    aria-describedby={state === "failed" ? `ob-app-err-${a.slug}` : undefined}
                    onClick={() => void connect(a)}
                  >
                    {body}
                  </button>
                ) : (
                  <div className="ob-app-tile">{body}</div>
                )}
                {state === "failed" && (
                  <span className="ob-app-error small" id={`ob-app-err-${a.slug}`} dir="auto">
                    {conn?.last_error ?? conn?.status}
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
      <div className="ob-connect-status" role="status">
        {notice && <p className="muted small">{notice}</p>}
        {syncing && (
          <p className="ob-sync muted small">
            <span className="ob-sync-bar" aria-hidden="true" />
            {t("cloud.syncing")}
          </p>
        )}
        {synced && !syncing && (
          <ul className="ob-sync-results muted small" aria-label={t("cloud.syncResults")}>
            {synced.map((r) => (
              <li key={r.toolkit}>
                {r.error
                  ? t("cloud.syncFailed", {
                      name: nameOf(r.toolkit),
                      error: r.error,
                    })
                  : t("cloud.syncResult", {
                      name: nameOf(r.toolkit),
                      added: r.added,
                      updated: r.updated,
                    })}
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

function Check() {
  return (
    <svg
      className="ob-app-check"
      viewBox="0 0 16 16"
      aria-hidden="true"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M4 8.5 6.8 11 12 5.5" />
    </svg>
  );
}
