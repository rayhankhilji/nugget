//! Load test for the cloud service against a REAL local Postgres and fake upstreams.
//!
//! Spins up a fresh database (shim + migrations), N users, the real axum router on a random
//! localhost port with fake Auth / Composio / LLM / Embedder (latencies configurable), and drives
//! it over HTTP. Scenarios: seeding (connect + `/v1/sync`), Ask at N concurrent users, the sync
//! scheduler (`Ingest::run_due`), and the AI gateway (DB overhead, quota race).
//!
//! ```sh
//! LT_DATABASE_URL=postgres://postgres@127.0.0.1:54329/postgres?sslmode=disable \
//!   cargo run --release -p nugget-cloud --example loadtest
//! ```
//!
//! Knobs (env): LT_USERS (100), LT_DURATION_SECS (60, Ask), LT_CEIL_SECS (20, Ask with zero-latency
//! fakes), LT_LLM_MS (1500), LT_LLM_IN (7800), LT_LLM_OUT (520), LT_EMBED_MS (100),
//! LT_COMPOSIO_MS (300), LT_MSGS (50 per Gmail page), LT_MSG_BYTES (2000), LT_GMAIL_PAGES (1),
//! LT_EVENTS (30), LT_INCR (5 new items per incremental sync), LT_POOLS ("8,16"),
//! LT_EMBED_MODES ("off,on"), LT_RACE (20), LT_KEEP_DB (unset: drop the database at the end).

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nugget_cloud::api::{AppState, router};
use nugget_cloud::auth::{AuthError, AuthProvider, AuthUser};
use nugget_cloud::composio::{Account, Composio, ComposioError, Link, Toolkit};
use nugget_cloud::db::{Db, PoolOptions, UsageKind};
use nugget_cloud::gateway::{
    Allowance, Embedder, Gateway, GatewayError, Llm, LlmRequest, LlmResponse, Role,
};
use nugget_cloud::plans::{Catalog, ModelTier};
use nugget_cloud::ratelimit::Limits;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

// ---- config ------------------------------------------------------------------------------------

fn env<T: FromStr>(k: &str, d: T) -> T {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

#[derive(Clone)]
struct Cfg {
    url: String,
    users: usize,
    duration: Duration,
    ceil: Duration,
    llm_ms: u64,
    llm_in: u32,
    llm_out: u32,
    embed_ms: u64,
    composio_ms: u64,
    msgs: usize,
    msg_bytes: usize,
    pages: u64,
    events: usize,
    incr: usize,
    pools: Vec<usize>,
    modes: Vec<bool>,
    race: usize,
    keep_db: bool,
}

impl Cfg {
    fn from_env() -> Self {
        let pools = env("LT_POOLS", "8,16".to_owned())
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        let modes = env("LT_EMBED_MODES", "off,on".to_owned())
            .split(',')
            .map(|s| s.trim() == "on")
            .collect();
        Self {
            url: env(
                "LT_DATABASE_URL",
                "postgres://postgres@127.0.0.1:54329/postgres?sslmode=disable".to_owned(),
            ),
            users: env("LT_USERS", 100),
            duration: Duration::from_secs(env("LT_DURATION_SECS", 60)),
            ceil: Duration::from_secs(env("LT_CEIL_SECS", 20)),
            llm_ms: env("LT_LLM_MS", 1500),
            llm_in: env("LT_LLM_IN", 7800),
            llm_out: env("LT_LLM_OUT", 520),
            embed_ms: env("LT_EMBED_MS", 100),
            composio_ms: env("LT_COMPOSIO_MS", 300),
            msgs: env("LT_MSGS", 50),
            msg_bytes: env("LT_MSG_BYTES", 2000),
            pages: env("LT_GMAIL_PAGES", 1),
            events: env("LT_EVENTS", 30),
            incr: env("LT_INCR", 5),
            pools,
            modes,
            race: env("LT_RACE", 20),
            keep_db: std::env::var("LT_KEEP_DB").is_ok(),
        }
    }
}

// ---- synthetic text ----------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

const SYL: [&str; 20] = [
    "ba", "ke", "li", "mo", "nu", "ra", "se", "to", "vi", "zu", "da", "fe", "gi", "ho", "ju", "ka",
    "le", "mi", "no", "pu",
];
const TOPICS: usize = 2000;
const FILLER: &[&str] = &[
    "please",
    "thanks",
    "regarding",
    "attached",
    "schedule",
    "update",
    "team",
    "follow",
    "next",
    "week",
    "call",
    "review",
    "details",
    "confirm",
    "available",
    "morning",
    "afternoon",
    "quick",
    "note",
    "check",
    "plan",
    "send",
    "share",
    "done",
    "today",
    "tomorrow",
    "later",
    "great",
    "sure",
    "also",
    "just",
    "need",
    "want",
    "think",
    "know",
    "time",
    "good",
    "new",
    "before",
    "after",
];

/// One of 2,000 pseudo-words; each appears in a few percent of messages, like real topics.
fn topic(i: usize) -> String {
    format!(
        "{}{}{}",
        SYL[i % 20],
        SYL[(i / 20) % 20],
        SYL[(i / 400) % 20]
    )
}

fn text(rng: &mut Rng, bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 100);
    while s.len() < bytes {
        let n = 8 + rng.below(7);
        for w in 0..n {
            let word = if rng.below(10) < 3 {
                topic(rng.below(TOPICS))
            } else {
                FILLER[rng.below(FILLER.len())].to_owned()
            };
            if w > 0 {
                s.push(' ');
            }
            s.push_str(&word);
        }
        s.push_str(". ");
    }
    s
}

