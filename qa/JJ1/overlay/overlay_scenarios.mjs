// JJ1 explore QA: Double Tap overlay (II3) and quick ask (II4) in the real debug app under
// Xvfb + openbox. Reuses apps/desktop/e2e-native (linux driver, daemon starter) and a
// programmable mock provider (./mock.mjs). Run through ./run.sh.
//
//   node qa/JJ1/overlay/overlay_scenarios.mjs --out <dir> [--only s1,s2,...] [--repeat N]
import { execFileSync, spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { parseArgs } from "node:util";
import { REPO, rpc, startDaemon } from "../../../apps/desktop/e2e/daemon.mjs";
import { linux } from "../../../apps/desktop/e2e-native/linux.mjs";
import { startMock } from "./mock.mjs";

const here = import.meta.dirname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const t0 = Date.now();
const log = (m) => console.log(`[${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}s] ${m}`);
const { values: args } = parseArgs({ options: { out: { type: "string" }, only: { type: "string" }, repeat: { type: "string", default: "1" } } });
const out = path.resolve(args.out);
fs.mkdirSync(out, { recursive: true });
const probeFile = path.join(out, "probe.jsonl");
fs.rmSync(probeFile, { force: true });
const APP = process.env.QA_APP ?? path.join(REPO, "target/debug/nugget-desktop");
const NUGGETD = path.join(REPO, "target/debug/nuggetd");
const SCREEN = { w: 1920, h: 1080 };

const probe = () => (fs.existsSync(probeFile) ? fs.readFileSync(probeFile, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);
const since = (t) => probe().filter((l) => l.t >= t - 5);
const views = (t) => since(t).filter((l) => l.event === "view").map((l) => l.detail);
async function waitFor(what, fn, ms = 15000, poll = 60) {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error(`timeout: ${what}`);
    await sleep(poll);
  }
}
const waitPhase = (t, phase, ms = 20000) =>
  waitFor(`phase ${phase}`, () => {
    const vs = since(t).filter((l) => l.event === "view");
    const bad = vs.find((l) => ["error", "refused"].includes(l.detail.phase) && phase !== l.detail.phase);
    if (bad) throw new Error(`went to ${bad.detail.phase}: ${bad.detail.message}`);
    return vs.find((l) => l.detail.phase === phase);
  }, ms);

const xte = (...c) => execFileSync("xte", c, { stdio: "ignore" });
const xdo = (...a) => {
  try {
    return execFileSync("xdotool", a, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  } catch {
    return "";
  }
};
const key = (k) => xte(`key ${k}`);
const ctrl = (k, shift = false) => xte("keydown Control_L", ...(shift ? ["keydown Shift_L"] : []), `key ${k}`, ...(shift ? ["keyup Shift_L"] : []), "keyup Control_L");
const typeStr = (s) => xte(`str ${s}`);
const atspi = (...a) => {
  const r = spawnSync("/usr/bin/python3.12", [path.join(here, "atspi.py"), ...a], { encoding: "utf8", timeout: 20000 });
  try {
    return JSON.parse(r.stdout);
  } catch {
    return { error: (r.stderr || r.stdout || "").slice(-300) };
  }
};
const overlayRect = async () => (await linux.overlayWindows()).find((w) => w.visible) ?? null;
const shot = (name, rect, scale = 60) => {
  const f = path.join(out, `${name}.png`);
  const a = ["-window", "root"];
  if (rect) a.push("-crop", `${rect.w}x${rect.h}+${rect.x}+${rect.y}`, "+repage");
  a.push("-resize", `${scale}%`, "-colors", "64", f);
  try {
    execFileSync("import", a, { stdio: "ignore" });
  } catch {}
  return path.basename(f);
};

const results = [];
function record(id, name, ok, evidence) {
  results.push({ id, name, ok, evidence });
  log(`${ok ? "PASS" : "FAIL"} ${id} ${name}: ${JSON.stringify(evidence).slice(0, 600)}`);
}

// ---- stack -----------------------------------------------------------------------------------
const config = (baseUrl) => `[runtime]\nenabled = false\n[mcp]\nhttp = false\n[knowledge]\nenabled = false\n[[providers]]\nid = "fake"\nkind = "openai_compatible"\nbase_url = "${baseUrl}"\nmodel = "fake-draft"\ntiers = ["triage", "light", "synthesis"]\n`;
let mock, daemon, appProc, appExit, target;
const appEnv = () => ({ ...process.env, NUGGET_HOME: daemon.home, NUGGET_KEYCHAIN: "off", NUGGET_E2E_PROBE: probeFile, NUGGET_LOG: "info,nugget_desktop=debug", ...linux.appEnv(daemon.home) });

async function startStack() {
  mock = await startMock({ log: () => {} });
  daemon = await startDaemon(NUGGETD, { stderrPath: path.join(out, "nuggetd.log"), config: config(mock.baseUrl) });
  log(`daemon ${daemon.socket} home ${daemon.home}`);
  const appLog = fs.openSync(path.join(out, "nugget-desktop.log"), "w");
  appProc = spawn(APP, [], { env: appEnv(), stdio: ["ignore", appLog, appLog] });
  appProc.on("exit", (code, signal) => (appExit = { code, signal }));
  await waitFor("listener", () => {
    if (appExit) throw new Error(`app exited ${JSON.stringify(appExit)}`);
    return probe().find((l) => l.event === "listener");
  }, 90000, 200);
  await waitFor("daemon connection", () => fs.readFileSync(path.join(out, "nugget-desktop.log"), "utf8").includes("connected to nuggetd"), 30000, 200);
  await sleep(1500);
}
async function stopStack() {
  try {
    await target?.close();
  } catch {}
  if (appProc && !appExit) {
    appProc.kill();
    for (let i = 0; i < 50 && !appExit; i++) await sleep(100);
    if (!appExit) appProc.kill("SIGKILL");
  }
  await daemon?.stop({ keep: true });
  await mock?.close();
}

/** A fresh GTK fixture (composer focused, "Hello "), optionally moved to (x, y). */
async function freshTarget(pos) {
  if (target) {
    await target.close();
    await sleep(400);
  }
  target = await linux.openTarget(() => {});
  if (pos) {
    const id = xdo("search", "--name", "^Nugget IT - chat$").split("\n")[0];
    xdo("windowmove", "--sync", id, String(pos.x), String(pos.y));
    await sleep(400);
  }
  await waitFor("fixture foreground", async () => {
    if ((await linux.foreground()).includes(target.title)) return true;
    await linux.activate(target);
    return false;
  }, 15000, 300);
  return target;
}

/** Fire the trigger (retrying a missed double tap) and wait for `done`. Returns the trigger time. */
async function fireAndDone({ text, delayMs = 120, chunks = 6, wait = "done" } = {}) {
  if (text) mock.next = { text, delayMs, chunks };
  for (let i = 0; i < 3; i++) {
    const t = Date.now();
    await linux.trigger();
    try {
      await waitFor("trigger", () => since(t).find((l) => l.event === "trigger"), 3000);
      if (wait) await waitPhase(t, wait);
      return t;
    } catch (e) {
      if (!String(e.message).includes("timeout: trigger")) throw e;
      await linux.activate(target);
      await sleep(400);
    }
  }
  throw new Error("trigger never fired");
}
const readField = async () => (await target.read()) ?? "";
const hiddenSoon = () => waitFor("overlay hidden", async () => !(await overlayRect()), 5000, 150).then(() => true, () => false);
/** Make sure no overlay is left from a failed scenario. */
async function settle() {
  const r = await overlayRect();
  if (r && !(await linux.foreground()).includes("Nugget Double Tap")) {
    await linux.click(r.x + r.w / 2, r.y + 12);
    await sleep(300);
  }
  if (await overlayRect()) {
    key("Escape");
    await sleep(300);
    if (await overlayRect()) {
      key("Escape");
      await sleep(300);
    }
  }
}

// ---- scenarios -------------------------------------------------------------------------------
const S = {};

S.s1 = async () => {
  await freshTarget();
  const draft = "Sounds good, noon works for me. See you there.";
  const t = await fireAndDone({ text: draft });
  key("Tab");
  await sleep(300);
  typeStr(" EDITED-X");
  await sleep(300);
  const acc = Date.now();
  ctrl("Return");
  const ins = await waitPhase(acc, "inserted").catch((e) => ({ err: e.message }));
  await sleep(600);
  const got = await readField();
  const want = `Hello ${draft} EDITED-X`;
  const hid = await hiddenSoon();
  record("1", "edit then Ctrl+Enter inserts edited text", got === want && hid, { want, got, inserted: ins.detail ?? ins, overlay_hidden: hid, phases: [...new Set(views(t).map((v) => v.phase))] });
};

S.s2 = async () => {
  await freshTarget();
  const draft = "Original draft for insert as is.";
  await fireAndDone({ text: draft });
  key("Tab");
  await sleep(300);
  typeStr(" EDIT-DISCARDED");
  await sleep(300);
  const acc = Date.now();
  ctrl("Return", true);
  await waitPhase(acc, "inserted").catch(() => {});
  await sleep(600);
  const got = await readField();
  const want = `Hello ${draft}`;
  record("2", "edit then Ctrl+Shift+Enter inserts the original", got === want, { want, got, overlay_hidden: await hiddenSoon() });
};

const REF = { s: ["Shorter", "shorter"], l: ["Longer", "longer"], f: ["Formal", "formal tone"], c: ["Casual", "casual"] };
function refineReplies(base) {
  mock.next = (body) => {
    const all = (body.messages ?? []).map((m) => (typeof m.content === "string" ? m.content : JSON.stringify(m.content))).join("\n");
    for (const [k, [label, needle]] of Object.entries(REF)) if (new RegExp(`Rewrite the selected draft[^\\n]*${needle}`, "i").test(all)) return { text: `${label} version (${k}).`, delayMs: 120 };
    return { text: base, delayMs: 120 };
  };
}
const bodyText = (r) => r.messages.map((m) => `${m.role}: ${typeof m.content === "string" ? m.content : JSON.stringify(m.content)}`).join("\n");

S.s3 = async () => {
  await freshTarget();
  const base = "Base draft about the quarterly budget review meeting.";
  refineReplies(base);
  await fireAndDone();
  let shown = base;
  const per = [];
  for (const k of ["s", "l", "f", "c"]) {
    const n0 = mock.streams().length;
    const t = Date.now();
    key(k);
    let done;
    try {
      done = await waitFor("refined done", () => views(t).find((v) => v.phase === "done" && v.text !== shown), 10000);
    } catch (e) {
      done = { text: null, err: e.message };
    }
    await sleep(300);
    const reqs = mock.streams().slice(n0);
    const body = reqs[0] ? bodyText(reqs[0]) : "";
    const [label, needle] = REF[k];
    const ok = reqs.length === 1 && new RegExp(`Rewrite the selected draft[^\\n]*${needle}`, "i").test(body) && body.includes(shown) && done.text === `${label} version (${k}).`;
    per.push({ key: k.toUpperCase(), ok, new_requests: reqs.length, instruction_in_body: new RegExp(needle, "i").test(body), prev_draft_in_body: body.includes(shown), shown: done.text });
    if (done.text) shown = done.text;
  }
  // save one body for evidence
  const last = mock.streams().at(-1);
  fs.writeFileSync(path.join(out, "s3-refine-body.txt"), bodyText(last).slice(-1500));
  record("3a", "S/L/F/C each send one request with instruction + current draft", per.every((p) => p.ok), per);

  // Rapid repeats: S S S quickly -> one request.
  const n0 = mock.streams().length;
  const t = Date.now();
  xte("key s", "key s", "key s", "usleep 40000", "key s");
  await waitFor("done after rapid", () => views(t).find((v) => v.phase === "done"), 10000).catch(() => {});
  await sleep(800);
  const rapid = mock.streams().length - n0;
  // Rapid R R R.
  const n1 = mock.streams().length;
  const t2 = Date.now();
  xte("key r", "key r", "key r");
  await waitFor("done after rapid r", () => views(t2).find((v) => v.phase === "done"), 10000).catch(() => {});
  await sleep(800);
  const rapidR = mock.streams().length - n1;
  record("3b", "rapid repeated S / R send one request each burst", rapid === 1 && rapidR === 1, { s_burst_requests: rapid, r_burst_requests: rapidR });
  key("Escape");
  await hiddenSoon();

  // Refinement while streaming: slow draft, press S after the first chunk.
  await freshTarget();
  const slow = "One two three four five six seven eight nine ten eleven twelve thirteen fourteen.";
  mock.next = (body) => {
    const all = bodyText({ messages: body.messages ?? [] });
    return /Rewrite the selected draft[^\n]*shorter/i.test(all) ? { text: "Shorter version (s).", delayMs: 120 } : { text: slow, delayMs: 700, chunks: 8 };
  };
  const t3 = await fireAndDone({ wait: "streaming" });
  await waitFor("2nd chunk", () => views(t3).filter((v) => v.phase === "streaming" && v.text).length >= 2, 8000).catch(() => {});
  const partial = views(t3).filter((v) => v.phase === "streaming").at(-1)?.text;
  const n2 = mock.streams().length;
  const t4 = Date.now();
  key("s");
  const d = await waitFor("refined", () => views(t4).find((v) => v.phase === "done"), 10000).catch(() => null);
  await sleep(300);
  const reqs = mock.streams().slice(n2);
  const firstReq = mock.streams()[n2 - 1];
  const body = reqs[0] ? bodyText(reqs[0]) : "";
  const sel = body.match(/selected[^\n]*\n?[^\n]*/i)?.[0];
  record("3c", "S while streaming", Boolean(d) && reqs.length === 1, {
    partial_on_screen: partial,
    refine_body_has_partial: partial ? body.includes(partial) : null,
    refine_body_has_full_draft: body.includes(slow),
    first_stream_closed_early: firstReq?.closed && !firstReq?.finished,
    first_stream_chunks_sent: firstReq?.sent,
    new_requests: reqs.length,
    shown: d?.text,
    sel: sel?.slice(0, 200),
  });
  key("Escape");
  await hiddenSoon();
};

S.s4 = async () => {
  for (const when of ["done", "streaming"]) {
    await freshTarget();
    const t = await fireAndDone({ text: "A draft that will be cancelled with Escape.", delayMs: when === "streaming" ? 600 : 100, wait: when });
    await sleep(200);
    const n = mock.streams().length;
    const esc = Date.now();
    key("Escape");
    const hid = await hiddenSoon();
    await sleep(400);
    const after = await readField();
    const fg = await linux.foreground();
    typeStr("z");
    await sleep(500);
    const typed = await readField();
    const last = mock.streams().at(-1);
    const hideLine = since(esc).find((l) => l.event === "hide");
    record(`4-${when}`, `Esc while ${when} cancels and returns focus`, hid && after === "Hello " && typed === "Hello z", {
      field_after_esc: after,
      foreground_after_esc: fg,
      after_typing_z: typed,
      overlay_hidden: hid,
      hide: hideLine?.detail,
      stream_closed: when === "streaming" ? last?.closed && !last?.finished : undefined,
      new_requests_after_esc: mock.streams().length - n,
      phases: [...new Set(views(t).map((v) => v.phase))],
    });
  }
};

S.s5 = async () => {
  const id = () => xdo("search", "--name", "^Nugget IT - chat$").split("\n")[0];
  const spots = { tl: { x: 0, y: 0 }, tr: { x: 1320, y: 0 }, bl: { x: 0, y: 580 }, br: { x: 1320, y: 580 }, top: { x: 660, y: 0 }, bottom: { x: 660, y: 580 } };
  // Chat fields are capped at 60 words (nugget-draft LengthBand hard_max), so tall = many short lines.
  const long = Array.from({ length: 28 }, (_, i) => `Point ${i + 1}.`).join("\n");
  const rows = [];
  for (const [name, pos] of Object.entries(spots)) {
    for (const draft of ["short", "long"]) {
      await freshTarget(pos);
      const geo = Object.fromEntries(xdo("getwindowgeometry", "--shell", id()).split("\n").map((l) => l.split("=")));
      const comp = atspi("ext", "gtk_test_app|python", "Message composer");
      const t = await fireAndDone({ text: draft === "long" ? long : "Short draft near an edge.", delayMs: 60, chunks: draft === "long" ? 12 : 4 });
      await sleep(700);
      const r = await overlayRect();
      const lastShow = since(t).filter((l) => ["show", "resize"].includes(l.event)).at(-1)?.detail;
      const inside = r && r.x >= 0 && r.y >= 0 && r.x + r.w <= SCREEN.w && r.y + r.h <= SCREEN.h;
      // Overlap with the composer line holding the caret: the composer's last 30 px band.
      let covers = null;
      if (r && comp && comp.w) {
        const band = { x: comp.x, y: comp.y, w: comp.w, h: Math.min(comp.h, 20) };
        covers = !(r.x + r.w <= band.x || band.x + band.w <= r.x || r.y + r.h <= band.y || band.y + band.h <= r.y);
      }
      if (name === "br" && draft === "long") shot("s5-br-long", r ?? undefined, 70);
      rows.push({ spot: name, draft, win: `${geo.X},${geo.Y}`, composer: comp?.w ? `${comp.x},${comp.y},${comp.w}x${comp.h}` : comp?.error ?? null, overlay: r ? `${r.x},${r.y},${r.w}x${r.h}` : null, probe: lastShow ? `${lastShow.position}/${lastShow.size}` : null, inside, covers_caret_line: covers, done_chars: views(t).filter((v) => v.phase === "done").at(-1)?.text.length, ok: Boolean(inside) && covers !== true && (draft === "short" || r.h <= 420) });
      key("Escape");
      await hiddenSoon();
    }
  }
  record("5", "edges/corners + long draft stay on screen, off the field", rows.every((r) => r.ok), rows);
};

S.s6 = async () => {
  // Build up history: three inserts (normal, card, SSN, IBAN, API-ish), then list with H.
  const drafts = {
    plain: "History plain draft one.",
    card: "Pay with card 4111 1111 1111 1111 please.",
    ssn: "My SSN is 123-45-6789 for the form.",
    iban: "IBAN DE89 3704 0044 0532 0130 00 for the transfer.",
    plain2: "History plain draft two.",
  };
  for (const [k, text] of Object.entries(drafts)) {
    await freshTarget();
    await fireAndDone({ text, delayMs: 50 });
    const acc = Date.now();
    key("Return");
    await waitPhase(acc, "inserted").catch(() => {});
    await hiddenSoon();
  }
  await freshTarget();
  await fireAndDone({ text: "Current draft before history.", delayMs: 50 });
  key("h");
  await sleep(800);
  const r = await overlayRect();
  const dump = atspi("dump", "Nugget Double Tap");
  const texts = Array.isArray(dump) ? [...new Set(dump.map((d) => d.name || d.text).filter(Boolean))] : dump;
  const blob = JSON.stringify(texts);
  shot("s6-history", r ?? undefined, 70);
  const has = Object.fromEntries(Object.entries(drafts).map(([k, v]) => [k, blob.includes(v.slice(0, 18))]));
  // Re-insert: the list focus is on the newest; move down one (plain draft? newest first) and Enter.
  key("Down");
  await sleep(200);
  const acc = Date.now();
  key("Return");
  await waitPhase(acc, "inserted").catch(() => {});
  await sleep(600);
  const got = await readField();
  record("6", "history list contents + re-insert", has.plain && has.plain2 && !has.card, { in_history: has, read_back_after_reinsert: got, atspi_sample: Array.isArray(texts) ? texts.slice(0, 25) : texts });
};

S.s8 = async () => {
  // Index one small file so answers can cite it.
  const dir = path.join(daemon.home, "qa-notes");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "offsite.md"), "# Offsite plan\n\nThe team offsite is in Lisbon on 14 March. Budget owner: Mara Quint.\n");
  if (!S.s8.added) await rpc(daemon.socket, "sources.add", { id: null, kind: "files", name: "QA notes", settings: { roots: [dir], include: [], exclude: [] } });
  await waitFor("indexed", async () => (await rpc(daemon.socket, "status")).items > 0, 60000, 500).catch(() => {});
  S.s8.added = true;
  const st = await rpc(daemon.socket, "status");
  mock.next = { text: "The offsite is in Lisbon on 14 March [1].", delayMs: 150, chunks: 4 };
  await freshTarget();
  const qaWins = () => xdo("search", "--onlyvisible", "--name", "^Nugget Quick Ask$").split("\n").filter(Boolean);
  const n0 = mock.streams().length;
  ctrl("space", true);
  const opened = await waitFor("quick ask visible", () => qaWins().length > 0, 5000).then(() => true, () => false);
  await sleep(400);
  const fg1 = await linux.foreground();
  typeStr("Where is the offsite?");
  key("Return");
  await waitFor("ask request", () => mock.streams().length > n0, 10000).catch(() => {});
  await sleep(2500);
  const wid = qaWins()[0];
  const g = wid ? Object.fromEntries(xdo("getwindowgeometry", "--shell", wid).split("\n").map((l) => l.split("="))) : null;
  const rect = g ? { x: +g.X, y: +g.Y, w: +g.WIDTH, h: +g.HEIGHT } : null;
  shot("s8-quickask", rect ?? undefined, 70);
  const dump = atspi("dump", "Nugget Quick Ask");
  const texts = Array.isArray(dump) ? [...new Set(dump.map((d) => d.name || d.text).filter(Boolean))] : dump;
  const blob = JSON.stringify(texts);
  const req = mock.streams()[n0];
  // Esc closes and returns focus.
  key("Escape");
  const closed = await waitFor("quick ask hidden", () => qaWins().length === 0, 4000).then(() => true, () => false);
  await sleep(400);
  const fgEsc = await linux.foreground();
  typeStr("q");
  await sleep(500);
  const typed = await readField();
  record("8a", "quick ask via Ctrl+Shift+Space: streams, cites, Esc closes + focus back", opened && closed && blob.includes("Lisbon") && typed === "Hello q", {
    items_indexed: st.items,
    opened,
    focused_on_open: fg1,
    provider_got_request: Boolean(req),
    request_mentions_file: req ? bodyText(req).includes("Lisbon") : null,
    answer_seen: blob.includes("Lisbon"),
    citation_ui: texts.filter?.((s) => /offsite|\[1\]|source|Open/i.test(s)).slice(0, 8),
    closed_on_esc: closed,
    foreground_after_esc: fgEsc,
    field_after_typing_q: typed,
  });

  // Blur: open, then click the fixture.
  ctrl("space", true);
  await waitFor("visible", () => qaWins().length > 0, 5000).catch(() => {});
  await sleep(800);
  const fid = xdo("search", "--name", "^Nugget IT - chat$").split("\n")[0];
  const fg = Object.fromEntries(xdo("getwindowgeometry", "--shell", fid).split("\n").map((l) => l.split("=")));
  await linux.click(+fg.X + 20, +fg.Y + 20);
  const blurHidden = await waitFor("hidden on blur", () => qaWins().length === 0, 3000).then(() => true, () => false);
  // Toggle twice quickly / open twice: no duplicates.
  ctrl("space", true);
  await sleep(700);
  const n1 = qaWins().length;
  const sp = spawnSync(APP, ["nugget://ask?q=second%20open"], { env: appEnv(), timeout: 15000 });
  await sleep(1500);
  const allQa = xdo("search", "--name", "^Nugget Quick Ask$").split("\n").filter(Boolean).length;
  const visQa = qaWins().length;
  const dump2 = atspi("dump", "Nugget Quick Ask");
  const pre = Array.isArray(dump2) ? dump2.filter((d) => /second open/.test(d.text ?? d.name ?? "")).length > 0 : dump2;
  record("8b", "quick ask: blur hides; second open does not duplicate", blurHidden && allQa === 1 && visQa === 1, {
    hidden_on_blur: blurHidden,
    visible_after_shortcut: n1,
    link_exit: sp.status,
    windows_titled_quick_ask: allQa,
    visible: visQa,
    prefilled_from_link: pre,
  });
  key("Escape");
  await sleep(500);
};


