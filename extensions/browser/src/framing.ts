// Native-messaging framing and message validation.
//
// The browser does the stdio framing for us (a 32-bit native-endian length, then UTF-8 JSON),
// but we still enforce our own size caps on both directions and validate every message from the
// host before acting on it. `encodeFrame` / `decodeFrames` implement the same framing for tests
// and the end-to-end fake host.

import {
  type BrowserAction,
  type ExtensionMessage,
  type HostRequest,
  MAX_FROM_HOST_BYTES,
  MAX_TO_HOST_BYTES,
  ProtocolError,
  type WaitCondition,
} from "./protocol";

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });

export function utf8Length(s: string): number {
  return encoder.encode(s).length;
}

/** Serialize a message for the host, refusing anything over the cap. */
export function encodeForHost(msg: ExtensionMessage, cap = MAX_TO_HOST_BYTES): string {
  const json = JSON.stringify(msg);
  const len = utf8Length(json);
  if (len > cap) {
    throw new ProtocolError("too_large", `message of ${len} bytes exceeds the ${cap}-byte cap`);
  }
  return json;
}

/** Length-prefixed frame: 4-byte little-endian length (all supported platforms are LE), then JSON. */
export function encodeFrame(value: unknown, cap = MAX_TO_HOST_BYTES): Uint8Array {
  const body = encoder.encode(JSON.stringify(value));
  if (body.length > cap) {
    throw new ProtocolError("too_large", `frame of ${body.length} bytes exceeds the ${cap}-byte cap`);
  }
  const out = new Uint8Array(4 + body.length);
  new DataView(out.buffer).setUint32(0, body.length, true);
  out.set(body, 4);
  return out;
}

/** Decode every complete frame in `buf`; returns the values and the unconsumed tail. */
export function decodeFrames(
  buf: Uint8Array,
  cap = MAX_FROM_HOST_BYTES,
): { messages: unknown[]; rest: Uint8Array } {
  const messages: unknown[] = [];
  let off = 0;
  while (buf.length - off >= 4) {
    const len = new DataView(buf.buffer, buf.byteOffset + off, 4).getUint32(0, true);
    if (len > cap) {
      throw new ProtocolError("too_large", `frame of ${len} bytes exceeds the ${cap}-byte cap`);
    }
    if (buf.length - off - 4 < len) break;
    const body = buf.subarray(off + 4, off + 4 + len);
    messages.push(JSON.parse(decoder.decode(body)));
    off += 4 + len;
  }
  return { messages, rest: buf.subarray(off) };
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function str(o: Record<string, unknown>, k: string, optional = false): string | undefined {
  const v = o[k];
  if (v === undefined && optional) return undefined;
  if (typeof v !== "string") throw new ProtocolError("bad_request", `\`${k}\` must be a string`);
  return v;
}

function num(o: Record<string, unknown>, k: string, optional = false): number | undefined {
  const v = o[k];
  if (v === undefined && optional) return undefined;
  if (typeof v !== "number" || !Number.isFinite(v)) {
    throw new ProtocolError("bad_request", `\`${k}\` must be a finite number`);
  }
  return v;
}

function parseWait(v: unknown): WaitCondition {
  if (!isObj(v)) throw new ProtocolError("bad_request", "`until` must be an object");
  const keys = Object.keys(v);
  if (keys.length !== 1) throw new ProtocolError("bad_request", "`until` needs exactly one condition");
  if ("text_present" in v) return { text_present: str(v, "text_present")! };
  if ("url_contains" in v) return { url_contains: str(v, "url_contains")! };
  if ("element_gone" in v) return { element_gone: str(v, "element_gone")! };
  if ("element_present" in v) {
    const p = v.element_present;
    if (!isObj(p)) throw new ProtocolError("bad_request", "`element_present` must be an object");
    const role = str(p, "role", true);
    const name = str(p, "name", true);
    if (role === undefined && name === undefined) {
      throw new ProtocolError("bad_request", "`element_present` needs a role or a name");
    }
    return { element_present: { role, name } };
  }
  throw new ProtocolError("bad_request", `unknown wait condition \`${keys[0]}\``);
}

/** Validate the shape of an action. Semantic checks against a snapshot live in `actions.ts`. */
export function parseAction(v: unknown): BrowserAction {
  if (!isObj(v)) throw new ProtocolError("bad_request", "`action` must be an object");
  switch (v.kind) {
    case "click":
      return { kind: "click", element: str(v, "element")! };
    case "type":
      if (v.clear !== undefined && typeof v.clear !== "boolean") {
        throw new ProtocolError("bad_request", "`clear` must be a boolean");
      }
      return { kind: "type", element: str(v, "element")!, text: str(v, "text")!, clear: v.clear as boolean | undefined };
    case "select":
      return { kind: "select", element: str(v, "element")!, value: str(v, "value")! };
    case "scroll":
      return { kind: "scroll", element: str(v, "element", true), dx: num(v, "dx", true), dy: num(v, "dy", true) };
    case "navigate":
      return { kind: "navigate", url: str(v, "url")! };
    case "submit":
      return { kind: "submit", element: str(v, "element")! };
    case "wait_for":
      return { kind: "wait_for", until: parseWait(v.until), timeout_ms: num(v, "timeout_ms", true) };
    default:
      throw new ProtocolError("bad_request", `unknown action kind \`${String(v.kind)}\``);
  }
}

/** Validate a message received from the host. Throws `ProtocolError` on anything malformed. */
export function parseHostRequest(raw: unknown): HostRequest {
  if (!isObj(raw)) throw new ProtocolError("bad_request", "message must be a JSON object");
  const id = num(raw, "id")!;
  if (!Number.isSafeInteger(id) || id < 0) throw new ProtocolError("bad_request", "`id` must be a non-negative integer");
  const tab_id = num(raw, "tab_id", true);
  switch (raw.type) {
    case "hello":
      return { id, type: "hello", protocol: num(raw, "protocol")! };
    case "snapshot": {
      const o = raw.options;
      if (o !== undefined && !isObj(o)) throw new ProtocolError("bad_request", "`options` must be an object");
      return {
        id,
        type: "snapshot",
        tab_id,
        options: o && {
          include_text: o.include_text === undefined ? undefined : Boolean(o.include_text),
          include_hidden: o.include_hidden === undefined ? undefined : Boolean(o.include_hidden),
        },
      };
    }
    case "act":
      return { id, type: "act", tab_id, snapshot_id: str(raw, "snapshot_id")!, action: parseAction(raw.action) };
    case "overlay":
      if (typeof raw.active !== "boolean") throw new ProtocolError("bad_request", "`active` must be a boolean");
      return { id, type: "overlay", tab_id, active: raw.active, label: str(raw, "label", true) };
    case "halt":
      return { id, type: "halt" };
    default:
      throw new ProtocolError("bad_request", `unknown request type \`${String(raw.type)}\``);
  }
}
