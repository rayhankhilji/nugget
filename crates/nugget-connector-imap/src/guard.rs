//! A read-side guard between TLS and `async-imap`.
//!
//! `async-imap` sizes its buffer from the server's `{N}` literal headers before any of the
//! literal arrives (zero-filling up to 512 MiB), and buffers whole responses before we see them.
//! [`Guarded`] sits underneath it and enforces two limits on the raw byte stream:
//! - **Literal headers.** `{N}\r\n` tokens are found as they stream past (across read
//!   boundaries, skipping the bodies of literals already announced) and any `N` above the
//!   connection's literal cap, or above what is left of the current command's budget, fails the
//!   read *before* `async-imap` sees the header.
//! - **Per-command budget.** The command layer resets a byte budget before each command; a
//!   response stream that exceeds it fails the read.
//!
//! A violation is remembered in [`Limits`] so the command layer can report it clearly instead of
//! as a generic I/O error.

use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Digit runs inside `{...}` are counted up to this length; longer runs only matter for their
/// value, which then saturates.
const MAX_DIGITS: u8 = 32;

/// Limits shared between a [`Guarded`] stream and the command layer.
#[derive(Debug)]
pub(crate) struct Limits {
    max_literal: u64,
    budget: AtomicU64,
    used: AtomicU64,
    violation: Mutex<Option<String>>,
}

impl Limits {
    pub(crate) fn new(max_literal: u64, budget: u64) -> Self {
        Self {
            max_literal,
            budget: AtomicU64::new(budget),
            used: AtomicU64::new(0),
            violation: Mutex::new(None),
        }
    }

    /// Start a new command: `budget` bytes may arrive until the next reset.
    pub(crate) fn reset(&self, budget: u64) {
        self.budget.store(budget, Ordering::Relaxed);
        self.used.store(0, Ordering::Relaxed);
    }

    /// The limit violation that broke the stream, if any.
    pub(crate) fn violation(&self) -> Option<String> {
        self.violation.lock().ok().and_then(|v| v.clone())
    }

    fn fail(&self, msg: String) -> io::Error {
        if let Ok(mut v) = self.violation.lock() {
            v.get_or_insert_with(|| msg.clone());
        }
        io::Error::new(io::ErrorKind::InvalidData, msg)
    }
}

/// Scanner state for `{N}\r\n` literal headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// Ordinary response text.
    Text,
    /// After `{`: the digits so far.
    Brace { n: u64, digits: u8 },
    /// After `{N}`.
    Close(u64),
    /// After `{N}\r`.
    Cr(u64),
    /// Inside a literal body: this many bytes to skip unscanned.
    Skip(u64),
}

impl Scan {
    /// Feed `data`. `limit(offset)` gives the largest literal acceptable once the header ends at
    /// byte `offset` of `data`. Returns the first oversized `N` found.
    fn feed(&mut self, data: &[u8], limit: impl Fn(usize) -> u64) -> Result<(), u64> {
        let mut i = 0;
        while i < data.len() {
            if let Scan::Skip(rem) = *self {
                let take = rem.min((data.len() - i) as u64);
                i += take as usize;
                *self = if rem == take {
                    Scan::Text
                } else {
                    Scan::Skip(rem - take)
                };
                continue;
            }
            let b = data[i];
            *self = match (*self, b) {
                // Saturating, and leading zeros never stop the scan: `imap-proto` accepts any
                // digit run whose value fits a u32 (`{000…0001000000}` is a 1 MB literal), so
                // the scanner must see the same value rather than give up on a long run.
                (Scan::Brace { n, digits }, b'0'..=b'9') => Scan::Brace {
                    n: n.saturating_mul(10).saturating_add(u64::from(b - b'0')),
                    digits: digits.saturating_add(1).min(MAX_DIGITS),
                },
                (Scan::Brace { n, digits }, b'}') if digits > 0 => Scan::Close(n),
                (Scan::Close(n), b'\r') => Scan::Cr(n),
                (Scan::Cr(n), b'\n') => {
                    if n > limit(i + 1) {
                        return Err(n);
                    }
                    if n == 0 { Scan::Text } else { Scan::Skip(n) }
                }
                (_, b'{') => Scan::Brace { n: 0, digits: 0 },
                _ => Scan::Text,
            };
            i += 1;
        }
        Ok(())
    }
}

/// An `AsyncRead + AsyncWrite` wrapper enforcing [`Limits`] on everything read.
pub(crate) struct Guarded<S> {
    inner: S,
    limits: std::sync::Arc<Limits>,
    scan: Scan,
}

