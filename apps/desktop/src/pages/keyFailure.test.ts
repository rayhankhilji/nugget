import { describe, expect, it } from "vitest";
import { keyFailure } from "./Models";

describe("keyFailure", () => {
  it("says why a saved key isn't working in plain words", () => {
    expect(keyFailure("401 Unauthorized: invalid api key")).toBe("models.ai.reason.refused");
    expect(keyFailure("rate limited")).toBe("models.ai.reason.limited");
    expect(keyFailure("connect timed out")).toBe("models.ai.reason.unreachable");
    expect(keyFailure("model not found")).toBe("models.ai.reason.other");
    expect(keyFailure(null)).toBe("models.ai.reason.other");
  });
});