fn question(rng: &mut Rng) -> String {
    let (a, b) = (topic(rng.below(TOPICS)), topic(rng.below(TOPICS)));
    match rng.below(5) {
        0 => format!("When is the {a} {b}?"),
        1 => format!("What did {a} say about {b}?"),
        2 => format!("Do I have anything about {a} or {b} this week?"),
        3 => format!("Summarise the {a} thread"),
        _ => format!("Where is the {a} {b} meeting?"),
    }
}

fn rfc(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

async fn nap(ms: u64) {
    if ms > 0 {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

// ---- doubles -----------------------------------------------------------------------------------

/// Tokens are `tok-<uuid>`.
struct FakeAuth;

#[async_trait]
impl AuthProvider for FakeAuth {
    async fn verify(&self, token: &str) -> Result<AuthUser, AuthError> {
        let id = token
            .strip_prefix("tok-")
            .and_then(|s| s.parse().ok())
            .ok_or(AuthError::Invalid)?;
        Ok(AuthUser {
            id,
            email: Some(format!("{id}@example.com")),
        })
    }
    async fn delete_user(&self, _id: Uuid) -> Result<(), AuthError> {
        Ok(())
    }
}

/// Composio with in-memory accounts. The first `pages` Gmail calls per account return full pages
/// (the backfill); later calls return `incr` new items (incremental syncs).
struct FakeComposio {
    latency_ms: AtomicU64,
    accounts: Mutex<HashMap<String, Vec<Account>>>,
    calls: Mutex<HashMap<String, u64>>,
    tool_calls: AtomicU64,
    cfg: Cfg,
}

impl FakeComposio {
    fn new(cfg: &Cfg) -> Self {
        Self {
            latency_ms: AtomicU64::new(cfg.composio_ms),
            accounts: Mutex::default(),
            calls: Mutex::default(),
            tool_calls: AtomicU64::new(0),
            cfg: cfg.clone(),
        }
    }

    fn gmail(&self, account: &str, n: u64) -> Value {
        let backfill = n < self.cfg.pages;
        let count = if backfill {
            self.cfg.msgs
        } else {
            self.cfg.incr
        };
        let now = OffsetDateTime::now_utc();
        let messages: Vec<Value> = (0..count)
            .map(|i| {
                let id = format!("{account}-{n}-{i}");
                let mut r = Rng::new(hash(&id));
                let age = if backfill {
                    time::Duration::minutes(r.below(90 * 24 * 60) as i64)
                } else {
                    time::Duration::minutes(r.below(30) as i64)
                };
                let sender = topic(r.below(TOPICS));
                json!({
                    "messageId": id, "threadId": format!("t-{id}"),
                    "messageTimestamp": rfc(now - age),
                    "sender": format!("{sender} <{sender}@example.com>"), "to": "me@example.com",
                    "subject": format!("{} {} {}", topic(r.below(TOPICS)), FILLER[r.below(FILLER.len())], topic(r.below(TOPICS))),
                    "messageText": text(&mut r, self.cfg.msg_bytes),
                    "labelIds": ["INBOX"],
                })
            })
            .collect();
        let mut v = json!({ "messages": messages });
        if backfill && n + 1 < self.cfg.pages {
            v["nextPageToken"] = json!(format!("p{}", n + 1));
        }
        v
    }

    fn calendar(&self, account: &str, n: u64) -> Value {
        let count = if n == 0 {
            self.cfg.events
        } else {
            self.cfg.incr
        };
        let now = OffsetDateTime::now_utc();
        let items: Vec<Value> = (0..count)
            .map(|i| {
                let id = format!("{account}-{n}-{i}");
                let mut r = Rng::new(hash(&id));
                let start = now + time::Duration::hours(r.below(24 * 90) as i64 - 24 * 30);
                json!({
                    "id": id, "status": "confirmed",
                    "summary": format!("{} {}", topic(r.below(TOPICS)), topic(r.below(TOPICS))),
                    "start": {"dateTime": rfc(start)}, "end": {"dateTime": rfc(start + time::Duration::minutes(30))},
                    "location": topic(r.below(TOPICS)),
                    "attendees": [{"email": format!("{}@example.com", topic(r.below(TOPICS)))}, {"displayName": topic(r.below(TOPICS))}],
                    "description": text(&mut r, 300),
                    "htmlLink": format!("https://calendar.google.com/event?eid={id}"),
                })
            })
            .collect();
        json!({ "items": items })
    }
}

#[async_trait]
impl Composio for FakeComposio {
    async fn toolkits(&self) -> Result<Vec<Toolkit>, ComposioError> {
        Ok(["gmail", "googlecalendar"]
            .iter()
            .map(|s| Toolkit {
                slug: (*s).into(),
                name: (*s).into(),
                logo: None,
            })
            .collect())
    }
    async fn create_link(
        &self,
        user: &str,
        auth_config: &str,
        _cb: &str,
    ) -> Result<Link, ComposioError> {
        nap(self.latency_ms.load(Ordering::Relaxed)).await;
        let toolkit = if auth_config == "ac_gmail" {
            "gmail"
        } else {
            "googlecalendar"
        };
        let id = format!("ca_{user}_{toolkit}");
        let mut acc = self.accounts.lock().unwrap();
        let list = acc.entry(user.into()).or_default();
        if !list.iter().any(|a| a.id == id) {
            list.push(Account {
                id: id.clone(),
                toolkit: toolkit.into(),
                status: "ACTIVE".into(),
            });
        }
        Ok(Link {
            connected_account_id: id,
            redirect_url: format!("https://connect.example/{auth_config}"),
        })
    }
    async fn accounts(&self, user: &str) -> Result<Vec<Account>, ComposioError> {
        nap(self.latency_ms.load(Ordering::Relaxed)).await;
        Ok(self
            .accounts
            .lock()
            .unwrap()
            .get(user)
            .cloned()
            .unwrap_or_default())
    }
    async fn delete_account(&self, id: &str) -> Result<(), ComposioError> {
        for list in self.accounts.lock().unwrap().values_mut() {
            list.retain(|a| a.id != id);
        }
        Ok(())
    }
    async fn upsert_trigger(
        &self,
        _user: &str,
        _account: &str,
        _trigger: &str,
        _config: Value,
    ) -> Result<String, ComposioError> {
        // The load test runs without a webhook secret, so triggers are never enabled.
        Err(ComposioError::NotConfigured)
    }
    async fn execute(
        &self,
        _user: &str,
        account: &str,
        tool: &str,
        _args: Value,
    ) -> Result<Value, ComposioError> {
        nap(self.latency_ms.load(Ordering::Relaxed)).await;
        self.tool_calls.fetch_add(1, Ordering::Relaxed);
        let n = {
            let mut c = self.calls.lock().unwrap();
            let e = c.entry(account.into()).or_default();
            *e += 1;
            *e - 1
        };
        match tool {
            "GMAIL_FETCH_EMAILS" => Ok(self.gmail(account, n)),
            "GOOGLECALENDAR_EVENTS_LIST" => Ok(self.calendar(account, n)),
            _ => Err(ComposioError::Tool {
                tool: tool.into(),
                message: "unknown tool".into(),
            }),
        }
    }
}

struct FakeLlm {
    latency_ms: AtomicU64,
    input: u32,
    output: u32,
}

#[async_trait]
impl Llm for FakeLlm {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, GatewayError> {
        nap(self.latency_ms.load(Ordering::Relaxed)).await;
        let prompt = req.messages.last().map(|m| m.1.as_str()).unwrap_or("");
        let mut text = if prompt.contains("<source n=\"1\"") {
            "From your mail: it is on Thursday at 10:00 [1]".to_owned()
        } else {
            "I couldn't find that in your mail or calendar.".to_owned()
        };
        if prompt.contains("<source n=\"2\"") {
            text.push_str(" and was confirmed later [2]");
        }
        // A realistic answer body (~4 chars per output token).
        while text.len() < self.output as usize * 4 {
            text.push_str(" More detail about the plan.");
        }
        Ok(LlmResponse {
            text,
            input_tokens: self.input,
            output_tokens: self.output,
        })
    }
}

/// Bag-of-words vectors (similar texts are near each other), one sleep per call.
struct FakeEmbedder {
    latency_ms: AtomicU64,
}

fn bow(t: &str) -> Vec<f32> {
    let mut v = vec![0f32; 1536];
    for w in t
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
    {
        v[(hash(w) % 1536) as usize] += 1.0;
    }
    v[0] += 0.01;
    v
}

#[async_trait]
impl Embedder for FakeEmbedder {
    fn model(&self) -> &str {
        "text-embedding-3-small"
    }
    fn dims(&self) -> usize {
        1536
    }
    async fn embed(&self, texts: &[String]) -> Result<(Vec<Vec<f32>>, u32), GatewayError> {
        nap(self.latency_ms.load(Ordering::Relaxed)).await;
        Ok((
            texts.iter().map(|t| bow(t)).collect(),
            texts.iter().map(|t| t.len() as u32 / 4).sum(),
        ))
    }
}

struct Fakes {
    composio: Arc<FakeComposio>,
    llm: Arc<FakeLlm>,
    embedder: Arc<FakeEmbedder>,
}

impl Fakes {
    fn new(cfg: &Cfg) -> Self {
        Self {
            composio: Arc::new(FakeComposio::new(cfg)),
            llm: Arc::new(FakeLlm {
                latency_ms: AtomicU64::new(cfg.llm_ms),
                input: cfg.llm_in,
                output: cfg.llm_out,
            }),
            embedder: Arc::new(FakeEmbedder {
                latency_ms: AtomicU64::new(cfg.embed_ms),
            }),
        }
    }
    fn set_latency(&self, llm: u64, embed: u64) {
        self.llm.latency_ms.store(llm, Ordering::Relaxed);
        self.embedder.latency_ms.store(embed, Ordering::Relaxed);
    }
    fn gateway(&self, embed: bool) -> Gateway {
        Gateway::new(
            Some(self.llm.clone()),
            embed.then(|| self.embedder.clone() as Arc<dyn Embedder>),
            "claude-haiku-5-5".into(),
            "claude-sonnet-5-5".into(),
        )
    }
}

// ---- database ----------------------------------------------------------------------------------

type Pg = Arc<tokio_postgres::Client>;

async fn pg(cfg: &tokio_postgres::Config) -> Pg {
    let (client, conn) = cfg.connect(tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(conn);
    Arc::new(client)
}

/// `postgres://…` → key/value form for `dbname`.
fn kv_url(url: &str, db: &str) -> String {
    let cfg: tokio_postgres::Config = url.parse().expect("database url");
    let mut kv = Vec::new();
    for h in cfg.get_hosts() {
        if let tokio_postgres::config::Host::Tcp(h) = h {
            kv.push(format!("host={h}"));
        }
    }
    if let Some(p) = cfg.get_ports().first() {
        kv.push(format!("port={p}"));
    }
    if let Some(u) = cfg.get_user() {
        kv.push(format!("user={u}"));
    }
    if let Some(pw) = cfg.get_password() {
        kv.push(format!("password='{}'", String::from_utf8_lossy(pw)));
    }
    kv.push("sslmode=disable".into());
    kv.push(format!("dbname={db}"));
    kv.join(" ")
}

/// A fresh database with the shim and the migrations; returns (kv url, name, superuser client).
async fn fresh_db(url: &str) -> (String, String, Pg) {
    let admin = pg(&url.parse().unwrap()).await;
    let name = format!("nugget_lt_{}", Uuid::new_v4().simple());
    admin
        .batch_execute(&format!("create database {name}"))
        .await
        .expect("create db");
    let kv = kv_url(url, &name);
    let client = pg(&kv.parse().unwrap()).await;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../supabase");
    let mut files = vec![root.join("tests/supabase_shim.sql")];
    let mut migrations: Vec<_> = std::fs::read_dir(root.join("migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    migrations.sort();
    files.extend(migrations);
    for f in files {
        let sql = std::fs::read_to_string(&f).unwrap();
        client
            .batch_execute(&sql)
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", f.display()));
    }
    (kv, name, client)
}

async fn drop_db(url: &str, name: &str) {
    let admin = pg(&url.parse().unwrap()).await;
    if let Err(e) = admin
        .batch_execute(&format!("drop database {name} with (force)"))
        .await
    {
        eprintln!("could not drop {name}: {e}");
    }
}

async fn count(pg: &Pg, sql: &str) -> i64 {
    pg.query_one(sql, &[]).await.unwrap().get(0)
}

// ---- server ------------------------------------------------------------------------------------

struct Server {
    base: String,
    state: Arc<AppState>,
    task: tokio::task::JoinHandle<()>,
}

/// The default plans with placeholder Stripe prices (billing is not exercised here).
fn catalog() -> Catalog {
    Catalog::parse(nugget_cloud::plans::DEFAULT_TOML, |k| {
        k.starts_with("STRIPE_PRICE_").then(|| format!("price_{k}"))
    })
    .expect("plans")
}

/// The plan every load-test user subscribes to (the entry plan).
const PLAN: &str = "heed";

/// Active subscriptions for `users`, as a verified Stripe webhook would write them.
async fn subscribe(admin: &Pg, users: &[Uuid]) {
    admin
        .execute(
            "insert into public.subscriptions (id, user_id, plan, interval, status, stripe_price_id)
             select 'sub_' || replace(u::text, '-', ''), u, $2, 'month', 'active', 'price_test'
             from unnest($1::uuid[]) u",
            &[&users, &PLAN],
        )
        .await
        .unwrap();
}

async fn serve(kv: &str, pool: usize, fakes: &Fakes, embed: bool, limits: Limits) -> Server {
    let db = Db::connect_with(
        kv,
        None,
        PoolOptions {
            max_size: pool,
            ..PoolOptions::default()
        },
    )
    .expect("pool");
    let auth_configs = BTreeMap::from([
        ("gmail".to_owned(), "ac_gmail".to_owned()),
        ("googlecalendar".to_owned(), "ac_cal".to_owned()),
    ]);
    let mut state = AppState::new(
        db,
        Arc::new(FakeAuth),
        fakes.gateway(embed),
        Arc::new(catalog()),
    );
    state.composio = Some(fakes.composio.clone() as Arc<dyn Composio>);
    state.auth_configs = auth_configs;
    let state = Arc::new(state.with_limits(limits).with_client_ip_header(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { base, state, task }
}

async fn call(
    http: &reqwest::Client,
    method: reqwest::Method,
    url: String,
    user: Uuid,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = http.request(method, url).bearer_auth(format!("tok-{user}"));
    if let Some(b) = body {
        req = req.json(&b);
    }
    match req.send().await {
        Ok(res) => {
            let s = res.status().as_u16();
            (s, res.json().await.unwrap_or(Value::Null))
        }
        Err(_) => (0, Value::Null),
    }
}

// ---- stats -------------------------------------------------------------------------------------

fn pct(sorted: &[u64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[i] as f64 / 1000.0
}

#[derive(Default)]
struct Lat {
    us: Vec<u64>,
    status: BTreeMap<u16, u64>,
}

impl Lat {
    fn merge(&mut self, o: Lat) {
        self.us.extend(o.us);
        for (k, v) in o.status {
            *self.status.entry(k).or_default() += v;
        }
    }
    fn errors(&self) -> String {
        let e: Vec<String> = self
            .status
            .iter()
            .filter(|(k, _)| **k != 200)
            .map(|(k, v)| format!("{k}×{v}"))
            .collect();
        if e.is_empty() {
            "0".into()
        } else {
            e.join(" ")
        }
    }
    /// "p50 / p95 / p99 / max" in ms.
    fn summary(&mut self) -> String {
        self.summary_prec(0)
    }
    fn summary_prec(&mut self, p: usize) -> String {
        self.us.sort_unstable();
        let v = &self.us;
        format!(
            "{:.p$} / {:.p$} / {:.p$} / {:.p$}",
            pct(v, 0.5),
            pct(v, 0.95),
            pct(v, 0.99),
            pct(v, 1.0)
        )
    }
}

/// Samples the pool while a run lasts: (max connections in use, share of samples with none free).
fn sample_pool(db: Db, until: Instant) -> tokio::task::JoinHandle<(usize, f64)> {
    tokio::spawn(async move {
        let (mut max_used, mut sat, mut n) = (0, 0u64, 0u64);
        while Instant::now() < until {
            let (size, avail) = db.pool_status();
            max_used = max_used.max(size.saturating_sub(avail));
            n += 1;
            if avail == 0 {
                sat += 1;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        (max_used, sat as f64 / n.max(1) as f64)
    })
}

// ---- scenarios ---------------------------------------------------------------------------------

async fn seed(http: &reqwest::Client, s: &Server, users: &[Uuid], admin: &Pg) {
    let t0 = Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for &u in users {
        let (http, base) = (http.clone(), s.base.clone());
        set.spawn(async move {
            let mut ok = true;
            for tk in ["gmail", "googlecalendar"] {
                let (st, _) = call(
                    &http,
                    reqwest::Method::POST,
                    format!("{base}/v1/connections"),
                    u,
                    Some(json!({"toolkit": tk})),
                )
                .await;
                ok &= st == 200;
            }
            let (st, _) = call(
                &http,
                reqwest::Method::GET,
                format!("{base}/v1/connections"),
                u,
                None,
            )
            .await;
            ok && st == 200
        });
    }
    let connected = set.join_all().await.iter().filter(|b| **b).count();
    let connect_s = t0.elapsed().as_secs_f64();

    let t1 = Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for &u in users {
        let (http, base) = (http.clone(), s.base.clone());
        set.spawn(async move {
            let t = Instant::now();
            let (st, body) = call(
                &http,
                reqwest::Method::POST,
                format!("{base}/v1/sync"),
                u,
                None,
            )
            .await;
            let sync_errors = body["synced"]
                .as_array()
                .map(|a| a.iter().filter(|r| !r["error"].is_null()).count())
                .unwrap_or(0);
            let mut l = Lat::default();
            l.us.push(t.elapsed().as_micros() as u64);
            *l.status.entry(st).or_default() += 1;
            (l, sync_errors)
        });
    }
    let mut lat = Lat::default();
    let mut sync_errors = 0;
    for (l, e) in set.join_all().await {
        lat.merge(l);
        sync_errors += e;
    }
    let sync_s = t1.elapsed().as_secs_f64();
    let items = count(admin, "select count(*) from public.items").await;
    let chunks = count(admin, "select count(*) from public.chunks").await;
    let embedded = count(
        admin,
        "select count(*) from public.chunks where embedding is not null",
    )
    .await;
    let never = count(
        admin,
        "select count(*) from public.connections where last_synced_at is null",
    )
    .await;
    println!(
        "  connect: {connected}/{} users ok in {connect_s:.1}s ({:.0} users/s)",
        users.len(),
        users.len() as f64 / connect_s
    );
    println!(
        "  sync ({} users at once): wall {sync_s:.1}s, per-request p50/p95/p99/max ms {}, status errors {}, connection errors {sync_errors}",
        users.len(),
        lat.summary(),
        lat.errors()
    );
    println!(
        "  indexed {items} items, {chunks} chunks ({embedded} embedded): {:.0} items/s, {:.0} chunks/s; connections never marked synced: {never}",
        items as f64 / sync_s,
        chunks as f64 / sync_s,
    );
}

async fn vector_check(s: &Server, fakes: &Fakes, users: &[Uuid], admin: &Pg) {
    let mut got = Vec::new();
    let mut r = Rng::new(7);
    for &u in users.iter().take(10) {
        let (v, _) = fakes.embedder.embed(&[question(&mut r)]).await.unwrap();
        let hits = s.state.db.search_vector(u, &v[0], 40).await.unwrap();
        got.push(hits.len());
    }
    let per_user = count(admin, "select (count(*) / count(distinct user_id))::bigint from public.chunks where embedding is not null").await;
    println!(
        "  vector search hits for 10 users (limit 40, each user has ~{per_user} embedded chunks): {got:?}"
    );
    let lit = nugget_cloud::db::vector_literal(&bow("hello world"));
    let sql = format!(
        "begin; set local role nugget_service; select set_config('nugget.user_id', '{u}', true);
         explain select c.id from public.chunks c join public.items i on i.id = c.item_id
         where c.user_id = '{u}' and c.embedding is not null
         order by c.embedding operator(extensions.<=>) '{lit}'::extensions.vector limit 40; commit;",
        u = users[0]
    );
    let plan: Vec<String> = admin
        .simple_query(&sql)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|m| match m {
            tokio_postgres::SimpleQueryMessage::Row(r) if r.columns()[0].name() == "QUERY PLAN" => {
                r.get(0).map(str::to_owned)
            }
            _ => None,
        })
        .collect();
    println!(
        "  vector plan: {}",
        plan.iter()
            .map(|l| l.trim())
            .filter(|l| l.contains("Scan") || l.contains("Limit"))
            .collect::<Vec<_>>()
            .join(" | ")
    );
}

async fn xacts(admin: &Pg) -> i64 {
    // Statistics are flushed by backends about once a second; good enough over a long run.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    count(
        admin,
        "select xact_commit + xact_rollback from pg_stat_database where datname = current_database()",
    )
    .await
}

async fn ask_run(
    admin: &Pg,
    http: &reqwest::Client,
    s: &Server,
    users: &[Uuid],
    dur: Duration,
    label: &str,
    model_ms: u64,
) {
    let x0 = xacts(admin).await;
    let start = Instant::now();
    let until = start + dur;
    let sampler = sample_pool(s.state.db.clone(), until);
    let mut set = tokio::task::JoinSet::new();
    for (i, &u) in users.iter().enumerate() {
        let (http, base) = (http.clone(), s.base.clone());
        set.spawn(async move {
            let mut r = Rng::new(1000 + i as u64);
            let mut l = Lat::default();
            let mut cited = 0u64;
            while Instant::now() < until {
                let t = Instant::now();
                let (st, body) = call(
                    &http,
                    reqwest::Method::POST,
                    format!("{base}/v1/ask"),
                    u,
                    Some(json!({"question": question(&mut r)})),
                )
                .await;
                l.us.push(t.elapsed().as_micros() as u64);
                *l.status.entry(st).or_default() += 1;
                if body["citations"].as_array().is_some_and(|c| !c.is_empty()) {
                    cited += 1;
                }
            }
            (l, cited)
        });
    }
    let mut lat = Lat::default();
    let mut cited = 0;
    for (l, c) in set.join_all().await {
        lat.merge(l);
        cited += c;
    }
    let elapsed = start.elapsed().as_secs_f64();
    let (max_used, sat) = sampler.await.unwrap();
    let tx_per = (xacts(admin).await - x0) as f64 / lat.us.len().max(1) as f64;
    let n = lat.us.len();
    let ok = lat.status.get(&200).copied().unwrap_or(0);
    let summary = lat.summary();
    let over50 = pct(&lat.us, 0.5) - model_ms as f64;
    let over99 = pct(&lat.us, 0.99) - model_ms as f64;
    println!(
        "| {label} | {n} | {:.1} | {summary} | {over50:.0} / {over99:.0} | {} | {:.0}% | {max_used} / {:.0}% | {tx_per:.1} |",
        n as f64 / elapsed,
        lat.errors(),
        100.0 * cited as f64 / ok.max(1) as f64,
        100.0 * sat
    );
}

async fn scheduler(cfg: &Cfg, s: &Server, fakes: &Fakes, admin: &Pg) {
    let ingest = s.state.ingest().expect("composio configured");
    // Warm-up: finish whatever the seeding left (e.g. backfills cut off by a 504), untimed.
    admin
        .execute("update public.connections set next_sync_at = now()", &[])
        .await
        .unwrap();
    let t = Instant::now();
    let mut warm = 0;
    loop {
        let n = ingest.run_due(20).await;
        if n == 0 {
            break;
        }
        warm += n;
    }
    println!(
        "  warm-up drain (leftover backfills): {warm} connections in {:.1}s",
        t.elapsed().as_secs_f64()
    );
    for machines in [1usize, 2] {
        let due = admin
            .execute("update public.connections set next_sync_at = now()", &[])
            .await
            .unwrap();
        let calls0 = fakes.composio.tool_calls.load(Ordering::Relaxed);
        let t0 = Instant::now();
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..machines {
            let ingest = ingest.clone();
            set.spawn(async move {
                let mut passes = Vec::new();
                loop {
                    let t = Instant::now();
                    let n = ingest.run_due(20).await;
                    if n == 0 {
                        break;
                    }
                    passes.push((n, t.elapsed().as_secs_f64()));
                }
                passes
            });
        }
        let passes: Vec<(usize, f64)> = set.join_all().await.into_iter().flatten().collect();
        let drain = t0.elapsed().as_secs_f64();
        let synced: usize = passes.iter().map(|p| p.0).sum();
        let mean_pass = passes.iter().map(|p| p.1).sum::<f64>() / passes.len().max(1) as f64;
        let failed = count(
            admin,
            "select count(*) from public.connections where last_error is not null",
        )
        .await;
        println!(
            "| {machines} | {due} | {synced} | {} | {mean_pass:.2} | {drain:.1} | {:.0} | {failed} | {} |",
            passes.len(),
            synced as f64 / drain * 60.0,
            fakes.composio.tool_calls.load(Ordering::Relaxed) - calls0
        );
        if machines == 1 {
            let per_day_tick = 20.0 * 86_400.0 / mean_pass.max(300.0);
            let per_day_free = 20.0 * 86_400.0 / mean_pass.max(0.001);
            let demand_h = cfg.users as f64 * 2.0 * 2.0;
            println!(
                "  capacity per machine with the 300 s tick: {per_day_tick:.0} syncs/day ({:.0}/h); back-to-back passes: {per_day_free:.0}/day; demand of {} active users (2 conns / 30 min): {demand_h:.0}/h ({:.0}/day if active 8 h/day + daily syncs)",
                per_day_tick / 24.0,
                cfg.users,
                demand_h * 8.0 + cfg.users as f64 * 2.0
            );
        }
    }
}

async fn gateway_overhead(s: &Server, fakes: &Fakes, users: &[Uuid], admin: &Pg) {
    let db = s.state.db.clone();
    admin
        .batch_execute("truncate public.usage_events")
        .await
        .unwrap();
    fakes.set_latency(0, 0);
    let gw = fakes.gateway(false);
    let req = || LlmRequest {
        model: String::new(),
        system: "s".into(),
        messages: vec![(Role::User, "q".into())],
        max_tokens: 800,
    };
    // Sequential: each part alone, then complete() (zero-latency LLM: all of it is DB).
    let mut parts: Vec<(&str, Lat)> = vec![
        ("month_cost_micros (quota check)", Lat::default()),
        ("record_usage (insert)", Lat::default()),
        ("Gateway::choose + complete, 1 caller", Lat::default()),
    ];
    let allowance = Allowance::from(catalog().get(PLAN).expect("plan"));
    for i in 0..300 {
        let u = users[i % users.len()];
        let t = Instant::now();
        db.month_cost_micros(u).await.unwrap();
        parts[0].1.us.push(t.elapsed().as_micros() as u64);
        let t = Instant::now();
        db.record_usage(u, UsageKind::Llm, "x", 1, 1, 0)
            .await
            .unwrap();
        parts[1].1.us.push(t.elapsed().as_micros() as u64);
        let t = Instant::now();
        let choice = gw.choose(&db, u, allowance, None).await.unwrap();
        gw.complete(&db, u, choice.tier, req()).await.unwrap();
        parts[2].1.us.push(t.elapsed().as_micros() as u64);
    }
    for (name, mut l) in parts {
        println!("| {name} | {} | {} | - |", l.us.len(), l.summary_prec(2));
    }
    admin
        .batch_execute("truncate public.usage_events")
        .await
        .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let mut set = tokio::task::JoinSet::new();
    for &u in users {
        let (gw, db) = (gw.clone(), db.clone());
        set.spawn(async move {
            let mut l = Lat::default();
            while Instant::now() < until {
                let t = Instant::now();
                // The fast model with no quota check: metering overhead only.
                let r = gw
                    .complete(
                        &db,
                        u,
                        ModelTier::Fast,
                        LlmRequest {
                            model: String::new(),
                            system: "s".into(),
                            messages: vec![(Role::User, "q".into())],
                            max_tokens: 800,
                        },
                    )
                    .await;
                l.us.push(t.elapsed().as_micros() as u64);
                let code = match r {
                    Ok(_) => 200,
                    Err(GatewayError::QuotaExceeded) => 429,
                    Err(_) => 500,
                };
                *l.status.entry(code).or_default() += 1;
            }
            l
        });
    }
    let mut lat = Lat::default();
    for l in set.join_all().await {
        lat.merge(l);
    }
    let n = lat.us.len();
    println!(
        "| Gateway::complete, {} callers, {:.0} calls/s | {n} | {} | {} |",
        users.len(),
        n as f64 / 10.0,
        lat.summary_prec(2),
        lat.errors()
    );
}

async fn quota_race(
    cfg: &Cfg,
    http: &reqwest::Client,
    s: &Server,
    fakes: &Fakes,
    admin: &Pg,
    label: &str,
) {
    fakes.set_latency(cfg.llm_ms, cfg.embed_ms);
    let user = Uuid::new_v4();
    admin
        .execute(
            "insert into auth.users (id, email) values ($1, 'race@example.com')",
            &[&user],
        )
        .await
        .unwrap();
    subscribe(admin, &[user]).await;
    let quota = Allowance::from(catalog().get(PLAN).expect("plan")).hard_cap_micros;
    let headroom = 500i64;
    admin
        .execute(
            "insert into public.usage_events (user_id, kind, model, input_tokens, output_tokens, cost_micros)
             values ($1, 'llm', 'seed', 0, 0, $2)",
            &[&user, &(quota - headroom)],
        )
        .await
        .unwrap();
    let mut set = tokio::task::JoinSet::new();
    for i in 0..cfg.race {
        let (http, base) = (http.clone(), s.base.clone());
        set.spawn(async move {
            call(
                &http,
                reqwest::Method::POST,
                format!("{base}/v1/ask"),
                user,
                Some(json!({"question": format!("race question {i}")})),
            )
            .await
            .0
        });
    }
    let mut st: BTreeMap<u16, usize> = BTreeMap::new();
    for code in set.join_all().await {
        *st.entry(code).or_default() += 1;
    }
    let after = s.state.db.month_cost_micros(user).await.unwrap();
    println!(
        "  quota race ({label}): free quota {quota} µUSD, headroom {headroom} µUSD (< one ask ≈ 1,040 µUSD), {} concurrent asks → statuses {st:?}; month cost after {after} µUSD = {:.1}% of quota (overshoot {} µUSD ≈ {:.0} extra asks)",
        cfg.race,
        100.0 * after as f64 / quota as f64,
        after - quota,
        (after - quota) as f64 / 1040.0
    );
}

// ---- main --------------------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cfg = Cfg::from_env();
    let http = reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(1000)
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();
    println!(
        "loadtest: {} users, Ask {}s (ceiling {}s), LLM {} ms ({}/{} tok), embed {} ms, Composio {} ms, Gmail {}×{} msgs of {} B, {} events, incr {}",
        cfg.users,
        cfg.duration.as_secs(),
        cfg.ceil.as_secs(),
        cfg.llm_ms,
        cfg.llm_in,
        cfg.llm_out,
        cfg.embed_ms,
        cfg.composio_ms,
        cfg.pages,
        cfg.msgs,
        cfg.msg_bytes,
        cfg.events,
        cfg.incr
    );
    for (mi, &embed) in cfg.modes.iter().enumerate() {
        let last = mi + 1 == cfg.modes.len();
        println!("\n=== embeddings {} ===", if embed { "ON" } else { "OFF" });
        let (kv, name, admin) = fresh_db(&cfg.url).await;
        let users: Vec<Uuid> = (0..cfg.users).map(|_| Uuid::new_v4()).collect();
        admin
            .execute(
                "insert into auth.users (id, email) select u, u::text || '@example.com' from unnest($1::uuid[]) u",
                &[&users],
            )
            .await
            .unwrap();
        subscribe(&admin, &users).await;
        let fakes = Fakes::new(&cfg);
        let main_srv = serve(&kv, cfg.pools[0], &fakes, embed, Limits::unlimited()).await;
        println!("seeding:");
        seed(&http, &main_srv, &users, &admin).await;
        if embed {
            vector_check(&main_srv, &fakes, &users, &admin).await;
        }
        if std::env::var("LT_ONLY_SEED").is_ok() {
            main_srv.task.abort();
            drop_db(&cfg.url, &name).await;
            continue;
        }
        println!(
            "\n| Ask run | requests | req/s | p50 / p95 / p99 / max ms | overhead p50 / p99 ms | errors | answers with citations | pool max used / saturated | DB tx/ask |"
        );
        println!("|---|---|---|---|---|---|---|---|---|");
        let model_ms = cfg.llm_ms + if embed { cfg.embed_ms } else { 0 };
        for (ceil, dur) in [(false, cfg.duration), (true, cfg.ceil)] {
            if dur.is_zero() {
                continue;
            }
            for &pool in &cfg.pools {
                admin
                    .batch_execute("truncate public.usage_events")
                    .await
                    .unwrap();
                let srv = if pool == cfg.pools[0] {
                    None
                } else {
                    Some(serve(&kv, pool, &fakes, embed, Limits::unlimited()).await)
                };
                let s = srv.as_ref().unwrap_or(&main_srv);
                if ceil {
                    fakes.set_latency(0, 0);
                } else {
                    fakes.set_latency(cfg.llm_ms, cfg.embed_ms);
                }
                let label = format!(
                    "{} users, pool {pool}, {}",
                    users.len(),
                    if ceil {
                        "zero-latency fakes".to_owned()
                    } else {
                        format!("LLM {} ms", cfg.llm_ms)
                    }
                );
                ask_run(
                    &admin,
                    &http,
                    s,
                    &users,
                    dur,
                    &label,
                    if ceil { 0 } else { model_ms },
                )
                .await;
                if let Some(s) = srv {
                    s.task.abort();
                }
            }
        }
        fakes.set_latency(cfg.llm_ms, cfg.embed_ms);
        if last {
            println!(
                "\nscheduler (Ingest::run_due(20) back to back, SYNC_CONCURRENCY 4, Composio {} ms):",
                cfg.composio_ms
            );
            println!(
                "| machines | due | synced | passes | mean pass s | drain s | conns/min | failed | tool calls |"
            );
            println!("|---|---|---|---|---|---|---|---|---|");
            scheduler(&cfg, &main_srv, &fakes, &admin).await;
            println!(
                "\ngateway DB overhead (zero-latency LLM, pool {}):",
                cfg.pools[0]
            );
            println!("| what | calls | p50 / p95 / p99 / max ms | errors |");
            println!("|---|---|---|---|");
            gateway_overhead(&main_srv, &fakes, &users, &admin).await;
            quota_race(&cfg, &http, &main_srv, &fakes, &admin, "no rate limits").await;
            let limited = serve(&kv, cfg.pools[0], &fakes, embed, Limits::default()).await;
            quota_race(&cfg, &http, &limited, &fakes, &admin, "default rate limits").await;
            limited.task.abort();
        }
        main_srv.task.abort();
        drop(main_srv);
        drop(admin);
        if cfg.keep_db {
            println!("kept database {name}");
        } else {
            drop_db(&cfg.url, &name).await;
        }
    }
}
