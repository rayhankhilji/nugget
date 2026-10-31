// GG6 days 2-7 on a real OS with the installed (release) nuggetd and a real local model, over the
// daemon's IPC (Unix socket or Windows named pipe). Each scenario records numbers in results.days.
//
// `ctx` gives: socket() (current), restart() (stop + start the daemon the way the platform does,
// resolves when it answers), home (data dir root or null), dataDir.
import fs from "node:fs";
import path from "node:path";
import { rpc, rpcStream } from "../../../apps/desktop/e2e/daemon.mjs";
import { check, du, friction, killPid, log, procs, results, save, sleep } from "./lib.mjs";

const days = (results.days ??= {});
const LLAMA = /llama-server/i;
const NUGGETD = /^nuggetd(\.exe)?$/i;

/** One chat turn through the daemon (tier routing, like Chat). Returns timings and text. */
export async function ask(socket, text, timeoutMs = 300_000) {
  const t0 = Date.now();
  let first = null;
  try {
    const { result, notes } = await rpcStream(
      socket,
      "chat",
      { provider: null, tier: "synthesis", messages: [{ role: "user", text }], items: [] },
      (_r, notes) => {
        if (first === null && notes.some((n) => n.method === "chat.delta")) first = Date.now() - t0;
        return notes.some((n) => n.method === "chat.done");
      },
      timeoutMs,
    );
    const done = notes.find((n) => n.method === "chat.done")?.params ?? {};
    const out = notes.filter((n) => n.method === "chat.delta").map((n) => n.params.text).join("");
    return { ok: !done.error, provider: result?.provider, error: done.error ?? null, first_token_ms: first, total_ms: Date.now() - t0, text: out.slice(0, 400) };
  } catch (e) {
    return { ok: false, error: String(e.message || e).slice(0, 600), first_token_ms: first, total_ms: Date.now() - t0 };
  }
}

export async function runtimeStatus(socket) {
  return rpc(socket, "runtime.status", {}).catch((e) => ({ error: e.message }));
}

/** Day 2: the machine restarted. Same items, model still there, first answer after a cold start. */
export async function dayRestart(ctx) {
  await check("day 2: restart keeps the index and the model; first answer after a cold start", async () => {
    const before = await rpc(ctx.socket(), "status", {});
    const t0 = Date.now();
    await ctx.restart();
    const ready_ms = Date.now() - t0;
    const after = await rpc(ctx.socket(), "status", {});
    const rt = await runtimeStatus(ctx.socket());
    const a = await ask(ctx.socket(), "In one sentence: what is Project Harbor?");
    days.restart = { ready_ms, items_before: before.items, items_after: after.items, chunks_before: before.chunks, chunks_after: after.chunks, runtime_after: rt, first_answer: a };
    log(`restart: ready in ${ready_ms} ms; items ${before.items} -> ${after.items}; first answer ${JSON.stringify(a).slice(0, 300)}`);
    if (after.items !== before.items) throw new Error(`items changed across a restart: ${before.items} -> ${after.items}`);
    if (!a.ok) throw new Error(`no answer after restart: ${a.error}`);
    if (a.first_token_ms > 60_000) friction(`after a restart the first answer took ${Math.round(a.first_token_ms / 1000)} s to start (cold model load) with no warm-up`, { first_answer: a });
    return days.restart;
  });
}

/** The local model server crashes: idle, then mid-answer. How long until answers work again? */
export async function dayModelCrash(ctx) {
  await check("day 3: llama-server killed while idle recovers on the next question", async () => {
    await ask(ctx.socket(), "Say hello.");
    const ls = procs(LLAMA);
    if (!ls.length) throw new Error(`no llama-server process running (runtime: ${JSON.stringify(await runtimeStatus(ctx.socket()))})`);
    for (const p of ls) killPid(p.pid);
    const killedAt = Date.now();
    await sleep(2000);
    const rt = await runtimeStatus(ctx.socket());
    const a = await ask(ctx.socket(), "Who sponsors Project Harbor?");
    let attempts = 1;
    let b = a;
    while (!b.ok && attempts < 5) {
      await sleep(5000);
      b = await ask(ctx.socket(), "Who sponsors Project Harbor?");
      attempts++;
    }
    days.crash_idle = { killed: ls.map((p) => p.pid), runtime_after_kill: rt, first_try: a, attempts, recovered_ms: b.ok ? Date.now() - killedAt : null, last: b };
    log(`crash idle: first try ${JSON.stringify(a).slice(0, 300)}; attempts ${attempts}`);
    if (!a.ok) friction(`after the local model server crashed, the next question failed with: ${a.error}`, { runtime_after_kill: rt });
    if (!b.ok) throw new Error(`never recovered after ${attempts} questions: ${b.error}`);
    return days.crash_idle;
  });
  await check("day 3: llama-server killed mid-answer: a clear error, then recovery", async () => {
    const socket = ctx.socket();
    const pending = ask(socket, "Write a detailed 300-word summary of everything you know about planning a trip to Lisbon.", 300_000);
    // Kill once the answer has started streaming (or after 20 s).
    const deadline = Date.now() + 20_000;
    await sleep(3000);
    while (Date.now() < deadline && !procs(LLAMA).length) await sleep(500);
    const ls = procs(LLAMA);
    for (const p of ls) killPid(p.pid);
    const mid = await pending;
    const after = await ask(socket, "Who sponsors Project Harbor?");
    days.crash_mid = { killed: ls.map((p) => p.pid), mid, after };
    log(`crash mid: ${JSON.stringify(mid).slice(0, 300)} / after: ${JSON.stringify(after).slice(0, 200)}`);
    if (mid.ok) friction("the answer streaming when the model server died was reported as a success (truncated text, no error)", { mid });
    if (!after.ok) throw new Error(`no recovery after a mid-answer crash: ${after.error}`);
    return days.crash_mid;
  });
}

