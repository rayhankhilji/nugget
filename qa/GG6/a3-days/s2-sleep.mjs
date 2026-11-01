// Scenario 2: "sleep" = SIGSTOP the daemon for N s, change files meanwhile, SIGCONT.
// usage: node qa/GG6/a3-days/s2-sleep.mjs [stopSecs=180]
import fs from "node:fs";
import path from "node:path";
import { generate } from "./gen-corpus.mjs";
import { SCRATCH, startAt, addFiles, waitIndexed, search, rpc, log, sleep, now, procStat, grepLogs, writeJson } from "./lib.mjs";

const SECS = Number(process.argv[2] || 180);
const root = path.join(SCRATCH, "s2");
fs.rmSync(root, { recursive: true, force: true });
const corpus = path.join(root, "corpus");
generate(corpus, 500);
const d = await startAt({ home: path.join(root, "home") });
await addFiles(d.socket, [corpus]);
await waitIndexed(d.socket, { want: 500 });
const R = { stop_secs: SECS, build: "debug" };
d.kill("SIGSTOP");
log("stopped");
await sleep(5000);
// Changes while asleep: new file, modified file, deleted file, new folder with files.
fs.writeFileSync(path.join(corpus, "notes", "asleep-new.md"), "# Written while asleep\n\nzzqnewword marker.\n");
const mod = fs.readdirSync(path.join(corpus, "mail"))[0];
fs.appendFileSync(path.join(corpus, "mail", mod), "\r\nAppended zzqmodword.\r\n");
const del = path.join(corpus, "mail", fs.readdirSync(path.join(corpus, "mail"))[1]);
const delTok = /qtok\d+/.exec(fs.readFileSync(del, "utf8"))[0];
fs.rmSync(del);
fs.mkdirSync(path.join(corpus, "asleep-dir", "a", "b"), { recursive: true });
for (let i = 0; i < 20; i++) fs.writeFileSync(path.join(corpus, "asleep-dir", "a", "b", `n${i}.md`), `zzqdirword ${i}\n`);
// A client calls while the daemon is stopped: does it hang?
const t1 = now();
const during = rpc(d.socket, "status", null, (SECS + 30) * 1000).then((r) => ({ ok: true, ms: now() - t1 }), (e) => ({ err: e.message, ms: now() - t1 }));
await sleep(SECS * 1000 - 5000);
const t = now();
d.kill("SIGCONT");
log("continued");
R.client_call_while_stopped = await during;
R.status_after_cont_ms = await rpc(d.socket, "status").then(() => now() - t);
const want = { zzqnewword: 1, zzqmodword: 1, zzqdirword: 20, [delTok]: 0 };
const seen = {};
for (let i = 0; i < 600 && Object.keys(seen).length < 4; i++) {
  for (const [q, n] of Object.entries(want)) {
    if (seen[q] !== undefined) continue;
    const r = await search(d.socket, q, 50);
    const hits = q === delTok ? r.hits.filter((h) => (h.snippet || "").includes(q)).length : r.hits.length;
    if (hits === n) seen[q] = now() - t;
  }
  await sleep(250);
}
R.caught_up_ms = seen;
R.status = await rpc(d.socket, "status");
R.ingest = await rpc(d.socket, "ingest.status");
R.cpu_after = procStat(d.pid);
R.log_warn = grepLogs(d.logDir, /"level":"(ERROR|WARN)"/).filter((l) => !/KEYCHAIN|keychain/.test(l)).map((l) => l.slice(0, 300));
await d.stop();
writeJson("s2-sleep.json", R);
log(JSON.stringify(R.caught_up_ms), JSON.stringify(R.client_call_while_stopped));