S.s9 = async () => {
  // Hostile: while the draft is ready, the user clicks the fixture's Search entry, then comes
  // back to the overlay and presses Enter. Where does the draft go? Is the overlay left up?
  await freshTarget();
  const draft = "Thanks, see you at noon in the lobby.";
  await fireAndDone({ text: draft, delayMs: 60 });
  const ent = atspi("ext", "gtk_test_app|python", "Search");
  await linux.click(ent.x + 30, ent.y + ent.h / 2);
  await sleep(1500);
  const stillUp = await overlayRect();
  const fgAfterClick = await linux.foreground();
  let entryText = null, comp = null, phase = null;
  if (stillUp) {
    await linux.click(stillUp.x + stillUp.w / 2, stillUp.y + 40);
    await sleep(400);
    const acc = Date.now();
    key("Return");
    phase = await waitFor("inserted/err", () => views(acc).find((v) => ["inserted", "error"].includes(v.phase)), 8000).catch(() => null);
    await sleep(800);
  }
  entryText = atspi("ext", "gtk_test_app|python", "Search")?.text ?? "";
  comp = await readField();
  record("9a", "click away to another field, back to overlay, Enter", !entryText.includes(draft) && (!stillUp || comp.includes(draft)), {
    overlay_visible_after_click_away: Boolean(stillUp), foreground_after_click: fgAfterClick, phase: phase?.phase, method: phase?.method, composer: comp, search_entry: entryText,
  });
  await settle();
  // Trigger fired twice quickly (double-double tap): one overlay, one request.
  await freshTarget();
  mock.next = { text: "Only one draft please.", delayMs: 200 };
  const n0 = mock.streams().length;
  const t = Date.now();
  await linux.trigger();
  await linux.trigger();
  await waitPhase(t, "done").catch(() => {});
  await sleep(800);
  const trig = since(t).filter((l) => l.event === "trigger").length;
  const wins = (await linux.overlayWindows()).length;
  record("9b", "trigger pressed twice fast: one overlay, one request", mock.streams().length - n0 === 1 && wins === 1, { triggers: trig, requests: mock.streams().length - n0, overlay_windows: wins });
  key("Escape");
  await hiddenSoon();
  // Trigger again while the overlay is already open (done): what happens?
  await freshTarget();
  await fireAndDone({ text: "First session draft.", delayMs: 50 });
  const n1 = mock.streams().length;
  const t2 = Date.now();
  await linux.activate(target);
  await sleep(300);
  await linux.trigger();
  await sleep(2500);
  const vs = views(t2).map((v) => `${v.phase}:${v.text.slice(0, 20)}`);
  const up = await overlayRect();
  const fg = await linux.foreground();
  key("Escape");
  const hidAfterEsc = await hiddenSoon();
  const t3 = Date.now();
  await linux.trigger();
  await sleep(2000);
  const vs3 = views(t3).map((v) => v.phase);
  const trig3 = since(t3).filter((l) => l.event === "trigger").length;
  shot("s9c-stuck", undefined, 25);
  record("9c", "trigger again while overlay is open (target refocused)", hidAfterEsc || vs3.length > 0, { second_trigger_new_requests: mock.streams().length - n1, second_trigger_views: vs.slice(0, 8), overlay_visible: Boolean(up), foreground: fg, esc_in_target_hides_overlay: hidAfterEsc, third_trigger_fired: trig3, third_trigger_views: vs3, overlay_still_visible: Boolean(await overlayRect()) });
  await settle();
};


