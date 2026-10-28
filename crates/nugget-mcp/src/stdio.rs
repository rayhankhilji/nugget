//! stdio (and any byte stream) transport, plus the `nuggetd mcp` bridge handshake.
//!
//! MCP apps launch `nuggetd mcp --client <id>` with the client's token in
//! [`TOKEN_ENV`]. That process is a thin pipe: it connects to the running daemon over local IPC,
//! sends a one-line [`BridgeHello`] carrying the client id and token, and then copies
//! stdin/stdout to and from the socket. The daemon authenticates the hello with
//! [`accept_bridge`] and serves MCP on the socket with [`McpServer::serve_io`], so scopes,
//! revocation, rate limits and audit are enforced in the daemon, never in the launched process.

use std::sync::Arc;

use nugget_core::ipc::{McpClientInfo, McpScopes};
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};

use crate::audit::Transport;
use crate::auth::ClientRegistry;
use crate::backend::KnowledgeBackend;
use crate::server::McpServer;

/// Environment variable holding the client token for `nuggetd mcp --client <id>`.
pub const TOKEN_ENV: &str = "NUGGET_MCP_TOKEN";
const MAX_HELLO_BYTES: usize = 4096;

impl McpServer {
    /// Serve MCP for one authenticated client over a byte stream (newline-delimited JSON-RPC),
    /// until the peer closes it.
    pub async fn serve_io<R, W>(
        &self,
        client: McpClientInfo,
        transport: Transport,
        reader: R,
        writer: W,
    ) -> Result<()>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let running = rmcp::serve_server(self.handler_for(client, transport), (reader, writer))
            .await
            .map_err(|e| Error::Other(format!("mcp session failed to start: {e}")))?;
        running
            .waiting()
            .await
            .map_err(|e| Error::Other(format!("mcp session ended abnormally: {e}")))?;
        Ok(())
    }

    /// Serve MCP on this process's stdin/stdout for one authenticated client.
    pub async fn serve_stdio(&self, client: McpClientInfo) -> Result<()> {
        self.serve_io(
            client,
            Transport::Stdio,
            tokio::io::stdin(),
            tokio::io::stdout(),
        )
        .await
    }
}

/// Serve MCP on stdin/stdout with fixed scopes and no registry (single-user embedding, tests).
/// The daemon's bridge uses [`McpServer::serve_io`] with a registry instead, so revocation applies.
pub async fn serve_stdio(
    backend: Arc<dyn KnowledgeBackend>,
    client_scopes: McpScopes,
) -> Result<()> {
    let client = McpClientInfo {
        id: "stdio".into(),
        name: "stdio".into(),
        scopes: client_scopes,
        created_at: time::OffsetDateTime::now_utc(),
        last_used: None,
    };
    McpServer::builder(backend)
        .build()
        .serve_stdio(client)
        .await
}

/// First line the `nuggetd mcp` bridge sends to the daemon.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeHello {
    /// Always 1.
    pub nugget_mcp_bridge: u32,
    pub client: String,
    pub token: String,
}

impl std::fmt::Debug for BridgeHello {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeHello")
            .field("client", &self.client)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// The daemon's one-line answer to a [`BridgeHello`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Bridge side: send the hello and wait for the daemon's verdict.
pub async fn bridge_connect<R, W>(
    reader: &mut R,
    writer: &mut W,
    client: &str,
    token: &str,
) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let hello = BridgeHello {
        nugget_mcp_bridge: 1,
        client: client.to_owned(),
        token: token.to_owned(),
    };
    let mut line = serde_json::to_vec(&hello)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await?;
    let reply: BridgeReply = serde_json::from_str(&read_line(reader).await?)?;
    if reply.ok {
        Ok(())
    } else {
        Err(Error::Unauthorized(
            reply.error.unwrap_or_else(|| "rejected by nuggetd".into()),
        ))
    }
}

/// Daemon side: read and authenticate the hello, answer it, and return the client to serve.
pub async fn accept_bridge<R, W>(
    registry: &ClientRegistry,
    reader: &mut R,
    writer: &mut W,
) -> Result<McpClientInfo>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let verdict = match serde_json::from_str::<BridgeHello>(&read_line(reader).await?) {
        Ok(h) if h.nugget_mcp_bridge == 1 => registry.authenticate_client(&h.client, &h.token)?,
        _ => None,
    };
    let reply = match &verdict {
        Some(_) => BridgeReply {
            ok: true,
            error: None,
        },
        None => BridgeReply {
            ok: false,
            error: Some("unknown or revoked MCP client, or wrong token".into()),
        },
    };
    let mut line = serde_json::to_vec(&reply)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await?;
    verdict.ok_or_else(|| Error::Unauthorized("mcp bridge authentication failed".into()))
}

async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<String> {
    let mut buf = Vec::new();
    let mut limited = (&mut *reader).take(MAX_HELLO_BYTES as u64);
    limited.read_until(b'\n', &mut buf).await?;
    if buf.last() != Some(&b'\n') {
        return Err(Error::Invalid(
            "mcp bridge handshake line missing or too long".into(),
        ));
    }
    String::from_utf8(buf).map_err(|_| Error::Invalid("mcp bridge handshake is not UTF-8".into()))
}
