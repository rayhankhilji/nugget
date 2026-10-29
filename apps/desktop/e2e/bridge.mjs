// Stands in for the Tauri shell: serves the built frontend (dist/) and answers `POST /invoke`
// `{ cmd, args }` the way `src-tauri/src/commands.rs` does. Daemon commands are forwarded to
// nuggetd over its Unix socket (`knowledge_status` -> `knowledge.status`); commands the real shell
// answers itself (windows, pickers, chat history, updates, Double Tap) get fixed stubs.
//
// Replies are `{ ok: value }` or `{ err: { kind, message, code? } }`, the error shape the shell
// rejects with (CmdError in src-tauri/src/daemon.rs). See tauri-shim.js for the browser side.
//
// Standalone: node e2e/bridge.mjs <socket> [port]   (serves apps/desktop/dist with the shim inlined)
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { rpcRaw } from "./daemon.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
export const DIST = path.resolve(here, "..", "dist");
const SHIM = path.join(here, "tauri-shim.js");

/** JSON-RPC "method not found": the daemon has no such method. */
export const METHOD_NOT_FOUND = -32601;

const updateStatus = {
  current_version: "0.0.1",
  configured: false,
  settings: { channel: "off", auto_install: false, start_at_login: false },
  available: null,
  staged: false,
  busy: false,
  last_checked: null,
  last_error: null,
};
const chord = { kind: "chord", chord: "Ctrl+Alt+Space" };
const doubletapSettings = { enabled: false, trigger: chord, app_providers: {}, excluded_apps: [], allow_terminals: false };
const doubletapStatus = {
  platform: "linux",
  session: "x11",
  listening: false,
  trigger_error: null,
  accessibility: "not_required",
  input_monitoring: "not_required",
  hint: null,
  modifiers: [],
  recommended_trigger: chord,
};

/** Every command the shell handles without the daemon (`generate_handler!` in src-tauri/src/main.rs). */
function shellCommands(socket, { pickFolders = [] } = {}) {
  const conn = () => ({ connected: true, socket, error: null });
  return {
    connection_info: conn,
    start_daemon: conn,
    open_url: () => ({ err: { kind: "other", message: "opening links is not available in the e2e bridge" } }),
    open_mailto: () => null,
    pick_folders: (a) => (typeof pickFolders === "function" ? pickFolders(a) : pickFolders),
    pick_plugin_file: () => null,
    notify: () => null,
    chat_history_load: () => [],
    chat_history_save: () => null,
    chat_history_delete: () => null,
    chat_history_clear: () => null,
    updates_status: () => updateStatus,
    updates_set_settings: () => updateStatus,
    updates_check: () => updateStatus,
    updates_install: () => updateStatus,
    login_set: () => updateStatus,
    autostart_get: () => ({ supported: true, enabled: false }),
    autostart_set: (a) => ({ supported: true, enabled: a?.enabled === true }),
    doubletap_settings_get: () => doubletapSettings,
    doubletap_settings_set: (a) => a?.settings ?? doubletapSettings,
    doubletap_status: () => doubletapStatus,
    doubletap_request_permissions: () => doubletapStatus,
    doubletap_default_excluded: () => [],
    doubletap_fire: () => null,
    overlay_state: () => ({ phase: "done", text: "", provider: null, locality: null, citations: 0, excluded_sensitive: 0, message: null, verified: null, method: null }),
    overlay_accept: () => null,
    overlay_cancel: () => null,
  };
}

/** Commands whose shell proxy sends `{}` rather than `null` when there are no params. */
const EMPTY_OBJECT_PARAMS = new Set(["brief_get", "proactive_metrics", "proactive_refresh", "reminders_list"]);

const snake = (k) => k.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);

/** The daemon params for a command's invoke args (Tauri passes args camelCased). */
export function paramsFor(cmd, args) {
  const a = Object.fromEntries(Object.entries(args ?? {}).filter(([k]) => !k.startsWith("__")));
  const keys = Object.keys(a);
  if (keys.length === 1 && keys[0] === "request") return a.request;
  if (keys.length === 0) return EMPTY_OBJECT_PARAMS.has(cmd) ? {} : null;
  return Object.fromEntries(keys.map((k) => [snake(k), a[k]]));
}

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
  ".ttf": "font/ttf",
  ".wasm": "application/wasm",
};

