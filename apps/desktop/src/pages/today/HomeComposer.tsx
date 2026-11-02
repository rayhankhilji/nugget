import { useId, useState, type FormEvent, type KeyboardEvent } from "react";
import { requestChatAsk, requestPage } from "../../hooks.nav";
import { useT } from "../../i18n";
import { Icon } from "../../components/Icon";

/** Home's ask box: Enter sends the question to Chat as a new conversation (Shift+Enter: new line). */
export function HomeComposer() {
  const t = useT();
  const [q, setQ] = useState("");
  const [sent, setSent] = useState(false);
  const labelId = useId();

  const ask = (e?: FormEvent) => {
    e?.preventDefault();
    const question = q.trim();
    if (!question) return;
    setSent(true);
    requestChatAsk(question);
    requestPage("chat");
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      ask();
    }
  };

  return (
    <form className="home-composer" onSubmit={ask}>
      <label id={labelId} className="visually-hidden" htmlFor={`${labelId}-q`}>
        {t("home.askLabel")}
      </label>
      <textarea
        id={`${labelId}-q`}
        rows={2}
        value={q}
        placeholder={t("home.askPlaceholder")}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={onKeyDown}
        dir="auto"
      />
      <div className="home-composer-bar">
        <span className="muted small">{t("home.askHint")}</span>
        <button type="submit" className={`home-send${sent ? " is-sent" : ""}`} disabled={!q.trim()} aria-label={t("home.askSend")}>
          <Icon name="up" size={16} />
        </button>
      </div>
    </form>
  );
}
