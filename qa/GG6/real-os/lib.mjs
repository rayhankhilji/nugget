// GG6 real-OS QA harness: shared helpers (Node built-ins only, plus Playwright where a page is used).
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO = path.resolve(HERE, "../../..");
export const CORPUS_SRC = path.join(HERE, "corpus");
export const QUESTIONS = JSON.parse(fs.readFileSync(path.join(HERE, "questions.json"), "utf8"));
export const OUT = path.resolve(process.env.GG6_OUT || path.join(os.tmpdir(), "gg6-out"));
fs.mkdirSync(path.join(OUT, "shots"), { recursive: true });

const t0 = Date.now();
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const secs = (ms) => `${(ms / 1000).toFixed(1)}s`;
export function log(...a) {
  const line = `[gg6 ${secs(Date.now() - t0)}] ${a.join(" ")}`;
  console.log(line);
  fs.appendFileSync(path.join(OUT, "gg6.log"), line + "\n");
}

// ---- UI strings (en) ----
const EN = JSON.parse(fs.readFileSync(path.join(REPO, "apps/desktop/src/i18n/locales/en.json"), "utf8"));
const FLAT = {};
(function flat(o, p) {
  for (const [k, v] of Object.entries(o)) typeof v === "object" ? flat(v, p ? `${p}.${k}` : k) : (FLAT[p ? `${p}.${k}` : k] = v);
})(EN, "");
export const t = (key) => {
  if (!(key in FLAT)) throw new Error(`no UI string ${key}`);
  return FLAT[key];
};
export const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
export const exact = (key) => new RegExp(`^\\s*${escapeRe(t(key))}\\s*$`);
export const starts = (key) => new RegExp(`^\\s*${escapeRe(t(key))}`);

// ---- results ----
export const results = { started: new Date().toISOString(), os: `${process.platform} ${os.release()} ${process.arch}`, cpus: os.cpus().length, cpu_model: os.cpus()[0]?.model, ram_gb: Math.round(os.totalmem() / 2 ** 30), marks: {}, steps: [], checks: [], friction: [] };
export function save() {
  fs.writeFileSync(path.join(OUT, "results.json"), JSON.stringify(results, null, 2));
}
let lastMark = t0;
/** A timed step: `name` took the time since the previous mark. */
export function mark(name, extra = {}) {
  const now = Date.now();
  results.marks[name] = now - t0;
  results.steps.push({ name, at_ms: now - t0, took_ms: now - lastMark, ...extra });
  log(`step ${name}: +${secs(now - lastMark)} (at ${secs(now - t0)})`);
  lastMark = now;
  save();
}
export function resetClock() {
  lastMark = Date.now();
}
export async function check(name, fn) {
  const s = Date.now();
  try {
    const detail = await fn();
    results.checks.push({ name, ok: true, ms: Date.now() - s, detail: detail ?? null });
    log(`PASS ${name}`);
    return detail;
  } catch (e) {
    results.checks.push({ name, ok: false, ms: Date.now() - s, error: String(e?.stack || e).slice(0, 2000) });
    log(`FAIL ${name}: ${String(e?.message || e).slice(0, 500)}`);
    return undefined;
  } finally {
    save();
  }
}
/** A moment a new user would be confused or blocked, with evidence. */
export function friction(what, evidence = {}) {
  results.friction.push({ what, ...evidence });
  log(`FRICTION ${what}`);
  save();
}
export async function until(what, probe, timeoutMs = 20_000, pollMs = 250) {
  const deadline = Date.now() + timeoutMs;
  let last;
  for (;;) {
    try {
      const [ok, v] = await probe();
      last = v;
      if (ok) return v;
    } catch (e) {
      if (e?.fatal) throw e;
      last = String(e?.message || e);
    }
    if (Date.now() > deadline) throw new Error(`timed out after ${secs(timeoutMs)} waiting for ${what}; last saw ${JSON.stringify(last)?.slice(0, 800)}`);
    await sleep(pollMs);
  }
}

