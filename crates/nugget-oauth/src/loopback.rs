//! A one-shot loopback HTTP listener for the OAuth redirect (RFC 8252 §7.3).
//!
//! Binds `127.0.0.1` on a random port. Every connection is read in its own task (so a browser's
//! speculative pre-connect cannot stall the real redirect); requests for anything but the
//! callback path get a 404. A callback whose `state` does not match gets a 400 and is otherwise
//! ignored (any local process can reach the port; it must not be able to abort the flow). The
//! first callback with the right `state` decides the outcome: a `code` must be present. At most
//! [`MAX_CONNS`] connections are served at once. The browser gets a minimal page either way.

use std::time::Duration;

use nugget_core::{Error, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::pkce::ct_eq;

/// Largest request head we read from the browser.
const MAX_REQUEST: usize = 16 * 1024;
/// How long one connection may take to send its request head.
const CONN_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections served at once; further ones wait in the listen backlog.
const MAX_CONNS: usize = 16;
/// The redirect path.
pub(crate) const CALLBACK_PATH: &str = "/";

pub(crate) struct Loopback {
    listener: TcpListener,
    port: u16,
}

/// Query parameters of the callback request.
#[derive(Debug, Default)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

impl Loopback {
    pub(crate) async fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, port })
    }

    pub(crate) fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{CALLBACK_PATH}", self.port)
    }

    /// Wait for the redirect and return the authorization code.
    pub(crate) async fn wait(self, expected_state: &str, timeout: Duration) -> Result<String> {
        let (tx, mut rx) = mpsc::channel::<(TcpStream, Callback)>(4);
        let mut conns = JoinSet::new();
        let mut ignored = 0usize;
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => {
                    let note = if ignored > 0 {
                        format!(" ({ignored} redirect(s) with a wrong OAuth state were ignored)")
                    } else {
                        String::new()
                    };
                    return Err(Error::Unauthorized(format!(
                        "timed out after {}s waiting for the browser redirect{note}",
                        timeout.as_secs()
                    )));
                }
                accepted = self.listener.accept(), if conns.len() < MAX_CONNS => match accepted {
                    Ok((stream, _)) => {
                        let tx = tx.clone();
                        conns.spawn(handle_conn(stream, tx));
                    }
                    Err(e) => tracing::debug!("oauth loopback accept failed: {e}"),
                },
                Some((mut stream, cb)) = rx.recv() => {
                    if !state_matches(&cb, expected_state) {
                        ignored += 1;
                        tracing::warn!("ignoring an OAuth redirect with a wrong state");
                        conns.spawn(async move {
                            let _ = respond(&mut stream, false).await;
                        });
                        continue;
                    }
                    let outcome = validate(cb, expected_state);
                    let _ = respond(&mut stream, outcome.is_ok()).await;
                    return outcome;
                }
                // Reap finished connection tasks so the set does not grow.
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
            }
        }
    }
}

fn state_matches(cb: &Callback, expected_state: &str) -> bool {
    cb.state
        .as_deref()
        .is_some_and(|s| ct_eq(s, expected_state))
}

fn validate(cb: Callback, expected_state: &str) -> Result<String> {
    if !state_matches(&cb, expected_state) {
        return Err(Error::Unauthorized(
            "OAuth state mismatch on the redirect; authorization aborted".into(),
        ));
    }
    if let Some(err) = cb.error {
        let desc = cb
            .error_description
            .map(|d| format!(": {}", truncate(&d, 200)))
            .unwrap_or_default();
        return Err(Error::Unauthorized(format!(
            "authorization was not granted ({}){desc}",
            truncate(&err, 64)
        )));
    }
    cb.code
        .filter(|c| !c.is_empty())
        .ok_or_else(|| Error::Invalid("redirect carried no authorization code".into()))
}

async fn handle_conn(mut stream: TcpStream, tx: mpsc::Sender<(TcpStream, Callback)>) {
    let head = match tokio::time::timeout(CONN_READ_TIMEOUT, read_head(&mut stream)).await {
        Ok(Ok(Some(head))) => head,
        _ => return,
    };
    match parse_callback(&head) {
        Some(cb) => {
            let _ = tx.send((stream, cb)).await;
        }
        None => {
            let _ = write_response(&mut stream, "404 Not Found", "Not found.").await;
        }
    }
}

/// Read up to the end of the request head (`\r\n\r\n`), capped at [`MAX_REQUEST`].
async fn read_head(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
        if buf.len() > MAX_REQUEST {
            return Ok(None);
        }
    }
}

/// `GET /?code=...&state=... HTTP/1.1` → the callback parameters. `None` for anything else.
fn parse_callback(head: &str) -> Option<Callback> {
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let (method, target) = (parts.next()?, parts.next()?);
    if method != "GET" {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH || query.is_empty() {
        return None;
    }
    let mut cb = Callback::default();
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        let slot = match k.as_ref() {
            "code" => &mut cb.code,
            "state" => &mut cb.state,
            "error" => &mut cb.error,
            "error_description" => &mut cb.error_description,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(v.into_owned());
        }
    }
    Some(cb)
}

async fn respond(stream: &mut TcpStream, ok: bool) -> std::io::Result<()> {
    if ok {
        write_response(
            stream,
            "200 OK",
            "Nugget is connected. You can close this tab.",
        )
        .await
    } else {
        write_response(
            stream,
            "400 Bad Request",
            "Authorization failed. You can close this tab and try again in Nugget.",
        )
        .await
    }
}

async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    message: &str,
) -> std::io::Result<()> {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Nugget</title></head>\
         <body style=\"font-family:system-ui,sans-serif;margin:3rem\"><p>{message}</p></body></html>"
    );
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.shutdown().await
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn parses_callback_and_ignores_other_paths() {
        let cb = parse_callback("GET /?code=a%2Fb&state=xyz HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!(cb.code.as_deref(), Some("a/b"));
        assert_eq!(cb.state.as_deref(), Some("xyz"));
        assert!(parse_callback("GET /favicon.ico HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_callback("POST /?code=a HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_callback("GET / HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_callback("").is_none());
        assert!(parse_callback("\u{0}\u{0}garbage").is_none());
    }

    #[test]
    fn validate_checks_state_error_and_code() {
        let ok = Callback {
            code: Some("c".into()),
            state: Some("s".into()),
            ..Default::default()
        };
        assert_eq!(validate(ok, "s").unwrap(), "c");
        let bad_state = Callback {
            code: Some("c".into()),
            state: Some("t".into()),
            ..Default::default()
        };
        assert!(matches!(
            validate(bad_state, "s"),
            Err(Error::Unauthorized(_))
        ));
        let denied = Callback {
            state: Some("s".into()),
            error: Some("access_denied".into()),
            ..Default::default()
        };
        assert!(matches!(validate(denied, "s"), Err(Error::Unauthorized(_))));
        let no_code = Callback {
            state: Some("s".into()),
            ..Default::default()
        };
        assert!(matches!(validate(no_code, "s"), Err(Error::Invalid(_))));
    }
}
