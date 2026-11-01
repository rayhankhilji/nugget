// Scenario 4: the home lives on a 30 MB tmpfs; index a corpus whose index does not fit.
// Needs root (mount). usage: node qa/GG6/a3-days/s4-disk-full.mjs [sizeMB=30]
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { SCRATCH, startAt, addFiles, waitIndexed, search, rpc, rpcRaw, log, sleep, now, du, grepLogs, writeJson } from "./lib.mjs";

const SIZE = Number(process.argv[2] || 30);
const root = path.join(SCRATCH, "s4");
const mnt = path.join(root, "mnt");
try { execFileSync("umount", [mnt]); } catch {}
fs.rmSync(root, { recursive: true, force: true });
fs.mkdirSync(mnt, { recursive: true });
execFileSync("mount", ["-t", "tmpfs", "-o", `size=${SIZE}m`, "tmpfs", mnt]);
const df = () => execFileSync("df", ["-k", mnt]).toString().trim().split("\n")[1].split(/\s+/).slice(1, 4).join("/");
// Corpus (on the normal disk): 4000 notes of ~16 KB of mostly unique words -> a big FTS index.
const corpus = path.join(root, "corpus");
let x = 7;
const w = () => { let s = ""; for (let i = 0; i < 6; i++) { x = (x * 1103515245 + 12345) & 0x7fffffff; s += "abcdefghijklmnopqrstuvwxyz"[x % 26]; } return s; };
for (let i = 0; i < 4000; i++) {
  const dir = path.join(corpus, `d${i % 40}`);
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, `n${i}.md`), `# Note ${i} dtok${i}\n\n` + Array.from({ length: 2300 }, w).join(" ") + "\n");
}
const R = { tmpfs_mb: SIZE, corpus_files: 4000, build: "debug", samples: [] };
const home = path.join(mnt, "home");
const d = await startAt({ home });
await addFiles(d.socket, [corpus]);
const t0 = now();
let lastItems = -1, flat = 0;
for (;;) {
  await sleep(1000);
  if (d.exited()) { R.daemon_exited = d.exited(); break; }
  let st, src, ing, se;
  try {
    st = await rpc(d.socket, "status", null, 10_000);
    src = (await rpc(d.socket, "sources.list", null, 10_000))[0];
    ing = await rpc(d.socket, "ingest.status", null, 10_000);
    se = await rpcRaw(d.socket, "search", { query: "dtok5", limit: 5 }, 10_000);
  } catch (e) { R.samples.push({ s: Math.round((now() - t0) / 1000), rpc_error: e.message }); continue; }
  const sm = { s: Math.round((now() - t0) / 1000), df: df(), items: st.items, state: src.state, last_error: src.last_error, queue: ing.queue_depth, search: se.error ? se.error.message : `${se.result.length} hits` };
  R.samples.push(sm);
  if (sm.s % 5 === 0) log(JSON.stringify(sm));
  flat = st.items === lastItems ? flat + 1 : 0;
  lastItems = st.items;
  if (st.items >= 4000 || flat >= 25 || now() - t0 > 300_000) break;
}
R.at_full = R.samples.at(-1);
R.log_errors = grepLogs(d.logDir, /"level":"(ERROR|WARN)"/).filter((l) => !/keychain|KEYCHAIN/.test(l)).map((l) => l.slice(0, 400)).slice(0, 15);
R.log_bytes = du(d.logDir);
R.stdout_tail = fs.existsSync(path.join(home, "stdout.log")) ? fs.readFileSync(path.join(home, "stdout.log"), "utf8").slice(-1500) : null;
// Other operations while full.
for (const [m, p] of [["status", {}], ["sources.sync", {}], ["knowledge.status", {}], ["proactive.refresh", {}], ["mcp.clients.create", { name: "x", scopes: { search: true } }]]) {
  try { const r = await rpcRaw(d.socket, m, p, 20_000); R[`while_full_${m}`] = r.error ? r.error : "ok"; } catch (e) { R[`while_full_${m}`] = e.message; }
}
// Free space: grow the tmpfs; does indexing resume by itself?
execFileSync("mount", ["-o", `remount,size=${SIZE * 20}m`, mnt]);
const t1 = now();
log("space freed", df());
const resumed = await waitIndexed(d.socket, { want: 4000, timeoutMs: 120_000 }).catch((e) => ({ err: e.message }));
R.after_free_no_action = { ...resumed, ms: now() - t1 };
if (resumed.items < 4000) {
  await rpc(d.socket, "sources.sync", {});
  R.after_free_manual_sync = await waitIndexed(d.socket, { want: 4000, timeoutMs: 240_000 });
}
R.stop = await d.stop();
// Restart: DB still opens, counts and search agree.
try {
  const d2 = await startAt({ home });
  const st = await rpc(d2.socket, "status");
  R.restart = { ready_ms: d2.readyMs, items: st.items, chunks: st.chunks, audit_chain_ok: st.audit_chain_ok, search_dtok3999: (await search(d2.socket, "dtok3999")).hits.length, search_dtok5: (await search(d2.socket, "dtok5")).hits.length };
  R.restart_settle = await waitIndexed(d2.socket, { want: 4000, timeoutMs: 120_000 });
  R.restart_errors = grepLogs(d2.logDir, /"level":"ERROR"/).map((l) => l.slice(0, 300)).slice(-5);
  await d2.stop();
} catch (e) { R.restart = { error: e.message }; }
R.samples = R.samples.filter((s, i, a) => i % 5 === 0 || i === a.length - 1 || s.rpc_error);
writeJson("s4-disk-full.json", R);
execFileSync("umount", [mnt]);
log("done", JSON.stringify(R.at_full), JSON.stringify(R.after_free_no_action), JSON.stringify(R.restart));
