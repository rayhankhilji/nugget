// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { AiKeyAddRequest, AiKeyAddResult, AiKeySaveRequest, AiKeySaveResult } from "../api";
import { byText, cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import { ByokForm } from "./ByokForm";

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
      { name: "custom", kind: "openai_compatible", base_url: "http://127.0.0.1:9999/v1", api_key: "sk-e2e-good", model: "m1", light_model: null },
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
    expect(saves[1]).toEqual({ name: "openai", kind: "openai_responses", base_url: null, model: "gpt-5", light_model: "gpt-6-luna", key_ref: "env:MY_OPENAI_KEY" });
    expect(container.querySelector('[role="status"]')?.textContent).toBe("Key checked and saved. You can use it now.");

    // And back to pasting a key; focus follows to the field that appears.
    await click(byText(container, "button", "Paste the key instead"));
    expect(field(container, "API key").value).toBe("");
    expect(document.activeElement).toBe(field(container, "API key"));
    await click(byText(container, "button", "Use an environment variable instead"));
    expect(document.activeElement).toBe(field(container, "Environment variable name"));
  });

  it("a preset fills the address and a model per tier, and links to its key page", async () => {
    const saves: AiKeySaveRequest[] = [];
    const opened: string[] = [];
    mockDaemon({
      ai_key_save: (args: unknown) => {
        saves.push((args as { request: AiKeySaveRequest }).request);
        return result({ ok: true, saved: true, provider_id: "gemini" });
      },
      open_url: (args: unknown) => {
        opened.push((args as { url: string }).url);
        return { opened: true };
      },
    });
    const { container } = await render(<ByokForm kind="gemini" />);
    expect(field(container, "Address (base URL)").value).toBe("https://generativelanguage.googleapis.com/v1beta/openai");
    expect(field(container, "Model").value).toBe("gemini-4-argon");
    expect(field(container, "Model for quick tasks").value).toBe("gemini-3.5-flash-lite");
    await click(byText(container, "button", "Get a key from Google Gemini"));
    expect(opened).toEqual(["https://aistudio.google.com/apikey"]);
    await type(field(container, "API key"), "AIza-test");
    await click(byText(container, "button", "Check and save key"));
    // An OpenAI-compatible preset always sends its address (config.toml has no default for it).
    expect(saves[0]).toEqual({
      name: "gemini",
      kind: "openai_compatible",
      base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
      model: "gemini-4-argon",
      light_model: "gemini-3.5-flash-lite",
      api_key: "AIza-test",
    });
    expect(await violations()).toEqual([]);
  });

  it("a server on this computer is saved without a key", async () => {
    const saves: AiKeySaveRequest[] = [];
    mockDaemon({
      ai_key_save: (args: unknown) => {
        saves.push((args as { request: AiKeySaveRequest }).request);
        return result({ ok: true, saved: true, provider_id: "custom" });
      },
    });
    const { container } = await render(<ByokForm kind="compatible" />);
    await type(field(container, "Address (base URL)"), "http://localhost:1234/v1");
    await type(field(container, "Model"), "qwen3:4b");
    expect(field(container, "API key").required).toBe(false);
    await click(byText(container, "button", "Check and save key"));
    expect(saves[0]).toEqual({ name: "custom", kind: "openai_compatible", base_url: "http://localhost:1234/v1", model: "qwen3:4b", light_model: null });
    // A cloud address still needs a key.
    await type(field(container, "Address (base URL)"), "https://api.example.com/v1");
    await click(byText(container, "button", "Check and save key"));
    expect(saves).toHaveLength(1);
  });
});
