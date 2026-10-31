#!/usr/bin/env node
// MCP compatibility check with the official TypeScript SDK (@modelcontextprotocol/sdk, pinned in
// package.json) against a running nuggetd, over the stdio bridge (`nuggetd mcp --client <id>`,
// launched exactly as the `claude-desktop` setup snippet says) and loopback streamable HTTP (the
// `generic-http` snippet's URL and Authorization header).
//
// Run through the fixture, which starts the daemon and writes the connection details:
//   node ../fixture.mjs -- node client.mjs
// Prints one line per check and a summary; exits nonzero when any check fails.
import { spawnSync } from "node:child_process";
import fs from "node:fs";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import {
  CallToolResultSchema,
  ErrorCode,
  LATEST_PROTOCOL_VERSION,
  ListToolsResultSchema,
  McpError,
  SUPPORTED_PROTOCOL_VERSIONS,
} from "@modelcontextprotocol/sdk/types.js";

const fixturePath = process.env.NUGGET_MCP_COMPAT_FIXTURE;
if (!fixturePath) {
  console.error("set NUGGET_MCP_COMPAT_FIXTURE (run through ../fixture.mjs)");
  process.exit(2);
}
const fx = JSON.parse(fs.readFileSync(fixturePath, "utf8"));

/** Revisions Nugget (rmcp 3.8.0) speaks over the initialize handshake. */
const NUGGET_HANDSHAKE = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
/** The two newest revisions this SDK and Nugget share, newest first. */
const REVISIONS = SUPPORTED_PROTOCOL_VERSIONS.filter((v) => NUGGET_HANDSHAKE.includes(v))
  .sort()
  .reverse()
  .slice(0, 2);

/** What `nuggetd mcp` prints when the daemon refuses the client. */
const BRIDGE_REFUSAL = "unknown or revoked MCP client, or wrong token";
const TOOLS = ["search", "get_context", "find_entities", "get_entity", "get_neighbourhood", "get_item", "recent_changes"];
const CALL_TIMEOUT = { timeout: 30_000 };

// ---- reporting ----

const results = [];
let section = "";

async function check(name, fn) {
  const label = `${section} ${name}`.trim();
  try {
    await fn();
    results.push({ label, ok: true });
    console.log(`PASS ${label}`);
  } catch (e) {
    results.push({ label, ok: false, err: e });
    console.log(`FAIL ${label}: ${e?.message ?? e}`);
  }
}

function assert(cond, msg) {
  if (!cond) throw new Error(msg);
}

/** Expect `promise` to reject with an McpError (optionally of `code`). Returns the error. */
async function rejectsWith(promise, code, what) {
  let err;
  try {
    await promise;
  } catch (e) {
    err = e;
  }
  assert(err, `${what}: expected an error, got success`);
  if (code !== undefined) {
    assert(err instanceof McpError, `${what}: expected an MCP error, got ${err?.constructor?.name}: ${err?.message}`);
    assert(err.code === code, `${what}: expected code ${code}, got ${err.code} (${err.message})`);
  }
  return err;
}

const textOf = (r) => (r.content ?? []).filter((c) => c.type === "text").map((c) => c.text).join("\n");

// ---- transports ----

/**
 * Wraps a transport to pin the protocol revision the client offers in `initialize` (the SDK always
 * offers its latest) and to record the revision the server chose.
 */
class PinnedTransport {
  constructor(inner, revision) {
    this.inner = inner;
    this.revision = revision;
    this.negotiated = undefined;
    this.initId = undefined;
  }
  get sessionId() {
    return this.inner.sessionId;
  }
  async start() {
    this.inner.onmessage = (m, extra) => {
      if (m.id !== undefined && m.id === this.initId && m.result) this.negotiated = m.result.protocolVersion;
      this.onmessage?.(m, extra);
    };
    this.inner.onclose = () => this.onclose?.();
    this.inner.onerror = (e) => this.onerror?.(e);
    return this.inner.start();
  }
  send(message, options) {
    if (message.method === "initialize") {
      this.initId = message.id;
      message = { ...message, params: { ...message.params, protocolVersion: this.revision } };
    }
    return this.inner.send(message, options);
  }
  close() {
    return this.inner.close();
  }
  setProtocolVersion(v) {
    this.inner.setProtocolVersion?.(v);
  }
}

