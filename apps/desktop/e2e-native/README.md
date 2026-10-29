# Native Double Tap overlay run

`run.mjs` drives the **real desktop app** (the debug build of `nugget-desktop`, with the real
`nuggetd`) on a real desktop session, on Linux (X11 under Xvfb), Windows and macOS:

1. starts a fake OpenAI-compatible provider on `127.0.0.1` (`mock-provider.mjs`): it streams a
   fixed draft in six chunks, 250 ms apart, and answers the daemon's non-streaming prewarm;
2. starts `nuggetd` in a temp home whose `config.toml` has that provider as its only one (no
   runtime, no knowledge extraction, no MCP listener; `../e2e/daemon.mjs`);
3. starts `nugget-desktop` with `NUGGET_HOME` set to that home, and waits for its Double Tap
   listener and its daemon connection;
4. opens a text field in a native app and brings it to the front: the GTK 3 fixture of
   `nugget-a11y`'s AT-SPI tests on Linux (composer, holding `Hello `), Notepad on Windows,
   TextEdit on macOS (both on an empty scratch file);
5. presses the real trigger with synthesized keys: double tap Right Ctrl through XTest
   (`xte`) on Linux and `SendInput` on Windows, double tap Right Command as posted
   `CGEvent`s on macOS (the app's recommended trigger on each). A missed tap is retried (up to
   `--attempts`, reported);
6. checks that **the overlay window appears**: the app's probe says it was shown, and the OS
   window list has it visible (`xdotool search --onlyvisible`; `EnumWindows` +
   `IsWindowVisible`; `CGWindowListCopyWindowInfo` on-screen windows of the app's pid at the
   overlay's size, plus its Accessibility window titles);
7. checks that **it streams the fake provider's draft**: several growing `streaming` views, then
   `done` with the provider `fake` and the streamed text, and screenshots it (`overlay.png`
   cropped to the window, `overlay-screen.png`, `overlay-streaming.png` mid-stream);
8. **accepts with Enter**, the overlay's own key. If the window system did not give the overlay
   the keyboard, it clicks the overlay's text first (no button there) and the summary says
   `overlay focused: false`;
9. checks that **the draft is inserted into the target**: the overlay reaches `inserted`, and the
   text is read back from the target app itself (the fixture's own report of its buffer on
   Linux, UI Automation on Notepad, Accessibility on TextEdit), with the field's original text
   kept; then the overlay hides.

It writes to `--out`: `summary.json` (timings, insert method, verified flag, the OS window rect),
`probe.jsonl`, `nugget-desktop.log`, `nuggetd.log`, and screenshots (`before.png`,
`overlay*.png`, `after.png`, `failure.png` on failure). On CI it also appends a table to the job
summary. Timings are measured between probe timestamps (`trigger` → `show` → first streamed view →
`done`) and from the Enter press to `inserted`.

## The probe

The overlay is a webview, so its phase and text are read from a probe rather than scraped from
the screen: in **debug builds only**, with `NUGGET_E2E_PROBE=<file>`, the Double Tap flow
(`src-tauri/src/doubletap/probe.rs`) appends one JSON line per step: the listener starting, the
trigger firing, each overlay view it emits (the same `OverlayView` the overlay renders), the
window being shown or hidden (with the window system's position, size, visibility and focus) and
the insert result. Release builds compile it to nothing. No test-only trigger exists: the trigger
is always the real key press.

## Running it

Build first (the debug build must embed the frontend, so use the Tauri CLI, not `cargo build`):

```sh
cargo build -p nuggetd
(cd apps/desktop && pnpm install --frozen-lockfile && pnpm tauri build --debug --no-bundle)
```

- **Linux** (needs `xvfb openbox xdotool xautomation x11-utils imagemagick dbus-x11 at-spi2-core
  python3-gi gir1.2-gtk-3.0` plus the Tauri build packages): `apps/desktop/e2e-native/run-linux.sh
  --out /tmp/overlay`. It starts Xvfb, openbox (which hands focus back to the target when the
  overlay hides, as desktop window managers do) and a private session bus, then `run.mjs`.
- **Windows / macOS**, from a desktop session: `node apps/desktop/e2e-native/run.mjs --out
  <dir>`. On macOS the terminal (or the process that starts the run) needs Accessibility, and
  `swiftc` (Xcode command line tools) builds the helper. On GitHub's macOS runners the job's
  processes have Accessibility already. **It presses real keys and clicks: don't use the
  machine while it runs.**

Binaries default to `$CARGO_TARGET_DIR/debug` (else `<repo>/target/debug`); `--app` and
`--nuggetd` override them. CI: the `overlay` job (`.github/workflows/real-os.yml`).

## Files

- `run.mjs`: the scenario, platform independent.
- `mock-provider.mjs`: the fake provider.
- `helper.mjs`: line protocol to a long-running helper process.
- `linux.mjs` (+ `run-linux.sh`), `windows.mjs` (+ `windows-helper.ps1`), `macos.mjs`
  (+ `macos-helper.swift`): keys, clicks, window list, foreground app, target app, read-back,
  screenshots.
