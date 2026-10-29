//! The plugin host: wasmtime with the component model, resource limits and the capability-checked
//! implementations of the `nugget:plugin` imports.
//!
//! Every call into a plugin gets a fresh [`Store`] instantiated from a cached, pre-linked
//! [`PluginPre`], so no state leaks between calls and a trapped instance is simply dropped. Each
//! store has:
//! - **fuel** (a CPU budget per call; the guest also yields to the async runtime periodically),
//! - an **epoch deadline** (a wall-clock limit that interrupts even a tight loop),
//! - a **memory cap** through a [`ResourceLimiter`], counted across every linear memory of the
//!   instance (growth beyond it traps), with caps on table elements and on the number of
//!   instances, tables and memories,
//! - a `tokio` timeout around the whole call.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use nugget_connector::http::MeteredHttp;
use nugget_connector::{Change, ChangeSink, Content, Document, KnownItems, SecretStore};
use nugget_core::egress::EgressSink;
use nugget_core::ids::ItemKey;
use nugget_core::{Error, Result};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use url::Url;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, ResourceLimiter, Store, Trap};

use crate::kv::KvNamespace;
use crate::manifest::{Capabilities, HostRule, Manifest, PluginKind};
use crate::store::VerifiedPlugin;

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "plugin",
        imports: { default: async | trappable },
        exports: { default: async },
    });
}

use bindings::nugget::plugin::{clock, http, kv, log, secrets, sink, types};
use bindings::{Plugin, PluginPre};
use types::PluginError;

/// How often the epoch advances. Wall-clock deadlines are rounded up to this.
const EPOCH_TICK: Duration = Duration::from_millis(10);
/// Fuel consumed between voluntary yields to the async runtime.
const FUEL_YIELD_INTERVAL: u64 = 10_000_000;
/// Interfaces of the `nugget:plugin` package a component may import.
const HOST_INTERFACES: &[&str] = &["types", "http", "kv", "secrets", "log", "clock", "sink"];
/// Header names a plugin may not set.
const FORBIDDEN_HEADERS: &[&str] = &[
    "host",
    "connection",
    "content-length",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "keep-alive",
    "proxy-authorization",
    "proxy-authenticate",
    "proxy-connection",
];

/// Resource limits applied to every plugin call.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Cap on each linear memory of a plugin instance.
    pub memory_bytes: usize,
    /// Fuel (roughly, wasm instructions) for one tool call or `validate`.
    pub call_fuel: u64,
    /// Fuel for one connector sync pass.
    pub sync_fuel: u64,
    /// Wall-clock limit of one tool call (including its HTTP requests).
    pub call_timeout: Duration,
    /// Wall-clock limit of `validate`.
    pub validate_timeout: Duration,
    /// Wall-clock limit of one sync pass (including time waiting on the pipeline).
    pub sync_timeout: Duration,
    /// Largest HTTP response body a plugin may receive.
    pub max_http_response_bytes: usize,
    /// Largest HTTP request body a plugin may send.
    pub max_http_request_bytes: usize,
    pub max_http_requests_per_call: u32,
    pub max_http_requests_per_sync: u32,
    /// Largest document content a connector may emit.
    pub max_document_bytes: usize,
    /// Largest tool input or output (JSON).
    pub max_tool_io_bytes: usize,
    /// Log lines kept per call; the rest are dropped.
    pub max_log_lines: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            call_fuel: 5_000_000_000,
            sync_fuel: 200_000_000_000,
            call_timeout: Duration::from_secs(15),
            validate_timeout: Duration::from_secs(2),
            sync_timeout: Duration::from_secs(30 * 60),
            max_http_response_bytes: 16 * 1024 * 1024,
            max_http_request_bytes: 4 * 1024 * 1024,
            max_http_requests_per_call: 32,
            max_http_requests_per_sync: 10_000,
            max_document_bytes: 16 * 1024 * 1024,
            max_tool_io_bytes: 1024 * 1024,
            max_log_lines: 1000,
        }
    }
}

/// Host-wide configuration.
#[derive(Clone)]
pub struct HostConfig {
    pub limits: Limits,
    /// Root of the plugins' key-value state ([`crate::PluginStore::state_root`]).
    pub state_dir: PathBuf,
    /// Egress ledger for tool calls (connectors use their source's ledger).
    pub egress: Arc<dyn EgressSink>,
    /// Keychain for tool calls (connectors use their source's store). `None` denies secrets to
    /// tools.
    pub secrets: Option<Arc<dyn SecretStore>>,
    /// HTTP client override (tests). It must not follow redirects. Without
    /// `allow_private_network` the destination check before each request still applies, but the
    /// connection is not pinned to the checked addresses (the override has its own resolver).
    pub http_client: Option<reqwest::Client>,
    /// Let plugins reach hosts on loopback, private, link-local and other non-public addresses
    /// (tests against a local server). `false` in production: a request to such an IP literal,
    /// or to a name that resolves to ANY such address, is denied, and the host's HTTP client
    /// connects only to the addresses that were checked (no DNS rebinding in between).
    pub allow_private_network: bool,
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("limits", &self.limits)
            .field("state_dir", &self.state_dir)
            .field("allow_private_network", &self.allow_private_network)
            .finish_non_exhaustive()
    }
}

/// The plugin runtime. Cheap to clone.
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<HostInner>,
}

struct HostInner {
    engine: Engine,
    limits: Arc<Limits>,
    state_dir: PathBuf,
    egress: Arc<dyn EgressSink>,
    secrets: Option<Arc<dyn SecretStore>>,
    client: reqwest::Client,
    /// Public-network-only guard; `None` when private networks are allowed.
    net_guard: Option<NetGuard>,
    /// Compiled components by SHA-256, shared by every load of the same bytes.
    compiled: Mutex<HashMap<String, Component>>,
    kv: Mutex<HashMap<PathBuf, Weak<KvNamespace>>>,
    ticker: Arc<EpochTicker>,
}

/// Advances the engine's epoch every [`EPOCH_TICK`] while at least one plugin call is running,
/// and parks otherwise, so an idle host costs no wakeups. Deadlines are relative to the epoch
/// when a call starts, so pausing between calls does not change them.
#[derive(Default)]
struct EpochTicker {
    state: Mutex<TickerState>,
    cv: Condvar,
}

