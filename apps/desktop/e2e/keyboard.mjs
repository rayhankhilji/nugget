// Keyboard-only scenario: the core flows with `page.keyboard` alone (Tab, Shift+Tab, arrows,
// Enter, Space, Escape, typing and the palette shortcut). Locators are only read, to find out
// where focus is and what the page shows; nothing is clicked, filled or focused by the test.
//
// Every Tab (and every arrow that moves focus) is checked: the focused element must show a focus
// indicator (an outline, or a box-shadow ring that it or a close ancestor only has while focused),
// focus must never be lost to <body> while a page is up, and every dialog keeps focus inside,
// closes on Escape and returns focus to the control that opened it.
//
// Setup, outside the keyboard flows: `nuggetd-e2e` with the ai-setup scenario's signed test
// manifest and fake llama-server, the recommended model installed through `models.install` before
// the window opens (onboarding then takes "Decide later"), so Chat answers with citations. The
// Today card comes from the corpus itself (an unanswered mail thread); the test asks the daemon
// to compute cards with `proactive.refresh` instead of waiting for the scheduled refresh.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { startBridge, METHOD_NOT_FOUND } from "./bridge.mjs";
import { CORPUS, locateE2eBinaries, rpc, rpcStream, startDaemon } from "./daemon.mjs";
import { MODEL_ID, signedTestManifest, startModelServer } from "./ai-setup.mjs";
import { check, CheckError, DIST, expect, INDEX_TIMEOUT_MS, KEEP, log, newContext, OUT, settle, shot, t, tPattern, until } from "./harness.mjs";

/** The daemon config: the local runtime on (for Chat), the corpus owner set (for the Today card). */
const KEYBOARD_CONFIG = `# Written by apps/desktop/e2e/keyboard.mjs.
[runtime]
enabled = true
idle_stop_minutes = 0

[knowledge.me]
name = "Robin Ashgrove"
emails = ["robin.ashgrove@glimmerfen.test", "robin@ashgrove.test"]

[mcp]
http = false
`;

/** Same question and corpus facts as the first run (notes/projects/Tidewater Billing.md). */
const QUERY = "Tidewater Billing vendor contract";
const EXPECT_TITLE = "Tidewater Billing";

/** The app's shortcut modifier: ⌘ on macOS, Ctrl elsewhere (`hasMod` in src/hooks.shortcuts.ts). */
const MOD = process.platform === "darwin" ? "Meta" : "Control";

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactName = (key) => new RegExp(`^${escapeRe(t(key))}$`);
const startsWith = (key) => new RegExp(`^${escapeRe(t(key))}`);

// ---- focus probes (run in the page) ----

/**
 * What has focus: a stable id per element (a WeakMap in the page, the DOM is not touched), a short
 * description, whether it is inside an open dialog, and whether a focus indicator is visible.
 */
