# nugget-computer

Computer use for Nugget's agent: `nugget_agent::Tool`s that drive any desktop app through its
accessibility tree (`nugget_a11y::ComputerUse`), by stable element id rather than by pixels.

```rust
let tools = nugget_computer::tools(nugget_computer::ComputerConfig::default());
// or, over any backend (tests, the bench):
let tools = nugget_computer::tools_with(backend, config);
```

| tool | does | risk |
|------|------|------|
| `observe` | renders a window's tree: `[e1a2b3c] button "Send"` lines | Observe |
| `click` | press (or select) an element by id | Edit, raised by the element's label (send / destroy / spend / security words); unknown id → Send |
| `type_text` | literal text into an element or the focus | Edit; secret field or terminal → Security |
| `press_key` | `enter`, `ctrl+a`, `shift+tab`… (strictly parsed) | Edit; enter → Send (Edit in a multi-line field); shift/cmd+delete → Destroy; terminal → Security |
| `set_value` | set a value, or pick a combo box / list option by name | like `type_text` |
| `scroll` | scroll an element | Observe |
| `open_app` | launch or bring an app to the front | Edit |
| `wait_for` | poll until an element appears / disappears / has a value | Observe |

The full table is in the crate docs (`src/lib.rs`).

- **Verified, not assumed.** Mutating tools observe the element and a fingerprint of its window
  before and after. `verify` passes only when the backend reports the effect *and* the
  after-observation shows it; it fails when the observation contradicts the intent.
- **Untrusted screen text.** Names and values are escaped so they cannot forge element lines,
  ids or fences, and results are marked untrusted so the agent loop fences them.
- **Secure fields** are never typed into; their text is masked in previews and logs.
- **Targets.** Results that show a screen (`observe`, actions that return the changed window) list
  its items (rows, files, events, products) by label in `ToolResult::targets`, for the agent's
  ambiguity check; never sent to the model (`src/targets.rs`).
- **Screenshot fallback** (`screenshot_fallback = true`, off by default) only when a window has
  no actionable element; the PNG goes to the result's data, never to coordinate clicks.

Configuration (`ComputerConfig`, every field optional): snapshot limits, render caps,
`screenshot_fallback`, `wait_for` timeouts, extra `risk_keywords` and `terminal_apps`.
