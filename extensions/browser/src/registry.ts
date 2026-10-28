// Stable element ids for one document. An element keeps its id for as long as it lives; a new
// document (navigation) gets a fresh registry, so ids from an old page can never match.

export class ElementRegistry {
  private readonly ids = new WeakMap<Element, string>();
  private readonly byId = new Map<string, WeakRef<Element>>();
  private next = 1;
  private seq = 0;
  /** Random per-document token; snapshot ids from another document never validate. */
  readonly token: string;

  constructor(token?: string) {
    this.token = token ?? randomToken();
  }

  idFor(el: Element): string {
    let id = this.ids.get(el);
    if (id === undefined) {
      id = `n${this.next++}`;
      this.ids.set(el, id);
      this.byId.set(id, new WeakRef(el));
    }
    return id;
  }

  /** The live element for `id`, if it is still attached to a document. */
  get(id: string): Element | undefined {
    const el = this.byId.get(id)?.deref();
    if (el === undefined) {
      this.byId.delete(id);
      return undefined;
    }
    return el.isConnected ? el : undefined;
  }

  nextSnapshotId(): string {
    this.seq += 1;
    return `${this.token}.${this.seq}`;
  }
}

function randomToken(): string {
  const bytes = new Uint8Array(6);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}