#[derive(Default)]
struct TickerState {
    active: usize,
    stop: bool,
}

/// One running call; the ticker runs while any exists.
struct Ticking<'a>(&'a EpochTicker);

impl Drop for Ticking<'_> {
    fn drop(&mut self) {
        let mut st = self.0.lock();
        st.active = st.active.saturating_sub(1);
    }
}

impl EpochTicker {
    fn lock(&self) -> std::sync::MutexGuard<'_, TickerState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn begin(&self) -> Ticking<'_> {
        self.lock().active += 1;
        self.cv.notify_all();
        Ticking(self)
    }

    fn stop(&self) {
        self.lock().stop = true;
        self.cv.notify_all();
    }

    /// The ticker thread's loop; returns once stopped.
    fn run(&self, engine: &Engine) {
        let mut st = self.lock();
        loop {
            if st.stop {
                return;
            }
            if st.active == 0 {
                st = self.cv.wait(st).unwrap_or_else(|p| p.into_inner());
                continue;
            }
            drop(st);
            std::thread::sleep(EPOCH_TICK);
            engine.increment_epoch();
            st = self.lock();
        }
    }
}

impl Drop for HostInner {
    fn drop(&mut self) {
        self.ticker.stop();
    }
}

impl std::fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginHost")
            .field("limits", &self.inner.limits)
            .finish_non_exhaustive()
    }
}

impl PluginHost {
    pub fn new(config: HostConfig) -> Result<Self> {
        let mut cfg = Config::new();
        cfg.async_support(true)
            .wasm_component_model(true)
            .consume_fuel(true)
            .epoch_interruption(true)
            .max_wasm_stack(1024 * 1024);
        let engine = Engine::new(&cfg).map_err(|e| Error::Other(format!("wasmtime: {e:#}")))?;
        let ticker = Arc::new(EpochTicker::default());
        {
            let engine = engine.clone();
            let ticker = ticker.clone();
            std::thread::Builder::new()
                .name("nugget-plugin-epoch".into())
                .spawn(move || ticker.run(&engine))?;
        }
        let net_guard = (!config.allow_private_network).then(NetGuard::default);
        let client = match config.http_client {
            Some(c) => c,
            None => {
                let mut builder = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::from_secs(15))
                    .timeout(Duration::from_secs(120))
                    .user_agent(concat!("nugget-plugin/", env!("CARGO_PKG_VERSION")));
                if let Some(guard) = &net_guard {
                    builder = builder.dns_resolver(guard.clone());
                }
                builder
                    .build()
                    .map_err(|e| Error::Other(format!("http client: {e}")))?
            }
        };
        Ok(Self {
            inner: Arc::new(HostInner {
                engine,
                limits: Arc::new(config.limits),
                state_dir: config.state_dir,
                egress: config.egress,
                secrets: config.secrets,
                client,
                net_guard,
                compiled: Mutex::new(HashMap::new()),
                kv: Mutex::new(HashMap::new()),
                ticker,
            }),
        })
    }

    pub fn limits(&self) -> &Limits {
        &self.inner.limits
    }

    /// Compile (or reuse) and link a verified, installed plugin. The host enforces the
    /// capabilities the user approved, narrowed to what the manifest still declares.
    pub fn load(&self, plugin: &VerifiedPlugin) -> Result<Arc<LoadedPlugin>> {
        let installed = &plugin.installed;
        let component = self.compile(&installed.wasm_sha256, &plugin.wasm)?;
        check_imports(&self.inner.engine, &component)?;
        let mut linker: Linker<HostState> = Linker::new(&self.inner.engine);
        Plugin::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |s| s)
            .map_err(link_err)?;
        // WASI imports a toolchain adds (Rust std's environment, stdio, clocks, random, ...)
        // are linked to functions that trap: plugins get no ambient authority.
        stub_wasi_imports(&self.inner.engine, &component, &mut linker).map_err(link_err)?;
        let pre = linker.instantiate_pre(&component).map_err(link_err)?;
        let pre = PluginPre::new(pre).map_err(link_err)?;
        let caps = Caps::new(&installed.manifest, &installed.approved)?;
        Ok(Arc::new(LoadedPlugin {
            host: self.inner.clone(),
            manifest: installed.manifest.clone(),
            caps: Arc::new(caps),
            pre,
            tools: tokio::sync::OnceCell::new(),
        }))
    }

    fn compile(&self, sha: &str, wasm: &[u8]) -> Result<Component> {
        let mut cache = self
            .inner
            .compiled
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cache.get(sha) {
            return Ok(c.clone());
        }
        let component = Component::new(&self.inner.engine, wasm)
            .map_err(|e| Error::Invalid(format!("plugin component does not compile: {e:#}")))?;
        cache.insert(sha.to_string(), component.clone());
        Ok(component)
    }
}

fn link_err(e: wasmtime::Error) -> Error {
    Error::Invalid(format!(
        "plugin does not match the nugget:plugin world: {e:#}"
    ))
}

/// Refuse components that import anything other than `nugget:plugin` interfaces (which the
/// linker provides) and `wasi:` interfaces (which trap).
fn check_imports(engine: &Engine, component: &Component) -> Result<()> {
    for (name, _) in component.component_type().imports(engine) {
        if let Some(rest) = name.strip_prefix("nugget:plugin/") {
            let (iface, version) = rest.split_once('@').unwrap_or((rest, ""));
            if !HOST_INTERFACES.contains(&iface) || !version.starts_with("0.1.") {
                return Err(Error::Invalid(format!(
                    "plugin imports `{name}`, which this host does not provide"
                )));
            }
        } else if !name.starts_with("wasi:") {
            return Err(Error::Invalid(format!(
                "plugin imports `{name}`, which this host does not provide"
            )));
        }
    }
    Ok(())
}

