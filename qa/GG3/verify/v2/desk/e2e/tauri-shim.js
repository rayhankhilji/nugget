// Injected before the app loads (page.addInitScript). Provides the bits of the Tauri 2 webview
// runtime that @tauri-apps/api uses: `invoke` POSTs `{ cmd, args }` to the bridge (bridge.mjs) and
// rejects with the shell's `{ kind, message, code }` error shape. Event plugin calls (listen,
// unlisten, emit) succeed; events reach the listeners only when the test pushes them with
// `window.__E2E_EMIT__(event, payload)` (bridge.mjs `deliverEvents(page)` does, for the daemon's
// notifications).
(() => {
  let next = 1;
  const callbacks = new Map();
  /** eventId -> { event, handler } (the callback id from transformCallback). */
  const listeners = new Map();
  window.__E2E_EMIT__ = (event, payload) => {
    for (const [id, l] of [...listeners]) {
      if (l.event === event) callbacks.get(l.handler)?.({ event, id, payload });
    }
  };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
    transformCallback(cb) {
      const id = next++;
      callbacks.set(id, cb);
      return id;
    },
    unregisterCallback(id) {
      callbacks.delete(id);
    },
    convertFileSrc(p) {
      return p;
    },
    async invoke(cmd, args) {
      if (cmd === "plugin:event|listen") {
        const id = next++;
        listeners.set(id, { event: args?.event, handler: args?.handler });
        return id;
      }
      if (cmd === "plugin:event|unlisten") {
        listeners.delete(args?.eventId);
        return null;
      }
      if (cmd.startsWith("plugin:")) return null;
      const r = await fetch("/invoke", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ cmd, args: args ?? {} }),
      });
      const j = await r.json();
      if (j.err) throw j.err;
      return j.ok;
    },
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener(_event, eventId) {
      listeners.delete(eventId);
    },
  };
})();
