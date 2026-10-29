// The Double Tap overlay, end to end in the real desktop app on a real desktop session.
//
// Starts a fake OpenAI-compatible provider on 127.0.0.1, a real nuggetd configured to draft with
// it, and the real debug build of nugget-desktop. Opens a text field in a native app (the GTK
// fixture on Linux, Notepad on Windows, TextEdit on macOS), presses the real Double Tap trigger
// (XTest / SendInput / CGEvent), then checks that:
//   1. the overlay window (label `doubletap-overlay`, title "Nugget Double Tap") appears, in
//      the OS window list,
//   2. it streams the fake provider's draft (the debug-only probe the app writes with
//      NUGGET_E2E_PROBE shows each view; screenshots show it on screen),
//   3. pressing Enter in the overlay (its real accept key) inserts the draft into the target
//      field, read back from the target app itself.
// Writes screenshots, logs, the probe and a summary (timings) to --out.
//
//   node apps/desktop/e2e-native/run.mjs --app <nugget-desktop> --nuggetd <nuggetd> --out <dir>
//
// See README.md. Node built-ins only.
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { parseArgs } from "node:util";
import { startDaemon } from "../e2e/daemon.mjs";
import { DRAFT_TEXT, startMockProvider } from "./mock-provider.mjs";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const t0 = Date.now();
const log = (msg) => console.log(`[${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}s] ${msg}`);

const { values: args } = parseArgs({
  options: {
    app: { type: "string" },
    nuggetd: { type: "string" },
    out: { type: "string", default: path.join(os.tmpdir(), "nugget-overlay-e2e") },
    attempts: { type: "string", default: "3" },
  },
});

async function platform() {
  switch (process.platform) {
    case "linux":
      return (await import("./linux.mjs")).linux;
    case "win32":
      return (await import("./windows.mjs")).windows;
    case "darwin":
      return (await import("./macos.mjs")).macos;
    default:
      throw new Error(`unsupported platform ${process.platform}`);
  }
}

function binary(arg, name) {
  const exe = process.platform === "win32" ? `${name}.exe` : name;
  const target = process.env.CARGO_TARGET_DIR ?? path.resolve(import.meta.dirname, "../../../target");
  const p = path.resolve(arg ?? path.join(target, "debug", exe));
  if (!fs.existsSync(p)) throw new Error(`${p} does not exist (build it, or pass --${name === "nuggetd" ? "nuggetd" : "app"})`);
  return p;
}

/** The probe file the app appends to, parsed. */
function probeLines(file) {
  if (!fs.existsSync(file)) return [];
  return fs
    .readFileSync(file, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l));
}

async function waitFor(what, fn, timeoutMs, pollMs = 50) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > deadline) throw new Error(`timed out after ${timeoutMs} ms waiting for ${what}`);
    await sleep(pollMs);
  }
}

const config = (baseUrl) => `# Written by apps/desktop/e2e-native/run.mjs: draft with the fake provider only.
[runtime]
enabled = false

[mcp]
http = false

[knowledge]
enabled = false

[[providers]]
id = "fake"
kind = "openai_compatible"
base_url = "${baseUrl}"
model = "fake-draft"
tiers = ["triage", "light", "synthesis"]
`;

