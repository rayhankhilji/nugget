//! Native-messaging framing: a 32-bit length in native byte order, then that many bytes of UTF-8
//! JSON. Every supported target (x86-64, aarch64) is little-endian, so native order is
//! little-endian; we use `to_ne_bytes` to state the contract the browsers document.
//!
//! Size caps: Chrome refuses host-to-extension messages over 1 MiB ([`MAX_TO_EXTENSION`]). It
//! allows 64 MiB the other way; we cap extension-to-host messages at
//! [`MAX_FROM_EXTENSION`] (the extension caps itself at the same value) and treat a larger
//! length as a protocol violation that ends the session, since the stream can't be trusted after.

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest message the host may send to the extension (Chrome's hard limit).
pub const MAX_TO_EXTENSION: usize = 1024 * 1024;
/// Largest message the host accepts from the extension.
pub const MAX_FROM_EXTENSION: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("message of {len} bytes exceeds the {cap}-byte cap")]
    TooLarge { len: usize, cap: usize },
    #[error("stream closed")]
    Closed,
    #[error("stream closed mid-message")]
    Truncated,
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Serialize `value` into a frame, refusing anything over `cap`.
pub fn encode<T: Serialize + ?Sized>(value: &T, cap: usize) -> Result<Vec<u8>, FrameError> {
    let body = serde_json::to_vec(value)?;
    if body.len() > cap {
        return Err(FrameError::TooLarge {
            len: body.len(),
            cap,
        });
    }
    let len = u32::try_from(body.len()).map_err(|_| FrameError::TooLarge {
        len: body.len(),
        cap,
    })?;
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&len.to_ne_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

fn check_len(header: [u8; 4], cap: usize) -> Result<usize, FrameError> {
    let len = u32::from_ne_bytes(header) as usize;
    if len > cap {
        return Err(FrameError::TooLarge { len, cap });
    }
    Ok(len)
}

/// Read one frame. `Err(Closed)` on a clean end of stream before a header.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R, cap: usize) -> Result<T, FrameError> {
    let mut header = [0u8; 4];
    match r.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = check_len(header, cap)?;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => FrameError::Truncated,
        _ => e.into(),
    })?;
    Ok(serde_json::from_slice(&body)?)
}

/// Write one frame and flush.
pub fn write_frame<W: Write, T: Serialize + ?Sized>(
    w: &mut W,
    value: &T,
    cap: usize,
) -> Result<(), FrameError> {
    w.write_all(&encode(value, cap)?)?;
    w.flush()?;
    Ok(())
}

/// Async [`read_frame`].
pub async fn read_frame_async<R: AsyncRead + Unpin, T: DeserializeOwned>(
    r: &mut R,
    cap: usize,
) -> Result<T, FrameError> {
    let mut header = [0u8; 4];
    match r.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = check_len(header, cap)?;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await.map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => FrameError::Truncated,
        _ => e.into(),
    })?;
    Ok(serde_json::from_slice(&body)?)
}

/// Async [`write_frame`].
pub async fn write_frame_async<W: AsyncWrite + Unpin, T: Serialize + ?Sized>(
    w: &mut W,
    value: &T,
    cap: usize,
) -> Result<(), FrameError> {
    w.write_all(&encode(value, cap)?).await?;
    w.flush().await?;
    Ok(())
}
