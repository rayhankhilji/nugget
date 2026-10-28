/**
 * A small ICU MessageFormat subset, enough for UI strings:
 *
 * - `{name}` inserts an argument (numbers are formatted for the locale)
 * - `{n, number}`, `{n, number, percent}`, `{n, number, integer}`
 * - `{d, date, short|medium|long}`, `{d, time, short|medium}`
 * - `{n, plural, =0 {none} one {# file} other {# files}}` (`#` is the formatted number)
 * - `{x, select, a {…} other {…}}`
 * - `<tag>…</tag>` rich-text spans, rendered by `formatParts` (plain `format` keeps the text)
 *
 * Unlike full ICU, apostrophes are literal (French and English text uses them freely).
 * Plural categories come from `Intl.PluralRules`, numbers and dates from `Intl`.
 */

export type ArgValue = string | number | Date | null | undefined;
export type Args = Readonly<Record<string, ArgValue>>;

type Node =
  | { t: "text"; v: string }
  | { t: "arg"; name: string; style?: string; fmt?: string }
  | { t: "hash" }
  | { t: "plural"; name: string; branches: Record<string, Node[]> }
  | { t: "select"; name: string; branches: Record<string, Node[]> }
  | { t: "tag"; name: string; children: Node[] };

export class MessageSyntaxError extends Error {}

class Parser {
  i = 0;
  constructor(readonly src: string) {}

  fail(msg: string): never {
    throw new MessageSyntaxError(`${msg} at ${this.i} in ${JSON.stringify(this.src)}`);
  }

  /** Parse until `}` (inside a branch), `</` (inside a tag) or the end. */
  nodes(inPlural: boolean): Node[] {
    const out: Node[] = [];
    let text = "";
    const flush = () => {
      if (text) out.push({ t: "text", v: text });
      text = "";
    };
    while (this.i < this.src.length) {
      const c = this.src[this.i]!;
      if (c === "}") break;
      if (c === "<" && this.src[this.i + 1] === "/") break;
      if (c === "{") {
        flush();
        out.push(this.placeholder());
        continue;
      }
      if (c === "<" && /[a-zA-Z]/.test(this.src[this.i + 1] ?? "")) {
        flush();
        out.push(this.tag(inPlural));
        continue;
      }
      if (c === "#" && inPlural) {
        flush();
        out.push({ t: "hash" });
        this.i++;
        continue;
      }
      text += c;
      this.i++;
    }
    flush();
    return out;
  }

  ws() {
    while (/\s/.test(this.src[this.i] ?? "")) this.i++;
  }

  ident(): string {
    this.ws();
    const m = /^[^\s,{}<>]+/.exec(this.src.slice(this.i));
    if (!m) this.fail("expected a name");
    this.i += m[0].length;
    this.ws();
    return m[0];
  }

  expect(c: string) {
    if (this.src[this.i] !== c) this.fail(`expected "${c}"`);
    this.i++;
  }

  placeholder(): Node {
    this.expect("{");
    const name = this.ident();
    if (this.src[this.i] === "}") {
      this.i++;
      return { t: "arg", name };
    }
    this.expect(",");
    const kind = this.ident();
    if (kind === "plural" || kind === "select") {
      this.expect(",");
      const branches: Record<string, Node[]> = {};
      this.ws();
      while (this.src[this.i] !== "}") {
        const key = this.ident();
        this.expect("{");
        branches[key] = this.nodes(kind === "plural");
        this.expect("}");
        this.ws();
        if (this.i >= this.src.length) this.fail("unterminated branch list");
      }
      this.i++;
      if (!branches.other) this.fail(`${kind} needs an "other" branch`);
      return { t: kind, name, branches };
    }
    if (kind !== "number" && kind !== "date" && kind !== "time") this.fail(`unknown format "${kind}"`);
    let fmt: string | undefined;
    if (this.src[this.i] === ",") {
      this.i++;
      fmt = this.ident();
    }
    this.expect("}");
    return { t: "arg", name, style: kind, fmt };
  }

  tag(inPlural: boolean): Node {
    this.expect("<");
    const m = /^[a-zA-Z][a-zA-Z0-9]*/.exec(this.src.slice(this.i))!;
    const name = m[0];
    this.i += name.length;
    this.expect(">");
    const children = this.nodes(inPlural);
    const close = `</${name}>`;
    if (!this.src.startsWith(close, this.i)) this.fail(`expected ${close}`);
    this.i += close.length;
    return { t: "tag", name, children };
  }

  parse(): Node[] {
    const n = this.nodes(false);
    if (this.i < this.src.length) this.fail(`unexpected "${this.src[this.i]}"`);
    return n;
  }
}

const cache = new Map<string, Node[]>();

export function parse(src: string): Node[] {
  let n = cache.get(src);
  if (!n) {
    n = new Parser(src).parse();
    cache.set(src, n);
  }
  return n;
}

