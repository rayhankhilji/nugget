import { useCallback, useEffect, useId, useRef, useState } from "react";
import { practiceApi, type DoubleTapSettings, type DoubleTapStatus, type PracticeAction } from "../api";
import { useEvent } from "../hooks";
import { useT } from "../i18n";
import { triggerLabel } from "../onboarding/TriggerStep";
import { detectOs } from "../onboarding/platform";
import { fromSpec } from "../onboarding/trigger";

/** The trigger as the user presses it: the desktop's own binding when it set one, else the saved trigger. */
function triggerText(settings: DoubleTapSettings | undefined, status: DoubleTapStatus | undefined): string | null {
  if (status?.shortcut?.state === "active" && status.shortcut.trigger) return status.shortcut.trigger;
  return settings ? triggerLabel(fromSpec(settings.trigger), detectOs()) : null;
}

/** Bring the trigger settings into view and put focus on their first control. */
function goToTrigger() {
  const heading = document.getElementById("dt-trigger");
  const section = heading?.closest("section");
  heading?.scrollIntoView?.({ block: "start" });
  section?.querySelector<HTMLElement>("input, select, button")?.focus();
}

/**
 * "Try your shortcut": a practice field on the Double Tap page. While it has focus the shell knows
 * about it (`doubletap_practice`), so the trigger drafts a reply to the sample message and puts the
 * accepted draft here instead of in another app. Older shells ignore all of this.
 */
export function DoubleTapPractice({ settings, status }: { settings: DoubleTapSettings | undefined; status: DoubleTapStatus | undefined }) {
  const t = useT();
  const fieldId = useId();
  const sampleId = useId();
  const hintId = useId();
  const field = useRef<HTMLTextAreaElement>(null);
  const [value, setValue] = useState("");
  const [said, setSaid] = useState("");
  const sample = t("doubletap.practice.sample");

  /** Tell the shell where the practice field is and what's in it. */
  const send = useCallback(() => {
    const el = field.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const rtl = getComputedStyle(el).direction === "rtl";
    practiceApi
      .set({
        value: el.value,
        selection: [el.selectionStart ?? el.value.length, el.selectionEnd ?? el.value.length],
        rect: { x: r.x, y: r.y, width: r.width, height: r.height },
        rtl,
        sample,
      })
      .catch(() => {});
  }, [sample]);

  const release = useCallback(() => {
    practiceApi.set(null).catch(() => {});
  }, []);

  // Leaving the page while the field has focus.
  useEffect(() => release, [release]);

  useEvent(practiceApi.onAction, (action: PracticeAction) => {
    const el = field.current;
    if (!el) return;
    if (action.kind === "insert") {
      const start = el.selectionStart ?? el.value.length;
      const end = el.selectionEnd ?? el.value.length;
      const next = el.value.slice(0, start) + action.text + el.value.slice(end);
      setValue(next);
      setSaid(t("doubletap.practice.inserted"));
      el.focus();
      // After React has written the new value: put the caret after the draft.
      const caret = start + action.text.length;
      requestAnimationFrame(() => {
        try {
          el.setSelectionRange(caret, caret);
          send();
        } catch {
          /* detached */
        }
      });
    } else {
      el.focus();
    }
  });

  const trigger = triggerText(settings, status);
  const off = settings !== undefined && !settings.enabled;
  const deaf = !off && status !== undefined && !status.listening;

  return (
    <section aria-labelledby="dt-practice" className="dt-practice">
      <h2 id="dt-practice">{t("doubletap.practice.title")}</h2>
      <p>{trigger ? t("doubletap.practice.intro", { trigger }) : t("doubletap.practice.introNoTrigger")}</p>
      <p className="muted small">{t("doubletap.practice.private")}</p>
      {(off || deaf) && (
        <div className="note" role="note">
          <p>{off ? t("doubletap.practice.off") : t("doubletap.practice.notListening")}</p>
          <button type="button" className="btn btn-ghost btn-small" onClick={goToTrigger}>
            {t("doubletap.practice.goToTrigger")}
          </button>
        </div>
      )}
      <figure className="dt-practice-sample">
        <figcaption id={sampleId} className="muted small">
          {t("doubletap.practice.sampleFrom")}
        </figcaption>
        <blockquote dir="auto">{sample}</blockquote>
      </figure>
      <div className="field">
        <label htmlFor={fieldId}>{t("doubletap.practice.fieldLabel")}</label>
        <textarea
          id={fieldId}
          ref={field}
          dir="auto"
          rows={4}
          value={value}
          aria-describedby={`${sampleId} ${hintId}`}
          onChange={(e) => {
            setValue(e.target.value);
            setSaid("");
          }}
          onFocus={send}
          onInput={send}
          onSelect={send}
          onKeyUp={send}
          onBlur={release}
        />
        <p id={hintId} className="muted small">
          {t("doubletap.practice.fieldHint")}
        </p>
      </div>
      <div className="inline-row">
        <button
          type="button"
          className="btn btn-ghost btn-small"
          disabled={!value}
          onClick={() => {
            setValue("");
            setSaid("");
            field.current?.focus();
          }}
        >
          {t("doubletap.practice.clear")}
        </button>
        <span className="muted small" role="status">
          {said}
        </span>
      </div>
    </section>
  );
}
