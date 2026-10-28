// Wire protocol between the Nugget native-messaging host (`crates/nugget-browser`) and this
// extension. Field names are snake_case to match the Rust serde types one to one; keep the two
// in step (`crates/nugget-browser/src/protocol.rs`, `snapshot.rs`, `action.rs`).

export const PROTOCOL_VERSION = 1;

/** Native-messaging host name registered by `nugget-browser-host install`. */
export const HOST_NAME = "dev.nugget.browser";

/** Chrome refuses host-to-extension messages over 1 MiB; we hold ourselves to the same. */
export const MAX_FROM_HOST_BYTES = 1024 * 1024;
/** Our own cap on extension-to-host messages (Chrome allows 64 MiB; snapshots stay far below). */
export const MAX_TO_HOST_BYTES = 4 * 1024 * 1024;

export const LIMITS = {
  maxElements: 1500,
  maxTextChars: 20_000,
  maxNameChars: 160,
  maxTitleChars: 500,
  maxUrlChars: 8192,
  maxHrefChars: 2048,
  maxValueChars: 500,
  maxOptions: 100,
  maxTypeChars: 10_000,
  maxWaitMs: 15_000,
} as const;

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type Sensitive = "password" | "payment";

export interface SelectOption {
  value: string;
  label: string;
  selected: boolean;
}

/** One interactive element, flattened in document order (shadow roots and same-origin frames
 * included). `id` is stable for the element's lifetime within one document. */
export interface ElementInfo {
  id: string;
  role: string;
  name: string;
  tag: string;
  input_type?: string;
  /** Current value. Never present for `sensitive` fields. */
  value?: string;
  checked?: boolean;
  options?: SelectOption[];
  href?: string;
  /** Marked password or payment field; its value is never read. */
  sensitive?: Sensitive;
  disabled: boolean;
  editable: boolean;
  focused: boolean;
  visible: boolean;
  rect: Rect;
  /** Id of the owning form element, when the element belongs to one. */
  form?: string;
  /** Id of the frame (see `frames`) the element lives in; absent for the top document. */
  frame?: string;
  in_shadow: boolean;
}

export interface FrameInfo {
  id: string;
  origin: string;
  same_origin: boolean;
  /** Whether its contents are part of this snapshot (same-origin frames only). */
  traversed: boolean;
  rect: Rect;
}

export interface Viewport {
  width: number;
  height: number;
  scroll_x: number;
  scroll_y: number;
}

export interface PageSnapshot {
  snapshot_id: string;
  url: string;
  title: string;
  captured_at_ms: number;
  viewport: Viewport;
  elements: ElementInfo[];
  elements_truncated: boolean;
  text: string;
  text_truncated: boolean;
  frames: FrameInfo[];
}

export interface ElementChange {
  id: string;
  fields: string[];
}

export interface SnapshotDiff {
  url_changed?: { from: string; to: string };
  title_changed?: { from: string; to: string };
  added: string[];
  removed: string[];
  changed: ElementChange[];
  text_changed: boolean;
}

export type WaitCondition =
  | { text_present: string }
  | { element_present: { role?: string; name?: string } }
  | { url_contains: string }
  | { element_gone: string };

export type BrowserAction =
  | { kind: "click"; element: string }
  | { kind: "type"; element: string; text: string; clear?: boolean }
  | { kind: "select"; element: string; value: string }
  | { kind: "scroll"; element?: string; dx?: number; dy?: number }
  | { kind: "navigate"; url: string }
  | { kind: "submit"; element: string }
  | { kind: "wait_for"; until: WaitCondition; timeout_ms?: number };

export interface SnapshotOptions {
  include_text?: boolean;
  include_hidden?: boolean;
}

export type HostRequest =
  | { id: number; type: "hello"; protocol: number }
  | { id: number; type: "snapshot"; tab_id?: number; options?: SnapshotOptions }
  | { id: number; type: "act"; tab_id?: number; snapshot_id: string; action: BrowserAction }
  | { id: number; type: "overlay"; tab_id?: number; active: boolean; label?: string }
  | { id: number; type: "halt" };

export interface ActResult {
  before_snapshot_id: string;
  snapshot: PageSnapshot;
  diff: SnapshotDiff;
}

export interface HelloResult {
  protocol: number;
  extension_version: string;
  browser: string;
}

export type ErrorCode =
  | "bad_request"
  | "too_large"
  | "no_tab"
  | "permission_required"
  | "stale_snapshot"
  | "unknown_element"
  | "not_visible"
  | "disabled"
  | "not_editable"
  | "sensitive_field"
  | "bad_option"
  | "bad_url"
  | "timeout"
  | "halted"
  | "internal";

export interface ReplyError {
  code: ErrorCode;
  message: string;
}

export type ExtensionMessage =
  | { type: "reply"; id: number; result: unknown }
  | { type: "reply"; id: number; error: ReplyError }
  | { type: "event"; event: "user_stop"; reason: string }
  | { type: "event"; event: "permission_changed"; origins: string[]; granted: boolean };

export class ProtocolError extends Error {
  constructor(
    readonly code: ErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "ProtocolError";
  }
}
