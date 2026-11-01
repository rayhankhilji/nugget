# Native Double Tap overlay run

`run.mjs` drives the real debug build of `nugget-desktop` with the real `nuggetd` on a real desktop
session (Linux X11 under Xvfb, Windows, macOS). It starts a fake OpenAI-compatible provider
(`mock-provider.mjs`, a fixed draft in six chunks) and a daemon that drafts only with it, opens a
text field in a native app (the GTK fixture of `nugget-a11y`'s AT-SPI tests, Notepad, TextEdit),
presses the real trigger with synthesized keys (XTest, `SendInput`, `CGEvent`), and checks that the
overlay window appears in the OS window list, streams the draft, fits itself to it, retries with R,
and that Enter inserts the draft, read back from the target app itself.

The overlay's state comes from a probe, not the screen: in debug builds only, with
`NUGGET_E2E_PROBE=<file>`, `src-tauri/src/doubletap/probe.rs` appends one JSON line per step.
Release builds compile it out; the trigger is always the real key press.

`--out` gets `summary.json`, `probe.jsonl`, both logs and screenshots (`failure.png` on failure).

## Running

```sh
cargo build -p nuggetd
(cd apps/desktop && pnpm install --frozen-lockfile && pnpm tauri build --debug --no-bundle)
```

- Linux (needs `xvfb openbox xdotool xautomation x11-utils imagemagick dbus-x11 at-spi2-core
  python3-gi gir1.2-gtk-3.0`): `apps/desktop/e2e-native/run-linux.sh --out /tmp/overlay`.
- Windows / macOS, from a desktop session: `node apps/desktop/e2e-native/run.mjs --out <dir>` (macOS
  needs Accessibility for the terminal and `swiftc`). It presses real keys: don't use the machine
  while it runs.

Binaries default to `$CARGO_TARGET_DIR/debug` (else `<repo>/target/debug`); `--app` and `--nuggetd`
override them. CI: the `overlay` job in `.github/workflows/real-os.yml`. Per-OS code lives in
`linux.mjs`, `windows.mjs` (+ `windows-helper.ps1`) and `macos.mjs` (+ `macos-helper.swift`);
`helper.mjs` is the line protocol to their helper processes.
