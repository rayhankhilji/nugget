//! A scripted IMAP server for tests: TLS (rcgen certificate) or STARTTLS, LOGIN, CAPABILITY,
//! ENABLE, LIST, SELECT/EXAMINE (CONDSTORE), UID SEARCH, UID FETCH (CHANGEDSINCE, VANISHED),
//! IDLE, LOGOUT. It keeps a command log and simulates `\Seen` being set by non-PEEK fetches.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::{ClientConfig, RootCertStore, ServerConfig};

use crate::conn::parse_internal_date;

#[derive(Debug, Clone)]
pub struct Msg {
    pub flags: BTreeSet<String>,
    pub modseq: u64,
    pub body: Vec<u8>,
    pub date: String,
}

#[derive(Debug, Clone)]
pub struct Mailbox {
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub modseq: u64,
    pub msgs: BTreeMap<u32, Msg>,
    pub special: Option<&'static str>,
    /// (uid, modseq at expunge) for QRESYNC VANISHED (EARLIER).
    pub expunged: Vec<(u32, u64)>,
}

impl Mailbox {
    pub fn new(uidvalidity: u32) -> Self {
        Self {
            uidvalidity,
            uidnext: 1,
            modseq: 1,
            msgs: BTreeMap::new(),
            special: None,
            expunged: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Implicit,
    Starttls,
    /// STARTTLS answered with NO.
    StarttlsRefused,
    /// No TLS at all (a plaintext server on the "TLS" port).
    Plaintext,
    /// Completes the TLS handshake, then never says anything.
    Silent,
}

#[derive(Debug)]
pub struct State {
    pub user: String,
    pub pass: String,
    pub condstore: bool,
    pub qresync: bool,
    pub idle: bool,
    pub mailboxes: BTreeMap<String, Mailbox>,
    pub log: Vec<String>,
    /// Untagged lines to push to a client in IDLE.
    pub pending: Vec<String>,
    /// Answer EXAMINE with malformed data.
    pub garbage: bool,
    /// Answer EXAMINE with these raw bytes, then go silent (connection kept open).
    pub raw_examine: Option<Vec<u8>>,
    /// Announce this literal length for message bodies, send a few bytes, then go silent.
    pub lie_literal: Option<u64>,
    pub logins: usize,
    /// Advertise and serve `UID MOVE` (RFC 6851).
    pub move_cap: bool,
    /// Body FETCHes to answer before sending `* BYE` and dropping the connection (once).
    pub bye_after_body_fetches: Option<usize>,
    /// Answer every body FETCH with `* BYE` and a closed connection.
    pub bye_every_body_fetch: bool,
    /// Messages a body FETCH cannot deliver: left out, and the command ends in
    /// `NO [SERVERBUG]` (as Dovecot does for a corrupt message).
    pub broken: BTreeSet<u32>,
    /// Body FETCH commands answered.
    pub body_fetches: usize,
}

impl State {
    pub fn new() -> Self {
        Self {
            user: "me@example.com".into(),
            pass: "app-pass".into(),
            condstore: true,
            qresync: true,
            idle: true,
            mailboxes: BTreeMap::new(),
            log: Vec::new(),
            pending: Vec::new(),
            garbage: false,
            raw_examine: None,
            lie_literal: None,
            logins: 0,
            move_cap: false,
            bye_after_body_fetches: None,
            bye_every_body_fetch: false,
            broken: BTreeSet::new(),
            body_fetches: 0,
        }
    }

    pub fn mailbox(&mut self, name: &str, uidvalidity: u32) -> &mut Mailbox {
        self.mailboxes
            .entry(name.to_string())
            .or_insert_with(|| Mailbox::new(uidvalidity))
    }

    pub fn add(&mut self, mbox: &str, body: &[u8]) -> u32 {
        let date = now_internal_date();
        self.add_dated(mbox, body, &date)
    }

    pub fn add_dated(&mut self, mbox: &str, body: &[u8], date: &str) -> u32 {
        let mb = self.mailboxes.get_mut(mbox).expect("mailbox");
        let uid = mb.uidnext;
        mb.uidnext += 1;
        mb.modseq += 1;
        mb.msgs.insert(
            uid,
            Msg {
                flags: BTreeSet::new(),
                modseq: mb.modseq,
                body: body.to_vec(),
                date: date.to_string(),
            },
        );
        uid
    }

    pub fn expunge(&mut self, mbox: &str, uid: u32) {
        let mb = self.mailboxes.get_mut(mbox).expect("mailbox");
        mb.msgs.remove(&uid);
        mb.modseq += 1;
        mb.expunged.push((uid, mb.modseq));
    }

    pub fn set_flag(&mut self, mbox: &str, uid: u32, flag: &str) {
        let mb = self.mailboxes.get_mut(mbox).expect("mailbox");
        mb.modseq += 1;
        let m = mb.msgs.get_mut(&uid).expect("message");
        m.flags.insert(flag.to_string());
        m.modseq = mb.modseq;
    }

    /// New UIDVALIDITY; messages renumbered from 1.
    pub fn reset_uidvalidity(&mut self, mbox: &str, uidvalidity: u32) {
        let mb = self.mailboxes.get_mut(mbox).expect("mailbox");
        let msgs: Vec<Msg> = std::mem::take(&mut mb.msgs).into_values().collect();
        mb.uidvalidity = uidvalidity;
        mb.uidnext = 1;
        mb.expunged.clear();
        for m in msgs {
            mb.msgs.insert(mb.uidnext, m);
            mb.uidnext += 1;
        }
        mb.modseq += 1;
    }

    pub fn flags(&self, mbox: &str, uid: u32) -> BTreeSet<String> {
        self.mailboxes[mbox].msgs[&uid].flags.clone()
    }
}

pub fn now_internal_date() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!(
        "{}-{}-{} {:02}:{:02}:{:02} +0000",
        t.day(),
        month_name(t.month()),
        t.year(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

fn month_name(m: time::Month) -> &'static str {
    const M: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    M[usize::from(u8::from(m)) - 1]
}

pub struct TestServer {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<State>>,
    pub notify: Arc<Notify>,
    pub client_tls: Arc<ClientConfig>,
}

impl TestServer {
    pub async fn start(state: State, mode: Mode) -> Self {
        let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
        let ck = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
        let cert_der: CertificateDer<'static> = ck.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
        let server_cfg = ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key)
            .unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(cert_der).unwrap();
        // Test-only trust: a normal webpki verifier rooted at the test certificate.
        let client_tls = Arc::new(
            ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(state));
        let notify = Arc::new(Notify::new());
        let acceptor = TlsAcceptor::from(Arc::new(server_cfg));
        let (st, nt) = (state.clone(), notify.clone());
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let (st, nt, acc) = (st.clone(), nt.clone(), acceptor.clone());
                tokio::spawn(async move {
                    let _ = serve(tcp, acc, st, nt, mode).await;
                });
            }
        });
        Self {
            addr,
            state,
            notify,
            client_tls,
        }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    /// Queue an untagged line for a client in IDLE.
    pub fn push(&self, line: &str) {
        self.with(|s| s.pending.push(line.to_string()));
        self.notify.notify_one();
    }

