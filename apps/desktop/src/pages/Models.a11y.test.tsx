// @vitest-environment jsdom
/**
 * LL2: the Models page's KK1/KK2 states for assistive technology: a download in progress, failed
 * and cancelled (no axe violations, keyboard focus kept as Install turns into Cancel and Cancel
 * goes away, the finished install said once), and "Your providers" while it loads, fails, or
 * lists a provider that isn't in use (in plain words, the daemon's line under Details).
 */
import axe from "axe-core";
import { emit } from "@tauri-apps/api/event";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { ModelInfo, ModelProgress, SavedKeyProvider } from "../api";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { Onboarding } from "../onboarding/Onboarding";
import { ModelsPage, YourProviders } from "./Models";

beforeEach(installDomShims);
afterEach(cleanup);

const ID = "qwen3-4b-q4km";
const MODEL: ModelInfo = { id: ID, size_bytes: 2_000_000_000, installed: false, recommended: true, tiers: ["light", "synthesis"] };
const progress = (over: Partial<ModelProgress>): ModelProgress => ({ id: ID, downloaded: 0, total: 2_000_000_000, phase: "downloading", error: null, ...over });
const send = (p: ModelProgress) => act(() => emit("nugget:models-progress", p));
const noFit = () => {
  throw { kind: "rpc", message: "method not found", code: -32601 };
};

async function violations(): Promise<string[]> {
  const r = await axe.run(document.body, { rules: { "color-contrast": { enabled: false }, region: { enabled: false } } });
  return r.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
}

