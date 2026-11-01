// @vitest-environment jsdom
/**
 * KK1: model download failures read as plain language (by reason, localised), keep the staged
 * bytes, and both the Models page and onboarding offer Cancel while downloading and Retry /
 * Resume after.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { emit } from "@tauri-apps/api/event";
import { act } from "react";
import type { ModelInfo, ModelProgress } from "../api";
import { t } from "../i18n";
import { failureMessage } from "../onboarding/DownloadFailure";
import { Onboarding } from "../onboarding/Onboarding";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { ModelsPage } from "./Models";

beforeEach(installDomShims);
afterEach(cleanup);

const ID = "qwen3-4b-q4km";
const MODEL: ModelInfo = { id: ID, size_bytes: 2_000_000_000, installed: false, recommended: true, tiers: ["light", "synthesis"] };
const progress = (over: Partial<ModelProgress>): ModelProgress => ({ id: ID, downloaded: 0, total: 2_000_000_000, phase: "downloading", error: null, ...over });
const send = (p: ModelProgress) => act(() => emit("nugget:models-progress", p));

describe("failureMessage", () => {
  it("localises by reason, and falls back to the daemon's text or a generic line", () => {
    const failed = (over: Partial<ModelProgress>) => progress({ phase: "failed", error: "daemon text", ...over });
    expect(failureMessage(t, failed({ reason: "offline" }), "x")).toBe(t("models.fail.offline"));
    expect(failureMessage(t, failed({ reason: "http_error", http_status: 500 }), "x")).toBe(
      "The download server returned an error (HTTP 500). Try again later.",
    );
    expect(failureMessage(t, failed({ reason: "captive_portal" }), "x")).toContain("sign in");
    expect(failureMessage(t, failed({ reason: "stalled" }), "x")).toBe(t("models.fail.stalled"));
    // An older daemon (no reason) or a newer reason: its own plain text.
    expect(failureMessage(t, failed({}), "x")).toBe("daemon text");
    expect(failureMessage(t, failed({ reason: "solar_flare" }), "x")).toBe("daemon text");
    expect(failureMessage(t, failed({ reason: "other" }), "fallback")).toBe("fallback");
  });
});

describe("Models page: download controls", () => {
  it("cancels a download, keeps the bytes, and resumes with focus on Resume", async () => {
    const cancels: unknown[] = [];
    const installs: unknown[] = [];
    mockDaemon({
      models_list: [MODEL],
      models_fit: () => {
        throw { kind: "rpc", message: "method not found", code: -32601 };
      },
      models_install: (a: unknown) => installs.push(a),
      models_cancel: (a: unknown) => {
        cancels.push(a);
        return { cancelled: true };
      },
    });
    const { container } = await render(<ModelsPage />);
    await flush();
    await click(byText(container, "li.card button", "Install"));
    await send(progress({ downloaded: 500_000_000 }));
    await flush();

    const cancel = container.querySelector<HTMLButtonElement>('li.card button[aria-label="Cancel downloading Qwen3 4B"]')!;
    expect(cancel.textContent).toBe("Cancel");
    cancel.focus();
    await click(cancel);
    expect(cancels).toEqual([{ id: ID }]);
    expect(cancel.disabled).toBe(true);

    await send(progress({ phase: "failed", downloaded: 500_000_000, reason: "cancelled", error: "Download cancelled." }));
    await flush();
    const note = container.querySelector("li.card .note")!;
    expect(note.getAttribute("role")).toBe("status");
    expect(note.textContent).toBe("Download cancelled. 500 MB downloaded so far is kept: the next try continues from there.");
    const resume = byText(container, "li.card button", "Resume download")!;
    expect(document.activeElement).toBe(resume);
    expect(container.querySelector('button[aria-label^="Cancel downloading"]')).toBeNull();

    await click(resume);
    expect(installs).toHaveLength(2);
    // The staged bytes stay on screen until the daemon reports.
    expect(container.querySelector("progress")?.getAttribute("value")).toBe("25");
  });

  it("shows a plain failure as an alert with Retry, never the raw daemon error", async () => {
    mockDaemon({
      models_list: [MODEL],
      models_fit: () => {
        throw { kind: "rpc", message: "method not found", code: -32601 };
      },
      models_install: null,
    });
    const { container } = await render(<ModelsPage />);
    await flush();
    await click(byText(container, "li.card button", "Install"));
    await send(progress({ phase: "failed", downloaded: 0, reason: "offline", error: "network error: http://127.0.0.1/x" }));
    await flush();
    const alert = container.querySelector('li.card [role="alert"]')!;
    expect(alert.textContent).toBe(t("models.fail.offline"));
    expect(container.textContent).not.toContain("127.0.0.1");
    expect(byText(container, "li.card button", "Retry install")).toBeDefined();
  });
});

describe("Onboarding AI step: download controls", () => {
  it("offers Cancel while downloading and Retry after a failure", async () => {
    const cancels: unknown[] = [];
    mockDaemon({
      models_cancel: (a: unknown) => {
        cancels.push(a);
        return { cancelled: true };
      },
    });
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    await click(byText(container, "button", "Continue"));
    await click(container.querySelector<HTMLInputElement>('input[type="radio"][value="local"]'));
    await flush();
    await click(byText(container, "li.card button", "Install"));
    const id = "qwen3-4b-q4";
    await act(() => emit("nugget:models-progress", { id, downloaded: 100, total: 1000, phase: "downloading", error: null }));
    await flush();
    const cancel = container.querySelector<HTMLButtonElement>(`li.card button[aria-label="Cancel downloading ${id}"]`)!;
    await click(cancel);
    expect(cancels).toEqual([{ id }]);

    await act(() =>
      emit("nugget:models-progress", { id, downloaded: 100, total: 1000, phase: "failed", error: "x", reason: "http_error", http_status: 503 }),
    );
    await flush();
    expect(container.querySelector('li.card [role="alert"]')?.textContent).toContain("HTTP 503");
    expect(byText(container, "li.card button", "Retry install")).toBeDefined();
  });
});
