//! Error types: [`RpcError`] is what travels on the wire, [`IpcError`] is what the client API returns.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// JSON-RPC 2.0 error codes used by Nugget.
///
/// `-32700..=-32600` are the codes reserved by the JSON-RPC 2.0 spec. `-32000..=-32099` is the
/// spec's "implementation-defined server error" range; Nugget maps each [`nugget_core::Error`]
/// variant to one code in it.
pub mod code {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON sent is not a valid request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal JSON-RPC error (handler panicked, response could not be encoded, ...).
    pub const INTERNAL_ERROR: i64 = -32603;

    /// Generic server error (`nugget_core::Error::Other`).
    pub const SERVER_ERROR: i64 = -32000;
    /// `nugget_core::Error::NotFound`.
    pub const NOT_FOUND: i64 = -32001;
    /// `nugget_core::Error::Unsupported`.
    pub const UNSUPPORTED: i64 = -32002;
    /// `nugget_core::Error::Provider`.
    pub const PROVIDER: i64 = -32003;
    /// `nugget_core::Error::Storage`.
    pub const STORAGE: i64 = -32004;
    /// `nugget_core::Error::Integrity`.
    pub const INTEGRITY: i64 = -32005;
    /// `nugget_core::Error::Cancelled`.
    pub const CANCELLED: i64 = -32006;
    /// `nugget_core::Error::Io`.
    pub const IO: i64 = -32007;
    /// A message exceeded [`MAX_MESSAGE_BYTES`](crate::MAX_MESSAGE_BYTES).
    pub const MESSAGE_TOO_LARGE: i64 = -32008;
}

/// A JSON-RPC 2.0 error object, exactly as serialized on the wire.
#[derive(Debug, Clone, PartialEq, Error, Serialize, Deserialize)]
#[error("rpc error {code}: {message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(code::PARSE_ERROR, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_REQUEST, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            code::METHOD_NOT_FOUND,
            format!("method not found: {method}"),
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_PARAMS, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(code::INTERNAL_ERROR, message)
    }

    pub fn too_large(len: usize) -> Self {
        Self::new(
            code::MESSAGE_TOO_LARGE,
            format!(
                "message of {len} bytes exceeds the {} byte limit",
                crate::MAX_MESSAGE_BYTES
            ),
        )
    }
}

impl From<nugget_core::Error> for RpcError {
    fn from(err: nugget_core::Error) -> Self {
        use nugget_core::Error as E;
        let code = match &err {
            E::NotFound(_) => code::NOT_FOUND,
            E::Invalid(_) | E::Serde(_) => code::INVALID_PARAMS,
            E::Integrity(_) => code::INTEGRITY,
            E::Unsupported(_) => code::UNSUPPORTED,
            E::Provider(_) => code::PROVIDER,
            E::Storage(_) => code::STORAGE,
            E::Io(_) => code::IO,
            E::Cancelled => code::CANCELLED,
            E::Other(_) => code::SERVER_ERROR,
        };
        // Send the inner message only, so the client can rebuild the same variant losslessly.
        let message = match err {
            E::NotFound(m)
            | E::Invalid(m)
            | E::Integrity(m)
            | E::Unsupported(m)
            | E::Provider(m)
            | E::Storage(m)
            | E::Other(m) => m,
            E::Io(e) => e.to_string(),
            E::Serde(e) => e.to_string(),
            E::Cancelled => "cancelled".to_owned(),
        };
        Self::new(code, message)
    }
}

impl From<RpcError> for nugget_core::Error {
    fn from(err: RpcError) -> Self {
        use nugget_core::Error as E;
        let m = err.message;
        match err.code {
            code::NOT_FOUND => E::NotFound(m),
            code::INVALID_PARAMS | code::INVALID_REQUEST | code::PARSE_ERROR => E::Invalid(m),
            code::INTEGRITY => E::Integrity(m),
            code::UNSUPPORTED | code::METHOD_NOT_FOUND => E::Unsupported(m),
            code::PROVIDER => E::Provider(m),
            code::STORAGE => E::Storage(m),
            code::IO => E::Io(std::io::Error::other(m)),
            code::CANCELLED => E::Cancelled,
            _ => E::Other(m),
        }
    }
}

/// Errors returned by the client API and by [`Notifier::send`](crate::Notifier::send).
#[derive(Debug, Error)]
pub enum IpcError {
    /// Could not reach the daemon (not running, wrong user, socket missing).
    #[error("cannot connect to nuggetd: {0}")]
    Connect(#[source] std::io::Error),
    /// The connection is closed (peer went away, or it was shut down).
    #[error("disconnected from nuggetd")]
    Disconnected,
    /// The remote side answered with a JSON-RPC error.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// An outgoing message would exceed [`MAX_MESSAGE_BYTES`](crate::MAX_MESSAGE_BYTES). Nothing
    /// was sent and the connection stays usable.
    #[error("message of {0} bytes exceeds the {max} byte limit", max = crate::MAX_MESSAGE_BYTES)]
    TooLarge(usize),
    /// Params could not be encoded, or a result could not be decoded into the requested type.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    /// The peer violated the protocol.
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl IpcError {
    /// True when the daemon is unreachable or went away, i.e. reconnecting may help.
    pub fn is_disconnect(&self) -> bool {
        matches!(self, Self::Connect(_) | Self::Disconnected | Self::Io(_))
    }

    /// The JSON-RPC error, if the server answered with one.
    pub fn as_rpc(&self) -> Option<&RpcError> {
        match self {
            Self::Rpc(e) => Some(e),
            _ => None,
        }
    }
}

impl From<IpcError> for nugget_core::Error {
    fn from(err: IpcError) -> Self {
        match err {
            IpcError::Rpc(e) => e.into(),
            IpcError::Connect(e) | IpcError::Io(e) => Self::Io(e),
            IpcError::Serde(e) => Self::Serde(e),
            other => Self::Other(other.to_string()),
        }
    }
}

/// Deserialize handler params, mapping failures to [`code::INVALID_PARAMS`].
pub fn parse_params<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))
}

/// Serialize a handler result, mapping failures to [`code::INTERNAL_ERROR`].
pub fn to_result<T: Serialize>(value: T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::internal(e.to_string()))
}
