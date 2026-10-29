import { useEffect, useId, useRef, useState } from "react";
import { doubletapApi, errorMessage } from "../api";
import { useEvent } from "../hooks";
import { t as translate, useT, type MessageKey } from "../i18n";
import { detectOs, type Os } from "./platform";
import { StepHeading, type StepProps } from "./steps";
import {
  CHORDS,
  DEFAULT_WINDOW_MS,
  fromSpec,
  loadTrigger,
  modifiersFor,
  readTrigger,
  writeTrigger,
  type Modifier,
  type TriggerPref,
} from "./trigger";

const SPEEDS: { ms: number; label: MessageKey }[] = [
  { ms: 250, label: "onboarding.trigger.speed.fast" },
  { ms: DEFAULT_WINDOW_MS, label: "onboarding.trigger.speed.normal" },
  { ms: 500, label: "onboarding.trigger.speed.relaxed" },
];

/** A modifier's name as printed on this OS's keyboards (Option on a Mac, Alt elsewhere). */
export function modifierLabel(m: Modifier, os: Os): string {
  const key = os !== "macos" && m.endsWith("_option") ? m.replace("_option", "_alt") : m;
  return translate(`onboarding.trigger.mod.${key}` as MessageKey);
}

/** An accelerator as this OS writes it (`CmdOrCtrl` is ⌘ on a Mac, Ctrl elsewhere). */
export function acceleratorLabel(accel: string, os: Os): string {
  return accel
    .split("+")
    .map((k) => {
      if (k === "CmdOrCtrl") return os === "macos" ? "⌘" : "Ctrl";
      if (k === "Alt") return os === "macos" ? "⌥" : "Alt";
      if (k === "Shift") return os === "macos" ? "⇧" : "Shift";
      return k;
    })
    .join(os === "macos" ? "" : "+");
}

export function triggerLabel(pref: TriggerPref, os: Os): string {
  return pref.kind === "double_tap"
    ? translate("onboarding.trigger.summaryDoubleTap", { key: modifierLabel(pref.modifier, os) })
    : acceleratorLabel(pref.accelerator, os);
}

