// @vitest-environment jsdom
import axe from "axe-core";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { SavedKeyProvider } from "../api";
import { byText, cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import { YourProviders } from "./Models";

beforeEach(() => installDomShims());
afterEach(cleanup);

const custom: SavedKeyProvider = {
  id: "custom",
  kind: "openai_compatible",
  base_url: "http://127.0.0.1:9/v1",
  model: "m1",
  key: "keychain",
  env_var: null,
  available: true,
  error: null,
};
const work: SavedKeyProvider = {
  id: "work",
  kind: "anthropic",
  base_url: null,
  model: "claude-x",
  key: "env",
  env_var: "WORK_KEY",
  available: false,
  error: "provider work: api_key env:WORK_KEY: environment variable WORK_KEY is not set",
};

async function violations(): Promise<string[]> {
  const result = await axe.run(document.body, { rules: { "color-contrast": { enabled: false } } });
  return result.violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

describe("Your providers (Models page)", () => {
  it("lists providers with a key and labels their actions for screen readers", async () => {
    mockDaemon({ ai_key_saved: { providers: [custom, work] } });
    const { container } = await render(<YourProviders />);
    const list = container.querySelector('ul[aria-label="Your providers"]')!;
    expect(list).not.toBeNull();
    const items = list.querySelectorAll("li");
    expect(items).toHaveLength(2);
    expect(items[0]!.textContent).toContain("In use");
    expect(items[0]!.textContent).toContain("Key stored in your keychain");
    expect(items[1]!.textContent).toContain("Not in use");
    expect(items[1]!.textContent).toContain("Key read from the environment variable WORK_KEY");
    expect(items[1]!.textContent).toContain("WORK_KEY is not set");
    expect(items[0]!.querySelector('button[aria-label="Replace the key of custom"]')?.textContent).toBe("Replace key");
    expect(items[0]!.querySelector('button[aria-label="Remove custom"]')?.textContent).toBe("Remove");
    // No restart instructions anywhere.
    expect(container.textContent).not.toMatch(/restart/i);
    expect(await violations()).toEqual([]);
  });

  it("removes a provider after confirming, with focus on Cancel first", async () => {
    let providers = [custom, work];
    const removed: unknown[] = [];
    mockDaemon({
      ai_key_saved: () => ({ providers }),
      ai_key_remove: (args: unknown) => {
        removed.push((args as { request: unknown }).request);
        providers = [work];
        return { ok: true, reason: "ok", message: "", removed: true, provider_id: "custom" };
      },
    });
    const { container } = await render(<YourProviders />);
    await click(container.querySelector('button[aria-label="Remove custom"]'));
    const dialog = document.querySelector("dialog")!;
    expect(dialog.querySelector("h2")?.textContent).toBe("Remove custom?");
    expect(dialog.textContent).toContain("deletes its key from your keychain");
    expect(document.activeElement?.textContent).toBe("Cancel");
    expect(await violations()).toEqual([]);

    // Cancel changes nothing.
    await click(byText(dialog, "button", "Cancel"));
    expect(document.querySelector("dialog")).toBeNull();
    expect(removed).toEqual([]);

    await click(container.querySelector('button[aria-label="Remove custom"]'));
    await click(byText(document.querySelector("dialog")!, "button", "Remove"));
    expect(removed).toEqual([{ name: "custom" }]);
    expect(document.querySelector("dialog")).toBeNull();
    // The card that opened the dialog is gone; focus goes to the section heading.
    expect(document.activeElement?.tagName).toBe("H2");
    expect(document.activeElement?.textContent).toBe("Your providers");
    expect(container.querySelector('[role="status"]')?.textContent).toBe("custom was removed.");
    expect(container.querySelectorAll('ul[aria-label="Your providers"] li')).toHaveLength(1);
  });

  it("replaces a key in a dialog", async () => {
    const updates: unknown[] = [];
    mockDaemon({
      ai_key_saved: { providers: [custom] },
      ai_key_update: (args: unknown) => {
        updates.push((args as { request: unknown }).request);
        return { ok: true, reason: "ok", message: "", models: [], saved: true, provider_id: "custom", restart_required: false };
      },
    });
    const { container } = await render(<YourProviders />);
    await click(container.querySelector('button[aria-label="Replace the key of custom"]'));
    const dialog = document.querySelector("dialog")!;
    expect(dialog.querySelector("h2")?.textContent).toBe("Replace the key of custom");
    const label = byText(dialog, "label", "API key") as HTMLLabelElement;
    const input = document.getElementById(label.htmlFor) as HTMLInputElement;
    expect(input.type).toBe("password");
    expect(document.activeElement).toBe(input);
    const { act } = await import("react");
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "sk-new");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(byText(dialog, "button", "Replace key"));
    expect(updates).toEqual([{ name: "custom", api_key: "sk-new" }]);
    expect(document.querySelector("dialog")).toBeNull();
    expect(container.querySelector('[role="status"]')?.textContent).toBe("The key of custom was replaced. Nugget uses it now.");
  });

  it("is hidden on a daemon without saved-key support", async () => {
    mockDaemon({});
    const { container } = await render(<YourProviders />);
    expect(container.textContent).toBe("");
  });
});
