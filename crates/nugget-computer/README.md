# nugget-computer

Computer use for Nugget's agent: `nugget_agent::Tool`s that drive any desktop app through its
accessibility tree (`nugget_a11y::ComputerUse`), by stable element id rather than pixels.

```rust
let tools = nugget_computer::tools(nugget_computer::ComputerConfig::default());
let tools = nugget_computer::tools_with(backend, config);   // any backend (tests, the bench)
```

| Tool | Does | Risk |
|------|------|------|
| `observe` | renders a window's tree: `[e1a2b3c] button "Send"` lines | Observe |
| `click` | press (or select) an element by id | Edit, raised by the label (send / destroy / spend / security words); unknown id → Send |
| `type_text` | literal text into an element or the focus | Edit; secret field or terminal → Security |
| `press_key` | `enter`, `ctrl+a`, `shift+tab`… (strictly parsed) | Edit; enter → Send (Edit in a multi-line field); shift/cmd+delete → Destroy; terminal → Security |
| `set_value` | set a value, or pick a combo box / list option | like `type_text` |
| `scroll` | scroll an element | Observe |
| `open_app` | launch or focus an app | Edit |
| `wait_for` | poll until an element appears / disappears / has a value | Observe |

Full table in `src/lib.rs`.

- **Verified.** Mutating tools observe the element and a window fingerprint before and after;
  `verify` passes only if the backend reports the effect *and* the observation shows it.
- **Untrusted screen text** is escaped so it cannot forge element lines, ids or fences; results
  are marked untrusted.
- **Secure fields** are never typed into and are masked in previews and logs.
- **Targets**: screens list their items by label in `ToolResult::targets` for the agent's
  ambiguity check, never sent to the model (`src/targets.rs`).
- **Screenshot fallback** (`screenshot_fallback`, off by default) only when a window has no
  actionable element; never used for coordinate clicks.

`ComputerConfig` (all optional): snapshot limits, render caps, `screenshot_fallback`, `wait_for`
timeouts, extra `risk_keywords` and `terminal_apps`.