S.s10 = async () => {
  // Real one-line replies that start like a preamble and mention a meta word (post.rs).
  const rows = [];
  for (const text of ["Sure, I'll email you the slides tomorrow.", "Okay, I will reply to Sam today.", "Great, the new version looks good to me!", "Here is the address: 12 Elm Street."]) {
    await freshTarget();
    const t = await fireAndDone({ text, delayMs: 60, chunks: 3 });
    const done = views(t).find((v) => v.phase === "done");
    const acc = Date.now();
    key("Return");
    await sleep(1200);
    const after = views(acc).map((v) => v.phase);
    const up = await overlayRect();
    if (rows.length === 0) shot("s10-empty-draft", up ?? undefined, 70);
    rows.push({ provider_sent: text, overlay_done_text: done?.text, enter_phases: after, overlay_still_up_after_enter: Boolean(up), field: await readField() });
    key("Escape");
    await hiddenSoon();
  }
  record("10", "short real replies survive the preamble filter", rows.every((r) => r.overlay_done_text === r.provider_sent), rows);
};


const mainDump = () => {
  const d = atspi("dump", "^Nugget$");
  return Array.isArray(d) ? d : [];
};
async function clickNamed(name, role) {
  const n = await waitFor(`main window node ${name}`, () => mainDump().find((d) => (d.name === name || d.text === name) && (!role || d.role === role) && d.showing && d.w > 0 && d.y > 0 && d.y < 1060), 15000, 500);
  await linux.click(n.x + n.w / 2, n.y + n.h / 2);
  await sleep(700);
  return n;
}
S.s7 = async () => {
  await freshTarget();
  const mid = xdo("search", "--name", "^Nugget$").split("\n")[0];
  xdo("windowactivate", "--sync", mid);
  await sleep(800);
  if (mainDump().some((d) => d.name === "Skip setup")) await clickNamed("Skip setup", "push button");
  await sleep(1500);
  const nav = mainDump().filter((d) => d.name === "Double Tap" && d.showing).map((d) => d.role);
  await clickNamed("Double Tap", nav.includes("link") ? "link" : undefined);
  await sleep(1500);
  await scrollTo("Your reply (practice field)", "entry");
  fs.writeFileSync(path.join(out, "s7-main.json"), JSON.stringify(mainDump().filter((d) => /practice|Try|reply/i.test(d.name || d.text || "")).map((d) => [d.role, (d.name || d.text || "").slice(0, 50), d.showing, d.x, d.y, d.w, d.h])));
  const field = await clickNamed("Your reply (practice field)", "entry");
  await sleep(500);
  const draft = "Practice reply: Friday morning works for me.";
  const practiceDraft = "Friday morning works for me, see you then.";
  mock.next = { text: practiceDraft, delayMs: 80 };
  const n0 = mock.streams().length;
  const t = await fireAndDone();
  const pr = since(t).some((l) => l.event === "practice");
  const req = mock.streams()[n0];
  const body = req ? bodyText(req) : "";
  const acc = Date.now();
  key("Return");
  await waitPhase(acc, "inserted").catch(() => {});
  await sleep(1000);
  const pf = mainDump().find((d) => d.name === "Your reply (practice field)" && d.role !== "label");
  const comp = await readField();
  shot("s7-practice", { x: field.x - 20, y: field.y - 120, w: Math.min(700, field.w + 40), h: field.h + 200 }, 60);
  // History must not have it: trigger in the fixture, press H.
  await linux.activate(target);
  await sleep(500);
  await fireAndDone({ text: "Fixture draft after practice.", delayMs: 50 });
  key("h");
  await sleep(800);
  const dump = atspi("dump", "Nugget Double Tap");
  const blob = JSON.stringify(dump);
  const histVisible = blob.includes("Previous drafts");
  key("Escape");
  await sleep(200);
  key("Escape");
  await hiddenSoon();
  record("7", "practice field: sandbox draft, nothing outside, not in history", pr && pf?.text?.includes(practiceDraft) && comp === "Hello " && !blob.includes(practiceDraft.slice(0, 20)), {
    probe_practice_event: pr, request_has_sample: body.includes("Thursday") || body.includes("Friday morning instead"), practice_field_text: pf?.text, fixture_composer: comp,
    history_toggle_seen: histVisible, practice_in_overlay_history: blob.includes(practiceDraft.slice(0, 20)), nav_roles: nav,
  });
};


