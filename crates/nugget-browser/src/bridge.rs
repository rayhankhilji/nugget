//! The daemon side: [`BrowserBridge`] is how the agent reaches the extension, and [`BridgeHub`]
//! is a ready-made implementation the daemon can hold.
//!
//! ```text
//! agent tool -> BrowserController -> BrowserBridge (BridgeHub, in nuggetd)
//!     -> IPC notification `browser.host.request` -> nugget-browser-host (spawned by the browser)
//!     -> native messaging (stdio) -> extension background -> content script
//! replies and events travel back via the IPC call `browser.host.deliver`.
//! ```
//!
//! Daemon wiring (no daemon changes are made by this crate): hold an `Arc<BridgeHub>`, route
//! methods starting with `browser.host.` to [`HubHandler::call`], and give the hub a
//! [`KillSwitch`] connected to the policy engine.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nugget_ipc::{Notifier, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::protocol::{BrowserEvent, ErrorCode, ExtensionMessage, HostMessage, HostRequest};

/// IPC method and notification names between the host process and the daemon. Proposed for
/// `nugget_core::ipc` once the daemon wires the hub in.
pub mod ipc_method {
    /// Host -> daemon, long-lived: registers the host; returns when the daemon detaches it.
    pub const ATTACH: &str = "browser.host.attach";
    /// Host -> daemon: a reply or event from the extension ([`super::ExtensionMessage`]).
    pub const DELIVER: &str = "browser.host.deliver";
    /// Daemon -> host notification: a [`super::HostMessage`] to forward to the extension.
    pub const REQUEST: &str = "browser.host.request";
}

/// Default time to wait for the extension to answer one request.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BridgeError {
    #[error("no browser is connected (is the Nugget extension installed and the browser open?)")]
    NotConnected,
    #[error("the browser did not answer in time")]
    Timeout,
    #[error("the browser connection closed")]
    Disconnected,
    #[error("browser error ({code:?}): {message}")]
    Extension { code: ErrorCode, message: String },
    #[error("malformed reply from the browser: {0}")]
    Malformed(String),
}

/// The transport from the agent to the extension. The daemon implements it (see
/// [`BridgeHub`]); tests use fakes.
#[async_trait]
pub trait BrowserBridge: Send + Sync {
    /// Send `request` to the extension and return its `result`.
    async fn request(&self, request: HostRequest) -> Result<Value, BridgeError>;
}

/// Engages the global kill switch. The daemon connects this to the policy engine so that the
/// Stop button in the page halts every executor, not just the browser.
pub trait KillSwitch: Send + Sync {
    fn engage(&self, reason: &str);
}

/// What the host process says about itself when it attaches.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AttachInfo {
    /// The calling extension: `chrome-extension://<id>/` or a Firefox add-on id.
    pub origin: String,
    pub host_version: String,
}

struct Attached {
    generation: u64,
    tx: mpsc::Sender<HostMessage>,
    info: AttachInfo,
}

/// Daemon-side bridge: correlates requests and replies for the one attached host process.
pub struct BridgeHub {
    next_id: AtomicU64,
    generation: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<ExtensionMessage>>>,
    attached: Mutex<Option<Attached>>,
    events: broadcast::Sender<BrowserEvent>,
    kill_switch: Option<Arc<dyn KillSwitch>>,
    timeout: Duration,
}

