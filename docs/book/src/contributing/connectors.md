# Writing a connector

A connector turns one source into a stream of changes; ingest owns everything after that, and
connectors never touch the store. Connectors are native Rust crates or WebAssembly plugins (below).

## The API (`nugget-connector`)

```rust,ignore
#[async_trait]
pub trait Connector: Send + Sync {
    fn kind(&self) -> &'static str;

    /// One pass from `cursor` (None = no cursor); returns the cursor to resume from.
    async fn sync(&self, cursor: Option<Value>, sink: &dyn ChangeSink,
                  cancel: &CancellationToken) -> Result<Value>;

    fn poll_interval(&self) -> Duration { Duration::from_secs(300) }
    fn supports_watch(&self) -> bool { false }
    /// Live changes until cancelled; called after a successful sync.
    async fn watch(&self, sink: Arc<dyn ChangeSink>, cancel: CancellationToken) -> Result<()>;
    /// Interactive sign-in (OAuth), if the kind needs one.
    async fn begin_auth(&self) -> Result<AuthFlow>;
}

pub trait ConnectorFactory: Send + Sync {
    fn info(&self) -> KindInfo;   // kind, display_name, auth (None | OAuth | Password), settings_schema
    fn validate(&self, settings: &Value) -> Result<()>;
    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>>;
}
```

`sync` sends `Change::Upsert(Document)` and `Change::Delete(ItemKey)`. A `Document` has a stable
`key`, a `mime`, `content` (`Text`, `Bytes` or `File`), a cheap `version`, and optional `title`,
`modified_at`, `url` and `metadata`. `ConnectorContext` provides `source`, `paths`, `egress`, a
source-scoped keychain `secrets` store, and `known` (keys and versions already stored).

## The contract

1. **Cursors after persistence.** Call `sink.checkpoint(cursor)` during long passes; it commits only
   after everything sent before it is stored. Return the final cursor.
2. **No cursor means reconcile.** Also after a settings change: upsert everything in scope and delete
   every `ctx.known` key that is no longer in scope.
3. **Versions** change when content does (mtime and size, ETag, mod-sequence); matching versions are
   skipped.
4. **Order and backpressure:** changes apply in order; `.await` the `send`.
5. **Egress:** use `nugget_connector::http::MeteredHttp` (feature `sync:<kind>`, no redirects).
6. **Secrets** only through `ctx.secrets` (`password:<source>`, `oauth:<source>`), never in settings,
   logs or errors. Missing or rejected credentials are `Error::Unauthorized` (`needs_auth`).
7. **Cancellation:** return `Error::Cancelled` promptly.
8. **Hostile input:** cap sizes and counts, time out, never panic.
9. **Read-only:** never change the user's data at the source.

## Settings and registration

Deserialize settings with `#[serde(deny_unknown_fields)]`, validate in `validate`, publish a JSON
Schema in `KindInfo::settings_schema` (the app builds forms from it), and never accept secrets. Document
every key in [Connector settings](../reference/connectors.md) and add the struct to `SOURCES` and the
file to `CONNECTOR_FILES` in `docs/book/check-refs.py`.

Register the factory in `registry()` in `crates/nuggetd/src/sources.rs`. Credential patterns:

- **`AuthKind::OAuth`** (Google, Microsoft): `begin_auth` runs PKCE with a loopback redirect
  (`nugget-oauth`); tokens go under `oauth:<source>`, a client secret under
  `oauth_client_secret:<source>` via `sources.secret.set`, injected at build time.
- **`AuthKind::Password`** (`imap`, `caldav`, SaaS): a pasted password or token under
  `password:<source>`; may also offer `begin_auth` (`github` device flow, `linear` PKCE).
- **`AuthKind::None`** (`files`, `obsidian`).

## Testing

Point HTTP connectors at a mock server (`with_endpoints`, `with_base`) with recorded fixtures. Cover
first sync, incremental sync, deletes, a reset cursor, an invalid sync token, cancellation, bad
credentials and egress recording, plus malformed input for every parser.

## Plugin connectors (WebAssembly)

A component built against the SDK (`crates/nugget-plugin/sdk`) runs as kind `plugin:<plugin id>`. Its
manifest's `[connector]` section sets `display_name`, `auth` (`none` or `password`),
`poll_interval_secs` and `settings_schema`. It declares capabilities (HTTP hosts, named secrets,
key-value size) that the user approves; the host makes all HTTP requests (ledgered as
`sync:plugin:<plugin id>`) and keeps secrets under `plugin:<plugin id>:<name>`. Example:
`crates/nugget-plugin/sdk/examples/feed-reader`. Author guide:
[`docs/plugins.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/plugins.md).
