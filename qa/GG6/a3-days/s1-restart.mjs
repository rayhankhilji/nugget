// Scenario 1: reboot (clean stop + restart) and crash recovery (SIGKILL mid-indexing).
// usage: node qa/GG6/a3-days/s1-restart.mjs [corpusCount=3000]
import fs from "node:fs";
import path from "node:path";
import { generate } from "./gen-corpus.mjs";
import { SCRATCH, BIN, startAt, addFiles, waitIndexed, search, rpc, log, sleep, now, du, MB, grepLogs, writeJson } from "./lib.mjs";

const N = Number(process.argv[2] || 3000);
const root = path.join(SCRATCH, "s1");
fs.rmSync(root, { recursive: true, force: true });
const corpus = path.join(root, "corpus");
generate(corpus, N);
const QUERIES = ["Harbor Ledger vendor contract", "security audit", "qtok17", "qtok2501", "Quartz Relay hiring plan", "Cora Lindqvist"];
const R = { corpus_files: N, bin: BIN.nuggetd, build: "debug" };
const snap = async (s) => {
  const out = {};
  for (const q of QUERIES) {
    const r = await search(s, q, 20);
    out[q] = { ms: r.ms, keys: r.hits.map((h) => h.item.key) };
  }
  return out;
};
const same = (a, b) => Object.fromEntries(Object.keys(a).map((q) => [q, JSON.stringify(a[q].keys) === JSON.stringify(b[q].keys)]));

// --- A: first index, clean stop, restart
const homeA = path.join(root, "homeA");
let d = await startAt({ home: homeA });
R.a_first_ready_ms = d.readyMs;
const src = await addFiles(d.socket, [corpus]);
let t0 = now();
const idx = await waitIndexed(d.socket, { want: N, onPoll: (l, ms) => ms % 5000 < 500 && log("A index", JSON.stringify(l)) });
R.a_first_index = { ...idx, wall_ms: now() - t0 };
log("A indexed", JSON.stringify(idx));
const before = await snap(d.socket);
R.a_status_before = await rpc(d.socket, "status");
R.a_sources_before = await rpc(d.socket, "sources.list");
const stopA = await d.stop();
R.a_clean_stop = stopA;
log("A stopped", JSON.stringify(stopA));
R.a_db_after_stop = { db: du(path.join(homeA, "data", "nugget.db")), wal: du(path.join(homeA, "data", "nugget.db-wal")) };

d = await startAt({ home: homeA });
R.a_restart_ready_ms = d.readyMs;
t0 = now();
// How fast is it settled again, and does it re-ingest anything?
const st0 = await rpc(d.socket, "status");
R.a_items_immediately_after_restart = st0.items;
const progress = [];
const conn = (await import("node:net")).connect(d.socket);
let buf = "";
conn.on("data", (c) => { buf += c; let i; while ((i = buf.indexOf("\n")) >= 0) { const l = buf.slice(0, i); buf = buf.slice(i + 1); try { const m = JSON.parse(l); if (m.method === "ingest.progress") progress.push({ ms: now() - t0, ...m.params }); } catch {} } });
conn.write(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "status", params: {} }) + "\n");
const idx2 = await waitIndexed(d.socket, { want: N });
R.a_restart_settle = idx2;
await sleep(3000);
conn.destroy();
R.a_restart_progress = { count: progress.length, first: progress.slice(0, 3), last: progress.slice(-3), max_done: Math.max(0, ...progress.map((p) => p.done || 0)) };
const after = await snap(d.socket);
R.a_search_same_after_restart = same(before, after);
R.a_search_latency = { before: Object.fromEntries(Object.entries(before).map(([q, v]) => [q, v.ms])), after: Object.fromEntries(Object.entries(after).map(([q, v]) => [q, v.ms])) };
R.a_status_after = await rpc(d.socket, "status");
R.a_sources_after = await rpc(d.socket, "sources.list");
// Upserts/re-extract log lines after restart?
R.a_log_restart_reindex = grepLogs(d.logDir, /reindex|re-index|resend|root list changed/i).slice(0, 5);
await d.stop();

// --- B: SIGKILL mid-indexing, restart, check duplicates / stuck counts
const homeB = path.join(root, "homeB");
d = await startAt({ home: homeB });
await addFiles(d.socket, [corpus]);
let killedAt = null;
for (;;) {
  const st = await rpc(d.socket, "status");
  if (st.items >= N * 0.3) { killedAt = st.items; break; }
  await sleep(100);
}
R.b_killed_at_items = killedAt;
R.b_ingest_before_kill = await rpc(d.socket, "ingest.status");
d.kill("SIGKILL");
await d.waitExit();
log("B killed at", killedAt);
R.b_files_after_kill = fs.readdirSync(path.join(homeB, "data")).map((f) => `${f}:${du(path.join(homeB, "data", f))}`);
d = await startAt({ home: homeB });
R.b_restart_ready_ms = d.readyMs;
const st1 = await rpc(d.socket, "status");
R.b_items_on_restart = st1.items;
R.b_sources_on_restart = await rpc(d.socket, "sources.list");
t0 = now();
const idx3 = await waitIndexed(d.socket, { want: N, timeoutMs: 600_000 });
R.b_resume = idx3;
log("B resumed", JSON.stringify(idx3));
R.b_knowledge = await rpc(d.socket, "knowledge.status");
R.b_status = await rpc(d.socket, "status");
// Duplicates: each qtok must hit exactly one item.
let dup = 0, missing = 0;
const sample = Array.from({ length: 60 }, (_, i) => Math.floor((i * N) / 60));
for (const i of sample) {
  const r = await search(d.socket, `qtok${i}`, 10);
  const keys = new Set(r.hits.filter((h) => (h.snippet || "").includes(`qtok${i}`) || true).map((h) => h.item.key));
  const exact = r.hits.filter((h) => h.item.key.includes(`-${i}`));
  if (exact.length === 0) missing++;
  if (new Set(exact.map((h) => h.item.key)).size !== exact.length) dup++;
}
R.b_dup_sample = { sampled: sample.length, missing, duplicate_keys: dup };
R.b_items_vs_files = { items: R.b_status.items, files: N };
R.b_log_errors = grepLogs(d.logDir, /"level":"(ERROR|WARN)"/).slice(0, 20);
await d.stop();
writeJson("s1-restart.json", R);
log("done", JSON.stringify({ first: R.a_first_index, restart: R.a_restart_settle, same: R.a_search_same_after_restart, b: R.b_dup_sample, items: R.b_items_vs_files }));
