# Writing a connector

A connector turns one configured source into a stream of changes. The ingest pipeline owns everything
after that: extraction, chunking, embedding, storage, scheduling and cursor persistence. Connectors
never touch the store.

Today connectors are **native Rust crates** compiled into the daemon. **WASM connectors** are planned
for M6 (below).

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

A `sync` pass sends `Change::Upsert(Document)` and `Change::Delete(ItemKey)` to the `ChangeSink`. A
`Document` has a stable `key`, a `mime` type, `content` (`Text`, `Bytes` or a local `File`), a cheap
`version` token, and optionally a `title`, `modified_at`, `url` and JSON `metadata`.

`ConnectorContext` gives the connector its `source` id, `paths`, an `egress` sink, a `secrets` store
(the OS keychain, scoped to this source) and `known`: the item keys and versions the store already
holds for this source.

## The contract

1. **Cursors after persistence.** Long passes call `sink.checkpoint(cursor)` periodically. The
   pipeline commits a checkpoint only after every change sent before it is stored, so a crash never
   skips data. Return the final cursor from `sync`.
2. **A pass without a cursor reconciles.** `cursor = None` is not only the first run: ingest also resets
   the cursor when the source's settings change, while items indexed under the old settings remain.
   Such a pass must upsert every current document and **delete every known key that is no longer in
   scope**, using `ctx.known`.
3. **Versions make re-sends cheap.** Set `version` to something that changes when the content does
   (mtime and size, an ETag, a mod-sequence). The pipeline skips a document whose version matches the
   stored one.
4. **Order and backpressure.** Changes of one source are applied in the order sent. `send` waits when
   the pipeline is busy; just `.await` it.
5. **Egress.** Every network exchange is recorded with feature `sync:<kind>`. Use
   `nugget_connector::http::MeteredHttp`, which records each request and never follows redirects.
6. **Secrets.** Credentials only through `ctx.secrets`, under the account names in
   `nugget_core::secrets` (`password:<source>`, `oauth:<source>`). Never in settings, logs or errors.
   Missing or rejected credentials are `Error::Unauthorized`, which puts the source in `needs_auth`.
7. **Cancellation.** Honour `cancel` promptly and return `Error::Cancelled`.
8. **Hostile input.** Everything from the network or disk is untrusted: cap sizes and counts, time out,
   never panic.
9. **Read-only.** A connector must not change the user's data at the source.

## Settings

Settings are a JSON object stored in `sources.toml`. Deserialize them into a struct with
`#[serde(deny_unknown_fields)]`, validate ranges in `validate`, and publish a JSON Schema in
`KindInfo::settings_schema` (the desktop app builds forms from it). Never accept secrets in settings.

Document every key in [Connector settings](../reference/connectors.md). The docs check compares that
page with your settings struct.

## Registering the kind

Expose a factory and register it in `nuggetd`'s registry (`crates/nuggetd/src/sources.rs`):

```rust,ignore
pub fn registry() -> Registry {
    let mut r = Registry::new();
    nugget_connector_files::register(&mut r);
    nugget_connector_google::register(&mut r);
    r.register(Arc::new(nugget_connector_imap::ImapFactory::new()));
    r.register(Arc::new(nugget_connector_caldav::CalDavFactory::new()));
    r
}
```

Then add the kind to the desktop's add-source wizard if it needs a custom form.

## Testing

Follow the existing connectors (`nugget-connector-imap`, `nugget-connector-caldav`,
`nugget-connector-google`):

- a fake server (wiremock, or a scripted protocol server) with **recorded fixtures**;
- tests for the first sync, an incremental sync, deletes, a reset cursor (reconciliation), an invalid
  sync token (full resync), cancellation, `Unauthorized` on bad credentials, and egress being recorded;
- malformed-input tests for every parser.

## WASM connectors (planned, M6)

Third-party connectors will be **WebAssembly components** loaded by `nugget-plugin` (wasmtime, with
WIT interfaces mirroring the `Connector` contract above). A component has no ambient file, network or
process access: it declares the capabilities it needs (network hosts, scopes), the user approves them,
and the host performs network requests for it, so they land in the egress ledger like any other. The
WIT interface and SDK are not published yet; until they are, contribute connectors as native crates.
