// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { AiKeySaveRequest, LocalServer } from "../api";
import { byText, cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import { LocalServers } from "./LocalServers";

beforeEach(() => installDomShims());
afterEach(cleanup);

const ollama: LocalServer = {
  id: "ollama",
  label: "Ollama",
  base_url: "http://127.0.0.1:11434/v1",
  models: ["qwen3:4b", "llama3.2"],
  cloud_models: ["gpt-oss:120b-cloud"],
  configured: false,
};

describe("LocalServers", () => {
  it("lists a running server's local models and adds it without a key", async () => {
    const saves: AiKeySaveRequest[] = [];
    let configured = false;
    mockDaemon({
      ai_local_detect: () => ({ servers: [{ ...ollama, configured }] }),
      ai_key_save: (args: unknown) => {
        saves.push((args as { request: AiKeySaveRequest }).request);
        configured = true;
        return { ok: true, reason: "ok", message: "", models: [], saved: true, provider_id: "ollama", restart_required: false };
      },
    });
    const added: string[] = [];
    const { container } = await render(<LocalServers onSaved={(p) => added.push(p)} />);
    const options = [...container.querySelectorAll("option")].map((o) => o.textContent);
    // Cloud-forwarding models are not offered as local.
    expect(options).toEqual(["qwen3:4b", "llama3.2"]);
    await click(byText(container, "button", "Use Ollama"));
    expect(saves).toEqual([{ name: "ollama", kind: "openai_compatible", base_url: "http://127.0.0.1:11434/v1", model: "qwen3:4b" }]);
    expect(added).toEqual(["ollama"]);
    expect(container.textContent).toContain("Added");
  });

  it("renders nothing when no server is running", async () => {
    mockDaemon({ ai_local_detect: () => ({ servers: [] }) });
    const { container } = await render(<LocalServers />);
    expect(container.textContent).toBe("");
  });
});