// ---- the corpus, copied to a user-like folder ----
export function copyCorpus(dest) {
  fs.mkdirSync(dest, { recursive: true });
  fs.cpSync(CORPUS_SRC, dest, { recursive: true });
  let n = 0;
  (function walk(d) {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) e.isDirectory() ? walk(path.join(d, e.name)) : n++;
  })(dest);
  return n;
}

// ---- processes ----
/** [{pid, name, rss_mb, cpu_s}] for processes whose name matches `re`. */
export function procs(re) {
  try {
    if (process.platform === "win32") {
      const out = execFileSync("powershell", ["-NoProfile", "-Command", "Get-Process | Select-Object Id,ProcessName,WorkingSet64,CPU | ConvertTo-Json -Compress"], { encoding: "utf8", maxBuffer: 1 << 24 });
      return JSON.parse(out)
        .filter((p) => re.test(p.ProcessName))
        .map((p) => ({ pid: p.Id, name: p.ProcessName, rss_mb: Math.round(p.WorkingSet64 / 2 ** 20), cpu_s: p.CPU ?? 0 }));
    }
    const out = execFileSync("ps", ["-axo", "pid=,rss=,time=,comm="], { encoding: "utf8", maxBuffer: 1 << 24 });
    return out
      .split("\n")
      .map((l) => l.trim().match(/^(\d+)\s+(\d+)\s+(\S+)\s+(.*)$/))
      .filter(Boolean)
      .map(([, pid, rss, time, comm]) => ({ pid: Number(pid), name: path.basename(comm), rss_mb: Math.round(Number(rss) / 1024), cpu_s: cpuSecs(time) }))
      .filter((p) => re.test(p.name));
  } catch (e) {
    log(`procs failed: ${e.message}`);
    return [];
  }
}
function cpuSecs(time) {
  // [[dd-]hh:]mm:ss(.cc)
  const [d, rest] = time.includes("-") ? time.split("-") : [0, time];
  const parts = rest.split(":").map(Number);
  while (parts.length < 3) parts.unshift(0);
  return Number(d) * 86400 + parts[0] * 3600 + parts[1] * 60 + parts[2];
}
export function killPid(pid, signal = "SIGKILL") {
  if (process.platform === "win32") execFileSync("taskkill", ["/F", "/PID", String(pid)]);
  else process.kill(pid, signal);
}

/** Directory size in bytes (follows nothing). */
export function du(dir) {
  let n = 0;
  if (!fs.existsSync(dir)) return 0;
  (function walk(d) {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.isFile()) n += fs.statSync(p).size;
    }
  })(dir);
  return n;
}

/** The daemon socket (or pipe) from the "nuggetd ready" log line under `logDir`, newest first. */
export function socketFromLogs(logDir) {
  if (!fs.existsSync(logDir)) return null;
  const files = fs.readdirSync(logDir).map((f) => path.join(logDir, f)).sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs);
  for (const f of files) {
    const lines = fs.readFileSync(f, "utf8").split("\n").reverse();
    for (const line of lines) {
      if (!line.includes("nuggetd ready")) continue;
      try {
        const s = JSON.parse(line)?.fields?.socket;
        if (s) return s;
      } catch {
        const m = line.match(/"socket":"((?:[^"\\]|\\.)+)"/);
        if (m) return JSON.parse(`"${m[1]}"`);
      }
    }
  }
  return null;
}

/** Default per-user dirs (directories::ProjectDirs for dev/Nugget/Nugget), as nugget-core resolves them. */
export function defaultDataDir() {
  const h = os.homedir();
  switch (process.platform) {
    case "win32":
      return path.join(process.env.LOCALAPPDATA || path.join(h, "AppData", "Local"), "Nugget", "Nugget", "data");
    case "darwin":
      return path.join(h, "Library", "Application Support", "dev.Nugget.Nugget");
    default:
      return path.join(process.env.XDG_DATA_HOME || path.join(h, ".local", "share"), "nugget");
  }
}