function stdioTransport(conn, overrideEnv = {}) {
  return new StdioClientTransport({
    command: conn.stdio.command,
    args: conn.stdio.args,
    env: { ...conn.stdio.env, ...overrideEnv },
    stderr: "pipe",
  });
}

function httpTransport(conn, headers = conn.http.headers) {
  return new StreamableHTTPClientTransport(new URL(conn.http.url), { requestInit: { headers } });
}

function makeTransport(kind, conn) {
  return kind === "stdio" ? stdioTransport(conn) : httpTransport(conn);
}

async function connect(kind, conn, revision) {
  const transport = new PinnedTransport(makeTransport(kind, conn), revision);
  const client = new Client({ name: "nugget-compat-ts", version: "1.0.0" });
  await client.connect(transport, CALL_TIMEOUT);
  return { client, transport };
}

// ---- the leg: one transport at one revision ----

async function leg(kind, conn, revision) {
  section = `[ts ${kind} ${revision}]`;
  let client, transport;
  await check("initialize", async () => {
    ({ client, transport } = await connect(kind, conn, revision));
    assert(transport.negotiated === revision, `negotiated ${transport.negotiated}, wanted ${revision}`);
    const info = client.getServerVersion();
    assert(info?.name === "nugget", `server name ${info?.name}`);
    const caps = client.getServerCapabilities();
    assert(caps?.tools && caps?.resources, `capabilities ${JSON.stringify(caps)}`);
    assert((client.getInstructions() ?? "").includes("untrusted"), "instructions carry the safety note");
  });
  if (!client) return;
  try {
    await body(kind, client);
  } finally {
    await check("close", () => client.close());
  }
}

