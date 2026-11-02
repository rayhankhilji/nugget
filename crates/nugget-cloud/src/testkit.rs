//! Test doubles and a throwaway Postgres database with the real migrations applied.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::{AuthError, AuthProvider, AuthUser};
use crate::composio::{Account, Composio, ComposioError, Link, Toolkit};
use crate::db::Db;
use crate::gateway::{Embedder, GatewayError, Llm, LlmRequest, LlmResponse};

/// Tokens are `tok-<uuid>`.
pub struct FakeAuth {
    pub deleted: Mutex<Vec<Uuid>>,
}

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
    async fn delete_user(&self, id: Uuid) -> Result<(), AuthError> {
        self.deleted.lock().unwrap().push(id);
        Ok(())
    }
}

/// Composio with in-memory accounts; links become ACTIVE immediately (as after consent).
#[derive(Default)]
pub struct FakeComposio {
    /// user → accounts
    pub accounts: Mutex<HashMap<String, Vec<Account>>>,
    /// tool → response `data`
    pub data: Mutex<HashMap<String, Value>>,
    pub calls: Mutex<Vec<(String, String, Value)>>,
    pub deleted: Mutex<Vec<String>>,
    /// (user, account, trigger slug) of enabled triggers.
    pub triggers: Mutex<Vec<(String, String, String)>>,
    /// Scripted responses, checked before `data`: (tool, required argument, response).
    pub script: Mutex<Vec<Scripted>>,
    /// Tools that fail with this error.
    pub fail: Mutex<HashMap<String, String>>,
    /// Delay per tool call (slow-backfill tests).
    pub delay_ms: std::sync::atomic::AtomicU64,
    /// When set, tool calls wait here until the test adds a permit (deterministic "slow" calls).
    pub gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    /// Tool calls that have started (including those waiting at the gate).
    pub entered: std::sync::atomic::AtomicUsize,
}

/// A recorded response for `tool`, used when the call's arguments contain `when` (every key
/// with an equal value; `None` matches any call).
#[derive(Clone)]
pub struct Scripted {
    pub tool: String,
    pub when: Option<Value>,
    pub data: Value,
}

impl Scripted {
    pub fn matches(&self, tool: &str, args: &Value) -> bool {
        self.tool == tool
            && match &self.when {
                None => true,
                Some(Value::Object(want)) => want.iter().all(|(k, v)| args.get(k) == Some(v)),
                Some(_) => false,
            }
    }
}

/// The most specific scripted response for a call (most keys matched).
pub fn scripted(script: &[Scripted], tool: &str, args: &Value) -> Option<Value> {
    script
        .iter()
        .filter(|s| s.matches(tool, args))
        .max_by_key(|s| {
            s.when
                .as_ref()
                .and_then(Value::as_object)
                .map_or(0, |m| m.len())
        })
        .map(|s| s.data.clone())
}

