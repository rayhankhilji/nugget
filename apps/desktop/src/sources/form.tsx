/** Building blocks shared by the Add source wizard's forms. */
import "./sources.css";
import { useEffect, useId, useRef, type ReactNode } from "react";
import { api, ApiError, type SourceInfo, type SourceSettings } from "../api";
import { useT } from "../i18n";
import { Spinner } from "../components/ui";

export function useAutofocus<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  useEffect(() => {
    ref.current?.focus();
  }, []);
  return ref;
}

/**
 * `sources.add` once; later attempts (after a failed password or auth step) update the source
 * that was already created instead of adding a duplicate.
 */
export function useSaveSource(kind: string) {
  const created = useRef<SourceInfo | null>(null);
  return async (name: string, settings: SourceSettings): Promise<SourceInfo> => {
    const s = created.current
      ? await api.sourcesUpdate({ id: created.current.id, name, settings })
      : await api.sourcesAdd({ id: null, kind, name, settings });
    created.current = s;
    return s;
  };
}

/** Label + control + hint + error, wired with `aria-describedby` / `aria-invalid`. */
export function Field({
  label,
  hint,
  error,
  optional,
  children,
}: {
  label: string;
  hint?: ReactNode;
  error?: string | null;
  optional?: boolean;
  children: (a: { id: string; "aria-describedby"?: string; "aria-invalid"?: true }) => ReactNode;
}) {
  const t = useT();
  const id = useId();
  const hintId = `${id}-hint`;
  const errId = `${id}-err`;
  const describedBy = [hint ? hintId : "", error ? errId : ""].filter(Boolean).join(" ") || undefined;
  return (
    <div className="field">
      <label htmlFor={id}>
        {label}
        {optional && <span className="muted small"> {t("addSource.optional")}</span>}
      </label>
      {children({ id, "aria-describedby": describedBy, "aria-invalid": error ? true : undefined })}
      {hint && (
        <p id={hintId} className="muted small field-hint">
          {hint}
        </p>
      )}
      {error && (
        <p id={errId} className="field-error small">
          {error}
        </p>
      )}
    </div>
  );
}

export function FormError({ error }: { error: unknown }) {
  const t = useT();
  if (!error) return null;
  const e = ApiError.from(error);
  return (
    <p className="note note-error" role="alert">
      {e.isMethodNotFound ? t("common.notAvailableSentence") : e.message}
    </p>
  );
}

export function Actions({ onBack, busy, submitLabel }: { onBack: () => void; busy: boolean; submitLabel: string }) {
  const t = useT();
  return (
    <div className="dialog-actions">
      <button type="button" className="btn" onClick={onBack} disabled={busy}>
        {t("common.back")}
      </button>
      <button type="submit" className="btn btn-primary" disabled={busy}>
        {busy ? (
          <>
            <Spinner label={t("addSource.working")} /> {submitLabel}
          </>
        ) : (
          submitLabel
        )}
      </button>
    </div>
  );
}

export function NameField({ value, onChange, placeholder }: { value: string; onChange: (v: string) => void; placeholder: string }) {
  const t = useT();
  return (
    <Field label={t("addSource.name")} optional hint={t("addSource.nameHintDefault", { name: placeholder })}>
      {(a) => (
        <input
          {...a}
          type="text"
          dir="auto"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={t("addSource.namePlaceholder")}
          autoComplete="off"
        />
      )}
    </Field>
  );
}

export function Done({ text, onClose }: { text: string; onClose: () => void }) {
  const t = useT();
  const ref = useAutofocus<HTMLButtonElement>();
  return (
    <div>
      <p role="status">{text}</p>
      <div className="dialog-actions">
        <button ref={ref} type="button" className="btn btn-primary" onClick={onClose}>
          {t("common.done")}
        </button>
      </div>
    </div>
  );
}

/** A checkbox with its label (and an optional hint under it). */
export function Check({
  checked,
  onChange,
  label,
  hint,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  hint?: string;
  disabled?: boolean;
}) {
  return (
    <label className="check">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} disabled={disabled} />
      <span>
        {label}
        {hint && <span className="muted small block">{hint}</span>}
      </span>
    </label>
  );
}

/** Props of each kind's form in the wizard. */
export interface FormProps<K extends string = string> {
  kind: K;
  onBack: () => void;
  onClose: () => void;
  onAdded: (s: SourceInfo) => void;
}

/** Tags allowed in translated help text: `<b>…</b>` and `<code>…</code>`. */
export const HELP_TAGS = { b: (c: ReactNode) => <b>{c}</b>, code: (c: ReactNode) => <code>{c}</code> };
