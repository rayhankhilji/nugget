# Writing a connector

A connector turns one source into a stream of changes; ingest owns everything after that, and
connectors never touch the store. Connectors are native Rust crates or WebAssembly plugins (below).

## The API (`nugget-connector`)

```rust,ignore
pub trait Connector: Send + Sync {          // #[async_trait]
    fn kind(&self) -> &'static str;
    async fn sync(&self, cursor: Option<Value>, sink: &dyn ChangeSink,
                  cancel: &CancellationToken) -> Result<Value>;   // returns the next cursor
    fn poll_interval(&self) -> Duration;    // default 300 s
    fn supports_watch(&self) -> bool;       // then `watch` streams live changes after a sync
    async fn begin_auth(&self) -> Result<AuthFlow>;               // interactive sign-in, if any
}
pub trait ConnectorFactory: Send + Sync {
    fn info(&self) -> KindInfo;             // kind, display_name, auth, settings_schema
    fn validate(&self, settings: &Value) -> Result<()>;
    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>>;
}
```

`sync` sends `Change::Upsert(Document)` and `Change::Delete(ItemKey)`. A `Document` has a stable
`key`, a `mime`, `content` (`Text`, `Bytes` or `File`), a cheap `version`, and optional `title`,
`modified_at`, `url` and `metadata`. `ConnectorContext` provides `source`, `paths`, `egress`, a
source-scoped keychain `secrets` store, and `known` (keys and versions already stored).

## The contract

1. Call `sink.checkpoint(cursor)` during long passes (committed only after earlier changes are stored).
2. No cursor (first run or changed settings) means **reconcile**: upsert everything in scope and delete
   every `ctx.known` key no longer in scope.
3. `version` changes when content does; matching versions are skipped. `.await` every `send`.
4. Network through `nugget_connector::http::MeteredHttp` (egress feature `sync:<kind>`, no redirects).
5. Secrets only via `ctx.secrets`, never in settings, logs or errors; bad credentials are
   `Error::Unauthorized`. Cancellation returns `Error::Cancelled` promptly.
6. Treat input as hostile (caps, timeouts, no panics) and never change the user's data at the source.

## Settings and registration

Settings use `#[serde(deny_unknown_fields)]`, are checked in `validate`, publish a JSON Schema in
`KindInfo::settings_schema`, and are documented in [Connector settings](../reference/connectors.md)
(add the struct to `SOURCES` and the file to `CONNECTOR_FILES` in `docs/book/check-refs.py`). Register
the factory in `registry()` in `crates/nuggetd/src/sources.rs`. Credential patterns:

- **`AuthKind::OAuth`** (Google, Microsoft): `begin_auth` runs PKCE with a loopback redirect
  (`nugget-oauth`); tokens go under `oauth:<source>`, a client secret under
  `oauth_client_secret:<source>` via `sources.secret.set`, injected at build time.
- **`AuthKind::Password`** (`imap`, `caldav`, SaaS): a pasted password or token under
  `password:<source>`; may also offer `begin_auth` (`github` device flow, `linear` PKCE).
- **`AuthKind::None`** (`files`, `obsidian`).

**Testing:** point HTTP connectors at a mock server (`with_endpoints`, `with_base`) with recorded
fixtures; cover first and incremental sync, deletes, a reset cursor, an invalid sync token,
cancellation, bad credentials, egress recording and malformed input.

## Plugin connectors (WebAssembly)

A component built against the SDK (`crates/nugget-plugin/sdk`) runs as kind `plugin:<plugin id>`. Its
manifest's `[connector]` section sets `display_name`, `auth` (`none` or `password`),
`poll_interval_secs` and `settings_schema`. It declares capabilities (HTTP hosts, named secrets,
key-value size) that the user approves; the host makes all HTTP requests (ledgered as
`sync:plugin:<plugin id>`) and keeps secrets under `plugin:<plugin id>:<name>`. Example:
`crates/nugget-plugin/sdk/examples/feed-reader`. Author guide:
[`docs/plugins.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/plugins.md).