export function probeFocus() {
  const reg = (window.__kbdIds ??= { map: new WeakMap(), next: 1 });
  const el = document.activeElement;
  if (!el || el === document.body || el === document.documentElement) {
    return { id: 0, desc: "<body>", body: true, ring: false, ringBy: null, dialog: false };
  }
  if (!reg.map.has(el)) reg.map.set(el, reg.next++);
  const tag = el.tagName.toLowerCase();
  const role = el.getAttribute("role") || (tag === "input" ? `input[${el.type}]` : tag);
  const name = (el.getAttribute("aria-label") || el.labels?.[0]?.textContent || el.textContent || el.getAttribute("placeholder") || el.id || "")
    .trim()
    .replace(/\s+/g, " ")
    .slice(0, 60);

  // A ring the node has only while focus is on (or in) it: compare with a bare, unfocused copy
  // (name and id dropped so a radio copy can't uncheck its group), inserted and removed at once.
  const ringOf = (node) => {
    const cs = getComputedStyle(node);
    const outline = cs.outlineStyle !== "none" && parseFloat(cs.outlineWidth) > 0;
    if (!outline && cs.boxShadow === "none") return null;
    const twin = node.cloneNode(false);
    for (const a of ["id", "name", "autofocus", "data-autofocus"]) twin.removeAttribute(a);
    node.after(twin);
    const bare = getComputedStyle(twin);
    const plain = { outline: bare.outlineStyle !== "none" && parseFloat(bare.outlineWidth) > 0 ? `${bare.outlineStyle} ${bare.outlineWidth} ${bare.outlineColor}` : "none", shadow: bare.boxShadow };
    twin.remove();
    if (outline && plain.outline !== `${cs.outlineStyle} ${cs.outlineWidth} ${cs.outlineColor}`) return `outline ${cs.outlineWidth} ${cs.outlineStyle}`;
    if (cs.boxShadow !== "none" && cs.boxShadow !== plain.shadow) return "box-shadow";
    return null;
  };
  // A control drawn by something else shows focus there: a visually hidden radio on its card (an
  // ancestor, a few levels up at most, not past the dialog or the page), a switch's checkbox on
  // the track drawn right after it.
  const shown = (node) => {
    const r = node.getBoundingClientRect();
    const cs = getComputedStyle(node);
    return r.width >= 4 && r.height >= 4 && cs.opacity !== "0" && cs.visibility !== "hidden" && cs.clipPath === "none" && cs.clip === "auto";
  };
  let ringBy = null;
  if (shown(el)) {
    const r = ringOf(el);
    if (r) ringBy = `itself (${r})`;
  }
  const stand = el.nextElementSibling;
  if (!ringBy && stand && shown(stand)) {
    const r = ringOf(stand);
    if (r) ringBy = `${stand.tagName.toLowerCase()}.${[...stand.classList].join(".")} (${r})`;
  }
  for (let n = el.parentElement, depth = 1; !ringBy && n && depth <= 4 && !n.matches("dialog, main, body"); n = n.parentElement, depth++) {
    if (!shown(n)) continue;
    const r = ringOf(n);
    if (r) ringBy = `${n.tagName.toLowerCase()}.${[...n.classList].join(".")} (${r})`;
  }
  return { id: reg.map.get(el), desc: `${role} "${name}"`, body: false, ring: ringBy !== null, ringBy, dialog: !!el.closest("dialog[open]") };
}

const focusOf = (page) => page.evaluate(probeFocus);

/** Fail unless the focused element shows a focus indicator. */
function expectRing(f, how) {
  expect(f.body || f.ring, `after ${how}, ${f.desc} has focus but no visible focus indicator (no outline or focus ring on it or its card)`);
}

/** Press `key` (Tab, Shift+Tab, an arrow) and check the element it lands on. */
async function step(page, key) {
  await page.keyboard.press(key);
  const f = await focusOf(page);
  expectRing(f, key);
  return f;
}

/** True when `locator` (its first match) has focus. */
const hasFocus = (locator) => locator.first().evaluate((el) => el === document.activeElement || el.contains(document.activeElement));

/**
 * Press Tab (or Shift+Tab with `back`) until `locator` has focus, checking the focus ring on every
 * stop. Fails after `max` presses, or as soon as focus comes back to where it started without
 * reaching the target, with the elements it went through.
 */
async function tabTo(page, locator, { max = 60, back = false, what } = {}) {
  await locator.first().waitFor({ state: "attached" });
  const seen = [];
  const ids = new Set();
  const start = await focusOf(page);
  if (start.id) ids.add(start.id);
  for (let i = 0; i < max; i++) {
    if (await hasFocus(locator)) return seen;
    const f = await step(page, back ? "Shift+Tab" : "Tab");
    seen.push(f.desc);
    if (f.id && ids.has(f.id) && f.id === start.id) {
      throw new CheckError(`${what ?? "the target"} is not reachable with ${back ? "Shift+Tab" : "Tab"}: focus went round ${seen.length} stops: ${seen.join(" → ")}`);
    }
    if (f.id) ids.add(f.id);
  }
  if (await hasFocus(locator)) return seen;
  throw new CheckError(`${what ?? "the target"} not focused after ${max} ${back ? "Shift+Tab" : "Tab"} presses; went through: ${seen.join(" → ")}`);
}

/**
 * Tab round the whole window from where focus is and check it is not trapped: outside a dialog
 * the cycle must take at least `min` stops before an element repeats.
 */
