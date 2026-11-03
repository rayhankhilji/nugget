// Heed Cloud end to end: the real app (dist/), the real shell cloud client (e2e_shell, see
// cloud-shell.mjs) and the real nuggetd against a running self-hosted stack with fake Composio,
// Anthropic, OpenAI and Stripe (crates/nugget-cloud/examples/fake_providers.rs). Run it with
// `selfhost/e2e.sh`, which starts all of that; the scenario skips itself without E2E_CLOUD_URL.
//
// Magic-link sign-in from Mailpit, connecting Gmail and Calendar, the sync pill, the brief with
// cited author chips, cards and an action proposed from one; editing and approving it (Undo hold,
// exactly one Composio execute with the edited body, a stale digest refused); snoozing and
// rejecting others; a cited quick ask and a ghost-text completion through /llm/v1; plan, trial
// and usage with fake Stripe (Checkout and portal) or the self-host state with billing off; and
// deleting the account, after which none of the user's rows remain in Postgres.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { FIRST_RUN_CONFIG, rpc, rpcStream, startDaemon } from "./daemon.mjs";
import { check, checkWire, DIST, exactName, expect, log, navTo, newContext, OUT, shot, stopDaemon, t, tPattern, until } from "./harness.mjs";

const env = (k, d) => process.env[k] || d;
const CLOUD_URL = env("E2E_CLOUD_URL");
const MAILPIT = env("E2E_MAILPIT_URL", "http://localhost:8025");
const FAKE = env("E2E_FAKE_URL", "http://127.0.0.1:9100");
const BILLING = env("E2E_CLOUD_BILLING", "on") !== "off";
const SYNC_TIMEOUT_MS = Number(env("E2E_CLOUD_SYNC_TIMEOUT_MS", 120_000));

/** `t(key)` with `{name}` placeholders filled. */
const fmt = (key, vars) => t(key).replace(/\{(\w+)\}/g, (_, k) => String(vars[k] ?? ""));
const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

async function getJson(url, init) {
  const r = await fetch(url, init);
  const text = await r.text();
  if (!r.ok) throw new Error(`${init?.method ?? "GET"} ${url}: ${r.status} ${text.slice(0, 300)}`);
  return text ? JSON.parse(text) : null;
}

