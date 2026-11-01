# nugget-computer

Computer use for Nugget's agent: `nugget_agent::Tool`s that drive any desktop app through its
accessibility tree (`nugget_a11y::ComputerUse`), by stable element id rather than by pixels.

```rust
let tools = nugget_computer::tools(nugget_computer::ComputerConfig::default());
// or, over any backend (tests, the bench):
let tools = nugget_computer::tools_with(backend, config);
```

Tools: `observe`, `click`, `type_text`, `press_key`, `set_value`, `scroll`, `open_app`,
`wait_for`. The crate docs (`src/lib.rs`) describe the risk classes, verification, untrusted
screen text, secure fields, targets and the screenshot fallback.
