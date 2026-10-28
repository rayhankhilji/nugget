//! Fakes shared by the integration tests: a scripted connector, its factory, a fake model
//! provider and a harness.
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::Future;
use nugget_connector::{
    AuthFlow, AuthKind, CancellationToken, Change, ChangeSink, Connector, ConnectorContext,
    ConnectorFactory, Content, Document, KindInfo, Registry,
};
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::ids::{ItemKey, ItemRef, ProviderId, SourceId};
use nugget_core::ipc::{SourceInfo, SourceSpec, SourceState};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Part, Provider, ProviderKind, ProviderMeta, Router,
    StopReason, TierMap,
};
use nugget_ingest::{Ingest, IngestConfig, MemorySecrets, NoPowerPolicy, PowerPolicy};
use nugget_store::Store;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

// ---------------------------------------------------------------- connector

#[derive(Debug, Clone, Copy)]
pub enum ErrKind {
    Network,
    Unauthorized,
}

impl ErrKind {
    fn err(self) -> Error {
        match self {
            ErrKind::Network => Error::Network("connection reset".into()),
            ErrKind::Unauthorized => Error::Unauthorized("token revoked".into()),
        }
    }
}

/// One scripted action of a sync pass.
#[derive(Debug, Clone)]
pub enum Step {
    Send(Change),
    Checkpoint(Value),
    Fail(ErrKind),
    Return(Value),
    /// Never finishes and ignores cancellation (a misbehaving connector).
    Hang,
    SetSecret(String, String),
    RecordKnown,
}

pub struct FakeState {
    pub passes: Mutex<VecDeque<Vec<Step>>>,
    pub sync_calls: AtomicUsize,
    pub sync_times: Mutex<Vec<Instant>>,
    pub cursors_seen: Mutex<Vec<Option<Value>>>,
    /// Sends that returned (backpressure observation).
    pub sent: AtomicUsize,
    pub watch: AtomicBool,
    pub watch_calls: AtomicUsize,
    pub watch_cancelled: AtomicUsize,
    live_tx: mpsc::UnboundedSender<Change>,
    live_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Change>>>,
    pub poll_interval: Mutex<Duration>,
    pub known: Mutex<Option<HashMap<ItemKey, Option<String>>>>,
    pub auth_done: Mutex<Option<oneshot::Receiver<()>>>,
}

impl Default for FakeState {
    fn default() -> Self {
        let (live_tx, live_rx) = mpsc::unbounded_channel();
        Self {
            passes: Mutex::new(VecDeque::new()),
            sync_calls: AtomicUsize::new(0),
            sync_times: Mutex::new(Vec::new()),
            cursors_seen: Mutex::new(Vec::new()),
            sent: AtomicUsize::new(0),
            watch: AtomicBool::new(false),
            watch_calls: AtomicUsize::new(0),
            watch_cancelled: AtomicUsize::new(0),
            live_tx,
            live_rx: Arc::new(tokio::sync::Mutex::new(live_rx)),
            poll_interval: Mutex::new(Duration::from_secs(3600)),
            known: Mutex::new(None),
            auth_done: Mutex::new(None),
        }
    }
}

impl FakeState {
    pub fn script(&self, steps: Vec<Step>) {
        self.passes.lock().unwrap().push_back(steps);
    }

    pub fn live(&self, change: Change) {
        self.live_tx.send(change).unwrap();
    }

    pub fn calls(&self) -> usize {
        self.sync_calls.load(Ordering::SeqCst)
    }
}

pub struct FakeConnector {
    state: Arc<FakeState>,
    ctx: ConnectorContext,
}

struct WatchGuard(Arc<FakeState>);
impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.0.watch_cancelled.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Connector for FakeConnector {
    fn kind(&self) -> &'static str {
        "fake"
    }

    async fn sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        _cancel: &CancellationToken,
    ) -> Result<Value> {
        let s = &self.state;
        s.sync_calls.fetch_add(1, Ordering::SeqCst);
        s.sync_times.lock().unwrap().push(Instant::now());
        s.cursors_seen.lock().unwrap().push(cursor.clone());
        let steps = s.passes.lock().unwrap().pop_front();
        let fallback = cursor.unwrap_or(json!("empty"));
        let Some(steps) = steps else {
            return Ok(fallback);
        };
        for step in steps {
            match step {
                Step::Send(c) => {
                    sink.send(c).await?;
                    s.sent.fetch_add(1, Ordering::SeqCst);
                }
                Step::Checkpoint(v) => sink.checkpoint(v).await?,
                Step::Fail(k) => return Err(k.err()),
                Step::Return(v) => return Ok(v),
                Step::Hang => std::future::pending::<()>().await,
                Step::SetSecret(a, v) => self.ctx.secrets.set(&a, &v)?,
                Step::RecordKnown => {
                    let v = self.ctx.known.versions().await?;
                    *s.known.lock().unwrap() = Some(v);
                }
            }
        }
        Ok(fallback)
    }

    fn poll_interval(&self) -> Duration {
        *self.state.poll_interval.lock().unwrap()
    }

    fn supports_watch(&self) -> bool {
        self.state.watch.load(Ordering::SeqCst)
    }

    async fn watch(&self, sink: Arc<dyn ChangeSink>, cancel: CancellationToken) -> Result<()> {
        self.state.watch_calls.fetch_add(1, Ordering::SeqCst);
        let _guard = WatchGuard(self.state.clone());
        let mut rx = self.state.live_rx.lock().await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(Error::Cancelled),
                c = rx.recv() => match c {
                    Some(c) => sink.send(c).await?,
                    None => return Ok(()),
                },
            }
        }
    }

    async fn begin_auth(&self) -> Result<AuthFlow> {
        let rx = self
            .state
            .auth_done
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| Error::Unsupported("no auth scripted".into()))?;
        let secrets = self.ctx.secrets.clone();
        let account = nugget_core::secrets::oauth_account(self.ctx.source.as_str());
        Ok(AuthFlow {
            url: "https://auth.example/authorize?x=1".into(),
            done: Box::pin(async move {
                rx.await.map_err(|_| Error::Cancelled)?;
                secrets.set(&account, "{\"access_token\":\"t\"}")
            }),
        })
    }
}

