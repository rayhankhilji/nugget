// @vitest-environment jsdom
/** The shortcuts list: one row per action, and keyboard-scrollable without a footer button. */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { t } from "../i18n";
import { cleanup, installDomShims, render } from "../test/render";
import { tabStops } from "./Dialog";
import { ShortcutsHelp } from "./ShortcutsHelp";

beforeEach(installDomShims);
afterEach(cleanup);

describe("ShortcutsHelp", () => {
  it("shows several keys on one row, and its scrolling body is a named Tab stop", async () => {
    await render(
      <ShortcutsHelp
        rows={[
          { combos: [{ key: "?" }, { mod: true, key: "/" }], label: "Show this list" },
          { combos: [{ key: "Escape" }], label: "Close" },
        ]}
        onClose={() => {}}
      />,
    );
    const dialog = document.querySelector<HTMLElement>("dialog[open]")!;
    const rows = dialog.querySelectorAll(".shortcut-row");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.querySelectorAll("kbd")).toHaveLength(2);
    expect(rows[0]!.textContent).toContain(t("shortcuts.or"));

    const body = dialog.querySelector<HTMLElement>(".dialog-body")!;
    expect(body.tabIndex).toBe(0);
    expect(body.getAttribute("role")).toBe("region");
    expect(document.getElementById(body.getAttribute("aria-labelledby")!)?.textContent).toBe(t("shortcuts.title"));
    expect(tabStops(dialog).some((el) => body.contains(el) || el === body)).toBe(true);
  });
});
