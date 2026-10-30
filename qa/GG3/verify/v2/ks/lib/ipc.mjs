// A persistent newline-delimited JSON-RPC client over nuggetd's Unix socket. Requests on one
// connection are matched by id; notifications (agent.event, policy.kill.changed, browser.host.*)
// are timestamped and kept in `notes`, and handed to `onNote` listeners.
import net from "node:net";
import { performance } from "node:perf_hooks";

export const now = () => performance.now();

export class Ipc {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.waiting = new Map();
    this.notes = [];
    this.listeners = [];
    this.buf = "";
  }

  connect() {
    return new Promise((resolve, reject) => {
      this.conn = net.connect(this.socket);
      this.conn.setEncoding("utf8");
      this.conn.once("connect", resolve);
      this.conn.once("error", reject);
      this.conn.on("data", (chunk) => this.onData(chunk));
      this.conn.on("close", () => {
        for (const [, w] of this.waiting) w.reject(new Error("connection closed"));
        this.waiting.clear();
      });
    });
  }

  onData(chunk) {
    this.buf += chunk;
    let nl;
    while ((nl = this.buf.indexOf("\n")) >= 0) {
      const line = this.buf.slice(0, nl);
      this.buf = this.buf.slice(nl + 1);
      if (!line.trim()) continue;
      let msg;
      try {
        msg = JSON.parse(line);
      } catch {
        continue;
      }
      if (msg.id !== undefined && msg.id !== null && this.waiting.has(msg.id)) {
        const w = this.waiting.get(msg.id);
        this.waiting.delete(msg.id);
        w.resolve({ ...msg, t: now() });
      } else if (msg.method) {
        const note = { t: now(), method: msg.method, params: msg.params };
        this.notes.push(note);
        for (const l of this.listeners) l(note);
      }
    }
  }

  onNote(fn) {
    this.listeners.push(fn);
  }

  /** Send a request; resolves to the raw reply `{result|error, t}` (never throws on RPC error). */
  call(method, params = {}) {
    const id = this.nextId++;
    const t0 = now();
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve: (m) => resolve({ ...m, t0 }), reject });
      this.conn.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }

  /** Like `call` but returns `result` and throws on an RPC error. */
  async rpc(method, params = {}) {
    const r = await this.call(method, params);
    if (r.error) throw new Error(`${method}: ${r.error.message} (code ${r.error.code})`);
    return r.result;
  }

  /** Resolve with the first notification (already seen or future) matching `pred`. */
  waitNote(pred, timeoutMs = 15000, what = "notification") {
    const seen = this.notes.find(pred);
    if (seen) return Promise.resolve(seen);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`timeout waiting for ${what}`)), timeoutMs);
      this.listeners.push((n) => {
        if (pred(n)) {
          clearTimeout(timer);
          resolve(n);
        }
      });
    });
  }

  close() {
    this.conn?.destroy();
  }
}

/** Agent events of one task, in arrival order. */
export const agentEvents = (ipc, taskId) =>
  ipc.notes.filter((n) => n.method === "agent.event" && n.params?.task_id === taskId).map((n) => ({ t: n.t, ...n.params.event }));

export const isEvent = (taskId, kind) => (n) =>
  n.method === "agent.event" && n.params?.task_id === taskId && n.params?.event?.event === kind;
