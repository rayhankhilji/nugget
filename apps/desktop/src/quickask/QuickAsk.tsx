/**
 * The quick-ask window (`src-tauri/src/quickask.rs`): one question field, the streamed answer
 * with its `[n]` citations, the cited sources and a way on to Chat or Search. The answer comes
 * from the existing `retrieve_ask` command and its `chat.delta` / `chat.done` events.
 */
import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import {
  CHAT_ASK_BUDGET,
  api,
  errorMessage,
  events,
  isOpenableUrl,
  quickAskApi,
  type AskDateFilter,
  type ChatDone,
  type Citation,
  type Locality,
  type QuickAskHandover,
  type QuickAskPage,
} from "../api";
import { citedNumbers, groupSources, splitCitations, withheldNotice } from "../citations";
import { Announce, useAnnouncer } from "../components/Announce";
import { itemFileName } from "../components/Provenance";
import { Spinner } from "../components/ui";
import { useEvent, useOpener } from "../hooks";
import { useLocale, useT } from "../i18n";
import { textDir } from "../bidi";
import { sourceKindOf } from "../recency";
import { askCitations } from "../askSearch";
import { AskDateNote, PartialBadge } from "../components/AskSearchNote";

interface Answer {
  /** The question this answers (the field may have changed since). */
  question: string;
  streamId: string | null;
  provider: string | null;
  locality: Locality | null;
  citations: Citation[];
  excludedSensitive: number;
  general: boolean;
  /** JJ2: the question's date range and how it filtered the search. */
  date: AskDateFilter | null;
  text: string;
  done: ChatDone | null;
  error: string | null;
}

const PENDING: Answer = {
  question: "",
  streamId: null,
  provider: null,
  locality: null,
  citations: [],
  excludedSensitive: 0,
  general: false,
  date: null,
  text: "",
  done: null,
  error: null,
};

function titleOf(c: Citation, untitled: string): string {
  return c.title || itemFileName(c.item.key, sourceKindOf(c.item.source)) || untitled;
}

