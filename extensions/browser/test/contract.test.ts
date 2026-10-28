// Cross-language contract: the Rust crate deserializes these exact snapshots and act results in
// its tests (crates/nugget-browser/tests/fixtures). Regenerate with UPDATE_FIXTURES=1 after a
// deliberate protocol change.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { PageAgent } from "../src/agent";
import { dom, fixture, measure } from "./helpers";

const dir = join(__dirname, "../../../crates/nugget-browser/tests/fixtures");

function check(name: string, value: unknown) {
  const path = join(dir, name);
  const json = JSON.stringify(value, null, 2) + "\n";
  if (process.env.UPDATE_FIXTURES || !existsSync(path)) {
    mkdirSync(dir, { recursive: true });
    writeFileSync(path, json);
  }
  expect(readFileSync(path, "utf8")).toBe(json);
}

describe("protocol fixtures shared with Rust", () => {
  it("form and checkout snapshots, and a type act result", async () => {
    const opts = { measure, settleMs: 0, token: "fx", now: () => 1700000000000 };
    const form = new PageAgent(dom(fixture("form.html")), opts);
    const snap = form.snapshot();
    check("form-snapshot.json", snap);
    const email = snap.elements.find((e) => e.name === "Email address")!.id;
    check("form-act-type.json", await form.act(snap.snapshot_id, { kind: "type", element: email, text: "grace@example.com" }));
    check("checkout-snapshot.json", new PageAgent(dom(fixture("checkout.html")), opts).snapshot());
  });
});
