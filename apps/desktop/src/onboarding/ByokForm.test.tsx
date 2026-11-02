// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { AiKeyAddRequest, AiKeyAddResult, AiKeyCheckRequest, AiKeySaveRequest, AiKeySaveResult } from "../api";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { ByokForm, kindLabel } from "./ByokForm";

beforeEach(() => installDomShims());
afterEach(cleanup);

async function violations(): Promise<string[]> {
  const result = await axe.run(document.body, { rules: { "color-contrast": { enabled: false } } });
  return result.violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

/** The input labelled `label`. */
function field(c: HTMLElement, label: string): HTMLInputElement {
  const l = byText(c, "label", label) as HTMLLabelElement | undefined;
  if (!l) throw new Error(`no label ${label}`);
  return document.getElementById((l as HTMLLabelElement).htmlFor) as HTMLInputElement;
}

async function type(input: HTMLInputElement, value: string) {
  await act(async () => {
    const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    set.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

const result = (r: Partial<AiKeySaveResult>): AiKeySaveResult => ({
  ok: false,
  reason: "ok",
  message: "",
  models: [],
  saved: false,
  provider_id: null,
  restart_required: false,
  ...r,
});

const added = (r: Partial<AiKeyAddResult>): AiKeyAddResult => ({ ...result({}), ...r });

describe("ByokForm", () => {
  it("saves a key that works, says it can be used now and forgets the key", async () => {
    const saves: AiKeySaveRequest[] = [];
    mockDaemon({
      ai_key_save: (args: unknown) => {
        saves.push((args as { request: AiKeySaveRequest }).request);
        return result({ ok: true, models: ["m1"], saved: true, provider_id: "custom" });
      },
    });
    const saved: string[] = [];
    const { container } = await render(<ByokForm kind="compatible" onSaved={(id) => saved.push(id)} />);
    expect(field(container, "Name").value).toBe("custom");
    await type(field(container, "Address (base URL)"), "http://127.0.0.1:9999/v1/");
    await type(field(container, "API key"), " sk-e2e-good ");
    await type(field(container, "Model"), "m1");
    expect(field(container, "API key").type).toBe("password");
    expect(field(container, "API key").getAttribute("autocomplete")).toBe("off");

    await click(byText(container, "button", "Check and save key"));
    expect(saves).toEqual([
      { name: "custom", kind: "openai_compatible", base_url: "http://127.0.0.1:9999/v1", api_key: "sk-e2e-good", model: "m1" },
    ]);
    expect(container.querySelector('[role="status"]')?.textContent).toBe("Key checked and saved. You can use it now.");
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(field(container, "API key").value).toBe("");
    // Tells onboarding the BYOK path is set up.
    expect(saved).toEqual(["custom"]);
    expect(await violations()).toEqual([]);
  });

  it("without a keyring, explains and saves an environment variable's name instead", async () => {
    const saves: AiKeyAddRequest[] = [];
    mockDaemon({
      ai_key_save: (args: unknown) => {
        const r = (args as { request: AiKeyAddRequest }).request;
        saves.push(r);
        return r.api_key
          ? added({ reason: "no_keychain", message: "raw", suggested_env: "NUGGET_OPENAI_KEY" })
          : added({ ok: true, saved: true, provider_id: "openai" });
      },
    });
    const { container } = await render(<ByokForm kind="openai" />);
    await type(field(container, "API key"), "sk-x");
    await type(field(container, "Model"), "gpt-5");
    await click(byText(container, "button", "Check and save key"));
    expect(container.querySelector('[role="alert"]')?.textContent).toBe(
      "This computer has no keyring to store the key safely. Put the key in an environment variable (for example NUGGET_OPENAI_KEY) and enter its name here.",
    );
    // The key field is gone (and forgotten); the variable's name is suggested.
    expect(byText(container, "label", "API key")).toBeUndefined();
    const env = field(container, "Environment variable name");
    expect(env.value).toBe("NUGGET_OPENAI_KEY");
    expect(env.type).toBe("text");
    expect(await violations()).toEqual([]);

    await type(env, "1bad");
    await click(byText(container, "button", "Check and save key"));
    expect(container.querySelector('[role="alert"]')?.textContent).toBe(
      "Use letters, digits and _ for the variable name, and don't start it with a digit.",
    );
    expect(saves).toHaveLength(1);

    await type(env, "MY_OPENAI_KEY");
    await click(byText(container, "button", "Check and save key"));
    expect(saves[1]).toEqual({ name: "openai", kind: "openai_responses", base_url: null, model: "gpt-5", key_ref: "env:MY_OPENAI_KEY" });
    expect(container.querySelector('[role="status"]')?.textContent).toBe("Key checked and saved. You can use it now.");

    // And back to pasting a key; focus follows to the field that appears.
    await click(byText(container, "button", "Paste the key instead"));
    expect(field(container, "API key").value).toBe("");
    expect(document.activeElement).toBe(field(container, "API key"));
    await click(byText(container, "button", "Use an environment variable instead"));
    expect(document.activeElement).toBe(field(container, "Environment variable name"));
  });

  it("Ollama: the address on this computer, no key, checked and saved without one", async () => {
    const tests: AiKeyCheckRequest[] = [];
    const saves: AiKeyAddRequest[] = [];
    mockDaemon({
      ai_key_test: (a: unknown) => {
        tests.push((a as { request: AiKeyCheckRequest }).request);
        return { ok: true, reason: "ok", message: "", models: ["llama3.2:3b", "qwen3:4b"] };
      },
      ai_key_save: (a: unknown) => {
        saves.push((a as { request: AiKeyAddRequest }).request);
        return added({ ok: true, saved: true, provider_id: "ollama", models: ["llama3.2:3b"] });
      },
    });
    const { container } = await render(<ByokForm kind="ollama" />);
    expect(field(container, "Address (base URL)").value).toBe("http://127.0.0.1:11434/v1");
    expect(byText(container, "label", "API key")).toBeUndefined();
    expect(field(container, "Name").value).toBe("ollama");
    await click(byText(container, "button", "Check connection"));
    expect(tests).toEqual([{ kind: "openai_compatible", base_url: "http://127.0.0.1:11434/v1", model: null }]);
    const check = container.querySelector(".key-field")!;
    expect(check.getAttribute("data-state")).toBe("valid");
    expect(check.querySelector(".key-field-status")?.textContent).toBe("✓Works: Ollama, 2 models available");
    await type(field(container, "Model"), "llama3.2:3b");
    await click(byText(container, "button", "Check and save"));
    expect(saves).toEqual([{ name: "ollama", kind: "openai_compatible", base_url: "http://127.0.0.1:11434/v1", model: "llama3.2:3b" }]);
    expect(await violations()).toEqual([]);
  });

  it("OpenRouter: its address and name preset, a key, and a link to get one", async () => {
    const tests: AiKeyCheckRequest[] = [];
    const opened: string[] = [];
    mockDaemon({
      ai_key_test: (a: unknown) => {
        tests.push((a as { request: AiKeyCheckRequest }).request);
        return { ok: true, reason: "ok", message: "", models: ["a/b"] };
      },
      open_url: (a: unknown) => {
        opened.push((a as { url: string }).url);
        return "opened";
      },
    });
    const { container } = await render(<ByokForm kind="openrouter" />);
    expect(field(container, "Address (base URL)").value).toBe("https://openrouter.ai/api/v1");
    expect(field(container, "Name").value).toBe("openrouter");
    await click(byText(container, "button", "Get a key from OpenRouter"));
    expect(opened).toEqual(["https://openrouter.ai/keys"]);
    // Without a key, nothing is sent and the field says why.
    await click(byText(container, "button", "Test key"));
    expect(tests).toEqual([]);
    expect(container.querySelector(".key-field")?.getAttribute("data-state")).toBe("invalid");
    await type(field(container, "API key"), "sk-or-1");
    expect(container.querySelector(".key-field")?.getAttribute("data-state")).toBe("idle");
    await click(byText(container, "button", "Test key"));
    expect(tests).toEqual([{ kind: "openai_compatible", base_url: "https://openrouter.ai/api/v1", model: null, api_key: "sk-or-1" }]);
    expect(container.querySelector(".key-field-status")?.textContent).toBe("✓Works: OpenRouter, 1 model available");
  });

  it("any OpenAI-compatible server: an address, and a key only if it needs one", async () => {
    const tests: AiKeyCheckRequest[] = [];
    mockDaemon({
      ai_key_test: (a: unknown) => {
        tests.push((a as { request: AiKeyCheckRequest }).request);
        return { ok: true, reason: "ok", message: "", models: [] };
      },
    });
    const { container } = await render(<ByokForm kind="compatible" />);
    expect(field(container, "API key").required).toBe(false);
    expect(container.textContent).toContain("Leave this empty if your server doesn't need a key.");
    await type(field(container, "Address (base URL)"), "http://10.0.0.5:8080/v1");
    await click(byText(container, "button", "Test key"));
    expect(tests).toEqual([{ kind: "openai_compatible", base_url: "http://10.0.0.5:8080/v1", model: null }]);
    expect(container.querySelector(".key-field-status")?.textContent).toBe("✓Works: 10.0.0.5:8080, 0 models available");
  });

  it("Anthropic: checks a pasted key at once (checking, then valid), says why a key fails, and links to the console", async () => {
    let answer: (v: unknown) => void = () => {};
    const tests: AiKeyCheckRequest[] = [];
    const opened: string[] = [];
    mockDaemon({
      ai_key_test: (a: unknown) => {
        tests.push((a as { request: AiKeyCheckRequest }).request);
        return new Promise((r) => (answer = r));
      },
      open_url: (a: unknown) => {
        opened.push((a as { url: string }).url);
        return "opened";
      },
    });
    const { container } = await render(<ByokForm kind="anthropic" />);
    const keyField = () => container.querySelector(".key-field")!;
    expect(keyField().getAttribute("data-state")).toBe("idle");
    await click(byText(container, "button", "Get a key from Anthropic Console"));
    expect(opened).toEqual(["https://console.anthropic.com/settings/keys"]);

    const input = field(container, "API key");
    await act(async () => {
      input.dispatchEvent(new Event("paste", { bubbles: true }));
    });
    await type(input, "sk-ant-good");
    expect(tests).toEqual([{ kind: "anthropic", base_url: null, model: null, api_key: "sk-ant-good" }]);
    expect(keyField().getAttribute("data-state")).toBe("checking");
    expect(keyField().querySelector(".key-field-status")?.textContent).toBe("Checking…");
    await act(async () => answer({ ok: true, reason: "ok", message: "", models: ["claude-a", "claude-b"] }));
    await flush();
    expect(keyField().getAttribute("data-state")).toBe("valid");
    expect(keyField().querySelector(".key-field-status")?.textContent).toBe("✓Works: Anthropic, 2 models available");

    await type(input, "sk-ant-bad");
    expect(keyField().getAttribute("data-state")).toBe("idle");
    await click(byText(container, "button", "Test key"));
    await act(async () => answer({ ok: false, reason: "bad_key", message: "401", models: [] }));
    await flush();
    expect(keyField().getAttribute("data-state")).toBe("invalid");
    expect(input.getAttribute("aria-invalid")).toBe("true");
    expect(keyField().querySelector('[role="alert"]')?.textContent).toBe("The provider rejected this key. Check that you copied all of it.");
    expect(container.querySelectorAll('[role="alert"]')).toHaveLength(1);
    expect(await violations()).toEqual([]);
  });

  it("names saved providers by their preset", () => {
    expect(kindLabel("openai_compatible", "https://openrouter.ai/api/v1/")).toBe("onboarding.byok.provider.openrouter");
    expect(kindLabel("openai_compatible", "http://localhost:11434/v1")).toBe("onboarding.byok.provider.ollama");
    expect(kindLabel("openai_compatible", "http://10.0.0.5:8080/v1")).toBe("onboarding.byok.provider.compatible");
    expect(kindLabel("anthropic")).toBe("onboarding.byok.provider.anthropic");
  });
});
