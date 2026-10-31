#!/usr/bin/env node
// Fixture daemon for the MCP compatibility clients: start a real `nuggetd` in a throwaway home
// with the loopback HTTP listener on (a free port), index a generated corpus big enough that
// `resources/list` needs more than one page, register MCP clients through the daemon's own IPC
// (`mcp.clients.create`, exactly what the desktop app and `nuggetd mcp install` use), write what
// the clients need to a JSON file, run the given command with `NUGGET_MCP_COMPAT_FIXTURE` set to
// that file, then stop the daemon. Node built-ins only; loopback only.
//
//   node fixture.mjs [--json PATH] -- <command> [args...]
//
// Every client entry carries the setup snippets the daemon generated for it. The compat clients
// launch the stdio bridge from the `claude-desktop` snippet (the `mcpServers` JSON shared by
// Claude Desktop and Cursor) and connect over HTTP with the `generic-http` snippet's URL and
// header, so CI exercises the same command, args, env and header shape the docs tell users to
// paste (`docs/book/src/guide/mcp.md`, kept in step by `crates/nugget-mcp/tests/docs.rs`).
//
// `NUGGETD` points at a built nuggetd; otherwise `cargo build --locked -p nuggetd` runs first.
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { locateNuggetd, rpc, startDaemon } from "../../../apps/desktop/e2e/daemon.mjs";

/** Entities generated; `resources/list` pages hold 100, so this needs two pages. */
export const PROJECTS = 150;
/** People the projects link to (gives `get_neighbourhood` edges). */
const PEOPLE = ["Alice Smith", "Bob Jones", "Carmen Diaz"];
/** A phrase only one note contains, for search and get_context. */
export const NEEDLE = "Falcon telemetry rollback checklist";
/** Clients made for the compat runs: each leg gets its own, so per-client rate limits never mix. */
const CLIENTS = 12;

const CONFIG = `# Written by crates/nugget-mcp/compat/fixture.mjs.
[runtime]
enabled = false

[knowledge]
debounce_ms = 100
interval_minutes = 0

[mcp]
http = true
port = 0
`;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function writeCorpus(dir) {
  fs.mkdirSync(dir, { recursive: true });
  for (let i = 1; i <= PROJECTS; i++) {
    const n = String(i).padStart(3, "0");
    const person = PEOPLE[i % PEOPLE.length];
    fs.writeFileSync(
      path.join(dir, `Compat Project ${n}.md`),
      `---\ntype: project\nstatus: active\naliases: [Compat ${n}]\npeople: ["[[${person}]]"]\n---\n` +
        `# Compat Project ${n}\n\nProject ${n} is run with ${person}.\n`,
    );
  }
  fs.writeFileSync(
    path.join(dir, "falcon.md"),
    `# Falcon launch\n\nThe Falcon launch review is on Thursday. Bring the ${NEEDLE} and the dashboard.\n`,
  );
}

/** Until the graph holds at least `want` entities and nothing is pending. */
async function waitForEntities(socket, want, timeoutMs = 180_000) {
  const deadline = Date.now() + timeoutMs;
  let prev = "";
  for (;;) {
    const ks = await rpc(socket, "knowledge.status");
    const key = `${ks.entities}/${ks.pending_items}`;
    if (ks.entities >= want && ks.pending_items === 0 && key === prev) return ks;
    if (key !== prev) console.error(`fixture: indexing… entities ${ks.entities}, pending ${ks.pending_items}`);
    prev = key;
    if (Date.now() > deadline) throw new Error(`fixture: knowledge did not settle (want ${want} entities; last ${key})`);
    await sleep(500);
  }
}

/** The launch command and HTTP endpoint from a client's setup snippets. */
function connection(created) {
  const snip = (app) => {
    const s = created.setup.find((x) => x.app === app);
    if (!s) throw new Error(`fixture: the daemon gave no ${app} snippet`);
    return s.text;
  };
  const server = JSON.parse(snip("claude-desktop")).mcpServers.nugget;
  const http = snip("generic-http");
  const url = http.match(/^url: (\S+)$/m)?.[1];
  const header = http.match(/^header: ([^:]+): (.+)$/m);
  if (!url || !header) throw new Error(`fixture: unreadable generic-http snippet:\n${http}`);
  return {
    id: created.client.id,
    token: created.token,
    stdio: { command: server.command, args: server.args, env: server.env },
    http: { url, headers: { [header[1]]: header[2] } },
    setup: created.setup,
  };
}