async function body(kind, client) {
  await check("ping", () => client.ping(CALL_TIMEOUT));

  let tools = [];
  await check("tools/list", async () => {
    const r = await client.listTools(undefined, CALL_TIMEOUT);
    tools = r.tools;
    const names = tools.map((t) => t.name).sort();
    assert(JSON.stringify(names) === JSON.stringify([...TOOLS].sort()), `tools ${names}`);
    for (const t of tools) {
      assert(t.inputSchema?.type === "object", `${t.name}: inputSchema`);
      assert(t.annotations?.readOnlyHint === true, `${t.name}: readOnlyHint`);
    }
    assert(r.nextCursor === undefined, "tools/list is a single page");
  });
  await check("tools/list ignores a cursor", async () => {
    const r = await client.request({ method: "tools/list", params: { cursor: "anything" } }, ListToolsResultSchema, CALL_TIMEOUT);
    assert(r.tools.length === TOOLS.length, `got ${r.tools.length} tools`);
  });

  // Happy paths, chained: search finds the needle item, find_entities finds a project.
  const call = (name, args) => client.callTool({ name, arguments: args }, CallToolResultSchema, CALL_TIMEOUT);
  let hit, entity;
  await check("call search", async () => {
    const r = await call("search", { query: fx.needle, limit: 5 });
    assert(!r.isError, textOf(r));
    hit = r.structuredContent?.hits?.find((h) => (h.snippet ?? "").includes("Falcon")) ?? r.structuredContent?.hits?.[0];
    assert(hit?.source && hit?.key && hit?.uri, `no hit: ${textOf(r).slice(0, 300)}`);
    assert(textOf(r).includes("untrusted-content"), "content is wrapped as untrusted");
  });
  await check("call get_context", async () => {
    const r = await call("get_context", { query: "When is the Falcon launch review?", budget_tokens: 500 });
    assert(!r.isError, textOf(r));
    assert(textOf(r).includes("[1]"), "numbered passages");
  });
  await check("call find_entities", async () => {
    const r = await call("find_entities", { text: "Compat Project 001", kind: "project", limit: 5 });
    assert(!r.isError, textOf(r));
    entity = r.structuredContent?.entities?.[0];
    assert(entity?.id && entity?.uri, `no entity: ${textOf(r).slice(0, 300)}`);
  });
  await check("call get_entity", async () => {
    const r = await call("get_entity", { id: entity.id });
    assert(!r.isError, textOf(r));
    assert(textOf(r).includes("Compat Project 001"), "entity page names the entity");
  });
  await check("call get_neighbourhood", async () => {
    const r = await call("get_neighbourhood", { id: entity.id, depth: 1 });
    assert(!r.isError, textOf(r));
    assert((r.structuredContent?.nodes ?? []).length >= 1, "at least the centre node");
  });
  await check("call get_item", async () => {
    const r = await call("get_item", { source: hit.source, key: hit.key, max_chars: 2000 });
    assert(!r.isError, textOf(r));
    assert(textOf(r).includes("Falcon"), "item text");
  });
  await check("call recent_changes", async () => {
    const r = await call("recent_changes", { limit: 5 });
    assert(!r.isError, textOf(r));
    assert((r.structuredContent?.entities ?? []).length > 0, "recent entities");
  });

  // Invalid arguments: a tool error result (isError), as the spec asks for input validation.
  const bad = {
    search: { limit: 3 },
    get_context: { query: 42 },
    find_entities: { text: "x", kind: "spaceship" },
    get_entity: {},
    get_neighbourhood: { id: "x", depth: "deep" },
    get_item: { source: "only-source" },
    recent_changes: { since: "yesterday-ish" },
  };
  for (const name of TOOLS) {
    await check(`call ${name} with invalid args`, async () => {
      const r = await call(name, bad[name]);
      assert(r.isError === true, `expected isError, got ${JSON.stringify(r).slice(0, 300)}`);
    });
  }
  await check("call get_entity for a missing entity", async () => {
    const r = await call("get_entity", { id: "ent_does_not_exist" });
    assert(r.isError === true, "expected isError");
  });
  await check("unknown tool is -32602", () => rejectsWith(call("rm_rf", {}), ErrorCode.InvalidParams, "unknown tool"));

  // Resources.
  let resources = [];
  await check("resources/list follows nextCursor", async () => {
    let cursor;
    let pages = 0;
    do {
      const r = await client.listResources(cursor ? { cursor } : undefined, CALL_TIMEOUT);
      resources.push(...r.resources);
      cursor = r.nextCursor;
      pages++;
      assert(pages < 20, "too many pages");
    } while (cursor);
    const uris = new Set(resources.map((r) => r.uri));
    assert(pages > 1, `expected more than one page, got ${pages}`);
    assert(uris.size === resources.length, "no duplicates across pages");
    assert(resources.length >= fx.projects, `listed ${resources.length}, want >= ${fx.projects}`);
    assert(resources.every((r) => r.uri.startsWith("nugget://entity/") && r.name), "entity resources");
  });
  await check("resources/list rejects a garbage cursor", () =>
    rejectsWith(client.listResources({ cursor: "not-a-cursor" }, CALL_TIMEOUT), ErrorCode.InvalidParams, "garbage cursor"),
  );
  await check("resources/templates/list", async () => {
    const r = await client.listResourceTemplates(undefined, CALL_TIMEOUT);
    const t = r.resourceTemplates.map((x) => x.uriTemplate).sort();
    assert(JSON.stringify(t) === JSON.stringify(["nugget://entity/{id}", "nugget://item/{source}/{key}"]), `templates ${t}`);
  });
  await check("resources/read entity", async () => {
    const r = await client.readResource({ uri: entity.uri }, CALL_TIMEOUT);
    assert(r.contents[0]?.mimeType === "text/markdown", `mime ${r.contents[0]?.mimeType}`);
    assert(r.contents[0].text.includes("Compat Project 001"), "entity page");
  });
  await check("resources/read listed entity", async () => {
    const r = await client.readResource({ uri: resources[resources.length - 1].uri }, CALL_TIMEOUT);
    assert(r.contents[0]?.uri === resources[resources.length - 1].uri, "echoes the uri");
  });
  await check("resources/read item", async () => {
    const r = await client.readResource({ uri: hit.uri }, CALL_TIMEOUT);
    assert(r.contents[0]?.mimeType === "text/plain", `mime ${r.contents[0]?.mimeType}`);
    assert(r.contents[0].text.includes("Falcon"), "item text");
  });
  await check("resources/read unknown entity is -32002", () =>
    rejectsWith(client.readResource({ uri: "nugget://entity/ent_does_not_exist" }, CALL_TIMEOUT), -32002, "unknown entity"),
  );
  await check("resources/read foreign URI is -32002", () =>
    rejectsWith(client.readResource({ uri: "file:///etc/passwd" }, CALL_TIMEOUT), -32002, "foreign uri"),
  );

  // Cancellation: abort in-flight requests (the SDK sends notifications/cancelled), then a stray
  // cancellation for an id that never existed; the session must keep working.
  await check("cancellation", async () => {
    const pending = [];
    for (let i = 0; i < 3; i++) {
      const ac = new AbortController();
      const p = client.callTool(
        { name: "get_context", arguments: { query: `Falcon launch ${i}`, budget_tokens: 8000 } },
        CallToolResultSchema,
        { ...CALL_TIMEOUT, signal: ac.signal },
      );
      ac.abort("compat: cancelled by the client");
      pending.push(p.then(() => "done", () => "cancelled"));
    }
    const outcomes = await Promise.all(pending);
    assert(outcomes.every((o) => o === "cancelled"), `aborted calls settle as cancelled: ${outcomes}`);
    await client.notification({ method: "notifications/cancelled", params: { requestId: 987654, reason: "stray" } });
    const r = await call("search", { query: "Falcon" });
    assert(!r.isError, "the session still answers after cancellations");
    await client.ping(CALL_TIMEOUT);
  });
}

