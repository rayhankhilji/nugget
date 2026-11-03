// The shell's cloud half for the bridge: spawns the `e2e_shell` example of nugget-cloud-client
// (the real client and the shell's own command code, session in memory) and talks to it in JSON
// lines. See crates/nugget-cloud-client/examples/e2e_shell.rs.
import { spawn } from "node:child_process";
import readline from "node:readline";

export class CloudShell {
  /** `bin`: the built e2e_shell; `env`: NUGGET_SUPABASE_URL, NUGGET_SUPABASE_ANON_KEY, NUGGET_CLOUD_URL. */
  constructor(bin, env) {
    this.proc = spawn(bin, [], { env: { ...process.env, ...env }, stdio: ["pipe", "pipe", "inherit"] });
    this.pending = new Map();
    this.nextId = 1;
    readline.createInterface({ input: this.proc.stdout }).on("line", (line) => {
      let msg;
      try {
        msg = JSON.parse(line);
      } catch {
        return;
      }
      const p = this.pending.get(msg.id);
      if (p) {
        this.pending.delete(msg.id);
        p(msg);
      }
    });
    this.proc.on("exit", (code) => {
      for (const p of this.pending.values()) p({ err: { kind: "spawn", message: `cloud shell exited (${code})` }, effects: [] });
      this.pending.clear();
    });
  }

  /** `{ ok } | { err }`, plus `effects`. */
  call(cmd, args = {}, timeoutMs = 90_000) {
    const id = this.nextId++;
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        resolve({ err: { kind: "other", message: `${cmd}: no answer from the cloud shell in ${timeoutMs} ms` }, effects: [] });
      }, timeoutMs);
      this.pending.set(id, (m) => (clearTimeout(timer), resolve(m)));
      this.proc.stdin.write(JSON.stringify({ id, cmd, args }) + "\n");
    });
  }

  close() {
    this.proc.kill();
  }
}
