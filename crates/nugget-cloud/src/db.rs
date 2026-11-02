//! Postgres access. Every per-user query runs inside a transaction from [`Db::scoped`] that
//! sets `role nugget_service` and `nugget.user_id`, so RLS confines it to that user's rows even
//! if a query forgot its `where user_id = …`.

use std::collections::HashMap;

use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod, Transaction};
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_store::Hit;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio_postgres::Row;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Pg(#[from] tokio_postgres::Error),
    #[error(transparent)]
    Pool(#[from] deadpool_postgres::PoolError),
    #[error("database setup: {0}")]
    Setup(String),
}

#[derive(Debug, Clone, Copy)]
pub enum UsageKind {
    Llm,
    Embed,
    Tool,
}

impl UsageKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::Embed => "embed",
            Self::Tool => "tool",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Connection {
    pub id: Uuid,
    pub toolkit: String,
    pub composio_account_id: String,
    pub status: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_synced_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    #[serde(skip)]
    pub sync_cursor: Value,
}

/// One normalised email, event or doc, ready to store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewItem {
    pub source: String,
    pub external_id: String,
    pub kind: &'static str,
    pub title: Option<String>,
    pub author: Option<String>,
    pub url: Option<String>,
    pub occurred_at: Option<OffsetDateTime>,
    pub body: String,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Added,
    Updated,
}

/// Facts about an item for citations and prompts.
#[derive(Debug, Clone)]
pub struct ItemMeta {
    pub id: Uuid,
    pub source: String,
    pub kind: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub url: Option<String>,
    pub occurred_at: Option<OffsetDateTime>,
}

impl Connection {
    fn from_row(r: &Row) -> Self {
        Self {
            id: r.get("id"),
            toolkit: r.get("toolkit"),
            composio_account_id: r.get("composio_account_id"),
            status: r.get("status"),
            last_synced_at: r.get("last_synced_at"),
            last_error: r.get("last_error"),
            sync_cursor: r.get("sync_cursor"),
        }
    }
}

impl ItemMeta {
    fn from_row(r: &Row) -> Self {
        Self {
            id: r.get("id"),
            source: r.get("source"),
            kind: r.get("kind"),
            title: r.get("title"),
            author: r.get("author"),
            url: r.get("url"),
            occurred_at: r.get("occurred_at"),
        }
    }
}

#[derive(Clone)]
pub struct Db {
    pool: Pool,
    statement_timeout_ms: u64,
}

/// Pool limits (`DATABASE_POOL_MAX`, `DATABASE_POOL_TIMEOUT_SECS`,
/// `DATABASE_STATEMENT_TIMEOUT_MS`). Keep `max_size` × machines under the pooler's limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolOptions {
    pub max_size: usize,
    /// How long a request waits for a connection (or to open one) before failing with 503.
    pub timeout_secs: u64,
    /// Per-transaction `statement_timeout`; 0 leaves the server default.
    pub statement_timeout_ms: u64,
}

impl Default for PoolOptions {
    fn default() -> Self {
        Self {
            max_size: 8,
            timeout_secs: 5,
            statement_timeout_ms: 15_000,
        }
    }
}

impl DbError {
    /// The pool had no connection to give in time (overload, or the database is down).
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Pool(
                deadpool_postgres::PoolError::Timeout(_) | deadpool_postgres::PoolError::Backend(_)
            )
        )
    }
}

