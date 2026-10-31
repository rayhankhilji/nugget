// Scenario 6: a big messy folder: ~20k text files (nested 15 deep), big binaries, a 500 MB .log,
// a 60 MB .txt (over the 50 MiB cap) and a 40 MB .txt (under it), a symlink loop, and files
// changing while indexing. Samples RSS/CPU, status progress and search latency every second.
// usage: node qa/GG6/a3-days/s6-big-folder.mjs [files=20000]
import fs from "node:fs";
import path from "node:path";
import net from "node:net";
import { execFileSync } from "node:child_process";
import { generate } from "./gen-corpus.mjs";
import { SCRATCH, HERE, startAt, addFiles, search, rpc, procStat, log, sleep, now, du, MB, grepLogs, writeJson } from "./lib.mjs";

const N = Number(process.argv[2] || 20000);
const root = path.join(SCRATCH, "s6");
fs.rmSync(root, { recursive: true, force: true });
const corpus = path.join(root, "corpus");
const tg = now();
generate(path.join(corpus, "flat"), Math.floor(N * 0.7));
// Deep nesting: 15 levels, the rest of the files spread over the levels.
let deep = path.join(corpus, "deep");
const rest = N - Math.floor(N * 0.7);
for (let lvl = 0; lvl < 15; lvl++) {
  deep = path.join(deep, `level${lvl}`);
  fs.mkdirSync(deep, { recursive: true });
  for (let i = 0; i < Math.ceil(rest / 15); i++) fs.writeFileSync(path.join(deep, `d${lvl}-${i}.txt`), `deep level ${lvl} file ${i} deeptok${lvl}x${i} ` + "lorem ipsum dolor ".repeat(40));
}
fs.writeFileSync(path.join(deep, "bottom.md"), "# Bottom\n\nbottomtok at depth 16\n");
fs.mkdirSync(path.join(corpus, "big"), { recursive: true });
for (let i = 0; i < 8; i++) execFileSync("fallocate", ["-l", "100M", path.join(corpus, "big", `blob${i}.bin`)]);
execFileSync("fallocate", ["-l", "100M", path.join(corpus, "big", "video.mp4")]);
// A 500 MB log (text), a 60 MB and a 40 MB text file.
const line = "2026-10-30T10:00:00Z INFO service handled request id=1234 path=/api/v1/items status=200 latency_ms=12\n";
const writeText = (p, mb, marker) => { const fd = fs.openSync(p, "w"); const chunk = Buffer.from(line.repeat(Math.ceil(1048576 / line.length))); fs.writeSync(fd, `${marker}\n`); for (let i = 0; i < mb; i++) fs.writeSync(fd, chunk); fs.closeSync(fd); };
writeText(path.join(corpus, "big", "server.log"), 500, "logtok500");
writeText(path.join(corpus, "big", "huge-export.txt"), 60, "hugetok60");
writeText(path.join(corpus, "big", "large-notes.txt"), 40, "largetok40");
// Symlink loop.
fs.mkdirSync(path.join(corpus, "loop"), { recursive: true });
fs.symlinkSync("..", path.join(corpus, "loop", "parent"));
fs.symlinkSync(path.join(corpus, "loop"), path.join(corpus, "loop", "self"));
const R = { files_text: N + 1, gen_ms: now() - tg, corpus_bytes: du(corpus), build: "debug" };
log("corpus", MB(R.corpus_bytes), "MB", R.gen_ms, "ms");

