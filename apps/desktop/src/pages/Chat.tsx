import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import {
  ApiError,
  api,
  askInConversation,
  CHAT_TIERS,
  chatHistory,
  events,
  errorMessage,
  isCommandMissing,
  isOpenableUrl,
  type AskStarted,
  type ChatDone,
  type ChatMessage,
  type Citation,
  type ProviderInfo,
  type StoredChatMessage,
  type StoredConversation,
  type Tier,
} from "../api";
import { useAsync, useEvent, useOpener } from "../hooks";
import { usePendingNewChat } from "../hooks.nav";
import { localityLabel, tierLabel } from "../format";
import { GoToPage } from "../components/GoToPage";
import { Badge, ErrorNote, PageHeader } from "../components/ui";
import { CopyButton } from "../components/CopyButton";
import { Markdown, type CitationLinks } from "../components/Markdown";
import { stripCitationMarkers, withheldNotice } from "../citations";
import { Trans, useT } from "../i18n";
import "../styles-chat.css";

interface Turn {
  id: number;
  role: "user" | "assistant";
  text: string;
  /** Assistant turns only. */
  streamId?: string;
  provider?: string;
  state?: "pending" | "streaming" | "done" | "error" | "cancelled";
  error?: string;
  tokens?: { input: number | null; output: number | null };
  /** Sources the reply's `[n]` markers refer to, when the reply came with any. */
  citations?: Citation[];
  /** What was searched, when the daemon rewrote a follow-up (not saved). */
  queryRewrite?: string;
  /** Sensitive items withheld from a cloud model (not saved). */
  excludedSensitive?: number;
  /** Sources were on but none matched, so this is a plain chat answer. */
  generalAnswer?: boolean;
}

const SOURCES_KEY = "nugget.chat.useSources";

/** Whether Chat answers from the user's sources (default on). */
export function readUseSources(): boolean {
  try {
    return localStorage.getItem(SOURCES_KEY) !== "false";
  } catch {
    return true;
  }
}

export function writeUseSources(on: boolean): void {
  try {
    localStorage.setItem(SOURCES_KEY, on ? "true" : "false");
  } catch {
    /* storage unavailable: the choice lasts for this session */
  }
}

/** Deltas/done events that arrive before `chat` returns its stream id. */
interface Early {
  text: string;
  done?: ChatDone;
}

let nextTurnId = 1;

/** Bounds matching `src-tauri/src/history.rs` (chars, so the byte limits always hold). */
const MAX_SAVED_TEXT = 60_000;
const MAX_SAVED_ERROR = 1_000;
const MAX_SAVED_MESSAGES = 200;
const MAX_TITLE = 80;

