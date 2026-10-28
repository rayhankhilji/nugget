/**
 * Finds user-visible text that bypasses the catalog, for `i18n.test.ts` (and `node`-free use in
 * Vitest only: it reads files through the TypeScript compiler API).
 *
 * Flags, in `.tsx` files:
 * - JSX text containing a letter (`<p>Hello</p>`)
 * - string literals in user-visible attributes (`aria-label="Close"`, `title`, `placeholder`, …)
 * - string literals that read like prose anywhere else (`setError("Install failed.")`): two or
 *   more words starting with a capital, or one capitalised word ending in `.`, `…`, `!` or `?`.
 *
 * Text made only of symbols, digits and punctuation (`·`, `–`, `→`) is fine. A line containing
 * `i18n-ignore` (or the line after a comment containing it) is skipped: use it for brand names,
 * code samples and the like, with a reason.
 */
import ts from "typescript";

export interface Finding {
  file: string;
  line: number;
  text: string;
  kind: "jsx-text" | "attribute" | "literal";
}

export const VISIBLE_ATTRIBUTES = new Set([
  "aria-label",
  "aria-description",
  "aria-roledescription",
  "aria-valuetext",
  "aria-placeholder",
  "title",
  "placeholder",
  "alt",
  "label",
  "description",
  "submitLabel",
  "what",
]);

const LETTER = /\p{L}/u;
/** Looks like prose meant for a person rather than an identifier, class list, URL or key. */
export function looksLikeProse(s: string): boolean {
  const text = s.trim();
  if (!LETTER.test(text)) return false;
  if (/^[a-z0-9_.:\/-]+$/i.test(text) && !/\s/.test(text) && !/[.…!?]$/.test(text)) return false;
  if (/^(https?|file|obsidian):/.test(text)) return false;
  // Capitalised word followed by more words: "Install failed", "Add a source".
  if (/^\p{Lu}[\p{Ll}'’]+[,:]?(\s+\S+)+/u.test(text)) return true;
  // A single capitalised word that ends a sentence: "Failed.", "Starting…".
  if (/^\p{Lu}\p{Ll}+[.…!?]$/u.test(text)) return true;
  return false;
}

function ignored(sf: ts.SourceFile, node: ts.Node): boolean {
  const { line } = sf.getLineAndCharacterOfPosition(node.getStart(sf));
  const lines = sf.text.split("\n");
  return Boolean(lines[line]?.includes("i18n-ignore") || lines[line - 1]?.includes("i18n-ignore"));
}

export function lintSource(file: string, source: string): Finding[] {
  const sf = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const out: Finding[] = [];
  const add = (node: ts.Node, text: string, kind: Finding["kind"]) => {
    if (ignored(sf, node)) return;
    const { line } = sf.getLineAndCharacterOfPosition(node.getStart(sf));
    out.push({ file, line: line + 1, text: text.trim().replace(/\s+/g, " ").slice(0, 80), kind });
  };

  const visit = (node: ts.Node) => {
    if (ts.isJsxText(node)) {
      if (LETTER.test(node.text)) add(node, node.text, "jsx-text");
    } else if (ts.isJsxAttribute(node)) {
      const name = node.name.getText(sf);
      const init = node.initializer;
      if (VISIBLE_ATTRIBUTES.has(name) && init && ts.isStringLiteral(init) && LETTER.test(init.text)) {
        add(node, `${name}="${init.text}"`, "attribute");
        return;
      }
    } else if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) {
      if (!isTypeOrModulePosition(node) && looksLikeProse(node.text)) add(node, node.text, "literal");
    } else if (ts.isTemplateExpression(node)) {
      const pieces = [node.head.text, ...node.templateSpans.map((s) => s.literal.text)];
      if (looksLikeProse(pieces.join("x"))) add(node, node.getText(sf), "literal");
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return out;
}

/** Import paths, type literals and object keys are not UI text. */
function isTypeOrModulePosition(node: ts.Node): boolean {
  const p = node.parent;
  if (!p) return false;
  if (ts.isImportDeclaration(p) || ts.isExportDeclaration(p) || ts.isExternalModuleReference(p)) return true;
  if (ts.isLiteralTypeNode(p)) return true;
  if (ts.isPropertyAssignment(p) && p.name === node) return true;
  if (ts.isElementAccessExpression(p)) return true;
  return false;
}
