// @vitest-environment jsdom
/** ErrorNote: what each kind of failure shows, announces, and keeps behind "Details". */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { t } from "../i18n";
import { cleanup, installDomShims, render } from "../test/render";
import { ErrorNote } from "./ui";

beforeEach(installDomShims);
afterEach(cleanup);

async function note(error: unknown) {
  const { container } = await render(<ErrorNote error={error} onRetry={() => {}} />);
  const alert = container.querySelector<HTMLElement>("[role=alert]")!;
  return {
    alert,
    headline: alert.querySelector(".note-text")?.textContent ?? alert.firstElementChild?.textContent ?? "",
    details: alert.querySelector("details p")?.textContent ?? null,
  };
}

describe("ErrorNote", () => {
  it("busy or locked: a friendly headline; the raw text and an unformatted code under Details", async () => {
    const n = await note({ kind: "rpc", message: "database is locked", code: -32000 });
    expect(n.alert.classList.contains("note-error")).toBe(true);
    expect(n.headline).toBe(t("common.error.busy"));
    expect(n.details).toBe("database is locked (code -32000)");
    expect(n.alert.textContent).not.toContain("-32,000");
  });

  it("timed out: a friendly headline", async () => {
    const n = await note({ kind: "rpc", message: "request timed out after 30s", code: -32000 });
    expect(n.headline).toBe(t("common.error.timeout"));
    expect(n.details).toContain("timed out");
  });

  it("other daemon errors keep the daemon's message visible, with the code under Details", async () => {
    const n = await note({ kind: "rpc", message: "a source named Notes already exists", code: -32602 });
    expect(n.headline).toBe("a source named Notes already exists");
    expect(n.details).toBe("(code -32602)");
  });

  it("version mismatch: a friendly headline", async () => {
    const n = await note({ kind: "protocol", message: "Unexpected response" });
    expect(n.headline).toBe(t("common.error.protocol"));
    expect(n.details).toBe("Unexpected response");
  });

  it("shell and script errors: a generic headline, the raw text under Details", async () => {
    const n = await note({ kind: "other", message: "Permission denied (os error 13)" });
    expect(n.headline).toBe(t("common.error.generic"));
    expect(n.details).toBe("Permission denied (os error 13)");
    const js = await note(new TypeError("x is undefined"));
    expect(js.headline).toBe(t("common.error.generic"));
  });

  it("an Error thrown by the app keeps its own (translated) sentence", async () => {
    const n = await note(new Error(t("plugins.approve.failed")));
    expect(n.headline).toBe(t("plugins.approve.failed"));
    expect(n.details).toBeNull();
  });

  it("offline: a quiet hint, not an error", async () => {
    const n = await note({ kind: "disconnected", message: "broken pipe" });
    expect(n.alert.classList.contains("note-error")).toBe(false);
    expect(n.alert.textContent).toContain(t("common.offlineHint"));
  });
});
