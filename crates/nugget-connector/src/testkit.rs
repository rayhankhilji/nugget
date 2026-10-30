//! Conformance test kit for connectors (feature `testkit`, dev-only).
//!
//! Every connector crate runs the same robustness properties against a mock server
//! (`wiremock`): paging, throttling, token refresh, deletes and moves, deltas, size caps,
//! unicode, partial failure and crash resumption. This module holds the parts that are the same
//! for every connector:
//!
//! - in-memory host fakes: [`VecEgress`], [`MemSecrets`], [`Replica`] (a miniature index that
//!   is also the connector's [`KnownItems`]) and [`ReplicaSink`] (applies changes to a replica,
//!   can simulate a crash at any send or checkpoint);
//! - [`assert_crash_resumable`]: run a pass once cleanly, then crash it at every point and
//!   resume from the last committed cursor, and require the same final index each time;
//! - [`Sequence`]: a wiremock responder that replays scripted responses (429 → 503 → 200);
//! - fixture hygiene: [`scrub_json`] / [`assert_scrubbed`] for recorded responses;
//! - edge-case generators: [`UNICODE_SAMPLES`], [`html_mail`], [`big_text`].

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use serde_json::Value;

use crate::{
    CancellationToken, Change, ChangeSink, Connector, ConnectorContext, Content, Document,
    KnownItems, SecretStore,
};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Host fakes
// ---------------------------------------------------------------------------------------------

/// Records every egress row.
#[derive(Default)]
pub struct VecEgress(pub Mutex<Vec<EgressEvent>>);

impl EgressSink for VecEgress {
    fn record(&self, e: EgressEvent) {
        lock(&self.0).push(e);
    }
}

impl VecEgress {
    pub fn events(&self) -> Vec<EgressEvent> {
        lock(&self.0).clone()
    }

    /// Every row carries `feature`, and there is one row per request the mock server saw.
    pub fn assert_one_row_per_request(&self, requests: usize, feature: &str) {
        let events = lock(&self.0);
        assert_eq!(
            events.len(),
            requests,
            "one egress row per request (feature {feature})"
        );
        for e in events.iter() {
            assert_eq!(e.feature, feature, "egress feature");
        }
    }
}

/// An in-memory keychain.
#[derive(Default)]
pub struct MemSecrets(pub Mutex<HashMap<String, String>>);

impl SecretStore for MemSecrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(lock(&self.0).get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        lock(&self.0).insert(account.into(), value.into());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<()> {
        lock(&self.0).remove(account);
        Ok(())
    }
}

/// What the replica keeps per item: enough to compare two runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaItem {
    pub version: Option<String>,
    pub title: Option<String>,
    pub mime: String,
    /// Text content, or `bytes:<len>` / `file:<len>` for binary content.
    pub content: String,
    pub url: Option<String>,
}

impl ReplicaItem {
    fn of(d: &Document) -> Self {
        let content = match &d.content {
            Content::Text(t) => t.clone(),
            Content::Bytes(b) => match std::str::from_utf8(b) {
                Ok(s) => s.to_owned(),
                Err(_) => format!("bytes:{}", b.len()),
            },
            Content::File(p) => format!(
                "file:{}",
                std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
            ),
        };
        Self {
            version: d.version.clone(),
            title: d.title.clone(),
            mime: d.mime.clone(),
            content,
            url: d.url.clone(),
        }
    }
}

/// A miniature index: the state ingest would hold after applying a connector's changes. It is
/// also the connector's [`KnownItems`], so reconciling passes see what was really applied.
#[derive(Default)]
pub struct Replica {
    items: Mutex<BTreeMap<ItemKey, ReplicaItem>>,
}

impl Replica {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn apply(&self, change: &Change) {
        let mut items = lock(&self.items);
        match change {
            Change::Upsert(d) => {
                items.insert(d.key.clone(), ReplicaItem::of(d));
            }
            Change::Delete(k) => {
                items.remove(k);
            }
        }
    }

    pub fn snapshot(&self) -> BTreeMap<ItemKey, ReplicaItem> {
        lock(&self.items).clone()
    }

    pub fn restore(&self, snap: BTreeMap<ItemKey, ReplicaItem>) {
        *lock(&self.items) = snap;
    }

