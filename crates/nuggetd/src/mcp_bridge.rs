//! `nuggetd mcp --client <id>`: the stdio bridge MCP apps launch.
//!
//! It reads the client's token from `NUGGET_MCP_TOKEN`, connects to the running daemon over the
//! local IPC socket, opens a bridge session ([`crate::mcp::BRIDGE_OPEN`]) and then copies MCP
//! messages: each stdin line becomes a [`crate::mcp::BRIDGE_SEND`] call (sent one at a time, so
//! order is kept), and each [`crate::mcp::BRIDGE_RECV`] notification becomes a stdout line. All
//! checks (token, scopes, revocation, rate limits, audit) happen in the daemon; this process
//! holds no state. Nothing but MCP messages is ever written to stdout; diagnostics go to stderr.

use futures::StreamExt;
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_ipc::{Client, IpcError};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::mcp::{
    BRIDGE_CLOSE, BRIDGE_CLOSED, BRIDGE_OPEN, BRIDGE_RECV, BRIDGE_SEND, BridgeOpen, BridgeSession,
    MAX_BRIDGE_LINE,
};

fn ipc(e: IpcError) -> Error {
    match e {
        IpcError::Rpc(r) => match r.code {
            nugget_ipc::code::UNAUTHORIZED => Error::Unauthorized(r.message),
            _ => Error::Other(r.message),
        },
        other => Error::Other(other.to_string()),
    }
}

/// Bridge stdin/stdout to the daemon's MCP server for `client`, authenticating with `token`.
pub async fn run(paths: &Paths, client: &str, token: &str) -> Result<()> {
    run_with(
        paths,
        client,
        token,
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await
}

/// [`run`] over any byte streams.
pub async fn run_with<R, W>(
    paths: &Paths,
    client: &str,
    token: &str,
    input: R,
    mut output: W,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let conn = Client::connect(paths).await.map_err(|e| {
        Error::Other(format!(
            "nuggetd is not running (no answer on {}): {e}",
            nugget_ipc::socket_display(paths)
        ))
    })?;
    // Subscribe before opening, so the first message is never missed.
    let mut notes = conn.notifications();
    let open: BridgeSession = conn
        .call(
            BRIDGE_OPEN,
            &BridgeOpen {
                client: client.to_owned(),
                token: token.to_owned(),
            },
        )
        .await
        .map_err(ipc)?;
    let session = open.session;

    let sid = session.clone();
    let writer = tokio::spawn(async move {
        while let Some(n) = notes.next().await {
            if n.params.get("session").and_then(Value::as_str) != Some(sid.as_str()) {
                continue;
            }
            match n.method.as_str() {
                BRIDGE_RECV => {
                    let Some(line) = n.params.get("line").and_then(Value::as_str) else {
                        continue;
                    };
                    if output.write_all(line.as_bytes()).await.is_err()
                        || output.write_all(b"\n").await.is_err()
                        || output.flush().await.is_err()
                    {
                        return;
                    }
                }
                BRIDGE_CLOSED => return,
                _ => {}
            }
        }
    });

    let mut reader = BufReader::new(input);
    let pump = async {
        loop {
            let mut buf = Vec::new();
            let n = (&mut reader)
                .take(MAX_BRIDGE_LINE as u64 + 1)
                .read_until(b'\n', &mut buf)
                .await?;
            if n == 0 {
                return Ok::<(), Error>(());
            }
            if buf.len() > MAX_BRIDGE_LINE {
                return Err(Error::Invalid("MCP message too large".into()));
            }
            while matches!(buf.last(), Some(b'\n' | b'\r')) {
                buf.pop();
            }
            let line = String::from_utf8(buf)
                .map_err(|_| Error::Invalid("MCP message is not UTF-8".into()))?;
            if line.trim().is_empty() {
                continue;
            }
            conn.call_value(BRIDGE_SEND, json!({"session": session, "line": line}))
                .await
                .map_err(ipc)?;
        }
    };
    let mut writer = writer;
    let result = tokio::select! {
        r = pump => r,
        _ = &mut writer => Ok(()),
    };
    // Input ended (the app closed us) or the session ended: close, then let pending output out.
    let _ = conn
        .call_value(BRIDGE_CLOSE, json!({"session": session}))
        .await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), writer).await;
    result
}