impl<S> std::fmt::Debug for Guarded<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guarded")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl<S> Guarded<S> {
    pub(crate) fn new(inner: S, limits: std::sync::Arc<Limits>) -> Self {
        Self {
            inner,
            limits,
            scan: Scan::Text,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Guarded<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if let Some(v) = this.limits.violation() {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, v)));
        }
        let before = buf.filled().len();
        ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        let data = &buf.filled()[before..];
        let limits = &*this.limits;
        let budget = limits.budget.load(Ordering::Relaxed);
        let used_before = limits.used.fetch_add(data.len() as u64, Ordering::Relaxed);
        let found = this.scan.feed(data, |offset| {
            let left = budget.saturating_sub(used_before + offset as u64);
            limits.max_literal.min(left)
        });
        if let Err(n) = found {
            buf.set_filled(before);
            return Poll::Ready(Err(limits.fail(format!(
                "IMAP server announced a {n}-byte literal; the limit is {} bytes",
                limits.max_literal.min(budget)
            ))));
        }
        if used_before + data.len() as u64 > budget {
            buf.set_filled(before);
            return Poll::Ready(Err(limits.fail(format!(
                "IMAP response exceeded {budget} bytes for one command"
            ))));
        }
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Guarded<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// A server-controlled string made safe for logs and IPC: at most `max` characters, with
/// control characters (and the backslash) escaped.
pub(crate) fn clip(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (count, c) in s.chars().enumerate() {
        if count >= max {
            out.push('…');
            break;
        }
        if c.is_control() || c == '\\' {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_chunks(chunks: &[&[u8]], limit: u64) -> Result<(), u64> {
        let mut s = Scan::Text;
        for c in chunks {
            s.feed(c, |_| limit)?;
        }
        Ok(())
    }

    #[test]
    fn literal_headers_are_found_across_chunk_boundaries() {
        let msg: &[u8] = b"* 1 FETCH (UID 1 BODY[] {5000}\r\n";
        for split in 0..msg.len() {
            let (a, b) = msg.split_at(split);
            assert_eq!(scan_chunks(&[a, b], 4999), Err(5000), "split at {split}");
            assert_eq!(scan_chunks(&[a, b], 5000), Ok(()), "split at {split}");
        }
        // Byte by byte, too.
        let bytes: Vec<&[u8]> = msg.chunks(1).collect();
        assert_eq!(scan_chunks(&bytes, 10), Err(5000));
    }

    #[test]
    fn literal_bodies_are_skipped_and_non_headers_ignored() {
        // A literal whose body contains something that looks like a huge header.
        let msg: &[u8] = b"* 1 FETCH (BODY[] {20}\r\n{99999999}\r\nabcdefgh)\r\n";
        assert_eq!(scan_chunks(&[msg], 100), Ok(()));
        // After the literal, scanning resumes.
        let msg2: &[u8] = b"* 1 FETCH (BODY[] {3}\r\nabc)\r\n* 2 FETCH (BODY[] {500}\r\n";
        assert_eq!(scan_chunks(&[msg2], 100), Err(500));
        // `{N}` not followed by CRLF, empty braces and overlong digit runs are not headers.
        assert_eq!(scan_chunks(&[b"* OK {99999} x\r\n{}\r\n"], 10), Ok(()));
        // An absurdly long digit run saturates and is refused (imap-proto rejects it anyway).
        assert_eq!(
            scan_chunks(&[b"{99999999999999999999999999}\r\n"], 10),
            Err(u64::MAX)
        );
        // A brace restarts the scan.
        assert_eq!(scan_chunks(&[b"{{12}\r\n"], 10), Err(12));
    }

    #[test]
    fn leading_zeros_do_not_hide_a_literal_size() {
        // imap-proto parses this as a 100 MB literal and async-imap sizes its buffer from it;
        // the scanner must not give up after 19 digits.
        let msg: &[u8] = b"* 1 FETCH (UID 1 BODY[] {0000000000000000000000000100000000}\r\n";
        assert_eq!(scan_chunks(&[msg], 1000), Err(100_000_000));
        let bytes: Vec<&[u8]> = msg.chunks(3).collect();
        assert_eq!(scan_chunks(&bytes, 1000), Err(100_000_000));
        // Small, zero-padded literals are still fine.
        assert_eq!(
            scan_chunks(&[b"{0000000000000000000000005}\r\n"], 10),
            Ok(())
        );
    }

    #[tokio::test]
    async fn per_command_budget_and_literal_cap_fail_the_read() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut server, client) = tokio::io::duplex(64 * 1024);
        let limits = std::sync::Arc::new(Limits::new(1000, 100));
        let mut g = Guarded::new(client, limits.clone());
        server.write_all(&[b'x'; 60]).await.unwrap();
        let mut buf = vec![0u8; 4096];
        assert_eq!(g.read(&mut buf).await.unwrap(), 60);
        server.write_all(&[b'x'; 60]).await.unwrap();
        let err = g.read(&mut buf).await.unwrap_err();
        assert!(err.to_string().contains("exceeded 100 bytes"), "{err}");
        assert!(limits.violation().is_some());
        // Broken for good, even after a reset.
        limits.reset(1_000_000);
        assert!(g.read(&mut buf).await.is_err());

        // The literal cap applies regardless of the budget.
        let (mut server, client) = tokio::io::duplex(64 * 1024);
        let limits = std::sync::Arc::new(Limits::new(1000, 1_000_000));
        let mut g = Guarded::new(client, limits);
        server
            .write_all(b"* 1 FETCH (BODY[] {1001}\r\n")
            .await
            .unwrap();
        let err = g.read(&mut buf).await.unwrap_err();
        assert!(err.to_string().contains("1001-byte literal"), "{err}");
    }

    #[test]
    fn clip_bounds_and_escapes() {
        assert_eq!(clip("a\r\nb\0c", 100), "a\\r\\nb\\u{0}c");
        let long = "x".repeat(1000);
        assert_eq!(clip(&long, 200).chars().count(), 201);
    }
}