/** Laptop lid closed: SIGSTOP the daemon and model server for `secs`, then continue (Unix only). */
export async function daySleep(ctx, secs = 180) {
  if (process.platform === "win32") return;
  await check(`day 4: suspend for ${secs}s (SIGSTOP/SIGCONT), then search and answer`, async () => {
    const ps = [...procs(NUGGETD), ...procs(LLAMA)];
    for (const p of ps) process.kill(p.pid, "SIGSTOP");
    // A note written while "asleep" must be picked up afterwards.
    const note = path.join(ctx.folder, "notes", "written-while-asleep.md");
    fs.writeFileSync(note, "# Parking\n\nThe office car park code changed to 7316 on the first of November.\n");
    await sleep(secs * 1000);
    for (const p of ps) process.kill(p.pid, "SIGCONT");
    const t0 = Date.now();
    const st = await rpc(ctx.socket(), "status", {});
    const status_ms = Date.now() - t0;
    let found = null;
    const deadline = Date.now() + 120_000;
    while (Date.now() < deadline) {
      const r = await rpc(ctx.socket(), "search", { query: "car park code", limit: 5 }).catch((e) => ({ error: e.message }));
      if (JSON.stringify(r).includes("7316")) {
        found = Date.now() - t0;
        break;
      }
      await sleep(3000);
    }
    const a = await ask(ctx.socket(), "What is the new car park code?");
    days.sleep = { secs, status_ms, items: st.items, note_found_ms: found, answer: a };
    if (found === null) throw new Error("a note written while asleep was not searchable 2 minutes after resume");
    if (!a.ok) throw new Error(`no answer after resume: ${a.error}`);
    return days.sleep;
  });
}

/** Left running overnight: sample CPU and memory of nuggetd and llama-server. */
export async function daySoak(ctx, minutes = 40, everySecs = 30) {
  await check(`day 5: idle for ${minutes} min, CPU and memory sampled every ${everySecs}s`, async () => {
    const samples = [];
    const end = Date.now() + minutes * 60_000;
    let prev = null;
    while (Date.now() < end) {
      const now = Date.now();
      const ps = [...procs(NUGGETD), ...procs(LLAMA)];
      const s = { t_s: Math.round((now - (end - minutes * 60_000)) / 1000) };
      for (const p of ps) {
        const key = NUGGETD.test(p.name) ? "nuggetd" : "llama";
        s[`${key}_rss_mb`] = (s[`${key}_rss_mb`] ?? 0) + p.rss_mb;
        s[`${key}_cpu_s`] = (s[`${key}_cpu_s`] ?? 0) + p.cpu_s;
      }
      if (prev) {
        const dt = (now - prev.at) / 1000;
        s.nuggetd_cpu_pct = +(((s.nuggetd_cpu_s ?? 0) - (prev.nuggetd_cpu_s ?? 0)) / dt * 100).toFixed(2);
        s.llama_cpu_pct = +(((s.llama_cpu_s ?? 0) - (prev.llama_cpu_s ?? 0)) / dt * 100).toFixed(2);
      }
      if (ctx.dataDir) s.data_mb = Math.round(du(ctx.dataDir) / 2 ** 20);
      samples.push(s);
      prev = { ...s, at: now };
      await sleep(everySecs * 1000);
    }
    const csv = ["t_s,nuggetd_rss_mb,nuggetd_cpu_pct,llama_rss_mb,llama_cpu_pct,data_mb", ...samples.map((s) => [s.t_s, s.nuggetd_rss_mb ?? "", s.nuggetd_cpu_pct ?? "", s.llama_rss_mb ?? "", s.llama_cpu_pct ?? "", s.data_mb ?? ""].join(","))].join("\n");
    fs.writeFileSync(path.join(process.env.GG6_OUT || ".", "soak.csv"), csv + "\n");
    const pick = (k) => samples.map((s) => s[k]).filter((v) => typeof v === "number");
    const avg = (a) => (a.length ? +(a.reduce((x, y) => x + y, 0) / a.length).toFixed(2) : null);
    const n = pick("nuggetd_rss_mb");
    const l = pick("llama_rss_mb");
    days.soak = {
      minutes,
      samples: samples.length,
      nuggetd_rss_first_mb: n[0],
      nuggetd_rss_last_mb: n[n.length - 1],
      nuggetd_rss_max_mb: Math.max(...n),
      nuggetd_cpu_avg_pct: avg(pick("nuggetd_cpu_pct")),
      nuggetd_cpu_max_pct: Math.max(0, ...pick("nuggetd_cpu_pct")),
      llama_rss_first_mb: l[0] ?? 0,
      llama_rss_last_mb: l[l.length - 1] ?? 0,
      llama_cpu_avg_pct: avg(pick("llama_cpu_pct")),
      llama_running_at_end: procs(LLAMA).length > 0,
    };
    log(`soak: ${JSON.stringify(days.soak)}`);
    if (days.soak.llama_running_at_end && days.soak.llama_rss_last_mb > 1000) friction(`after ${minutes} idle minutes the local model server still holds ${days.soak.llama_rss_last_mb} MB of memory`, days.soak);
    if (days.soak.nuggetd_cpu_avg_pct > 2) friction(`the idle daemon averages ${days.soak.nuggetd_cpu_avg_pct}% CPU`, days.soak);
    return days.soak;
  });
}

