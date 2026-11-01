/**
 * The base direction of generated text (answers, drafts) by its majority script.
 *
 * `dir="auto"` decides by the first strong character, so an Arabic answer that starts with a
 * product name (`iPhone 15 سيصل يوم الخميس…`) is laid out left-to-right. Counting letters instead
 * gives the direction a reader expects; text with no letters, or as many of each, stays `auto`.
 */
export type TextDir = "rtl" | "ltr" | "auto";

const LETTER = /\p{L}/u;
const RTL_LETTER =
  /[\p{Script=Arabic}\p{Script=Hebrew}\p{Script=Syriac}\p{Script=Thaana}\p{Script=Nko}\p{Script=Samaritan}\p{Script=Mandaic}\p{Script=Adlam}\p{Script=Hanifi_Rohingya}]/u;
/** Link targets and bare URLs: Latin letters the reader does not read as prose. */
const URLS = /\]\([^)\s]*\)|\bhttps?:\/\/\S+|\bmailto:\S+/g;

export function textDir(text: string | null | undefined): TextDir {
  if (!text) return "auto";
  let rtl = 0;
  let ltr = 0;
  for (const ch of text.replace(URLS, " ")) {
    if (!LETTER.test(ch)) continue;
    if (RTL_LETTER.test(ch)) rtl++;
    else ltr++;
  }
  return rtl > ltr ? "rtl" : ltr > rtl ? "ltr" : "auto";
}
