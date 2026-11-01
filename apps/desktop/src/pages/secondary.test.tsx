// @vitest-environment jsdom
/**
 * Secondary-page polish: Status reports one daemon failure once, Safety names built-in rules and
 * folds long pattern lists, Models groups by role and badges one "best" pick, and Apps tells
 * same-named connections apart.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { RESPONSES } from "../test/fixtures";
import { cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { StatusPage } from "./Status";

beforeEach(installDomShims);
afterEach(cleanup);

const busy = () => {
  throw { kind: "rpc", message: "database is locked", code: -32000 };
};

describe("Status: one daemon failure, one error", () => {
  it("shows a single page-level error whose Retry reloads every section", async () => {
    let failing = true;
    const answer = (cmd: string) => () => {
      if (failing) busy();
      return RESPONSES[cmd as keyof typeof RESPONSES];
    };
    const calls = mockDaemon({
      status: answer("status"),
      ingest_status: answer("ingest_status"),
      runtime_status: answer("runtime_status"),
      knowledge_status: answer("knowledge_status"),
      updates_status: null,
    });
    const { container } = await render(<StatusPage />);
    await flush();
    const errors = container.querySelectorAll(".note-error");
    expect(errors).toHaveLength(1);
    // The sections with nothing to show are left out while the page reports the failure.
    expect(container.querySelector("#indexing-title")).toBeNull();
    const retry = Array.from(errors[0]!.querySelectorAll("button")).find((b) => b.textContent === "Retry");
    expect(retry).toBeDefined();
    const count = (cmd: string) => calls.filter((c) => c === cmd).length;
    const before = ["status", "ingest_status", "runtime_status", "knowledge_status"].map(count);
    failing = false;
    await click(retry);
    await flush();
    expect(["status", "ingest_status", "runtime_status", "knowledge_status"].map(count)).toEqual(before.map((n) => n + 1));
    expect(container.querySelectorAll(".note-error")).toHaveLength(0);
    expect(container.querySelector("#indexing-title")).not.toBeNull();
  });
});
