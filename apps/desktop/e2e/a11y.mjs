// Accessibility audit: axe-core over every page, their main states and dialogs, onboarding, quick
// ask and the Double Tap overlay, in light and dark. Fails on serious or critical violations (all
// are written to a11y-violations.json); a11y-tree.mjs checks each page's accessibility tree.
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { treeChecks } from "./a11y-tree.mjs";
import { check, DIST, exactName, expect, log, newContext, OUT, settle, startsWith, t, tPattern, until } from "./harness.mjs";

const require = createRequire(import.meta.url);
const AXE = path.join(path.dirname(require.resolve("axe-core/package.json")), "axe.min.js");

const THEMES = ["light", "dark"];
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "best-practice"];
/** Impacts that fail a check; the others are recorded only. */
const FAILING = new Set(["serious", "critical"]);

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/**
 * Pages audited again in further states, in order on the same page: `[name, steps, scope?]`
 * (results, an open entity, the graph, a dialog the page opens).
 */
const STATES = {
  search: [
    [
      "results",
      async (page) => {
        const box = page.getByRole("searchbox", { name: t("search.query") });
        await box.fill("Tidewater budget");
        await box.press("Enter");
        await page.locator("ol.results li.result").first().waitFor();
        // the filters (II2) show beside the results.
        await page.locator("#search-filters fieldset").first().waitFor();
      },
    ],
    [
      // a filter ticked (the list reruns with it, Clear filters appears).
      "filtered",
      async (page) => {
        await page.locator('#search-filters fieldset[data-facet="kind"] input[type=checkbox]').first().check();
        await page.locator("#search-filters").getByRole("button", { name: exactName("searchui.clearFilters") }).waitFor();
        await page.locator("ol.results:not([aria-busy=true]) li.result").first().waitFor();
      },
    ],
    [
      // a result's preview open beside the list (search.preview).
      "preview",
      async (page) => {
        await page.locator("ol.results li.result button.result-select").first().click();
        await page.locator("#search-preview .sr-preview-text").waitFor();
      },
    ],
    [
      // a person's timeline (search.timeline), from the filters; Escape goes back.
      "timeline",
      async (page) => {
        await page.locator("#search-filters .sr-facet-tl").first().click();
        await page.locator("section.sr-timeline h3[id^=timeline-month-]").first().waitFor();
      },
    ],
  ],
  models: [
    [
      // "Your providers" with its Add a provider form open.
      "add-provider",
      async (page) => {
        const add = page.locator("details.models-add-provider");
        await add.locator("summary").click();
        await add.locator("input, select").first().waitFor();
      },
    ],
  ],
  knowledge: [
    [
      "entity",
      async (page) => {
        const list = page.getByRole("region", { name: t("knowledge.entitiesLabel") });
        await list.getByRole("button", { name: /^Ada Wexford\b/ }).first().click();
        const detail = page.locator("section.entity-detail");
        await detail.getByRole("heading", { name: "Ada Wexford" }).first().waitFor();
        await detail.getByRole("button", { name: new RegExp(`^${escapeRe(t("entity.why"))}`) }).first().click();
        await detail.locator("ul.evidence > li").first().waitFor();
      },
    ],
    [
      "graph",
      async (page) => {
        await page.getByRole("tab", { name: t("knowledge.tab.graph") }).click();
        await page.locator("canvas.graph-canvas").waitFor();
      },
    ],
  ],
  apps: [
    [
      "connect-dialog",
      async (page) => {
        await page.locator("main").getByRole("button", { name: exactName("apps.connect") }).first().click();
        await page.locator("dialog[open]").waitFor();
      },
      "dialog[open]",
    ],
  ],
};

/**
 * Today's cards come from the corpus by date (the unanswered "Moorland sync" mail of 2026-10-24
 * makes an "awaiting your reply" card for a few days after it). So the audit doesn't depend on the
 * date, `cards_list` answers with one fixed card when the daemon has none; the real card is used
 * whenever there is one. Returns whether the stub was served.
 */
async function cardsOrStub(page) {
  let stubbed = false;
  await page.route("**/invoke", async (route) => {
    const req = route.request();
    if (req.method() !== "POST" || JSON.parse(req.postData() || "{}").cmd !== "cards_list") return route.fallback();
    const res = await route.fetch();
    const body = await res.json();
    if (body.ok && body.ok.enabled && body.ok.cards.length === 0) {
      stubbed = true;
      body.ok.cards = [STUB_CARD];
    }
    await route.fulfill({ response: res, json: body });
  });
  return () => stubbed;
}