function newConversationId(): string {
  return `c-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

const isActive = (x: Turn) => x.state === "pending" || x.state === "streaming";

/** Settled turns as saved; a turn still streaming (e.g. when leaving the page) counts as stopped. */
function toStored(turns: readonly Turn[]): StoredChatMessage[] {
  return turns.slice(-MAX_SAVED_MESSAGES).map((x) => {
    const text = x.text.slice(0, MAX_SAVED_TEXT);
    if (x.role === "user") return { role: "user", text };
    const state = isActive(x) ? "cancelled" : (x.state as "done" | "error" | "cancelled");
    const m: StoredChatMessage = { role: "assistant", text, state };
    if (x.provider) m.provider = x.provider.slice(0, 200);
    if (x.error) m.error = x.error.slice(0, MAX_SAVED_ERROR);
    if (x.tokens?.output != null) m.output_tokens = x.tokens.output;
    if (x.citations?.length) m.citations = x.citations;
    if (x.generalAnswer) m.general_answer = true;
    return m;
  });
}

function fromStored(messages: readonly StoredChatMessage[]): Turn[] {
  return messages.map((m) =>
    m.role === "user"
      ? { id: nextTurnId++, role: "user", text: m.text }
      : {
          id: nextTurnId++,
          role: "assistant",
          text: m.text,
          provider: m.provider ?? undefined,
          state: m.state ?? "done",
          error: m.error ?? undefined,
          tokens: m.output_tokens != null ? { input: null, output: m.output_tokens } : undefined,
          citations: m.citations ?? undefined,
          generalAnswer: m.general_answer === true || undefined,
        },
  );
}

function titleOf(turns: readonly Turn[]): string {
  const first = turns.find((x) => x.role === "user")?.text ?? "";
  const line = first.replace(/\s+/g, " ").trim();
  return line.length > MAX_TITLE ? `${line.slice(0, MAX_TITLE - 1)}…` : line;
}

/** History commands missing (older shell) or not running in the app at all. */
function historyUnavailable(err: unknown): boolean {
  return isCommandMissing(err) || ApiError.from(err).kind === "unavailable";
}

export function ChatPage() {
  const t = useT();
  const providers = useAsync(api.providersList);
  const opener = useOpener();
  const [tier, setTier] = useState<Tier>("synthesis");
  const [provider, setProvider] = useState("");
  const [useSources, setUseSources] = useState(readUseSources);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [input, setInput] = useState("");
  const [announce, setAnnounce] = useState("");
  const early = useRef(new Map<string, Early>());
  const logRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // ---- history ----
  const [convId, setConvId] = useState(newConversationId);
  const [convs, setConvs] = useState<StoredConversation[]>([]);
  const [historyOn, setHistoryOn] = useState(true);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  /** What was last saved (or loaded) for the current conversation, to skip identical saves. */
  const lastSaved = useRef("");
  const historyOnRef = useRef(true);
  historyOnRef.current = historyOn;
  const tRef = useRef(t);
  tRef.current = t;

  const active = turns.find(isActive);

  const persist = useCallback(async (id: string, ts: readonly Turn[]) => {
    if (!historyOnRef.current) return;
    const messages = toStored(ts);
    if (messages.length === 0) return;
    const sig = `${id}\n${JSON.stringify(messages)}`;
    if (sig === lastSaved.current) return;
    lastSaved.current = sig;
    const conv: StoredConversation = { id, title: titleOf(ts), updated_at: Date.now(), messages };
    try {
      await chatHistory.save(conv);
      setConvs((cs) => [conv, ...cs.filter((c) => c.id !== id)]);
      setHistoryError(null);
    } catch (err) {
      if (historyUnavailable(err)) setHistoryOn(false);
      else setHistoryError(tRef.current("chat.history.saveFailed", { error: errorMessage(err) }));
    }
  }, []);

  /** "New chat" was asked for (e.g. from the command palette) before history loaded. */
  const freshChat = useRef(false);
  const latest = useRef({ turns, convId });
  latest.current = { turns, convId };

  // Resume the most recent conversation.
  useEffect(() => {
    let alive = true;
    chatHistory
      .load()
      .then((list) => {
        if (!alive) return;
        setConvs(list);
        const last = list[0];
        // Unless the user already started a (new) chat while this loaded.
        if (last && latest.current.turns.length === 0 && !freshChat.current) {
          const restored = fromStored(last.messages);
          lastSaved.current = `${last.id}\n${JSON.stringify(toStored(restored))}`;
          setConvId(last.id);
          setTurns(restored);
        }
      })
      .catch((err: unknown) => {
        if (!alive) return;
        if (historyUnavailable(err)) setHistoryOn(false);
        else setHistoryError(t("chat.history.loadFailed", { error: errorMessage(err) }));
      });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Save once a turn settles (done, stopped or failed), never per delta.
  useEffect(() => {
    if (!active && turns.length > 0) void persist(convId, turns);
  }, [turns, active, convId, persist]);

  // Leaving the page mid-reply: stop the stream and keep what arrived, marked as stopped.
  useEffect(
    () => () => {
      const { turns: ts, convId: id } = latest.current;
      const running = ts.find(isActive);
      if (!running) return;
      if (running.streamId) void api.chatCancel(running.streamId).catch(() => {});
      void persist(id, ts);
    },
    [persist],
  );

  const finish = (turn: Turn, done: ChatDone): Turn => ({
    ...turn,
    state: done.error ? (/cancel/i.test(done.error) ? "cancelled" : "error") : "done",
    error: done.error ?? undefined,
    tokens: { input: done.input_tokens, output: done.output_tokens },
  });

  // Stream ids already attached to a turn. Updated synchronously (unlike state), so an event
  // is either applied through `setTurns` (queued after the attach) or buffered in `early`.
  const attached = useRef(new Set<string>());
  // `chat` calls in flight. Unknown stream ids are buffered only while one is pending;
  // otherwise they belong to a stopped stream and are dropped.
  const awaiting = useRef(0);
  // Streams dropped before they were attached (a sourced answer with nothing to cite).
  const dropped = useRef(new Set<string>());

  useEvent(events.onChatDelta, (d) => {
    if (dropped.current.has(d.stream_id)) return;
    if (attached.current.has(d.stream_id)) {
      setTurns((ts) =>
        ts.map((x) => (x.streamId === d.stream_id && isActive(x) ? { ...x, text: x.text + d.text, state: "streaming" } : x)),
      );
    } else if (awaiting.current > 0) {
      const e = early.current.get(d.stream_id) ?? { text: "" };
      e.text += d.text;
      early.current.set(d.stream_id, e);
    }
  });

  useEvent(events.onChatDone, (d) => {
    if (dropped.current.delete(d.stream_id)) return;
    if (attached.current.has(d.stream_id)) {
      attached.current.delete(d.stream_id);
      setTurns((ts) => ts.map((x) => (x.streamId === d.stream_id && x.state !== "cancelled" ? finish(x, d) : x)));
    } else if (awaiting.current > 0) {
      const e = early.current.get(d.stream_id) ?? { text: "" };
      e.done = d;
      early.current.set(d.stream_id, e);
    }
  });

  // Keep the newest output in view while streaming.
  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [turns]);

  const send = async (e?: FormEvent) => {
    e?.preventDefault();
    const text = input.trim();
    if (!text || active) return;

    const history: ChatMessage[] = turns
      .filter((x) => x.role === "user" || x.state === "done" || (x.state === "cancelled" && x.text))
      // An earlier reply's `[n]` markers pointed at that reply's passages: drop them.
      .map((x) => ({ role: x.role, text: x.role === "assistant" ? stripCitationMarkers(x.text) : x.text }));

    const userTurn: Turn = { id: nextTurnId++, role: "user", text };
    const reply: Turn = { id: nextTurnId++, role: "assistant", text: "", state: "pending" };
    setTurns((ts) => [...ts, userTurn, reply]);
    setInput("");
    setAnnounce("");

    awaiting.current++;
    try {
      const route = { provider: provider || null, tier };
      // With sources, the daemon retrieves for the question (rewritten from the conversation
      // when it is a follow-up) and the reply cites what it was given.
      let asked: AskStarted | null = useSources ? await askInConversation(text, history, route) : null;
      // Nothing in the sources matched: the sourced prompt would only decline, so stop that
      // stream and answer as a plain chat instead, saying so on the turn.
      let general: AskStarted | null = null;
      if (asked && asked.citations.length === 0) {
        general = asked;
        asked = null;
        dropped.current.add(general.stream_id);
        early.current.delete(general.stream_id);
        void api.chatCancel(general.stream_id).catch(() => {});
      }
      const started = asked ?? (await api.chat({ ...route, messages: [...history, { role: "user", text }] }));
      const buffered = early.current.get(started.stream_id);
      early.current.delete(started.stream_id);
      if (!buffered?.done) attached.current.add(started.stream_id);
      setTurns((ts) =>
        ts.map((x) => {
          if (x.id !== reply.id) return x;
          let next: Turn = {
            ...x,
            streamId: started.stream_id,
            provider: started.provider,
            text: buffered?.text ?? "",
            state: buffered?.text ? "streaming" : "pending",
          };
          if (asked) {
            next.citations = asked.citations;
            next.queryRewrite = asked.query_rewrite ?? undefined;
            next.excludedSensitive = asked.excluded_sensitive ?? 0;
          } else if (general) {
            next.generalAnswer = true;
            next.excludedSensitive = general.excluded_sensitive ?? 0;
          }
          if (buffered?.done) next = finish(next, buffered.done);
          return next;
        }),
      );
    } catch (err) {
      setTurns((ts) => ts.map((x) => (x.id === reply.id ? { ...x, state: "error", error: errorMessage(err) } : x)));
    } finally {
      awaiting.current--;
      if (awaiting.current === 0) early.current.clear();
      inputRef.current?.focus();
    }
  };

  const stop = async () => {
    if (!active?.streamId) return;
    const streamId = active.streamId;
    // Detach first so deltas still in flight don't land after "Stopped".
    attached.current.delete(streamId);
    setTurns((ts) => ts.map((x) => (x.id === active.id && x.state !== "done" ? { ...x, state: "cancelled" } : x)));
    setAnnounce(t("chat.stoppedAnnounce"));
    try {
      await api.chatCancel(streamId);
    } catch {
      /* the stream is detached either way */
    }
    inputRef.current?.focus();
  };

  const startNew = () => {
    if (active) return;
    freshChat.current = true;
    setConvId(newConversationId());
    setTurns([]);
    setConfirmClear(false);
    lastSaved.current = "";
    inputRef.current?.focus();
  };
  usePendingNewChat(startNew);

  const open = (c: StoredConversation) => {
    if (active || c.id === convId) return;
    const restored = fromStored(c.messages);
    lastSaved.current = `${c.id}\n${JSON.stringify(toStored(restored))}`;
    setConvId(c.id);
    setTurns(restored);
    inputRef.current?.focus();
  };

  const remove = async (c: StoredConversation) => {
    try {
      await chatHistory.remove(c.id);
      setConvs((cs) => cs.filter((x) => x.id !== c.id));
      if (c.id === convId) startNew();
    } catch (err) {
      if (historyUnavailable(err)) setHistoryOn(false);
      else setHistoryError(errorMessage(err));
    }
  };

  const clearAll = async () => {
    setConfirmClear(false);
    try {
      await chatHistory.clear();
      setConvs([]);
      setHistoryError(null);
      startNew();
      setAnnounce(t("chat.history.cleared"));
    } catch (err) {
      if (historyUnavailable(err)) setHistoryOn(false);
      else setHistoryError(errorMessage(err));
    }
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send();
    }
  };

  // Escape stops a reply from anywhere on the page.
  const onPageKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape" && active) {
      e.preventDefault();
      void stop();
    }
  };

  const list: ProviderInfo[] = providers.data ?? [];
  const noProviders = !providers.loading && !providers.error && providers.data !== undefined && list.length === 0;

  return (
    <div className="page page-chat" onKeyDown={onPageKeyDown}>
      <PageHeader
        title={t("chat.title")}
        actions={
          <>
            <label className="field-inline">
              <span>{t("chat.tier")}</span>
              <select value={tier} onChange={(e) => setTier(e.target.value as Tier)}>
                {CHAT_TIERS.map((tr) => (
                  <option key={tr} value={tr}>
                    {tierLabel(tr)}
                  </option>
                ))}
              </select>
            </label>
            <label className="field-inline">
              <span>{t("chat.provider")}</span>
              <select value={provider} onChange={(e) => setProvider(e.target.value)} aria-busy={providers.loading || undefined}>
                <option value="">{t("chat.providerAuto")}</option>
                {list.map((p) => (
                  <option key={p.id} value={p.id} disabled={!!p.error}>
                    {t(p.error ? "chat.providerOptionUnavailable" : "chat.providerOption", {
                      id: p.id,
                      model: p.model,
                      locality: localityLabel(p.locality),
                    })}
                  </option>
                ))}
              </select>
            </label>
            <label className="field-inline" title={t("chat.useSourcesHint")}>
              <input
                type="checkbox"
                checked={useSources}
                onChange={(e) => {
                  setUseSources(e.target.checked);
                  writeUseSources(e.target.checked);
                }}
              />
              <span>{t("chat.useSources")}</span>
            </label>
            <button type="button" className="btn btn-ghost" onClick={startNew} disabled={!!active || turns.length === 0}>
              {t("chat.newChat")}
            </button>
          </>
        }
      />
      {providers.error ? <ErrorNote error={providers.error} onRetry={providers.reload} /> : null}
      {noProviders && (
        <p className="note note-warn">
          {t("chat.noProviders")} <GoToPage page="models" className="btn btn-ghost btn-small" />
        </p>
      )}

      <div className="chat-main">
        {historyOn && (
          <nav className="chat-history" aria-labelledby="chat-history-title">
            <h2 className="chat-history-title" id="chat-history-title">
              {t("chat.history.title")}
            </h2>
            {convs.length === 0 ? (
              <p className="muted small">{t("chat.history.empty")}</p>
            ) : (
              <ul className="chat-history-list">
                {convs.map((c) => {
                  const title = c.title || t("chat.history.untitled");
                  return (
                    <li key={c.id} className="chat-history-item">
                      <button
                        type="button"
                        className="chat-history-open"
                        aria-current={c.id === convId ? "true" : undefined}
                        disabled={!!active && c.id !== convId}
                        onClick={() => open(c)}
                        title={title}
                      >
                        {title}
                      </button>
                      <button
                        type="button"
                        className="chat-history-delete"
                        aria-label={t("chat.history.delete", { title })}
                        disabled={!!active && c.id === convId}
                        onClick={() => void remove(c)}
                      >
                        <span aria-hidden="true">×</span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
            <div className="chat-history-foot">
              {confirmClear ? (
                <div className="chat-history-confirm" role="group" aria-label={t("chat.history.clearConfirm")}>
                  <span className="small">{t("chat.history.clearConfirm")}</span>
                  <button type="button" className="btn btn-small btn-danger" onClick={() => void clearAll()}>
                    {t("chat.history.clearYes")}
                  </button>
                  <button type="button" className="btn btn-small btn-ghost" onClick={() => setConfirmClear(false)} autoFocus>
                    {t("chat.history.cancel")}
                  </button>
                </div>
              ) : (
                <button
                  type="button"
                  className="btn btn-small btn-ghost"
                  onClick={() => setConfirmClear(true)}
                  disabled={!!active || convs.length === 0}
                >
                  {t("chat.history.clear")}
                </button>
              )}
              <p className="muted small">{t("chat.history.note")}</p>
              {historyError && (
                <p className="note note-error small" role="alert">
                  {historyError}
                </p>
              )}
            </div>
          </nav>
        )}

        <div className="chat-pane">
          {/* The log region wraps the list: role="log" on the <ol> itself would hide its list semantics. */}
          <div className="chat-log" ref={logRef} role="log" aria-label={t("chat.conversation")} aria-busy={!!active}>
            <ol className="chat-list">
              {turns.length === 0 && (
                <li className="chat-empty muted">
                  {providers.loading ? (
                    t("chat.loadingProviders")
                  ) : (
                    <Trans
                      k={provider ? "chat.emptyPinned" : "chat.empty"}
                      args={{ tier: tierLabel(tier), provider }}
                      tags={{ strong: (c) => <strong>{c}</strong> }}
                    />
                  )}
                </li>
              )}
              {turns.map((turn) => (
                <MessageItem key={turn.id} turn={turn} onOpenLink={(url, label) => void opener.open(url, label)} />
              ))}
            </ol>
          </div>

          {opener.status && (
            <p className={`chat-status small ${opener.status.error ? "note note-error" : "muted"}`} role="status">
              {opener.status.text}
            </p>
          )}
          <p className="visually-hidden" role="status">
            {announce}
          </p>

          <form className="composer" onSubmit={send}>
            <label htmlFor="chat-input" className="visually-hidden">
              {t("chat.message")}
            </label>
            <textarea
              id="chat-input"
              ref={inputRef}
              rows={3}
              value={input}
              placeholder={t("chat.placeholder")}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={onKeyDown}
              autoFocus
            />
            {active ? (
              <button type="button" className="btn" onClick={stop} disabled={!active.streamId}>
                {t("chat.stop")}
              </button>
            ) : (
              <button type="submit" className="btn btn-primary" disabled={!input.trim()}>
                {t("chat.send")}
              </button>
            )}
          </form>
        </div>
      </div>
    </div>
  );
}

/** Element id of source `n` in the list under reply `turnId`. */
const citeId = (turnId: number, n: number) => `msg-${turnId}-cite-${n}`;

function MessageItem({ turn, onOpenLink }: { turn: Turn; onOpenLink: (url: string, label: string) => void }) {
  const t = useT();
  const opener = onOpenLink;
  const sourcesRef = useRef<HTMLOListElement>(null);
  // A source with a URL the shell can open opens; any other moves focus to it in the list below.
  const cite = useMemo<CitationLinks | undefined>(() => {
    const citations = turn.citations ?? [];
    if (citations.length === 0) return undefined;
    const byN = new Map(citations.map((c) => [c.n, c]));
    const titleOf = (n: number) => byN.get(n)?.title || byN.get(n)?.item.key || "";
    return {
      valid: new Set(byN.keys()),
      label: (n) => t("chat.citationLabel", { n, title: titleOf(n) }),
      onCite: (n) => {
        const c = byN.get(n);
        if (c && isOpenableUrl(c.url)) opener(c.url, titleOf(n));
        else sourcesRef.current?.querySelector<HTMLElement>(`#${citeId(turn.id, n)}`)?.focus();
      },
    };
  }, [turn.citations, turn.id, opener, t]);
  const withheld = turn.provider ? withheldNotice(turn.excludedSensitive, turn.provider) : null;

  const streaming = isActive(turn);
  return (
    <li className={`msg msg-${turn.role}`}>
      <div className="msg-meta">
        {turn.role === "user" ? t("chat.you") : (turn.provider ?? t("app.name"))}
        {turn.state === "cancelled" && <Badge tone="warn">{t("chat.stopped")}</Badge>}
        {turn.state === "done" && turn.tokens?.output != null && (
          <span className="muted small">{t("chat.tokens", { n: turn.tokens.output })}</span>
        )}
        {turn.role === "assistant" && !streaming && turn.text && (
          <span className="msg-actions">
            <CopyButton text={turn.text} what={t("chat.replyWhat")} className="btn btn-small btn-ghost" />
          </span>
        )}
      </div>
      {turn.role === "user" ? (
        <div className="msg-body">{turn.text}</div>
      ) : (
        <div className="msg-body msg-md">
          {turn.text && <Markdown text={turn.text} onOpenLink={onOpenLink} citations={cite} />}
          {streaming && <span className="caret" aria-hidden="true" />}
          {turn.state === "pending" && !turn.text && <span className="visually-hidden">{t("chat.waiting")}</span>}
        </div>
      )}
      {turn.queryRewrite && <p className="muted small chat-rewrite">{t("chat.searchedFor", { query: turn.queryRewrite })}</p>}
      {turn.generalAnswer && <p className="muted small chat-general">{t("chat.generalAnswer")}</p>}
      {withheld && <p className="note small">{withheld}</p>}
      {turn.role === "assistant" && !!turn.citations?.length && (
        <div className="chat-sources">
          <h3 className="chat-sources-title small muted">{t("chat.sources")}</h3>
          <ol className="citations chat-citations" ref={sourcesRef}>
            {turn.citations.map((c) => {
              const title = c.title || c.item.key;
              return (
                <li key={c.n} id={citeId(turn.id, c.n)} tabIndex={-1} className="citation">
                  <span className="cite-n">[{c.n}]</span>
                  <span className="citation-body">
                    <span className="citation-title">{title}</span>
                  </span>
                  {isOpenableUrl(c.url) && (
                    <button
                      type="button"
                      className="btn btn-small"
                      onClick={() => opener(c.url!, title)}
                      aria-label={t("search.openTitle", { title })}
                    >
                      {t("search.open")}
                    </button>
                  )}
                </li>
              );
            })}
          </ol>
        </div>
      )}
      {turn.state === "error" && (
        <p className="note note-error" role="alert">
          {turn.error}
        </p>
      )}
    </li>
  );
}