#[async_trait]
impl Composio for FakeComposio {
    async fn toolkits(&self) -> Result<Vec<Toolkit>, ComposioError> {
        Ok(vec![
            Toolkit {
                slug: "slack".into(),
                name: "Slack".into(),
                logo: None,
            },
            Toolkit {
                slug: "gmail".into(),
                name: "Gmail".into(),
                logo: None,
            },
            Toolkit {
                slug: "googlecalendar".into(),
                name: "Google Calendar".into(),
                logo: None,
            },
        ])
    }
    async fn create_link(
        &self,
        user: &str,
        auth_config: &str,
        _cb: &str,
    ) -> Result<Link, ComposioError> {
        // `ac_<toolkit>`; `ac_cal` is Google Calendar (as in the M1 tests).
        let toolkit = match auth_config {
            "ac_cal" => "googlecalendar",
            other => other.strip_prefix("ac_").unwrap_or(other),
        };
        let id = format!("ca_{}_{toolkit}", &user[..8]);
        self.accounts
            .lock()
            .unwrap()
            .entry(user.into())
            .or_default()
            .push(Account {
                id: id.clone(),
                toolkit: toolkit.into(),
                status: "ACTIVE".into(),
            });
        Ok(Link {
            connected_account_id: id,
            redirect_url: format!("https://connect.example/{auth_config}"),
        })
    }
    async fn accounts(&self, user: &str) -> Result<Vec<Account>, ComposioError> {
        Ok(self
            .accounts
            .lock()
            .unwrap()
            .get(user)
            .cloned()
            .unwrap_or_default())
    }
    async fn delete_account(&self, id: &str) -> Result<(), ComposioError> {
        self.deleted.lock().unwrap().push(id.into());
        for list in self.accounts.lock().unwrap().values_mut() {
            list.retain(|a| a.id != id);
        }
        Ok(())
    }
    async fn upsert_trigger(
        &self,
        user: &str,
        account: &str,
        trigger: &str,
        _config: Value,
    ) -> Result<String, ComposioError> {
        let mut t = self.triggers.lock().unwrap();
        t.push((user.into(), account.into(), trigger.into()));
        Ok(format!("ti_{}", t.len()))
    }
    async fn execute(
        &self,
        user: &str,
        account: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value, ComposioError> {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            // Returned right away: once the test opens the gate, every call passes.
            drop(gate.acquire().await);
        }
        let delay = self.delay_ms.load(std::sync::atomic::Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        self.calls
            .lock()
            .unwrap()
            .push((user.into(), tool.into(), args.clone()));
        if let Some(m) = self.fail.lock().unwrap().get(tool) {
            return Err(ComposioError::Tool {
                tool: tool.into(),
                message: m.clone(),
            });
        }
        if !account.starts_with(&format!("ca_{}", &user[..8])) {
            return Err(ComposioError::Tool {
                tool: tool.into(),
                message: "account not owned".into(),
            });
        }
        if let Some(v) = scripted(&self.script.lock().unwrap(), tool, &args) {
            return Ok(v);
        }
        Ok(self
            .data
            .lock()
            .unwrap()
            .get(tool)
            .cloned()
            .unwrap_or(json!({})))
    }
}

/// Cites source 1 when there is one, and says it doesn't know otherwise.
pub struct FakeLlm {
    pub prompts: Mutex<Vec<String>>,
}

#[async_trait]
impl Llm for FakeLlm {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, GatewayError> {
        let prompt = req.messages.last().map(|m| m.1.clone()).unwrap_or_default();
        self.prompts.lock().unwrap().push(prompt.clone());
        let text = if prompt.contains("<source n=\"1\"") {
            "Your lease renews on 1 December [1].".to_owned()
        } else {
            "I couldn't find that in your mail or calendar.".to_owned()
        };
        Ok(LlmResponse {
            text,
            input_tokens: 1000,
            output_tokens: 50,
        })
    }
}

/// Deterministic bag-of-words vectors, so similar texts are near each other.
#[derive(Default)]
pub struct FakeEmbedder {
    /// Provider calls made (batching check).
    pub calls: std::sync::atomic::AtomicUsize,
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
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let vecs = texts
            .iter()
            .map(|t| {
                let mut v = vec![0f32; 1536];
                for w in t
                    .to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.len() > 2)
                {
                    let h = w
                        .bytes()
                        .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
                    v[(h % 1536) as usize] += 1.0;
                }
                v[0] += 0.01; // never all zeros
                v
            })
            .collect();
        Ok((vecs, texts.iter().map(|t| t.len() as u32 / 4).sum()))
    }
}

/// A superuser connection to one test's database. When the last clone is dropped (the test ended,
/// passed or panicked), the database is dropped too, so test runs leave nothing behind.
pub type Pg = Arc<TestDb>;

pub struct TestDb {
    client: tokio_postgres::Client,
    /// The cluster's admin URL and this database's name, for the drop.
    admin_url: String,
    name: String,
}

impl TestDb {
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl std::ops::Deref for TestDb {
    type Target = tokio_postgres::Client;
    fn deref(&self) -> &tokio_postgres::Client {
        &self.client
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // Drop runs inside the test's runtime (or while unwinding from a panic), where blocking on
        // async work is not allowed: do it on a plain thread with its own small runtime, and wait,
        // so the database is gone before the next test starts. FORCE ends the pool's connections.
        let (url, name) = (self.admin_url.clone(), self.name.clone());
        let done = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async {
                let (admin, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
                    .await
                    .ok()?;
                tokio::spawn(conn);
                admin
                    .batch_execute(&format!("drop database if exists {name} with (force)"))
                    .await
                    .ok()
            })
        })
        .join();
        if !matches!(done, Ok(Some(()))) {
            eprintln!(
                "testkit: could not drop {}; the next run's sweep will",
                self.name
            );
        }
    }
}

/// Test databases are named `nugget_t_<uuid>`. `_` is a LIKE wildcard, hence the escapes: the
/// pattern must not match the RLS suite's `nugget_test`.
const TEST_DB_LIKE: &str = r"nugget\_t\_%";

