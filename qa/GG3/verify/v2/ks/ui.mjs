// Scenario 6: the desktop app's confirmation dialog vs. the kill switch, through the real frontend
// (apps/desktop dist + e2e/bridge.mjs + tauri-shim) in headless Chromium, against the real daemon.
//   KS_DIST=<built apps/desktop dist> node ui.mjs        (KS_REPEAT=N, default 3)
// Writes logs/ui.json and logs/ui-*.png.
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { setup } from "./lib/setup.mjs";
import { isEvent, now } from "./lib/ipc.mjs";
import { startBridge } from "../../../../../apps/desktop/e2e/bridge.mjs";
import { newContext, t } from "../../../../../apps/desktop/e2e/harness.mjs";

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.KS_PLAYWRIGHT ?? "/opt/node-tools/node_modules/playwright");
const here = path.dirname(fileURLToPath(import.meta.url));
const LOGS = path.join(here, "logs");
const DIST = process.env.KS_DIST;
if (!DIST || !fs.existsSync(path.join(DIST, "index.html"))) throw new Error("set KS_DIST to a built apps/desktop dist/ (see README)");
const REPEAT = Number(process.env.KS_REPEAT ?? 3);
const ONBOARDED = { version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const SCRIPT = [
  { calls: [{ name: "browser_snapshot", args: {} }] },
  { calls: [{ name: "browser_type", args: { element: "n2", text: "Synthetic QA body: quarterly numbers attached" } }] },
  { calls: [{ name: "browser_submit", args: { element: "n3" } }] },
  { calls: [{ name: "finish", args: { outcome: "done", summary: "sent" } }] },
];

const env = await setup({ outDir: path.join(LOGS, "ui-daemon") });
const bridge = await startBridge({ socket: env.daemon.socket, dist: DIST });
const browser = await chromium.launch(process.env.E2E_CHROMIUM ? { executablePath: process.env.E2E_CHROMIUM } : {});
const results = [];

async function openAgentPage() {
  const ctx = await newContext(browser);
  await ctx.addInitScript(([k, v]) => localStorage.setItem(k, JSON.stringify(v)), ["nugget.onboarding.v1", ONBOARDED]);
  await ctx.addInitScript(() => localStorage.setItem("nugget.page", "agent"));
  const page = await ctx.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  bridge.deliverEvents(page);
  await page.goto(`${bridge.url}index.html`);
  await page.locator(`main[aria-label="${t("app.nav.agent")}"]`).waitFor();
  return { ctx, page, errors };
}

async function startFromUi(page) {
  env.mock.setScript(SCRIPT);
  env.fake.reset();
  await page.getByLabel(t("agent.form.goal")).fill("Send the synthetic QA message to qa@example.test");
  await page.getByRole("button", { name: t("agent.form.run"), exact: true }).click();
  const dialog = page.getByRole("dialog", { name: t("agent.confirm.title") });
  await dialog.waitFor({ timeout: 20000 });
  const facts = await dialog.locator("dl.confirm-facts").innerText();
  const taskId = env.ipc.notes.filter((n) => n.method === "agent.event" && n.params.event.event === "confirmation_needed").at(-1).params.task_id;
  return { dialog, facts, taskId };
}

const btn = (dialog, key) => dialog.getByRole("button", { name: t(key), exact: true });

for (let i = 0; i < REPEAT; i++) {
  await env.ipc.rpc("policy.resume");
  const r = { rep: i + 1 };
  // ---- A: dialog content; Stop from outside (hotkey / tray / policy.kill) while it is open ----
  {
    const { ctx, page, errors } = await openAgentPage();
    const { dialog, facts, taskId } = await startFromUi(page);
    await page.screenshot({ path: path.join(LOGS, `ui-${i + 1}-a-dialog.png`) });
    r.dialog_facts = facts;
    r.typed_body_shown_in_dialog = facts.includes("quarterly numbers");
    const approveEnabledBefore = await btn(dialog, "agent.confirm.approveOnce").isEnabled();
    const kill = await env.ipc.call("policy.kill");
    // Sample the DOM directly (no Playwright auto-wait) every ~5 ms: when does the dialog go, and
    // is Approve ever disabled while it is still shown?
    const sample = await page.evaluate(async ({ title, approve }) => {
      const t0 = performance.now();
      let sawDisabled = false;
      while (performance.now() - t0 < 5000) {
        const d = [...document.querySelectorAll("dialog")].find((x) => x.getAttribute("aria-label") === title || x.querySelector("h2")?.textContent === title);
        if (!d) return { goneMs: performance.now() - t0, sawDisabled };
        const b = [...d.querySelectorAll("button")].find((x) => x.textContent.trim() === approve);
        if (b && b.disabled) sawDisabled = true;
        await new Promise((r) => setTimeout(r, 5));
      }
      return { goneMs: null, sawDisabled };
    }, { title: t("agent.confirm.title"), approve: t("agent.confirm.approveOnce") });
    const gone = sample.goneMs === null ? null : sample.goneMs + (now() - kill.t) * 0; // measured from just after the kill reply
    const sawDisabled = sample.sawDisabled;
    await page.screenshot({ path: path.join(LOGS, `ui-${i + 1}-a-after-kill.png`) });
    r.A = {
      approve_enabled_before_kill: approveEnabledBefore,
      dialog_gone_after_kill_ms: gone === null ? "still open after 2 s" : +gone.toFixed(0),
      approve_disabled_while_dialog_open_after_kill: sawDisabled,
      halted_banner: await page.getByRole("alert").filter({ hasText: t("agent.halted.title") }).count(),
      submits: env.fake.st.acts.filter((a) => a.kind === "submit").length,
      page_errors: errors,
    };
    await ctx.close();
  }
  // ---- B: keyboard user has Approve once focused; Stop and Enter land together ----
  for (const gap of [0, 20, 60]) {
    await env.ipc.rpc("policy.resume");
    const { ctx, page, errors } = await openAgentPage();
    const { dialog, taskId } = await startFromUi(page);
    const approve = btn(dialog, "agent.confirm.approveOnce");
    // Keyboard: focus starts on Deny (data-autofocus); Tab until Approve once has focus.
    for (let k = 0; k < 6 && !(await approve.evaluate((el) => el === document.activeElement)); k++) await page.keyboard.press("Tab");
    const focused = await approve.evaluate((el) => el === document.activeElement);
    const kill = await env.ipc.call("policy.kill");
    if (gap) await sleep(gap);
    const tEnter = now();
    await page.keyboard.press("Enter");
    const fin = await env.ipc.waitNote(isEvent(taskId, "finished"), 5000, "finished").catch(() => null);
    await sleep(300);
    const confirmCalls = bridge.calls.filter((c) => c.cmd === "agent_confirm");
    const sent = confirmCalls.length > (r._confirms ?? 0);
    const last = sent ? confirmCalls.at(-1) : null;
    await page.screenshot({ path: path.join(LOGS, `ui-${i + 1}-b-gap${gap}.png`) });
    const evs = env.ipc.notes.filter((n) => n.method === "agent.event" && n.params.task_id === taskId).map((n) => n.params.event);
    const g = await env.ipc.rpc("agent.get", { task_id: taskId });
    r[`B_gap${gap}ms`] = {
      approve_had_focus: focused,
      enter_after_kill_reply_ms: +(tEnter - kill.t).toFixed(1),
      ui_sent_agent_confirm: sent,
      agent_confirm_result: last ? (last.ok ? "accepted" : `error: ${last.message}`) : "not sent",
      resolved_event: evs.find((e) => e.event === "confirmation_resolved")?.answer ?? "none",
      finished: fin?.params.event.outcome?.outcome,
      log_tail: g.entries.slice(-3).map((e) => `${e.kind}${e.tool ? `(${e.tool})` : ""}${e.decision ? ":" + (e.decision.decision ?? e.decision) : ""}${e.interrupted ? ":INTERRUPTED" : ""}`),
      submits: env.fake.st.acts.filter((a) => a.kind === "submit").length,
      error_note_shown: await page.locator(".note-error, [role=alert]").allInnerTexts(),
      page_errors: errors,
    };
    r._confirms = confirmCalls.length;
    await ctx.close();
  }
  delete r._confirms;
  results.push(r);
  console.log(JSON.stringify(r));
}
fs.writeFileSync(path.join(LOGS, "ui.json"), JSON.stringify(results, null, 2));
await browser.close();
await bridge.close();
await env.teardown();