/// Link every `wasi:` function a component imports to a function that traps, and every `wasi:`
/// resource type to an inert host type. ([`Linker::define_unknown_imports_as_traps`] cannot be
/// used: it also visits the type-only `nugget:plugin/types` instance the bindings define.)
fn stub_wasi_imports(
    engine: &Engine,
    component: &Component,
    linker: &mut Linker<HostState>,
) -> wasmtime::Result<()> {
    use wasmtime::component::types::ComponentItem;
    use wasmtime::component::{ResourceType, Val};
    for (name, item) in component.component_type().imports(engine) {
        if !name.starts_with("wasi:") {
            continue;
        }
        let ComponentItem::ComponentInstance(instance) = item else {
            continue;
        };
        let mut inst = linker.instance(name)?;
        for (export, ty) in instance.exports(engine) {
            match ty {
                ComponentItem::ComponentFunc(_) => {
                    let what = format!("{name}#{export}");
                    inst.func_new(export, move |_, _: &[Val], _: &mut [Val]| {
                        Err(wasmtime::Error::msg(format!(
                            "unknown import `{what}`: plugins have no WASI access"
                        )))
                    })?;
                }
                ComponentItem::Resource(_) => {
                    inst.resource(export, ResourceType::host::<()>(), |_, _| Ok(()))?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// The effective, enforced capabilities of one plugin.
#[derive(Debug)]
struct Caps {
    id: String,
    hosts: Vec<HostRule>,
    secrets: Vec<String>,
    kv_max_bytes: u64,
}

impl Caps {
    fn new(manifest: &Manifest, approved: &Capabilities) -> Result<Self> {
        let declared = manifest.capabilities.host_rules()?;
        let hosts = approved
            .host_rules()?
            .into_iter()
            .filter(|r| declared.contains(r))
            .collect();
        let secrets = approved
            .secrets
            .iter()
            .filter(|s| manifest.capabilities.secrets.contains(s))
            .cloned()
            .collect();
        Ok(Self {
            id: manifest.id.clone(),
            hosts,
            secrets,
            kv_max_bytes: approved
                .kv_max_bytes
                .min(manifest.capabilities.kv_max_bytes),
        })
    }
}

/// A tool a plugin offers to the agent.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the input object.
    pub input_schema: Value,
}

/// A compiled, linked plugin, ready to run.
pub struct LoadedPlugin {
    host: Arc<HostInner>,
    manifest: Manifest,
    caps: Arc<Caps>,
    pre: PluginPre<HostState>,
    tools: tokio::sync::OnceCell<Vec<ToolSpec>>,
}

impl std::fmt::Debug for LoadedPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedPlugin")
            .field("id", &self.manifest.id)
            .field("version", &self.manifest.version)
            .finish_non_exhaustive()
    }
}

/// What a call may do, beyond the declared capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `connector.validate`: no network, no secrets, no key-value store, no sink.
    Validate,
    /// `connector.sync`: everything declared.
    Sync,
    /// `tools.*`: read-only (GET/HEAD, key-value reads, secret reads), no sink.
    Tool,
}

impl LoadedPlugin {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// The tools this plugin offers (asked once, then cached).
    pub async fn tools(&self) -> Result<Vec<ToolSpec>> {
        self.require(PluginKind::Tool)?;
        self.tools
            .get_or_try_init(|| async {
                let state = self.state(Mode::Tool, None, None, None)?;
                let limits = self.host.limits.clone();
                let specs = self
                    .run(state, limits.call_fuel, limits.call_timeout, |p, s| {
                        Box::pin(async move { p.nugget_plugin_tools().call_list_tools(s).await })
                    })
                    .await?;
                validate_tool_specs(specs)
            })
            .await
            .cloned()
    }

    /// Call one tool with a JSON input and return its JSON output.
    pub async fn call_tool(&self, name: &str, input: &Value) -> Result<Value> {
        if !self.tools().await?.iter().any(|t| t.name == name) {
            return Err(Error::NotFound(format!(
                "plugin `{}` has no tool `{name}`",
                self.id()
            )));
        }
        let limits = self.host.limits.clone();
        let input = serde_json::to_string(input)?;
        if input.len() > limits.max_tool_io_bytes {
            return Err(Error::Invalid("tool input is too large".into()));
        }
        let state = self.state(Mode::Tool, None, None, None)?;
        let name = name.to_string();
        let out = self
            .run(state, limits.call_fuel, limits.call_timeout, |p, s| {
                Box::pin(async move { p.nugget_plugin_tools().call_call(s, &name, &input).await })
            })
            .await?
            .map_err(|e| from_guest(self.id(), e))?;
        if out.len() > limits.max_tool_io_bytes {
            return Err(Error::Invalid(format!(
                "plugin `{}` returned more than {} bytes",
                self.id(),
                limits.max_tool_io_bytes
            )));
        }
        serde_json::from_str(&out).map_err(|e| {
            Error::Invalid(format!("plugin `{}` returned invalid JSON: {e}", self.id()))
        })
    }

    /// `connector.validate`, run to completion on the calling thread.
    pub(crate) fn validate_settings(&self, settings: &Value) -> Result<()> {
        self.require(PluginKind::Connector)?;
        let settings = serde_json::to_string(settings)?;
        let limits = self.host.limits.clone();
        let state = self.state(Mode::Validate, None, None, None)?;
        // `validate` cannot reach the network or the pipeline, so it needs no async runtime; the
        // epoch deadline bounds it.
        futures::executor::block_on(self.run_untimed(
            state,
            limits.call_fuel,
            limits.validate_timeout,
            |p, s| {
                Box::pin(async move {
                    p.nugget_plugin_connector()
                        .call_validate(s, &settings)
                        .await
                })
            },
        ))?
        .map_err(|e| from_guest(self.id(), e))
    }

    /// One `connector.sync` pass, forwarding the plugin's changes to `sink`.
    pub(crate) async fn sync(
        &self,
        settings: &str,
        cursor: Option<Value>,
        ctx: &nugget_connector::ConnectorContext,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.require(PluginKind::Connector)?;
        let limits = self.host.limits.clone();
        let (tx, mut rx) = mpsc::channel::<SinkOp>(1);
        let http = MeteredHttp::with_client(
            self.host.client.clone(),
            ctx.egress.clone(),
            format!("sync:plugin:{}", self.id()),
        )
        .with_max_body(limits.max_http_response_bytes);
        let kv_ns = format!("source-{}", hex::encode(ctx.source.0.as_bytes()));
        let mut state = self.state(
            Mode::Sync,
            Some(http),
            Some(&kv_ns),
            Some(ctx.secrets.clone()),
        )?;
        state.sink = Some(SinkBridge {
            tx,
            known: ctx.known.clone(),
        });
        state.cancel = cancel.clone();
        let cursor = cursor.map(|c| c.to_string());
        let settings = settings.to_string();

        let wasm = self.run_untimed(state, limits.sync_fuel, limits.sync_timeout, |p, s| {
            Box::pin(async move {
                p.nugget_plugin_connector()
                    .call_sync(s, &settings, cursor.as_deref())
                    .await
            })
        });
        let driver = async {
            while let Some(op) = rx.recv().await {
                match op {
                    SinkOp::Change(change, reply) => {
                        let _ = reply.send(sink.send(change).await);
                    }
                    SinkOp::Checkpoint(cursor, reply) => {
                        let _ = reply.send(sink.checkpoint(cursor).await);
                    }
                }
            }
        };
        let both = async { tokio::join!(wasm, driver).0 };
        let out = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            r = tokio::time::timeout(limits.sync_timeout, both) => r.map_err(|_| self.timed_out())??,
        };
        let cursor = out.map_err(|e| from_guest(self.id(), e))?;
        serde_json::from_str(&cursor).map_err(|e| {
            Error::Invalid(format!(
                "plugin `{}` returned a cursor that is not JSON: {e}",
                self.id()
            ))
        })
    }

    fn require(&self, kind: PluginKind) -> Result<()> {
        if self.manifest.provides(kind) {
            Ok(())
        } else {
            Err(Error::Unsupported(format!(
                "plugin `{}` does not provide a {kind:?}",
                self.id()
            )))
        }
    }

    fn state(
        &self,
        mode: Mode,
        http: Option<MeteredHttp>,
        kv_namespace: Option<&str>,
        secrets: Option<Arc<dyn SecretStore>>,
    ) -> Result<HostState> {
        let limits = self.host.limits.clone();
        let (http, kv, secrets) = match mode {
            Mode::Validate => (None, None, None),
            Mode::Sync => (http, self.kv(kv_namespace.unwrap_or("source")), secrets),
            Mode::Tool => {
                let http = MeteredHttp::with_client(
                    self.host.client.clone(),
                    self.host.egress.clone(),
                    format!("tool:plugin:{}", self.id()),
                )
                .with_max_body(limits.max_http_response_bytes);
                (Some(http), self.kv("tools"), self.host.secrets.clone())
            }
        };
        let http_budget = match mode {
            Mode::Sync => limits.max_http_requests_per_sync,
            _ => limits.max_http_requests_per_call,
        };
        Ok(HostState {
            caps: self.caps.clone(),
            limiter: Limiter::new(limits.memory_bytes),
            limits,
            mode,
            http,
            http_budget,
            net_guard: self.host.net_guard.clone(),
            kv,
            secrets,
            sink: None,
            cancel: CancellationToken::new(),
            log_lines: 0,
        })
    }

    /// The key-value namespace `name` of this plugin, if it declared a store.
    fn kv(&self, name: &str) -> Option<Arc<KvNamespace>> {
        if self.caps.kv_max_bytes == 0 {
            return None;
        }
        let path = self
            .host
            .state_dir
            .join(&self.caps.id)
            .join(format!("{name}.json"));
        let mut map = self.host.kv.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, w| w.strong_count() > 0);
        if let Some(ns) = map.get(&path).and_then(Weak::upgrade) {
            return Some(ns);
        }
        let ns = Arc::new(KvNamespace::new(path.clone(), self.caps.kv_max_bytes));
        map.insert(path, Arc::downgrade(&ns));
        Some(ns)
    }

    /// Run with a `tokio` timeout as well as the epoch deadline.
    async fn run<R: Send>(
        &self,
        state: HostState,
        fuel: u64,
        timeout: Duration,
        f: impl for<'a> FnOnce(&'a Plugin, &'a mut Store<HostState>) -> CallFuture<'a, R> + Send,
    ) -> Result<R> {
        tokio::time::timeout(timeout, self.run_untimed(state, fuel, timeout, f))
            .await
            .map_err(|_| self.timed_out())?
    }

    /// Instantiate a fresh store and run `f` in it. `timeout` sets the epoch deadline only.
    async fn run_untimed<R: Send>(
        &self,
        state: HostState,
        fuel: u64,
        timeout: Duration,
        f: impl for<'a> FnOnce(&'a Plugin, &'a mut Store<HostState>) -> CallFuture<'a, R> + Send,
    ) -> Result<R> {
        let mut store = Store::new(&self.host.engine, state);
        store.limiter(|s| &mut s.limiter);
        let setup = (|| {
            store.set_fuel(fuel)?;
            store.fuel_async_yield_interval(Some(FUEL_YIELD_INTERVAL))?;
            wasmtime::Result::<()>::Ok(())
        })();
        setup.map_err(|e| Error::Other(format!("wasmtime: {e:#}")))?;
        // The epoch only advances while some call is running (see `EpochTicker`).
        let _ticking = self.host.ticker.begin();
        let ticks = (timeout.as_millis() / EPOCH_TICK.as_millis()).max(1) as u64 + 1;
        store.set_epoch_deadline(ticks);
        store.epoch_deadline_trap();
        let result = match self.pre.instantiate_async(&mut store).await {
            Ok(plugin) => f(&plugin, &mut store).await,
            Err(e) => Err(e),
        };
        // Close the sink bridge so a sync driver loop ends.
        store.data_mut().sink = None;
        result.map_err(|e| self.trap_error(e, store.data().limiter.memory_exceeded))
    }

    fn trap_error(&self, e: wasmtime::Error, memory_exceeded: bool) -> Error {
        let id = self.id();
        if memory_exceeded {
            return Error::Other(format!(
                "plugin `{id}` exceeded its memory limit of {} bytes",
                self.host.limits.memory_bytes
            ));
        }
        match e.downcast_ref::<Trap>() {
            Some(Trap::OutOfFuel) => Error::Other(format!(
                "plugin `{id}` exceeded its CPU budget (out of fuel)"
            )),
            Some(Trap::Interrupt) => self.timed_out(),
            _ => Error::Other(format!("plugin `{id}` failed: {e:#}")),
        }
    }

    fn timed_out(&self) -> Error {
        Error::Other(format!("plugin `{}` exceeded its time limit", self.id()))
    }
}

