import { readFileSync } from "node:fs";
import { join } from "node:path";
import { JSDOM } from "jsdom";
import { ElementRegistry } from "../src/registry";
import type { Rect } from "../src/protocol";

export function fixture(name: string): string {
  return readFileSync(join(__dirname, "fixtures", name), "utf8");
}

/** A fresh document at `url` (jsdom has no layout, so we pass `measure` explicitly). */
export function dom(html: string, url = "https://shop.example/page"): Document {
  return new JSDOM(html, { url, pretendToBeVisual: true }).window.document;
}

/** Every element is 100x20 at the origin: jsdom does no layout. */
export const measure = (_el: Element): Rect => ({ x: 0, y: 0, width: 100, height: 20 });

export function registry(): ElementRegistry {
  return new ElementRegistry("t0");
}