const STUB_CARD = {
  id: "card_a11y_stub",
  kind: "awaiting_reply",
  title: "Reply to Margaret Thistlewood: Moorland sync",
  detail: "Margaret Thistlewood wrote and hasn't heard back.",
  summary: null,
  people: ["margaret.thistlewood@cindergate.test"],
  due: null,
  score: 0.8,
  rank: 1,
  factors: [{ name: "importance", value: 1, weight: 1, contribution: 1 }],
  evidence: [{ item: { source: "files-1", key: "0:mail/mail-036.eml" }, quote: "Robin,", url: null }],
  actions: [{ index: 0, kind: "draft_reply", label: "Draft a reply", risk: "edit", supported: false }],
  state: "new",
  sensitive: false,
  created_at: "2026-10-24T08:15:00Z",
};

/** LL2: the overlay's state (what the shell would report after a Double Tap) and its history. */
const OVERLAY_VIEW = {
  phase: "done",
  text: "Thanks Ines, the contract is with Vellum & Thistle LLP for review.",
  provider: "local-chat",
  locality: "on_device",
  citations: 2,
  excluded_sensitive: 1,
  can_retry: true,
  message: null,
  verified: null,
  method: null,
};
const OVERLAY_HISTORY = [
  { id: 2, text: "Friday at 10 suits me. I'll bring the draft agenda.", app: "Mail", at: 0 },
  { id: 1, text: "Sounds good, see you then.", app: "Slack", at: 0 },
];

/** Wait for finite animations and transitions (fade-ins skew colour contrast mid-way). */
async function quiet(page) {
  await page.evaluate(() => {
      const finite = document.getAnimations().filter((a) => a.effect?.getComputedTiming().iterations !== Infinity);
      const done = Promise.all(finite.map((a) => a.finished.catch(() => {})));
      return Promise.race([done, new Promise((r) => setTimeout(r, 2000))]);
    }).catch(() => {});
}

/** Run axe in the page, over `scope` (a selector) or the whole document; returns the violations. */
async function runAxe(page, scope) {
  await settle(page);
  await quiet(page);
  if (!(await page.evaluate(() => typeof window.axe === "object"))) await page.addScriptTag({ path: AXE });
  // Text scrolled out of a scroll container (the page body, a long dialog) is "obscured" and axe
  // can't measure its contrast: grow the viewport to the tallest scroll content for the run.
  const viewport = page.viewportSize();
  const tallest = await page.evaluate(() =>
    Math.max(...[document.documentElement, ...document.querySelectorAll("*")].filter((el) => el.scrollHeight > el.clientHeight + 1)
        .map((el) => el.scrollHeight),
    ),
  );
  const grow = viewport && tallest > viewport.height;
  if (grow) {
    await page.setViewportSize({ width: viewport.width, height: Math.min(tallest + 200, 8000) });
    await quiet(page);
  }
  // axe doesn't model the top layer a modal <dialog> is drawn in: page text behind the dialog's
  // opaque surface reads as overlapping it, and the contrast can't be decided. Hide the page
  // (not the dialog's ancestors) while the dialog is audited.
  const isolate = scope?.startsWith("dialog");
  if (isolate)
    await page.addStyleTag({
        content: `body :not(dialog[open], dialog[open] *, :has(dialog[open])) { visibility: hidden !important; }`,
      }).then((h) => h.evaluate((el) => el.setAttribute("data-a11y-isolate", "")));
  try {
    return await axeIn(page, scope);
  } finally {
    if (isolate) await page.evaluate(() => document.querySelector("style[data-a11y-isolate]")?.remove());
    if (grow) await page.setViewportSize(viewport);
  }
}