// ---- Intl caches ----

const numFmts = new Map<string, Intl.NumberFormat>();
export function numberFormat(locale: string, opts: Intl.NumberFormatOptions = {}): Intl.NumberFormat {
  const key = `${locale}|${JSON.stringify(opts)}`;
  let f = numFmts.get(key);
  if (!f) {
    f = new Intl.NumberFormat(locale, opts);
    numFmts.set(key, f);
  }
  return f;
}

const dateFmts = new Map<string, Intl.DateTimeFormat>();
export function dateFormat(locale: string, opts: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const key = `${locale}|${JSON.stringify(opts)}`;
  let f = dateFmts.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(locale, opts);
    dateFmts.set(key, f);
  }
  return f;
}

const pluralRules = new Map<string, Intl.PluralRules>();
function pluralCategory(locale: string, n: number): string {
  let r = pluralRules.get(locale);
  if (!r) {
    r = new Intl.PluralRules(locale);
    pluralRules.set(locale, r);
  }
  return r.select(n);
}

type DateStyle = "short" | "medium" | "long" | "full";
const DATE_STYLES = new Set(["short", "medium", "long", "full"]);

function formatArg(locale: string, value: ArgValue, style?: string, fmt?: string): string {
  if (value === null || value === undefined) return "";
  if (style === "date" || style === "time" || value instanceof Date) {
    const d = value instanceof Date ? value : new Date(value);
    if (Number.isNaN(d.getTime())) return String(value);
    const s = (fmt && DATE_STYLES.has(fmt) ? fmt : "medium") as DateStyle;
    return dateFormat(locale, style === "time" ? { timeStyle: s } : { dateStyle: s }).format(d);
  }
  if (typeof value === "number") {
    if (fmt === "percent") return numberFormat(locale, { style: "percent", maximumFractionDigits: 0 }).format(value);
    if (fmt === "integer") return numberFormat(locale, { maximumFractionDigits: 0 }).format(value);
    return numberFormat(locale).format(value);
  }
  return value;
}

// ---- rendering ----

/** Render a parsed message. `wrap` turns a tag's rendered children into one part. */
function render<P>(
  nodes: Node[],
  locale: string,
  args: Args,
  wrap: (tag: string, children: (string | P)[]) => string | P,
  num?: number,
): (string | P)[] {
  const out: (string | P)[] = [];
  const push = (p: string | P) => {
    const last = out.length - 1;
    if (typeof p === "string" && typeof out[last] === "string") out[last] = (out[last] as string) + p;
    else out.push(p);
  };
  for (const n of nodes) {
    switch (n.t) {
      case "text":
        push(n.v);
        break;
      case "hash":
        push(num === undefined ? "#" : numberFormat(locale).format(num));
        break;
      case "arg":
        push(formatArg(locale, args[n.name], n.style, n.fmt));
        break;
      case "plural": {
        const v = Number(args[n.name] ?? 0);
        const branch = n.branches[`=${v}`] ?? n.branches[pluralCategory(locale, v)] ?? n.branches.other!;
        for (const p of render(branch, locale, args, wrap, v)) push(p);
        break;
      }
      case "select": {
        const v = String(args[n.name] ?? "");
        const branch = n.branches[v] ?? n.branches.other!;
        for (const p of render(branch, locale, args, wrap, num)) push(p);
        break;
      }
      case "tag":
        push(wrap(n.name, render(n.children, locale, args, wrap, num)));
        break;
    }
  }
  return out;
}

/** Format to a plain string; tags keep only their text. */
export function format(src: string, locale: string, args: Args = {}): string {
  return render<string>(parse(src), locale, args, (_tag, children) => children.join("")).join("");
}

/** Format to parts, letting `wrap` turn each `<tag>` into a rich node (e.g. a React element). */
export function formatParts<P>(
  src: string,
  locale: string,
  args: Args,
  wrap: (tag: string, children: (string | P)[], index: number) => P,
): (string | P)[] {
  let i = 0;
  return render<P>(parse(src), locale, args, (tag, children) => wrap(tag, children, i++));
}

/** Argument names, tag names and plural/select branch keys a message uses (for catalog checks). */
export function signature(src: string): { args: string[]; tags: string[] } {
  const args = new Set<string>();
  const tags = new Set<string>();
  const walk = (nodes: Node[]) => {
    for (const n of nodes) {
      if (n.t === "arg" || n.t === "plural" || n.t === "select") args.add(n.name);
      if (n.t === "plural" || n.t === "select") Object.values(n.branches).forEach(walk);
      if (n.t === "tag") {
        tags.add(n.name);
        walk(n.children);
      }
    }
  };
  walk(parse(src));
  return { args: [...args].sort(), tags: [...tags].sort() };
}
