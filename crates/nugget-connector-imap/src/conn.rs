//! Connection setup (TCP, TLS, STARTTLS, login), byte metering and a small command layer over
//! `async-imap`'s framing. Every response is handed to a callback as a parsed
//! [`imap_proto::Response`], so nothing is silently dropped (unlike the bounded unsolicited
//! channel) and large literals are copied out once.

use std::collections::HashSet;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use async_imap::imap_proto::{
    self, AttributeValue, MailboxDatum, MessageSection, NameAttribute, Response, ResponseCode,
    SectionPath, Status,
};
use nugget_connector::CancellationToken;
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::{Error, Result};
use time::OffsetDateTime;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::ClientConfig;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::guard::{Guarded, Limits, clip};
use crate::settings::TlsMode;

/// Longest line accepted during the plaintext STARTTLS preamble.
const MAX_PREAMBLE_LINE: usize = 8 * 1024;
/// Upper bound on untagged responses for one command (guards against a server that never ends).
const MAX_RESPONSES_PER_COMMAND: usize = 2_000_000;
/// Body fetches may take this many times the normal per-response timeout.
const BODY_TIMEOUT_FACTOR: u32 = 10;
/// Bytes one non-body command (LIST, SEARCH, metadata FETCH, ...) may receive.
pub(crate) const META_BUDGET: u64 = 64 * 1024 * 1024;
/// Protocol overhead allowed on top of the body bytes of one body FETCH.
const BODY_BUDGET_SLACK: u64 = 4 * 1024 * 1024;
/// Allowance on top of `max_message_mb` for one literal (headers-only fetches, size drift).
pub(crate) const LITERAL_SLACK: u64 = 1024 * 1024;
/// Longest server-supplied text kept in an error message.
const MAX_ERROR_TEXT: usize = 200;

// ---------------------------------------------------------------------------------------------
// Byte metering
// ---------------------------------------------------------------------------------------------

/// Bytes moved over one connection (wire bytes, TLS records included).
#[derive(Debug, Default)]
pub(crate) struct Counters {
    sent: AtomicU64,
    received: AtomicU64,
}

impl Counters {
    pub(crate) fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }
    pub(crate) fn received(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }
}

/// AsyncRead/AsyncWrite wrapper that counts bytes into [`Counters`].
#[derive(Debug)]
pub(crate) struct Metered<S> {
    inner: S,
    counters: Arc<Counters>,
}

