// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { AiKeyAddRequest, AiKeyAddResult, AiKeySaveRequest, AiKeySaveResult, AiKeyTestRequest, AiKeyTestResult, AiKeyUpdateRequest } from "../api";
import { byText, cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import { ByokForm, ReplaceKeyForm } from "./ByokForm";

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

  it("explains a rejected key", async () => {
    mockDaemon({
      ai_key_save: () => result({ reason: "bad_key", message: "rejected" }),
    });
    const saved: string[] = [];
    const { container } = await render(<ByokForm kind="openai" onSaved={(id) => saved.push(id)} />);
    expect(field(container, "Address (base URL)").value).toBe("https://api.openai.com/v1");
    await type(field(container, "API key"), "sk-wrong");
    await type(field(container, "Model"), "m1");
    await click(byText(container, "button", "Check and save key"));
    expect(container.querySelector('[role="alert"]')?.textContent).toBe(
      "The provider rejected this key. Check that you copied all of it.",
    );
    expect(container.querySelector('[role="status"]')?.textContent).toBe("");
    // A failed save keeps the key so it can be corrected, and sets nothing up.
    expect(field(container, "API key").value).toBe("sk-wrong");
    expect(saved).toEqual([]);
  });

  it("tests a key and reports how many models the provider has", async () => {
    const tests: AiKeyTestRequest[] = [];
    mockDaemon({
      ai_key_test: (args: unknown): AiKeyTestResult => {
        tests.push((args as { request: AiKeyTestRequest }).request);
        return { ok: true, reason: "ok", message: "", models: ["a", "b", "c"] };
      },
    });
    const { container } = await render(<ByokForm />);
    await click(byText(container, "button", "Test key"));
    // No key yet: nothing is sent.
    expect(container.querySelector('[role="alert"]')?.textContent).toBe("Enter your API key.");
    expect(tests).toEqual([]);

    await type(field(container, "API key"), "sk-ant-x");
    await click(byText(container, "button", "Test key"));
    // The provider's own address is sent as "default" (null).
    expect(tests).toEqual([{ kind: "anthropic", base_url: null, api_key: "sk-ant-x", model: null }]);
    expect(container.querySelector('[role="status"]')?.textContent).toBe("Key works. Found 3 models.");
    expect(container.querySelector("datalist")?.querySelectorAll("option")).toHaveLength(3);
  });

  it("offers to replace the key of a provider that already exists", async () => {
    const updates: AiKeyUpdateRequest[] = [];
    mockDaemon({
      ai_key_save: () => added({ reason: "exists", message: "taken" }),
      ai_key_update: (args: unknown) => {
        updates.push((args as { request: AiKeyUpdateRequest }).request);
        return added({ ok: true, saved: true, provider_id: "custom" });
      },
    });
    const saved: string[] = [];
    const { container } = await render(<ByokForm kind="compatible" onSaved={(id) => saved.push(id)} />);
    await type(field(container, "Address (base URL)"), "http://127.0.0.1:9999/v1");
    await type(field(container, "API key"), "sk-rotated");
    await type(field(container, "Model"), "m1");
    await click(byText(container, "button", "Check and save key"));
    expect(container.querySelector('[role="alert"]')?.textContent).toBe(
      "A provider with this name is already set up. Replace its key, or choose another name.",
    );
    expect(saved).toEqual([]);

    // It says what Replace key keeps (the kind and model typed here are not sent).
    expect(container.textContent).toContain("Replace key keeps the type and model of “custom” as they are");
    await click(byText(container, "button", "Replace key"));
    expect(updates).toEqual([{ name: "custom", base_url: "http://127.0.0.1:9999/v1", api_key: "sk-rotated" }]);
    expect(container.querySelector('[role="status"]')?.textContent).toBe("Key checked and replaced. Nugget uses the new key now.");
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(byText(container, "button", "Replace key")).toBeUndefined();
    expect(field(container, "API key").value).toBe("");
    expect(saved).toEqual(["custom"]);
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

  it("explains a variable the daemon cannot see", async () => {
    mockDaemon({ ai_key_test: () => ({ ok: false, reason: "env_missing", message: "raw", models: [] }) });
    const { container } = await render(<ByokForm kind="anthropic" />);
    await click(byText(container, "button", "Use an environment variable instead"));
    await type(field(container, "Environment variable name"), "ANTHROPIC_API_KEY");
    await click(byText(container, "button", "Test key"));
    expect(container.querySelector('[role="alert"]')?.textContent).toMatch(/^Nugget can't use this environment variable/);
  });
});

describe("ReplaceKeyForm", () => {
  it("checks and replaces the key", async () => {
    const updates: AiKeyUpdateRequest[] = [];
    mockDaemon({
      ai_key_update: (args: unknown) => {
        const r = (args as { request: AiKeyUpdateRequest }).request;
        updates.push(r);
        return r.api_key === "sk-bad" ? added({ reason: "bad_key" }) : added({ ok: true, saved: true, provider_id: "custom" });
      },
    });
    let done = 0;
    let cancelled = 0;
    const { container } = await render(
      <ReplaceKeyForm provider={{ id: "custom", key: "keychain", env_var: null }} onDone={() => done++} onCancel={() => cancelled++} />,
    );
    await click(byText(container, "button", "Replace key"));
    expect(container.querySelector('[role="alert"]')?.textContent).toBe("Enter your API key.");
    await type(field(container, "API key"), "sk-bad");
    await click(byText(container, "button", "Replace key"));
    expect(container.querySelector('[role="alert"]')?.textContent).toBe("The provider rejected this key. Check that you copied all of it.");
    expect(done).toBe(0);
    await type(field(container, "API key"), "sk-good");
    await click(byText(container, "button", "Replace key"));
    expect(updates.at(-1)).toEqual({ name: "custom", api_key: "sk-good" });
    expect(done).toBe(1);
    await click(byText(container, "button", "Cancel"));
    expect(cancelled).toBe(1);
  });

  it("keeps a variable-backed provider on its variable", async () => {
    const updates: AiKeyUpdateRequest[] = [];
    mockDaemon({
      ai_key_update: (args: unknown) => {
        updates.push((args as { request: AiKeyUpdateRequest }).request);
        return added({ ok: true, saved: true, provider_id: "work" });
      },
    });
    let done = 0;
    const env = await render(<ReplaceKeyForm provider={{ id: "work", key: "env", env_var: "WORK_KEY" }} onDone={() => done++} onCancel={() => {}} />);
    expect(field(env.container, "Environment variable name").value).toBe("WORK_KEY");
    await click(byText(env.container, "button", "Replace key"));
    expect(updates.at(-1)).toEqual({ name: "work", key_ref: "env:WORK_KEY" });
    expect(done).toBe(1);
  });
});
