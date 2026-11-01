/**
 * The one-time question "Check for updates automatically?", asked in setup and, until answered,
 * on the Status page's Updates card. Nothing contacts the update server before it is answered
 * (`channel_chosen` in `src-tauri/src/settings.rs`).
 */
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorMessage, inTauri } from "../api";
import { useT, type MessageKey } from "../i18n";

export type Channel = "stable" | "beta" | "off";

const LABEL: Record<Channel, MessageKey> = {
  stable: "updates.channel.stable",
  beta: "updates.channel.beta",
  off: "updates.channel.off",
};

const CHOICES: { id: Channel; label: MessageKey; primary?: boolean }[] = [
  { id: "stable", label: "updates.ask.stable", primary: true },
  { id: "beta", label: "updates.channel.beta" },
  { id: "off", label: "updates.channel.off" },
];

export function UpdateChoice({ disabled, onChoose }: { disabled?: boolean; onChoose: (channel: Channel) => void }) {
  const t = useT();
  return (
    <div className="note small" style={{ display: "block" }} role="group" aria-labelledby="update-choice-title" aria-describedby="update-choice-body">
      <p id="update-choice-title">
        <strong>{t("updates.ask.title")}</strong>
      </p>
      <p id="update-choice-body">{t("updates.ask.body")}</p>
      <div className="row" style={{ justifyContent: "flex-start", flexWrap: "wrap" }}>
        {CHOICES.map((c) => (
          <button
            key={c.id}
            type="button"
            className={c.primary ? "btn btn-primary btn-small" : "btn btn-ghost btn-small"}
            disabled={disabled}
            onClick={() => onChoose(c.id)}
          >
            {t(c.label)}
          </button>
        ))}
      </div>
    </div>
  );
}

/** The part of `updates_status` the setup question needs. */
interface ChoiceStatus {
  configured: boolean;
  settings: { channel: Channel; channel_chosen: boolean; auto_install: boolean };
}

/**
 * The question in setup: shown only in the app, for a build that can update (it has an update
 * key), while no channel has been chosen. After the answer it says what was chosen.
 */
export function UpdateQuestion() {
  const t = useT();
  const [status, setStatus] = useState<ChoiceStatus | null>(null);
  const [answer, setAnswer] = useState<Channel | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!inTauri()) return;
    let alive = true;
    invoke<ChoiceStatus | null>("updates_status")
      .then((s) => alive && setStatus(s))
      .catch(() => {
        /* no updater: nothing to ask */
      });
    return () => {
      alive = false;
    };
  }, []);

  if (answer) {
    return (
      <p className="muted" role="status">
        {t("updates.ask.chosen", { channel: t(LABEL[answer]) })}
      </p>
    );
  }
  if (!status?.configured || status.settings.channel_chosen) return null;

  const choose = async (channel: Channel) => {
    setBusy(true);
    setError(null);
    try {
      await invoke("updates_set_settings", { channel, autoInstall: status.settings.auto_install });
      setAnswer(channel);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <UpdateChoice disabled={busy} onChoose={(c) => void choose(c)} />
      {error && (
        <div className="note note-error" role="alert">
          {error}
        </div>
      )}
    </>
  );
}