async function expectNoTrap(page, { min, max = 250 }) {
  const order = [];
  const ids = new Set();
  for (let i = 0; i < max; i++) {
    const f = await step(page, "Tab");
    if (f.dialog) throw new CheckError(`Tab moved into a dialog that was not open before: ${f.desc}`);
    if (f.id && ids.has(f.id)) {
      expect(order.length >= min, `focus cycles through only ${order.length} stops (want at least ${min}): ${order.join(" → ")}`);
      return order.length;
    }
    if (f.id) ids.add(f.id);
    order.push(f.desc);
  }
  return order.length; // longer than max without repeating: certainly not trapped
}

/** In an open modal, `n` Tabs and `n` Shift+Tabs keep focus inside it. */
async function expectFocusContained(page, n = 12) {
  for (const key of ["Tab", "Shift+Tab"]) {
    for (let i = 0; i < n; i++) {
      const f = await step(page, key);
      expect(f.dialog, `${key} took focus out of the open dialog to ${f.desc}`);
    }
  }
}

/** Remember the focused element as a dialog's opener. Returns its description. */
async function markOpener(page) {
  const f = await focusOf(page);
  await page.evaluate(() => (window.__kbdOpener = document.activeElement));
  return f;
}

/** Wait for a dialog to open with focus inside it (and a visible indicator on it). */
async function expectDialogFocused(page, what) {
  await page.locator("dialog[open]").first().waitFor();
  const f = await until(`${what}: focus moves into the dialog`, async () => {
    const f = await focusOf(page);
    return [f.dialog, f];
  });
  expectRing(f, `opening ${what}`);
  return f;
}

/** Escape closes the open dialog and focus goes back to the element `markOpener` saved. */
async function escapeReturnsFocus(page, what) {
  await page.keyboard.press("Escape");
  await until(`Escape closes ${what}`, async () => [(await page.locator("dialog[open]").count()) === 0, "a dialog is still open"]);
  await until(`closing ${what} returns focus to the control that opened it`, async () => {
    const back = await page.evaluate(() => document.activeElement === window.__kbdOpener);
    return [back, (await focusOf(page)).desc];
  });
}

/** Focus is on something (not lost to <body>) and shows it. */
async function expectFocusKept(page, what) {
  const f = await until(`${what}: focus stays on an element`, async () => {
    const f = await focusOf(page);
    return [!f.body, f];
  });
  return f;
}

/** A shell page's landmark (`<main aria-label>` is the page's nav label). */
const pageMain = (page, key) => page.locator(`main[aria-label="${t(key)}"]`);

/** Settle animations, then screenshot. */
async function snap(page, name) {
  await page
    .evaluate(() => {
      const finite = document.getAnimations().filter((a) => a.effect?.getComputedTiming().iterations !== Infinity);
      return Promise.race([Promise.all(finite.map((a) => a.finished.catch(() => {}))), new Promise((r) => setTimeout(r, 2000))]);
    })
    .catch(() => {});
  await shot(page, name);
}

// ---- the scenario ----

/**
 * Run the scenario with `browser` (Playwright). Starts its own daemon (`nuggetd-e2e`; `bin`, the
 * plain daemon, is not used: Chat needs the fake local model). Returns the summary for
 * summary.json (`keyboard`).
 */
