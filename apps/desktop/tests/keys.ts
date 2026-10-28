/** Keyboard and typing helpers for the shell tests. */
import { act } from "react";
import { flush } from "../src/test/render";

/** Dispatch a keydown on `target` (default: the focused element), letting it bubble to window. */
export async function key(init: KeyboardEventInit & { key: string }, target: EventTarget | null = document.activeElement ?? document.body): Promise<KeyboardEvent> {
  const e = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  await act(async () => {
    (target ?? document.body).dispatchEvent(e);
  });
  await flush(2);
  return e;
}

/** Set an input's value the way a user would, so React's onChange fires. */
export async function type(input: HTMLInputElement, value: string): Promise<void> {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush(2);
}

/** Skip onboarding and open the app on `page`. */
export function finishedOnboarding(page?: string): void {
  localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true }));
  if (page) localStorage.setItem("nugget.page", page);
}
