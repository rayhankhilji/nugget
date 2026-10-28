import { describe, expect, it } from "vitest";
import { decodeFrames, encodeForHost, encodeFrame, parseAction, parseHostRequest } from "../src/framing";
import { MAX_FROM_HOST_BYTES, ProtocolError } from "../src/protocol";

function concat(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.length;
  }
  return out;
}

function errCode(f: () => unknown): string | undefined {
  try {
    f();
  } catch (e) {
    return e instanceof ProtocolError ? e.code : "other";
  }
  return undefined;
}

describe("native framing", () => {
  it("round-trips several frames, including multi-byte UTF-8", () => {
    const a = encodeFrame({ id: 1, type: "hello", protocol: 1 });
    const b = encodeFrame({ text: "naïve — 日本語 🍣" });
    expect(new DataView(a.buffer).getUint32(0, true)).toBe(a.length - 4);
    const { messages, rest } = decodeFrames(concat(a, b));
    expect(messages).toEqual([{ id: 1, type: "hello", protocol: 1 }, { text: "naïve — 日本語 🍣" }]);
    expect(rest.length).toBe(0);
  });

  it("keeps a partial frame for later", () => {
    const a = encodeFrame({ id: 2 });
    const { messages, rest } = decodeFrames(a.subarray(0, a.length - 1));
    expect(messages).toEqual([]);
    expect(rest.length).toBe(a.length - 1);
  });

  it("rejects oversized frames on both sides", () => {
    expect(errCode(() => encodeFrame({ s: "x".repeat(100) }, 50))).toBe("too_large");
    const header = new Uint8Array(4);
    new DataView(header.buffer).setUint32(0, MAX_FROM_HOST_BYTES + 1, true);
    expect(errCode(() => decodeFrames(header))).toBe("too_large");
  });

  it("caps messages to the host", () => {
    expect(errCode(() => encodeForHost({ type: "reply", id: 1, result: "x".repeat(200) }, 100))).toBe("too_large");
    expect(JSON.parse(encodeForHost({ type: "reply", id: 1, result: {} }))).toEqual({ type: "reply", id: 1, result: {} });
  });
});

describe("host request validation", () => {
  it("parses every request type", () => {
    expect(parseHostRequest({ id: 1, type: "hello", protocol: 1 })).toEqual({ id: 1, type: "hello", protocol: 1 });
    expect(parseHostRequest({ id: 2, type: "snapshot", options: { include_text: false } })).toMatchObject({
      type: "snapshot",
      options: { include_text: false },
    });
    expect(parseHostRequest({ id: 3, type: "act", snapshot_id: "a.1", action: { kind: "click", element: "n1" } })).toMatchObject({
      action: { kind: "click", element: "n1" },
    });
    expect(parseHostRequest({ id: 4, type: "overlay", active: true })).toMatchObject({ active: true });
    expect(parseHostRequest({ id: 5, type: "halt" })).toEqual({ id: 5, type: "halt" });
  });

  it("rejects malformed requests", () => {
    for (const bad of [
      null,
      [],
      { type: "hello", protocol: 1 },
      { id: -1, type: "halt" },
      { id: 1.5, type: "halt" },
      { id: 1, type: "exec" },
      { id: 1, type: "act", action: { kind: "click", element: "n1" } },
      { id: 1, type: "overlay", active: "yes" },
    ]) {
      expect(errCode(() => parseHostRequest(bad))).toBe("bad_request");
    }
  });

  it("validates action shapes", () => {
    expect(parseAction({ kind: "type", element: "n1", text: "hi", clear: false })).toEqual({ kind: "type", element: "n1", text: "hi", clear: false });
    expect(parseAction({ kind: "wait_for", until: { element_present: { name: "OK" } } })).toMatchObject({ until: { element_present: { name: "OK" } } });
    for (const bad of [
      { kind: "eval", code: "1" },
      { kind: "click" },
      { kind: "type", element: "n1", text: 3 },
      { kind: "scroll", dy: "far" },
      { kind: "scroll", dy: Infinity },
      { kind: "wait_for", until: {} },
      { kind: "wait_for", until: { text_present: "a", url_contains: "b" } },
      { kind: "wait_for", until: { element_present: {} } },
    ]) {
      expect(errCode(() => parseAction(bad))).toBe("bad_request");
    }
  });
});
