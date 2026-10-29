// Injected before the app loads (page.addInitScript). Provides the bits of the Tauri 2 webview
// runtime that @tauri-apps/api uses: `invoke` POSTs `{ cmd, args }` to the bridge (bridge.mjs) and
// rejects with the shell's `{ kind, message, code }` error shape. Event plugin calls (listen,
// unlisten, emit) succeed but no events are delivered.
(() => {
  let next = 1;
  const callbacks = new Map();
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
      if (cmd.startsWith("plugin:")) return cmd === "plugin:event|listen" ? next++ : null;
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
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
})();