/** Runs SQL in the stack's database as postgres (E2E_CLOUD_PSQL); returns the trimmed output. */
function psql(sql) {
  const cmd = env("E2E_CLOUD_PSQL");
  if (!cmd) throw new Error("E2E_CLOUD_PSQL is not set (selfhost/e2e.sh sets it)");
  const r = spawnSync("sh", ["-c", cmd], { input: sql, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`psql failed (${r.status}): ${r.stderr}`);
  return r.stdout.trim();
}

/** The newest magic link Mailpit holds for `email`. */
async function magicLink(email) {
  return until(`a magic-link email for ${email} reaches Mailpit`, async () => {
    const list = await getJson(`${MAILPIT}/api/v1/search?query=${encodeURIComponent(`to:${email}`)}`);
    const id = list.messages?.[0]?.ID;
    if (!id) return [false, list.messages_count ?? 0];
    const msg = await getJson(`${MAILPIT}/api/v1/message/${id}`);
    const body = `${msg.Text ?? ""} ${msg.HTML ?? ""}`;
    const m = body.match(/https?:\/\/[^\s"'<)]+verify[^\s"'<)]*/);
    return [Boolean(m), m ? m[0].replaceAll("&amp;", "&") : body.slice(0, 200)];
  }, 30_000);
}

/** Opens `url` like a browser would, without following redirects; returns the Location. */
async function redirectOf(url) {
  const r = await fetch(url, { redirect: "manual" });
  const loc = r.headers.get("location");
  if (!loc) throw new Error(`no redirect from ${url}: ${r.status} ${(await r.text()).slice(0, 200)}`);
  return loc;
}

const fakeCalls = () => getJson(`${FAKE}/__fake/calls`);

export async function cloudScenario({ browser, bin }) {
  const summary = { billing: BILLING };
  if (!CLOUD_URL) {
    log("cloud: E2E_CLOUD_URL is not set; skipped (run selfhost/e2e.sh)");
    await check("cloud: skipped without a stack (selfhost/e2e.sh runs it)", async () => {});
    return summary;
  }
  const shellBin = env("E2E_CLOUD_SHELL");
  expect(shellBin && fs.existsSync(shellBin), `E2E_CLOUD_SHELL (${shellBin}) is not the built e2e_shell example`);
  const anon = env("E2E_CLOUD_ANON_KEY");
  const email = `e2e-${Date.now().toString(36)}-${process.pid}@me.example`;
  summary.email = email;
  await getJson(`${FAKE}/__fake/reset`, { method: "POST" });

  // A note on this computer for the quick ask (it answers from the local index, on the cloud model).
  const notes = fs.mkdtempSync(path.join(os.tmpdir(), "nugget-e2e-cloud-"));
  fs.writeFileSync(path.join(notes, "kickoff.md"), "# Harbourline kickoff\n\nThe Harbourline kickoff is in the Atlas room on the fourth floor.\n");

  // No local model here to clear files for cloud models (triage), so this user lets unjudged
  // files go to them; otherwise every local passage is withheld from the cloud model.
  const config = `${FIRST_RUN_CONFIG}\n[ingest]\nassume_unflagged_safe = true\n`;
  const daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "cloud-nuggetd.out.log"), config });
  log(`cloud: daemon home ${daemon.home}, stack ${CLOUD_URL}, billing ${BILLING ? "on" : "off"}, ${email}`);
  let bridge;
  let ctx;
  const pageErrors = [];
  try {
    bridge = await startBridge({
      socket: daemon.socket,
      dist: DIST,
      cloud: { bin: shellBin, env: { NUGGET_SUPABASE_URL: CLOUD_URL, NUGGET_SUPABASE_ANON_KEY: anon, NUGGET_CLOUD_URL: CLOUD_URL } },
    });
    const shell = (cmd, args = {}) => bridge.invoke(cmd, args);
    ctx = await newContext(browser, { onboarded: true, page: "today" });
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    bridge.deliverEvents(page);
    const main = (id) => page.locator(`main[aria-label="${t(`app.nav.${id}`)}"]`);
    const go = async (id) => {
      await navTo(page, id);
      await main(id).waitFor();
    };
    const fail = (name) => async (e) => {
      await shot(page, `FAIL-cloud-${name}`);
      throw e;
    };

    // ---- 1. sign in with a magic link ----
    await check("cloud: magic-link sign-in from Mailpit lands on Home signed in", async () => {
      try {
        await page.goto(`${bridge.url}index.html`);
        await go("connections");
        await page.getByLabel(t("cloud.signIn.email"), { exact: true }).fill(email);
        await page.getByRole("button", { name: exactName("cloud.signIn.sendLink") }).click();
        await page.getByRole("status").filter({ hasText: fmt("cloud.signIn.checkEmail", { email }) }).waitFor();
        const link = await magicLink(email);
        const callback = await redirectOf(link);
        expect(/^(nugget|heed):\/\/auth\/callback\?/.test(callback), `the link redirected to ${callback}`);
        const r = await bridge.cloudDeepLink(callback);
        expect(!r.err, `the deep link failed: ${JSON.stringify(r.err)}`);
        await page.getByText(fmt("cloud.signedInAs", { email }), { exact: true }).waitFor();
        await go("today");
        await until("Home reads from the cloud (cloud_brief called)", async () => [bridge.calls.some((c) => c.cmd === "cloud_brief"), bridge.calls.map((c) => c.cmd).slice(-8)]);
        const status = await shell("cloud_model_status");
        expect(status.ok?.active === true, `nuggetd did not get the cloud model: ${JSON.stringify(status)}`);
        await shot(page, "cloud-signed-in");
      } catch (e) {
        await fail("sign-in")(e);
      }
    });

    // ---- 7a. billing on: start the trial through fake Stripe Checkout first (intel needs a plan) ----
    if (BILLING) {
      await check("cloud: Checkout opens fake Stripe and its webhook starts the trial", async () => {
        try {
          await go("account");
          const before = bridge.opened.length;
          await page.getByRole("button", { name: fmt("cloud.billing.startMonthlyLabel", { name: "Heed" }), exact: true }).click();
          await page.getByRole("status").filter({ hasText: t("cloud.billing.inBrowser") }).waitFor();
          const url = bridge.opened.slice(before).find((u) => u.startsWith(`${FAKE}/c/`));
          expect(url, `Checkout did not open a fake Stripe session: ${JSON.stringify(bridge.opened)}`);
          const back = await redirectOf(url);
          expect(back === `${CLOUD_URL}/billing/done`, `Checkout returned to ${back}`);
          const done = await (await fetch(back)).text();
          expect(done.includes("nugget://billing/done"), `the done page does not hand back to the app: ${done.slice(0, 200)}`);
          bridge.cloudEvent("nugget:cloud-billing");
          const { stripe } = await fakeCalls();
          const form = stripe.find((c) => c.path === "/v1/checkout/sessions")?.form ?? {};
          expect(form.mode === "subscription" && form["subscription_data[trial_period_days]"] === "14", `checkout form: ${JSON.stringify(form)}`);
          await page.locator(".plan-strip").getByText(t("cloud.billing.status.trialing"), { exact: true }).waitFor();
          // The rest of Account follows too: the plan's devices.
          await page.locator('section[aria-labelledby="account-devices"] p').getByText(/^Your plan includes \d+ devices?\./).waitFor();
        } catch (e) {
          await fail("checkout")(e);
        }
      });
    }

    // ---- 2. connect Gmail and Calendar, then sync ----
    await check("cloud: connect Gmail and Calendar; the sync pill and Connections show the import", async () => {
      try {
        await go("connections");
        for (const name of ["Gmail", "Google Calendar"]) {
          const before = bridge.opened.length;
          await page.getByRole("button", { name: fmt("cloud.connectLabel", { name }), exact: true }).click();
          await page.getByRole("status").filter({ hasText: fmt("cloud.connectInBrowser", { name }) }).waitFor();
          const consent = bridge.opened.slice(before).find((u) => u.startsWith(`${FAKE}/connect/`));
          expect(consent, `connecting ${name} opened ${JSON.stringify(bridge.opened.slice(before))}`);
          await fetch(consent);
          bridge.cloudEvent("nugget:cloud-connections");
          await page.locator(".cloud-conn").filter({ hasText: name }).waitFor();
        }
        // Sync now and watch: the pill (any page) and the card's progress while it imports.
        const seen = { pill: false, progress: false };
        const watch = setInterval(async () => {
          if (await page.locator(".sync-pill").isVisible().catch(() => false)) seen.pill = true;
          if (await page.locator(".cloud-conn-sync [role=status]").first().isVisible().catch(() => false)) seen.progress = true;
        }, 100);
        try {
          await page.getByRole("button", { name: exactName("cloud.sync") }).click();
          await until("both apps finish their first import", async () => {
            const r = await shell("cloud_connections_list");
            const conns = r.ok?.connections ?? [];
            return [conns.length === 2 && conns.every((c) => !c.syncing && c.last_synced_at && !c.last_error), conns.map((c) => ({ t: c.toolkit, s: c.syncing, n: c.items_synced, e: c.last_error }))];
          }, SYNC_TIMEOUT_MS);
        } finally {
          clearInterval(watch);
        }
        summary.syncSeen = seen;
        expect(seen.pill, "the sidebar's sync pill never showed during the import");
        expect(seen.progress, "Connections never showed the import's progress");
        await page.locator(".cloud-sync-results li").first().waitFor();
      } catch (e) {
        await fail("connect")(e);
      }
    });

    // ---- 3. the brief, cards, and an action proposed from a card ----
    let proposed;
    await check("cloud: Home has a brief with author citation chips and cards", async () => {
      try {
        await go("today");
        const refresh = page.getByRole("button", { name: exactName("today.refresh") });
        await refresh.click();
        const chip = page.locator(".today-brief .cite.cite-app");
        let last = Date.now();
        await until("the brief's prose has a citation chip naming Dana", async () => {
          const names = (await chip.count()) ? await chip.allTextContents() : [];
          // The intel run can finish after the first refresh: check again now and then (not in a
          // tight loop: refreshes are rate-limited).
          if (!names.length && Date.now() - last > 8000) {
            last = Date.now();
            await refresh.click().catch(() => {});
          }
          return [names.some((s) => s.includes("Dana Okoro")), names];
        }, 60_000);
        const label = await chip.first().getAttribute("aria-label");
        expect(new RegExp(`^${escapeRe("Dana Okoro · Gmail")}, source 1$`).test(label ?? ""), `chip label: ${label}`);
        // The cards: counted in Home's summary (they are listed under Needs you).
        const want = /\b[1-9]\d* things? needs? you/; // home.needsYou, plural
        await until("Home's summary counts the cards", async () => {
          const d = (await page.locator(".page-home .page-header").textContent()) ?? "";
          return [want.test(d), d];
        });
        await shot(page, "cloud-home");
      } catch (e) {
        await fail("home")(e);
      }
    });

    await check("cloud: Needs you shows an action proposed from a card", async () => {
      try {
        await go("needs");
        const listed = (await shell("cloud_cards", { tz: "UTC" })).ok?.cards ?? [];
        const replies = listed.filter((c) => c.kind === "unanswered_thread");
        summary.cards = listed.map((c) => c.kind);
        expect(replies.length >= 3, `unanswered-thread cards: ${JSON.stringify(summary.cards)}`);
        for (const [i, c] of replies.slice(0, 3).entries()) {
          await page.locator(`.needs-list [data-card="${c.id}"]`).click();
          await page.locator(".needs-pane .today-proposed button.btn-primary").click();
          await until(`action ${i + 1} is proposed`, async () => {
            const r = await shell("cloud_actions_list", { status: "pending" });
            return [(r.ok ?? []).length >= i + 1, r.ok?.length ?? r];
          }, 30_000);
          await page.locator(".needs-list [data-action]").nth(i).waitFor();
        }
        const r = await shell("cloud_actions_list", { status: "pending" });
        proposed = r.ok;
        expect(proposed.every((a) => a.kind === "reply_mail" && a.toolkit === "gmail"), JSON.stringify(proposed));
        await page.locator("#needs-approvals").waitFor();
        await shot(page, "cloud-needs");
      } catch (e) {
        await fail("needs")(e);
      }
    });

    // ---- 4. edit and approve one: Undo hold, one execute with the edited body, stale digest refused ----
    const EDITED = "Hi Dana, the signed contract is attached. Edited in the e2e run.";
    await check("cloud: edit (E) and approve an action; Undo holds it; Composio runs it exactly once", async () => {
      try {
        const target = proposed.find((a) => a.summary.includes("dana@harbourline.example")) ?? proposed[0];
        const oldDigest = target.digest;
        const row = page.locator(`.needs-list [data-action="${target.id}"]`);
        await row.click();
        await page.keyboard.press("e");
        const body = page.getByLabel(t("needs.edit.body"), { exact: true });
        await body.fill(EDITED);
        await page.getByRole("button", { name: exactName("needs.edit.save") }).click();
        await until("the edited body is in the preview", async () => {
          const txt = (await page.locator(".needs-pane .action-fields").textContent()) ?? "";
          return [txt.includes("Edited in the e2e run."), txt.slice(0, 200)];
        });
        const edited = (await shell("cloud_actions_list", { status: "pending" })).ok.find((a) => a.id === target.id);
        expect(edited.digest !== oldDigest, "editing did not change the digest");
        expect(JSON.stringify(edited.preview).includes("Edited in the e2e run."), `the server's preview: ${JSON.stringify(edited.preview)}`);

        // Approve, then Undo inside the hold: nothing reaches Composio.
        await row.click();
        await page.keyboard.press(process.platform === "darwin" ? "Meta+Enter" : "Control+Enter");
        const toast = page.locator(".toast").filter({ hasText: fmt("needs.actions.sent", { summary: edited.summary }) });
        await toast.waitFor();
        await toast.getByRole("button", { name: exactName("toast.undo") }).click();
        await page.locator(".toast").filter({ hasText: t("needs.actions.undone") }).waitFor();
        await page.waitForTimeout(7000);
        expect((await fakeCalls()).composio.length === 0, `Undo still sent: ${JSON.stringify((await fakeCalls()).composio)}`);

        // Approve again and let the hold run out.
        await page.locator(`.needs-list [data-action="${target.id}"]`).click();
        await page.getByRole("button", { name: fmt("needs.actions.approveLabel", { summary: edited.summary }), exact: true }).click();
        await until("the action executes", async () => {
          const a = (await shell("cloud_action_status", { id: target.id })).ok;
          return [a?.status === "succeeded", a?.status];
        }, 30_000);
        const calls = (await fakeCalls()).composio;
        expect(calls.length === 1, `Composio executes: ${JSON.stringify(calls)}`);
        expect(calls[0].tool === "GMAIL_REPLY_TO_THREAD", `tool ${calls[0].tool}`);
        const sent = JSON.stringify(calls[0].arguments);
        expect(sent.includes("Edited in the e2e run."), `the executed body is not the edited one: ${sent}`);

        // A second approve with the old digest is refused, and nothing more runs.
        const again = await shell("cloud_action_approve", { id: target.id, digest: oldDigest, idempotencyKey: `e2e-${Date.now()}` });
        expect(again.err, `the stale digest was accepted: ${JSON.stringify(again)}`);
        summary.staleDigest = again.err.cloud ?? again.err.message;
        expect((await fakeCalls()).composio.length === 1, "the stale approve ran the tool again");
      } catch (e) {
        await fail("approve")(e);
      }
    });

    // ---- 5. snooze one, reject another ----
    await check("cloud: snooze one action (H), then reject another (Backspace)", async () => {
      try {
        const left = (await shell("cloud_actions_list", { status: "pending" })).ok;
        expect(left.length >= 2, `pending: ${JSON.stringify(left)}`);
        const [a, b] = left;
        await page.locator(`.needs-list [data-action="${a.id}"]`).click();
        await page.keyboard.press("h");
        await page.getByRole("group", { name: t("today.snoozeUntil") }).getByRole("button").first().click();
        await until("the snoozed action leaves the queue", async () => [(await page.locator(`.needs-list [data-action="${a.id}"]`).count()) === 0, null]);
        const snoozed = (await shell("cloud_actions_list", { status: "pending", includeSnoozed: true })).ok.find((x) => x.id === a.id);
        expect(snoozed?.snoozed_until, `not snoozed on the server: ${JSON.stringify(snoozed)}`);

        await page.locator(`.needs-list [data-action="${b.id}"]`).click();
        await page.keyboard.press("Backspace");
        await until("the rejected action is cancelled", async () => {
          const s = (await shell("cloud_action_status", { id: b.id })).ok;
          return [s?.status === "cancelled" || s?.status === "rejected", s?.status];
        });
        expect((await fakeCalls()).composio.length === 1, "snoozing or rejecting ran a tool");
      } catch (e) {
        await fail("snooze-reject")(e);
      }
    });

    // ---- 6. quick ask and ghost text through /llm/v1 ----
    await check("cloud: a quick ask answers with citations through /llm/v1", async () => {
      const qa = await ctx.newPage();
      try {
        await rpc(daemon.socket, "sources.add", { id: null, kind: "files", name: "Notes", settings: { roots: [notes], include: [], exclude: [] } });
        await until("the note is indexed and found by search", async () => {
          const hits = await rpc(daemon.socket, "search", { query: "Atlas room", limit: 5 });
          const list = Array.isArray(hits) ? hits : (hits.hits ?? hits.results ?? []);
          return [list.length > 0, list.length];
        }, 60_000);
        bridge.deliverEvents(qa);
        await qa.goto(`${bridge.url}quickask.html`);
        const box = qa.getByRole("searchbox", { name: t("quickAsk.inputLabel") });
        await box.fill("Where is the Harbourline kickoff?");
        await box.press("Enter");
        await qa.locator(".quickask-answer:not([aria-busy=true])").waitFor({ timeout: 30_000 });
        const text = (await qa.locator(".quickask-text").textContent()) ?? "";
        expect(text.includes("Atlas room"), `answer: ${text}`);
        await qa.locator(".quickask-answer .cite, .quickask-answer [data-cite]").first().waitFor();
        // Metered once the stream has ended.
        await until("the ask is metered on /llm/v1 (heed-smart)", async () => {
          const n = Number(psql(`select count(*) from public.usage_events u join auth.users a on a.id = u.user_id where a.email = '${email}' and u.kind = 'llm' and u.route like 'heed-smart:%';`));
          return [n >= 1, n];
        });
      } catch (e) {
        await shot(qa, "FAIL-cloud-quickask");
        throw e;
      } finally {
        await qa.close();
      }
    });

    await check("cloud: one ghost-text completion through the cloud fast tier", async () => {
      const req = {
        context: {
          app: { id: "org.nugget.e2e.mail", name: "E2E Mail", window_title: "Re: Signed contract" },
          field: { role: "text_area", label: "Message body", value: "Hi Dana, the contract is signed" },
          thread: [],
          recipients: ["dana@harbourline.example"],
          subject: "Re: Signed contract",
        },
        mode: "continue",
        instruction: null,
        provider: null,
        max_tokens: null,
      };
      const { result, notes: ns } = await rpcStream(daemon.socket, "draft.begin", req, (r, all) => all.some((n) => n.method === "chat.done" && n.params?.stream_id === r.stream_id), 60_000);
      const mine = ns.filter((n) => n.params?.stream_id === result.stream_id);
      const done = mine.find((n) => n.method === "chat.done").params;
      const text = mine.filter((n) => n.method === "chat.delta").map((n) => n.params.text).join("");
      expect(!done.error, `ghost text ended with an error: ${JSON.stringify(done.error)}`);
      expect(/cloud/.test(result.provider ?? ""), `ghost text came from ${result.provider}`);
      expect(text.includes("send it over tomorrow"), `ghost text: ${JSON.stringify(text)}`);
    });

    // ---- 7. plan, trial and usage; the portal; or the self-host state ----
    await check(BILLING ? "cloud: Account shows plan, trial and usage; the portal opens fake Stripe" : "cloud: billing off, Account shows the self-hosted plan", async () => {
      try {
        await go("account");
        bridge.cloudEvent("nugget:cloud-billing");
        if (BILLING) {
          const strip = page.locator(".plan-strip");
          await strip.getByText(t("cloud.billing.status.trialing"), { exact: true }).waitFor();
          await strip.getByText(t("cloud.billing.strip.trialEnds"), { exact: true }).waitFor();
          await page.locator(".usage").getByText(tPattern("cloud.billing.usage.count")).waitFor();
          const used = (await page.locator(".usage-numbers").textContent()) ?? "";
          expect(/^[1-9]\d* /.test(used.trim()), `smart asks used: ${used}`);
          const before = bridge.opened.length;
          await page.getByRole("button", { name: exactName("cloud.billing.manage") }).click();
          await page.getByRole("status").filter({ hasText: t("cloud.billing.inBrowser") }).waitFor();
          const portal = bridge.opened.slice(before).find((u) => u.startsWith(`${FAKE}/p/`));
          expect(portal, `the portal opened ${JSON.stringify(bridge.opened.slice(before))}`);
          const { stripe } = await fakeCalls();
          const form = stripe.find((c) => c.path === "/v1/billing_portal/sessions")?.form ?? {};
          expect(form.return_url === `${CLOUD_URL}/billing/done`, `portal return_url: ${JSON.stringify(form)}`);
        } else {
          await page.getByText(t("cloud.billing.selfHostedTitle"), { exact: true }).waitFor();
          await page.getByRole("status").filter({ hasText: tPattern("cloud.billing.selfHosted") }).waitFor();
          expect((await page.locator(".cloud-plans").count()) === 0, "billing off still offers plans");
          const plans = await shell("cloud_billing_checkout", { plan: "heed", interval: "month" });
          expect(plans.err, "Checkout works with billing off");
          expect((await fakeCalls()).stripe.length === 0, "billing off still called Stripe");
        }
        await shot(page, `cloud-account-${BILLING ? "billing" : "selfhost"}`);
      } catch (e) {
        await fail("account")(e);
      }
    });

    // ---- 8. delete the account; nothing of the user's is left ----
    await check("cloud: deleting the account by typing the email removes every row of the user", async () => {
      try {
        await getJson(`${CLOUD_URL}/v1/waitlist`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ email }) });
        const uid = psql(`select id from auth.users where email = '${email}';`);
        expect(/^[0-9a-f-]{36}$/.test(uid), `no auth user for ${email}: ${uid}`);
        expect(Number(psql(`select count(*) from public.waitlist where lower(email) = lower('${email}');`)) === 1, "no waitlist row to delete");
        const tables = psql(
          `select c.relname from pg_class c join pg_namespace n on n.oid = c.relnamespace
             join pg_attribute a on a.attrelid = c.oid and a.attname = 'user_id' and not a.attisdropped
            where n.nspname = 'public' and c.relkind = 'r' and c.relrowsecurity order by 1;`,
        ).split("\n").filter(Boolean);
        expect(tables.length > 10, `few RLS tables with user_id: ${tables}`);
        const count = (tbls) => Object.fromEntries(tbls.map((x) => [x, Number(psql(`select count(*) from public.${x} where user_id = '${uid}';`))]));
        const before = count(tables);
        summary.rowsBefore = Object.fromEntries(Object.entries(before).filter(([, n]) => n > 0));
        expect((before.items ?? 0) > 0 && (before.pending_actions ?? 0) > 0 && (before.cards ?? 0) > 0, `the user has no items or actions to delete: ${JSON.stringify(summary.rowsBefore)}`);

        await go("account");
        await page.getByRole("button", { name: exactName("account.delete.button") }).click();
        const confirm = page.getByRole("button", { name: exactName("account.delete.confirm") });
        expect(await confirm.isDisabled(), "Delete is enabled before the email is typed");
        await page.getByLabel(fmt("account.delete.typeEmail", { email }), { exact: true }).fill(email.toUpperCase());
        await confirm.click();
        await page.getByLabel(t("cloud.signIn.email"), { exact: true }).or(page.getByText(t("account.signedOut.title"), { exact: true })).first().waitFor({ timeout: 30_000 });

        const after = count(tables);
        const left = Object.entries(after).filter(([, n]) => n > 0);
        expect(left.length === 0, `rows left after deleting the account: ${JSON.stringify(left)}`);
        expect(psql(`select count(*) from auth.users where id = '${uid}';`) === "0", "the auth user is still there");
        expect(Number(psql(`select count(*) from public.waitlist where lower(email) = lower('${email}');`)) === 0, "the waitlist row is still there");
        const status = await shell("cloud_model_status");
        expect(status.ok?.active === false, `nuggetd still has the cloud model after deletion: ${JSON.stringify(status)}`);
        if (BILLING) {
          const { stripe } = await fakeCalls();
          expect(stripe.some((c) => c.method === "DELETE" && c.path.startsWith("/v1/customers/")), "the Stripe customer was not deleted");
        }
      } catch (e) {
        await fail("delete")(e);
      }
    });

    await checkWire(bridge, "cloud");
    await check("cloud: no uncaught page errors", async () => expect(pageErrors.length === 0, `uncaught page errors: ${JSON.stringify(pageErrors)}`));
    summary.rpcErrors = bridge.calls.filter((c) => !c.ok);
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    await stopDaemon(daemon, "cloud");
    fs.rmSync(notes, { recursive: true, force: true });
  }
  return summary;
}
