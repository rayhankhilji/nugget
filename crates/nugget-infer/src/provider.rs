//! The provider trait and construction from config.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nugget_core::capability::Locality;
use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use reqwest::header::{HeaderName, HeaderValue};

use crate::adapters::openai_responses::OpenAiResponsesProvider;
use crate::adapters::{anthropic::AnthropicProvider, openai_compatible::OpenAiCompatibleProvider};
use crate::config::{ProviderConfig, Secret, SecretResolver, SystemSecrets, resolve_headers};
use crate::http::{Http, HttpSettings};
use crate::locality::{HostResolver, Resolved, SystemResolver};
use crate::types::{ChatRequest, ChatStream, FEATURE_PREWARM, ProviderKind, ProviderMeta};

/// One model behind one API. Implementations are cheap to share (`Arc<dyn Provider>`).
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn meta(&self) -> ProviderMeta;
    /// Start a streaming chat. Transient HTTP failures are retried before the stream starts;
    /// errors after that arrive as `Err` items and end the stream.
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream>;
    /// Embed texts, one vector per input in order. `Error::Unsupported` if the API has none.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Get ready for an imminent request without sending any user content: resolve, connect and
    /// complete the TLS handshake so the connection sits warm in the pool (HTTP/2 keep-alive),
    /// and the next [`Provider::chat`] skips that latency. The built-in remote adapters make one
    /// cheap authenticated `GET …/models` request, recorded in the egress ledger under
    /// [`FEATURE_PREWARM`] with no items. Best effort: callers may ignore the error. The default
    /// does nothing.
    async fn prewarm(&self) -> Result<()> {
        Ok(())
    }

    /// Like [`Provider::prewarm`], and also warm the provider's prompt cache with the stable
    /// prefix of `req` ([`ChatRequest::cache_prefix`]) when the provider supports doing that
    /// without generating output. Unlike `prewarm`, this DOES send the prefix content (system
    /// prompt, tools, messages up to the last [`crate::Part::CacheBreakpoint`]), recorded in the
    /// egress ledger under `req.feature` and `req.items`, so apply the same routing and
    /// sensitivity rules as for the real request. The default calls `prewarm`.
    ///
    /// - Anthropic: a `max_tokens: 0` request with the same `cache_control` breakpoints (only if
    ///   the request marks something cacheable).
    /// - llama-server (`llama_cpp`): `n_predict: 0` with `cache_prompt`, evaluating the prefix
    ///   into a slot's KV cache.
    /// - OpenAI Responses and other OpenAI-compatible servers: connection prewarm only.
    async fn prewarm_prefix(&self, req: &ChatRequest) -> Result<()> {
        let _ = req;
        self.prewarm().await
    }
}

/// Build a provider, resolving its API key from the environment or OS keychain.
pub fn build_provider(
    cfg: &ProviderConfig,
    egress: Arc<dyn EgressSink>,
) -> Result<Arc<dyn Provider>> {
    build_provider_with(cfg, egress, &SystemSecrets)
}

/// [`build_provider`] with an explicit secret resolver.
///
/// A `base_url` host name other than `localhost` is resolved here, once, with the system resolver
/// (a blocking lookup; call from a blocking context), and the client is pinned to the result. See
/// [`crate::locality`].
pub fn build_provider_with(
    cfg: &ProviderConfig,
    egress: Arc<dyn EgressSink>,
    secrets: &dyn SecretResolver,
) -> Result<Arc<dyn Provider>> {
    build_provider_inner(cfg, egress, secrets, &SystemResolver)
}

/// Everything resolved at build time that the adapters hand to [`Common::new`].
pub(crate) struct Prepared {
    pub locality: Locality,
    pub resolved: Resolved,
    pub headers: Vec<(HeaderName, HeaderValue)>,
}

pub(crate) fn build_provider_inner(
    cfg: &ProviderConfig,
    egress: Arc<dyn EgressSink>,
    secrets: &dyn SecretResolver,
    resolver: &dyn HostResolver,
) -> Result<Arc<dyn Provider>> {
    cfg.validate()?;
    let (locality, resolved) = cfg.resolve_network(resolver)?;
    let prepared = Prepared {
        locality,
        resolved,
        headers: resolve_headers(cfg, secrets)?,
    };
    let key = cfg
        .api_key
        .as_ref()
        .map(|k| {
            secrets.resolve(k).map_err(|e| match e {
                Error::NotFound(m) => {
                    Error::NotFound(format!("provider {}: api_key {k}: {m}", cfg.id))
                }
                other => other,
            })
        })
        .transpose()?;
    Ok(match cfg.kind {
        ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(cfg, egress, key, prepared)?),
        ProviderKind::OpenaiResponses => {
            Arc::new(OpenAiResponsesProvider::new(cfg, egress, key, prepared)?)
        }
        ProviderKind::OpenaiCompatible => {
            Arc::new(OpenAiCompatibleProvider::new(cfg, egress, key, prepared)?)
        }
    })
}

