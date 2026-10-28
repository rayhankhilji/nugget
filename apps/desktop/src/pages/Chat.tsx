import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { api, CHAT_TIERS, events, errorMessage, type ChatDone, type ChatMessage, type ProviderInfo, type Tier } from "../api";
import { useAsync, useEvent } from "../hooks";
import { LOCALITY_LABEL, TIER_LABEL } from "../format";
import { Badge, ErrorNote, PageHeader } from "../components/ui";

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
  const providers = useAsync(api.providersList);
  const [tier, setTier] = useState<Tier>("synthesis");
  const [provider, setProvider] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [input, setInput] = useState("");
  const early = useRef(new Map<string, Early>());
  const logRef = useRef<HTMLOListElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  const active = turns.find((t) => t.state === "pending" || t.state === "streaming");

  const finish = (t: Turn, done: ChatDone): Turn => ({
    ...t,
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
        ts.map((t) =>
          t.streamId === d.stream_id && (t.state === "pending" || t.state === "streaming")
            ? { ...t, text: t.text + d.text, state: "streaming" }
            : t,
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
      setTurns((ts) => ts.map((t) => (t.streamId === d.stream_id && t.state !== "cancelled" ? finish(t, d) : t)));
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
      .filter((t) => t.role === "user" || t.state === "done" || (t.state === "cancelled" && t.text))
      .map((t) => ({ role: t.role, text: t.text }));
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
        ts.map((t) => {
          if (t.id !== reply.id) return t;
          let next: Turn = {
            ...t,
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
      setTurns((ts) => ts.map((t) => (t.id === reply.id ? { ...t, state: "error", error: errorMessage(err) } : t)));
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
    setTurns((ts) => ts.map((t) => (t.id === active.id && t.state !== "done" ? { ...t, state: "cancelled" } : t)));
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
        title="Chat"
        actions={
          <>
            <label className="field-inline">
              <span>Tier</span>
              <select value={tier} onChange={(e) => setTier(e.target.value as Tier)}>
                {CHAT_TIERS.map((t) => (
                  <option key={t} value={t}>
                    {TIER_LABEL[t]}
                  </option>
                ))}
              </select>
            </label>
            <label className="field-inline">
              <span>Provider</span>
              <select value={provider} onChange={(e) => setProvider(e.target.value)}>
                <option value="">Auto (route by tier)</option>
                {list.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.id} · {p.model} ({LOCALITY_LABEL[p.locality] ?? p.locality})
                  </option>
                ))}
              </select>
            </label>
            <button type="button" className="btn btn-ghost" onClick={() => setTurns([])} disabled={!!active || turns.length === 0}>
              Clear
            </button>
          </>
        }
      />
      {providers.error ? <ErrorNote error={providers.error} onRetry={providers.reload} /> : null}

      <ol className="chat-log" ref={logRef} role="log" aria-label="Conversation" aria-busy={!!active}>
        {turns.length === 0 && (
          <li className="chat-empty muted">
            Ask anything. Replies stream from the provider routed for the <strong>{TIER_LABEL[tier]}</strong> tier
            {provider ? ` (pinned to ${provider})` : ""}.
          </li>
        )}
        {turns.map((t) => (
          <li key={t.id} className={`msg msg-${t.role}`}>
            <div className="msg-meta">
              {t.role === "user" ? "You" : (t.provider ?? "Nugget")}
              {t.state === "cancelled" && <Badge tone="warn">Stopped</Badge>}
              {t.state === "done" && t.tokens?.output != null && (
                <span className="muted small">{t.tokens.output} tokens</span>
              )}
            </div>
            <div className="msg-body">
              {t.text}
              {(t.state === "pending" || t.state === "streaming") && <span className="caret" aria-hidden="true" />}
              {t.state === "pending" && !t.text && <span className="visually-hidden">Waiting for reply</span>}
            </div>
            {t.state === "error" && (
              <p className="note note-error" role="alert">
                {t.error}
              </p>
            )}
          </li>
        ))}
      </ol>

      <form className="composer" onSubmit={send}>
        <label htmlFor="chat-input" className="visually-hidden">
          Message
        </label>
        <textarea
          id="chat-input"
          ref={inputRef}
          rows={3}
          value={input}
          placeholder="Message Nugget  (Enter to send, Shift+Enter for a new line)"
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKeyDown}
          autoFocus
        />
        {active ? (
          <button type="button" className="btn" onClick={stop} disabled={!active.streamId}>
            Stop
          </button>
        ) : (
          <button type="submit" className="btn btn-primary" disabled={!input.trim()}>
            Send
          </button>
        )}
      </form>
    </div>
  );
}
