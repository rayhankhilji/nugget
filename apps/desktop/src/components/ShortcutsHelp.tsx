import { Fragment } from "react";
import { useT } from "../i18n";
import { comboLabel, type Combo } from "../hooks.shortcuts";
import { Dialog } from "./Dialog";

export interface ShortcutRow {
  /** One or more key combinations that do the same thing ("?" or Ctrl+/). */
  combos: readonly Combo[];
  /** Already translated description. */
  label: string;
}

/**
 * The list of keyboard shortcuts, opened with `?` or ⌘/. Information only: it closes with the
 * title bar's X or Escape, so there's no second Close button.
 */
export function ShortcutsHelp({ rows, onClose }: { rows: readonly ShortcutRow[]; onClose: () => void }) {
  const t = useT();
  return (
    <Dialog title={t("shortcuts.title")} description={t("shortcuts.description")} onClose={onClose}>
      <dl className="shortcut-list">
        {rows.map((r) => (
          <div key={`${r.combos.map((c) => `${c.mod ? "mod-" : ""}${c.key}`).join("|")}-${r.label}`} className="shortcut-row">
            <dt>
              {r.combos.map((c, i) => (
                <Fragment key={`${c.mod ? "mod-" : ""}${c.key}`}>
                  {i > 0 && <span className="shortcut-or">{t("shortcuts.or")}</span>}
                  <kbd dir="ltr">{comboLabel(c)}</kbd>
                </Fragment>
              ))}
            </dt>
            <dd>{r.label}</dd>
          </div>
        ))}
      </dl>
    </Dialog>
  );
}
