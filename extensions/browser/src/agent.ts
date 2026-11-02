// The in-page half of Heed: owns the element registry for one document, hands out snapshots,
// and performs validated actions followed by a verification snapshot and diff.

import { performAction, validateAction } from "./actions";
import { formDigest, scopeDigest } from "./digest";
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
   * Validate against the latest snapshot, act, then snapshot again and diff. With `expectForm`
   * (a native form submit) or `expectScope` (a send whose content is a dialog or region: a
   * compose window's contenteditable body), the confirmed content is digested again
   * synchronously at the last moment, right before `click()` or `requestSubmit()` and after the
   * target was focused (focus handlers run synchronously too), with no `await` in between, and
   * the action is refused if it differs from what the user confirmed or cannot be read.
   *
   * Residual gap: the page's own `click` and `submit` handlers run after this check, as part of
   * dispatching, and can still rewrite the content before it is sent.
   */
  async act(
    snapshotId: string,
    action: BrowserAction,
    expectForm?: FormExpectation,
    expectScope?: FormExpectation,
  ): Promise<ActResult> {
    if (this.halted) throw new ProtocolError("halted", "Heed was stopped on this page");
    validateAction(action, snapshotId, this.latest);
    const before = this.latest!;
    const check = (): void => {
      const confirm = (expect: FormExpectation | undefined, digest: (el: Element) => string, what: string) => {
        if (!expect) return;
        const el = this.registry.get(expect.form);
        let now: string | undefined;
        try {
          now = el ? digest(el) : undefined;
        } catch {
          now = undefined;
        }
        if (now === undefined) {
          throw new ProtocolError("form_changed", `the confirmed ${what} can no longer be read; nothing was sent`);
        }
        if (now !== expect.digest) {
          throw new ProtocolError("form_changed", `the ${what} changed after it was confirmed; nothing was sent`);
        }
      };
      const formClass = this.doc.defaultView?.HTMLFormElement;
      confirm(
        expectForm,
        (el) => {
          if (!formClass || !(el instanceof formClass)) throw new Error("not a form");
          return formDigest(el, this.registry.salt);
        },
        "form",
      );
      confirm(expectScope, (el) => scopeDigest(el, this.registry.salt), "message");
    };
    check();
    await performAction(this.registry, this.doc, action, check);
    const settle = this.opts.settleMs ?? 150;
    if (settle > 0) await new Promise((r) => setTimeout(r, settle));
    const after = this.snapshot();
    return { before_snapshot_id: before.snapshot_id, snapshot: after, diff: diffSnapshots(before, after) };
  }
}
