// @vitest-environment jsdom
// GG7 explore probes: AI output in other UI locales. Prints "GG7 <id>:" lines; writes rendered
// fragments to /tmp/claude-0/ui-shots/*.html for bidi.mjs (Chromium layout). Asserts nothing.
import { mkdirSync, writeFileSync } from "node:fs";
import { afterEach, beforeEach, it } from "vitest";
import { cleanup, flush, installDomShims, mockDaemon, render } from "../../../../apps/desktop/src/test/render";
import { loadLocale, t } from "../../../../apps/desktop/src/i18n";
import { TodayPage } from "../../../../apps/desktop/src/pages/Today";
import { Overlay } from "../../../../apps/desktop/src/overlay/Overlay";
import { Markdown } from "../../../../apps/desktop/src/components/Markdown";
import { truncateText } from "../../../../apps/desktop/src/knowledge";
import { formatDatesInText } from "../../../../apps/desktop/src/format";
import { reminderNotification } from "../../../../apps/desktop/src/proactiveNotifications";

const OUT = "/tmp/claude-0/ui-shots";
mkdirSync(OUT, { recursive: true });
const log = (id: string, v: unknown) => console.log(`GG7 ${id}: ${typeof v === "string" ? v : JSON.stringify(v)}`);
beforeEach(() => installDomShims());
afterEach(async () => { cleanup(); await loadLocale("en"); });

// Card text exactly as crates/nugget-proactive/src/generators.rs builds it (lines 196, 877, 1068-1069).
const mk = (id: string, kind: string, title: string, detail: string) => ({
  id, kind, title, detail, summary: null, people: [], due: "2026-11-03T08:00:00Z", score: 1, rank: 0.9, factors: [],
  evidence: [], actions: [], state: "new", sensitive: false, created_at: "2026-10-30T08:00:00Z",
});
const cards = [
  mk("c1", "unanswered_thread", "Reply to محمد العلي: عرض السعر 2026", "You asked 3 days ago and haven't heard back."),
  mk("c2", "renewal", "Pixelboard renews 2026-11-03 — $49.00", "From your note \"Abos\"."),
  mk("c3", "awaiting_reply", "No answer from 田中: 見積もりの件", "You asked 5 days ago and haven't heard back."),
];

for (const loc of ["de", "ja", "ar"]) {
  it(`today cards in ${loc}`, async () => {
    await loadLocale(loc);
    mockDaemon({ cards_list: { cards, last_refresh: "2026-10-31T08:00:00Z", enabled: true }, brief_get: null });
    const { container } = await render(<TodayPage />);
    await flush(10);
    const titles = [...container.querySelectorAll(".today-card-title, .today-card-detail, .today-card-meta")].map((e) => e.textContent);
    log(`A.cards.${loc}`, titles);
    if (loc === "ar") writeFileSync(`${OUT}/today-ar.html`, container.innerHTML);
  });
}

it("formatDatesInText exists but card titles skip it", () => {
  for (const loc of ["de", "ja", "ar"]) log(`B.formatDatesInText.${loc}`, formatDatesInText(cards[1]!.title, loc, "UTC"));
});

it("plurals around AI output", async () => {
  for (const loc of ["ar", "hi", "ja"]) {
    await loadLocale(loc);
    const row: Record<string, string[]> = {};
    for (const k of ["today.evidence", "doubletap.overlay.citations", "chat.check.flagged", "knowledge.evidenceSummary"] as const)
      row[k] = [0, 1, 2, 3, 11, 100].map((n) => t(k, { n, count: n }));
    log(`C.plural.${loc}`, row);
  }
});

it("truncation of Devanagari / Arabic / emoji", () => {
  const hi = "प्रियंका ".repeat(3) + "क्षत्रिय";
  for (const max of [10, 11, 13]) log(`D.truncateText.hi.${max}`, truncateText(hi.replace(/ /g, ""), max).text);
  const ar = "سَيَصِلُ الطَّلَبُ";
  log("D.truncateText.ar.3", truncateText(ar, 3).text);
  log("D.truncateText.emoji", truncateText("👩‍👩‍👧 family plan renewal for the household", 2).text);
  const rem = reminderNotification({ text: "x".repeat(299) + "😀 after", id: 1 } as never).body;
  log("D.reminder.slice300.lastCodeUnit", rem.charCodeAt(rem.length - 1).toString(16) + " wellFormed=" + (rem as unknown as { isWellFormed(): boolean }).isWellFormed());
});

it("overlay and chat answer markup for Chromium", async () => {
  await loadLocale("ar");
  const view = { phase: "ready", text: "iPhone 15 سيصل يوم الخميس، والفاتورة ١٢٠ دينار.", provider: "local", locality: "on_device", citations: 2, excluded_sensitive: 0, message: null, verified: null, method: null };
  mockDaemon({ overlay_state: view });
  const o = await render(<Overlay />);
  await flush(10);
  writeFileSync(`${OUT}/overlay-ar.html`, o.container.innerHTML);
  log("E.overlay.textDir", o.container.querySelector(".overlay-text")?.getAttribute("dir"));
  const answer = "**Nugget Pro** هو الاشتراك الحالي [1].\n\nيتجدد في 3 نوفمبر بسعر 49 دولارًا [2].";
  const m = await render(<div className="msg-body msg-md" dir="auto"><Markdown text={answer} citations={undefined as never} /></div>);
  writeFileSync(`${OUT}/chat-ar.html`, m.container.innerHTML);
  log("E.chat.paragraphDirs", [...m.container.querySelectorAll(".msg-body p")].map((p) => p.getAttribute("dir")));
});
