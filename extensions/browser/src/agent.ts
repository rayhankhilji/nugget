// The in-page half of Nugget: owns the element registry for one document, hands out snapshots,
// and performs validated actions followed by a verification snapshot and diff.

import { performAction, validateAction } from "./actions";
import { diffSnapshots } from "./diff";
import { ElementRegistry } from "./registry";
import { buildSnapshot, type SnapshotContext } from "./snapshot";
import { ProtocolError, type ActResult, type BrowserAction, type PageSnapshot, type SnapshotOptions } from "./protocol";

export interface PageAgentOptions {
  measure?: SnapshotContext["measure"];
  now?: () => number;
  /** How long to let the page settle after an action before the verification snapshot. */
  settleMs?: number;
  token?: string;
}

export class PageAgent {
  readonly registry: ElementRegistry;
  private latest: PageSnapshot | undefined;
  private halted = false;

  constructor(
    private readonly doc: Document,
    private readonly opts: PageAgentOptions = {},
  ) {
    this.registry = new ElementRegistry(opts.token);
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

  /** Validate against the latest snapshot, act, then snapshot again and diff. */
  async act(snapshotId: string, action: BrowserAction): Promise<ActResult> {
    if (this.halted) throw new ProtocolError("halted", "Nugget was stopped on this page");
    validateAction(action, snapshotId, this.latest);
    const before = this.latest!;
    await performAction(this.registry, this.doc, action);
    const settle = this.opts.settleMs ?? 150;
    if (settle > 0) await new Promise((r) => setTimeout(r, settle));
    const after = this.snapshot();
    return { before_snapshot_id: before.snapshot_id, snapshot: after, diff: diffSnapshots(before, after) };
  }
}
