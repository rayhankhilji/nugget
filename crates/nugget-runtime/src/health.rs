//! HTTP checks against a running `llama-server` (on its Unix socket or loopback port): readiness
//! and request activity.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nugget_core::{Error, Result};
use nugget_infer::Secret;

use crate::listener::{Ownership, owner};
use crate::logs::{SharedLog, lock};
use crate::process::ServerProcess;

/// HTTP client for 127.0.0.1 only: never through a proxy, never following redirects.
pub(crate) fn loopback_client() -> Result<reqwest::Client> {
    client_builder()
        .build()
        .map_err(|e| Error::Other(format!("cannot build HTTP client: {e}")))
}

fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
}

/// Where one server listens, with a client that reaches it.
#[derive(Clone)]
pub(crate) struct Target {
    client: reqwest::Client,
    /// `http://127.0.0.1:<port>`, or `http://localhost` for a Unix socket.
    origin: String,
    port: u16,
    socket: Option<PathBuf>,
}

impl Target {
    /// `127.0.0.1:port`, through the shared loopback client.
    pub(crate) fn tcp(client: &reqwest::Client, port: u16) -> Self {
        Self {
            client: client.clone(),
            origin: format!("http://127.0.0.1:{port}"),
            port,
            socket: None,
        }
    }

    /// The Unix socket at `path`, through a client of its own.
    #[cfg(unix)]
    pub(crate) fn unix(path: &Path) -> Result<Self> {
        let client = client_builder()
            .unix_socket(path.to_path_buf())
            .build()
            .map_err(|e| Error::Other(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            client,
            origin: "http://localhost".into(),
            port: 0,
            socket: Some(path.to_path_buf()),
        })
    }

    #[cfg(not(unix))]
    pub(crate) fn unix(path: &Path) -> Result<Self> {
        Err(Error::Unsupported(format!(
            "Unix socket {} is not supported on this platform",
            path.display()
        )))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    /// Who listens there: a socket in our private directory is ours (see [`crate::socket`]);
    /// a TCP port is checked where the platform allows ([`crate::listener`]).
    fn owner(&self, child_pid: u32) -> Ownership {
        match &self.socket {
            Some(_) => Ownership::Child,
            None => owner(self.port, child_pid),
        }
    }
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

pub(crate) async fn probe(target: &Target) -> Health {
    match target.client.get(target.url("/health")).send().await {
        Ok(r) if r.status().is_success() => match r.text().await {
            // llama-server answers {"status":"ok"} once the model is loaded.
            Ok(body) if body.contains("ok") => Health::Ready,
            _ => Health::Loading,
        },
        Ok(_) => Health::Loading,
        Err(_) => Health::Down,
    }
}

/// The server process slot of a role: `None` once a stop took the process.
pub(crate) type ProcSlot = Mutex<Option<ServerProcess>>;

/// Liveness of the process in `slot`.
enum Child {
    Running(u32),
    Exited(std::process::ExitStatus),
    /// Taken by a stop (or never had a pid).
    Gone,
}

fn child(slot: &ProcSlot) -> Child {
    let mut g = lock_slot(slot);
    match g.as_mut() {
        None => Child::Gone,
        Some(p) => match p.try_wait() {
            Some(status) => Child::Exited(status),
            None => p.pid().map_or(Child::Gone, Child::Running),
        },
    }
}

fn lock_slot(slot: &ProcSlot) -> std::sync::MutexGuard<'_, Option<ServerProcess>> {
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Poll `/health` until the server in `slot` is ready, its process exits, or `timeout` passes.
///
/// Ready means: the process is alive before and after a `/health` probe that answered ok, and
/// the listener is the server's: a Unix socket in the private socket directory, or (on Linux)
/// a TCP port whose every listener reachable on `127.0.0.1:port` belongs to that process or its
/// process group ([`crate::listener`]). Another process answering on the port fails the start
/// at once.
pub(crate) async fn wait_ready(
    target: &Target,
    slot: &ProcSlot,
    timeout: Duration,
    poll: Duration,
    log: &SharedLog,
) -> Result<()> {
    let start = Instant::now();
    let mut delay = poll.max(Duration::from_millis(10));
    loop {
        let pid = match child(slot) {
            Child::Running(pid) => pid,
            Child::Exited(status) => {
                // Give the log pumps a moment to drain the last lines.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let tail = lock(log).tail(5).join(" | ");
                return Err(Error::Provider(format!(
                    "llama-server exited during start-up ({status}); last output: {tail}"
                )));
            }
            Child::Gone => return Err(Error::Cancelled),
        };
        if probe(target).await == Health::Ready
            && matches!(child(slot), Child::Running(p) if p == pid)
        {
            match target.owner(pid) {
                Ownership::Child | Ownership::Unverified => {
                    // Still the same live process after the ownership check.
                    if matches!(child(slot), Child::Running(p) if p == pid) {
                        return Ok(());
                    }
                }
                Ownership::Foreign(who) => {
                    return Err(Error::Provider(format!(
                        "another process answered on the server's port: {who}; refusing to \
                         use it"
                    )));
                }
                // Raced with the listener going away; the next round sees why.
                Ownership::NotListening => {}
            }
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
pub(crate) async fn activity(target: &Target, key: &Secret) -> Option<(f64, bool)> {
    let resp = target
        .client
        .get(target.url("/metrics"))
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