/// State every adapter shares.
pub(crate) struct Common {
    pub id: ProviderId,
    pub meta: ProviderMeta,
    pub http: Http,
    pub cfg: ProviderConfig,
}

impl Common {
    pub(crate) fn new(
        cfg: &ProviderConfig,
        egress: Arc<dyn EgressSink>,
        auth: Vec<(HeaderName, HeaderValue)>,
        prepared: Prepared,
    ) -> Result<Self> {
        let base = cfg.base_url()?;
        let Prepared {
            locality,
            resolved,
            headers,
        } = prepared;
        let settings = HttpSettings {
            connect_timeout: Duration::from_secs(cfg.connect_timeout_secs.unwrap_or(10)),
            idle_timeout: Duration::from_secs(cfg.idle_timeout_secs.unwrap_or(600)),
            max_retries: cfg.max_retries.unwrap_or(2),
            retry_base: Duration::from_millis(cfg.retry_base_ms.unwrap_or(500)),
            max_retry_delay: Duration::from_secs(60),
            headers,
            pin: resolved.pin,
            // Traffic to this machine or the LAN never goes through a proxy (which would also
            // re-resolve the name and defeat the pin).
            direct: resolved.locality != Locality::Cloud,
            unix_socket: cfg.socket_path.clone(),
        };
        let http = Http::new(cfg.id.clone(), base, locality, egress, settings, auth)?;
        Ok(Self {
            id: cfg.id.clone(),
            meta: ProviderMeta {
                kind: cfg.kind,
                model: cfg.model.clone(),
                locality,
                context_tokens: cfg.context_tokens,
            },
            http,
            cfg: cfg.clone(),
        })
    }
}

impl Common {
    /// Connection prewarm: one bodiless authenticated `GET {base}/{path}`, response drained and
    /// metered under [`FEATURE_PREWARM`]. The connection then stays in the pool.
    pub(crate) async fn prewarm_get(&self, path: &str) -> Result<()> {
        let (resp, mut meter, clock) = self.http.get(path, FEATURE_PREWARM).await?;
        let mut body = resp.bytes_stream();
        while let Some(chunk) = futures::StreamExt::next(&mut body).await {
            match chunk {
                Ok(c) => meter.add_received(c.len()),
                Err(_) => break,
            }
        }
        tracing::debug!(provider = %self.id, connect = ?clock.connect, headers = ?clock.headers, "prewarmed");
        Ok(())
    }
}

