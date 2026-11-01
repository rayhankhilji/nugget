//! A triage request that yields to an interactive request is cancelled at the server: the real
//! OpenAI-compatible adapter's connection is closed (llama-server stops a request whose
//! connection closed), not left running on the server's single slot. A mock server stands in
//! for llama-server: it answers with stream headers and then stalls, like a long prefill.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use nugget_core::capability::{Capabilities, Tier};
use nugget_core::egress::NullEgress;
use nugget_core::ids::ProviderId;
use nugget_infer::{ProviderConfig, Router, TierMap, build_provider};
use nugget_ingest::Foreground;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// Accepts connections; for each, reads the request, sends SSE headers and stalls. Reports
/// `"request"` when a request arrived and `"closed"` when the client closed the connection.
async fn stalling_server() -> (u16, mpsc::UnboundedReceiver<(&'static str, Instant)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                let mut seen = Vec::new();
                // Read until the end of the headers and the announced body.
                loop {
                    let n = match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    seen.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&seen).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        if seen.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let _ = tx.send(("request", Instant::now()));
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                          cache-control: no-cache\r\ntransfer-encoding: chunked\r\n\r\n",
                    )
                    .await;
                // "Prefill": no tokens, until the client goes away.
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => {
                            let _ = tx.send(("closed", Instant::now()));
                            return;
                        }
                        Ok(_) => {}
                    }
                }
            });
        }
    });
    (port, rx)
}

#[tokio::test]
async fn a_yielded_triage_request_closes_its_connection_to_the_server() {
    let (port, mut events) = stalling_server().await;
    let cfg: ProviderConfig = toml::from_str(&format!(
        "id = \"local-chat\"\nkind = \"openai_compatible\"\n\
         base_url = \"http://127.0.0.1:{port}/v1\"\nmodel = \"m\"\ntiers = [\"triage\"]\n\
         max_retries = 0\n"
    ))
    .unwrap();
    let provider = build_provider(&cfg, Arc::new(NullEgress)).unwrap();
    let mut map = TierMap::new();
    map.insert(Tier::Triage, vec![ProviderId::new("local-chat")]);
    let router = Arc::new(Router::new(vec![provider], map));
    router.set_capabilities(
        &ProviderId::new("local-chat"),
        Capabilities {
            streaming: true,
            json_schema: true,
            ..Default::default()
        },
    );
    let fg = Arc::new(Foreground::new(Duration::from_millis(50)));
    let h = start(Opts {
        router: Some(router),
        foreground: Some(fg.clone()),
        ..Default::default()
    })
    .await;
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("a", "a note about lunch", Some("1"))),
        Step::Return(json!("done")),
    ]);
    h.add("fake-1").await;

    // Triage sends its request; the server "prefills" forever.
    let (kind, _) = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("triage request reached the server")
        .unwrap();
    assert_eq!(kind, "request");

    // An Ask starts: the server must see the triage connection close promptly.
    let asked = Instant::now();
    let ask = fg.begin();
    let (kind, at) = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("the yielded triage request was not cancelled at the server")
        .unwrap();
    assert_eq!(kind, "closed");
    assert!(at.duration_since(asked) < Duration::from_secs(2));
    // No new triage request while the Ask is in flight.
    assert!(
        tokio::time::timeout(Duration::from_millis(500), events.recv())
            .await
            .is_err()
    );
    drop(ask);
    // Afterwards triage tries again.
    let (kind, _) = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("triage retried after the Ask")
        .unwrap();
    assert_eq!(kind, "request");
    h.ingest.shutdown().await;
}
