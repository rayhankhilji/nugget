export type Os = "macos" | "windows" | "linux" | "other";

/** The OS the app runs on, from the webview's user agent (enough for showing the right help). */
export function detectOs(ua: string = typeof navigator === "undefined" ? "" : navigator.userAgent): Os {
  if (/Mac OS X|Macintosh/i.test(ua)) return "macos";
  if (/Windows/i.test(ua)) return "windows";
  if (/Linux|X11|CrOS/i.test(ua)) return "linux";
  return "other";
}

/** True when the user asked the OS to minimise motion. */
export function prefersReducedMotion(): boolean {
  try {
    return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return false;
  }
}