export function TriggerStep({ headingRef }: StepProps) {
  const t = useT();
  const os = detectOs();
  const name = useId();
  const keyId = useId();
  const speedId = useId();
  const chordId = useId();
  // The local copy for the first render; the shell's settings (what Double Tap runs with) next.
  const [pref, setPref] = useState<TriggerPref>(() => loadTrigger(os));
  // Remember both halves so flipping the type back and forth doesn't lose a choice.
  const [lastTap, setLastTap] = useState(() => (pref.kind === "double_tap" ? pref : loadTrigger(os)));
  const [lastChord, setLastChord] = useState<string>(pref.kind === "chord" ? pref.accelerator : CHORDS[0]);
  const [error, setError] = useState<string | null>(null);
  // Saves run one after another; only the latest one's answer is shown.
  const queue = useRef<Promise<unknown>>(Promise.resolve());
  const latest = useRef(0);

  const show = (next: TriggerPref) => {
    setPref(next);
    if (next.kind === "double_tap") setLastTap(next);
    else setLastChord(next.accelerator);
  };

  useEffect(() => {
    let alive = true;
    const seq = latest.current;
    readTrigger(os)
      .then((p) => alive && seq === latest.current && show(p))
      .catch(() => {
        /* keep the local copy; saving reports what's wrong */
      });
    return () => {
      alive = false;
    };
  }, [os]);
  // A change saved elsewhere (the Double Tap settings page, another window).
  useEvent(doubletapApi.onSettings, (s) => show(fromSpec(s.trigger)));

  const update = (next: TriggerPref) => {
    const before = pref;
    const seq = ++latest.current;
    show(next);
    setError(null);
    const run = queue.current.then(() => writeTrigger(next));
    queue.current = run.catch(() => {});
    run
      .then((saved) => {
        if (seq === latest.current) show(saved);
      })
      .catch((err: unknown) => {
        if (seq !== latest.current) return;
        show(before);
        setError(translate("doubletap.saveFailed", { error: errorMessage(err) }));
      });
  };
  const tap = lastTap.kind === "double_tap" ? lastTap : { kind: "double_tap" as const, modifier: modifiersFor(os)[0]!, window_ms: DEFAULT_WINDOW_MS };
  // The shell may hold a value onboarding doesn't offer (set in Double Tap settings): list it too.
  const modifiers = pref.kind === "double_tap" && !modifiersFor(os).includes(pref.modifier) ? [...modifiersFor(os), pref.modifier] : modifiersFor(os);
  const speeds = pref.kind === "double_tap" && !SPEEDS.some((s) => s.ms === pref.window_ms)
    ? [...SPEEDS.map((s) => ({ ms: s.ms, label: t(s.label) })), { ms: pref.window_ms, label: t("format.latency.ms", { n: pref.window_ms }) }]
    : SPEEDS.map((s) => ({ ms: s.ms, label: t(s.label) }));
  const chords: string[] = pref.kind === "chord" && !(CHORDS as readonly string[]).includes(pref.accelerator) ? [...CHORDS, pref.accelerator] : [...CHORDS];

  return (
    <>
      <StepHeading headingRef={headingRef}>{t("onboarding.trigger.heading")}</StepHeading>
      <p className="ob-lead">{t("onboarding.trigger.lead")}</p>
      <fieldset className="ob-fieldset">
        <legend className="visually-hidden">{t("onboarding.trigger.legend")}</legend>
        <div className="kind-picker ob-choices">
          <label className="kind-option">
            <input type="radio" name={name} checked={pref.kind === "double_tap"} onChange={() => update(tap)} />
            <span>
              <span className="kind-label">{t("onboarding.trigger.doubleTap")}</span>
              <span className="muted small">{t("onboarding.trigger.doubleTapBlurb")}</span>
            </span>
          </label>
          <label className="kind-option">
            <input
              type="radio"
              name={name}
              checked={pref.kind === "chord"}
              onChange={() => update({ kind: "chord", accelerator: lastChord })}
            />
            <span>
              <span className="kind-label">{t("onboarding.trigger.chord")}</span>
              <span className="muted small">{t("onboarding.trigger.chordBlurb")}</span>
            </span>
          </label>
        </div>
      </fieldset>

      {pref.kind === "double_tap" ? (
        <div className="ob-fields">
          <div className="field-inline">
            <label htmlFor={keyId}>{t("onboarding.trigger.key")}</label>
            <select
              id={keyId}
              value={pref.modifier}
              onChange={(e) => update({ ...pref, modifier: e.target.value as Modifier })}
            >
              {modifiers.map((m) => (
                <option key={m} value={m}>
                  {modifierLabel(m, os)}
                </option>
              ))}
            </select>
          </div>
          <div className="field-inline">
            <label htmlFor={speedId}>{t("onboarding.trigger.speed")}</label>
            <select
              id={speedId}
              value={pref.window_ms}
              onChange={(e) => update({ ...pref, window_ms: Number(e.target.value) })}
            >
              {speeds.map((s) => (
                <option key={s.ms} value={s.ms}>
                  {s.label}
                </option>
              ))}
            </select>
          </div>
        </div>
      ) : (
        <div className="ob-fields">
          <div className="field-inline">
            <label htmlFor={chordId}>{t("onboarding.trigger.combination")}</label>
            <select id={chordId} value={pref.accelerator} onChange={(e) => update({ kind: "chord", accelerator: e.target.value })}>
              {chords.map((c) => (
                <option key={c} value={c}>
                  {acceleratorLabel(c, os)}
                </option>
              ))}
            </select>
          </div>
        </div>
      )}

      <p className="note" role="status">
        {t("onboarding.trigger.summary", { trigger: triggerLabel(pref, os) })}
      </p>
      {error ? (
        <p className="field-error" role="alert">
          {error}
        </p>
      ) : (
        <p className="muted small">{t("onboarding.trigger.saved")}</p>
      )}
    </>
  );
}