export async function keyboardScenario({ browser, bin }) {
  const bins = locateE2eBinaries(log);
  const fixtures = fs.mkdtempSync(path.join(os.tmpdir(), "nugget-e2e-kbd-"));
  // A second, small folder for the Sources page's Add source.
  const extra = path.join(fixtures, "kbd-notes");
  fs.mkdirSync(extra);
  fs.writeFileSync(path.join(extra, "Keyboard walk.md"), "# Keyboard walk\n\nAdded from the Sources page without a mouse.\n");

  const files = {};
  // Fast: the download is setup here, not what is being tested.
  const served = await startModelServer(files, 64 * 1024 * 1024);
  const manifest = signedTestManifest(fixtures, served.port);
  Object.assign(files, manifest.files);

  const summary = { model: MODEL_ID, flows: {}, opened: [] };
  let daemon;
  try {
    daemon = await startDaemon(bins.nuggetd, {
      stderrPath: path.join(OUT, "keyboard-nuggetd.out.log"),
      config: KEYBOARD_CONFIG,
      env: { ...manifest.env, NUGGET_E2E_LLAMA_SERVER: bins.server, NUGGET_E2E_RAM_GB: "16", NUGGET_E2E_GPU: "unknown" },
    });
    // The install runs while the requesting connection is open: keep it open until it is done.
    const { notes } = await rpcStream(
      daemon.socket,
      "models.install",
      { id: MODEL_ID },
      (_, ns) => ns.some((n) => n.method === "models.progress" && ["installed", "failed"].includes(n.params?.phase)),
      60_000,
    );
    const last = notes.filter((n) => n.method === "models.progress").at(-1)?.params;
    if (last?.phase !== "installed") throw new Error(`installing ${MODEL_ID} failed: ${JSON.stringify(last)}`);
  } catch (e) {
    await daemon?.stop({ keep: true }).catch(() => {});
    await served.close().catch(() => {});
    fs.rmSync(fixtures, { recursive: true, force: true });
    throw e;
  }
  log(`keyboard: daemon home ${daemon.home}`);

  let bridge;
  let ctx;
  const pageErrors = [];
  // What the native folder picker "chooses" next (onboarding: the corpus; Sources: the small folder).
  let nextPick = [CORPUS];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST, pickFolders: () => nextPick });
    ctx = await newContext(browser);
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    bridge.deliverEvents(page);
    // The shell's `open_url` hands a file or link to the OS; record it and answer like the shell.
    await page.route("**/invoke", async (route) => {
      const body = route.request().postDataJSON();
      if (body?.cmd !== "open_url") return route.fallback();
      summary.opened.push(body.args?.url ?? null);
      return route.fulfill({ json: { ok: "opened" } });
    });

    const heading = (key) => page.getByRole("heading", { level: 1, name: t(key), exact: true });
    const footer = page.locator("footer.ob-actions");
    const continueBtn = footer.getByRole("button", { name: exactName("onboarding.continue") });
    /** Onboarding: Tab to Continue, Enter, and the next step's heading takes focus. */
    const next = async (headingKey) => {
      await tabTo(page, continueBtn, { what: "Continue" });
      await page.keyboard.press("Enter");
      await heading(headingKey).waitFor();
      await until(`the "${t(headingKey)}" step's heading takes focus`, async () => [await hasFocus(heading(headingKey)), (await focusOf(page)).desc]);
    };
    const flow = async (name, fn) => {
      const t0 = Date.now();
      await check(`keyboard: ${name}`, async () => {
        try {
          await fn();
          summary.flows[name] = { ok: true, ms: Date.now() - t0 };
        } catch (e) {
          summary.flows[name] = { ok: false, ms: Date.now() - t0 };
          await shot(page, `FAIL-keyboard-${name.replace(/\W+/g, "-")}`);
          throw e;
        }
      });
    };

    await flow("onboarding from an empty home to the source step", async () => {
      await page.goto(`${bridge.url}index.html`);
      await heading("onboarding.welcome.heading").waitFor();
      await next("onboarding.ai.heading");
      // A radio group is one Tab stop; arrows choose inside it.
      const radios = page.locator(".ob-fieldset input[type=radio]");
      const later = page.getByRole("radio", { name: startsWith("onboarding.ai.later") });
      await tabTo(page, radios, { what: "the AI choices" });
      for (let i = 0; i < 4 && !(await later.isChecked()); i++) await step(page, "ArrowDown");
      expect(await later.isChecked(), `arrow keys did not reach "${t("onboarding.ai.later")}"`);
      expect(await hasFocus(later), "the checked AI choice does not have focus");
      await snap(page, "keyboard-onboarding-ai");
      await next("onboarding.source.heading");
    });

    await flow("add a folder source in onboarding and wait for indexing", async () => {
      const addFolder = page.getByRole("button", { name: exactName("onboarding.source.addFolder") });
      await tabTo(page, addFolder, { what: t("onboarding.source.addFolder") });
      await markOpener(page);
      await page.keyboard.press("Enter");
      await expectDialogFocused(page, "the Add folder dialog");
      await expectFocusContained(page);
      await escapeReturnsFocus(page, "the Add folder dialog");

      await page.keyboard.press("Enter");
      const dialog = page.locator("dialog[open]");
      await expectDialogFocused(page, "the Add folder dialog");
      await tabTo(page, dialog.getByRole("button", { name: exactName("addSource.folder.choose") }), { what: t("addSource.folder.choose") });
      await page.keyboard.press("Enter");
      await until("the picked folder is listed in the wizard", async () => {
        const paths = await dialog.locator(".path-list code").allTextContents();
        return [paths.includes(CORPUS), paths];
      });
      await snap(page, "keyboard-folder");
      await tabTo(page, dialog.getByRole("button", { name: exactName("addSource.folder.submit") }), { what: t("addSource.folder.submit") });
      await page.keyboard.press("Enter");
      await dialog.waitFor({ state: "detached" });
      await until("adding the folder returns focus to the button that opened the wizard", async () => [
        await page.evaluate(() => document.activeElement === window.__kbdOpener),
        (await focusOf(page)).desc,
      ]);
      const readyRe = tPattern("onboarding.source.ready");
      await until(
        "the source step says the folder is indexed and search works",
        async () => {
          const row = await page.locator(".ob-index li").first().evaluate((li) => ({ phase: li.dataset.phase, text: li.textContent.trim() }));
          if (row.phase === "failed") throw new Error(`indexing failed: ${row.text}`);
          return [row.phase === "ready" && readyRe.test(row.text), row];
        },
        INDEX_TIMEOUT_MS,
      );
      await snap(page, "keyboard-indexed");
    });

    await flow("onboarding through Permissions and Double Tap to Done", async () => {
      await next("onboarding.perm.heading");
      await next("onboarding.trigger.heading");
      await next("onboarding.done.heading");
      await snap(page, "keyboard-done");
      const toSearch = footer.getByRole("button", { name: exactName("onboarding.done.search") });
      await tabTo(page, toSearch, { what: t("onboarding.done.search") });
      await page.keyboard.press("Enter");
      await pageMain(page, "app.nav.search").waitFor();
    });

    await flow("search and open a result", async () => {
      const box = page.getByRole("searchbox", { name: t("search.query") });
      await until("Search opens with focus in its box", async () => [await hasFocus(box), (await focusOf(page)).desc]);
      await page.keyboard.type(QUERY);
      await page.keyboard.press("Enter");
      const hits = page.locator("ol.results li.result");
      const hit = hits.filter({ has: page.locator(".result-title", { hasText: EXPECT_TITLE }) }).first();
      await until(`search "${QUERY}" lists "${EXPECT_TITLE}"`, async () => [(await hit.count()) > 0, await hits.locator(".result-title").allTextContents()]);
      const title = (await hit.locator(".result-title").textContent()).trim();
      const open = hit.getByRole("button", { name: t("search.openTitle").replace("{title}", title), exact: true });
      await tabTo(page, open, { what: `Open on "${title}"` });
      const before = summary.opened.length;
      await page.keyboard.press("Enter");
      const url = await until("Enter on Open hands the result to the OS (open_url)", async () => [summary.opened.length > before, summary.opened.at(-1)]);
      expect(url.startsWith("file:///") && decodeURIComponent(url).includes(EXPECT_TITLE), `opened ${url}, not the "${EXPECT_TITLE}" file`);
      const opened = t("hooks.open.opened").replace("{label}", title);
      await until("the page says the result was opened", async () => {
        const live = await page.locator("main [aria-live=polite]").allTextContents();
        return [live.some((s) => s.includes(opened)), live];
      });
      await expectFocusKept(page, "after opening a result");
      summary.flows.search_opened = url;
      await snap(page, "keyboard-search");
      // Tab all the way round: nothing on the page or in the sidebar traps focus.
      summary.search_tab_stops = await expectNoTrap(page, { min: 20 });
    });

    await flow("ask in Chat and follow a citation", async () => {
      await page.keyboard.press(`${MOD}+2`);
      await pageMain(page, "app.nav.chat").waitFor();
      const sources = page.getByRole("checkbox", { name: startsWith("chat.useSources") });
      if ((await sources.count()) > 0 && !(await sources.isChecked())) {
        await tabTo(page, sources, { what: t("chat.useSources") });
        await page.keyboard.press("Space");
        expect(await sources.isChecked(), `Space did not check "${t("chat.useSources")}"`);
      }
      const input = page.getByRole("textbox", { name: t("chat.message") });
      await tabTo(page, input, { what: t("chat.message") });
      await page.keyboard.type(QUERY);
      await page.keyboard.press("Enter");
      const reply = page.locator("li.msg-assistant").last();
      await until(
        "the reply finishes with [n] citation buttons",
        async () => {
          const r = await reply.evaluate((li) => ({
            streaming: !!li.querySelector(".caret"),
            marks: li.querySelectorAll(".msg-body button.cite").length,
            error: li.querySelector("[role=alert]")?.textContent?.trim() ?? null,
          }));
          if (r.error) throw new Error(`the reply failed: ${r.error}`);
          return [!r.streaming && r.marks > 0, r];
        },
        60_000,
      );
      const marker = reply.locator(".msg-body button.cite").first();
      const n = Number((await marker.textContent()).replace(/\D/g, ""));
      const source = reply.locator(`ol.chat-citations li.citation[data-cite-ns~="${n}"]`);
      await source.waitFor();
      const sourceTitle = (await source.locator(".citation-title").textContent()).trim();
      await tabTo(page, marker, { what: `citation [${n}]`, back: true });
      const before = summary.opened.length;
      await page.keyboard.press("Enter");
      // A source the shell can open opens; any other one takes focus in the list under the reply.
      const followed = await until(`Enter on [${n}] follows the citation to "${sourceTitle}"`, async () => {
        if (summary.opened.length > before) return [true, { opened: summary.opened.at(-1) }];
        return [await hasFocus(source), (await focusOf(page)).desc];
      });
      expect(await source.isVisible(), `the cited source "${sourceTitle}" is not shown under the reply`);
      summary.flows.citation = { n, title: sourceTitle, ...(typeof followed === "object" ? followed : { focused: followed }) };
      await expectFocusKept(page, "after following a citation");
      await snap(page, "keyboard-chat");
    });

    await flow("accept a Today card's action through its confirmation", async () => {
      // The corpus has an unanswered mail thread; compute cards now rather than on the schedule.
      await rpc(daemon.socket, "proactive.refresh", {});
      const { cards } = await rpc(daemon.socket, "cards.list", {});
      const card = cards.find((c) => c.actions.some((a) => a.supported));
      expect(card, `no Today card with a supported action after proactive.refresh: ${JSON.stringify(cards.map((c) => c.title))}`);
      const action = card.actions.find((a) => a.supported);
      summary.flows.today_card = { title: card.title, action: action.label };
      // Drafting a reply is allowed by default; a Safety rule makes the policy ask first, so the
      // flow goes through the dialog's confirmation as well as its Approve.
      const policy = await rpc(daemon.socket, "policy.get");
      const rule = { id: "e2e-keyboard-confirm-drafts", targets: ["mail"], verbs: ["save_draft"], effect: "confirm" };
      await rpc(daemon.socket, "policy.set", { config: { ...policy.config, rules: [...(policy.config.rules ?? []), rule] } });

      await page.keyboard.press(`${MOD}+1`);
      await pageMain(page, "app.nav.today").waitFor();
      // Known action kinds show a short verb ("Draft reply"); the daemon's full label is the tooltip.
      let actionName = action.label;
      try {
        actionName = t(`today.actionKind.${action.kind}`);
      } catch {
        /* an unknown kind keeps the daemon's label */
      }
      // Within this card's actions group: two cards can offer the same kind of action.
      const actionBtn = page
        .locator("main")
        .getByRole("group", { name: t("today.actionsLabel").replace("{title}", card.title), exact: true })
        .getByRole("button", { name: actionName, exact: true });
      await tabTo(page, actionBtn, { what: `the card action "${actionName}"`, max: 80 });
      await markOpener(page);
      await page.keyboard.press("Enter");
      await expectDialogFocused(page, "the card action dialog");
      await escapeReturnsFocus(page, "the card action dialog");

      await page.keyboard.press("Enter");
      const dialog = page.locator("dialog[open]");
      await expectDialogFocused(page, "the card action dialog");
      const approve = dialog.getByRole("button", { name: exactName("today.action.approve") });
      await approve.waitFor();
      await tabTo(page, approve, { what: t("today.action.approve") });
      await expectFocusContained(page, 6);
      await tabTo(page, approve, { what: t("today.action.approve") });
      await page.keyboard.press("Enter");
      const confirm = dialog.getByRole("button", { name: exactName("today.action.confirm") });
      const done = dialog.getByRole("button", { name: exactName("common.done") });
      const outcome = await until("the action asks for confirmation or finishes", async () => {
        if ((await confirm.count()) > 0) return [true, "needs_confirmation"];
        if ((await done.count()) > 0) return [true, "finished"];
        const f = await focusOf(page);
        if (f.body) throw new Error("focus was lost to <body> while the action ran");
        return [false, f.desc];
      }, 60_000);
      summary.flows.today_outcome = outcome;
      expect(outcome === "needs_confirmation", `the action ran without asking for confirmation (the "${rule.id}" rule should make it ask)`);
      await until("the confirmation takes focus", async () => [await hasFocus(confirm), (await focusOf(page)).desc]);
      expectRing(await focusOf(page), "the confirmation appearing");
      await page.keyboard.press("Enter");
      await done.waitFor({ timeout: 60_000 });
      await until(`"${t("common.done")}" takes focus once the action ran`, async () => [await hasFocus(done), (await focusOf(page)).desc]);
      await expectNoErrorNote(dialog, "the card action dialog");
      await snap(page, "keyboard-today-action");
      await page.keyboard.press("Enter");
      await dialog.waitFor({ state: "detached" });
      // The inbox reloads after an executed action (the card may leave it): check once it has.
      await settle(page);
      await expectFocusKept(page, "after closing the card action dialog");
    });

    await flow("stop all agents from Safety", async () => {
      await page.keyboard.press(`${MOD}+9`);
      await pageMain(page, "app.nav.safety").waitFor();
      const stop = page.getByRole("button", { name: exactName("safety.kill.stop") });
      const resume = page.getByRole("button", { name: exactName("safety.kill.resume") });
      await tabTo(page, stop, { what: t("safety.kill.stop") });
      await page.keyboard.press("Enter");
      // Stopping is immediate (there is no confirmation: the kill switch is the safe direction).
      await until("the kill switch says agents are stopped", async () => {
        const state = await rpc(daemon.socket, "policy.get");
        return [(await resume.count()) > 0 && state.halted === true, state.halted];
      });
      await until("focus stays on the kill switch", async () => [await hasFocus(resume), (await focusOf(page)).desc]);
      expectRing(await focusOf(page), "stopping all agents");
      await snap(page, "keyboard-safety");
      await page.keyboard.press("Enter");
      await stop.waitFor();
      expect((await rpc(daemon.socket, "policy.get")).halted === false, "Resume did not release the kill switch");
      await until("focus stays on the kill switch after Resume", async () => [await hasFocus(stop), (await focusOf(page)).desc]);
      summary.safety_tab_stops = await expectNoTrap(page, { min: 20 });
    });

    await flow("command palette: open, Escape, arrows and Enter to a page", async () => {
      const opener = await markOpener(page);
      expect(!opener.body, "nothing has focus before opening the palette");
      await page.keyboard.press(`${MOD}+K`);
      const input = page.getByRole("combobox", { name: t("palette.inputLabel") });
      await until("the palette opens with focus in its input", async () => [await hasFocus(input), (await focusOf(page)).desc]);
      expectRing(await focusOf(page), `${MOD}+K`);
      await escapeReturnsFocus(page, "the command palette");

      await page.keyboard.press(`${MOD}+K`);
      await until("the palette opens again", async () => [await hasFocus(input), (await focusOf(page)).desc]);
      const target = t("app.nav.status");
      const selected = page.locator('[role=option][aria-selected="true"] .palette-label');
      let label = "";
      for (let i = 0; i < 20 && label !== target; i++) {
        await page.keyboard.press("ArrowDown");
        label = ((await selected.textContent()) ?? "").trim();
        expect(await hasFocus(input), "arrow keys moved focus out of the palette input");
      }
      expect(label === target, `ArrowDown never selected "${target}" (last: "${label}")`);
      await snap(page, "keyboard-palette");
      await page.keyboard.press("Enter");
      await pageMain(page, "app.nav.status").waitFor();
      await until("the palette closes and focus moves to the Status page", async () => [
        (await page.locator("dialog[open]").count()) === 0 && (await hasFocus(page.locator("main"))),
        (await focusOf(page)).desc,
      ]);
    });

    await flow("add a folder from Sources → Add source", async () => {
      nextPick = [extra];
      await page.keyboard.press(`${MOD}+5`);
      await pageMain(page, "app.nav.sources").waitFor();
      const add = page.locator("main").getByRole("button", { name: exactName("sources.add") }).first();
      await tabTo(page, add, { what: t("sources.add") });
      await markOpener(page);
      await page.keyboard.press("Enter");
      const dialog = page.locator("dialog[open]");
      await expectDialogFocused(page, "the Add source wizard");
      await escapeReturnsFocus(page, "the Add source wizard");
      await page.keyboard.press("Enter");
      await expectDialogFocused(page, "the Add source wizard");
      // The kind picker starts on "Folder"; Next.
      await tabTo(page, dialog.getByRole("button", { name: exactName("common.next") }), { what: t("common.next") });
      await page.keyboard.press("Enter");
      const choose = dialog.getByRole("button", { name: exactName("addSource.folder.choose") });
      await choose.waitFor();
      await until("the folder form takes focus", async () => [(await focusOf(page)).dialog && !(await focusOf(page)).body, (await focusOf(page)).desc]);
      await tabTo(page, choose, { what: t("addSource.folder.choose") });
      await page.keyboard.press("Enter");
      await until("the picked folder is listed in the wizard", async () => {
        const paths = await dialog.locator(".path-list code").allTextContents();
        return [paths.includes(extra), paths];
      });
      await tabTo(page, dialog.getByRole("button", { name: exactName("addSource.folder.submit") }), { what: t("addSource.folder.submit") });
      await page.keyboard.press("Enter");
      await dialog.waitFor({ state: "detached" });
      await expectFocusKept(page, "after adding a source");
      await until(
        "the new folder source is indexed",
        async () => {
          const list = await rpc(daemon.socket, "sources.list");
          const s = list.find((x) => (x.settings?.roots ?? []).includes(extra));
          return [s?.items > 0, s ? { id: s.id, items: s.items } : list.map((x) => x.id)];
        },
        INDEX_TIMEOUT_MS,
      );
      await settle(page);
      await snap(page, "keyboard-sources");
    });

    await check("keyboard: no uncaught page errors and every daemon command exists", async () => {
      const bad = bridge.calls.filter((c) => c.via === "daemon" && (c.code === METHOD_NOT_FOUND || c.code === -32602));
      expect(bad.length === 0, `wire mismatches: ${JSON.stringify(bad)}`);
      expect(pageErrors.length === 0, `${pageErrors.length} uncaught page error(s): ${JSON.stringify(pageErrors)}`);
    });
    summary.rpcErrors = bridge.calls.filter((c) => !c.ok);
    summary.pageErrors = pageErrors;
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    await served.close().catch(() => {});
    fs.cpSync(daemon.logDir, path.join(OUT, "keyboard-daemon-logs"), { recursive: true });
    await daemon.stop({ keep: KEEP });
    if (KEEP) log(`kept the keyboard daemon home: ${daemon.home}`);
    fs.rmSync(fixtures, { recursive: true, force: true });
  }
  return summary;
}

/** No error note inside `scope` (a locator). */
async function expectNoErrorNote(scope, what) {
  const notes = await scope.locator(".note-error").allTextContents();
  expect(notes.length === 0, `${what} shows an error: ${JSON.stringify(notes)}`);
}
