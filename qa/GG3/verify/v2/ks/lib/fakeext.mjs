// A stand-in for the Nugget browser extension + native host (extensions/browser + nugget-browser-host),
// speaking the real daemon protocol: it calls the long-lived `browser.host.attach`, receives
// `browser.host.request` notifications ({id, type: hello|snapshot|act|overlay|halt, ...}) and answers
// with `browser.host.deliver` ({type: "reply", id, result|error}). It mirrors the extension's
// router semantics (extensions/browser/src/router.ts): after a `halt` request it refuses snapshot,
// act and overlay(active) with code "halted" until `resume()` (the popup's Resume).
//
// The page is a synthetic webmail compose form on https://mail.example.test/compose. Every `act`
// that reaches this "extension" is the point where a real browser would have performed the action,
// so `acts` (timestamped) is the ground truth for "did an action execute".
import { Ipc, now } from "./ipc.mjs";

const rect = { x: 10, y: 10, width: 200, height: 24 };
const el = (id, role, name, extra = {}) => ({
  id,
  role,
  name,
  tag: role === "button" ? "button" : "input",
  disabled: false,
  editable: role === "textbox",
  focused: false,
  visible: true,
  rect,
  form: "f1",
  in_shadow: false,
  ...extra,
});

export async function startFakeExtension(socket) {
  const attachConn = new Ipc(socket);
  await attachConn.connect();
  const deliverConn = new Ipc(socket);
  await deliverConn.connect();
  const st = {
    halted: false,
    acts: [],
    requests: [],
    page: { to: "", body: "", sent: 0, url: "https://mail.example.test/compose" },
    snapSeq: 0,
    actDelayMs: {}, // by action kind
  };
  const snapshot = () => ({
    snapshot_id: `s${++st.snapSeq}`,
    url: st.page.url,
    title: st.page.sent ? "Sent - QA Mail" : "Compose - QA Mail",
    captured_at_ms: Date.now(),
    viewport: { width: 1280, height: 800, scroll_x: 0, scroll_y: 0 },
    elements: [
      el("n1", "textbox", "To", { input_type: "email", value: st.page.to }),
      el("n2", "textbox", "Message", { value: st.page.body }),
      el("n3", "button", "Send", { input_type: "submit" }),
    ],
    elements_truncated: false,
    text: `Compose a message. Sent so far: ${st.page.sent}`,
    text_truncated: false,
    frames: [],
  });
  let latest = snapshot();
  const reply = (id, result, error) =>
    deliverConn.call("browser.host.deliver", error ? { type: "reply", id, error } : { type: "reply", id, result });
  attachConn.onNote(async (n) => {
    if (n.method !== "browser.host.request") return;
    const m = n.params;
    st.requests.push({ t: n.t, type: m.type, id: m.id, action: m.action, active: m.active });
    try {
      switch (m.type) {
        case "hello":
          return reply(m.id, { protocol: 1, extension_version: "qa-fake", browser: "chromium-qa" });
        case "halt":
          st.halted = true;
          return reply(m.id, {});
        case "overlay":
          if (st.halted && m.active) return reply(m.id, null, { code: "halted", message: "Nugget was stopped by the user" });
          return reply(m.id, {});
        case "snapshot":
          if (st.halted) return reply(m.id, null, { code: "halted", message: "Nugget was stopped by the user" });
          latest = snapshot();
          return reply(m.id, latest);
        case "act": {
          if (st.halted) return reply(m.id, null, { code: "halted", message: "Nugget was stopped by the user" });
          const a = m.action;
          const rec = { t: now(), kind: a.kind, action: a, done: null };
          st.acts.push(rec);
          const delay = st.actDelayMs[a.kind] ?? 0;
          if (delay) await new Promise((r) => setTimeout(r, delay));
          const before = latest;
          if (a.kind === "type") {
            if (a.element === "n1") st.page.to = a.text;
            else st.page.body = a.text;
          } else if (a.kind === "submit" || (a.kind === "click" && a.element === "n3")) {
            st.page.sent += 1;
          }
          rec.done = now();
          latest = snapshot();
          const changed = a.kind === "type" ? [{ id: a.element, fields: ["value"] }] : [];
          return reply(m.id, {
            before_snapshot_id: before.snapshot_id,
            snapshot: latest,
            diff: { added: [], removed: [], changed, text_changed: a.kind !== "type" },
          });
        }
        default:
          return reply(m.id, null, { code: "bad_request", message: `unknown ${m.type}` });
      }
    } catch (e) {
      st.requests.push({ t: now(), type: "fake-ext-error", error: String(e) });
    }
  });
  // Long-lived: returns only when the daemon detaches this host.
  attachConn.call("browser.host.attach", { origin: "chrome-extension://qafakeextension/", host_version: "qa" }).catch(() => {});
  return {
    st,
    resume: () => (st.halted = false),
    userStop: () => deliverConn.call("browser.host.deliver", { type: "event", event: "user_stop", reason: "qa: Stop pressed in page overlay" }),
    reset() {
      st.acts = [];
      st.requests = [];
      st.page = { to: "", body: "", sent: 0, url: "https://mail.example.test/compose" };
      st.actDelayMs = {};
    },
    close() {
      attachConn.close();
      deliverConn.close();
    },
  };
}
