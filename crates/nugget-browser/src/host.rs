//! The native-messaging host process: relays framed messages between the extension (stdio)
//! and the daemon (local IPC). It holds no state and makes no decisions; policy is enforced on
//! the daemon side before a request is ever sent.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use nugget_core::paths::Paths;
use nugget_ipc::{Client, IpcError};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc};

use crate::bridge::{AttachInfo, BridgeHub, ipc_method};
use crate::framing::{
    FrameError, MAX_FROM_EXTENSION, MAX_TO_EXTENSION, read_frame_async, write_frame_async,
};
use crate::protocol::{ErrorCode, ExtensionMessage, HostMessage};

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("daemon disconnected")]
    Disconnected,
    #[error(transparent)]
    Ipc(#[from] IpcError),
}

/// The host's connection to the daemon.
#[async_trait]
pub trait DaemonLink: Send + Sync {
    /// The next request to forward to the extension; `None` once the daemon is gone.
    async fn next_request(&self) -> Option<HostMessage>;
    /// Pass a reply or event from the extension to the daemon.
    async fn deliver(&self, msg: ExtensionMessage) -> Result<(), LinkError>;
}

/// Why a relay session ended.
#[derive(Debug, PartialEq, Eq)]
pub enum RelayEnd {
    /// The browser closed stdin (it does so when the extension disconnects): exit.
    ExtensionClosed,
    /// The daemon went away: reconnect, the extension is still there.
    DaemonGone,
}

/// Messages from the extension, read by one task for the life of the process so that a daemon
/// reconnect does not lose framing state.
pub fn spawn_extension_reader<R>(
    mut reader: R,
) -> mpsc::Receiver<Result<ExtensionMessage, FrameError>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        loop {
            let msg =
                read_frame_async::<_, ExtensionMessage>(&mut reader, MAX_FROM_EXTENSION).await;
            // A JSON error leaves the stream aligned (the whole frame was consumed): report and go
            // on. Anything else (oversized length, EOF, I/O) ends the stream.
            let fatal = !matches!(msg, Ok(_) | Err(FrameError::Json(_)));
            if tx.send(msg).await.is_err() || fatal {
                break;
            }
        }
    });
    rx
}

/// Relay until the extension or the daemon goes away.
pub async fn run_relay<W>(
    from_extension: &mut mpsc::Receiver<Result<ExtensionMessage, FrameError>>,
    to_extension: &mut W,
    link: &dyn DaemonLink,
) -> RelayEnd
where
    W: AsyncWrite + Unpin + Send,
{
    loop {
        tokio::select! {
            msg = from_extension.recv() => match msg {
                Some(Ok(msg)) => {
                    if link.deliver(msg).await.is_err() {
                        return RelayEnd::DaemonGone;
                    }
                }
                Some(Err(FrameError::Json(e))) => {
                    tracing::warn!(error = %e, "dropping malformed message from the extension");
                }
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "extension stream ended");
                    return RelayEnd::ExtensionClosed;
                }
                None => return RelayEnd::ExtensionClosed,
            },
            req = link.next_request() => match req {
                Some(req) => {
                    match write_frame_async(to_extension, &req, MAX_TO_EXTENSION).await {
                        Ok(()) => {}
                        Err(FrameError::TooLarge { len, cap }) => {
                            let reply = ExtensionMessage::error_reply(
                                req.id,
                                ErrorCode::TooLarge,
                                format!("request of {len} bytes exceeds the {cap}-byte cap"),
                            );
                            if link.deliver(reply).await.is_err() {
                                return RelayEnd::DaemonGone;
                            }
                        }
                        Err(_) => return RelayEnd::ExtensionClosed,
                    }
                }
                None => return RelayEnd::DaemonGone,
            },
        }
    }
}

/// [`DaemonLink`] over the daemon's local socket (`nugget-ipc`).
pub struct IpcDaemonLink {
    client: Client,
    requests: Mutex<mpsc::Receiver<HostMessage>>,
}

impl IpcDaemonLink {
    pub async fn connect(paths: &Paths, info: AttachInfo) -> Result<Self, IpcError> {
        Ok(Self::attach(Client::connect(paths).await?, info))
    }

    /// Attach over an existing client connection.
    pub fn attach(client: Client, info: AttachInfo) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let mut notes = client.notifications();
        tokio::spawn(async move {
            while let Some(n) = notes.next().await {
                if n.method != ipc_method::REQUEST {
                    continue;
                }
                match n.params_as::<HostMessage>() {
                    Ok(msg) => {
                        if tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "malformed request from the daemon"),
                }
            }
        });
        // The attach call stays open for the life of the session; the daemon pushes requests as
        // notifications on this connection meanwhile.
        let attach_client = client.clone();
        tokio::spawn(async move {
            let res: Result<serde_json::Value, _> =
                attach_client.call(ipc_method::ATTACH, &info).await;
            if let Err(e) = res {
                tracing::info!(error = %e, "daemon ended the browser session");
            }
        });
        Self {
            client,
            requests: Mutex::new(rx),
        }
    }
}

#[async_trait]
impl DaemonLink for IpcDaemonLink {
    async fn next_request(&self) -> Option<HostMessage> {
        let mut rx = self.requests.lock().await;
        tokio::select! {
            msg = rx.recv() => msg,
            () = self.client.closed() => None,
        }
    }

    async fn deliver(&self, msg: ExtensionMessage) -> Result<(), LinkError> {
        let _: serde_json::Value = self.client.call(ipc_method::DELIVER, &msg).await?;
        Ok(())
    }
}

/// [`DaemonLink`] straight into an in-process [`BridgeHub`] (tests, and embedding the hub in the
/// same process as the host).
pub struct HubLink {
    hub: Arc<BridgeHub>,
    generation: u64,
    requests: Mutex<mpsc::Receiver<HostMessage>>,
}

impl HubLink {
    pub fn attach(hub: Arc<BridgeHub>, info: AttachInfo) -> Self {
        let (generation, rx) = hub.attach(info);
        Self {
            hub,
            generation,
            requests: Mutex::new(rx),
        }
    }
}

impl Drop for HubLink {
    fn drop(&mut self) {
        self.hub.detach(self.generation);
    }
}

#[async_trait]
impl DaemonLink for HubLink {
    async fn next_request(&self) -> Option<HostMessage> {
        self.requests.lock().await.recv().await
    }

    async fn deliver(&self, msg: ExtensionMessage) -> Result<(), LinkError> {
        self.hub.deliver(msg);
        Ok(())
    }
}