type CallFuture<'a, R> =
    std::pin::Pin<Box<dyn std::future::Future<Output = wasmtime::Result<R>> + Send + 'a>>;

fn validate_tool_specs(specs: Vec<types_tools::ToolSpec>) -> Result<Vec<ToolSpec>> {
    if specs.len() > 64 {
        return Err(Error::Invalid("a plugin may offer at most 64 tools".into()));
    }
    let mut out: Vec<ToolSpec> = Vec::with_capacity(specs.len());
    for s in specs {
        let name_ok = !s.name.is_empty()
            && s.name.len() <= 64
            && s.name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !name_ok || out.iter().any(|t| t.name == s.name) {
            return Err(Error::Invalid(format!(
                "bad or duplicate tool name `{}`",
                s.name
            )));
        }
        let input_schema: Value = serde_json::from_str(&s.input_schema)
            .map_err(|e| Error::Invalid(format!("tool `{}`: input schema: {e}", s.name)))?;
        if !input_schema.is_object() || s.description.len() > 4096 {
            return Err(Error::Invalid(format!("tool `{}`: bad spec", s.name)));
        }
        out.push(ToolSpec {
            name: s.name,
            description: s.description,
            input_schema,
        });
    }
    Ok(out)
}

use bindings::exports::nugget::plugin::tools as types_tools;

