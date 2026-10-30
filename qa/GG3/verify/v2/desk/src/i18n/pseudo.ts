/**
 * Pseudo-localisation (`en-XA`): English with accented letters, padded about 35% longer and
 * bracketed, so untranslated strings, truncation and layouts that can't grow stand out without
 * waiting for a real translation. Only literal text changes: `{arg}` names, plural/select
 * keywords and branch keys, `#` and `<tag>` names stay as they are.
 */
import { parse } from "./icu";

type Node = ReturnType<typeof parse>[number];

const ACCENTS: Record<string, string> = {
  a: "á", b: "ƀ", c: "ç", d: "ď", e: "é", g: "ĝ", h: "ĥ", i: "í", j: "ĵ", k: "ķ", l: "ļ", n: "ñ",
  m: "ḿ", o: "ó", r: "ŕ", s: "š", t: "ţ", u: "ú", w: "ŵ", y: "ý", z: "ž",
  A: "Á", C: "Ç", D: "Ď", E: "É", G: "Ĝ", H: "Ĥ", I: "Í", J: "Ĵ", K: "Ķ", L: "Ļ", N: "Ñ",
  O: "Ó", R: "Ŕ", S: "Š", T: "Ţ", U: "Ú", W: "Ŵ", Y: "Ý", Z: "Ž",
};

/** Filler for the padding: accented, with spaces so long strings can still wrap. */
const FILLER = "ļóŕéḿ íƥšúḿ ďóļóŕ šíţ áḿéţ çóñšéçţéţúŕ áďíƥíšçíñĝ éļíţ ";

export function accent(text: string): string {
  let out = "";
  for (const ch of text) out += ACCENTS[ch] ?? ch;
  return out;
}

/** Length of the literal text a message shows (the longest plural/select branch). */
function textLength(nodes: Node[]): number {
  let n = 0;
  for (const node of nodes) {
    if (node.t === "text") n += [...node.v].length;
    else if (node.t === "tag") n += textLength(node.children);
    else if (node.t === "plural" || node.t === "select") n += Math.max(...Object.values(node.branches).map(textLength));
  }
  return n;
}

function serialize(nodes: Node[]): string {
  let out = "";
  for (const node of nodes) {
    switch (node.t) {
      case "text":
        out += accent(node.v);
        break;
      case "hash":
        out += "#";
        break;
      case "arg":
        out += `{${node.name}${node.style ? `, ${node.style}` : ""}${node.fmt ? `, ${node.fmt}` : ""}}`;
        break;
      case "plural":
      case "select":
        out += `{${node.name}, ${node.t}, ${Object.entries(node.branches)
          .map(([k, b]) => `${k} {${serialize(b)}}`)
          .join(" ")}}`;
        break;
      case "tag":
        out += `<${node.name}>${serialize(node.children)}</${node.name}>`;
        break;
    }
  }
  return out;
}

/** One message in pseudo form. Messages with no literal text (just `{arg}`) are left alone. */
export function pseudoMessage(src: string): string {
  const nodes = parse(src);
  const len = textLength(nodes);
  if (len === 0) return src;
  // "[" + text + " " + padding + "]": at least 4 extra characters, else about 40% more.
  const extra = Math.max(4, Math.ceil(len * 0.4));
  const padLen = Math.max(1, extra - 3);
  let pad = "";
  while (pad.length < padLen) pad += FILLER;
  pad = pad.slice(0, padLen).trimEnd() || "ļ";
  return `[${serialize(nodes)} ${pad}]`;
}

/** Turn an English catalog into its pseudo-locale catalog. */
export function pseudoCatalog<T extends Record<string, string>>(catalog: T): T {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(catalog)) out[k] = pseudoMessage(v);
  return out as T;
}