describe("Models downloads (LL2)", () => {
  it.each([
    ["downloading", progress({ downloaded: 500_000_000 })],
    ["failed", progress({ phase: "failed", downloaded: 500_000_000, reason: "offline", error: "x" })],
    ["cancelled", progress({ phase: "failed", downloaded: 500_000_000, reason: "cancelled", error: "x" })],
    ["failed with a disk full", progress({ phase: "failed", downloaded: 0, reason: "disk_full", error: "x" })],
  ] as const)("has no violations while %s", async (_name, p) => {
    mockDaemon({ models_list: [MODEL], models_fit: noFit, models_install: null });
    const { container } = await render(<ModelsPage />);
    await flush();
    await click(byText(container, "li.card button", "Install"));
    await send(p);
    await flush();
    expect(container.querySelector("li.card")!.textContent).not.toMatch(/undefined|\{"/);
    expect(await violations()).toEqual([]);
  });

  it("a refused install request hands focus back to Retry install", async () => {
    mockDaemon({
      models_list: [MODEL],
      models_fit: noFit,
      models_install: () => {
        throw { kind: "rpc", message: "Not enough disk space for this model.", code: -32000 };
      },
    });
    const { container } = await render(<ModelsPage />);
    await flush();
    const install = byText(container, "li.card button", "Install")!;
    install.focus();
    await click(install);
    await flush();
    expect(container.querySelector('li.card [role="alert"]')?.textContent).toContain("Not enough disk space");
    expect(document.activeElement?.textContent).toBe("Retry install");
  });

  it("moves focus from Install to Cancel, then to the card's title once installed, and says so", async () => {
    let installed = false;
    mockDaemon({ models_list: () => [{ ...MODEL, installed }], models_fit: noFit, models_install: null });
    const { container } = await render(<ModelsPage />);
    await flush();
    const install = byText(container, "li.card button", "Install")!;
    install.focus();
    await click(install);
    await send(progress({ downloaded: 100 }));
    expect(document.activeElement?.getAttribute("aria-label")).toBe("Cancel downloading Qwen3 4B");
    const live = container.querySelector('li.card .visually-hidden[role="status"]')!;
    expect(live.textContent).toBe("");
    installed = true;
    await send(progress({ phase: "installed", downloaded: 2_000_000_000 }));
    await flush();
    expect(container.querySelector('button[aria-label^="Cancel downloading"]')).toBeNull();
    expect(document.activeElement?.textContent).toBe("Qwen3 4B");
    expect(document.activeElement?.closest("li.card")).not.toBeNull();
    expect(container.querySelector('li.card .visually-hidden[role="status"]')!.textContent).toBe("Qwen3 4B is installed.");
    expect(await violations()).toEqual([]);
  });
});

const work: SavedKeyProvider = {
  id: "work",
  kind: "anthropic",
  base_url: null,
  model: "m2",
  key: "env",
  env_var: "WORK_KEY",
  available: false,
  error: "provider work: api_key env:WORK_KEY: environment variable WORK_KEY is not set",
};

describe("Your providers states (LL2)", () => {
  it("names Replace key and Remove with their visible words first (label in name)", async () => {
    mockDaemon({ ai_key_saved: { providers: [{ ...work, id: "custom", key: "keychain", env_var: null, available: true, error: null }] } });
    const { container } = await render(<YourProviders />);
    await flush();
    expect(container.querySelector('button[aria-label="Replace key: custom"]')?.textContent).toBe("Replace key");
    // axe's label-content-name-mismatch needs layout (it runs in the e2e audit); checked directly here.
    for (const b of Array.from(container.querySelectorAll<HTMLButtonElement>("li.card button[aria-label]"))) {
      expect(b.getAttribute("aria-label")!.toLowerCase()).toContain(b.textContent!.toLowerCase());
    }
  });

  it("shows a named loading state, never a blank section", async () => {
    mockDaemon({ ai_key_saved: () => new Promise(() => {}) });
    const { container } = await render(<YourProviders />);
    const status = container.querySelector('.pg-loading[role="status"]');
    expect(status?.textContent).toContain("Checking your providers");
    expect(await violations()).toEqual([]);
  });

  it("shows a failure as a plain alert with Retry", async () => {
    mockDaemon({
      ai_key_saved: () => {
        throw { kind: "disconnected", message: "connection reset by peer (os error 104)" };
      },
    });
    const { container } = await render(<YourProviders />);
    await flush();
    const alert = container.querySelector('[role="alert"]')!;
    expect(alert).not.toBeNull();
    expect(Array.from(alert.querySelectorAll("button")).map((b) => b.textContent)).toContain("Retry");
    expect(container.querySelector(".pg-loading")).toBeNull();
    expect(await violations()).toEqual([]);
  });

  it("says why a provider isn't in use in plain words, with the daemon's line under Details", async () => {
    mockDaemon({ ai_key_saved: { providers: [work, { ...work, id: "other", key: "keychain", env_var: null, error: "provider other: base_url: relative URL without a base" }] } });
    const { container } = await render(<YourProviders />);
    await flush();
    const [envCard, otherCard] = Array.from(container.querySelectorAll("li.card"));
    const visible = (el: Element) => Array.from(el.querySelectorAll("p")).filter((p) => !p.closest("details")).map((p) => p.textContent).join(" ");
    expect(visible(envCard!)).toContain("the environment variable WORK_KEY isn't set where Nugget runs.");
    expect(visible(envCard!)).not.toContain("api_key env:");
    expect(envCard!.querySelector("details")!.textContent).toContain("api_key env:WORK_KEY");
    expect(visible(otherCard!)).toContain("Nugget can't use this provider with its current settings.");
    expect(visible(otherCard!)).not.toContain("relative URL");
    expect(await violations()).toEqual([]);
  });
});

describe("Onboarding download (LL2)", () => {
  it("moves focus from Install to Cancel to the model's name, and its failure has no violations", async () => {
    mockDaemon({ models_install: null });
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    await click(byText(container, "button", "Continue"));
    await click(container.querySelector<HTMLInputElement>('input[type="radio"][value="local"]'));
    await flush();
    const id = "qwen3-4b-q4";
    const install = byText(container, "li.card button", "Install")!;
    install.focus();
    await click(install);
    await act(() => emit("nugget:models-progress", { id, downloaded: 100, total: 1000, phase: "downloading", error: null }));
    await flush();
    expect(document.activeElement?.getAttribute("aria-label")).toBe(`Cancel downloading ${id}`);
    await act(() => emit("nugget:models-progress", { id, downloaded: 1000, total: 1000, phase: "installed", error: null }));
    await flush();
    expect(document.activeElement?.textContent).toBe(id);

    cleanup();
    installDomShims();
    mockDaemon({ models_install: null });
    const again = await render(<Onboarding connected onExit={() => {}} />);
    await click(byText(again.container, "button", "Continue"));
    await click(again.container.querySelector<HTMLInputElement>('input[type="radio"][value="local"]'));
    await flush();
    await click(byText(again.container, "li.card button", "Install"));
    await act(() => emit("nugget:models-progress", { id, downloaded: 100, total: 1000, phase: "failed", error: "x", reason: "stalled" }));
    await flush();
    expect(again.container.querySelector('li.card [role="alert"]')).not.toBeNull();
    expect(await violations()).toEqual([]);
  });
});