    pub fn log(&self) -> Vec<String> {
        self.with(|s| s.log.clone())
    }
}

async fn serve(
    mut tcp: TcpStream,
    acceptor: TlsAcceptor,
    state: Arc<Mutex<State>>,
    notify: Arc<Notify>,
    mode: Mode,
) -> std::io::Result<()> {
    match mode {
        Mode::Implicit => {
            let tls = acceptor.accept(tcp).await?;
            session(tls, state, notify, true).await
        }
        Mode::Plaintext => session(tcp, state, notify, true).await,
        Mode::Silent => {
            let _tls = acceptor.accept(tcp).await?;
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            Ok(())
        }
        Mode::Starttls | Mode::StarttlsRefused => {
            tcp.write_all(b"* OK [CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED] hi\r\n")
                .await?;
            let mut line = Vec::new();
            loop {
                let mut b = [0u8; 1];
                tokio::io::AsyncReadExt::read_exact(&mut tcp, &mut b).await?;
                line.push(b[0]);
                if line.ends_with(b"\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&line).trim().to_string();
            state.lock().unwrap().log.push(text.clone());
            let tag = text.split(' ').next().unwrap_or("*").to_string();
            if mode == Mode::StarttlsRefused {
                tcp.write_all(format!("{tag} NO STARTTLS unavailable\r\n").as_bytes())
                    .await?;
                return Ok(());
            }
            tcp.write_all(format!("{tag} OK Begin TLS negotiation now\r\n").as_bytes())
                .await?;
            let tls = acceptor.accept(tcp).await?;
            session(tls, state, notify, false).await
        }
    }
}

/// Split a command line into tokens: atoms, quoted strings (unquoted), parenthesised groups
/// (kept verbatim).
fn tokens(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b' ' => i += 1,
            b'"' => {
                let mut t = String::new();
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                    }
                    t.push(b[i] as char);
                    i += 1;
                }
                i += 1;
                out.push(t);
            }
            b'(' => {
                let start = i;
                let mut depth = 0;
                while i < b.len() {
                    if b[i] == b'(' {
                        depth += 1;
                    } else if b[i] == b')' {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    i += 1;
                }
                out.push(s[start..i].to_string());
            }
            _ => {
                let start = i;
                while i < b.len() && b[i] != b' ' {
                    i += 1;
                }
                out.push(s[start..i].to_string());
            }
        }
    }
    out
}

