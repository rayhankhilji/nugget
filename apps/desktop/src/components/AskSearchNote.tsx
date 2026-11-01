import { askDateNote } from "../askSearch";
import type { AskDateFilter } from "../api";
import { useLocale, useT } from "../i18n";

/** "Searched items from 26 Oct to 1 Nov" under an answer whose question named a date (JJ2). */
export function AskDateNote({ date, className = "" }: { date: AskDateFilter | null | undefined; className?: string }) {
  const t = useT();
  const { code } = useLocale();
  const note = askDateNote(date, code);
  if (!note) return null;
  return <p className={`muted small ask-dates ${className}`.trim()}>{t(note.key, note.args)}</p>;
}

/** The "Partial match" label on a source that lacks some of the question's key words (JJ2). */
export function PartialBadge() {
  const t = useT();
  return (
    <span className="badge badge-neutral ask-partial" title={t("ask.partialHint")}>
      {t("ask.partial")}
      <span className="visually-hidden">: {t("ask.partialHint")}</span>
    </span>
  );
}