/// Drops test databases left by crashed or killed runs: those created over an hour ago (the
/// age of their `PG_VERSION` file), so concurrent runs keep theirs.
async fn sweep_stale(admin: &tokio_postgres::Client) {
    let stale = admin
        .query(
            "select datname from pg_database
             where datname like $1
               and (pg_stat_file('base/' || oid || '/PG_VERSION')).modification < now() - interval '1 hour'",
            &[&TEST_DB_LIKE],
        )
        .await
        .unwrap_or_default();
    for row in stale {
        let name: String = row.get(0);
        if name.starts_with("nugget_t_") && name[9..].chars().all(|c| c.is_ascii_hexdigit()) {
            let _ = admin
                .batch_execute(&format!("drop database if exists {name} with (force)"))
                .await;
        }
    }
}

/// A fresh database with the shim and migrations, or None (test skipped) when
/// `NUGGET_TEST_DATABASE_URL` (a superuser URL, `sslmode=disable`) is not set.
pub async fn test_db() -> Option<(Db, Pg)> {
    let Ok(url) = std::env::var("NUGGET_TEST_DATABASE_URL") else {
        eprintln!("skipping: NUGGET_TEST_DATABASE_URL is not set");
        return None;
    };
    static SETUP: OnceLock<tokio::sync::Mutex<bool>> = OnceLock::new();
    // Roles are cluster-wide; serialise setup so concurrent CREATE ROLEs don't race.
    let mut swept = SETUP
        .get_or_init(|| tokio::sync::Mutex::new(false))
        .lock()
        .await;
    let mut cfg: tokio_postgres::Config = url.parse().expect("database url");
    let (admin, admin_conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .expect("connect");
    tokio::spawn(admin_conn);
    if !*swept {
        sweep_stale(&admin).await;
        *swept = true;
    }
    let name = format!("nugget_t_{}", Uuid::new_v4().simple());
    admin
        .batch_execute(&format!("create database {name}"))
        .await
        .expect("create db");
    cfg.dbname(&name);
    let (client, conn) = cfg
        .connect(tokio_postgres::NoTls)
        .await
        .expect("connect test db");
    tokio::spawn(conn);
    // From here on the guard owns the database: a failed migration still drops it.
    let client = Arc::new(TestDb {
        client,
        admin_url: url.clone(),
        name: name.clone(),
    });
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
    // Same URL, the new database.
    let db_url = format!("{}{}dbname={name}", url_as_kv(&url), " ");
    Some((Db::connect(&db_url, None).expect("pool"), client))
}

/// `postgres://user@host:port/db?sslmode=disable` → key/value form without the dbname, so a
/// `dbname=` can be appended. Only what the tests use: host, port, user, password, sslmode.
fn url_as_kv(url: &str) -> String {
    let cfg: tokio_postgres::Config = url.parse().unwrap();
    let mut kv = Vec::new();
    for h in cfg.get_hosts() {
        match h {
            tokio_postgres::config::Host::Tcp(h) => kv.push(format!("host={h}")),
            tokio_postgres::config::Host::Unix(p) => kv.push(format!("host={}", p.display())),
        }
    }
    if let Some(p) = cfg.get_ports().first() {
        kv.push(format!("port={p}"));
    }
    if let Some(u) = cfg.get_user() {
        kv.push(format!("user={u}"));
    }
    if let Some(pw) = cfg.get_password() {
        kv.push(format!(
            "password='{}'",
            String::from_utf8_lossy(pw).replace('\'', "\\'")
        ));
    }
    kv.push("sslmode=disable".into());
    kv.join(" ")
}

pub async fn add_user(pool: &Pg) -> Uuid {
    let id = Uuid::new_v4();
    pool.execute(
        "insert into auth.users (id, email) values ($1, 'x@example.com')",
        &[&id],
    )
    .await
    .unwrap();
    id
}

/// Polls `check` every 20 ms until it holds, failing after `secs` (generous, for loaded CI).
pub async fn wait_until<F, Fut>(what: &str, secs: u64, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    while !check().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {secs}s waiting for: {what}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

pub fn fakes() -> (Arc<FakeAuth>, Arc<FakeComposio>, Arc<FakeLlm>) {
    (
        Arc::new(FakeAuth {
            deleted: Mutex::new(Vec::new()),
        }),
        Arc::new(FakeComposio::default()),
        Arc::new(FakeLlm {
            prompts: Mutex::new(Vec::new()),
        }),
    )
}

/// Stripe with in-memory subscriptions; records checkouts and subscription fetches.
#[derive(Default)]
pub struct FakeStripe {
    pub subs: Mutex<HashMap<String, crate::stripe::Subscription>>,
    /// (customer, price, trial_days)
    pub checkouts: Mutex<Vec<(String, String, Option<u32>)>>,
    pub customers: Mutex<Vec<String>>,
    pub fetches: Mutex<u32>,
    pub deleted_customers: Mutex<Vec<String>>,
    pub fail_delete: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl crate::stripe::Stripe for FakeStripe {
    async fn create_customer(
        &self,
        _email: Option<&str>,
        user_id: &str,
    ) -> Result<String, crate::stripe::StripeError> {
        let id = format!("cus_{}", &user_id[..8]);
        self.customers.lock().unwrap().push(id.clone());
        Ok(id)
    }
    async fn checkout(
        &self,
        r: &crate::stripe::CheckoutRequest<'_>,
    ) -> Result<String, crate::stripe::StripeError> {
        self.checkouts
            .lock()
            .unwrap()
            .push((r.customer.into(), r.price.into(), r.trial_days));
        Ok(format!("https://checkout.stripe.test/{}", r.price))
    }
    async fn portal(
        &self,
        customer: &str,
        _return_url: &str,
    ) -> Result<String, crate::stripe::StripeError> {
        Ok(format!("https://billing.stripe.test/{customer}"))
    }
    async fn subscription(
        &self,
        id: &str,
    ) -> Result<crate::stripe::Subscription, crate::stripe::StripeError> {
        *self.fetches.lock().unwrap() += 1;
        self.subs
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| crate::stripe::StripeError::Http("no such subscription".into()))
    }
    async fn delete_customer(&self, id: &str) -> Result<(), crate::stripe::StripeError> {
        if self.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::stripe::StripeError::Http(
                "stripe down (secret detail)".into(),
            ));
        }
        self.deleted_customers.lock().unwrap().push(id.into());
        Ok(())
    }
}

