import { describe, expect, it } from "vitest";
import type { DoubleTapSettings } from "./api";
import {
  chordError,
  normalizeSettings,
  sameSettings,
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
});