export function QuickAsk() {
  const t = useT();
  const { dir } = useLocale();
  const [question, setQuestion] = useState("");
  const [answer, setAnswer] = useState<Answer | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [announcement, announce] = useAnnouncer();
  const opener = useOpener();
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLOListElement>(null);
  // Chat events can arrive before `retrieve_ask` returns the stream id; buffer them by id.
  const streams = useRef(new Map<string, { text: string; done: ChatDone | null }>());

  const focusInput = useCallback((select: boolean) => {
    input.current?.focus();
    if (select) input.current?.select();
  }, []);

  const hide = useCallback(() => {
    quickAskApi.hide().catch(() => {});
  }, []);

  // Each time the window shows, the question field takes the keyboard.
  useEffect(() => {
    const onFocus = () => {
      if (!document.activeElement || document.activeElement === document.body) focusInput(true);
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [focusInput]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented) {
        e.preventDefault();
        hide();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [hide]);

  const prefill = useCallback((text: string) => {
    setQuestion(text);
    // After the render that puts the text in.
    setTimeout(() => focusInput(true), 0);
  }, [focusInput]);
  useEvent(quickAskApi.onPrefill, (text) => {
    prefill(text);
    // Consumed: so a later reload doesn't type it in again.
    quickAskApi.takePrefill().catch(() => {});
  });
  // A link that started Nugget may have been sent before this page subscribed.
  useEffect(() => {
    quickAskApi
      .takePrefill()
      .then((text) => text && prefill(text))
      .catch(() => {});
  }, [prefill]);

  const sync = (id: string) => {
    const s = streams.current.get(id);
    if (!s) return;
    setAnswer((a) => (a && a.streamId === id ? { ...a, text: s.text, done: s.done, error: s.done?.error ?? null } : a));
  };
  // Until `retrieve_ask` returns our stream id, buffer every stream (ours is among them); after
  // that, only ours.
  const waiting = useRef(false);
  const buffer = (id: string) => {
    let s = streams.current.get(id);
    if (!s && waiting.current) {
      s = { text: "", done: null };
      streams.current.set(id, s);
    }
    return s;
  };
  useEvent(events.onChatDelta, (d) => {
    const s = buffer(d.stream_id);
    if (!s) return;
    s.text += d.text;
    sync(d.stream_id);
  });
  useEvent(events.onChatDone, (d) => {
    const s = buffer(d.stream_id);
    if (!s) return;
    s.done = d;
    sync(d.stream_id);
  });

  const ask = async (e?: FormEvent) => {
    e?.preventDefault();
    const q = question.trim();
    if (!q) return;
    streams.current.clear();
    waiting.current = true;
    setFailure(null);
    setAnswer({ ...PENDING, question: q });
    announce(t("announce.answering"));
    try {
      const started = await api.retrieveAsk({
        query: q,
        budget_tokens: CHAT_ASK_BUDGET,
        provider: null,
        tier: "synthesis",
        utc_offset_minutes: -new Date().getTimezoneOffset(),
      });
      const s = streams.current.get(started.stream_id) ?? { text: "", done: null };
      streams.current.clear();
      streams.current.set(started.stream_id, s);
      setAnswer({
        question: q,
        streamId: started.stream_id,
        provider: started.provider,
        locality: started.locality,
        citations: askCitations(started),
        excludedSensitive: started.excluded_sensitive ?? 0,
        general: started.general === true,
        date: started.date ?? null,
        text: s.text,
        done: s.done,
        error: s.done?.error ?? null,
      });
    } catch (err) {
      setAnswer(null);
      setFailure(errorMessage(err));
    } finally {
      waiting.current = false;
    }
  };

  const valid = useMemo(() => new Set((answer?.citations ?? []).map((c) => c.n)), [answer?.citations]);
  const byN = useMemo(() => new Map((answer?.citations ?? []).map((c) => [c.n, c])), [answer?.citations]);
  const sources = useMemo(() => {
    const cited = citedNumbers(answer?.text ?? "", valid);
    const g = groupSources(answer?.citations ?? [], cited);
    return [...g.cited, ...g.uncited];
  }, [answer?.text, answer?.citations, valid]);

  const open = (c: Citation) => {
    const title = titleOf(c, t("search.untitled"));
    if (isOpenableUrl(c.url)) void opener.open(c.url, title);
  };
  const cite = (n: number) => {
    const c = byN.get(n);
    if (!c) return;
    if (isOpenableUrl(c.url)) open(c);
    else list.current?.querySelector<HTMLElement>(`[data-n~="${n}"]`)?.focus();
  };

  const streaming = answer !== null && answer.done === null && answer.error === null;
  const finished = answer !== null && answer.streamId !== null && answer.done !== null && answer.error === null;
  const answerError = answer?.error ?? null;
  useEffect(() => {
    if (finished && answer) announce(t("announce.answerReady", { count: citedNumbers(answer.text, valid).size }));
    // Once per finished answer.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [finished, answer?.streamId]);
  useEffect(() => {
    if (answerError) announce(t("quickAsk.failed", { error: answerError }));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [answerError]);

  // JJ2: the question goes along, and to Chat the answer with its sources once it finished
  // (the question in the field, alone, if it changed since).
  const handover = (): QuickAskHandover | undefined => {
    const typed = question.trim() || answer?.question || "";
    if (!typed) return undefined;
    if (finished && answer && answer.question === typed && answer.text.trim()) {
      return {
        question: typed,
        answer: {
          text: answer.text,
          provider: answer.provider,
          citations: answer.citations,
          general: answer.general,
          date: answer.date,
        },
      };
    }
    return { question: typed };
  };
  const openMain = (page: QuickAskPage) => {
    quickAskApi.openMain(page, handover()).catch((err: unknown) => setFailure(errorMessage(err)));
  };

  const withheld = answer?.provider ? withheldNotice(answer.excludedSensitive, answer.provider) : null;
  const hintId = "quickask-hint";

  return (
    <main className="quickask" dir={dir} aria-labelledby="quickask-title">
      <h1 id="quickask-title" className="visually-hidden">
        {t("quickAsk.title")}
      </h1>
      <form className="quickask-form" role="search" onSubmit={(e) => void ask(e)}>
        <label htmlFor="quickask-q" className="visually-hidden">
          {t("quickAsk.inputLabel")}
        </label>
        <input
          id="quickask-q"
          ref={input}
          className="quickask-input"
          type="search"
          dir="auto"
          autoComplete="off"
          spellCheck={false}
          autoFocus
          value={question}
          placeholder={t("quickAsk.placeholder")}
          aria-describedby={hintId}
          onChange={(e) => setQuestion(e.target.value)}
        />
        <button type="submit" className="btn btn-primary" disabled={streaming || !question.trim()}>
          {streaming ? <Spinner label={t("quickAsk.thinking")} /> : t("quickAsk.ask")}
        </button>
      </form>
      <p id={hintId} className="muted small quickask-hint">
        {t("quickAsk.hint")}
      </p>

      <div className="quickask-body">
        {failure && (
          <p className="note note-error" role="alert">
            {t("quickAsk.failed", { error: failure })}
          </p>
        )}
        {answer && (
          <section className="quickask-answer" aria-labelledby="quickask-answer-title" aria-busy={streaming}>
            <h2 id="quickask-answer-title" className="quickask-sub">
              {t("quickAsk.answer")}
              {answer.provider && (
                <span className="muted small"> {t("search.answerProvider", { provider: answer.provider, locality: answer.locality ?? "" })}</span>
              )}
            </h2>
            {withheld && <p className="note small">{withheld}</p>}
            {answer.general && <p className="muted small">{t("quickAsk.general")}</p>}
            <AskDateNote date={answer.date} />
            {answer.error ? (
              <p className="note note-error">{t("quickAsk.failed", { error: answer.error })}</p>
            ) : answer.text ? (
              <div className="quickask-text" dir={textDir(answer.text)}>
                {splitCitations(answer.text, valid).map((p, i) =>
                  p.kind === "text" ? (
                    <span key={i}>{p.text}</span>
                  ) : (
                    <button
                      key={i}
                      type="button"
                      className="cite"
                      onClick={() => cite(p.n)}
                      aria-label={t("search.citationLabel", { n: p.n, title: byN.get(p.n) ? titleOf(byN.get(p.n)!, t("search.untitled")) : "" })}
                    >
                      {p.raw}
                    </button>
                  ),
                )}
              </div>
            ) : (
              <p className="muted">
                <Spinner label={t("quickAsk.thinking")} /> {t("quickAsk.thinking")}
              </p>
            )}
            {sources.length > 0 && (
              <>
                <h3 className="quickask-sub">{t("quickAsk.sources")}</h3>
                <ol className="quickask-sources" ref={list}>
                  {sources.map((s) => {
                    const c = s.citations[0]!;
                    const title = titleOf(c, t("search.untitled"));
                    return (
                      <li key={s.key} className="quickask-source" data-n={s.ns.join(" ")} tabIndex={-1}>
                        <span className="cite-n">{s.ns.map((n) => `[${n}]`).join("")}</span>
                        <span className="quickask-source-title" dir="auto">
                          {title}
                        </span>
                        {s.citations.some((x) => x.partial) && <PartialBadge />}
                        {c.sensitive && <span className="badge badge-warn">{t("search.sensitive")}</span>}
                        {isOpenableUrl(c.url) && (
                          <button type="button" className="btn btn-small" onClick={() => open(c)} aria-label={t("search.openTitle", { title })}>
                            {t("search.open")}
                          </button>
                        )}
                      </li>
                    );
                  })}
                </ol>
              </>
            )}
          </section>
        )}
        {opener.status && (
          <p className={opener.status.error ? "note note-error small" : "muted small"} role={opener.status.error ? "alert" : undefined}>
            {opener.status.text}
          </p>
        )}
      </div>

      <div className="quickask-actions">
        <button type="button" className="btn btn-small" onClick={() => openMain("chat")}>
          {t("quickAsk.openInChat")}
        </button>
        <button type="button" className="btn btn-small" onClick={() => openMain("search")}>
          {t("quickAsk.showInSearch")}
        </button>
        <button type="button" className="btn btn-small btn-ghost quickask-close" onClick={hide}>
          {t("common.close")}
        </button>
      </div>
      <Announce message={announcement} />
    </main>
  );
}