/// Test price ids for the default plans.
pub fn test_prices(k: &str) -> Option<String> {
    match k {
        "STRIPE_PRICE_HEED_MONTHLY" => Some("price_hm".into()),
        "STRIPE_PRICE_HEED_ANNUAL" => Some("price_ha".into()),
        "STRIPE_PRICE_HEED_MAX_MONTHLY" => Some("price_xm".into()),
        "STRIPE_PRICE_HEED_MAX_ANNUAL" => Some("price_xa".into()),
        _ => None,
    }
}

/// A user with an active subscription to `plan`, written directly (as a webhook would).
pub async fn subscriber(pool: &Pg, plan: &str) -> Uuid {
    let id = add_user(pool).await;
    pool.execute(
        "insert into public.subscriptions (id, user_id, plan, interval, status, stripe_price_id)
         values ($1, $2, $3, 'month', 'active', 'price_test')",
        &[&format!("sub_{}", id.simple()), &id, &plan],
    )
    .await
    .unwrap();
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn exists(name: &str) -> bool {
        let url = std::env::var("NUGGET_TEST_DATABASE_URL").unwrap();
        let (admin, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(conn);
        admin
            .query_one(
                "select exists(select 1 from pg_database where datname = $1)",
                &[&name],
            )
            .await
            .unwrap()
            .get(0)
    }

    #[tokio::test]
    async fn test_databases_are_dropped_even_when_the_test_panics() {
        let Some((db, pool)) = test_db().await else {
            return;
        };
        let name = pool.name().to_owned();
        assert!(exists(&name).await);
        // A "test" that panics while holding the database (and a pool connection to it).
        let task = tokio::spawn(async move {
            let _db = db;
            let _pool = pool;
            panic!("simulated test failure");
        });
        assert!(task.await.unwrap_err().is_panic());
        assert!(!exists(&name).await, "{name} was left behind");
    }

    #[test]
    fn sweep_pattern_spares_the_rls_suite() {
        // The RLS suite's database must not look like a test database.
        let like = |s: &str| s.starts_with("nugget_t_");
        assert!(like("nugget_t_0123abcd"));
        assert!(!like("nugget_test"));
        assert_eq!(TEST_DB_LIKE, r"nugget\_t\_%");
    }
}