/// TLS for the database: verify against `DATABASE_CA_CERT` (a PEM file; Supabase's own CA)
/// when given, otherwise the OS trust store.
fn tls(ca_pem: Option<&str>) -> Result<tokio_postgres_rustls::MakeRustlsConnect, DbError> {
    use rustls_pki_types::CertificateDer;
    use rustls_pki_types::pem::PemObject;
    let setup = |e: &dyn std::fmt::Display| DbError::Setup(e.to_string());
    let config = match ca_pem {
        Some(path) => {
            let mut roots = rustls::RootCertStore::empty();
            for cert in CertificateDer::pem_file_iter(path).map_err(|e| setup(&e))? {
                roots
                    .add(cert.map_err(|e| setup(&e))?)
                    .map_err(|e| setup(&e))?;
            }
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
        None => {
            use rustls_platform_verifier::ConfigVerifierExt;
            rustls::ClientConfig::with_platform_verifier().map_err(|e| setup(&e))?
        }
    };
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

/// `[0.1,0.2,…]`, pgvector's text form.
pub fn vector_literal(v: &[f32]) -> String {
    let parts: Vec<String> = v
        .iter()
        .map(|x| {
            if x.is_finite() {
                x.to_string()
            } else {
                "0".into()
            }
        })
        .collect();
    format!("[{}]", parts.join(","))
}

/// An OR query over the question's content words for `to_tsquery('simple', …)`. Words are
/// reduced to letters and digits, so nothing the user types can inject tsquery syntax.
pub fn tsquery(question: &str) -> Option<String> {
    const STOP: &[&str] = &[
        "a", "an", "and", "any", "are", "about", "at", "be", "by", "can", "did", "do", "does",
        "for", "from", "has", "have", "how", "i", "in", "is", "it", "me", "my", "of", "on", "or",
        "our", "so", "that", "the", "this", "to", "was", "we", "what", "when", "where", "which",
        "who", "why", "will", "with", "you", "your",
    ];
    let mut words: Vec<String> = question
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 2 && !STOP.contains(&w.as_str()))
        .collect();
    words.sort();
    words.dedup();
    words.truncate(24);
    (!words.is_empty()).then(|| words.join(" | "))
}

impl Db {
    /// Connects with TLS unless the URL says `sslmode=disable` (local development only).
    pub fn connect(url: &str, ca_pem: Option<&str>) -> Result<Self, DbError> {
        Self::connect_with(url, ca_pem, PoolOptions::default())
    }

    /// [`Db::connect`] with explicit pool limits.
    pub fn connect_with(
        url: &str,
        ca_pem: Option<&str>,
        opts: PoolOptions,
    ) -> Result<Self, DbError> {
        let pg: tokio_postgres::Config = url.parse()?;
        let mgr_cfg = ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        };
        let mgr = if pg.get_ssl_mode() == tokio_postgres::config::SslMode::Disable {
            Manager::from_config(pg, tokio_postgres::NoTls, mgr_cfg)
        } else {
            Manager::from_config(pg, tls(ca_pem)?, mgr_cfg)
        };
        let wait = Some(std::time::Duration::from_secs(opts.timeout_secs));
        let pool = Pool::builder(mgr)
            .max_size(opts.max_size)
            .runtime(deadpool_postgres::Runtime::Tokio1)
            .wait_timeout(wait)
            .create_timeout(wait)
            .recycle_timeout(wait)
            .build()
            .map_err(|e| DbError::Setup(e.to_string()))?;
        Ok(Self {
            pool,
            statement_timeout_ms: opts.statement_timeout_ms,
        })
    }

    /// (size, available) of the pool, for metrics.
    pub fn pool_status(&self) -> (usize, usize) {
        let s = self.pool.status();
        (s.size, s.available)
    }

    /// Readiness: a connection and a trivial query.
    pub async fn ping(&self) -> Result<(), DbError> {
        let client = self.pool.get().await?;
        client.simple_query("select 1").await?;
        Ok(())
    }

    /// Runs `f` in a transaction that RLS confines to `user`, and commits if it succeeds.
    async fn scoped<T>(
        &self,
        user: Option<Uuid>,
        f: impl AsyncFnOnce(&Transaction<'_>) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.batch_execute(&format!(
            "set local role nugget_service; set local statement_timeout = {}",
            self.statement_timeout_ms
        ))
        .await?;
        if let Some(u) = user {
            tx.execute(
                "select set_config('nugget.user_id', $1, true)",
                &[&u.to_string()],
            )
            .await?;
        }
        let out = f(&tx).await?;
        tx.commit().await?;
        Ok(out)
    }

    // ---- profile and usage ------------------------------------------------------------------

    /// The user's plan; also records that they are active (drives the sync interval).
    pub async fn touch_profile(&self, user: Uuid) -> Result<String, DbError> {
        self.scoped(Some(user), async |tx| {
            let row = tx
                .query_opt(
                    "update public.profiles set last_active_at = now() where user_id = $1 returning plan",
                    &[&user],
                )
                .await?;
            Ok(row.map(|r| r.get(0)).unwrap_or_else(|| "free".into()))
        })
        .await
    }

    pub async fn record_usage(
        &self,
        user: Uuid,
        kind: UsageKind,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cost_micros: i64,
    ) -> Result<(), DbError> {
        let (i, o) = (
            i32::try_from(input_tokens).unwrap_or(i32::MAX),
            i32::try_from(output_tokens).unwrap_or(i32::MAX),
        );
        self.scoped(Some(user), async |tx| {
            tx.execute(
                "insert into public.usage_events (user_id, kind, model, input_tokens, output_tokens, cost_micros)
                 values ($1, $2, $3, $4, $5, $6)",
                &[&user, &kind.as_str(), &model, &i, &o, &cost_micros],
            )
            .await?;
            Ok(())
        })
        .await
    }

    /// Month-to-date AI cost (LLM and embeddings; tool calls are ours to absorb).
    pub async fn month_cost_micros(&self, user: Uuid) -> Result<i64, DbError> {
        self.scoped(Some(user), async |tx| {
            let r = tx
                .query_one(
                    "select coalesce(sum(cost_micros), 0)::bigint from public.usage_events
                     where user_id = $1 and kind <> 'tool' and created_at >= date_trunc('month', now())",
                    &[&user],
                )
                .await?;
            Ok(r.get(0))
        })
        .await
    }

    // ---- connections -------------------------------------------------------------------------

    pub async fn upsert_connection(
        &self,
        user: Uuid,
        toolkit: &str,
        account_id: &str,
        status: &str,
    ) -> Result<(), DbError> {
        self.scoped(Some(user), async |tx| {
            tx.execute(
                "insert into public.connections (user_id, toolkit, composio_account_id, status)
                 values ($1, $2, $3, $4)
                 on conflict (composio_account_id) do update set status = excluded.status",
                &[&user, &toolkit, &account_id, &status],
            )
            .await?;
            Ok(())
        })
        .await
    }

    pub async fn connections(&self, user: Uuid) -> Result<Vec<Connection>, DbError> {
        self.scoped(Some(user), async |tx| {
            let rows = tx
                .query(
                    "select id, toolkit, composio_account_id, status, last_synced_at, last_error, sync_cursor
                     from public.connections where user_id = $1 order by created_at",
                    &[&user],
                )
                .await?;
            Ok(rows.iter().map(Connection::from_row).collect())
        })
        .await
    }

    /// Deletes the connection and, by cascade, its items. Returns the Composio account id.
    pub async fn delete_connection(&self, user: Uuid, id: Uuid) -> Result<Option<String>, DbError> {
        self.scoped(Some(user), async |tx| {
            let r = tx
                .query_opt(
                    "delete from public.connections where user_id = $1 and id = $2 returning composio_account_id",
                    &[&user, &id],
                )
                .await?;
            Ok(r.map(|r| r.get(0)))
        })
        .await
    }

    pub async fn finish_sync(
        &self,
        user: Uuid,
        id: Uuid,
        cursor: Option<&Value>,
        error: Option<&str>,
        next_in_secs: i64,
    ) -> Result<(), DbError> {
        let secs = next_in_secs as f64;
        self.scoped(Some(user), async |tx| {
            tx.execute(
                "update public.connections set
                   sync_cursor = coalesce($3, sync_cursor),
                   last_synced_at = case when $4::text is null then now() else last_synced_at end,
                   last_error = $4,
                   next_sync_at = now() + make_interval(secs => $5)
                 where user_id = $1 and id = $2",
                &[&user, &id, &cursor, &error, &secs],
            )
            .await?;
            Ok(())
        })
        .await
    }

    /// Owner of a Composio account (webhooks), through the definer function.
    pub async fn connection_owner(
        &self,
        account_id: &str,
    ) -> Result<Option<(Uuid, Uuid)>, DbError> {
        self.scoped(None, async |tx| {
            let r = tx
                .query_opt(
                    "select user_id, connection_id from nugget_private.connection_owner($1)",
                    &[&account_id],
                )
                .await?;
            Ok(r.map(|r| (r.get(0), r.get(1))))
        })
        .await
    }

    /// Makes one connection (or all of the user's) due now.
    pub async fn mark_due(&self, user: Uuid, id: Option<Uuid>) -> Result<u64, DbError> {
        self.scoped(Some(user), async |tx| {
            Ok(tx
                .execute(
                    "update public.connections set next_sync_at = now()
                     where user_id = $1 and ($2::uuid is null or id = $2)",
                    &[&user, &id],
                )
                .await?)
        })
        .await
    }

    /// (user, connection) pairs due for sync, across users.
    pub async fn due_connections(&self, max: i32) -> Result<Vec<(Uuid, Uuid)>, DbError> {
        self.scoped(None, async |tx| {
            let rows = tx
                .query(
                    "select user_id, connection_id from nugget_private.due_connections($1)",
                    &[&max],
                )
                .await?;
            Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
        })
        .await
    }

    /// Claims up to `max` due connections across users with a lease of `lease_secs`, so no
    /// other machine or overlapping pass syncs them meanwhile.
    pub async fn claim_due(&self, max: i32, lease_secs: i32) -> Result<Vec<(Uuid, Uuid)>, DbError> {
        self.scoped(None, async |tx| {
            let rows = tx
                .query(
                    "select user_id, connection_id from nugget_private.claim_due_connections($1, $2)",
                    &[&max, &lease_secs],
                )
                .await?;
            Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
        })
        .await
    }

    /// Whether the user was active in the last hour (sync more often while they use the app).
    pub async fn recently_active(&self, user: Uuid) -> Result<bool, DbError> {
        self.scoped(Some(user), async |tx| {
            let r = tx
                .query_opt(
                    "select coalesce(last_active_at > now() - interval '1 hour', false) from public.profiles where user_id = $1",
                    &[&user],
                )
                .await?;
            Ok(r.is_some_and(|r| r.get(0)))
        })
        .await
    }

    // ---- items and chunks ----------------------------------------------------------------------

    /// The stored content hash of an item, if any (to skip unchanged items before embedding).
    pub async fn item_hash(
        &self,
        user: Uuid,
        source: &str,
        external_id: &str,
    ) -> Result<Option<String>, DbError> {
        self.scoped(Some(user), async |tx| {
            let r = tx
                .query_opt(
                    "select content_hash from public.items where user_id = $1 and source = $2 and external_id = $3",
                    &[&user, &source, &external_id],
                )
                .await?;
            Ok(r.map(|r| r.get(0)))
        })
        .await
    }

    /// Inserts or replaces an item and all its chunks in one transaction.
    pub async fn upsert_item(
        &self,
        user: Uuid,
        connection: Option<Uuid>,
        item: &NewItem,
        hash: &str,
        chunks: &[(String, Option<Vec<f32>>)],
    ) -> Result<Upsert, DbError> {
        self.scoped(Some(user), async |tx| {
            let row = tx
                .query_one(
                    "insert into public.items
                       (user_id, connection_id, source, external_id, kind, title, author, url, occurred_at, body, metadata, content_hash)
                     values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                     on conflict (user_id, source, external_id) do update set
                       kind = excluded.kind, title = excluded.title, author = excluded.author, url = excluded.url,
                       occurred_at = excluded.occurred_at, body = excluded.body, metadata = excluded.metadata,
                       content_hash = excluded.content_hash, updated_at = now()
                     returning id, (xmax = 0) as inserted",
                    &[
                        &user, &connection, &item.source, &item.external_id, &item.kind, &item.title, &item.author,
                        &item.url, &item.occurred_at, &item.body, &item.metadata, &hash,
                    ],
                )
                .await?;
            let id: Uuid = row.get(0);
            let inserted: bool = row.get(1);
            tx.execute("delete from public.chunks where item_id = $1", &[&id]).await?;
            let stmt = tx
                .prepare(
                    "insert into public.chunks (user_id, item_id, ord, text, embedding)
                     values ($1, $2, $3, $4, $5::text::extensions.vector)",
                )
                .await?;
            for (ord, (text, emb)) in chunks.iter().enumerate() {
                let ord = ord as i32;
                let emb = emb.as_deref().map(vector_literal);
                tx.execute(&stmt, &[&user, &id, &ord, text, &emb]).await?;
            }
            Ok(if inserted { Upsert::Added } else { Upsert::Updated })
        })
        .await
    }

    fn hits(rows: &[Row]) -> Vec<Hit> {
        rows.iter()
            .map(|r| {
                let item: Uuid = r.get("item_id");
                Hit {
                    item: ItemRef {
                        source: SourceId::new(r.get::<_, String>("source")),
                        key: ItemKey::new(item.to_string()),
                    },
                    chunk_id: r.get("id"),
                    title: r.get("title"),
                    snippet: r.get("text"),
                    score: r.get::<_, f32>("score"),
                }
            })
            .collect()
    }

    /// Full-text hits, best first (`ts_rank_cd`, higher is better).
    pub async fn search_text(
        &self,
        user: Uuid,
        question: &str,
        limit: i64,
    ) -> Result<Vec<Hit>, DbError> {
        let Some(q) = tsquery(question) else {
            return Ok(Vec::new());
        };
        self.scoped(Some(user), async |tx| {
            let rows = tx
                .query(
                    "select c.id, c.item_id, c.text, i.source, i.title, ts_rank_cd(c.tsv, q)::real as score
                     from public.chunks c join public.items i on i.id = c.item_id,
                          to_tsquery('simple', $2) q
                     where c.user_id = $1 and c.tsv @@ q
                     order by score desc, c.id limit $3",
                    &[&user, &q, &limit],
                )
                .await?;
            Ok(Self::hits(&rows))
        })
        .await
    }

    /// Vector hits by cosine similarity, best first.
    pub async fn search_vector(
        &self,
        user: Uuid,
        embedding: &[f32],
        limit: i64,
    ) -> Result<Vec<Hit>, DbError> {
        let v = vector_literal(embedding);
        self.scoped(Some(user), async |tx| {
            let rows = tx
                .query(
                    "select c.id, c.item_id, c.text, i.source, i.title,
                            (1 - (c.embedding operator(extensions.<=>) $2::text::extensions.vector))::real as score
                     from public.chunks c join public.items i on i.id = c.item_id
                     where c.user_id = $1 and c.embedding is not null
                     order by c.embedding operator(extensions.<=>) $2::text::extensions.vector limit $3",
                    &[&user, &v, &limit],
                )
                .await?;
            Ok(Self::hits(&rows))
        })
        .await
    }

    pub async fn items_meta(
        &self,
        user: Uuid,
        ids: &[Uuid],
    ) -> Result<HashMap<Uuid, ItemMeta>, DbError> {
        self.scoped(Some(user), async |tx| {
            let rows = tx
                .query(
                    "select id, source, kind, title, author, url, occurred_at from public.items
                     where user_id = $1 and id = any($2)",
                    &[&user, &ids],
                )
                .await?;
            Ok(rows
                .iter()
                .map(ItemMeta::from_row)
                .map(|m| (m.id, m))
                .collect())
        })
        .await
    }

    // ---- conversations -----------------------------------------------------------------------

    /// Stores a question and its answer; creates the conversation when `conversation` is None
    /// or not the user's.
    pub async fn save_exchange(
        &self,
        user: Uuid,
        conversation: Option<Uuid>,
        question: &str,
        answer: &str,
        citations: &Value,
    ) -> Result<Uuid, DbError> {
        self.scoped(Some(user), async |tx| {
            let existing = match conversation {
                Some(id) => tx
                    .query_opt(
                        "update public.conversations set updated_at = now() where user_id = $1 and id = $2 returning id",
                        &[&user, &id],
                    )
                    .await?
                    .map(|r| r.get::<_, Uuid>(0)),
                None => None,
            };
            let conv = match existing {
                Some(id) => id,
                None => {
                    let title: String = question.chars().take(80).collect();
                    tx.query_one(
                        "insert into public.conversations (user_id, title) values ($1, $2) returning id",
                        &[&user, &title],
                    )
                    .await?
                    .get(0)
                }
            };
            for (role, content, cites) in [("user", question, json!([])), ("assistant", answer, citations.clone())] {
                tx.execute(
                    "insert into public.messages (user_id, conversation_id, role, content, citations)
                     values ($1, $2, $3, $4, $5)",
                    &[&user, &conv, &role, &content, &cites],
                )
                .await?;
            }
            Ok(conv)
        })
        .await
    }

    /// Everything stored for the user, as JSON (GDPR export). Embeddings are left out: they
    /// are derived data and large.
    pub async fn export(&self, user: Uuid) -> Result<Value, DbError> {
        const PARTS: &[(&str, &str)] = &[
            ("profile", "select plan, created_at from public.profiles"),
            (
                "connections",
                "select id, toolkit, status, last_synced_at, created_at from public.connections",
            ),
            (
                "items",
                "select id, source, external_id, kind, title, author, url, occurred_at, body, metadata, created_at from public.items",
            ),
            (
                "entities",
                "select id, kind, name, metadata, created_at from public.entities",
            ),
            (
                "claims",
                "select id, subject_id, predicate, object_text, object_id, item_id, confidence, created_at from public.claims",
            ),
            (
                "cards",
                "select id, kind, title, body, status, created_at from public.cards",
            ),
            (
                "conversations",
                "select id, title, created_at from public.conversations",
            ),
            (
                "messages",
                "select conversation_id, role, content, citations, created_at from public.messages",
            ),
            (
                "usage_events",
                "select kind, model, input_tokens, output_tokens, cost_micros, created_at from public.usage_events",
            ),
        ];
        self.scoped(Some(user), async |tx| {
            let mut out = serde_json::Map::new();
            for (name, sql) in PARTS {
                // RLS scopes these to the user; the wrapper turns rows into one JSON array.
                let r = tx
                    .query_one(
                        &format!("select coalesce(jsonb_agg(t), '[]'::jsonb) from ({sql}) t"),
                        &[],
                    )
                    .await?;
                out.insert((*name).into(), r.get::<_, Value>(0));
            }
            Ok(Value::Object(out))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsquery_is_safe_or_query() {
        assert_eq!(
            tsquery("When is my lease renewal due?").as_deref(),
            Some("due | lease | renewal")
        );
        assert_eq!(
            tsquery("a'b & !c | <-> :*"),
            None,
            "operators and 1-char words dropped"
        );
        assert_eq!(
            tsquery("Réunion à Zürich").as_deref(),
            Some("réunion | zürich")
        );
        assert_eq!(tsquery(""), None);
    }

    #[test]
    fn vector_text_form() {
        assert_eq!(vector_literal(&[0.5, -1.0, f32::NAN]), "[0.5,-1,0]");
    }
}