async function scrollTo(name, role) {
  xte("mousemove 1000 600");
  for (let i = 0; i < 40; i++) {
    const all = mainDump().filter((d) => d.name === name && (!role || d.role === role) && d.w > 0);
    const n = all.find((d) => d.y > 150 && d.y < 900);
    if (n) return n;
    const up = all.length && all[0].y <= 150;
    xte(up ? "mouseclick 4" : "mouseclick 5", up ? "mouseclick 4" : "mouseclick 5");
    await sleep(250);
  }
  throw new Error(`${name} never scrolled into view`);
}
S.s11 = async () => {
  const mid = xdo("search", "--name", "^Nugget$").split("\n")[0];
  xdo("windowactivate", "--sync", mid);
  await sleep(800);
  if (mainDump().some((d) => d.name === "Skip setup")) await clickNamed("Skip setup", "push button");
  await sleep(1200);
  await clickNamed("Status", "push button");
  await sleep(1500);
  const name = "Open Nugget in the tray when I log in";
  const file = path.join(daemon.home, "xdg-config", "autostart", "dev.nugget.desktop.desktop");
  const before = fs.existsSync(file);
  let sw = await scrollTo(name);
  await linux.click(sw.x + 12, sw.y + sw.h / 2);
  const on = await waitFor("autostart file", () => fs.existsSync(file), 6000).then(() => true, () => false);
  const body = on ? fs.readFileSync(file, "utf8") : null;
  const exec = body?.match(/^Exec=(.*)$/m)?.[1];
  await sleep(800);
  sw = await scrollTo(name);
  await linux.click(sw.x + 12, sw.y + sw.h / 2);
  const off = await waitFor("autostart removed", () => !fs.existsSync(file), 6000).then(() => true, () => false);
  const settingsJson = fs.readdirSync(path.join(daemon.home, "xdg-config"), { recursive: true }).filter((f) => /settings/i.test(f));
  record("11", "launch at login toggle writes/removes the XDG autostart entry", !before && on && off && exec?.endsWith(" --minimized"), {
    file: file.replace(daemon.home, "$HOME"), existed_before: before, written: on, exec_line: exec, app_path: APP, removed: off, entry: body, settings_files: settingsJson,
  });
};


// ---- main ------------------------------------------------------------------------------------
const order = (args.only ?? "s1,s2,s3,s4,s6,s7,s8,s9,s10,s5").split(",");
try {
  await startStack();
  for (let rep = 0; rep < Number(args.repeat); rep++) {
    for (const s of order) {
      log(`== ${s} (rep ${rep + 1})`);
      try {
        await S[s]();
      } catch (e) {
        record(s, "scenario threw", false, { error: e.message, overlay: await overlayRect(), fg: await linux.foreground() });
        shot(`fail-${s}`, undefined, 30);
      }
      await settle();
    }
  }
} catch (e) {
  log(`setup failed: ${e.stack}`);
} finally {
  await stopStack();
  fs.writeFileSync(path.join(out, "results.json"), JSON.stringify(results, null, 1));
  log(`done: ${results.filter((r) => r.ok).length}/${results.length} pass`);
  process.exit(0);
}
