# Nugget browser extension

Manifest V3 extension for Chrome, Chromium, Edge (`dist/chromium`) and Firefox 128+ (`dist/firefox`).
It lets the local Nugget daemon read and act on **sites the user has allowed**. Its only channel out is
native messaging to `nugget-browser-host` (`crates/nugget-browser`): no network access and no localhost ports.

```sh
pnpm install && pnpm build && pnpm test      # unit tests (vitest + jsdom)
NUGGET_E2E=1 pnpm test:e2e                   # optional: real Chromium via the preinstalled playwright
cargo run -p nugget-browser --bin nugget-browser-host -- install --dry-run   # register the native host
```

## Permissions
| Permission | Why |
|---|---|
| `nativeMessaging` | talk to `nugget-browser-host` |
| `scripting` | inject `content.js` on demand, only into tabs on granted sites |
| `activeTab` | lets the popup see the current tab's URL so it can offer "Allow on this site" (not treated as consent) |
| `optional_host_permissions: http(s)://*/*` | **opt-in per site**: requested from the popup on a user click; nothing is granted at install |

CSP for extension pages: `script-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`.
Bundles are plain esbuild IIFEs; the build fails if `eval`, `new Function`, `importScripts` or remote scripts appear.

## Layout
- `background.ts`: native port, consent checks (`router.ts`), kill switch. Navigation goes only to granted sites.
- `content.ts` → `agent.ts`: per-document element registry, snapshots (`snapshot.ts`), validated actions
  (`actions.ts`), post-action snapshot + diff (`diff.ts`), and the overlay (`overlay.ts`, closed shadow root, Stop button).
- `popup.ts`: allow/remove site, Stop, and "Let Nugget act again" (only the user can resume after a Stop).
- `framing.ts`: message validation and size caps (1 MiB host→extension, 4 MiB extension→host).
- `protocol.ts`: wire types; mirrored in `crates/nugget-browser/src/{protocol,snapshot,action}.rs`.
  `test/contract.test.ts` writes the shared fixtures in `crates/nugget-browser/tests/fixtures`.

The `key` in `static/manifest.base.json` is a public key that pins the unpacked build's id to
`gljflghdpajgdddaibpdgjibnkeggchn` (the installer's default). Store builds pass `--extension-id`.