/** A big folder added on day 6: many files, deep nesting, a big binary. */
export async function dayBigFolder(ctx, { files = 20000, timeoutMs = 60 * 60_000 } = {}) {
  await check(`day 6: add a folder of ${files} files; time to searchable, peak memory, search latency while indexing`, async () => {
    const root = path.join(ctx.workDir, "big-folder");
    const t0 = Date.now();
    makeBigFolder(root, files);
    const gen_ms = Date.now() - t0;
    const before = await rpc(ctx.socket(), "status", {});
    const add0 = Date.now();
    await rpc(ctx.socket(), "sources.add", { id: null, kind: "files", name: "Archive", settings: { roots: [root], include: [], exclude: [] } });
    let peak = 0;
    const lat = [];
    let st;
    let stable = 0;
    let last = -1;
    while (Date.now() - add0 < timeoutMs) {
      await sleep(5000);
      for (const p of procs(NUGGETD)) peak = Math.max(peak, p.rss_mb);
      const q0 = Date.now();
      await rpc(ctx.socket(), "search", { query: "dentist appointment", limit: 5 }, 60_000).catch(() => {});
      lat.push(Date.now() - q0);
      st = await rpc(ctx.socket(), "status", {});
      if (st.items === last && st.items >= before.items + files * 0.9) stable++;
      else stable = 0;
      last = st.items;
      if (stable >= 3) break;
    }
    const took = Date.now() - add0;
    lat.sort((a, b) => a - b);
    days.big_folder = { files, gen_ms, items_before: before.items, items_after: st?.items, index_ms: took, peak_rss_mb: peak, search_p50_ms: lat[Math.floor(lat.length / 2)], search_p95_ms: lat[Math.floor(lat.length * 0.95)], search_max_ms: lat[lat.length - 1], data_mb: ctx.dataDir ? Math.round(du(ctx.dataDir) / 2 ** 20) : null };
    log(`big folder: ${JSON.stringify(days.big_folder)}`);
    if (stable < 3) throw new Error(`not indexed within ${timeoutMs / 60000} min: ${st?.items} items`);
    return days.big_folder;
  });
}

function makeBigFolder(root, n) {
  const words = "budget meeting invoice harbor plan travel recipe garden school report draft review contract renewal quarterly migration warehouse schedule note idea".split(" ");
  let seed = 7;
  const rnd = () => ((seed = (seed * 1103515245 + 12345) % 2 ** 31) / 2 ** 31);
  for (let i = 0; i < n; i++) {
    const depth = 1 + (i % 12);
    const dir = path.join(root, ...Array.from({ length: depth }, (_, d) => `d${d}-${(i >> d) % 7}`));
    fs.mkdirSync(dir, { recursive: true });
    const body = Array.from({ length: 40 + Math.floor(rnd() * 200) }, () => words[Math.floor(rnd() * words.length)]).join(" ");
    fs.writeFileSync(path.join(dir, `file-${i}.${i % 5 === 0 ? "txt" : "md"}`), `# Archive note ${i}\n\n${body}\n`);
  }
  fs.writeFileSync(path.join(root, "huge.log"), Buffer.alloc(200 * 2 ** 20, "log line about the warehouse schedule\n"));
  fs.writeFileSync(path.join(root, "photo.bin"), Buffer.alloc(50 * 2 ** 20, 7));
}

export function saveDays() {
  save();
}