function serveStatic(dist, req, res, shim) {
  const url = new URL(req.url, "http://localhost");
  const rel = decodeURIComponent(url.pathname).replace(/^\/+/, "") || "index.html";
  const file = path.resolve(dist, rel);
  if (!file.startsWith(dist + path.sep) || !fs.existsSync(file) || !fs.statSync(file).isFile()) {
    res.writeHead(404, { "Content-Type": "text/plain" }).end("not found");
    return;
  }
  if (shim && rel === "index.html") {
    const html = fs.readFileSync(file, "utf8").replace("<head>", `<head><script>${fs.readFileSync(SHIM, "utf8")}</script>`);
    res.writeHead(200, { "Content-Type": TYPES[".html"], "Cache-Control": "no-store" }).end(html);
    return;
  }
  res.writeHead(200, { "Content-Type": TYPES[path.extname(file)] ?? "application/octet-stream", "Cache-Control": "no-store" });
  fs.createReadStream(file).pipe(res);
}

/**
 * Start the bridge on 127.0.0.1 (`port` 0 picks a free one). `calls` records every command with
 * its outcome: `{ cmd, via: "shell" | "daemon", ok, code?, message? }`. With `shim`, index.html is
 * served with tauri-shim.js inlined, so any browser can open the app (the test injects it itself).
 * `pickFolders` is what the native folder picker "chooses": an array of paths, or a function of
 * the invoke args returning one (default: the user cancels, `[]`).
 */
export function startBridge({ socket, dist = DIST, port = 0, shim = false, pickFolders = [] }) {
  const shell = shellCommands(socket, { pickFolders });
  const calls = [];

  async function invoke(cmd, args) {
    if (Object.hasOwn(shell, cmd)) {
      const v = shell[cmd](args);
      if (v && typeof v === "object" && "err" in v && Object.keys(v).length === 1) {
        calls.push({ cmd, via: "shell", ok: false, message: v.err.message });
        return v;
      }
      calls.push({ cmd, via: "shell", ok: true });
      return { ok: v ?? null };
    }
    const method = cmd.replaceAll("_", ".");
    let msg;
    try {
      msg = await rpcRaw(socket, method, paramsFor(cmd, args));
    } catch (e) {
      calls.push({ cmd, via: "daemon", ok: false, message: e.message });
      return { err: { kind: "disconnected", message: "lost the connection to Nugget" } };
    }
    if (msg.error) {
      const { code, message } = msg.error;
      calls.push({ cmd, via: "daemon", ok: false, code, message });
      return { err: { kind: "rpc", code, message } };
    }
    calls.push({ cmd, via: "daemon", ok: true });
    return { ok: msg.result ?? null };
  }

  const server = http.createServer((req, res) => {
    if (req.method === "POST" && req.url === "/invoke") {
      let body = "";
      req.setEncoding("utf8");
      req.on("data", (c) => (body += c));
      req.on("end", async () => {
        let out;
        try {
          const { cmd, args } = JSON.parse(body);
          out = await invoke(String(cmd), args);
        } catch (e) {
          out = { err: { kind: "other", message: `bridge: ${e.message}` } };
        }
        const data = JSON.stringify(out);
        res.writeHead(200, { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(data) }).end(data);
      });
      return;
    }
    if (req.method === "GET" || req.method === "HEAD") return serveStatic(dist, req, res, shim);
    res.writeHead(405).end();
  });

  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", () => {
      const { port: actual } = server.address();
      resolve({
        url: `http://127.0.0.1:${actual}/`,
        calls,
        close: () => new Promise((r) => server.close(() => r())),
      });
    });
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [socket, port] = process.argv.slice(2);
  if (!socket) {
    console.error("usage: node e2e/bridge.mjs <nuggetd socket> [port]");
    process.exit(2);
  }
  const b = await startBridge({ socket, port: Number(port ?? 0), shim: true });
  console.log(`open ${b.url}index.html (talking to ${socket})`);
}
