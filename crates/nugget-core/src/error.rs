use thiserror::Error;

/// Cross-crate error. Crates may keep richer internal errors but convert to this at their public API.
#[derive(Debug, Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("integrity check failed: {0}")]
    Integrity(String),
    #[error("capability unsupported: {0}")]
    Unsupported(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("network error: {0}")]
    Network(String),
    /// The remote asked us to back off. `retry_after_ms` is the server hint, if any.
    #[error("rate limited{}", .retry_after_ms.map(|ms| format!(" (retry after {ms} ms)")).unwrap_or_default())]
    RateLimited { retry_after_ms: Option<u64> },
    /// The source or provider needs the user to (re)authorize or supply a credential.
    #[error("authorization required: {0}")]
    Unauthorized(String),
    #[error("insufficient disk space: need {needed} bytes, {available} available")]
    InsufficientSpace { needed: u64, available: u64 },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