// ---- access errors and HTTP specifics ----

async function access(kind) {
  section = `[ts ${kind} access]`;
  const conn = fx.clients[nextClient++];
  const expectRefused = async (what, transport) => {
    const client = new Client({ name: "nugget-compat-ts", version: "1.0.0" });
    let stderr = "";
    transport.stderr?.on("data", (d) => (stderr += d));
    let err;
    try {
      await client.connect(transport, { timeout: 15_000 });
    } catch (e) {
      err = e;
    }
    await client.close().catch(() => {});
    assert(err, `${what}: connected`);
    if (kind === "http") {
      assert(err?.code === 401, `${what}: want HTTP 401, got ${err?.code}: ${err?.message}`);
      assert(/bearer token/.test(String(err?.message)), `${what}: the refusal says why: ${err?.message}`);
    } else {
      // The bridge itself must refuse (not crash, not hang): its stderr names the reason.
      assert(stderr.includes(BRIDGE_REFUSAL), `${what}: bridge stderr ${JSON.stringify(stderr)}`);
    }
    return err;
  };
  if (kind === "stdio") {
    // Launched directly: a refused bridge exits nonzero with the reason on stderr, stdout empty.
    for (const [what, c, env] of [
      ["revoked", fx.revoked, {}],
      ["wrong token", conn, { NUGGET_MCP_TOKEN: "ngt_wrong" }],
    ]) {
      await check(`bridge exits nonzero for a ${what} client`, async () => {
        const r = spawnSync(c.stdio.command, c.stdio.args, {
          env: { ...process.env, ...c.stdio.env, ...env },
          input: "",
          encoding: "utf8",
          timeout: 30_000,
        });
        assert(r.status !== 0 && r.status !== null, `exit status ${r.status} (${r.error?.message ?? r.signal})`);
        assert(r.stderr.includes(BRIDGE_REFUSAL), `stderr ${JSON.stringify(r.stderr)}`);
        assert(r.stdout === "", `stdout carries only MCP messages: ${JSON.stringify(r.stdout)}`);
      });
    }
  }
  await check("revoked client is refused", () =>
    expectRefused("revoked", kind === "stdio" ? stdioTransport(fx.revoked) : httpTransport(fx.revoked)),
  );
  await check("wrong token is refused", () =>
    expectRefused(
      "wrong token",
      kind === "stdio"
        ? stdioTransport(conn, { NUGGET_MCP_TOKEN: "ngt_wrong" })
        : httpTransport(conn, { Authorization: "Bearer ngt_wrong" }),
    ),
  );
  if (kind === "http") {
    await check("missing token is refused", () => expectRefused("no token", httpTransport(conn, {})));
  }
  await check("scoped client sees only its tools", async () => {
    const { client } = await connect(kind, fx.limited, REVISIONS[0]);
    try {
      const r = await client.listTools(undefined, CALL_TIMEOUT);
      assert(r.tools.map((t) => t.name).join() === "search", `tools ${r.tools.map((t) => t.name)}`);
      const denied = await client.callTool({ name: "get_entity", arguments: { id: "x" } }, CallToolResultSchema, CALL_TIMEOUT);
      assert(denied.isError === true && /not enabled/.test(textOf(denied)), "get_entity is denied");
      const res = await client.listResources(undefined, CALL_TIMEOUT);
      assert(res.resources.length === 0, "no entity resources without get_entity");
    } finally {
      await client.close();
    }
  });
  if (kind === "http") {
    const post = (headers, body) =>
      fetch(conn.http.url, {
        method: "POST",
        headers: { "content-type": "application/json", accept: "application/json, text/event-stream", ...conn.http.headers, ...headers },
        body: JSON.stringify(body),
      });
    for (const rev of REVISIONS) {
      await check(`MCP-Protocol-Version ${rev} is accepted`, async () => {
        const r = await post({ "mcp-protocol-version": rev }, { jsonrpc: "2.0", id: 1, method: "tools/list" });
        const text = await r.text();
        assert(r.status === 200, `status ${r.status}: ${text}`);
        const j = JSON.parse(text);
        assert(j.result?.tools?.length === TOOLS.length, JSON.stringify(j).slice(0, 200));
      });
    }
    await check("unsupported MCP-Protocol-Version is 400", async () => {
      const r = await post({ "mcp-protocol-version": "1999-01-01" }, { jsonrpc: "2.0", id: 1, method: "tools/list" });
      assert(r.status === 400, `status ${r.status}`);
    });
    await check("token in the query string is refused", async () => {
      const r = await fetch(`${conn.http.url}?token=${encodeURIComponent(conn.token)}`, { method: "POST", body: "{}" });
      assert(r.status === 400, `status ${r.status}`);
    });
  }
}