async function main() {
  const os_ = await platform();
  const out = path.resolve(args.out);
  fs.mkdirSync(out, { recursive: true });
  const app = binary(args.app, "nugget-desktop");
  const nuggetd = binary(args.nuggetd, "nuggetd");
  const probe = path.join(out, "probe.jsonl");
  fs.rmSync(probe, { force: true });
  const summary = { os: os_.name, target: os_.targetName, trigger: os_.triggerLabel, ok: false };
  const cleanups = [];

  try {
    await os_.start(log);
    cleanups.push(() => os_.stop());

    const provider = await startMockProvider({ log });
    cleanups.push(() => provider.close());
    log(`fake provider at ${provider.baseUrl}`);

    const daemon = await startDaemon(nuggetd, { stderrPath: path.join(out, "nuggetd.log"), config: config(provider.baseUrl) });
    cleanups.push(() => daemon.stop({ keep: true }));
    log(`nuggetd ready at ${daemon.socket} (home ${daemon.home})`);

    const appLog = fs.openSync(path.join(out, "nugget-desktop.log"), "w");
    const appProc = spawn(app, [], {
      env: {
        ...process.env,
        NUGGET_HOME: daemon.home,
        NUGGET_KEYCHAIN: "off",
        NUGGET_E2E_PROBE: probe,
        NUGGET_LOG: "info,nugget_desktop=debug",
        ...os_.appEnv(daemon.home),
      },
      stdio: ["ignore", appLog, appLog],
    });
    let appExit = null;
    appProc.on("exit", (code, signal) => (appExit = { code, signal }));
    cleanups.push(async () => {
      if (appExit) return;
      appProc.kill();
      for (let i = 0; i < 50 && !appExit; i++) await sleep(100);
      if (!appExit) appProc.kill("SIGKILL");
    });
    log(`started ${app} (pid ${appProc.pid})`);
    os_.appPid = appProc.pid;

    const listener = await waitFor(
      "the Double Tap listener to start (probe 'listener')",
      () => {
        if (appExit) throw new Error(`nugget-desktop exited early (${JSON.stringify(appExit)}); see nugget-desktop.log`);
        return probeLines(probe).find((l) => l.event === "listener");
      },
      90_000,
      200,
    );
    if (!listener.detail.listening) throw new Error(`the Double Tap trigger is unavailable: ${listener.detail.error}`);
    log("Double Tap listener running");
    await waitFor(
      "nugget-desktop to connect to nuggetd",
      () => fs.readFileSync(path.join(out, "nugget-desktop.log"), "utf8").includes("connected to nuggetd"),
      30_000,
      200,
    );
    log("nugget-desktop connected to nuggetd");
    // Let the main window finish showing before the target takes the foreground.
    await sleep(1500);

    const target = await os_.openTarget(log, out);
    cleanups.push(() => target.close());
    await waitFor(
      `${os_.targetName} in the foreground`,
      async () => {
        const fg = await os_.foreground();
        if (fg.includes(target.title)) return true;
        await os_.activate(target);
        return false;
      },
      20_000,
      500,
    );
    log(`${os_.targetName} is in the foreground`);
    await os_.screenshot(path.join(out, "before.png"));

    // Fire the trigger with real key presses; retried when the listener missed it (reported).
    const attempts = Number(args.attempts);
    let firedAt = null;
    let pressedAt = 0;
    let attempt = 0;
    for (attempt = 1; attempt <= attempts && !firedAt; attempt++) {
      const before = probeLines(probe).length;
      pressedAt = Date.now();
      await os_.trigger();
      try {
        firedAt = await waitFor(
          "the trigger",
          () => probeLines(probe).slice(before).find((l) => l.event === "trigger"),
          3000,
        );
      } catch {
        log(`attempt ${attempt}: the trigger did not fire; foreground is "${await os_.foreground()}"`);
        await os_.activate(target);
        await sleep(500);
      }
    }
    if (!firedAt) throw new Error(`the Double Tap trigger did not fire in ${attempts} attempts`);
    summary.trigger_attempts = attempt - 1;
    log(`trigger fired (attempt ${attempt - 1}), ${(firedAt.t - pressedAt).toFixed(0)} ms after the key presses began`);

    // (a) The overlay appears, as the app and as the window system see it.
    const shown = await waitFor("the overlay to show (probe 'show')", () => {
      const lines = probeLines(probe);
      const failed = lines.find((l) => l.event === "view" && ["refused", "error"].includes(l.detail.phase));
      if (failed) throw new Error(`the overlay went to ${failed.detail.phase}: ${failed.detail.message}`);
      return lines.find((l) => l.event === "show" && l.t >= firedAt.t);
    }, 15_000);
    log(`overlay shown: ${JSON.stringify(shown.detail)}`);
    const win = await waitFor(
      "the overlay in the OS window list",
      async () => (await os_.overlayWindows()).find((w) => w.visible && w.w > 50 && w.h > 50),
      10_000,
      100,
    );
    log(`OS window list: ${JSON.stringify(win)}`);
    summary.overlay_window = win;

    // (b) It streams the fake provider's draft.
    const firstToken = await waitFor(
      "the first streamed text",
      () => probeLines(probe).find((l) => l.event === "view" && l.t >= firedAt.t && l.detail.phase === "streaming" && l.detail.text),
      30_000,
    );
    log(`first token: ${JSON.stringify(firstToken.detail.text)}`);
    await os_.screenshot(path.join(out, "overlay-streaming.png"));
    const done = await waitFor("the draft to finish (phase done)", () => {
      const lines = probeLines(probe);
      const err = lines.find((l) => l.event === "view" && l.t >= firedAt.t && l.detail.phase === "error");
      if (err) throw new Error(`the draft failed: ${err.detail.message}`);
      return lines.find((l) => l.event === "view" && l.t >= firedAt.t && l.detail.phase === "done");
    }, 30_000);
    const views = probeLines(probe).filter((l) => l.event === "view" && l.t >= firedAt.t && l.detail.phase === "streaming" && l.detail.text);
    const distinct = new Set(views.map((v) => v.detail.text)).size;
    log(`draft done after ${distinct} streamed views: ${JSON.stringify(done.detail.text)} (provider ${done.detail.provider}, ${done.detail.locality})`);
    if (done.detail.provider !== "fake") throw new Error(`the draft came from provider ${done.detail.provider}, not the fake one`);
    if (!done.detail.text.includes("noon works for me")) throw new Error(`unexpected draft text ${JSON.stringify(done.detail.text)} (provider streamed ${JSON.stringify(DRAFT_TEXT)})`);
    if (distinct < 2) throw new Error("the overlay showed the draft in one piece: no streaming seen");
    if (!provider.requests.some((r) => r.stream)) throw new Error("the fake provider got no streaming request");
    const winDone = (await os_.overlayWindows()).find((w) => w.visible) ?? win;
    await os_.screenshot(path.join(out, "overlay.png"), winDone);
    await os_.screenshot(path.join(out, "overlay-screen.png"));

    // (c) Accept with the overlay's own key, Enter. The overlay takes the keyboard while a draft
    // is open; if the window system kept it elsewhere, click the overlay's text first (no button
    // there), as a user would, and say so.
    const fg = await os_.foreground();
    summary.overlay_focused = fg.includes("Nugget Double Tap") || Boolean(os_.overlayIsForeground?.(fg));
    if (!summary.overlay_focused) {
      log(`overlay not focused (foreground "${fg}"); clicking it first`);
      await os_.click(winDone.x + winDone.w / 2, winDone.y + 70);
      await sleep(300);
    }
    const acceptAt = Date.now();
    await os_.enter();
    const inserted = await waitFor("the insertion (phase inserted)", () => {
      const lines = probeLines(probe);
      const err = lines.find((l) => l.event === "view" && l.t >= acceptAt - 5 && l.detail.phase === "error");
      if (err) throw new Error(`the insertion failed: ${err.detail.message}`);
      return lines.find((l) => l.event === "view" && l.t >= acceptAt - 5 && l.detail.phase === "inserted");
    }, 20_000);
    log(`inserted: method ${inserted.detail.method}, verified ${inserted.detail.verified}`);
    await os_.screenshot(path.join(out, "after.png"));
    const text = await waitFor(
      `the draft in ${os_.targetName}`,
      async () => {
        const t = await target.read();
        return t.includes(done.detail.text) ? t : null;
      },
      10_000,
      300,
    ).catch(async (e) => {
      throw new Error(`${e.message}; it holds ${JSON.stringify(await target.read())}`);
    });
    log(`${os_.targetName} reads back ${JSON.stringify(text)}`);
    if (!text.includes(target.initial)) throw new Error(`the field lost its original text ${JSON.stringify(target.initial)}`);
    await waitFor("the overlay to hide", async () => !(await os_.overlayWindows()).some((w) => w.visible), 10_000, 200);

    const lines = probeLines(probe);
    const hidden = lines.find((l) => l.event === "hide" && l.t >= acceptAt - 5);
    Object.assign(summary, {
      ok: true,
      method: inserted.detail.method,
      verified: inserted.detail.verified,
      streamed_views: distinct,
      draft: done.detail.text,
      read_back: text,
      ms: {
        keys_to_trigger: Math.round(firedAt.t - pressedAt),
        trigger_to_overlay_shown: Math.round(shown.t - firedAt.t),
        trigger_to_first_token: Math.round(firstToken.t - firedAt.t),
        trigger_to_draft_done: Math.round(done.t - firedAt.t),
        accept_to_overlay_hidden: hidden ? Math.round(hidden.t - acceptAt) : null,
        accept_to_inserted: Math.round(inserted.t - acceptAt),
      },
    });
    log(`OK ${JSON.stringify(summary.ms)}`);
  } catch (e) {
    summary.error = e.message;
    log(`FAILED: ${e.stack ?? e}`);
    try {
      await os_.screenshot(path.join(out, "failure.png"));
    } catch {
      // Best effort.
    }
  } finally {
    for (const c of cleanups.reverse()) {
      try {
        await c();
      } catch (e) {
        log(`cleanup: ${e.message}`);
      }
    }
  }
  fs.writeFileSync(path.join(out, "summary.json"), JSON.stringify(summary, null, 2));
  if (process.env.GITHUB_STEP_SUMMARY) {
    const m = summary.ms ?? {};
    fs.appendFileSync(
      process.env.GITHUB_STEP_SUMMARY,
      [
        `### Double Tap overlay on ${summary.os}: ${summary.ok ? "ok" : "FAILED"}`,
        "",
        "| target | trigger | keys→fire ms | fire→overlay shown ms | fire→first token ms | fire→draft done ms | accept→inserted ms | method | verified | overlay focused |",
        "|---|---|---|---|---|---|---|---|---|---|",
        `| ${summary.target} | ${summary.trigger} | ${m.keys_to_trigger ?? "–"} | ${m.trigger_to_overlay_shown ?? "–"} | ${m.trigger_to_first_token ?? "–"} | ${m.trigger_to_draft_done ?? "–"} | ${m.accept_to_inserted ?? "–"} | ${summary.method ?? "–"} | ${summary.verified ?? "–"} | ${summary.overlay_focused ?? "–"} |`,
        "",
        summary.error ? `Error: \`${summary.error}\`\n` : "",
      ].join("\n"),
    );
  }
  console.log(JSON.stringify(summary, null, 2));
  process.exit(summary.ok ? 0 : 1);
}

main();