impl<S> Metered<S> {
    pub(crate) fn new(inner: S, counters: Arc<Counters>) -> Self {
        Self { inner, counters }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Metered<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        ready!(Pin::new(&mut self.inner).poll_read(cx, buf))?;
        let n = (buf.filled().len() - before) as u64;
        self.counters.received.fetch_add(n, Ordering::Relaxed);
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Metered<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let n = ready!(Pin::new(&mut self.inner).poll_write(cx, buf))?;
        self.counters.sent.fetch_add(n as u64, Ordering::Relaxed);
        Poll::Ready(Ok(n))
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let n = ready!(Pin::new(&mut self.inner).poll_write_vectored(cx, bufs))?;
        self.counters.sent.fetch_add(n as u64, Ordering::Relaxed);
        Poll::Ready(Ok(n))
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

/// Reports the bytes counted on a connection to the egress ledger, as deltas. Flushed per folder
/// batch and once more on drop, so a failed session is still accounted for.
pub(crate) struct EgressMeter {
    counters: Arc<Counters>,
    egress: Arc<dyn EgressSink>,
    host: String,
    feature: &'static str,
    reported_sent: u64,
    reported_received: u64,
}

impl EgressMeter {
    pub(crate) fn new(egress: Arc<dyn EgressSink>, host: String, feature: &'static str) -> Self {
        Self {
            counters: Arc::new(Counters::default()),
            egress,
            host,
            feature,
            reported_sent: 0,
            reported_received: 0,
        }
    }

    pub(crate) fn counters(&self) -> Arc<Counters> {
        self.counters.clone()
    }

    pub(crate) fn flush(&mut self) {
        let sent = self.counters.sent();
        let received = self.counters.received();
        let (ds, dr) = (sent - self.reported_sent, received - self.reported_received);
        if ds == 0 && dr == 0 {
            return;
        }
        self.reported_sent = sent;
        self.reported_received = received;
        self.egress.record(EgressEvent {
            at: OffsetDateTime::now_utc(),
            host: self.host.clone(),
            feature: self.feature.to_string(),
            bytes_sent: ds,
            bytes_received: dr,
            items: Vec::new(),
        });
    }
}

impl Drop for EgressMeter {
    fn drop(&mut self) {
        self.flush();
    }
}

// ---------------------------------------------------------------------------------------------
// Connecting
// ---------------------------------------------------------------------------------------------

pub(crate) type Stream = Guarded<tokio_rustls::client::TlsStream<Metered<TcpStream>>>;

/// Where and how to connect.
#[derive(Clone)]
pub(crate) struct Endpoint {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub username: String,
    pub tls_config: Arc<ClientConfig>,
    pub io_timeout: Duration,
    /// Largest literal (`{N}`) accepted from the server.
    pub max_literal: u64,
}

fn net(msg: impl std::fmt::Display) -> Error {
    Error::Network(msg.to_string())
}

/// Map an `async-imap` error to a fixed message. Its parse errors embed the whole receive
/// buffer (server-controlled, possibly megabytes), so only a short escaped excerpt is kept.
fn imap_err(limits: &Limits, e: async_imap::error::Error) -> Error {
    use async_imap::error::Error as E;
    match e {
        E::Io(e) => read_err(limits, e),
        E::ConnectionLost => Error::Network("imap connection lost".into()),
        E::Parse(_) => {
            Error::Network("IMAP server sent a response that could not be parsed".into())
        }
        E::Bad(m) | E::No(m) => Error::Network(format!(
            "IMAP command rejected: {}",
            clip(&m, MAX_ERROR_TEXT)
        )),
        E::Validate(_) => Error::Invalid("IMAP command argument contains a line break".into()),
        _ => Error::Network("IMAP protocol error".into()),
    }
}

/// Map an I/O error from the IMAP stream. A guard violation is reported as such; a parse error
/// (`"<nom error> during parsing of <buffer>"`) keeps only the start of the offending data.
pub(crate) fn read_err(limits: &Limits, e: io::Error) -> Error {
    if let Some(v) = limits.violation() {
        return Error::Network(v);
    }
    let text = e.to_string();
    if let Some((_, data)) = text.split_once(" during parsing of ") {
        return Error::Network(format!(
            "IMAP server sent a malformed response: {}",
            clip(data, MAX_ERROR_TEXT)
        ));
    }
    Error::Network(format!("imap i/o: {}", clip(&text, MAX_ERROR_TEXT)))
}

async fn with_timeout<T>(
    dur: Duration,
    what: &str,
    fut: impl Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(dur, fut).await {
        Ok(r) => r,
        Err(_) => Err(Error::Network(format!("imap {what} timed out"))),
    }
}

/// Reads one CRLF-terminated line byte by byte (no read-ahead, so nothing sent before the TLS
/// handshake can leak into the TLS session).
async fn read_plain_line(s: &mut Metered<TcpStream>) -> Result<String> {
    let mut line = Vec::with_capacity(128);
    loop {
        let b = s.read_u8().await.map_err(net)?;
        line.push(b);
        if line.ends_with(b"\r\n") {
            line.truncate(line.len() - 2);
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
        if line.len() > MAX_PREAMBLE_LINE {
            return Err(net("imap greeting line too long"));
        }
    }
}

/// The server's verdict on a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Done {
    Ok,
    No(String),
    Bad(String),
}

/// A logged-in IMAP session.
pub(crate) struct Imap {
    session: async_imap::Session<Stream>,
    limits: Arc<Limits>,
    timeout: Duration,
    /// Bytes the next command may receive (see [`Limits`]).
    budget: u64,
    caps: HashSet<String>,
    pub condstore: bool,
    pub qresync: bool,
}

/// What EXAMINE reported.
#[derive(Debug, Clone, Default)]
pub(crate) struct Selected {
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
    pub exists: Option<u32>,
    pub highest_modseq: Option<u64>,
}

/// One mailbox from LIST.
#[derive(Debug, Clone)]
pub(crate) struct ListedMailbox {
    pub name: String,
    pub delimiter: Option<String>,
    pub noselect: bool,
    pub special: Option<Special>,
    /// A virtual view of mail stored elsewhere (RFC 6154 `\All`, `\Flagged`; RFC 8457
    /// `\Important`), e.g. Gmail's "All Mail", "Starred" and "Important".
    pub virtual_view: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Special {
    Sent,
    Trash,
    Junk,
    All,
    Drafts,
    Other,
}

/// Per-message attributes from a metadata FETCH.
#[derive(Debug, Clone, Default)]
pub(crate) struct MessageMeta {
    pub uid: u32,
    pub flags: Vec<String>,
    pub modseq: Option<u64>,
    pub size: Option<u32>,
    pub internal_date: Option<OffsetDateTime>,
}

/// Result of `UID FETCH ... (CHANGEDSINCE n [VANISHED])`.
#[derive(Debug, Default)]
pub(crate) struct Changes {
    pub changed: Vec<MessageMeta>,
    pub vanished: Vec<std::ops::RangeInclusive<u32>>,
}

impl Imap {
    /// Connect, secure the channel, and log in. A rejected login is `Unauthorized`.
    pub(crate) async fn connect(
        ep: &Endpoint,
        password: &str,
        counters: Arc<Counters>,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        tokio::select! {
            r = with_timeout(ep.io_timeout * 2, "connect", Self::connect_inner(ep, password, counters)) => r,
            _ = cancel.cancelled() => Err(Error::Cancelled),
        }
    }

    async fn connect_inner(ep: &Endpoint, password: &str, counters: Arc<Counters>) -> Result<Self> {
        let tcp = TcpStream::connect((ep.host.as_str(), ep.port))
            .await
            .map_err(|e| net(format!("connect {}:{}: {e}", ep.host, ep.port)))?;
        let _ = tcp.set_nodelay(true);
        let mut plain = Metered::new(tcp, counters);

        if ep.tls == TlsMode::Starttls {
            let greeting = read_plain_line(&mut plain).await?;
            let upper = greeting.to_ascii_uppercase();
            if upper.starts_with("* PREAUTH") {
                return Err(net(
                    "server sent PREAUTH before STARTTLS; refusing an unencrypted session",
                ));
            }
            if !upper.starts_with("* OK") {
                return Err(net(format!(
                    "unexpected IMAP greeting: {}",
                    clip(&greeting, MAX_ERROR_TEXT)
                )));
            }
            plain.write_all(b"S0 STARTTLS\r\n").await.map_err(net)?;
            plain.flush().await.map_err(net)?;
            loop {
                let line = read_plain_line(&mut plain).await?;
                let upper = line.to_ascii_uppercase();
                if upper.starts_with("S0 OK") {
                    break;
                }
                if upper.starts_with("S0 ") {
                    return Err(Error::Unsupported(format!(
                        "server refused STARTTLS: {}",
                        clip(&line, MAX_ERROR_TEXT)
                    )));
                }
                if upper.starts_with("* BYE") {
                    return Err(net(format!(
                        "server closed the connection: {}",
                        clip(&line, MAX_ERROR_TEXT)
                    )));
                }
                // Other untagged lines (e.g. CAPABILITY) are ignored before TLS.
            }
        }

        let server_name = ServerName::try_from(ep.host.clone())
            .map_err(|e| Error::Invalid(format!("invalid IMAP host `{}`: {e}", ep.host)))?;
        let tls = TlsConnector::from(ep.tls_config.clone())
            .connect(server_name, plain)
            .await
            .map_err(|e| net(format!("TLS handshake with {}: {e}", ep.host)))?;

        let limits = Arc::new(Limits::new(ep.max_literal, META_BUDGET));
        let mut client = async_imap::Client::new(Guarded::new(tls, limits.clone()));
        if ep.tls == TlsMode::Implicit {
            let greeting = client
                .read_response()
                .await
                .map_err(|e| read_err(&limits, e))?
                .ok_or_else(|| net("connection closed before the IMAP greeting"))?;
            match greeting.parsed() {
                Response::Data {
                    status: Status::Ok, ..
                } => {}
                Response::Data {
                    status: Status::PreAuth,
                    ..
                } => {
                    return Err(Error::Unsupported(
                        "pre-authenticated IMAP sessions are not supported".into(),
                    ));
                }
                other => {
                    return Err(net(format!(
                        "unexpected IMAP greeting: {}",
                        clip(&format!("{other:?}"), MAX_ERROR_TEXT)
                    )));
                }
            }
        }

        let session = match client.login(&ep.username, password).await {
            Ok(s) => s,
            Err((async_imap::error::Error::No(m), _))
            | Err((async_imap::error::Error::Bad(m), _)) => {
                return Err(Error::Unauthorized(format!(
                    "IMAP login rejected: {}",
                    clip(&m, MAX_ERROR_TEXT)
                )));
            }
            Err((async_imap::error::Error::Validate(_), _)) => {
                return Err(Error::Invalid(
                    "IMAP username or password contains a line break".into(),
                ));
            }
            Err((e, _)) => return Err(imap_err(&limits, e)),
        };

        let mut imap = Imap {
            session,
            limits,
            timeout: ep.io_timeout,
            budget: META_BUDGET,
            caps: HashSet::new(),
            condstore: false,
            qresync: false,
        };
        imap.refresh_capabilities().await?;
        imap.condstore = imap.has("CONDSTORE") || imap.has("QRESYNC");
        if imap.has("QRESYNC") && imap.has("ENABLE") {
            let mut enabled = false;
            let done = imap
                .exec("ENABLE QRESYNC", |r| {
                    if let Response::Capabilities(c) = r {
                        enabled |= c
                            .iter()
                            .any(|c| cap_name(c).eq_ignore_ascii_case("QRESYNC"));
                    }
                    Ok(())
                })
                .await?;
            imap.qresync = done == Done::Ok && enabled;
        }
        Ok(imap)
    }

    pub(crate) fn has(&self, cap: &str) -> bool {
        self.caps.contains(&cap.to_ascii_uppercase())
    }

    async fn refresh_capabilities(&mut self) -> Result<()> {
        let mut caps = HashSet::new();
        self.exec_ok("CAPABILITY", |r| {
            if let Response::Capabilities(c) = r {
                caps.extend(c.iter().map(|c| cap_name(c).to_ascii_uppercase()));
            }
            Ok(())
        })
        .await?;
        self.caps = caps;
        Ok(())
    }

    /// Run one tagged command, passing every untagged response to `on`.
    pub(crate) async fn exec<F>(&mut self, cmd: &str, mut on: F) -> Result<Done>
    where
        F: FnMut(&Response<'_>) -> Result<()>,
    {
        let timeout = self.timeout;
        let limits = self.limits.clone();
        limits.reset(self.budget);
        let id = with_timeout(timeout, "write", async {
            self.session
                .run_command(cmd)
                .await
                .map_err(|e| imap_err(&limits, e))
        })
        .await?;
        let mut count = 0usize;
        loop {
            let resp = with_timeout(timeout, "read", async {
                self.session
                    .read_response()
                    .await
                    .map_err(|e| read_err(&limits, e))
            })
            .await?
            .ok_or_else(|| net("IMAP server closed the connection"))?;
            match resp.parsed() {
                Response::Done {
                    tag,
                    status,
                    outcome,
                } if *tag == id => {
                    let info = clip(outcome.information.as_deref().unwrap_or(""), MAX_ERROR_TEXT);
                    return Ok(match status {
                        Status::Ok => Done::Ok,
                        Status::No => Done::No(info),
                        _ => Done::Bad(info),
                    });
                }
                Response::Done { .. } => {} // a stray tag: ignore
                Response::Data {
                    status: Status::Bye,
                    outcome,
                } => {
                    return Err(net(format!(
                        "IMAP server closed the session: {}",
                        clip(outcome.information.as_deref().unwrap_or(""), MAX_ERROR_TEXT)
                    )));
                }
                other => on(other)?,
            }
            count += 1;
            if count > MAX_RESPONSES_PER_COMMAND {
                return Err(net("too many IMAP responses for one command"));
            }
        }
    }

    /// Like [`exec`](Self::exec), but NO/BAD is an error.
    pub(crate) async fn exec_ok<F>(&mut self, cmd: &str, on: F) -> Result<()>
    where
        F: FnMut(&Response<'_>) -> Result<()>,
    {
        match self.exec(cmd, on).await? {
            Done::Ok => Ok(()),
            Done::No(m) | Done::Bad(m) => {
                let verb = cmd.split(' ').take(2).collect::<Vec<_>>().join(" ");
                Err(net(format!("IMAP {verb} failed: {m}")))
            }
        }
    }

    pub(crate) async fn list_all(&mut self) -> Result<Vec<ListedMailbox>> {
        let mut out = Vec::new();
        self.exec_ok("LIST \"\" \"*\"", |r| {
            if let Response::MailboxData(MailboxDatum::List(l)) = r {
                let mut noselect = false;
                let mut special = None;
                let mut virtual_view = false;
                for a in &l.name_attributes {
                    match a {
                        NameAttribute::NoSelect => noselect = true,
                        NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\NonExistent") => {
                            noselect = true
                        }
                        NameAttribute::Sent => special = Some(Special::Sent),
                        NameAttribute::Trash => special = Some(Special::Trash),
                        NameAttribute::Junk => special = Some(Special::Junk),
                        NameAttribute::All => {
                            special = Some(Special::All);
                            virtual_view = true;
                        }
                        NameAttribute::Drafts => special = Some(Special::Drafts),
                        NameAttribute::Flagged => {
                            special = special.or(Some(Special::Other));
                            virtual_view = true;
                        }
                        NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\Important") => {
                            special = special.or(Some(Special::Other));
                            virtual_view = true;
                        }
                        NameAttribute::Archive => special = special.or(Some(Special::Other)),
                        _ => {}
                    }
                }
                out.push(ListedMailbox {
                    name: l.name.to_string(),
                    delimiter: l.delimiter.as_ref().map(|d| d.to_string()),
                    noselect,
                    special,
                    virtual_view,
                });
            }
            Ok(())
        })
        .await?;
        Ok(out)
    }

    /// EXAMINE (read-only: the server never changes flags for us). `None` when the server says
    /// NO (missing or inaccessible mailbox).
    pub(crate) async fn examine(&mut self, mailbox: &str) -> Result<Option<Selected>> {
        let mut sel = Selected::default();
        let cmd = if self.condstore {
            format!("EXAMINE {} (CONDSTORE)", quote(mailbox)?)
        } else {
            format!("EXAMINE {}", quote(mailbox)?)
        };
        let done = self
            .exec(&cmd, |r| {
                match r {
                    Response::Data {
                        status: Status::Ok,
                        outcome,
                    } => match &outcome.code {
                        Some(ResponseCode::UidValidity(v)) => sel.uidvalidity = Some(*v),
                        Some(ResponseCode::UidNext(v)) => sel.uidnext = Some(*v),
                        Some(ResponseCode::HighestModSeq(v)) => sel.highest_modseq = Some(*v),
                        _ => {}
                    },
                    Response::MailboxData(MailboxDatum::Exists(n)) => sel.exists = Some(*n),
                    _ => {}
                }
                Ok(())
            })
            .await?;
        match done {
            Done::Ok => Ok(Some(sel)),
            Done::No(m) => {
                tracing::warn!(mailbox, "IMAP EXAMINE refused: {m}");
                Ok(None)
            }
            Done::Bad(m) => Err(net(format!("IMAP EXAMINE failed: {m}"))),
        }
    }

    /// SELECT (read-write, for [`uid_move`](Self::uid_move)). `None` when the server says NO.
    pub(crate) async fn select(&mut self, mailbox: &str) -> Result<Option<Selected>> {
        let mut sel = Selected::default();
        let done = self
            .exec(&format!("SELECT {}", quote(mailbox)?), |r| {
                match r {
                    Response::Data {
                        status: Status::Ok,
                        outcome,
                    } => {
                        if let Some(ResponseCode::UidValidity(v)) = &outcome.code {
                            sel.uidvalidity = Some(*v);
                        }
                    }
                    Response::MailboxData(MailboxDatum::Exists(n)) => sel.exists = Some(*n),
                    _ => {}
                }
                Ok(())
            })
            .await?;
        match done {
            Done::Ok => Ok(Some(sel)),
            Done::No(m) => {
                tracing::warn!(mailbox, "IMAP SELECT refused: {m}");
                Ok(None)
            }
            Done::Bad(m) => Err(net(format!("IMAP SELECT failed: {m}"))),
        }
    }

    /// `UID MOVE <set> <mailbox>` (RFC 6851) in the selected mailbox.
    pub(crate) async fn uid_move(&mut self, set: &str, dest: &str) -> Result<()> {
        self.exec_ok(&format!("UID MOVE {set} {}", quote(dest)?), |_| Ok(()))
            .await
    }

    /// Where archived mail goes: the `\Archive` special-use mailbox, else `\All` (e.g. Gmail's
    /// "All Mail", where moving out of the inbox only drops the Inbox label), else a mailbox
    /// named `Archive`.
    pub(crate) async fn archive_mailbox(&mut self) -> Result<Option<String>> {
        let (mut archive, mut all, mut named) = (None, None, None);
        self.exec_ok("LIST \"\" \"*\"", |r| {
            if let Response::MailboxData(MailboxDatum::List(l)) = r {
                let noselect = l.name_attributes.iter().any(|a| {
                    matches!(a, NameAttribute::NoSelect)
                        || matches!(a, NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\NonExistent"))
                });
                if noselect {
                    return Ok(());
                }
                let name = l.name.to_string();
                for a in &l.name_attributes {
                    match a {
                        NameAttribute::Archive if archive.is_none() => archive = Some(name.clone()),
                        NameAttribute::All if all.is_none() => all = Some(name.clone()),
                        _ => {}
                    }
                }
                if named.is_none() && name.eq_ignore_ascii_case("Archive") {
                    named = Some(name);
                }
            }
            Ok(())
        })
        .await?;
        Ok(archive.or(all).or(named))
    }

    pub(crate) async fn uid_search(&mut self, criteria: &str) -> Result<Vec<u32>> {
        let mut uids = Vec::new();
        self.exec_ok(&format!("UID SEARCH {criteria}"), |r| {
            if let Response::MailboxData(MailboxDatum::Search(s)) = r {
                uids.extend_from_slice(s);
            }
            Ok(())
        })
        .await?;
        uids.sort_unstable();
        uids.dedup();
        Ok(uids)
    }

    /// Flags, size, date (and MODSEQ under CONDSTORE) for a UID set.
    pub(crate) async fn fetch_meta(&mut self, set: &str) -> Result<Vec<MessageMeta>> {
        let items = if self.condstore {
            "(UID FLAGS RFC822.SIZE INTERNALDATE MODSEQ)"
        } else {
            "(UID FLAGS RFC822.SIZE INTERNALDATE)"
        };
        let mut out = Vec::new();
        self.exec_ok(&format!("UID FETCH {set} {items}"), |r| {
            if let Response::Fetch(_, attrs) = r
                && let Some(m) = meta_from(attrs)
            {
                out.push(m);
            }
            Ok(())
        })
        .await?;
        Ok(out)
    }

    /// Flags (and MODSEQ under CONDSTORE) only.
    pub(crate) async fn fetch_flags(&mut self, set: &str) -> Result<Vec<MessageMeta>> {
        let items = if self.condstore {
            "(UID FLAGS MODSEQ)"
        } else {
            "(UID FLAGS)"
        };
        let mut out = Vec::new();
        self.exec_ok(&format!("UID FETCH {set} {items}"), |r| {
            if let Response::Fetch(_, attrs) = r
                && let Some(m) = meta_from(attrs)
            {
                out.push(m);
            }
            Ok(())
        })
        .await?;
        Ok(out)
    }

    /// Messages whose mod-sequence moved past `since` among UIDs `1:last_uid`, plus (under
    /// QRESYNC) the UIDs expunged since then.
    pub(crate) async fn changed_since(&mut self, last_uid: u32, since: u64) -> Result<Changes> {
        let vanished = if self.qresync { " VANISHED" } else { "" };
        let cmd =
            format!("UID FETCH 1:{last_uid} (UID FLAGS MODSEQ) (CHANGEDSINCE {since}{vanished})");
        let mut out = Changes::default();
        self.exec_ok(&cmd, |r| {
            match r {
                Response::Fetch(_, attrs) => {
                    if let Some(m) = meta_from(attrs) {
                        out.changed.push(m);
                    }
                }
                Response::Vanished { uids, .. } => out.vanished.extend(uids.iter().cloned()),
                _ => {}
            }
            Ok(())
        })
        .await?;
        Ok(out)
    }

    /// `BODY.PEEK[]` (or `BODY.PEEK[HEADER]` when `header_only`) for a UID set. PEEK never sets
    /// `\Seen`. Stops with an error once more than `max_total` bytes arrived.
    pub(crate) async fn fetch_bodies(
        &mut self,
        set: &str,
        header_only: bool,
        max_total: usize,
    ) -> Result<Vec<(u32, Vec<u8>)>> {
        let section = if header_only {
            "BODY.PEEK[HEADER]"
        } else {
            "BODY.PEEK[]"
        };
        let mut out = Vec::new();
        let mut total = 0usize;
        // A single literal can be tens of MB: allow far longer than the per-response default.
        let normal = self.timeout;
        self.timeout = normal.saturating_mul(BODY_TIMEOUT_FACTOR);
        self.budget = (max_total as u64).saturating_add(BODY_BUDGET_SLACK);
        let result = self
            .exec(&format!("UID FETCH {set} (UID {section})"), |r| {
                if let Response::Fetch(_, attrs) = r {
                    let mut uid = None;
                    let mut data: Option<&[u8]> = None;
                    for a in attrs {
                        match a {
                            AttributeValue::Uid(u) => uid = Some(*u),
                            AttributeValue::BodySection {
                                section: None,
                                data: Some(d),
                                ..
                            } if !header_only => data = Some(d),
                            AttributeValue::BodySection {
                                section: Some(SectionPath::Full(MessageSection::Header)),
                                data: Some(d),
                                ..
                            } if header_only => data = Some(d),
                            AttributeValue::Rfc822(Some(d)) if !header_only => data = Some(d),
                            AttributeValue::Rfc822Header(Some(d)) if header_only => data = Some(d),
                            _ => {}
                        }
                    }
                    if let (Some(uid), Some(d)) = (uid, data) {
                        total += d.len();
                        if total > max_total {
                            return Err(Error::Invalid(format!(
                                "IMAP FETCH returned more than {max_total} bytes"
                            )));
                        }
                        out.push((uid, d.to_vec()));
                    }
                }
                Ok(())
            })
            .await;
        self.timeout = normal;
        self.budget = META_BUDGET;
        match result? {
            Done::Ok => Ok(out),
            // The server could not deliver (some of) these messages, e.g. Dovecot's
            // `NO [SERVERBUG]` for one corrupt message: the session is still usable.
            Done::No(m) => Err(Error::Provider(format!(
                "IMAP server refused to fetch messages {}: {m}",
                clip(set, 64)
            ))),
            // BAD is a protocol error (our command or the session), not a broken message.
            Done::Bad(m) => Err(net(format!("IMAP UID FETCH failed: {m}"))),
        }
    }

    /// Start IDLE; returns once the server sent its continuation.
    pub(crate) async fn idle_start(&mut self) -> Result<async_imap::imap_proto::RequestId> {
        let timeout = self.timeout;
        let limits = self.limits.clone();
        limits.reset(META_BUDGET);
        let id = with_timeout(timeout, "write", async {
            self.session
                .run_command("IDLE")
                .await
                .map_err(|e| imap_err(&limits, e))
        })
        .await?;
        loop {
            let resp = with_timeout(timeout, "read", async {
                self.session
                    .read_response()
                    .await
                    .map_err(|e| read_err(&limits, e))
            })
            .await?
            .ok_or_else(|| net("IMAP server closed the connection"))?;
            match resp.parsed() {
                Response::Continue(_) => return Ok(id),
                Response::Done { tag, outcome, .. } if *tag == id => {
                    return Err(net(format!(
                        "IMAP IDLE refused: {}",
                        clip(outcome.information.as_deref().unwrap_or(""), MAX_ERROR_TEXT)
                    )));
                }
                _ => {}
            }
        }
    }

    /// Wait (inside IDLE) for the next untagged response, up to `dur`. `Ok(None)` on timeout.
    /// Cancel-safe: the parser keeps partial data buffered.
    pub(crate) async fn idle_next(&mut self, dur: Duration) -> Result<Option<IdleEvent>> {
        let resp = match tokio::time::timeout(dur, self.session.read_response()).await {
            Err(_) => return Ok(None),
            Ok(r) => r
                .map_err(|e| read_err(&self.limits, e))?
                .ok_or_else(|| net("IMAP server closed the connection"))?,
        };
        Ok(Some(match resp.parsed() {
            Response::MailboxData(MailboxDatum::Exists(n)) => IdleEvent::Exists(*n),
            Response::Expunge(_) => IdleEvent::Expunge,
            Response::Vanished {
                earlier: false,
                uids,
            } => IdleEvent::Vanished(uids.clone()),
            Response::Fetch(..) => IdleEvent::FlagsChanged,
            Response::Data {
                status: Status::Bye,
                ..
            } => return Err(net("IMAP server closed the session")),
            _ => IdleEvent::Other,
        }))
    }

    /// End IDLE and wait for its tagged completion.
    pub(crate) async fn idle_done(&mut self, id: &async_imap::imap_proto::RequestId) -> Result<()> {
        let timeout = self.timeout;
        let limits = self.limits.clone();
        with_timeout(timeout, "write", async {
            self.session
                .run_command_untagged("DONE")
                .await
                .map_err(|e| imap_err(&limits, e))
        })
        .await?;
        loop {
            let resp = with_timeout(timeout, "read", async {
                self.session
                    .read_response()
                    .await
                    .map_err(|e| read_err(&limits, e))
            })
            .await?
            .ok_or_else(|| net("IMAP server closed the connection"))?;
            if let Response::Done { tag, .. } = resp.parsed()
                && tag == id
            {
                return Ok(());
            }
        }
    }

    /// Best-effort LOGOUT.
    pub(crate) async fn logout(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), self.session.logout()).await;
    }
}

/// Events seen while idling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IdleEvent {
    Exists(u32),
    Expunge,
    Vanished(Vec<std::ops::RangeInclusive<u32>>),
    FlagsChanged,
    Other,
}

fn cap_name<'a>(c: &'a imap_proto::Capability<'_>) -> std::borrow::Cow<'a, str> {
    match c {
        imap_proto::Capability::Imap4rev1 => "IMAP4REV1".into(),
        imap_proto::Capability::Auth(a) => format!("AUTH={a}").into(),
        imap_proto::Capability::Atom(a) => std::borrow::Cow::Borrowed(a.as_ref()),
    }
}

fn meta_from(attrs: &[AttributeValue<'_>]) -> Option<MessageMeta> {
    let mut m = MessageMeta::default();
    let mut uid = None;
    for a in attrs {
        match a {
            AttributeValue::Uid(u) => uid = Some(*u),
            AttributeValue::Flags(f) => m.flags = f.iter().map(|s| s.to_string()).collect(),
            AttributeValue::ModSeq(s) => m.modseq = Some(*s),
            AttributeValue::Rfc822Size(s) => m.size = Some(*s),
            AttributeValue::InternalDate(d) => m.internal_date = parse_internal_date(d),
            _ => {}
        }
    }
    m.uid = uid?;
    Some(m)
}

/// `date-time` from RFC 3501: `"17-Jul-1996 02:44:25 -0700"` (day may be space-padded).
pub(crate) fn parse_internal_date(s: &str) -> Option<OffsetDateTime> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let mut parts = s.split_whitespace();
    let (date, clock, zone) = (parts.next()?, parts.next()?, parts.next()?);
    let mut d = date.split('-');
    let day: u8 = d.next()?.parse().ok()?;
    let mon = d.next()?.to_ascii_lowercase();
    let month_idx = MONTHS.iter().position(|m| *m == mon)?;
    let year: i32 = d.next()?.parse().ok()?;
    let month = time::Month::try_from(u8::try_from(month_idx + 1).ok()?).ok()?;
    let mut c = clock.split(':');
    let (h, mi, se): (u8, u8, u8) = (
        c.next()?.parse().ok()?,
        c.next()?.parse().ok()?,
        c.next()?.parse().ok()?,
    );
    if zone.len() != 5 || !zone.is_ascii() {
        return None;
    }
    let sign: i8 = match &zone[..1] {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let zh: i8 = zone[1..3].parse().ok()?;
    let zm: i8 = zone[3..5].parse().ok()?;
    let offset = time::UtcOffset::from_hms(sign * zh, sign * zm, 0).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;
    let clock = time::Time::from_hms(h, mi, se).ok()?;
    Some(time::PrimitiveDateTime::new(date, clock).assume_offset(offset))
}

/// Quote a mailbox name as an IMAP quoted string. Names must be 7-bit (modified UTF-7 for
/// non-ASCII) and must not contain CR, LF or NUL.
pub(crate) fn quote(s: &str) -> Result<String> {
    if s.bytes()
        .any(|b| b == b'\r' || b == b'\n' || b == 0 || b >= 0x80)
    {
        return Err(Error::Invalid(format!(
            "mailbox name {s:?} contains characters IMAP cannot quote"
        )));
    }
    Ok(format!(
        "\"{}\"",
        s.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// Encode a mailbox name with IMAP's modified UTF-7 (RFC 3501 §5.1.3).
pub(crate) fn encode_mutf7(s: &str) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";
    let mut out = String::with_capacity(s.len());
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        out.push('&');
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            let chars = chunk.len() + 1;
            for i in 0..chars {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out.push('-');
        pending.clear();
    };
    for c in s.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Compress sorted UIDs into IMAP sequence sets (`1:5,7,9:12`), each at most `max_len` bytes.
pub(crate) fn uid_sets(uids: &[u32], max_len: usize) -> Vec<String> {
    let mut sets = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < uids.len() {
        let start = uids[i];
        let mut end = start;
        while i + 1 < uids.len() && uids[i + 1] == end.wrapping_add(1) {
            i += 1;
            end = uids[i];
        }
        i += 1;
        let part = if start == end {
            start.to_string()
        } else {
            format!("{start}:{end}")
        };
        if !cur.is_empty() && cur.len() + 1 + part.len() > max_len {
            sets.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(',');
        }
        cur.push_str(&part);
    }
    if !cur.is_empty() {
        sets.push(cur);
    }
    sets
}
