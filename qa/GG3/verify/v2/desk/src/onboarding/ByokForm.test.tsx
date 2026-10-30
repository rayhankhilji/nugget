// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { AiKeySaveRequest, AiKeySaveResult, AiKeyTestRequest, AiKeyTestResult } from "../api";
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
  restart_required: true,
  ...r,
});

describe("ByokForm", () => {
  it("saves a key that works, says a restart is needed and forgets the key", async () => {
    const saves: AiKeySaveRequest[] = [];
    mockDaemon({
      ai_key_save: (args: unknown) => {
        saves.push((args as { request: AiKeySaveRequest }).request);
        return result({ ok: true, models: ["m1"], saved: true, provider_id: "custom" });
      },
    });
    const { container } = await render(<ByokForm kind="compatible" />);
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
    expect(container.querySelector('[role="status"]')?.textContent).toBe("Key checked and saved. Restart Nugget to start using it.");
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(field(container, "API key").value).toBe("");
    expect(await violations()).toEqual([]);
  });

  it("explains a rejected key", async () => {
    mockDaemon({
      ai_key_save: () => result({ reason: "bad_key", message: "rejected" }),
    });
    const { container } = await render(<ByokForm kind="openai" />);
    expect(field(container, "Address (base URL)").value).toBe("https://api.openai.com/v1");
    await type(field(container, "API key"), "sk-wrong");
    await type(field(container, "Model"), "m1");
    await click(byText(container, "button", "Check and save key"));
    expect(container.querySelector('[role="alert"]')?.textContent).toBe(
      "The provider rejected this key. Check that you copied all of it.",
    );
    expect(container.querySelector('[role="status"]')?.textContent).toBe("");
    // A failed save keeps the key so it can be corrected.
    expect(field(container, "API key").value).toBe("sk-wrong");
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
});
