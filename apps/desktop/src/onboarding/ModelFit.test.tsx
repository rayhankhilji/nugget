// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { createRef } from "react";
import { byText, cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import type { FitReport, ModelFit } from "../api";
import { ModelsPage } from "../pages/Models";
import { AiStep } from "./AiStep";
import { INITIAL } from "./machine";

beforeEach(() => {
  installDomShims();
});
afterEach(cleanup);

const GB = 1_000_000_000;

function fit(over: Partial<ModelFit> & { id: string }): ModelFit {
  return {
    recommended: false,
    tiers: ["light"],
    size_bytes: 2.5 * GB,
    min_ram_gb: 8,
    disk_needed_bytes: 4 * GB,
    fits_ram: true,
    fits_disk: true,
    machine_ram_gb: 16,
    gpu: "nvidia",
    accel: "cuda",
    reasons: ["ram_fits", "gpu_accelerated"],
    ...over,
  };
}

function report(models: ModelFit[], gpu: FitReport["hardware"]["gpu"] = "nvidia"): FitReport {
  return {
    hardware: { total_ram: 15.2 * 2 ** 30, available_ram: 8 * 2 ** 30, cpu_cores: 8, gpu },
    ram_gb: 16,
    free_disk_bytes: 3 * GB,
    models,
  };
}

const renderAiStep = () =>
  render(<AiStep headingRef={createRef()} state={{ ...INITIAL, step: "ai", ai: "local" }} dispatch={() => {}} connected />);

const card = (c: HTMLElement, id: string) =>
  Array.from(c.querySelectorAll<HTMLElement>("li.card")).find((li) => li.querySelector(".card-title")?.textContent === id);

describe("model fit in onboarding", () => {
  it("explains why the recommended model is recommended and what it needs", async () => {
    mockDaemon({
      models_fit: report([
        fit({ id: "qwen3-4b-q4", recommended: true, reasons: ["ram_fits_largest", "gpu_accelerated"] }),
        fit({ id: "nomic-embed", recommended: true, min_ram_gb: 4, disk_needed_bytes: 1.2 * GB }),
      ]),
    });
    const { container } = await renderAiStep();
    expect(container.textContent).toContain("This computer: 16 GB memory, NVIDIA graphics.");
    const qwen = card(container, "qwen3-4b-q4")!;
    expect(qwen.textContent).toContain("Needs 8 GB of memory and 4.0 GB of free disk space.");
    expect(qwen.textContent).toContain(
      "Recommended for this computer: it has 16 GB of memory, and its NVIDIA graphics speeds the model up.",
    );
    expect(qwen.querySelector(".note-warn")).toBeNull();
    // The existing card structure is intact.
    expect(qwen.querySelector("button")?.textContent).toBe("Install");
  });

  it("says a CPU-only machine runs the model more slowly", async () => {
    mockDaemon({
      models_fit: report(
        [fit({ id: "qwen3-4b-q4", recommended: true, gpu: "unknown", accel: "cpu", reasons: ["ram_fits_largest", "cpu_only_slower"] })],
        "unknown",
      ),
    });
    const { container } = await renderAiStep();
    expect(container.textContent).toContain("This computer: 16 GB memory, no graphics card detected.");
    expect(card(container, "qwen3-4b-q4")!.textContent).toContain("runs on the processor");
  });

  it("warns when a model does not fit memory or disk", async () => {
    mockDaemon({
      models_fit: report([
        fit({ id: "qwen3-4b-q4", recommended: true, fits_disk: false, disk_needed_bytes: 4 * GB }),
        fit({ id: "llama-8b-q4", min_ram_gb: 32, fits_ram: false, reasons: ["ram_too_small"] }),
      ]),
    });
    const { container } = await renderAiStep();
    const qwen = card(container, "qwen3-4b-q4")!;
    expect(qwen.querySelector(".note-warn")?.textContent).toBe("Not enough free disk space: 4.0 GB needed, 3.0 GB free.");

    // The model that does not fit RAM is only under "Show all".
    await click(byText(container, "button", /Show all models/));
    const llama = card(container, "llama-8b-q4")!;
    expect(llama.querySelector(".note-warn")?.textContent).toBe(
      "This computer has 16 GB of memory; this model needs 32 GB and may not run well.",
    );
    expect(llama.textContent).not.toContain("Recommended for this computer");
  });

  it("shows the models without explanations when the daemon has no models.fit", async () => {
    mockDaemon(); // models_fit answers "method not found"
    const { container } = await renderAiStep();
    expect(card(container, "qwen3-4b-q4")).toBeDefined();
    expect(container.textContent).not.toContain("This computer:");
    expect(container.textContent).not.toContain("Needs ");
    expect(container.querySelector(".note-error")).toBeNull();
  });

  it("explains models on the Models page too", async () => {
    mockDaemon({ models_fit: report([fit({ id: "qwen3-4b-q4", recommended: true, reasons: ["ram_fits_largest", "gpu_accelerated"] })]) });
    const { container } = await render(<ModelsPage />);
    expect(container.textContent).toContain("This computer: 16 GB memory, NVIDIA graphics.");
    // The Models page titles cards with a readable name; the id is on the card as data.
    expect(container.querySelector('li.card[data-model-id="qwen3-4b-q4"]')!.textContent).toContain("Needs 8 GB of memory");
  });
});
