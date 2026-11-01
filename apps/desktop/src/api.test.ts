import { describe, expect, it } from "vitest";
import {
  isOpenableUrl,
  newPluginCapabilities,
  pluginCapabilitiesEmpty,
  SERVER_BUSY,
  withBusyRetry,
} from "./api";


describe("isOpenableUrl", () => {
  it("allows https, obsidian and local file URLs", () => {
    for (const u of [
      "https://mail.google.com/mail/u/0/#inbox/abc",
      "HTTPS://example.com/",
      "obsidian://open?vault=Notes&file=Daily%2F2026-10-28",
      "file:///home/me/Notes/a%20b.md",
      "file://localhost/tmp/x.txt",
      "file:///C:/Users/me/a.md",
    ]) {
      expect(isOpenableUrl(u), u).toBe(true);
    }
  });

  it("refuses script, data and other schemes", () => {
    for (const u of [
      "javascript:alert(1)",
      "JavaScript:alert(1)",
      " javascript:alert(1)",
      "java\tscript:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "vbscript:msgbox(1)",
      "http://example.com/",
      "mailto:me@example.com",
      "smb://server/share",
      "ms-msdt:/id",
      "blob:https://example.com/x",
      "about:blank",
    ]) {
      expect(isOpenableUrl(u), u).toBe(false);
    }
  });

  it("refuses network shares, malformed and oversized input", () => {
    for (const u of [
      "file://server/share/doc.txt",
      "file:////server/share/doc.txt",
      "https://",
      "not a url",
      "/etc/passwd",
      "https://example.com/a b",
      "https://example.com/\n",
      "",
      null,
      undefined,
      `https://example.com/${"a".repeat(9000)}`,
    ]) {
      expect(isOpenableUrl(u), String(u).slice(0, 40)).toBe(false);
    }
  });
});

// ---- knowledge & MCP (M2) ----



describe("plugin capabilities", () => {
  it("reports only what an update adds", () => {
    const prev = { hosts: ["feeds.example.com", "*.cdn.example"], secrets: ["token"], kv_max_bytes: 1000 };
    expect(newPluginCapabilities(prev, null)).toEqual(prev);
    expect(
      newPluginCapabilities({ hosts: ["https://feeds.example.com", "*.cdn.example", "new.example"], secrets: ["token", "pw"], kv_max_bytes: 500 }, prev),
    ).toEqual({ hosts: ["new.example"], secrets: ["pw"], kv_max_bytes: 0 });
    expect(newPluginCapabilities({ hosts: ["http://feeds.example.com"], secrets: [], kv_max_bytes: 2000 }, prev)).toEqual({
      hosts: ["http://feeds.example.com"],
      secrets: [],
      kv_max_bytes: 2000,
    });
    expect(pluginCapabilitiesEmpty({ hosts: [], secrets: [], kv_max_bytes: 0 })).toBe(true);
  });
});

describe("withBusyRetry", () => {
  const busy = { kind: "rpc", code: SERVER_BUSY, message: "too many requests in flight" };

  it("retries a busy daemon with a bounded backoff, then succeeds", async () => {
    const waits: number[] = [];
    let n = 0;
    const r = await withBusyRetry(
      async () => {
        if (++n < 3) throw busy;
        return "ok";
      },
      async (ms) => void waits.push(ms),
    );
    expect(r).toBe("ok");
    expect(n).toBe(3);
    expect(waits).toEqual([100, 300]);
  });
});