#[derive(Default)]
pub struct FakeFactory {
    states: Mutex<HashMap<String, Arc<FakeState>>>,
    pub contexts: Mutex<Vec<ConnectorContext>>,
}

impl FakeFactory {
    /// The (shared) state for a source id, created on first use.
    pub fn state(&self, id: &str) -> Arc<FakeState> {
        self.states
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_default()
            .clone()
    }
}

impl ConnectorFactory for FakeFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: "fake",
            display_name: "Fake source",
            auth: AuthKind::OAuth,
            settings_schema: json!({"type": "object"}),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        if settings.get("bad") == Some(&Value::Bool(true)) {
            return Err(Error::Invalid("bad settings".into()));
        }
        Ok(())
    }

    fn build(&self, _settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let state = self.state(ctx.source.as_str());
        self.contexts.lock().unwrap().push(ctx.clone());
        Ok(Arc::new(FakeConnector { state, ctx }))
    }
}

/// `Arc` wrapper so a test can keep a handle to the factory it registered.
pub struct SharedFactory(pub Arc<FakeFactory>);

impl ConnectorFactory for SharedFactory {
    fn info(&self) -> KindInfo {
        self.0.info()
    }
    fn validate(&self, settings: &Value) -> Result<()> {
        self.0.validate(settings)
    }
    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        self.0.build(settings, ctx)
    }
}

// ---------------------------------------------------------------- provider

/// A fake model: triage replies and deterministic embeddings, recording every input.
pub struct FakeProvider {
    id: ProviderId,
    locality: Locality,
    /// Inputs of every chat request (all text parts, joined).
    pub chat_inputs: Mutex<Vec<String>>,
    /// Every text sent for embedding.
    pub embed_inputs: Mutex<Vec<String>>,
    /// Embedding calls containing this marker wait until `open` is set.
    pub block_marker: Mutex<Option<String>>,
    open: tokio::sync::watch::Sender<bool>,
    pub dim: usize,
}

impl FakeProvider {
    pub fn new(id: &str, locality: Locality) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId::new(id),
            locality,
            chat_inputs: Mutex::new(Vec::new()),
            embed_inputs: Mutex::new(Vec::new()),
            block_marker: Mutex::new(None),
            open: tokio::sync::watch::channel(true).0,
            dim: 4,
        })
    }

    /// Block embedding calls whose input contains `marker` (`""` blocks everything).
    pub fn block(&self, marker: &str) {
        *self.block_marker.lock().unwrap() = Some(marker.to_owned());
        self.open.send_replace(false);
    }

    pub fn unblock(&self) {
        self.open.send_replace(true);
    }

    pub fn embedded(&self) -> Vec<String> {
        self.embed_inputs.lock().unwrap().clone()
    }

    pub fn chats(&self) -> Vec<String> {
        self.chat_inputs.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "fake-model".into(),
            locality: self.locality,
            context_tokens: None,
        }
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        let text: String = req
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.chat_inputs.lock().unwrap().push(text.clone());
        let junk = text.contains("SPAM");
        let sensitive = text.contains("CONFIDENTIAL");
        let reply = json!({
            "keep": !junk,
            "junk": junk,
            "sensitive": sensitive,
            "summary": "A short summary of the document.",
            "title": "Triage title"
        })
        .to_string();
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ChatEvent::TextDelta(reply)),
            Ok(ChatEvent::Done {
                stop_reason: StopReason::EndTurn,
                input_tokens: None,
                output_tokens: None,
            }),
        ])))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_inputs
            .lock()
            .unwrap()
            .extend(texts.iter().cloned());
        let marker = self.block_marker.lock().unwrap().clone();
        if let Some(m) = marker
            && texts.iter().any(|t| t.contains(&m))
        {
            let mut rx = self.open.subscribe();
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        }
        Ok(texts
            .iter()
            .map(|t| {
                let n = t.len() as f32;
                let mut v = vec![1.0; self.dim];
                v[0] += n % 7.0;
                v[1] += (n % 3.0) * 0.5;
                v
            })
            .collect())
    }
}

