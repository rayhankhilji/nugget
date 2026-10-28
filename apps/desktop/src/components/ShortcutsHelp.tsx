import { useT } from "../i18n";
import { comboLabel, type Combo } from "../hooks.shortcuts";
import { Dialog } from "./Dialog";

export interface ShortcutRow {
  combo: Combo;
  /** Already translated description. */
  label: string;
}

/** The list of keyboard shortcuts, opened with `?` or ⌘/. */
export function ShortcutsHelp({ rows, onClose }: { rows: readonly ShortcutRow[]; onClose: () => void }) {
  const t = useT();
  return (
    <Dialog title={t("shortcuts.title")} description={t("shortcuts.description")} onClose={onClose}>
      <dl className="shortcut-list">
        {rows.map((r) => (
          <div key={`${r.combo.mod ? "mod-" : ""}${r.combo.key}-${r.label}`} className="shortcut-row">
            <dt>
              <kbd>{comboLabel(r.combo)}</kbd>
            </dt>
            <dd>{r.label}</dd>
          </div>
        ))}
      </dl>
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onClose} data-autofocus>
          {t("common.close")}
        </button>
      </div>
    </Dialog>
  );
}