async function axeIn(page, scope) {
  return page.evaluate(
    async ({ scope, tags }) => {
      if (scope && !document.querySelector(scope)) throw new Error(`nothing matches ${scope} to audit`);
      const r = await window.axe.run(scope ? { include: [[scope]] } : document, {
        runOnly: { type: "tag", values: tags },
        resultTypes: ["violations", "incomplete"],
      });
      // "incomplete" is what axe could not decide (text over an image or gradient, say): recorded
      // for review with impact "needs-review", never failing.
      const rows = [...r.violations, ...r.incomplete.map((v) => ({ ...v, impact: "needs-review" }))];
      return rows.map((v) => ({
          id: v.id,
          impact: v.impact,
          help: v.help,
          helpUrl: v.helpUrl,
          nodes: v.nodes.map((n) => ({ target: n.target.join(" "), impact: n.impact, html: n.html.slice(0, 300), summary: n.failureSummary })),
        })).filter((v) => v.nodes.length > 0);
    },
    { scope, tags: TAGS },
  );
}

export async function a11yScenario({ browser, socket, pages }) {
  const all = [];
  const bridge = await startBridge({ socket, dist: DIST });
  const onboardedCtx = await newContext(browser, { onboarded: true });

  /** Open the app on page `id` in `theme`; `before(page)` runs first (routes). */
  const open = async (id, theme, { ctx = onboardedCtx, before } = {}) => {
    const page = await ctx.newPage();
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    await page.emulateMedia({ colorScheme: theme });
    if (id) await page.addInitScript((p) => localStorage.setItem("nugget.page", p), id);
    await before?.(page);
    await page.goto(`${bridge.url}index.html`);
    return { page, errors };
  };

  /** Audit `scope` (or the page) and fail on serious or critical violations. */
  const audit = async (page, name, theme, scope) => {
    const found = (await runAxe(page, scope)).map((v) => ({ check: name, theme, scope: scope ?? "document", ...v }));
    all.push(...found);
    await page.screenshot({ path: path.join(OUT, `a11y-${theme}-${name.replace(/[^\w-]+/g, "-")}.png`) }).catch(() => {});
    const bad = found.filter((v) => FAILING.has(v.impact));
    const where = (v) => v.nodes.map((n) => n.target).slice(0, 5).join(", ");
    expect(bad.length === 0, `${bad.length} serious/critical violation(s): ${bad.map((v) => `${v.id} (${v.impact}) at ${where(v)}`).join("; ")}`);
  };

  const dialog = (page) => page.locator("dialog[open]");
  const closeDialog = async (page) => {
    await page.keyboard.press("Escape");
    await dialog(page).waitFor({ state: "detached" });
  };

  try {
    // ---- the audit itself: planted violations are found (no false green) ----
    await check("a11y audit finds planted violations", async () => {
      const { page } = await open("status", "light");
      try {
        await page.locator("main h1").first().waitFor();
        await page.evaluate(() => {
          const div = document.createElement("div");
          div.id = "a11y-planted";
          div.innerHTML =
            '<p style="color:#bbb;background:#fff">faint</p><button type="button"><svg width="10" height="10"></svg></button>';
          document.querySelector("main").append(div);
        });
        const ids = (await runAxe(page, "#a11y-planted")).map((v) => v.id);
        expect(
          ids.includes("color-contrast") && ids.includes("button-name"),
          `expected color-contrast and button-name, got ${JSON.stringify(ids)}`,
        );
      } finally {
        await page.close();
      }
    });

    // ---- every page, both themes (the accessibility tree in light) ----
    for (const theme of THEMES) {
      for (const { id, label } of pages) {
        await check(`a11y ${theme} page ${id}`, async () => {
          const { page, errors } = await open(id, theme);
          try {
            await page.locator(`main[aria-label="${label}"] h1`).first().waitFor();
            await audit(page, `page-${id}`, theme);
            for (const [state, steps, scope] of STATES[id] ?? []) {
              await steps(page);
              await audit(page, `page-${id}-${state}`, theme, scope);
            }
            if (theme === "light") {
              const problems = await treeChecks(page, id);
              expect(problems.length === 0, `accessibility tree: ${problems.join("; ")}`);
            }
            expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
          } finally {
            await page.close();
          }
        });
      }
    }

    // ---- dialogs, both themes ----
    for (const theme of THEMES) {
      await check(`a11y ${theme} dialog add-source (kind and folder steps)`, async () => {
        const { page } = await open("sources", theme);
        try {
          await page.locator("main").getByRole("button", { name: exactName("sources.add") }).first().click();
          await dialog(page).getByRole("radio").first().waitFor();
          await audit(page, "dialog-add-source-kind", theme, "dialog[open]");
          await dialog(page).getByRole("button", { name: exactName("common.next") }).click();
          await dialog(page).getByRole("button", { name: exactName("addSource.folder.choose") }).waitFor();
          await audit(page, "dialog-add-source-folder", theme, "dialog[open]");
          // the messaging form (WhatsApp exports, II-wave), through Back to the kinds.
          await dialog(page).getByRole("button", { name: exactName("common.back") }).click();
          await dialog(page).getByRole("radio", { name: new RegExp(`^${escapeRe(t("format.sourceKind.whatsapp"))}\\b`) }).check();
          await dialog(page).getByRole("button", { name: exactName("common.next") }).click();
          await dialog(page).getByRole("button", { name: exactName("addSource.messages.browse") }).waitFor();
          await audit(page, "dialog-add-source-whatsapp", theme, "dialog[open]");
          await closeDialog(page);
        } finally {
          await page.close();
        }
      });

      await check(`a11y ${theme} dialogs merge and split entity`, async () => {
        const { page } = await open("knowledge", theme);
        try {
          const list = page.getByRole("region", { name: t("knowledge.entitiesLabel") });
          await list.getByRole("button", { name: /^Ada Wexford\b/ }).first().click();
          const detail = page.locator("section.entity-detail");
          await detail.getByRole("heading", { name: "Ada Wexford" }).first().waitFor();

          await detail.getByRole("button", { name: exactName("entity.merge") }).click();
          const merge = dialog(page);
          // Opens searching for the entity's own name (no other match); an empty search lists all.
          await merge.getByRole("searchbox").waitFor();
          await audit(page, "dialog-merge-search", theme, "dialog[open]");
          await merge.getByRole("searchbox").fill("");
          const options = merge.locator(".merge-option");
          await until("merge lists candidates", async () => {
            const n = await options.count();
            return [n > 0, n];
          });
          await options.first().locator("input[type=radio]").check();
          await merge.locator(".merge-preview dl").waitFor();
          await audit(page, "dialog-merge", theme, "dialog[open]");
          await closeDialog(page);

          await detail.getByRole("button", { name: exactName("entity.split") }).click();
          await dialog(page).getByRole("checkbox").first().check();
          await audit(page, "dialog-split", theme, "dialog[open]");
          await closeDialog(page);
        } finally {
          await page.close();
        }
      });

      await check(`a11y ${theme} today card sources and action`, async () => {
        let stubbed;
        const { page } = await open("needs", theme, { before: async (p) => (stubbed = await cardsOrStub(p)) });
        try {
          const card = page.locator("li.today-card").filter({ has: page.locator("button[aria-controls]") }).first();
          await card.waitFor();
          await card.locator("button[aria-controls]").first().click();
          await card.locator("ul.evidence").waitFor();
          const id = await card.getAttribute("id");
          await audit(page, "today-card-sources", theme, `li[id="${id}"]`);
          // The action's review dialog (a dry-run preview; nothing runs until Approve).
          const action = card.getByRole("group", { name: tPattern("today.actionsLabel") }).locator("button:not([aria-disabled=true])").first();
          if (!stubbed() && (await action.count()) > 0) {
            await action.click();
            await dialog(page).locator(".today-preview, .note-error").first().waitFor();
            await audit(page, "dialog-today-action", theme, "dialog[open]");
            await closeDialog(page);
          } else log(`  a11y ${theme}: no real card with a supported action; the action dialog is not audited`);
        } finally {
          await page.close();
        }
      });

      await check(`a11y ${theme} command palette`, async () => {
        const { page } = await open("today", theme);
        try {
          await page.locator("main h1").first().waitFor();
          await page.keyboard.press("Control+K");
          await dialog(page).getByRole("combobox").waitFor();
          await audit(page, "dialog-palette", theme, "dialog[open]");
          await dialog(page).getByRole("combobox").fill("se");
          await audit(page, "dialog-palette-filtered", theme, "dialog[open]");
          await closeDialog(page);
        } finally {
          await page.close();
        }
      });

      // the quick ask window (its own HTML entry point): empty, then after an ask on the
      // corpus (an answer, or a plain failure when the home has no AI provider).
      await check(`a11y ${theme} quick ask window`, async () => {
        const page = await onboardedCtx.newPage();
        const errors = [];
        page.on("pageerror", (e) => errors.push(e.message));
        try {
          await page.emulateMedia({ colorScheme: theme });
          await page.goto(`${bridge.url}quickask.html`);
          const box = page.getByRole("searchbox", { name: t("quickAsk.inputLabel") });
          await box.waitFor();
          await audit(page, "quickask", theme);
          await box.fill("Tidewater budget");
          await box.press("Enter");
          await page.locator(".quickask-answer:not([aria-busy=true]), .quickask [role=alert]").first().waitFor({ timeout: 30_000 });
          await audit(page, "quickask-answered", theme);
          const text = (await page.locator("div.quickask-body").textContent()) ?? "";
          expect(!/undefined|\{"|\bat line\b/.test(text), `quick ask shows raw text: ${text.slice(0, 200)}`);
          expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
        } finally {
          await page.close();
        }
      });

      // the Double Tap caret overlay (its own HTML entry point) with a finished draft, then
      // with the previous drafts list open (H). The shell's overlay state is answered here.
      await check(`a11y ${theme} double tap overlay`, async () => {
        const page = await onboardedCtx.newPage();
        try {
          await page.setViewportSize({ width: 460, height: 320 });
          await page.emulateMedia({ colorScheme: theme });
          await page.route("**/invoke", async (route) => {
            const body = JSON.parse(route.request().postData() ?? "{}");
            if (body.cmd === "overlay_state") return route.fulfill({ json: { ok: OVERLAY_VIEW } });
            if (body.cmd === "doubletap_history") {
              const now = Date.now();
              return route.fulfill({ json: { ok: OVERLAY_HISTORY.map((h, i) => ({ ...h, at: now - (i + 1) * 300_000 })) } });
            }
            return route.fallback();
          });
          await page.goto(`${bridge.url}overlay.html`);
          await page.getByText(OVERLAY_VIEW.text).first().waitFor();
          await audit(page, "overlay", theme);
          await page.keyboard.press("h");
          await page.getByText(OVERLAY_HISTORY[0].text).first().waitFor();
          await audit(page, "overlay-history", theme);
        } finally {
          await page.close();
        }
      });

      // Onboarding: a context without the finished state, walked step by step with Continue.
      await check(`a11y ${theme} onboarding steps`, async () => {
        const ctx = await newContext(browser);
        try {
          const { page, errors } = await open(null, theme, { ctx });
          const heading = (key) => page.getByRole("heading", { level: 1, name: t(key), exact: true });
          const cont = () =>
            page.locator("footer.ob-actions").getByRole("button", { name: exactName("onboarding.continue") }).click();
          const step = async (key, name) => {
            await heading(key).waitFor();
            await audit(page, `onboarding-${name}`, theme);
          };
          // The fork, then run it yourself: AI first, then a local source.
          await step("onboarding.fork.heading", "fork");
          await page.locator('button.ob-fork-card[data-path="local"]').click();
          await heading("onboarding.ai.heading").waitFor();
          for (const choice of ["chatgpt", "local", "ollama", "anthropic", "later"]) {
            await page.locator(`.ai-choices input[type=radio][value="${choice}"]`).check();
            await audit(page, `onboarding-ai-${choice}`, theme);
          }
          await cont();
          await step("onboarding.source.heading", "source");
          await page.getByRole("button", { name: new RegExp(`^(${escapeRe(t("onboarding.source.addFolder"))}|${escapeRe(t("onboarding.source.addAnother"))})$`) }).click();
          await dialog(page).getByRole("button", { name: exactName("addSource.folder.choose") }).waitFor();
          await audit(page, "onboarding-source-folder", theme, "dialog[open]");
          await closeDialog(page);
          await cont();
          await step("onboarding.perm.heading", "permissions");
          await cont();
          await step("onboarding.trigger.heading", "trigger");
          await cont();
          // The first-answer step: what setup did is its level-2 section.
          await page.getByRole("heading", { level: 2, name: t("onboarding.done.heading"), exact: true }).waitFor();
          await audit(page, "onboarding-done", theme);
          expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
        } finally {
          await ctx.close().catch(() => {});
        }
      });
    }
  } finally {
    await onboardedCtx.close().catch(() => {});
    await bridge.close().catch(() => {});
  }

  fs.writeFileSync(path.join(OUT, "a11y-violations.json"), JSON.stringify(all, null, 2));
  const failing = all.filter((v) => FAILING.has(v.impact)).length;
  log(`a11y: ${all.length} finding(s), ${failing} serious/critical`);
  return { violations: all.length, failing };
}
