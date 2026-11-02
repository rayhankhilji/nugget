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
    /// Delay per tool call (slow-backfill tests).
    pub delay_ms: std::sync::atomic::AtomicU64,
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
        let toolkit = if auth_config == "ac_gmail" {
            "gmail"
        } else {
            "googlecalendar"
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
    async fn execute(
        &self,
        user: &str,
        account: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value, ComposioError> {
        let delay = self.delay_ms.load(std::sync::atomic::Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        self.calls
            .lock()
            .unwrap()
            .push((user.into(), tool.into(), args));
        if !account.starts_with(&format!("ca_{}", &user[..8])) {
            return Err(ComposioError::Tool {
                tool: tool.into(),
                message: "account not owned".into(),
            });
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

/// A superuser connection to a test database.
pub type Pg = Arc<tokio_postgres::Client>;

async fn pg(url: &str) -> Pg {
    let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .expect("connect");
    tokio::spawn(conn);
    Arc::new(client)
}

/// A fresh database with the shim and migrations, or None (test skipped) when
/// `NUGGET_TEST_DATABASE_URL` (a superuser URL, `sslmode=disable`) is not set.
pub async fn test_db() -> Option<(Db, Pg)> {
    let Ok(url) = std::env::var("NUGGET_TEST_DATABASE_URL") else {
        eprintln!("skipping: NUGGET_TEST_DATABASE_URL is not set");
        return None;
    };
    static SETUP: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    // Roles are cluster-wide; serialise setup so concurrent CREATE ROLEs don't race.
    let _guard = SETUP
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let mut cfg: tokio_postgres::Config = url.parse().expect("database url");
    let admin = pg(&url).await;
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
    let client = Arc::new(client);
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
