import { describe, expect, it } from "vitest";
import type { DoubleTapSettings } from "./api";
import {
  addApp,
  chordError,
  clampWindow,
  keyOrText,
  normalizeSettings,
  permissionTone,
  providerRows,
  removeApp,
  rowsToMap,
  sameSettings,
  switchKind,
  triggerError,
} from "./doubletap";

const base: DoubleTapSettings = {
  enabled: true,
  trigger: { kind: "double_tap", modifier: "right_control", window_ms: 350 },
  app_providers: { "mail.google.com": "anthropic", slack: "local-chat" },
  excluded_apps: ["1Password", "Bitwarden"],
  allow_terminals: false,
};

describe("doubletap helpers", () => {
  it("clamps the tap window", () => {
    expect(clampWindow(10)).toBe(150);
    expect(clampWindow(5000)).toBe(1000);
    expect(clampWindow(352.4)).toBe(352);
    expect(clampWindow(Number.NaN)).toBe(350);
  });

  it("translates catalog keys and passes raw text through", () => {
    expect(keyOrText("doubletap.refused.secure")).toMatch(/password/i);
    expect(keyOrText("socket closed {weird}")).toBe("socket closed {weird}");
  });

  it("round-trips provider rows, dropping blanks", () => {
    const rows = providerRows(base.app_providers);
    expect(rows.map((r) => r.app)).toEqual(["mail.google.com", "slack"]);
    expect(rowsToMap([...rows, { app: "  ", provider: "x" }, { app: " zed ", provider: " p " }, { app: "slack", provider: "" }])).toEqual({
      "mail.google.com": "anthropic",
      slack: "local-chat",
      zed: "p",
    });
  });

  it("adds apps without duplicates and removes them", () => {
    expect(addApp(["1Password"], " 1password ")).toEqual(["1Password"]);
    expect(addApp(["1Password"], " KeePassXC ")).toEqual(["1Password", "KeePassXC"]);
    expect(addApp([], "   ")).toEqual([]);
    expect(removeApp(["a", "b"], "a")).toEqual(["b"]);
  });

  it("validates chords", () => {
    expect(chordError("")).toBe("doubletap.trigger.chordEmpty");
    expect(chordError("Space")).toBe("doubletap.trigger.chordInvalid");
    expect(chordError("Ctrl++")).toBe("doubletap.trigger.chordInvalid");
    expect(chordError("Ctrl+Alt+Space")).toBeNull();
    expect(triggerError(base.trigger)).toBeNull();
    expect(triggerError({ kind: "chord", chord: "" })).toBe("doubletap.trigger.chordEmpty");
  });

  it("normalises and compares settings", () => {
    const messy: DoubleTapSettings = {
      ...base,
      trigger: { kind: "double_tap", modifier: "right_control", window_ms: 349.6 },
      app_providers: { slack: "local-chat", "mail.google.com": "anthropic", "": "x" },
      excluded_apps: ["1Password", " Bitwarden ", "bitwarden"],
    };
    expect(normalizeSettings(messy).excluded_apps).toEqual(["1Password", "Bitwarden"]);
    expect(sameSettings(messy, base)).toBe(true);
    expect(sameSettings({ ...base, enabled: false }, base)).toBe(false);
    expect(sameSettings({ ...base, allow_terminals: true }, base)).toBe(false);
    expect(sameSettings({ ...base, trigger: { kind: "chord", chord: "Ctrl+Space" } }, base)).toBe(false);
    expect(normalizeSettings({ ...base, trigger: { kind: "chord", chord: " Ctrl+Space " } }).trigger).toEqual({
      kind: "chord",
      chord: "Ctrl+Space",
    });
  });

  it("switches trigger kind from memory", () => {
    const memory = { modifier: "left_shift" as const, window_ms: 400, chord: "Alt+Space" };
    expect(switchKind(base.trigger, "chord", memory)).toEqual({ kind: "chord", chord: "Alt+Space" });
    expect(switchKind({ kind: "chord", chord: "x" }, "double_tap", memory)).toEqual({ kind: "double_tap", modifier: "left_shift", window_ms: 400 });
    expect(switchKind(base.trigger, "double_tap", memory)).toBe(base.trigger);
  });

  it("maps permission states to tones", () => {
    expect(permissionTone("granted")).toBe("good");
    expect(permissionTone("denied")).toBe("bad");
    expect(permissionTone("unknown")).toBe("warn");
    expect(permissionTone("not_required")).toBe("neutral");
  });
});
