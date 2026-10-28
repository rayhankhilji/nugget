/**
 * App-wide OS notifications for the proactive layer: the morning brief (`brief.ready`) and due
 * reminders (`reminder.due`). Registered once by `App`, whichever page is open. Notification
 * text is plain text composed here; the Rust shell shows it (`notify` command).
 */
import { proactiveApi, proactiveEvents, type BriefView, type ReminderView } from "./api";
import { t } from "./i18n";

/** Distinct cards the brief lists (top, overdue and renewals may overlap). */
export function briefItemCount(brief: BriefView): number {
  return new Set([...brief.top, ...brief.overdue, ...brief.renewals].map((c) => c.card_id)).size;
}

export function briefNotification(brief: BriefView): { title: string; body: string } {
  const items = briefItemCount(brief);
  const meetings = brief.meetings.length;
  const body =
    items === 0 && meetings === 0
      ? t("today.notify.briefEmpty")
      : meetings > 0
        ? t("today.notify.briefBodyMeetings", { n: items, m: meetings })
        : t("today.notify.briefBody", { n: items });
  return { title: t("today.notify.briefTitle", { n: items }), body };
}

/** One line of untrusted reminder text, kept short. */
export function reminderNotification(reminder: ReminderView): { title: string; body: string } {
  const text = reminder.text.replace(/\s+/g, " ").trim().slice(0, 300);
  return { title: t("today.notify.reminderTitle"), body: text || t("today.notify.reminderEmpty") };
}

/** Subscribe; returns a function that unsubscribes. */
export function startProactiveNotifications(): () => void {
  let stopped = false;
  const unlisteners: (() => void)[] = [];
  const keep = (p: Promise<() => void>) =>
    p
      .then((u) => {
        if (stopped) u();
        else unlisteners.push(u);
      })
      .catch(() => {});
  const show = ({ title, body }: { title: string; body: string }) => {
    proactiveApi.notify(title, body).catch(() => {});
  };
  void keep(proactiveEvents.onBriefReady((b) => show(briefNotification(b))));
  void keep(proactiveEvents.onReminderDue((r) => show(reminderNotification(r))));
  return () => {
    stopped = true;
    for (const u of unlisteners.splice(0)) u();
  };
}