/// A router over the given providers: each `(provider, tiers)` joins those tiers in order and is
/// marked as probed for streaming, JSON-schema output and embeddings.
pub fn router(providers: &[(Arc<FakeProvider>, Vec<Tier>)]) -> Arc<Router> {
    let mut map = TierMap::new();
    for (p, tiers) in providers {
        for t in tiers {
            map.entry(*t).or_default().push(p.id().clone());
        }
    }
    let r = Router::new(
        providers
            .iter()
            .map(|(p, _)| p.clone() as Arc<dyn Provider>)
            .collect(),
        map,
    );
    for (p, _) in providers {
        r.set_capabilities(
            p.id(),
            Capabilities {
                streaming: true,
                json_schema: true,
                embeddings: true,
                ..Default::default()
            },
        );
    }
    Arc::new(r)
}

// ---------------------------------------------------------------- power

#[derive(Default)]
pub struct TogglePower(pub AtomicBool);

impl PowerPolicy for TogglePower {
    fn constrained(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------- harness

pub fn cfg() -> IngestConfig {
    IngestConfig {
        backoff_base_ms: 20,
        backoff_max_ms: 200,
        power_poll_ms: 20,
        extract_timeout_ms: 5_000,
        ..Default::default()
    }
}

pub struct Harness {
    pub ingest: Ingest,
    pub store: Store,
    pub factory: Arc<FakeFactory>,
    pub secrets: MemorySecrets,
    pub paths: Paths,
    pub dir: Arc<tempfile::TempDir>,
}

pub struct Opts {
    pub cfg: IngestConfig,
    pub router: Option<Arc<Router>>,
    pub power: Arc<dyn PowerPolicy>,
    pub store: Option<Store>,
    pub dir: Option<Arc<tempfile::TempDir>>,
    pub secrets: Option<MemorySecrets>,
    /// Pre-scripted factory (sources in `sources.toml` start syncing during `start`).
    pub factory: Option<Arc<FakeFactory>>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            cfg: cfg(),
            router: None,
            power: Arc::new(NoPowerPolicy),
            store: None,
            dir: None,
            secrets: None,
            factory: None,
        }
    }
}

pub async fn start(opts: Opts) -> Harness {
    let dir = opts
        .dir
        .unwrap_or_else(|| Arc::new(tempfile::tempdir().unwrap()));
    let paths = Paths::rooted(dir.path().to_path_buf());
    let store = opts
        .store
        .unwrap_or_else(|| Store::open_in_memory().unwrap());
    let secrets = opts.secrets.unwrap_or_default();
    let factory = opts.factory.unwrap_or_default();
    let mut registry = Registry::new();
    registry.register(Arc::new(SharedFactory(factory.clone())));
    let ingest = Ingest::builder(opts.cfg, store.clone(), registry, paths.clone())
        .router(opts.router)
        .secrets(Arc::new(secrets.clone()))
        .power(opts.power)
        .start()
        .await
        .unwrap();
    Harness {
        ingest,
        store,
        factory,
        secrets,
        paths,
        dir,
    }
}

impl Harness {
    /// Add source `id` of kind `fake` (script its state first with `self.factory.state(id)`).
    pub async fn add(&self, id: &str) -> SourceInfo {
        self.ingest
            .add(SourceSpec {
                id: Some(SourceId::new(id)),
                kind: "fake".into(),
                name: format!("Source {id}"),
                settings: json!({"root": "/tmp/x"}),
            })
            .await
            .unwrap()
    }

    pub fn item(&self, source: &str, key: &str) -> Option<nugget_store::Item> {
        self.store
            .get_item(&ItemRef {
                source: SourceId::new(source),
                key: ItemKey::new(key),
            })
            .unwrap()
    }

    pub fn cursor(&self, source: &str) -> Option<Value> {
        self.store.get_cursor(&SourceId::new(source)).unwrap()
    }

    pub async fn state(&self, source: &str) -> SourceState {
        self.ingest.get(&SourceId::new(source)).await.unwrap().state
    }

    pub async fn wait_state(&self, source: &str, want: SourceState) {
        let id = SourceId::new(source);
        eventually_async(&format!("{source} in state {want:?}"), || {
            let ingest = self.ingest.clone();
            let id = id.clone();
            async move { matches!(ingest.get(&id).await, Ok(i) if i.state == want) }
        })
        .await;
    }

    pub fn file_path(&self) -> std::path::PathBuf {
        self.paths.config.join(nugget_ingest::SOURCES_FILE)
    }
}

pub fn text_doc(key: &str, text: &str, version: Option<&str>) -> Change {
    let mut d = Document::new(key, "text/plain", Content::Text(text.to_owned()));
    d.version = version.map(str::to_owned);
    Change::Upsert(d)
}

pub fn delete(key: &str) -> Change {
    Change::Delete(ItemKey::new(key))
}

pub async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub async fn eventually_async<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn dir_of(h: &Harness) -> &Path {
    h.dir.path()
}
