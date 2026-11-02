/**
 * Snooze times for a proposed action, which expires within a day: the presets that still fall
 * before it expires, and a small parser for what people type ("in 2 hours", "at 4pm", "16:30",
 * "tonight", "tomorrow 9am"). Everything is local time; results are capped at `expires`.
 */
export type SnoozePreset = "hour" | "afternoon" | "evening";

const at = (base: Date, h: number, m = 0) => new Date(base.getFullYear(), base.getMonth(), base.getDate(), h, m, 0, 0);

/** The preset times still ahead of `now` and before `expires`. */
export function snoozePresets(now: Date, expires: Date): { id: SnoozePreset; until: Date }[] {
  const out: { id: SnoozePreset; until: Date }[] = [
    { id: "hour", until: new Date(now.getTime() + 3600_000) },
    { id: "afternoon", until: at(now, 14) },
    { id: "evening", until: at(now, 18) },
  ];
  return out.filter((p) => p.until > now && p.until <= expires);
}

/** A typed time, or null when it can't be read or falls outside (now, expires]. */
export function parseSnooze(text: string, now: Date, expires: Date): Date | null {
  const s = text.trim().toLowerCase();
  if (!s) return null;
  let when: Date | null = null;
  const rel = /^in\s+(\d+(?:\.\d+)?)\s*(m|min|mins|minutes?|h|hrs?|hours?)$/.exec(s);
  if (rel) {
    const n = Number(rel[1]);
    when = new Date(now.getTime() + n * (rel[2]!.startsWith("m") ? 60_000 : 3600_000));
  } else {
    const tomorrow = /^tomorrow\b/.test(s);
    const rest = s.replace(/^(tomorrow|today)\s*/, "").replace(/^at\s+/, "");
    const base = tomorrow ? new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1) : now;
    if (rest === "tonight" || rest === "this evening") when = at(base, 18);
    else if (rest === "this afternoon" || rest === "afternoon") when = at(base, 14);
    else if (rest === "" && tomorrow) when = at(base, 9);
    else {
      const t = /^(\d{1,2})(?::(\d{2}))?\s*(am|pm)?$/.exec(rest);
      if (t) {
        let h = Number(t[1]);
        const m = Number(t[2] ?? 0);
        if (t[3] === "pm" && h < 12) h += 12;
        if (t[3] === "am" && h === 12) h = 0;
        if (h < 24 && m < 60) {
          when = at(base, h, m);
        }
      }
    }
  }
  if (!when || Number.isNaN(when.getTime()) || when <= now || when > expires) return null;
  return when;
}
