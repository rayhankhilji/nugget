import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { api, CHAT_TIERS, events, errorMessage, type ChatDone, type ChatMessage, type ProviderInfo, type Tier } from "../api";
import { useAsync, useEvent } from "../hooks";
import { localityLabel, tierLabel } from "../format";
import { Badge, ErrorNote, PageHeader } from "../components/ui";
import { Trans, useT } from "../i18n";

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
}

/** Deltas/done events that arrive before `chat` returns its stream id. */
interface Early {
  text: string;
  done?: ChatDone;
}

let nextTurnId = 1;

export function ChatPage() {
  const t = useT();
  const providers = useAsync(api.providersList);
  const [tier, setTier] = useState<Tier>("synthesis");
  const [provider, setProvider] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [input, setInput] = useState("");
  const early = useRef(new Map<string, Early>());
  const logRef = useRef<HTMLOListElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  const active = turns.find((x) => x.state === "pending" || x.state === "streaming");

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

  useEvent(events.onChatDelta, (d) => {
    if (attached.current.has(d.stream_id)) {
      setTurns((ts) =>
        ts.map((x) =>
          x.streamId === d.stream_id && (x.state === "pending" || x.state === "streaming")
            ? { ...x, text: x.text + d.text, state: "streaming" }
            : x,
        ),
      );
    } else if (awaiting.current > 0) {
      const e = early.current.get(d.stream_id) ?? { text: "" };
      e.text += d.text;
      early.current.set(d.stream_id, e);
    }
  });

  useEvent(events.onChatDone, (d) => {
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
      .map((x) => ({ role: x.role, text: x.text }));
    history.push({ role: "user", text });

    const userTurn: Turn = { id: nextTurnId++, role: "user", text };
    const reply: Turn = { id: nextTurnId++, role: "assistant", text: "", state: "pending" };
    setTurns((ts) => [...ts, userTurn, reply]);
    setInput("");

    awaiting.current++;
    try {
      const started = await api.chat({ provider: provider || null, tier, messages: history });
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
    try {
      await api.chatCancel(active.streamId);
    } catch {
      /* the done event (or its absence) settles the turn */
    }
    attached.current.delete(active.streamId);
    setTurns((ts) => ts.map((x) => (x.id === active.id && x.state !== "done" ? { ...x, state: "cancelled" } : x)));
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send();
    } else if (e.key === "Escape" && active) {
      e.preventDefault();
      void stop();
    }
  };

  const list: ProviderInfo[] = providers.data ?? [];

  return (
    <div className="page page-chat">
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
              <select value={provider} onChange={(e) => setProvider(e.target.value)}>
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
            <button type="button" className="btn btn-ghost" onClick={() => setTurns([])} disabled={!!active || turns.length === 0}>
              {t("chat.clear")}
            </button>
          </>
        }
      />
      {providers.error ? <ErrorNote error={providers.error} onRetry={providers.reload} /> : null}

      <ol className="chat-log" ref={logRef} role="log" aria-label={t("chat.conversation")} aria-busy={!!active}>
        {turns.length === 0 && (
          <li className="chat-empty muted">
            <Trans
              k={provider ? "chat.emptyPinned" : "chat.empty"}
              args={{ tier: tierLabel(tier), provider }}
              tags={{ strong: (c) => <strong>{c}</strong> }}
            />
          </li>
        )}
        {turns.map((turn) => (
          <li key={turn.id} className={`msg msg-${turn.role}`}>
            <div className="msg-meta">
              {turn.role === "user" ? t("chat.you") : (turn.provider ?? t("app.name"))}
              {turn.state === "cancelled" && <Badge tone="warn">{t("chat.stopped")}</Badge>}
              {turn.state === "done" && turn.tokens?.output != null && (
                <span className="muted small">{t("chat.tokens", { n: turn.tokens.output })}</span>
              )}
            </div>
            <div className="msg-body">
              {turn.text}
              {(turn.state === "pending" || turn.state === "streaming") && <span className="caret" aria-hidden="true" />}
              {turn.state === "pending" && !turn.text && <span className="visually-hidden">{t("chat.waiting")}</span>}
            </div>
            {turn.state === "error" && (
              <p className="note note-error" role="alert">
                {turn.error}
              </p>
            )}
          </li>
        ))}
      </ol>

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
  );
}