const home = path.join(root, "home");
const d = await startAt({ home });
// Broadcast progress notifications.
const prog = [];
const conn = net.connect(d.socket); let buf = ""; const t0 = now();
conn.setEncoding("utf8");
conn.on("data", (c) => { buf += c; let i; while ((i = buf.indexOf("\n")) >= 0) { const l = buf.slice(0, i); buf = buf.slice(i + 1); try { const m = JSON.parse(l); if (m.method === "ingest.progress") prog.push({ s: (now() - t0) / 1000, ...m.params }); } catch {} } });
conn.write(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "status", params: {} }) + "\n");
await addFiles(d.socket, [corpus]);
const csv = path.join(HERE, "results", "s6-big-folder.csv");
fs.writeFileSync(csv, "t_s,cpu_pct,rss_mb,items,chunks,queue,state,progress_phase,progress_done,progress_total,search_ms,db_mb,wal_mb\n");
let prev = procStat(d.pid), tp = now(), changed = false, stable = 0, lastKey = "", firstSearchable = null, peakRss = 0, maxSearch = 0;
const lat = [];
for (;;) {
  await sleep(2000);
  const s = procStat(d.pid);
  if (!s) { R.daemon_died = d.exited(); break; }
  const cpu = ((s.ticks - prev.ticks) / 100 / ((now() - tp) / 1000)) * 100; prev = s; tp = now();
  peakRss = Math.max(peakRss, s.rss);
  const st = await rpc(d.socket, "status", null, 60_000);
  const src = (await rpc(d.socket, "sources.list", null, 60_000))[0];
  const ing = await rpc(d.socket, "ingest.status", null, 60_000);
  const q = await search(d.socket, "qtok12345", 10);
  lat.push(q.ms); maxSearch = Math.max(maxSearch, q.ms);
  if (firstSearchable === null && q.hits.some((h) => h.item.key.includes("-12345"))) firstSearchable = (now() - t0) / 1000;
  const p = prog.at(-1) || {};
  const ts = Math.round((now() - t0) / 1000);
  fs.appendFileSync(csv, [ts, cpu.toFixed(0), (s.rss / 1024).toFixed(0), st.items, st.chunks, ing.queue_depth, src.state, p.phase, p.done, p.total, q.ms, MB(du(path.join(home, "data", "nugget.db"))), MB(du(path.join(home, "data", "nugget.db-wal")))].join(",") + "\n");
  if (ts % 20 < 2) log(ts, "s items", st.items, "state", src.state, "rss", (s.rss / 1024).toFixed(0), "cpu", cpu.toFixed(0), "search", q.ms, "ms", JSON.stringify(p));
  // Change files during indexing.
  if (!changed && st.items > N * 0.2) {
    changed = true;
    fs.writeFileSync(path.join(corpus, "flat", "notes", "changed-during.md"), "midtok added during indexing\n");
    fs.appendFileSync(path.join(corpus, "flat", "mail", "msg-6.eml"), "\r\nmidmodtok\r\n");
    fs.rmSync(path.join(corpus, "flat", "mail", "msg-7.eml"));
    fs.renameSync(path.join(corpus, "deep", "level0", "d0-0.txt"), path.join(corpus, "deep", "level0", "renamed-d0-0.txt"));
  }
  const key = `${st.items}/${ing.queue_depth}/${src.state}`;
  stable = key === lastKey && src.state !== "syncing" && ing.queue_depth === 0 ? stable + 1 : 0;
  lastKey = key;
  if (stable >= 3 || now() - t0 > 1_500_000) { R.settled = stable >= 3; break; }
}
R.time_to_settled_s = (now() - t0) / 1000;
R.first_searchable_qtok12345_s = firstSearchable;
R.peak_rss_mb = (peakRss / 1024).toFixed(0);
R.vm_hwm_mb = (procStat(d.pid).hwm / 1024).toFixed(0);
R.search_latency_ms = { max: maxSearch, p50: lat.sort((a, b) => a - b)[Math.floor(lat.length / 2)], p95: lat[Math.floor(lat.length * 0.95)] };
R.status = await rpc(d.socket, "status");
R.sources = await rpc(d.socket, "sources.list");
R.db_mb = MB(du(path.join(home, "data", "nugget.db"))); R.wal_mb = MB(du(path.join(home, "data", "nugget.db-wal"))); R.home_mb = MB(du(home));
R.cpu_total_s = procStat(d.pid).ticks / 100;
const has = async (q) => (await search(d.socket, q, 10)).hits.map((h) => h.item.key);
R.checks = { bottomtok: await has("bottomtok"), largetok40: await has("largetok40"), hugetok60: await has("hugetok60"), logtok500: await has("logtok500"), midtok: await has("midtok"), midmodtok: await has("midmodtok"), deeptok0x0: await has("deeptok0x0"), qtok7_deleted: (await has("qtok7")).filter((k) => k.includes("msg-7")) };
R.progress = { count: prog.length, phases: [...new Set(prog.map((p) => p.phase))], last: prog.slice(-3), totals: [...new Set(prog.map((p) => p.total))].slice(0, 5), sample: prog.filter((_, i) => i % Math.max(1, Math.floor(prog.length / 10)) === 0) };
R.log_warn = grepLogs(d.logDir, /"level":"(ERROR|WARN)"/).filter((l) => !/keychain|KEYCHAIN/.test(l)).map((l) => l.slice(0, 300));
R.log_warn_count = R.log_warn.length; R.log_warn = R.log_warn.slice(0, 12);
R.knowledge = await rpc(d.socket, "knowledge.status");
conn.destroy();
R.stop = await d.stop(30_000);
writeJson("s6-big-folder.json", R);
log("done", JSON.stringify({ settled: R.time_to_settled_s, peak: R.peak_rss_mb, db: R.db_mb, lat: R.search_latency_ms, checks: R.checks }));
