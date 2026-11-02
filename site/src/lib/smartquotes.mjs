// Typographic quotes for site copy: ’ for apostrophes, ‘ ’ and “ ” for quotes. Used on HTML text at
// build time (integrations/smartquotes.mjs) and on the social-card lines.

/** Smart-quote plain text. */
export function smart(text) {
  return text
    .replace(/(\w)'(\w)/g, "$1’$2") // don't, it's, Heed's
    .replace(/(\w)'/g, "$1’") // trailing: users'
    .replace(/'(\w)/g, "‘$1") // opening single quote
    .replace(/(^|[\s(\[{—–-])"(?=\S)/g, "$1“")
    .replace(/"/g, "”");
}

/** Smart-quote the text nodes of an HTML document, leaving tags, attributes and code alone. */
export function smartHtml(html) {
  // Set aside raw blocks first: script and style bodies can contain "<", which would confuse a
  // simple tag split, and code must keep its straight quotes.
  const raw = [];
  const held = html.replace(/<(script|style|pre|code|textarea|kbd)\b[\s\S]*?<\/\1>/gi, (m) => {
    raw.push(m);
    return `\u0000${raw.length - 1}\u0000`;
  });
  const done = held
    .split(/(<[^>]*>)/)
    .map((part) => {
      if (part.startsWith("<") || !/['"]|&#39;|&quot;|&#x27;/.test(part)) return part;
      return smart(part.replace(/&#39;|&#x27;/g, "'").replace(/&quot;/g, '"'));
    })
    .join("");
  return done.replace(/\u0000(\d+)\u0000/g, (_, n) => raw[Number(n)]);
}
