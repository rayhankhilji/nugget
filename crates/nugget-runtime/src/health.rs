//! Loopback HTTP checks against a running `llama-server`: readiness and request activity.

use std::time::{Duration, Instant};

use nugget_core::{Error, Result};
use nugget_infer::Secret;

use crate::logs::{SharedLog, lock};
use crate::process::ServerProcess;

/// HTTP client for 127.0.0.1 only: never through a proxy, never following redirects.
pub(crate) fn loopback_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| Error::Other(format!("cannot build HTTP client: {e}")))
}

/// Outcome of one `/health` probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Health {
    Ready,
    /// Up, but still loading the model (HTTP 503).
    Loading,
    /// Not answering (yet).
    Down,
}

pub(crate) async fn probe(client: &reqwest::Client, port: u16) -> Health {
    let url = format!("http://127.0.0.1:{port}/health");
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => match r.text().await {
            // llama-server answers {"status":"ok"} once the model is loaded.
            Ok(body) if body.contains("ok") => Health::Ready,
            _ => Health::Loading,
        },
        Ok(_) => Health::Loading,
        Err(_) => Health::Down,
    }
}

/// Poll `/health` until ready, the process exits, or `timeout` passes.
pub(crate) async fn wait_ready(
    client: &reqwest::Client,
    port: u16,
    proc: &mut ServerProcess,
    timeout: Duration,
    poll: Duration,
    log: &SharedLog,
) -> Result<()> {
    let start = Instant::now();
    let mut delay = poll.max(Duration::from_millis(10));
    loop {
        if let Some(status) = proc.try_wait() {
            // Give the log pumps a moment to drain the last lines.
            tokio::time::sleep(Duration::from_millis(50)).await;
            let tail = lock(log).tail(5).join(" | ");
            return Err(Error::Provider(format!(
                "llama-server exited during start-up ({status}); last output: {tail}"
            )));
        }
        if probe(client, port).await == Health::Ready {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            let tail = lock(log).tail(3).join(" | ");
            return Err(Error::Provider(format!(
                "llama-server was not ready after {}s; last output: {tail}",
                timeout.as_secs_f32()
            )));
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 3 / 2).min(Duration::from_millis(500));
    }
}

/// A fingerprint of the server's request counters from `/metrics` (Prometheus text), plus
/// whether a request is in flight. `None` if metrics are unavailable.
pub(crate) async fn activity(
    client: &reqwest::Client,
    port: u16,
    key: &Secret,
) -> Option<(f64, bool)> {
    let url = format!("http://127.0.0.1:{port}/metrics");
    let resp = client
        .get(&url)
        .bearer_auth(key.expose())
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    Some(parse_metrics(&text))
}

pub(crate) fn parse_metrics(text: &str) -> (f64, bool) {
    let mut sum = 0.0;
    let mut busy = false;
    for line in text.lines().take(10_000) {
        if line.starts_with('#') || !line.starts_with("llamacpp:") {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(name), Some(value)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(v) = value.parse::<f64>() else {
            continue;
        };
        if name.ends_with("_total") {
            sum += v;
        } else if (name.starts_with("llamacpp:requests_processing")
            || name.starts_with("llamacpp:requests_deferred"))
            && v > 0.0
        {
            busy = true;
        }
    }
    (sum, busy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_fingerprint() {
        let text = "# HELP x\n# TYPE llamacpp:prompt_tokens_total counter\n\
                    llamacpp:prompt_tokens_total 12\nllamacpp:tokens_predicted_total 3\n\
                    llamacpp:requests_processing 0\nllamacpp:requests_deferred 0\nother 99\n";
        assert_eq!(parse_metrics(text), (15.0, false));
        let busy = text.replace("requests_processing 0", "requests_processing 1");
        assert_eq!(parse_metrics(&busy), (15.0, true));
        assert_eq!(
            parse_metrics("garbage\nllamacpp:x_total nan?\n"),
            (0.0, false)
        );
    }
}
