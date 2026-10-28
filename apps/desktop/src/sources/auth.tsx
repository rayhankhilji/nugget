/**
 * Waiting for an interactive sign-in started with `sources.auth.begin`: a browser redirect
 * (Google, Microsoft, Linear) or a device code to enter on the provider's page (GitHub).
 * Completion arrives as `sources.changed`; a slow poll of `sources.list` is the safety net.
 */
import { useEffect, useRef, useState } from "react";
import { api, errorMessage, events, sourcesFromChange, type AuthBegin, type SourceInfo } from "../api";
import { useEvent } from "../hooks";
import { Trans, useT } from "../i18n";
import { CopyButton, copyText } from "../components/CopyButton";
import { Dialog } from "../components/Dialog";
import { Spinner } from "../components/ui";
import { Done, FormError, HELP_TAGS, useAutofocus } from "./form";

export type AuthPhase = "form" | "waiting" | "done";

/** A sign-in is finished once the source left `needs_auth` (and isn't failing). */
export const isConnected = (s: SourceInfo) => s.state !== "needs_auth" && s.state !== "error";

/** The verification page without its query (the code is shown on its own). */
export function displayUrl(url: string): string {
  try {
    const u = new URL(url);
    return `${u.origin}${u.pathname}`;
  } catch {
    return url;
  }
}

export function useAuthWait(onConnected: (s: SourceInfo) => void) {
  const t = useT();
  const [phase, setPhase] = useState<AuthPhase>("form");
  const [source, setSource] = useState<SourceInfo | null>(null);
  const [flow, setFlow] = useState<AuthBegin | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const open = async (url: string) => {
    try {
      await api.openUrl(url);
      setNote(null);
    } catch (e) {
      setNote(t("addSource.google.browserFailed", { error: errorMessage(e) }));
    }
  };

  /** Start the sign-in for `s`. Redirect flows open the browser right away; device flows wait for the user. */
  const begin = async (s: SourceInfo) => {
    setSource(s);
    const f = await api.sourcesAuthBegin({ id: s.id });
    setFlow(f);
    setPhase("waiting");
    if (!f.user_code) await open(f.url);
  };

  const check = async (fromEvent?: unknown) => {
    if (!source) return;
    let latest = sourcesFromChange(fromEvent)?.find((s) => s.id === source.id);
    if (!latest) {
      try {
        latest = (await api.sourcesList()).find((s) => s.id === source.id);
      } catch {
        return;
      }
    }
    if (!latest) return;
    setSource(latest);
    if (isConnected(latest)) {
      setPhase("done");
      onConnected(latest);
    }
  };

  const checkRef = useRef(check);
  checkRef.current = check;
  useEvent(events.onSourcesChanged, (p) => {
    if (phase === "waiting") void checkRef.current(p);
  });
  useEffect(() => {
    if (phase !== "waiting") return;
    const timer = window.setInterval(() => void checkRef.current(), 5000);
    return () => window.clearInterval(timer);
  }, [phase]);

  return { phase, source, setSource, flow, note, begin, open };
}

/** The code to type on the provider's page, with a Copy button. */
export function DeviceCode({ code, url }: { code: string; url: string }) {
  const t = useT();
  return (
    <div className="device-code">
      <p>
        <Trans k="addSource.device.enter" args={{ url: displayUrl(url) }} tags={HELP_TAGS} />
      </p>
      <p className="device-code-value selectable">{code}</p>
      <CopyButton text={code} what={t("addSource.device.codeWhat")} />
    </div>
  );
}

/** The wizard's waiting step (replaces the form until the sign-in completes or is put off). */
export function ConnectWait({
  label,
  source,
  flow,
  note,
  onOpen,
  onClose,
}: {
  label: string;
  source: SourceInfo;
  flow: AuthBegin | null;
  note: string | null;
  onOpen: (url: string) => void;
  onClose: () => void;
}) {
  const t = useT();
  const primaryRef = useAutofocus<HTMLButtonElement>();
  const url = flow?.url ?? null;
  const code = flow?.user_code ?? null;

  const openPage = async () => {
    if (!url) return;
    // Device flow: put the code on the clipboard on the way, so it can be pasted on the page.
    if (code) await copyText(code);
    onOpen(url);
  };

  return (
    <div className="auth-wait">
      {code && url && <DeviceCode code={code} url={url} />}
      <p role="status">
        <Spinner label={t("addSource.wait.waiting")} />{" "}
        {code ? t("addSource.device.waiting", { label }) : t("addSource.wait.textAny", { label })}
      </p>
      {source.state === "error" && source.last_error && (
        <p className="note note-error" role="alert">
          {source.last_error}
        </p>
      )}
      {note && (
        <p className="note" role="alert">
          {note}
        </p>
      )}
      {url && note && (
        <p className="small">
          <code className="selectable">{url}</code>
        </p>
      )}
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onClose}>
          {t("addSource.wait.later")}
        </button>
        <button ref={primaryRef} type="button" className="btn btn-primary" onClick={() => void openPage()} disabled={!url}>
          {code ? t("addSource.device.copyAndOpen") : t("addSource.wait.reopen")}
        </button>
      </div>
    </div>
  );
}

/**
 * Reconnect an existing source whose sign-in shows a device code (GitHub with `client_id`):
 * starts `sources.auth.begin` and keeps the code on screen until the daemon reports success.
 */
export function DeviceSignInDialog({
  source,
  onClose,
  onConnected,
}: {
  source: SourceInfo;
  onClose: () => void;
  onConnected: (s: SourceInfo) => void;
}) {
  const t = useT();
  const auth = useAuthWait(onConnected);
  const [error, setError] = useState<unknown>(null);
  const started = useRef(false);
  const begin = auth.begin;
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    begin(source).catch(setError);
  }, [begin, source]);

  let body;
  if (auth.phase === "done") {
    body = <Done text={t("addSource.google.connected", { label: source.name })} onClose={onClose} />;
  } else if (auth.phase === "waiting" && auth.source) {
    body = <ConnectWait label={source.name} source={auth.source} flow={auth.flow} note={auth.note} onOpen={(url) => void auth.open(url)} onClose={onClose} />;
  } else if (error) {
    body = (
      <>
        <FormError error={error} />
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose} data-autofocus="">
            {t("common.close")}
          </button>
        </div>
      </>
    );
  } else {
    body = <Spinner label={t("sources.openingSignIn")} />;
  }
  return (
    <Dialog title={t("sources.deviceDialog.title", { name: source.name })} onClose={onClose}>
      {body}
    </Dialog>
  );
}