/** A runaway loop hits the per-client rate limit with an error, not a hang or a crash. */
async function rateLimit(kind) {
  section = `[ts ${kind} rate-limit]`;
  const conn = fx.clients[nextClient++];
  await check("burst over the limit is refused", async () => {
    const { client } = await connect(kind, conn, REVISIONS[0]);
    try {
      const outcomes = await Promise.allSettled(
        Array.from({ length: 80 }, () => client.callTool({ name: "search", arguments: { query: "Falcon" } }, CallToolResultSchema, CALL_TIMEOUT)),
      );
      const refused = outcomes.filter((o) => o.status === "rejected");
      assert(refused.length > 0, "no request was rate limited");
      const msg = String(refused[0].reason?.message);
      assert(/rate limited|429/.test(msg), `rate-limit error says so: ${msg}`);
    } finally {
      await client.close().catch(() => {});
    }
  });
}

// ---- main ----

let nextClient = 0;
console.log(`TypeScript SDK ${JSON.parse(fs.readFileSync(new URL("./node_modules/@modelcontextprotocol/sdk/package.json", import.meta.url))).version}; latest ${LATEST_PROTOCOL_VERSION}; testing ${REVISIONS.join(", ")}`);
for (const kind of ["stdio", "http"]) {
  for (const revision of REVISIONS) await leg(kind, fx.clients[nextClient++], revision);
  await access(kind);
  await rateLimit(kind);
}

const failed = results.filter((r) => !r.ok);
console.log(`\nTypeScript SDK: ${results.length - failed.length} passed, ${failed.length} failed`);
for (const f of failed) console.log(`  FAIL ${f.label}: ${f.err?.message ?? f.err}`);
process.exit(failed.length ? 1 : 0);
