/** Step 1 of the Add source wizard: every kind, grouped by where the data lives. */
import type { FormEvent } from "react";
import type { ConnectorKind, SourceKind } from "../api";
import { sourceKindLabel } from "../format";
import { useT, type MessageKey } from "../i18n";
import { useAutofocus } from "./form";
import { isNewKind, NEW_KIND_BLURB } from "./forms";
import { KIND_GROUPS } from "./kinds";
import { KindIcon } from "./KindIcon";

export function KindPicker({
  value,
  onChange,
  onNext,
  onCancel,
  blurb,
}: {
  value: ConnectorKind;
  onChange: (k: ConnectorKind) => void;
  onNext: () => void;
  onCancel: () => void;
  /** One-line descriptions of the M1 kinds (the wizard's own table). */
  blurb: Record<SourceKind, MessageKey>;
}) {
  const t = useT();
  const first = useAutofocus<HTMLInputElement>();
  const submit = (e: FormEvent) => {
    e.preventDefault();
    onNext();
  };
  return (
    <form onSubmit={submit}>
      <fieldset className="kind-picker">
        <legend className="visually-hidden">{t("addSource.kindLegend")}</legend>
        {KIND_GROUPS.map((g) => [
          <p key={g.id} className="kind-group">
            {t(g.label)}
          </p>,
          ...g.kinds.map((k) => (
            <label key={k} className="kind-option">
              <input
                ref={k === value ? first : undefined}
                data-autofocus={k === value ? "" : undefined}
                type="radio"
                name="source-kind"
                value={k}
                checked={k === value}
                onChange={() => onChange(k)}
              />
              <KindIcon kind={k} size={22} />
              <span>
                <span className="kind-label">{sourceKindLabel(k)}</span>
                <span className="muted small">{t(isNewKind(k) ? NEW_KIND_BLURB[k] : blurb[k])}</span>
              </span>
            </label>
          )),
        ])}
      </fieldset>
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onCancel}>
          {t("common.cancel")}
        </button>
        <button type="submit" className="btn btn-primary">
          {t("common.next")}
        </button>
      </div>
    </form>
  );
}
