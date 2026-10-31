// Scenario 7: idle daemon with a mid-size index left running; sample /proc every 30 s.
// usage: node qa/GG6/a3-days/s7-soak.mjs [minutes=45] [files=2000]
import fs from "node:fs";
import path from "node:path";
import { generate } from "./gen-corpus.mjs";
import { SCRATCH, HERE, startAt, addFiles, waitIndexed, rpc, procStat, children, log, sleep, now, du, fileSize, grepLogs, writeJson } from "./lib.mjs";

const MIN = Number(process.argv[2] || 45), N = Number(process.argv[3] || 2000);
const root = path.join(SCRATCH, "s7");
fs.rmSync(root, { recursive: true, force: true });
const corpus = path.join(root, "corpus");
generate(corpus, N);
const home = path.join(root, "home");
const d = await startAt({ home });
await addFiles(d.socket, [corpus]);
const idx = await waitIndexed(d.socket, { want: N });
log("indexed", JSON.stringify(idx));
const csv = path.join(HERE, "results", "s7-soak.csv");
fs.writeFileSync(csv, "t_s,cpu_pct,rss_kb,threads,fds,log_bytes,db_bytes,wal_bytes,items,status_ms\n");
const dataDir = path.join(home, "data");
let prev = procStat(d.pid), tPrev = now();
const t0 = now();
const ingestLines0 = grepLogs(d.logDir, /./).length;
while (now() - t0 < MIN * 60_000) {
  await sleep(30_000);
  const s = procStat(d.pid);
  if (!s) { log("daemon gone", JSON.stringify(d.exited())); break; }
  const dt = (now() - tPrev) / 1000;
  const cpu = ((s.ticks - prev.ticks) / 100 / dt) * 100;
  prev = s; tPrev = now();
  const ts = now();
  const st = await rpc(d.socket, "status");
  const sms = now() - ts;
  const row = [Math.round((now() - t0) / 1000), cpu.toFixed(2), s.rss, s.threads, s.fds, du(d.logDir), fileSize(path.join(dataDir, "nugget.db")), fileSize(path.join(dataDir, "nugget.db-wal")), st.items, sms];
  fs.appendFileSync(csv, row.join(",") + "\n");
}
const R = { minutes: MIN, files: N, index: idx, build: "debug", children: children(d.pid) };
R.log_lines_during_soak = grepLogs(d.logDir, /./).length - ingestLines0;
// What did it log while idle (message histogram, after indexing)?
const lines = grepLogs(d.logDir, /./).slice(ingestLines0);
const hist = {};
for (const l of lines) { const m = l.match(/"message":"([^"]{0,80})/); const k = m ? m[1] : l.slice(0, 80); hist[k] = (hist[k] || 0) + 1; }
R.idle_log_histogram = Object.entries(hist).sort((a, b) => b[1] - a[1]).slice(0, 30);
R.knowledge = await rpc(d.socket, "knowledge.status");
R.ingest = await rpc(d.socket, "ingest.status");
R.stop = await d.stop();
writeJson("s7-soak.json", R);
log("soak done", JSON.stringify(R.stop));