/**
 * On failure: where the logs are, and their last lines (CI keeps nothing else). A failed local run
 * keeps its temp dir, including fixture.json with the clients' live tokens (they work only against
 * that throwaway daemon home, which is stopped); delete it when done.
 */
function printLogs(work, daemon) {
  const tail = (file, n = 60) => {
    try {
      const lines = fs.readFileSync(file, "utf8").trimEnd().split("\n");
      console.error(`--- last ${Math.min(n, lines.length)} lines of ${file}`);
      console.error(lines.slice(-n).join("\n"));
    } catch {
      // No such log.
    }
  };
  tail(path.join(work, "nuggetd.log"));
  try {
    for (const f of fs.readdirSync(daemon.logDir)) tail(path.join(daemon.logDir, f));
  } catch {
    // No log directory.
  }
  console.error(`fixture: logs kept in ${work} and ${daemon.logDir}`);
}

async function createClient(socket, name, tools = []) {
  const created = await rpc(socket, "mcp.clients.create", {
    name,
    scopes: { sources: null, tools, include_sensitive: false },
  });
  return connection(created);
}

function parseArgs(argv) {
  const sep = argv.indexOf("--");
  const own = sep >= 0 ? argv.slice(0, sep) : argv;
  const command = sep >= 0 ? argv.slice(sep + 1) : [];
  let json = null;
  for (let i = 0; i < own.length; i++) {
    if (own[i] === "--json") json = own[++i];
    else throw new Error(`usage: node fixture.mjs [--json PATH] -- <command> [args...]`);
  }
  if (command.length === 0) throw new Error("usage: node fixture.mjs [--json PATH] -- <command> [args...]");
  return { json, command };
}

async function main() {
  const { json, command } = parseArgs(process.argv.slice(2));
  const bin = locateNuggetd((m) => console.error(m));
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "nugget-mcp-compat-"));
  const daemon = await startDaemon(bin, { config: CONFIG, stderrPath: path.join(work, "nuggetd.log") });
  let code = 1;
  try {
    const corpus = path.join(work, "corpus");
    writeCorpus(corpus);
    await rpc(daemon.socket, "sources.add", {
      id: null,
      kind: "files",
      name: "Compat corpus",
      settings: { roots: [corpus], include: [], exclude: [] },
    });
    const ks = await waitForEntities(daemon.socket, PROJECTS);
    const clients = [];
    for (let i = 0; i < CLIENTS; i++) clients.push(await createClient(daemon.socket, `compat-${i + 1}`));
    const revoked = await createClient(daemon.socket, "compat-revoked");
    await rpc(daemon.socket, "mcp.clients.revoke", { id: revoked.id });
    const limited = await createClient(daemon.socket, "compat-limited", ["search"]);
    const fixture = {
      v: 1,
      nuggetd: bin,
      home: daemon.home,
      entities: ks.entities,
      projects: PROJECTS,
      needle: NEEDLE,
      clients,
      revoked,
      limited,
    };
    const file = json ? path.resolve(json) : path.join(work, "fixture.json");
    // It holds live tokens. `mode` only applies when the file is created, so a file reused via
    // --json is narrowed too.
    fs.writeFileSync(file, JSON.stringify(fixture, null, 2), { mode: 0o600 });
    fs.chmodSync(file, 0o600);
    console.error(`fixture: ${ks.entities} entities, ${clients.length} clients; running ${command.join(" ")}`);
    const child = spawn(command[0], command.slice(1), {
      stdio: "inherit",
      env: { ...process.env, NUGGET_MCP_COMPAT_FIXTURE: file },
    });
    code = await new Promise((resolve) => {
      child.on("exit", (c, sig) => resolve(c ?? (sig ? 1 : 0)));
      child.on("error", (e) => {
        console.error(`fixture: cannot run ${command[0]}: ${e.message}`);
        resolve(1);
      });
    });
    if (code !== 0) printLogs(work, daemon);
  } finally {
    await daemon.stop({ keep: code !== 0 });
    if (code === 0) fs.rmSync(work, { recursive: true, force: true });
  }
  return code;
}

main().then(
  (code) => process.exit(code),
  (e) => {
    console.error(e?.stack ?? String(e));
    process.exit(1);
  },
);
