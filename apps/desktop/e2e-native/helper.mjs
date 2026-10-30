// A long-running helper process (windows-helper.ps1, the macOS Swift helper) driven over a line
// protocol: one command per line on stdin, one JSON object per line on stdout.
import { spawn } from "node:child_process";
import readline from "node:readline";

export function startHelper(cmd, args, { log = () => {} } = {}) {
  const proc = spawn(cmd, args, { stdio: ["pipe", "pipe", "pipe"] });
  const waiting = [];
  readline.createInterface({ input: proc.stdout }).on("line", (line) => {
    const w = waiting.shift();
    if (!w) return log(`helper: unexpected output ${line}`);
    try {
      w.resolve(JSON.parse(line));
    } catch {
      w.reject(new Error(`helper: unreadable reply ${line}`));
    }
  });
  readline.createInterface({ input: proc.stderr }).on("line", (l) => log(`helper stderr: ${l}`));
  proc.on("exit", (code) => {
    for (const w of waiting.splice(0)) w.reject(new Error(`helper exited (${code})`));
  });
  let queue = Promise.resolve();
  /** Send one command; resolves to the reply, throws when it says `ok: false`. */
  const call = (line, timeoutMs = 30_000) => {
    const run = () =>
      new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`helper: no reply to "${line}" within ${timeoutMs} ms`)), timeoutMs);
        waiting.push({
          resolve: (v) => {
            clearTimeout(timer);
            if (v.ok === false) reject(new Error(`helper: ${line.split(" ")[0]}: ${v.error}`));
            else resolve(v);
          },
          reject: (e) => {
            clearTimeout(timer);
            reject(e);
          },
        });
        proc.stdin.write(`${line}\n`);
      });
    // One command at a time, so replies match requests.
    const p = queue.then(run, run);
    queue = p.catch(() => {});
    return p;
  };
  return { call, stop: () => proc.kill() };
}
