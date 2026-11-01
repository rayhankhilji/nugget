// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { cleanup, installDomShims, mockDaemon, render } from "../test/render";
import { DeviceCode, DeviceSignInDialog, displayUrl } from "./auth";
import type { SourceInfo } from "../api";

const source: SourceInfo = { id: "github-1", kind: "github", name: "GitHub", enabled: true, state: "needs_auth", items: 0, chunks: 0, settings: { client_id: "Iv1.x" } };

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("device code", () => {
  it("shows the code and the page without the query", async () => {
    expect(displayUrl("https://github.com/login/device?user_code=ABCD-1234")).toBe("https://github.com/login/device");
    const { container } = await render(<DeviceCode code="ABCD-1234" url="https://github.com/login/device?user_code=ABCD-1234" />);
    expect(container.querySelector(".device-code-value")?.textContent).toBe("ABCD-1234");
    expect(container.textContent).toContain("https://github.com/login/device");
    expect(container.textContent).not.toContain("user_code=");
  });

  it("reconnect dialog begins the flow and finishes on sources.changed", async () => {
    const calls = mockDaemon({
      sources_auth_begin: { url: "https://github.com/login/device?user_code=QQQQ-1111", user_code: "QQQQ-1111" },
    });
    let connected: SourceInfo | null = null;
    await render(<DeviceSignInDialog source={source} onClose={() => {}} onConnected={(s) => (connected = s)} />);
    expect(calls.filter((c) => c === "sources_auth_begin")).toHaveLength(1);
    expect(calls).not.toContain("open_url");
    expect(document.querySelector(".device-code-value")?.textContent).toBe("QQQQ-1111");
    await act(() => emit("nugget:sources-changed", { ...source, state: "idle" }));
    await act(async () => {});
    expect(connected).not.toBeNull();
  });
});