// ---------------------------------------------------------------------------------------------
// Store state and the host side of the imports.

enum SinkOp {
    Change(Change, oneshot::Sender<Result<()>>),
    Checkpoint(Value, oneshot::Sender<Result<()>>),
}

struct SinkBridge {
    tx: mpsc::Sender<SinkOp>,
    known: Arc<dyn KnownItems>,
}

/// Most table elements one plugin store may hold, across all its tables.
const MAX_TABLE_ELEMENTS: usize = 100_000;
/// Most instances, tables and linear memories in one plugin store.
const MAX_INSTANCES: usize = 64;
const MAX_TABLES: usize = 64;
const MAX_MEMORIES: usize = 16;

/// Per-store resource limits. Memory and table growth are summed over every memory and table of
/// the store (one store per call), so many small memories cannot add up past
/// [`Limits::memory_bytes`].
pub(crate) struct Limiter {
    memory_bytes: usize,
    memory_used: usize,
    table_elements: usize,
    pub(crate) memory_exceeded: bool,
}

impl Limiter {
    pub(crate) fn new(memory_bytes: usize) -> Self {
        Self {
            memory_bytes,
            memory_used: 0,
            table_elements: 0,
            memory_exceeded: false,
        }
    }
}

impl ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let total = desired
            .checked_sub(current)
            .and_then(|delta| self.memory_used.checked_add(delta));
        match total {
            Some(total) if total <= self.memory_bytes => {
                self.memory_used = total;
                Ok(true)
            }
            _ => {
                self.memory_exceeded = true;
                Err(wasmtime::Error::msg("plugin memory limit exceeded"))
            }
        }
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let total = desired
            .checked_sub(current)
            .and_then(|delta| self.table_elements.checked_add(delta));
        match total {
            Some(total) if total <= MAX_TABLE_ELEMENTS => {
                self.table_elements = total;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn instances(&self) -> usize {
        MAX_INSTANCES
    }

    fn tables(&self) -> usize {
        MAX_TABLES
    }

    fn memories(&self) -> usize {
        MAX_MEMORIES
    }
}

/// Per-call store data.
pub(crate) struct HostState {
    caps: Arc<Caps>,
    limits: Arc<Limits>,
    limiter: Limiter,
    mode: Mode,
    http: Option<MeteredHttp>,
    http_budget: u32,
    net_guard: Option<NetGuard>,
    kv: Option<Arc<KvNamespace>>,
    secrets: Option<Arc<dyn SecretStore>>,
    sink: Option<SinkBridge>,
    cancel: CancellationToken,
    log_lines: u32,
}

type HostResult<T> = wasmtime::Result<std::result::Result<T, PluginError>>;

fn denied(msg: impl Into<String>) -> PluginError {
    PluginError::Denied(msg.into())
}

impl HostState {
    fn deny(&self, msg: String) -> PluginError {
        tracing::warn!(plugin = %self.caps.id, "capability denied: {msg}");
        denied(msg)
    }

    fn bridge(&self) -> std::result::Result<&SinkBridge, PluginError> {
        if self.cancel.is_cancelled() {
            return Err(PluginError::Cancelled);
        }
        self.sink
            .as_ref()
            .ok_or_else(|| self.deny("the sink is only available during connector.sync".into()))
    }

    async fn forward(
        &self,
        op: impl FnOnce(oneshot::Sender<Result<()>>) -> SinkOp,
    ) -> HostResult<()> {
        let bridge = match self.bridge() {
            Ok(b) => b,
            Err(e) => return Ok(Err(e)),
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if bridge.tx.send(op(reply_tx)).await.is_err() {
            return Ok(Err(PluginError::Cancelled));
        }
        Ok(match reply_rx.await {
            Ok(r) => r.map_err(to_guest),
            Err(_) => Err(PluginError::Cancelled),
        })
    }

    fn check_request(
        &self,
        req: &http::Request,
    ) -> std::result::Result<(reqwest::Method, Url), PluginError> {
        let method = match req.method.to_ascii_uppercase().as_str() {
            "GET" => reqwest::Method::GET,
            "HEAD" => reqwest::Method::HEAD,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "PATCH" => reqwest::Method::PATCH,
            "DELETE" => reqwest::Method::DELETE,
            other => {
                return Err(PluginError::Invalid(format!(
                    "unsupported method `{other}`"
                )));
            }
        };
        if self.mode == Mode::Tool
            && method != reqwest::Method::GET
            && method != reqwest::Method::HEAD
        {
            return Err(self.deny(format!("tools are read-only: {method} is not allowed")));
        }
        let url = Url::parse(&req.url)
            .map_err(|e| PluginError::Invalid(format!("bad url `{}`: {e}", req.url)))?;
        if !self.caps.hosts.iter().any(|rule| rule.matches(&url)) {
            let host = url.host_str().unwrap_or("");
            return Err(self.deny(format!(
                "{}://{host}{} is not a host declared in the manifest",
                url.scheme(),
                url.port().map(|p| format!(":{p}")).unwrap_or_default()
            )));
        }
        if req.headers.len() > 64 {
            return Err(PluginError::Invalid("too many request headers".into()));
        }
        for (name, _) in &req.headers {
            let lower = name.to_ascii_lowercase();
            if FORBIDDEN_HEADERS.contains(&lower.as_str()) || lower.starts_with("proxy-") {
                return Err(self.deny(format!("header `{name}` may not be set by a plugin")));
            }
        }
        if req
            .body
            .as_ref()
            .is_some_and(|b| b.len() > self.limits.max_http_request_bytes)
        {
            return Err(PluginError::Invalid(format!(
                "request body exceeds {} bytes",
                self.limits.max_http_request_bytes
            )));
        }
        Ok((method, url))
    }
}

impl types::Host for HostState {}

impl http::Host for HostState {
    async fn send(&mut self, req: http::Request) -> HostResult<http::Response> {
        let Some(client) = self.http.clone() else {
            return Ok(Err(self.deny("no network access in this call".into())));
        };
        if self.cancel.is_cancelled() {
            return Ok(Err(PluginError::Cancelled));
        }
        if self.http_budget == 0 {
            return Ok(Err(
                self.deny("HTTP request budget for this call is spent".into())
            ));
        }
        let (method, url) = match self.check_request(&req) {
            Ok(v) => v,
            Err(e) => return Ok(Err(e)),
        };
        if let Some(guard) = &self.net_guard {
            // Like an undeclared host, a non-public destination is denied before anything is
            // sent (so there is no ledger row).
            match guard.check(&url).await {
                Ok(()) => {}
                Err(NetRefusal::Blocked(msg)) => return Ok(Err(self.deny(msg))),
                Err(NetRefusal::Unresolved(msg)) => return Ok(Err(PluginError::Network(msg))),
            }
        }
        self.http_budget -= 1;
        let mut builder = client.request(method, url.as_str());
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        let resp = match client.send(builder, &[]).await {
            Ok(r) => r,
            Err(e) => return Ok(Err(to_guest(e))),
        };
        let headers = resp
            .headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        Ok(Ok(http::Response {
            status: resp.status.as_u16(),
            headers,
            body: resp.body,
        }))
    }
}

impl kv::Host for HostState {
    async fn get(&mut self, key: String) -> HostResult<Option<Vec<u8>>> {
        Ok(self.kv_ns().and_then(|ns| ns.get(&key).map_err(to_guest)))
    }

    async fn set(&mut self, key: String, value: Vec<u8>) -> HostResult<()> {
        Ok(self
            .kv_ns_mut()
            .and_then(|ns| ns.set(&key, &value).map_err(to_guest)))
    }

    async fn delete(&mut self, key: String) -> HostResult<()> {
        Ok(self
            .kv_ns_mut()
            .and_then(|ns| ns.delete(&key).map_err(to_guest)))
    }

    async fn keys(&mut self, prefix: String) -> HostResult<Vec<String>> {
        Ok(self
            .kv_ns()
            .and_then(|ns| ns.keys(&prefix).map_err(to_guest)))
    }
}

impl HostState {
    fn kv_ns(&self) -> std::result::Result<Arc<KvNamespace>, PluginError> {
        self.kv.clone().ok_or_else(|| {
            self.deny(
                "no key-value store (kv_max_bytes is 0, or not available in this call)".into(),
            )
        })
    }

    fn kv_ns_mut(&self) -> std::result::Result<Arc<KvNamespace>, PluginError> {
        if self.mode != Mode::Sync {
            return Err(self.deny("the key-value store is read-only outside a sync".into()));
        }
        self.kv_ns()
    }

    fn secret_account(
        &self,
        name: &str,
    ) -> std::result::Result<(Arc<dyn SecretStore>, String), PluginError> {
        if !self.caps.secrets.iter().any(|s| s == name) {
            return Err(self.deny(format!("secret `{name}` is not declared in the manifest")));
        }
        let store = self
            .secrets
            .clone()
            .ok_or_else(|| self.deny("no keychain access in this call".into()))?;
        Ok((store, secret_account(&self.caps.id, name)))
    }
}

/// The keychain account under which a plugin's secret `name` is stored.
pub fn secret_account(plugin_id: &str, name: &str) -> String {
    format!("plugin:{plugin_id}:{name}")
}

impl secrets::Host for HostState {
    async fn get(&mut self, name: String) -> HostResult<Option<String>> {
        Ok(self
            .secret_account(&name)
            .and_then(|(store, account)| store.get(&account).map_err(to_guest)))
    }

    async fn set(&mut self, name: String, value: String) -> HostResult<()> {
        if self.mode != Mode::Sync {
            return Ok(Err(self.deny("secrets are read-only outside a sync".into())));
        }
        if value.len() > 64 * 1024 {
            return Ok(Err(PluginError::Invalid("secret is too large".into())));
        }
        Ok(self
            .secret_account(&name)
            .and_then(|(store, account)| store.set(&account, &value).map_err(to_guest)))
    }
}

impl log::Host for HostState {
    async fn log(&mut self, level: log::Level, message: String) -> wasmtime::Result<()> {
        self.log_lines += 1;
        if self.log_lines > self.limits.max_log_lines {
            if self.log_lines == self.limits.max_log_lines + 1 {
                tracing::warn!(plugin = %self.caps.id, "log limit reached; dropping further lines");
            }
            return Ok(());
        }
        let mut msg = message;
        if msg.len() > 2048 {
            let mut end = 2048;
            while !msg.is_char_boundary(end) {
                end -= 1;
            }
            msg.truncate(end);
            msg.push('…');
        }
        let id = &self.caps.id;
        match level {
            log::Level::Trace => tracing::trace!(plugin = %id, "{msg}"),
            log::Level::Debug => tracing::debug!(plugin = %id, "{msg}"),
            log::Level::Info => tracing::info!(plugin = %id, "{msg}"),
            log::Level::Warn => tracing::warn!(plugin = %id, "{msg}"),
            log::Level::Error => tracing::error!(plugin = %id, "{msg}"),
        }
        Ok(())
    }
}

impl clock::Host for HostState {
    async fn now(&mut self) -> wasmtime::Result<i64> {
        let now = OffsetDateTime::now_utc();
        Ok((now.unix_timestamp_nanos() / 1_000_000) as i64)
    }
}

impl sink::Host for HostState {
    async fn upsert(&mut self, doc: types::Document) -> HostResult<()> {
        let doc = match convert_document(doc, &self.limits) {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        self.forward(|reply| SinkOp::Change(Change::Upsert(doc), reply))
            .await
    }

    async fn delete(&mut self, key: String) -> HostResult<()> {
        if key.is_empty() || key.len() > 1024 {
            return Ok(Err(PluginError::Invalid(
                "keys must be 1-1024 bytes".into(),
            )));
        }
        self.forward(|reply| SinkOp::Change(Change::Delete(ItemKey::new(key)), reply))
            .await
    }

    async fn checkpoint(&mut self, cursor: String) -> HostResult<()> {
        let cursor: Value = match serde_json::from_str(&cursor) {
            Ok(v) => v,
            Err(e) => {
                return Ok(Err(PluginError::Invalid(format!(
                    "cursor is not JSON: {e}"
                ))));
            }
        };
        self.forward(|reply| SinkOp::Checkpoint(cursor, reply))
            .await
    }

    async fn known_items(&mut self) -> HostResult<Vec<sink::KnownItem>> {
        let known = match self.bridge() {
            Ok(b) => b.known.clone(),
            Err(e) => return Ok(Err(e)),
        };
        Ok(known
            .versions()
            .await
            .map(|m| {
                m.into_iter()
                    .map(|(key, version)| sink::KnownItem {
                        key: key.0,
                        version,
                    })
                    .collect()
            })
            .map_err(to_guest))
    }

    async fn cancelled(&mut self) -> wasmtime::Result<bool> {
        Ok(self.cancel.is_cancelled())
    }
}

fn convert_document(
    doc: types::Document,
    limits: &Limits,
) -> std::result::Result<Document, PluginError> {
    let bad = |m: &str| Err(PluginError::Invalid(m.to_string()));
    if doc.key.is_empty() || doc.key.len() > 1024 {
        return bad("document keys must be 1-1024 bytes");
    }
    if doc.mime.is_empty() || doc.mime.len() > 255 {
        return bad("document mime must be 1-255 bytes");
    }
    let content = match doc.content {
        types::Content::Text(t) => Content::Text(t),
        types::Content::Bytes(b) => Content::Bytes(b),
    };
    let size = match &content {
        Content::Text(t) => t.len(),
        Content::Bytes(b) => b.len(),
        Content::File(_) => 0,
    };
    if size > limits.max_document_bytes {
        return Err(PluginError::Invalid(format!(
            "document content exceeds {} bytes",
            limits.max_document_bytes
        )));
    }
    let metadata = if doc.metadata.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        match serde_json::from_str::<Value>(&doc.metadata) {
            Ok(v @ Value::Object(_)) => v,
            _ => return bad("document metadata must be a JSON object"),
        }
    };
    let modified_at = match doc.modified_at {
        Some(ms) => match OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000) {
            Ok(t) => Some(t),
            Err(_) => return bad("modified-at is out of range"),
        },
        None => None,
    };
    let mut out = Document::new(doc.key, doc.mime, content);
    out.title = doc.title;
    out.version = doc.version;
    out.modified_at = modified_at;
    out.url = doc.url;
    out.metadata = metadata;
    Ok(out)
}

fn to_guest(e: Error) -> PluginError {
    match e {
        Error::Invalid(m) => PluginError::Invalid(m),
        Error::Unauthorized(m) => PluginError::Unauthorized(m),
        Error::Network(m) => PluginError::Network(m),
        Error::RateLimited { retry_after_ms } => PluginError::RateLimited(retry_after_ms),
        Error::NotFound(m) => PluginError::NotFound(m),
        Error::Cancelled => PluginError::Cancelled,
        other => PluginError::Other(other.to_string()),
    }
}

fn from_guest(id: &str, e: PluginError) -> Error {
    match e {
        PluginError::Invalid(m) => Error::Invalid(m),
        PluginError::Unauthorized(m) => Error::Unauthorized(m),
        PluginError::Network(m) => Error::Network(m),
        PluginError::RateLimited(retry_after_ms) => Error::RateLimited { retry_after_ms },
        PluginError::NotFound(m) => Error::NotFound(m),
        PluginError::Denied(m) => Error::Other(format!("plugin `{id}`: capability denied: {m}")),
        PluginError::Cancelled => Error::Cancelled,
        PluginError::Other(m) => Error::Other(format!("plugin `{id}`: {m}")),
    }
}

// ---------------------------------------------------------------------------------------------
// Public-network-only guard: plugins may not reach loopback, private or other non-public
// addresses (the user's router, local services, cloud metadata endpoints, ...).

/// How long a destination check pins the addresses it checked for the connection that follows.
const PIN_TTL: Duration = Duration::from_secs(60);
/// Bound on remembered pins; expired ones are dropped first.
const MAX_PINS: usize = 1024;

/// Checks request destinations and, installed as the host client's DNS resolver, makes the
/// connection use exactly the addresses that were checked. A name that is resolved without a
/// prior check (for example a proxy given by name) is resolved and checked the same way, so a
/// proxy must be configured by a public name or by IP address.
/// When a name was checked, and the addresses it resolved to.
type Pin = (Instant, Vec<IpAddr>);

#[derive(Clone, Default)]
pub(crate) struct NetGuard {
    pins: Arc<Mutex<HashMap<String, Pin>>>,
}

#[derive(Debug)]
pub(crate) enum NetRefusal {
    /// The destination is not on the public internet (a capability denial).
    Blocked(String),
    /// The name did not resolve (a network error).
    Unresolved(String),
}

impl std::fmt::Display for NetRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetRefusal::Blocked(m) | NetRefusal::Unresolved(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for NetRefusal {}

impl NetGuard {
    /// Check the URL's destination: an IP literal directly, a name by resolving it (every
    /// address must be public). The checked addresses are pinned for the connection.
    pub(crate) async fn check(&self, url: &Url) -> std::result::Result<(), NetRefusal> {
        match url.host() {
            Some(url::Host::Ipv4(ip)) => check_ip(&ip.to_string(), IpAddr::V4(ip)),
            Some(url::Host::Ipv6(ip)) => check_ip(&format!("[{ip}]"), IpAddr::V6(ip)),
            Some(url::Host::Domain(name)) => {
                let ips = resolve_checked(name).await?;
                self.pin(name, ips);
                Ok(())
            }
            None => Err(NetRefusal::Blocked(format!("`{url}` has no host"))),
        }
    }

    fn pin(&self, name: &str, ips: Vec<IpAddr>) {
        let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if pins.len() >= MAX_PINS {
            pins.retain(|_, (at, _)| now.duration_since(*at) < PIN_TTL);
            if pins.len() >= MAX_PINS {
                // Only fresh pins left: the oldest goes (its connection, if any, has started).
                if let Some(oldest) = pins
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(k, _)| k.clone())
                {
                    pins.remove(&oldest);
                }
            }
        }
        pins.insert(name.to_string(), (now, ips));
    }

    pub(crate) fn pinned(&self, name: &str) -> Option<Vec<IpAddr>> {
        let pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
        pins.get(name)
            .filter(|(at, _)| at.elapsed() < PIN_TTL)
            .map(|(_, ips)| ips.clone())
    }
}

impl reqwest::dns::Resolve for NetGuard {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let guard = self.clone();
        let name = name.as_str().to_string();
        Box::pin(async move {
            let ips = match guard.pinned(&name) {
                Some(ips) => ips,
                None => resolve_checked(&name).await?,
            };
            // The connector sets the port.
            let addrs: reqwest::dns::Addrs =
                Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

fn check_ip(shown: &str, ip: IpAddr) -> std::result::Result<(), NetRefusal> {
    if is_non_public(ip) {
        Err(NetRefusal::Blocked(format!(
            "{shown} is a private or local network address"
        )))
    } else {
        Ok(())
    }
}

/// Resolve `name` and return its addresses only if every one of them is public.
async fn resolve_checked(name: &str) -> std::result::Result<Vec<IpAddr>, NetRefusal> {
    let lookup = name.strip_suffix('.').unwrap_or(name);
    let addrs = tokio::net::lookup_host((lookup, 0))
        .await
        .map_err(|e| NetRefusal::Unresolved(format!("could not resolve `{name}`: {e}")))?;
    let mut ips: Vec<IpAddr> = Vec::new();
    for a in addrs {
        if !ips.contains(&a.ip()) {
            ips.push(a.ip());
        }
    }
    if ips.is_empty() {
        return Err(NetRefusal::Unresolved(format!(
            "`{name}` resolved to no addresses"
        )));
    }
    if let Some(ip) = ips.iter().find(|ip| is_non_public(**ip)) {
        return Err(NetRefusal::Blocked(format!(
            "`{name}` resolves to a private or local network address ({ip})"
        )));
    }
    Ok(ips)
}

/// Whether a plugin must not connect to `ip`: loopback, private, link-local, shared (CGNAT),
/// unspecified, broadcast, multicast, reserved, benchmarking and documentation ranges, IPv6
/// unique-local and link-local, and IPv4 addresses embedded in IPv6 (mapped, compatible, NAT64)
/// that are any of these.
pub(crate) fn is_non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_non_public_v4(v4),
        IpAddr::V6(v6) => is_non_public_v6(v6),
    }
}

fn is_non_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    // 127/8, 10/8, 172.16/12, 192.168/16, 169.254/16, 255.255.255.255, 224/4, and
    // 192.0.2/24, 198.51.100/24, 203.0.113/24 (documentation).
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        // 0/8 "this network", including the unspecified address.
        || a == 0
        // 100.64/10 shared address space (CGNAT).
        || (a == 100 && (b & 0xC0) == 64)
        // 192.0.0/24 protocol assignments.
        || (a == 192 && b == 0 && c == 0)
        // 198.18/15 benchmarking.
        || (a == 198 && (b & 0xFE) == 18)
        // 240/4 reserved.
        || a >= 240
}

fn is_non_public_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    // IPv4-mapped (::ffff:0:0/96).
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_non_public_v4(v4);
    }
    let seg = ip.segments();
    // IPv4-compatible (::/96, deprecated) and the NAT64 well-known prefix (64:ff9b::/96).
    if seg[..6] == [0; 6] || seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let v4 = Ipv4Addr::from((u32::from(seg[6]) << 16) | u32::from(seg[7]));
        return is_non_public_v4(v4);
    }
    // fc00::/7 unique local.
    (seg[0] & 0xFE00) == 0xFC00
        // fe80::/10 link-local and fec0::/10 site-local (deprecated).
        || (seg[0] & 0xFFC0) == 0xFE80
        || (seg[0] & 0xFFC0) == 0xFEC0
        // 2001:db8::/32 and 3fff::/20 documentation.
        || (seg[0] == 0x2001 && seg[1] == 0x0db8)
        || (seg[0] == 0x3fff && (seg[1] & 0xF000) == 0)
        // 64:ff9b:1::/48 local-use NAT64.
        || (seg[0] == 0x64 && seg[1] == 0xff9b && seg[2] == 1)
        // 100::/64 discard.
        || (seg[0] == 0x0100 && seg[1..4] == [0, 0, 0])
}
