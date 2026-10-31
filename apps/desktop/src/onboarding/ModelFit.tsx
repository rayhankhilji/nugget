import { modelFitApi, type FitReport, type GpuKind, type ModelFit } from "../api";
import { formatBytes } from "../format";
import { useT, type MessageKey } from "../i18n";

/**
 * `models.fit`, or `null` when it cannot be had (an older daemon without the method, or any other
 * failure): explanations are extra, so their absence never shows an error.
 */
export async function loadFit(): Promise<FitReport | null> {
  try {
    return await modelFitApi.modelsFit();
  } catch {
    return null;
  }
}

const GPU: Record<GpuKind, MessageKey> = {
  apple_silicon: "onboarding.models.fit.gpu.apple_silicon",
  nvidia: "onboarding.models.fit.gpu.nvidia",
  amd: "onboarding.models.fit.gpu.amd",
  intel: "onboarding.models.fit.gpu.intel",
  unknown: "onboarding.models.fit.gpu.unknown",
};

/** "This computer: 16 GB memory, NVIDIA graphics." */
export function MachineLine({ report }: { report: FitReport }) {
  const t = useT();
  return (
    <p className="muted small ob-machine">
      {t("onboarding.models.fit.machine", { ram: report.ram_gb, gpu: t(GPU[report.hardware.gpu] ?? GPU.unknown) })}
    </p>
  );
}

/** What a model needs, why it is recommended, and a warning when this machine falls short. */
export function FitNotes({
  fit,
  freeDisk,
  installed = false,
  why = true,
}: {
  fit: ModelFit;
  freeDisk: number | null;
  /** Already installed: disk space no longer matters. */
  installed?: boolean;
  /** Explain why it is recommended (onboarding says so on its one suggested model only). */
  why?: boolean;
}) {
  const t = useT();
  const gpu = t(GPU[fit.gpu] ?? GPU.unknown);
  return (
    <>
      <p className="muted small ob-fit-needs">
        {t("onboarding.models.fit.needs", { ram: fit.min_ram_gb, disk: formatBytes(fit.disk_needed_bytes) })}
      </p>
      {why && fit.recommended && fit.fits_ram && (
        <p className="small ob-fit-why">
          {fit.reasons.includes("gpu_accelerated")
            ? t("onboarding.models.fit.whyGpu", { ram: fit.machine_ram_gb, gpu })
            : t("onboarding.models.fit.whyCpu", { ram: fit.machine_ram_gb })}
        </p>
      )}
      {!fit.fits_ram && (
        <p className="note note-warn small ob-fit-warn">
          {t("onboarding.models.fit.ramTooSmall", { ram: fit.machine_ram_gb, need: fit.min_ram_gb })}
        </p>
      )}
      {fit.fits_disk === false && !installed && (
        <p className="note note-warn small ob-fit-warn">
          {t("onboarding.models.fit.diskTooSmall", {
            disk: formatBytes(fit.disk_needed_bytes),
            free: formatBytes(freeDisk ?? 0),
          })}
        </p>
      )}
    </>
  );
}
