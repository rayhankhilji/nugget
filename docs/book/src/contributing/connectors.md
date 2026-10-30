# Writing a connector

A connector turns one configured source into a stream of changes. The ingest pipeline owns everything
after that: extraction, chunking, embedding, storage, scheduling and cursor persistence. Connectors
never touch the store.

Connectors are **native Rust crates** compiled into the daemon, or **WebAssembly plugins** loaded by
`nugget-plugin` (below).

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

Document every key in [Connector settings](../reference/connectors.md). The docs check
(`docs/book/check-refs.py`) compares that page with your settings struct: add the struct to its
`SOURCES`, the file with your `KindInfo` to `CONNECTOR_FILES`, and mark the tables with
`<!-- check-refs: ... -->` (group names use `_` for `-`). It reads every field of the struct, public
or not, and resolves the `kind` constant in the same file or in a file listed in `CONNECTOR_CONSTS`.

## Registering the kind

Expose a factory (or a `register` function for a crate with several kinds) and register it in
`nuggetd`'s registry (`crates/nuggetd/src/sources.rs`). On this base:

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

Wave W adds `nugget_connector_microsoft::register` and `nugget_connector_saas::register` here, and
one line for plugin connector kinds. Then add the kind to the desktop's add-source wizard if it needs a
custom form.

Three credential patterns exist; pick the one that fits:

- **`AuthKind::OAuth`** (`gmail`, `gcal`, `gdrive`, the Microsoft kinds): `begin_auth` runs the
  authorization-code flow with PKCE from `nugget-oauth` (a loopback listener on `127.0.0.1`) and stores
  the tokens under `oauth:<source>`. A client secret, if the provider needs one, is set with
  `sources.secret.set`; the daemon stores it under `oauth_client_secret:<source>` and injects it into
  the settings as `client_secret` at build time, so it never reaches `sources.toml`.
- **`AuthKind::Password`** (`imap`, `caldav`, the SaaS kinds): the user pastes a password or token,
  stored under `password:<source>`. A Password kind may still implement `begin_auth` for an optional
  interactive sign-in: `github` runs the device flow (RFC 8628) and `linear` the PKCE flow, both
  storing tokens under `oauth:<source>`.
- **`AuthKind::None`** (`files`, `obsidian`).

`AuthFlow` carries the URL to open. For the device flow the user code is appended to that URL; wave W
adds a `user_code` field so the app can show it separately.

## Testing

Follow the existing connectors (`nugget-connector-imap`, `nugget-connector-caldav`,
`nugget-connector-google`, `nugget-connector-microsoft`, `nugget-connector-saas`). The HTTP
connectors take injectable endpoints (`with_endpoints`, `with_base`) so tests point them at a mock
server:

- a fake server (wiremock, or a scripted protocol server) with **recorded fixtures**;
- tests for the first sync, an incremental sync, deletes, a reset cursor (reconciliation), an invalid
  sync token (full resync), cancellation, `Unauthorized` on bad credentials, and egress being recorded;
- malformed-input tests for every parser.

## Plugin connectors (WebAssembly)

> **Status: being wired in (wave W).** `nugget-plugin` is a working WebAssembly component host with a
> guest SDK (`crates/nugget-plugin/sdk`), but this build of the daemon loads no plugins yet.

A plugin connector is a WebAssembly component built against the SDK. `PluginConnectorFactory` wraps it
as an ordinary `ConnectorFactory` under the kind `plugin:<plugin id>`, so the ingest pipeline runs it
unchanged. The manifest's `[connector]` section gives its `display_name`, `auth` (`none` or
`password`; plugins have no interactive OAuth), `poll_interval_secs` (default 1800, at least 60) and
`settings_schema`.

A component has no ambient file, network or process access. It declares capabilities (HTTP `hosts`,
named `secrets`, a key-value store size); the user approves them on install, and the host denies
everything else. The host makes the HTTP requests, under the feature `sync:plugin:<plugin id>`, so they
land in the egress ledger like any other. Secrets live in the keychain under
`plugin:<plugin id>:<name>`.

The example in `crates/nugget-plugin/sdk/examples/feed-reader` is an RSS and Atom feed connector. For
installing and approving plugins, see [Plugins](../guide/plugins.md); a plugin author guide
(`docs/plugins.md` in the repository) is being written in wave W.
