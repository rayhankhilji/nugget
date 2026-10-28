// Post-action verification: what changed between two snapshots of the same tab.

import type { ElementChange, ElementInfo, PageSnapshot, SnapshotDiff } from "./protocol";

const COMPARED: (keyof ElementInfo)[] = [
  "role", "name", "value", "checked", "disabled", "editable", "focused", "visible", "href",
];

export function diffSnapshots(before: PageSnapshot, after: PageSnapshot): SnapshotDiff {
  const diff: SnapshotDiff = { added: [], removed: [], changed: [], text_changed: before.text !== after.text };
  if (before.url !== after.url) diff.url_changed = { from: before.url, to: after.url };
  if (before.title !== after.title) diff.title_changed = { from: before.title, to: after.title };

  const old = new Map(before.elements.map((e) => [e.id, e]));
  const seen = new Set<string>();
  for (const el of after.elements) {
    seen.add(el.id);
    const prev = old.get(el.id);
    if (!prev) {
      diff.added.push(el.id);
      continue;
    }
    const fields = COMPARED.filter((k) => !same(prev[k], el[k]));
    if (prev.options && el.options && JSON.stringify(prev.options) !== JSON.stringify(el.options)) {
      fields.push("options");
    }
    if (fields.length > 0) diff.changed.push({ id: el.id, fields } satisfies ElementChange);
  }
  for (const el of before.elements) if (!seen.has(el.id)) diff.removed.push(el.id);
  return diff;
}

function same(a: unknown, b: unknown): boolean {
  return a === b;
}

export function isEmptyDiff(d: SnapshotDiff): boolean {
  return (
    !d.url_changed && !d.title_changed && !d.text_changed &&
    d.added.length === 0 && d.removed.length === 0 && d.changed.length === 0
  );
}
