// The in-page half of Nugget: owns the element registry for one document, hands out snapshots,
// and performs validated actions followed by a verification snapshot and diff.

import { performAction, validateAction } from "./actions";
import { formDigest } from "./digest";
import { diffSnapshots } from "./diff";
import { ElementRegistry } from "./registry";
import { buildSnapshot, type SnapshotContext } from "./snapshot";
import {
  ProtocolError,
  type ActResult,
  type BrowserAction,
  type FormExpectation,
  type PageSnapshot,
  type SnapshotOptions,
} from "./protocol";

export interface PageAgentOptions {
  measure?: SnapshotContext["measure"];
  now?: () => number;
  /** How long to let the page settle after an action before the verification snapshot. */
  settleMs?: number;
  token?: string;
  /** Form-digest salt (tests pass a fixed one; otherwise random per document). */
  salt?: string;
}

export class PageAgent {
  readonly registry: ElementRegistry;
  private latest: PageSnapshot | undefined;
  private halted = false;

  constructor(
    private readonly doc: Document,
    private readonly opts: PageAgentOptions = {},
  ) {
    this.registry = new ElementRegistry(opts.token, opts.salt);
  }

  get latestSnapshot(): PageSnapshot | undefined {
    return this.latest;
  }

  halt(): void {
    this.halted = true;
  }

  resume(): void {
    this.halted = false;
  }

  snapshot(options: SnapshotOptions = {}): PageSnapshot {
    this.latest = buildSnapshot(this.doc, { registry: this.registry, measure: this.opts.measure, now: this.opts.now }, options);
    return this.latest;
  }

  /**
   * Validate against the latest snapshot, act, then snapshot again and diff. With `expect`, the
   * form is digested again synchronously right before the action is dispatched (no `await` in
   * between, so no page script runs in the gap) and the action is refused if the form differs
   * from what the user confirmed.
   *
   * Residual gap: the page's own `submit` (or `click`) handlers run after this check, as part
   * of dispatching, and can still rewrite the form before the browser serialises it.
   */
  async act(snapshotId: string, action: BrowserAction, expect?: FormExpectation): Promise<ActResult> {
    if (this.halted) throw new ProtocolError("halted", "Nugget was stopped on this page");
    validateAction(action, snapshotId, this.latest);
    const before = this.latest!;
    if (expect) {
      const form = this.registry.get(expect.form);
      if (!(form instanceof (this.doc.defaultView?.HTMLFormElement ?? HTMLFormElement))) {
        throw new ProtocolError("form_changed", "the confirmed form is no longer in the page; nothing was sent");
      }
      if (formDigest(form, this.registry.salt) !== expect.digest) {
        throw new ProtocolError("form_changed", "the form changed after it was confirmed; nothing was sent");
      }
    }
    await performAction(this.registry, this.doc, action);
    const settle = this.opts.settleMs ?? 150;
    if (settle > 0) await new Promise((r) => setTimeout(r, settle));
    const after = this.snapshot();
    return { before_snapshot_id: before.snapshot_id, snapshot: after, diff: diffSnapshots(before, after) };
  }
}