/// Resolve a UID set (`1:5,7,9:*`) against the UIDs present.
fn uid_set(set: &str, present: &BTreeMap<u32, Msg>) -> BTreeSet<u32> {
    let max = present.keys().last().copied().unwrap_or(0);
    let val = |x: &str| {
        if x == "*" {
            max
        } else {
            x.parse().unwrap_or(0)
        }
    };
    let mut out = BTreeSet::new();
    for part in set.split(',') {
        let (a, b) = match part.split_once(':') {
            Some((a, b)) => (val(a), val(b)),
            None => (val(part), val(part)),
        };
        let (lo, hi) = (a.min(b), a.max(b));
        out.extend(present.range(lo..=hi).map(|(u, _)| *u));
    }
    out
}

fn in_set(set: &str, uid: u32) -> bool {
    set.split(',').any(|part| {
        let parse = |x: &str| {
            if x == "*" {
                u32::MAX
            } else {
                x.parse().unwrap_or(0)
            }
        };
        let (a, b) = match part.split_once(':') {
            Some((a, b)) => (parse(a), parse(b)),
            None => (parse(part), parse(part)),
        };
        (a.min(b)..=a.max(b)).contains(&uid)
    })
}

async fn session<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    state: Arc<Mutex<State>>,
    notify: Arc<Notify>,
    greet: bool,
) -> std::io::Result<()> {
    let (r, mut w) = tokio::io::split(stream);
    let mut r = BufReader::new(r);
    if greet {
        w.write_all(b"* OK [CAPABILITY IMAP4rev1] test server ready\r\n")
            .await?;
    }
    let mut authed = false;
    let mut selected: Option<String> = None;
    let mut qresync_on = false;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let text = line.trim_end().to_string();
        state.lock().unwrap().log.push(text.clone());
        let toks = tokens(&text);
        if toks.len() < 2 {
            w.write_all(b"* BAD empty\r\n").await?;
            continue;
        }
        let tag = toks[0].clone();
        let cmd = toks[1].to_ascii_uppercase();
        let mut out: Vec<u8> = Vec::new();
        // Raw bytes to send instead of `out`, after which the server goes silent.
        let mut silent_after: Option<Vec<u8>> = None;
        // Send `* BYE` instead of the answer and drop the connection.
        let mut bye_close = false;
        let put = |out: &mut Vec<u8>, s: &str| {
            out.extend_from_slice(s.as_bytes());
            out.extend_from_slice(b"\r\n");
        };
        match cmd.as_str() {
            "CAPABILITY" => {
                let s = state.lock().unwrap();
                let mut caps = String::from("* CAPABILITY IMAP4rev1 ENABLE SPECIAL-USE");
                if s.condstore {
                    caps.push_str(" CONDSTORE");
                }
                if s.qresync {
                    caps.push_str(" QRESYNC");
                }
                if s.idle {
                    caps.push_str(" IDLE");
                }
                if s.move_cap {
                    caps.push_str(" MOVE");
                }
                put(&mut out, &caps);
                put(&mut out, &format!("{tag} OK done"));
            }
            "LOGIN" => {
                let mut s = state.lock().unwrap();
                if toks.len() == 4 && toks[2] == s.user && toks[3] == s.pass {
                    authed = true;
                    s.logins += 1;
                    put(&mut out, &format!("{tag} OK logged in"));
                } else {
                    put(
                        &mut out,
                        &format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials"),
                    );
                }
            }
            "LOGOUT" => {
                put(&mut out, "* BYE bye");
                put(&mut out, &format!("{tag} OK logged out"));
                w.write_all(&out).await?;
                return Ok(());
            }
            "NOOP" => put(&mut out, &format!("{tag} OK")),
            _ if !authed => put(&mut out, &format!("{tag} BAD log in first")),
            "ENABLE" => {
                let s = state.lock().unwrap();
                if s.qresync && toks.iter().any(|t| t.eq_ignore_ascii_case("QRESYNC")) {
                    qresync_on = true;
                    put(&mut out, "* ENABLED QRESYNC");
                } else {
                    put(&mut out, "* ENABLED");
                }
                put(&mut out, &format!("{tag} OK enabled"));
            }
            "LIST" => {
                let s = state.lock().unwrap();
                for (name, mb) in &s.mailboxes {
                    let attrs = match mb.special {
                        Some(sp) => format!("\\HasNoChildren {sp}"),
                        None => "\\HasNoChildren".to_string(),
                    };
                    put(&mut out, &format!("* LIST ({attrs}) \"/\" \"{name}\""));
                }
                put(&mut out, &format!("{tag} OK list"));
            }
            "SELECT" | "EXAMINE" => {
                let s = state.lock().unwrap();
                let name = toks.get(2).cloned().unwrap_or_default();
                if let Some(raw) = s.raw_examine.clone() {
                    silent_after = Some(raw);
                } else if s.garbage {
                    out.extend_from_slice(b"* 1 FETCH (UID \x00\xff garbage ((((\r\n");
                    out.extend_from_slice(b")))) nonsense {99999999999}\r\n");
                } else if let Some(mb) = s.mailboxes.get(&name) {
                    selected = Some(name.clone());
                    put(
                        &mut out,
                        "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)",
                    );
                    put(&mut out, &format!("* {} EXISTS", mb.msgs.len()));
                    put(&mut out, "* 0 RECENT");
                    put(
                        &mut out,
                        &format!("* OK [UIDVALIDITY {}] UIDs valid", mb.uidvalidity),
                    );
                    put(&mut out, &format!("* OK [UIDNEXT {}] next", mb.uidnext));
                    if s.condstore {
                        put(
                            &mut out,
                            &format!("* OK [HIGHESTMODSEQ {}] modseq", mb.modseq),
                        );
                    } else {
                        put(&mut out, "* OK [NOMODSEQ] no modseq");
                    }
                    let rw = if cmd == "EXAMINE" {
                        "READ-ONLY"
                    } else {
                        "READ-WRITE"
                    };
                    put(&mut out, &format!("{tag} OK [{rw}] {cmd} completed"));
                } else {
                    put(&mut out, &format!("{tag} NO no such mailbox"));
                }
            }
            "UID" if selected.is_some() => {
                let sub = toks
                    .get(2)
                    .map(|s| s.to_ascii_uppercase())
                    .unwrap_or_default();
                let mbname = selected.clone().unwrap_or_default();
                let mut s = state.lock().unwrap();
                match sub.as_str() {
                    "SEARCH" => {
                        let mb = &s.mailboxes[&mbname];
                        let mut uids: BTreeSet<u32> = mb.msgs.keys().copied().collect();
                        let mut i = 3;
                        while i < toks.len() {
                            match toks[i].to_ascii_uppercase().as_str() {
                                "ALL" => {}
                                "UID" => {
                                    let set = uid_set(&toks[i + 1], &mb.msgs);
                                    uids.retain(|u| set.contains(u));
                                    i += 1;
                                }
                                "SINCE" => {
                                    let since = parse_internal_date(&format!(
                                        "{} 00:00:00 +0000",
                                        toks[i + 1]
                                    ))
                                    .unwrap();
                                    // Unparseable dates match (as servers fall back to the
                                    // receive time).
                                    uids.retain(|u| {
                                        parse_internal_date(&mb.msgs[u].date)
                                            .is_none_or(|d| d.date() >= since.date())
                                    });
                                    i += 1;
                                }
                                other => panic!("unsupported search key {other}"),
                            }
                            i += 1;
                        }
                        let list: Vec<String> = uids.iter().map(u32::to_string).collect();
                        if list.is_empty() {
                            put(&mut out, "* SEARCH");
                        } else {
                            put(&mut out, &format!("* SEARCH {}", list.join(" ")));
                        }
                        put(&mut out, &format!("{tag} OK search done"));
                    }
                    "FETCH" => {
                        let set_str = toks[3].clone();
                        let items = toks[4].to_ascii_uppercase();
                        let modifiers = toks.get(5).map(|m| m.to_ascii_uppercase());
                        let mut changed_since = None;
                        let mut vanished = false;
                        if let Some(m) = &modifiers {
                            let mt = tokens(m.trim_matches(|c| c == '(' || c == ')'));
                            for (i, t) in mt.iter().enumerate() {
                                if t == "CHANGEDSINCE" {
                                    changed_since =
                                        mt.get(i + 1).and_then(|v| v.parse::<u64>().ok());
                                }
                                if t == "VANISHED" {
                                    vanished = true;
                                }
                            }
                        }
                        let wants_body = items.contains("BODY.PEEK[") || items.contains("BODY[");
                        if wants_body {
                            s.body_fetches += 1;
                            bye_close |= s.bye_every_body_fetch;
                            match s.bye_after_body_fetches {
                                Some(0) => {
                                    s.bye_after_body_fetches = None;
                                    bye_close = true;
                                }
                                Some(n) => s.bye_after_body_fetches = Some(n - 1),
                                None => {}
                            }
                        }
                        let broken = if wants_body {
                            s.broken.clone()
                        } else {
                            BTreeSet::new()
                        };
                        let mut refused = false;
                        let condstore = s.condstore;
                        let s_lie = s.lie_literal;
                        let mut lie_out: Option<Vec<u8>> = None;
                        let mb = s.mailboxes.get_mut(&mbname).unwrap();
                        if vanished && qresync_on {
                            let since = changed_since.unwrap_or(0);
                            let v: Vec<String> = mb
                                .expunged
                                .iter()
                                .filter(|(u, ms)| *ms > since && in_set(&set_str, *u))
                                .map(|(u, _)| u.to_string())
                                .collect();
                            if !v.is_empty() {
                                put(&mut out, &format!("* VANISHED (EARLIER) {}", v.join(",")));
                            }
                        }
                        let uids = uid_set(&set_str, &mb.msgs);
                        let seqs: BTreeMap<u32, usize> = mb
                            .msgs
                            .keys()
                            .enumerate()
                            .map(|(i, u)| (*u, i + 1))
                            .collect();
                        let sets_seen = items.contains("BODY[]") && !items.contains("BODY.PEEK[]")
                            || items.contains("RFC822 ")
                            || items.contains("RFC822)");
                        let mut bump = mb.modseq;
                        for uid in uids {
                            if broken.contains(&uid) {
                                refused = true;
                                continue;
                            }
                            let m = mb.msgs.get_mut(&uid).unwrap();
                            if changed_since.is_some_and(|c| m.modseq <= c) {
                                continue;
                            }
                            if sets_seen && m.flags.insert("\\Seen".into()) {
                                bump += 1;
                                m.modseq = bump;
                            }
                            let mut parts: Vec<Vec<u8>> = vec![format!("UID {uid}").into_bytes()];
                            if items.contains("FLAGS") {
                                let f: Vec<&str> = m.flags.iter().map(String::as_str).collect();
                                parts.push(format!("FLAGS ({})", f.join(" ")).into_bytes());
                            }
                            if (items.contains("MODSEQ") || changed_since.is_some()) && condstore {
                                parts.push(format!("MODSEQ ({})", m.modseq).into_bytes());
                            }
                            if items.contains("RFC822.SIZE") {
                                parts.push(format!("RFC822.SIZE {}", m.body.len()).into_bytes());
                            }
                            if items.contains("INTERNALDATE") {
                                parts.push(format!("INTERNALDATE \"{}\"", m.date).into_bytes());
                            }
                            if items.contains("BODY.PEEK[HEADER]") || items.contains("BODY[HEADER]")
                            {
                                let end = m
                                    .body
                                    .windows(4)
                                    .position(|w| w == b"\r\n\r\n")
                                    .map_or(m.body.len(), |p| p + 4);
                                let mut p = format!("BODY[HEADER] {{{end}}}\r\n").into_bytes();
                                p.extend_from_slice(&m.body[..end]);
                                parts.push(p);
                            } else if (items.contains("BODY.PEEK[]") || items.contains("BODY[]"))
                                && let Some(n) = s_lie
                            {
                                let mut raw =
                                    format!("* {} FETCH (UID {uid} BODY[] {{{n}}}\r\n", seqs[&uid])
                                        .into_bytes();
                                raw.extend_from_slice(&m.body);
                                lie_out = Some(raw);
                                break;
                            } else if items.contains("BODY.PEEK[]") || items.contains("BODY[]") {
                                let mut p = format!("BODY[] {{{}}}\r\n", m.body.len()).into_bytes();
                                p.extend_from_slice(&m.body);
                                parts.push(p);
                            }
                            out.extend_from_slice(format!("* {} FETCH (", seqs[&uid]).as_bytes());
                            out.extend_from_slice(&parts.join(&b" "[..]));
                            out.extend_from_slice(b")\r\n");
                        }
                        mb.modseq = bump;
                        if let Some(raw) = lie_out {
                            silent_after = Some(raw);
                        }
                        if refused {
                            put(
                                &mut out,
                                &format!("{tag} NO [SERVERBUG] Internal error occurred"),
                            );
                        } else {
                            put(&mut out, &format!("{tag} OK fetch done"));
                        }
                    }
                    "STORE" => {
                        put(&mut out, &format!("{tag} NO read-only test server"));
                    }
                    "MOVE" if s.move_cap => {
                        let set_str = toks[3].clone();
                        let dest = toks[4].clone();
                        if !s.mailboxes.contains_key(&dest) {
                            put(&mut out, &format!("{tag} NO [TRYCREATE] no such mailbox"));
                        } else {
                            let uids = uid_set(&set_str, &s.mailboxes[&mbname].msgs);
                            for uid in uids {
                                let m = s.mailboxes.get_mut(&mbname).unwrap().msgs[&uid].clone();
                                s.expunge(&mbname, uid);
                                let d = s.mailboxes.get_mut(&dest).unwrap();
                                let nu = d.uidnext;
                                d.uidnext += 1;
                                d.modseq += 1;
                                d.msgs.insert(nu, m);
                            }
                            put(&mut out, &format!("{tag} OK move done"));
                        }
                    }
                    _ => put(&mut out, &format!("{tag} BAD unknown UID command")),
                }
            }
            "IDLE" => {
                w.write_all(b"+ idling\r\n").await?;
                w.flush().await?;
                let mut done_line = String::new();
                loop {
                    let pending: Vec<String> = std::mem::take(&mut state.lock().unwrap().pending);
                    for p in pending {
                        w.write_all(format!("{p}\r\n").as_bytes()).await?;
                    }
                    w.flush().await?;
                    tokio::select! {
                        n = r.read_line(&mut done_line) => {
                            if n? == 0 {
                                return Ok(());
                            }
                            state.lock().unwrap().log.push(done_line.trim_end().to_string());
                            break;
                        }
                        _ = notify.notified() => {}
                    }
                }
                put(&mut out, &format!("{tag} OK IDLE terminated"));
            }
            _ => put(&mut out, &format!("{tag} BAD unknown command")),
        }
        if bye_close {
            w.write_all(b"* BYE server shutting down\r\n").await?;
            w.flush().await?;
            return Ok(());
        }
        if let Some(raw) = silent_after {
            w.write_all(&raw).await?;
            w.flush().await?;
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            return Ok(());
        }
        w.write_all(&out).await?;
        w.flush().await?;
    }
}