impl BridgeHub {
    pub fn new(kill_switch: Option<Arc<dyn KillSwitch>>) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            attached: Mutex::new(None),
            events: broadcast::channel(64).0,
            kill_switch,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Register a host. A newer host replaces an older one (its receiver closes). Returns the
    /// generation (for [`detach`](Self::detach)) and the stream of requests to forward.
    pub fn attach(&self, info: AttachInfo) -> (u64, mpsc::Receiver<HostMessage>) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::channel(32);
        *lock(&self.attached) = Some(Attached {
            generation,
            tx,
            info,
        });
        (generation, rx)
    }

    /// Forget host `generation` if it is still the attached one, failing its pending requests.
    pub fn detach(&self, generation: u64) {
        let mut attached = lock(&self.attached);
        if attached
            .as_ref()
            .is_some_and(|a| a.generation == generation)
        {
            *attached = None;
            // Dropping the senders fails every waiting request with `Disconnected`.
            lock(&self.pending).clear();
        }
    }

    pub fn attached_info(&self) -> Option<AttachInfo> {
        lock(&self.attached).as_ref().map(|a| a.info.clone())
    }

    pub fn is_connected(&self) -> bool {
        lock(&self.attached).is_some()
    }

    /// Events from the extension (user stop, permission changes).
    pub fn subscribe(&self) -> broadcast::Receiver<BrowserEvent> {
        self.events.subscribe()
    }

    /// A message from the extension, relayed by the host.
    pub fn deliver(&self, msg: ExtensionMessage) {
        match msg {
            ExtensionMessage::Reply { id, .. } => {
                if let Some(tx) = lock(&self.pending).remove(&id) {
                    let _ = tx.send(msg);
                } else {
                    tracing::debug!(id, "reply for an unknown or expired request");
                }
            }
            ExtensionMessage::Event(event) => {
                if let BrowserEvent::UserStop { reason } = &event {
                    tracing::warn!(reason, "user pressed Stop in the browser");
                    if let Some(k) = &self.kill_switch {
                        k.engage(reason);
                    }
                }
                let _ = self.events.send(event);
            }
        }
    }
}

#[async_trait]
impl BrowserBridge for BridgeHub {
    async fn request(&self, request: HostRequest) -> Result<Value, BridgeError> {
        let tx = lock(&self.attached)
            .as_ref()
            .map(|a| a.tx.clone())
            .ok_or(BridgeError::NotConnected)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = oneshot::channel();
        lock(&self.pending).insert(id, reply_tx);
        if tx.send(HostMessage { id, request }).await.is_err() {
            lock(&self.pending).remove(&id);
            return Err(BridgeError::Disconnected);
        }
        let reply = match tokio::time::timeout(self.timeout, reply_rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => return Err(BridgeError::Disconnected),
            Err(_) => {
                lock(&self.pending).remove(&id);
                return Err(BridgeError::Timeout);
            }
        };
        reply_result(reply)
    }
}

/// Turn a reply into the bridge's result type.
pub fn reply_result(reply: ExtensionMessage) -> Result<Value, BridgeError> {
    match reply {
        ExtensionMessage::Reply { error: Some(e), .. } => Err(BridgeError::Extension {
            code: e.code,
            message: e.message,
        }),
        ExtensionMessage::Reply { result, .. } => Ok(result.unwrap_or(Value::Null)),
        ExtensionMessage::Event(_) => {
            Err(BridgeError::Malformed("event in place of a reply".into()))
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Serves the `browser.host.*` IPC methods for a [`BridgeHub`]. The daemon's
/// [`nugget_ipc::Handler`] delegates those methods here.
pub struct HubHandler {
    hub: Arc<BridgeHub>,
}

impl HubHandler {
    pub fn new(hub: Arc<BridgeHub>) -> Self {
        Self { hub }
    }

    pub fn handles(method: &str) -> bool {
        method.starts_with("browser.host.")
    }

    pub async fn call(
        &self,
        method: &str,
        params: Value,
        notify: Notifier,
    ) -> Result<Value, RpcError> {
        match method {
            ipc_method::ATTACH => {
                let info: AttachInfo = nugget_ipc::parse_params(params)?;
                let (generation, mut rx) = self.hub.attach(info);
                loop {
                    tokio::select! {
                        msg = rx.recv() => match msg {
                            Some(msg) => {
                                if notify.send(ipc_method::REQUEST, &msg).await.is_err() {
                                    break;
                                }
                            }
                            None => break, // replaced by a newer host
                        },
                        () = notify.closed() => break,
                    }
                }
                self.hub.detach(generation);
                Ok(Value::Null)
            }
            ipc_method::DELIVER => {
                let msg: ExtensionMessage = nugget_ipc::parse_params(params)?;
                self.hub.deliver(msg);
                Ok(Value::Null)
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

#[async_trait]
impl nugget_ipc::Handler for HubHandler {
    async fn call(&self, method: &str, params: Value, notify: Notifier) -> Result<Value, RpcError> {
        HubHandler::call(self, method, params, notify).await
    }
}