    pub fn keys(&self) -> Vec<String> {
        lock(&self.items).keys().map(|k| k.to_string()).collect()
    }

    pub fn get(&self, key: &str) -> Option<ReplicaItem> {
        lock(&self.items).get(&ItemKey::new(key)).cloned()
    }

    pub fn len(&self) -> usize {
        lock(&self.items).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl KnownItems for Replica {
    async fn versions(&self) -> Result<HashMap<ItemKey, Option<String>>> {
        Ok(lock(&self.items)
            .iter()
            .map(|(k, v)| (k.clone(), v.version.clone()))
            .collect())
    }
}

/// Error text of a simulated crash.
pub const CRASH: &str = "simulated crash";

/// A [`ChangeSink`] that applies changes to a [`Replica`] and records them. It can simulate a
/// process crash at the `n`th send or checkpoint (0-based): that call and every later call
/// fail, as if the process died there.
pub struct ReplicaSink {
    pub replica: Arc<Replica>,
    pub changes: Mutex<Vec<Change>>,
    pub checkpoints: Mutex<Vec<Value>>,
    fail_send_at: Option<usize>,
    fail_checkpoint_at: Option<usize>,
    sends: AtomicUsize,
    cps: AtomicUsize,
    crashed: std::sync::atomic::AtomicBool,
}

impl ReplicaSink {
    pub fn new(replica: Arc<Replica>) -> Self {
        Self {
            replica,
            changes: Mutex::default(),
            checkpoints: Mutex::default(),
            fail_send_at: None,
            fail_checkpoint_at: None,
            sends: AtomicUsize::new(0),
            cps: AtomicUsize::new(0),
            crashed: false.into(),
        }
    }

    pub fn crash_at_send(mut self, n: usize) -> Self {
        self.fail_send_at = Some(n);
        self
    }

    pub fn crash_at_checkpoint(mut self, n: usize) -> Self {
        self.fail_checkpoint_at = Some(n);
        self
    }

    /// The last committed cursor.
    pub fn committed(&self) -> Option<Value> {
        lock(&self.checkpoints).last().cloned()
    }

    pub fn crashed(&self) -> bool {
        self.crashed.load(Ordering::SeqCst)
    }

    /// `+key` for upserts, `-key` for deletes, in order.
    pub fn summary(&self) -> Vec<String> {
        lock(&self.changes)
            .iter()
            .map(|c| match c {
                Change::Upsert(d) => format!("+{}", d.key),
                Change::Delete(k) => format!("-{k}"),
            })
            .collect()
    }

    pub fn upserts(&self) -> Vec<Document> {
        lock(&self.changes)
            .iter()
            .filter_map(|c| match c {
                Change::Upsert(d) => Some(d.clone()),
                Change::Delete(_) => None,
            })
            .collect()
    }

    pub fn sends(&self) -> usize {
        self.sends.load(Ordering::SeqCst)
    }

    pub fn checkpoint_calls(&self) -> usize {
        self.cps.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ChangeSink for ReplicaSink {
    async fn send(&self, change: Change) -> Result<()> {
        let n = self.sends.fetch_add(1, Ordering::SeqCst);
        if self.crashed() || self.fail_send_at.is_some_and(|f| n >= f) {
            self.crashed.store(true, Ordering::SeqCst);
            return Err(Error::Other(CRASH.into()));
        }
        self.replica.apply(&change);
        lock(&self.changes).push(change);
        Ok(())
    }

    async fn checkpoint(&self, cursor: Value) -> Result<()> {
        let n = self.cps.fetch_add(1, Ordering::SeqCst);
        if self.crashed() || self.fail_checkpoint_at.is_some_and(|f| n >= f) {
            self.crashed.store(true, Ordering::SeqCst);
            return Err(Error::Other(CRASH.into()));
        }
        lock(&self.checkpoints).push(cursor);
        Ok(())
    }
}

/// A connector context over the given fakes.
pub fn context(
    source: &str,
    paths: Paths,
    egress: Arc<dyn EgressSink>,
    secrets: Arc<dyn SecretStore>,
    known: Arc<dyn KnownItems>,
) -> ConnectorContext {
    ConnectorContext {
        source: SourceId::new(source),
        paths,
        egress,
        secrets,
        known,
    }
}

/// Run one pass the way ingest does: the returned cursor is committed after the pass.
pub async fn run_pass(
    conn: &dyn Connector,
    cursor: Option<Value>,
    sink: &ReplicaSink,
) -> Result<Value> {
    let next = conn.sync(cursor, sink, &CancellationToken::new()).await?;
    sink.checkpoint(next.clone()).await?;
    Ok(next)
}

/// Result of [`assert_crash_resumable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReport {
    /// Sends in the clean pass (each one a crash point).
    pub sends: usize,
    /// Checkpoints in the clean pass, including the final commit.
    pub checkpoints: usize,
    /// Crash-and-resume runs performed.
    pub runs: usize,
}

/// Crash a pass at every send and every checkpoint, resume from the last committed cursor
/// (as ingest does after a restart), and require the same final index as a clean run.
///
/// `build` makes a fresh connector over the given replica (use it as the context's
/// [`KnownItems`]); it is called once per run, like a daemon restart. `start` is the committed
/// cursor before the pass, `seed` the index content at that point. The mock server must answer
/// the same way on every run (stateless mocks). At most `max_resumes` resumed passes are allowed
/// per crash point before the run is declared stuck.
pub async fn assert_crash_resumable<F, Fut>(
    build: F,
    start: Option<Value>,
    seed: BTreeMap<ItemKey, ReplicaItem>,
) -> CrashReport
where
    F: Fn(Arc<Replica>) -> Fut,
    Fut: Future<Output = Arc<dyn Connector>>,
{
    const MAX_RESUMES: usize = 4;

    // Clean reference run.
    let clean = Replica::new();
    clean.restore(seed.clone());
    let conn = build(clean.clone()).await;
    let sink = ReplicaSink::new(clean.clone());
    run_pass(&*conn, start.clone(), &sink)
        .await
        .expect("clean pass");
    let want = clean.snapshot();
    let (sends, checkpoints) = (sink.sends(), sink.checkpoint_calls());

    let mut runs = 0;
    let points = (0..sends)
        .map(|n| (Some(n), None))
        .chain((0..checkpoints).map(|n| (None, Some(n))));
    for (send_at, cp_at) in points {
        let replica = Replica::new();
        replica.restore(seed.clone());
        let mut sink = ReplicaSink::new(replica.clone());
        if let Some(n) = send_at {
            sink = sink.crash_at_send(n);
        }
        if let Some(n) = cp_at {
            sink = sink.crash_at_checkpoint(n);
        }
        let conn = build(replica.clone()).await;
        let r = run_pass(&*conn, start.clone(), &sink).await;
        assert!(
            r.is_err() && sink.crashed(),
            "crash at send {send_at:?} / checkpoint {cp_at:?} did not stop the pass: {r:?}"
        );
        drop(conn);
        let mut cursor = sink.committed().or(start.clone());
        let mut done = false;
        for _ in 0..MAX_RESUMES {
            runs += 1;
            let conn = build(replica.clone()).await;
            let resume = ReplicaSink::new(replica.clone());
            match run_pass(&*conn, cursor.clone(), &resume).await {
                Ok(_) => {
                    done = true;
                    break;
                }
                Err(e) => {
                    cursor = resume.committed().or(cursor);
                    eprintln!("resume after crash failed, retrying: {e}");
                }
            }
        }
        assert!(
            done,
            "resume after crash at send {send_at:?} / checkpoint {cp_at:?} never finished"
        );
        assert_eq!(
            replica.snapshot(),
            want,
            "index after crash at send {send_at:?} / checkpoint {cp_at:?} + resume differs from a clean run"
        );
    }
    CrashReport {
        sends,
        checkpoints,
        runs,
    }
}

// ---------------------------------------------------------------------------------------------
// Mock server helpers
// ---------------------------------------------------------------------------------------------

/// A wiremock responder that returns `steps` in order, then repeats the last one. Use it for
/// "429 with Retry-After, then 503, then success" scripts on one route.
pub struct Sequence {
    steps: Vec<wiremock::ResponseTemplate>,
    at: AtomicUsize,
}

impl Sequence {
    pub fn new(steps: Vec<wiremock::ResponseTemplate>) -> Self {
        assert!(!steps.is_empty(), "a sequence needs at least one response");
        Self {
            steps,
            at: AtomicUsize::new(0),
        }
    }

    /// How many requests it answered.
    pub fn calls(&self) -> usize {
        self.at.load(Ordering::SeqCst)
    }
}

impl wiremock::Respond for Sequence {
    fn respond(&self, _: &wiremock::Request) -> wiremock::ResponseTemplate {
        let i = self.at.fetch_add(1, Ordering::SeqCst);
        self.steps[i.min(self.steps.len() - 1)].clone()
    }
}

/// `429 Too Many Requests` with `Retry-After: <secs>`.
pub fn too_many_requests(secs: u64) -> wiremock::ResponseTemplate {
    wiremock::ResponseTemplate::new(429).insert_header("Retry-After", secs.to_string().as_str())
}

/// A 5xx with an optional `Retry-After`.
pub fn server_error(status: u16, retry_after_secs: Option<u64>) -> wiremock::ResponseTemplate {
    let t = wiremock::ResponseTemplate::new(status).set_body_string("upstream trouble");
    match retry_after_secs {
        Some(s) => t.insert_header("Retry-After", s.to_string().as_str()),
        None => t,
    }
}

// ---------------------------------------------------------------------------------------------
// Fixture hygiene
// ---------------------------------------------------------------------------------------------

const SECRET_KEYS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "client_secret",
    "password",
    "authorization",
    "cookie",
    "api_key",
    "apikey",
    "secret",
];

/// Placeholder written over scrubbed values.
pub const SCRUBBED: &str = "<scrubbed>";

/// Scrub a recorded JSON response in place: values of credential-like keys become
/// [`SCRUBBED`] and e-mail addresses in strings become `user@example.com`.
pub fn scrub_json(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                let lk = k.to_ascii_lowercase();
                if SECRET_KEYS.iter().any(|s| lk == *s || lk.ends_with(&format!("_{s}"))) {
                    *val = Value::String(SCRUBBED.into());
                } else {
                    scrub_json(val);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub_json),
        Value::String(s) => *s = scrub_emails(s),
        _ => {}
    }
}

fn scrub_emails(s: &str) -> String {
    s.split_inclusive(|c: char| c.is_whitespace() || "<>,;\"'()[]".contains(c))
        .map(|tok| {
            let core = tok.trim_end_matches(|c: char| c.is_whitespace() || "<>,;\"'()[]".contains(c));
            let tail = &tok[core.len()..];
            match core.split_once('@') {
                Some((user, host))
                    if !user.is_empty()
                        && host.contains('.')
                        && !host.ends_with("example.com")
                        && !host.ends_with("example.org") =>
                {
                    format!("user@example.com{tail}")
                }
                _ => tok.to_owned(),
            }
        })
        .collect()
}

/// Fail if a recorded fixture still contains a credential-like value or a real-looking e-mail
/// address (anything not on an `example.*` domain).
pub fn assert_scrubbed(fixture: &str) {
    let mut v: Value = match serde_json::from_str(fixture) {
        Ok(v) => v,
        Err(_) => Value::String(fixture.to_owned()),
    };
    let before = v.clone();
    scrub_json(&mut v);
    assert_eq!(before, v, "fixture contains unscrubbed secrets or e-mail addresses");
    for needle in ["ya29.", "xoxb-", "xoxp-", "ghp_", "gho_", "lin_api_", "secret_", "Bearer "] {
        assert!(
            !fixture.contains(needle),
            "fixture contains a token-like value ({needle}...)"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Edge-case generators
// ---------------------------------------------------------------------------------------------

/// Titles and bodies that have broken connectors elsewhere: emoji and ZWJ sequences, RTL,
/// combining marks, CJK, astral planes, a BOM, odd whitespace.
pub const UNICODE_SAMPLES: &[&str] = &[
    "Résumé — naïve café",
    "👩‍👩‍👧‍👦 family 🇯🇵 flag",
    "مرحبا بالعالم",
    "שלום עולם",
    "日本語のタイトル／スラッシュ",
    "Z̤͔ͧ̑̓ä͖̭̈̇lͮ̒ͫǧ̗͚̚o̙̔ͮ̇͐̇",
    "𝔘𝔫𝔦𝔠𝔬𝔡𝔢 𝟙𝟚𝟛",
    "\u{feff}BOM first",
    "tab\tand\u{00a0}nbsp\u{2028}line sep",
];

/// A small HTML mail body with markup that must not leak into indexed text (script, style,
/// comments, entities, a tracking pixel).
pub fn html_mail(text: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><style>p{{color:red}}</style>\
         <script>alert('x')</script></head><body><!-- hidden comment -->\
         <p>{text} &amp; more &lt;ok&gt;</p><img src=\"https://t.example.com/p.gif\" width=1>\
         </body></html>"
    )
}

/// `len` bytes of ASCII text.
pub fn big_text(len: usize) -> String {
    "abcdefghij".repeat(len / 10 + 1)[..len].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_tokens_and_emails() {
        let mut v = serde_json::json!({
            "access_token": "ya29.abc",
            "nested": {"client_secret": "s", "from": "Alice <alice@corp.io>", "x_api_key": "k"},
            "list": ["bob@example.com", "carol@corp.io,"],
        });
        scrub_json(&mut v);
        assert_eq!(v["access_token"], SCRUBBED);
        assert_eq!(v["nested"]["client_secret"], SCRUBBED);
        assert_eq!(v["nested"]["x_api_key"], SCRUBBED);
        assert_eq!(v["nested"]["from"], "Alice <user@example.com>");
        assert_eq!(v["list"][0], "bob@example.com");
        assert_eq!(v["list"][1], "user@example.com,");
        assert_scrubbed(&v.to_string());
    }

    #[test]
    #[should_panic(expected = "unscrubbed")]
    fn unscrubbed_fixture_fails() {
        assert_scrubbed(r#"{"refresh_token": "1//abc"}"#);
    }

    #[tokio::test]
    async fn replica_sink_crashes_and_stays_crashed() {
        let r = Replica::new();
        let s = ReplicaSink::new(r.clone()).crash_at_send(1);
        let doc = |k: &str| Change::Upsert(Document::new(k, "text/plain", Content::Text(k.into())));
        s.send(doc("a")).await.unwrap();
        assert!(s.send(doc("b")).await.is_err());
        assert!(s.checkpoint(Value::Null).await.is_err());
        assert_eq!(r.keys(), vec!["a"]);
        assert_eq!(s.committed(), None);
    }

    /// A toy connector over a fixed list, checkpointing after every item, proves the matrix.
    struct Toy(Arc<Replica>);
    #[async_trait]
    impl Connector for Toy {
        fn kind(&self) -> &'static str {
            "toy"
        }
        async fn sync(
            &self,
            cursor: Option<Value>,
            sink: &dyn ChangeSink,
            _: &CancellationToken,
        ) -> Result<Value> {
            let from = cursor.and_then(|c| c.as_u64()).unwrap_or(0) as usize;
            let items = ["a", "b", "c", "d"];
            for (i, k) in items.iter().enumerate().skip(from) {
                let mut d = Document::new(*k, "text/plain", Content::Text(k.to_string()));
                d.version = Some("1".into());
                sink.send(Change::Upsert(d)).await?;
                sink.checkpoint(Value::from(i as u64 + 1)).await?;
            }
            // Reconcile: drop what is gone (also on a resumed pass, or a crash would leak it).
            for k in self.0.versions().await?.keys() {
                if !items.contains(&k.as_str()) {
                    sink.send(Change::Delete(k.clone())).await?;
                }
            }
            Ok(Value::from(items.len() as u64))
        }
    }

    #[tokio::test]
    async fn crash_matrix_on_a_toy_connector() {
        let seed = Replica::new();
        seed.apply(&Change::Upsert(Document::new(
            "stale",
            "text/plain",
            Content::Text("x".into()),
        )));
        let report = assert_crash_resumable(
            |r| async move { Arc::new(Toy(r)) as Arc<dyn Connector> },
            None,
            seed.snapshot(),
        )
        .await;
        assert_eq!(report.sends, 5);
        assert_eq!(report.checkpoints, 5);
    }
}