/// The auth header for a resolved key (`value` formats the secret), marked sensitive so it is
/// never logged. No key, no header.
pub(crate) fn auth_header(
    name: &'static str,
    key: Option<Secret>,
    value: impl Fn(&str) -> String,
) -> Result<Vec<(HeaderName, HeaderValue)>> {
    let Some(key) = key else {
        return Ok(Vec::new());
    };
    let mut v = HeaderValue::from_str(&value(key.expose())).map_err(|_| {
        Error::Invalid("api key contains characters not allowed in an HTTP header".into())
    })?;
    v.set_sensitive(true);
    Ok(vec![(HeaderName::from_static(name), v)])
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    use nugget_core::egress::EgressEvent;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::config::ApiKeyRef;
    use crate::locality::tests::FakeResolver;

    #[derive(Default)]
    struct Sink(Mutex<Vec<EgressEvent>>);
    impl EgressSink for Sink {
        fn record(&self, e: EgressEvent) {
            self.0.lock().unwrap().push(e);
        }
    }

    struct NoSecrets;
    impl SecretResolver for NoSecrets {
        fn resolve(&self, _: &ApiKeyRef) -> Result<Secret> {
            Err(Error::NotFound("none".into()))
        }
    }

    fn cfg(base_url: &str, extra: &str) -> ProviderConfig {
        ProviderConfig::from_toml_str(&format!(
            "id = \"p\"\nkind = \"openai_compatible\"\nmodel = \"m\"\nmax_retries = 0\n\
             base_url = \"{base_url}\"\n{extra}"
        ))
        .unwrap()
    }

    fn build(c: &ProviderConfig, r: &FakeResolver) -> Result<(Arc<dyn Provider>, Arc<Sink>)> {
        let sink = Arc::new(Sink::default());
        let p = build_provider_inner(c, sink.clone(), &NoSecrets, r)?;
        Ok((p, sink))
    }

    /// A name is classified by what it resolves to at build time, and the client is pinned to
    /// those addresses: `lan-gpu.invalid` does not exist in real DNS, so reaching the mock server
    /// proves the pin is used, and the resolver is asked exactly once (no rebinding window).
    #[tokio::test]
    async fn host_names_are_resolved_once_and_pinned() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data": [{"index": 0, "embedding": [0.5]}]})),
            )
            .mount(&server)
            .await;
        let port = server.address().port();
        let r = FakeResolver::new(vec![
            ("lan-gpu.invalid", vec!["127.0.0.1"]),
            ("nas.invalid", vec!["192.168.7.7"]),
            ("public.invalid", vec!["192.168.7.7", "8.8.8.8"]),
        ]);

        let c = cfg(&format!("http://lan-gpu.invalid:{port}/v1"), "");
        let (p, sink) = build(&c, &r).unwrap();
        assert_eq!(p.meta().locality, Locality::OnDevice);
        assert_eq!(r.calls.load(Ordering::SeqCst), 1);
        for _ in 0..3 {
            assert_eq!(p.embed(&["x".into()]).await.unwrap(), vec![vec![0.5]]);
        }
        assert_eq!(r.calls.load(Ordering::SeqCst), 1, "resolved once, at build");
        assert!(sink.0.lock().unwrap().is_empty());

        // Upgrade-only override still applies on top of the resolved class.
        let c = cfg(
            &format!("http://lan-gpu.invalid:{port}/v1"),
            "locality = \"cloud\"",
        );
        let (p, sink) = build(&c, &r).unwrap();
        assert_eq!(p.meta().locality, Locality::Cloud);
        p.embed(&["x".into()]).await.unwrap();
        assert_eq!(sink.0.lock().unwrap().len(), 1);
        let c = cfg("http://nas.invalid:1/v1", "locality = \"on_device\"");
        assert!(build(&c, &r).is_err(), "override may not hide egress");

        // Private-only answers are LocalNetwork; any public address makes the name Cloud, and
        // a Cloud (or unresolvable) name over plaintext http is refused.
        let (p, _) = build(&cfg("http://nas.invalid:1/v1", ""), &r).unwrap();
        assert_eq!(p.meta().locality, Locality::LocalNetwork);
        for url in ["http://public.invalid/v1", "http://unresolvable.invalid/v1"] {
            let err = build(&cfg(url, ""), &r).err().unwrap();
            assert!(err.to_string().contains("plaintext http"), "{url}: {err}");
        }
        let (p, _) = build(&cfg("https://unresolvable.invalid/v1", ""), &r).unwrap();
        assert_eq!(p.meta().locality, Locality::Cloud);
    }

    #[test]
    fn literals_need_no_resolution() {
        let r = FakeResolver::new(vec![]);
        for (url, want) in [
            ("http://localhost:11434/v1", Locality::OnDevice),
            ("http://127.0.0.1:1/v1", Locality::OnDevice),
            ("http://[::1]:1/v1", Locality::OnDevice),
            ("http://10.1.2.3:1/v1", Locality::LocalNetwork),
        ] {
            let (p, _) = build(&cfg(url, ""), &r).unwrap();
            assert_eq!(p.meta().locality, want, "{url}");
        }
        assert_eq!(r.calls.load(Ordering::SeqCst), 0);
    }

    /// With `socket_path` every request goes to the Unix socket (the URL's host is never looked
    /// up or connected to), and the endpoint is on-device: nothing reaches the egress ledger.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_endpoints_skip_the_network() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let sock = std::env::temp_dir().join(format!("nugget-infer-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 64 * 1024];
            let mut got = Vec::new();
            // Headers, then the (small) JSON body.
            while !String::from_utf8_lossy(&got).contains("\"input\"") {
                let n = conn.read(&mut buf).await.unwrap();
                assert_ne!(n, 0, "connection closed early");
                got.extend_from_slice(&buf[..n]);
            }
            let body = r#"{"data": [{"index": 0, "embedding": [0.25]}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            conn.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&got).into_owned()
        });
        let r = FakeResolver::new(vec![]);
        let c = cfg(
            "http://unreachable.invalid/v1",
            &format!("socket_path = \"{}\"", sock.display()),
        );
        let (p, sink) = build(&c, &r).unwrap();
        assert_eq!(p.meta().locality, Locality::OnDevice);
        assert_eq!(p.embed(&["x".into()]).await.unwrap(), vec![vec![0.25]]);
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /v1/embeddings "), "{request}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("host: unreachable.invalid"),
            "{request}"
        );
        assert_eq!(r.calls.load(Ordering::SeqCst), 0, "no DNS lookup");
        assert!(sink.0.lock().unwrap().is_empty());
        let _ = std::fs::remove_file(&sock);
    }
}
