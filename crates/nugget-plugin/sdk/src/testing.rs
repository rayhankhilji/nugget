//! An in-process fake host for testing plugin logic natively (any target but `wasm32`).
//!
//! On native targets every SDK host call ([`http`](crate::http), [`kv`](crate::kv),
//! [`secrets`](crate::secrets), [`log`](crate::log), [`now_ms`](crate::now_ms),
//! [`fs`](crate::fs), [`sink`](crate::sink)) goes to the [`TestHost`] active on the current
//! thread. The fake host enforces what the real one enforces, as far as it can without a wasm
//! runtime: declared hosts, secrets, key-value size and file roots from the manifest, read-only
//! tools, no network or key-value store in `validate`, forbidden request headers, document and
//! file size limits. Requests that match no canned response fail with [`Error::Network`]; nothing
//! ever leaves the process.
//!
//! ```
//! use nugget_plugin_sdk::testing::TestHost;
//! use nugget_plugin_sdk::{Connector, Document, Result, Value, http, json, sink};
//!
//! struct Hello;
//!
//! impl Connector for Hello {
//!     fn sync(settings: &Value, _cursor: Option<Value>) -> Result<Value> {
//!         let url = settings["url"].as_str().unwrap_or_default();
//!         let body = http::get(url)?.error_for_status()?.text()?;
//!         sink::upsert(Document::text("hello", "text/plain", body))?;
//!         Ok(json!({ "done": true }))
//!     }
//! }
//!
//! let host = TestHost::from_manifest(
//!     r#"
//!     id = "dev.example.hello"
//!     name = "Hello"
//!     version = "0.1.0"
//!     description = ""
//!     kind = ["connector"]
//!     [capabilities]
//!     hosts = ["api.example.com"]
//!     [connector]
//!     display_name = "Hello"
//!     "#,
//! );
//! host.respond("GET", "https://api.example.com/hello", http::Response::ok("hi"));
//!
//! let pass = host
//!     .sync::<Hello>(json!({ "url": "https://api.example.com/hello" }), None)
//!     .unwrap();
//! assert_eq!(pass.cursor, json!({ "done": true }));
//! assert_eq!(pass.upserts[0].text_content(), Some("hi"));
//!
//! // Hosts the manifest does not declare are denied, exactly like the real host does.
//! let err = host
//!     .sync::<Hello>(json!({ "url": "https://evil.example.net/" }), None)
//!     .unwrap_err();
//! assert!(matches!(err, nugget_plugin_sdk::Error::Denied(_)));
//! ```
//!
//! A `TestHost` belongs to the thread that created it (cargo runs tests on parallel threads, each
//! test gets its own host). Plugin code called directly (outside [`TestHost::sync`],
//! [`TestHost::validate`] and [`TestHost::call`]) uses the most recently created host on the
//! thread with no mode restrictions, which is handy for testing helper functions.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::log::Level;
use crate::{Connector, Content, Document, Error, Result, Tool, Tools, Value, fs, http};

/// The default fake time: 2023-11-14T22:13:20Z.
pub const DEFAULT_NOW_MS: i64 = 1_700_000_000_000;

/// Longest key-value key, in bytes (as the host's store).
const MAX_KV_KEY_BYTES: usize = 256;
/// Header names a plugin may not set (as the host).
const FORBIDDEN_HEADERS: &[&str] = &[
    "host",
    "connection",
    "content-length",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "keep-alive",
    "proxy-authorization",
    "proxy-authenticate",
    "proxy-connection",
];

/// Resource limits the fake host applies. The defaults are the real host's defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub max_http_response_bytes: usize,
    pub max_http_request_bytes: usize,
    pub max_http_requests_per_call: usize,
    pub max_http_requests_per_sync: usize,
    pub max_document_bytes: usize,
    pub max_tool_io_bytes: usize,
    pub max_fs_file_bytes: u64,
    pub max_fs_dir_entries: usize,
    /// File bytes one `validate` may read.
    pub max_fs_read_bytes_per_call: u64,
    pub max_fs_read_bytes_per_sync: u64,
    /// File operations (`read_dir`, `read_file`, `stat`) one `validate` may make.
    pub max_fs_ops_per_call: u32,
    pub max_fs_ops_per_sync: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_http_response_bytes: 16 * 1024 * 1024,
            max_http_request_bytes: 4 * 1024 * 1024,
            max_http_requests_per_call: 32,
            max_http_requests_per_sync: 10_000,
            max_document_bytes: 16 * 1024 * 1024,
            max_tool_io_bytes: 1024 * 1024,
            max_fs_file_bytes: 16 * 1024 * 1024,
            max_fs_dir_entries: 10_000,
            max_fs_read_bytes_per_call: 4 * 1024 * 1024,
            max_fs_read_bytes_per_sync: 4 * 1024 * 1024 * 1024,
            max_fs_ops_per_call: 1_000,
            max_fs_ops_per_sync: 1_000_000,
        }
    }
}

/// One thing a connector sent to the sink.
#[derive(Debug, Clone, PartialEq)]
pub enum SinkEvent {
    Upsert(Document),
    Delete(String),
    Checkpoint(Value),
}

/// What one [`TestHost::sync`] pass did.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncPass {
    /// The cursor the pass returned (after a JSON round trip, as the host stores it).
    pub cursor: Value,
    pub upserts: Vec<Document>,
    pub deletes: Vec<String>,
    pub checkpoints: Vec<Value>,
}

impl SyncPass {
    /// Keys of the upserted documents, in order.
    pub fn upserted_keys(&self) -> Vec<&str> {
        self.upserts.iter().map(|d| d.key.as_str()).collect()
    }

    /// The last upsert of `key` in this pass.
    pub fn upserted(&self, key: &str) -> Option<&Document> {
        self.upserts.iter().rev().find(|d| d.key == key)
    }
}

/// An in-memory folder tree (or single file) to mount as a file root.
#[derive(Debug, Clone)]
pub struct MemFs {
    /// Path (`""` = the root) to node.
    nodes: BTreeMap<String, MemNode>,
    /// Paths whose reads fail.
    failing: BTreeSet<String>,
}

#[derive(Debug, Clone)]
enum MemNode {
    Dir { mtime: Option<i64> },
    File { data: Vec<u8>, mtime: Option<i64> },
}

impl Default for MemFs {
    fn default() -> Self {
        Self::new()
    }
}

impl MemFs {
    /// An empty folder.
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(String::new(), MemNode::Dir { mtime: None });
        Self {
            nodes,
            failing: BTreeSet::new(),
        }
    }

    /// A root that is a single file (the user chose a file, not a folder).
    pub fn single_file(data: impl Into<Vec<u8>>, mtime_ms: i64) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            String::new(),
            MemNode::File {
                data: data.into(),
                mtime: Some(mtime_ms),
            },
        );
        Self {
            nodes,
            failing: BTreeSet::new(),
        }
    }

    /// Add a file at `path` (`/`-separated), modified at [`DEFAULT_NOW_MS`]. Parent folders are
    /// created as needed.
    pub fn file(self, path: &str, data: impl Into<Vec<u8>>) -> Self {
        self.file_at(path, data, DEFAULT_NOW_MS)
    }

    /// Add a file at `path` modified at `mtime_ms`.
    pub fn file_at(mut self, path: &str, data: impl Into<Vec<u8>>, mtime_ms: i64) -> Self {
        self.add_parents(path);
        self.nodes.insert(
            path.to_string(),
            MemNode::File {
                data: data.into(),
                mtime: Some(mtime_ms),
            },
        );
        self
    }

    /// Add an (empty) folder at `path`.
    pub fn dir(mut self, path: &str) -> Self {
        self.add_parents(path);
        self.nodes
            .entry(path.to_string())
            .or_insert(MemNode::Dir { mtime: None });
        self
    }

    /// Make reading or listing `path` fail with `Error::Other` (an unreadable file or folder).
    pub fn fail(mut self, path: &str) -> Self {
        self.failing.insert(path.to_string());
        self
    }

    /// Remove a file or folder (and everything below it).
    pub fn remove(&mut self, path: &str) {
        let prefix = format!("{path}/");
        self.nodes
            .retain(|p, _| p != path && !p.starts_with(&prefix));
    }

    fn add_parents(&mut self, path: &str) {
        assert!(
            matches!(self.nodes.get(""), Some(MemNode::Dir { .. })),
            "MemFs::single_file has no children"
        );
        let mut at = 0;
        while let Some(i) = path[at..].find('/') {
            let parent = &path[..at + i];
            self.nodes
                .entry(parent.to_string())
                .or_insert(MemNode::Dir { mtime: None });
            at += i + 1;
        }
    }
}

/// What a file root is backed by in the test.
#[derive(Debug, Clone)]
enum Mount {
    Mem(MemFs),
    Disk(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Plugin code called directly from a test: everything declared is allowed.
    Free,
    Validate,
    Sync,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostRule {
    scheme: String,
    host: String,
    wildcard: bool,
    port: Option<u16>,
}

struct Url {
    scheme: String,
    host: String,
    port: Option<u16>,
    has_userinfo: bool,
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "http" { 80 } else { 443 }
}

/// Split `host[:port]` (IPv6 in brackets); lowercases the host.
fn split_host_port(s: &str) -> Option<(String, Option<u16>)> {
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let end = rest.find(']')?;
        let after = &rest[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        (rest[..end].to_string(), port)
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p.parse().ok()?)),
            None => (s.to_string(), None),
        }
    };
    let host = host.to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_string();
    if host.is_empty() {
        return None;
    }
    Some((host, port))
}

impl HostRule {
    fn parse(entry: &str) -> core::result::Result<Self, String> {
        let bad = |why: &str| format!("host `{entry}`: {why}");
        let entry = entry.trim();
        let (scheme, rest) = match entry.split_once("://") {
            Some((s, r)) => (s.to_ascii_lowercase(), r),
            None => ("https".to_string(), entry),
        };
        if scheme != "https" && scheme != "http" {
            return Err(bad("scheme must be https or http"));
        }
        if rest.is_empty() || rest.contains(['/', '@', '?', '#', '\\', ' ']) {
            return Err(bad(
                "must be a bare host[:port] without path or credentials",
            ));
        }
        let (wildcard, rest) = match rest.strip_prefix("*.") {
            Some(r) => (true, r),
            None => (false, rest),
        };
        if rest.contains('*') {
            return Err(bad("`*` is only allowed as a leading `*.` label"));
        }
        let (host, port) = split_host_port(rest).ok_or_else(|| bad("bad host or port"))?;
        if wildcard && host.split('.').filter(|l| !l.is_empty()).count() < 2 {
            return Err(bad("a wildcard must cover a registered domain, not a TLD"));
        }
        let port = port.filter(|p| *p != default_port(&scheme));
        Ok(Self {
            scheme,
            host,
            wildcard,
            port,
        })
    }

    fn matches(&self, url: &Url) -> bool {
        if url.scheme != self.scheme || url.has_userinfo {
            return false;
        }
        let host_ok = if self.wildcard {
            url.host.len() > self.host.len()
                && url.host.ends_with(&self.host)
                && url.host.as_bytes()[url.host.len() - self.host.len() - 1] == b'.'
        } else {
            url.host == self.host
        };
        host_ok && url.port == self.port
    }
}

impl Url {
    fn parse(url: &str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "https" && scheme != "http" {
            return None;
        }
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        let (has_userinfo, hostport) = match authority.rsplit_once('@') {
            Some((_, hp)) => (true, hp),
            None => (false, authority),
        };
        let (host, port) = split_host_port(hostport)?;
        let port = port.filter(|p| *p != default_port(&scheme));
        Some(Self {
            scheme,
            host,
            port,
            has_userinfo,
        })
    }
}

#[derive(Debug, Clone, Default)]
struct Caps {
    connector: bool,
    tool: bool,
    hosts: Vec<HostRule>,
    secrets: BTreeSet<String>,
    kv_max_bytes: u64,
    file_roots: BTreeSet<String>,
}

type Handler = Rc<dyn Fn(&http::Request) -> Result<http::Response>>;

struct State {
    caps: Caps,
    limits: Limits,
    mode: Mode,
    settings: Option<Value>,
    routes: Vec<(String, String, Handler)>,
    requests: Vec<http::Request>,
    requests_this_call: usize,
    kv: BTreeMap<String, Vec<u8>>,
    secrets: BTreeMap<String, String>,
    now: i64,
    logs: Vec<(Level, String)>,
    mounts: BTreeMap<String, Mount>,
    fs_read_this_call: u64,
    fs_ops_this_call: u32,
    store: BTreeMap<String, Document>,
    seeded: BTreeMap<String, Option<String>>,
    events: Vec<SinkEvent>,
    cancelled: bool,
    cancel_after_upserts: Option<usize>,
    upserts_this_pass: usize,
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<RefCell<State>>>> = const { RefCell::new(None) };
}

const NO_HOST: &str = "no nugget_plugin_sdk::testing::TestHost on this thread: create one with \
                       TestHost::new() or TestHost::from_manifest(..) before calling plugin code";

/// The fake host. See the [module docs](self).
pub struct TestHost {
    state: Rc<RefCell<State>>,
}

impl Default for TestHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        CURRENT.with(|c| {
            let mut c = c.borrow_mut();
            if c.as_ref().is_some_and(|s| Rc::ptr_eq(s, &self.state)) {
                *c = None;
            }
        });
    }
}

/// Restores the previous mode, settings and active host when a helper call ends (also on panic).
struct CallGuard {
    state: Rc<RefCell<State>>,
    prev_mode: Mode,
    prev_settings: Option<Value>,
    prev_current: Option<Rc<RefCell<State>>>,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        let mut s = self.state.borrow_mut();
        s.mode = self.prev_mode;
        s.settings = self.prev_settings.take();
        drop(s);
        let prev = self.prev_current.take();
        CURRENT.with(|c| *c.borrow_mut() = prev);
    }
}

impl TestHost {
    /// A host that allows both kinds of export and no capabilities at all (no hosts, secrets,
    /// key-value store or file roots), like an empty `[capabilities]` table. Add capabilities
    /// with [`allow_host`](Self::allow_host) and friends, or start from
    /// [`from_manifest`](Self::from_manifest). Becomes the active host on this thread.
    pub fn new() -> Self {
        Self::with_caps(Caps {
            connector: true,
            tool: true,
            ..Caps::default()
        })
    }

    /// A host with the capabilities and kinds of a `nugget-plugin.toml`, typically
    /// `TestHost::from_manifest(include_str!("../nugget-plugin.toml"))`. Panics when the manifest
    /// is malformed (the fields the host checks: id, kind, hosts, secrets, kv size, file roots).
    pub fn from_manifest(toml_text: &str) -> Self {
        match parse_manifest(toml_text) {
            Ok(caps) => Self::with_caps(caps),
            Err(e) => panic!("bad nugget-plugin.toml: {e}"),
        }
    }

    fn with_caps(caps: Caps) -> Self {
        let state = Rc::new(RefCell::new(State {
            caps,
            limits: Limits::default(),
            mode: Mode::Free,
            settings: None,
            routes: Vec::new(),
            requests: Vec::new(),
            requests_this_call: 0,
            kv: BTreeMap::new(),
            secrets: BTreeMap::new(),
            now: DEFAULT_NOW_MS,
            logs: Vec::new(),
            mounts: BTreeMap::new(),
            fs_read_this_call: 0,
            fs_ops_this_call: 0,
            store: BTreeMap::new(),
            seeded: BTreeMap::new(),
            events: Vec::new(),
            cancelled: false,
            cancel_after_upserts: None,
            upserts_this_pass: 0,
        }));
        CURRENT.with(|c| *c.borrow_mut() = Some(state.clone()));
        Self { state }
    }

    fn st(&self) -> std::cell::RefMut<'_, State> {
        self.state.borrow_mut()
    }

    // ---- capabilities and limits ----

    /// Declare a host rule, as in `capabilities.hosts` (`example.com`, `*.example.com`,
    /// `http://127.0.0.1:8080`). Panics on a malformed rule.
    pub fn allow_host(&self, rule: &str) -> &Self {
        let rule = HostRule::parse(rule).unwrap_or_else(|e| panic!("{e}"));
        self.st().caps.hosts.push(rule);
        self
    }

    /// Declare a secret name, as in `capabilities.secrets`.
    pub fn allow_secret(&self, name: &str) -> &Self {
        self.st().caps.secrets.insert(name.to_string());
        self
    }

    /// Set `capabilities.kv_max_bytes` (`0` = no key-value store).
    pub fn set_kv_max_bytes(&self, bytes: u64) -> &Self {
        self.st().caps.kv_max_bytes = bytes;
        self
    }

    /// Declare a file root, as in `capabilities.file_roots`.
    pub fn allow_file_root(&self, root: &str) -> &Self {
        self.st().caps.file_roots.insert(root.to_string());
        self
    }

    /// Replace the resource limits.
    pub fn set_limits(&self, limits: Limits) -> &Self {
        self.st().limits = limits;
        self
    }

    // ---- http ----

    /// Answer `method url` (exact match, method case-insensitive) with `response`, every time.
    /// A later registration for the same method and URL wins.
    pub fn respond(&self, method: &str, url: &str, response: http::Response) -> &Self {
        self.respond_with(method, url, move |_| Ok(response.clone()))
    }

    /// Answer `method url` by calling `handler` with the request (for conditional requests,
    /// sequences or errors such as [`Error::Network`]).
    pub fn respond_with(
        &self,
        method: &str,
        url: &str,
        handler: impl Fn(&http::Request) -> Result<http::Response> + 'static,
    ) -> &Self {
        self.st().routes.insert(
            0,
            (
                method.to_ascii_uppercase(),
                url.to_string(),
                Rc::new(handler),
            ),
        );
        self
    }

    /// Every request the plugin sent, in order, including denied and unmatched ones.
    pub fn requests(&self) -> Vec<http::Request> {
        self.state.borrow().requests.clone()
    }

    pub fn clear_requests(&self) -> &Self {
        self.st().requests.clear();
        self
    }

    // ---- kv, secrets, clock, logs ----

    /// The raw value of a key-value entry.
    pub fn kv_get(&self, key: &str) -> Option<Vec<u8>> {
        self.state.borrow().kv.get(key).cloned()
    }

    /// A key-value entry parsed as JSON.
    pub fn kv_json(&self, key: &str) -> Option<Value> {
        self.kv_get(key)
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    /// Put an entry into the key-value store directly (no size check).
    pub fn kv_set(&self, key: &str, value: impl Into<Vec<u8>>) -> &Self {
        self.st().kv.insert(key.to_string(), value.into());
        self
    }

    /// Every key-value key, in order.
    pub fn kv_keys(&self) -> Vec<String> {
        self.state.borrow().kv.keys().cloned().collect()
    }

    /// Set a secret's value (the name must still be declared for the plugin to read it).
    pub fn set_secret(&self, name: &str, value: &str) -> &Self {
        self.st()
            .secrets
            .insert(name.to_string(), value.to_string());
        self
    }

    pub fn secret(&self, name: &str) -> Option<String> {
        self.state.borrow().secrets.get(name).cloned()
    }

    /// Set the fake clock (milliseconds since the Unix epoch).
    pub fn set_now(&self, ms: i64) -> &Self {
        self.st().now = ms;
        self
    }

    /// Move the fake clock forward.
    pub fn advance(&self, ms: i64) -> &Self {
        self.st().now += ms;
        self
    }

    /// Every log line, in order.
    pub fn logs(&self) -> Vec<(Level, String)> {
        self.state.borrow().logs.clone()
    }

    /// Whether some log line at `level` or above contains `needle`.
    pub fn logged(&self, level: Level, needle: &str) -> bool {
        self.state
            .borrow()
            .logs
            .iter()
            .any(|(l, m)| *l >= level && m.contains(needle))
    }

    // ---- fs ----

    /// Mount an in-memory tree as file root `root` (overrides the settings path).
    pub fn mount(&self, root: &str, fs: MemFs) -> &Self {
        self.st().mounts.insert(root.to_string(), Mount::Mem(fs));
        self
    }

    /// Mount a real folder or file (say, a temp dir) as file root `root`. Without a mount, the
    /// path in the settings field named `root` is used, like the real host does.
    pub fn mount_path(&self, root: &str, path: impl Into<PathBuf>) -> &Self {
        self.st()
            .mounts
            .insert(root.to_string(), Mount::Disk(path.into()));
        self
    }

    /// Change an in-memory mount in place (add, change or remove files between passes).
    pub fn edit_mount(&self, root: &str, f: impl FnOnce(&mut MemFs)) -> &Self {
        let mut st = self.st();
        match st.mounts.get_mut(root) {
            Some(Mount::Mem(fs)) => f(fs),
            _ => panic!("file root `{root}` is not an in-memory mount"),
        }
        drop(st);
        self
    }

    // ---- sink ----

    /// Pretend the store already holds `key` at `version` (returned by `known_items`).
    pub fn seed_item(&self, key: &str, version: Option<&str>) -> &Self {
        self.st()
            .seeded
            .insert(key.to_string(), version.map(str::to_string));
        self
    }

    /// The documents the store holds now: everything upserted and not deleted since.
    pub fn documents(&self) -> BTreeMap<String, Document> {
        self.state.borrow().store.clone()
    }

    pub fn document(&self, key: &str) -> Option<Document> {
        self.state.borrow().store.get(key).cloned()
    }

    /// What `sink::known_items` returns now: seeded items plus everything upserted, minus
    /// deletes, sorted by key.
    pub fn known_items(&self) -> Vec<(String, Option<String>)> {
        known_items_of(&self.state.borrow())
    }

    /// Every sink event since the host was created (including failed passes).
    pub fn sink_events(&self) -> Vec<SinkEvent> {
        self.state.borrow().events.clone()
    }

    /// Make `sink::cancelled` return `true` (or `false` again).
    pub fn set_cancelled(&self, cancelled: bool) -> &Self {
        self.st().cancelled = cancelled;
        self
    }

    /// Make `sink::cancelled` return `true` once a pass has upserted `n` documents.
    pub fn cancel_after_upserts(&self, n: usize) -> &Self {
        self.st().cancel_after_upserts = Some(n);
        self
    }

    // ---- running the plugin ----

    fn enter(&self, mode: Mode, settings: Option<Value>) -> CallGuard {
        let prev_current = CURRENT.with(|c| c.borrow_mut().replace(self.state.clone()));
        let mut st = self.st();
        let guard = CallGuard {
            state: self.state.clone(),
            prev_mode: st.mode,
            prev_settings: st.settings.take(),
            prev_current,
        };
        st.mode = mode;
        st.settings = settings;
        st.requests_this_call = 0;
        st.fs_read_this_call = 0;
        st.fs_ops_this_call = 0;
        guard
    }

    /// Run `C::validate` the way the host does (settings through JSON, no network, key-value
    /// store, secrets or sink).
    pub fn validate<C: Connector>(&self, settings: Value) -> Result<()> {
        let settings = json_round_trip(&settings)?;
        self.require_kind(true)?;
        let _guard = self.enter(Mode::Validate, Some(settings.clone()));
        C::validate(&settings)
    }

    /// Run one `C::sync` pass the way the host does, and return what it sent to the sink. On
    /// error, the events of the failed pass are still in [`sink_events`](Self::sink_events).
    pub fn sync<C: Connector>(&self, settings: Value, cursor: Option<Value>) -> Result<SyncPass> {
        let settings = json_round_trip(&settings)?;
        let cursor = cursor.as_ref().map(json_round_trip).transpose()?;
        self.require_kind(true)?;
        let first_event = {
            let mut st = self.st();
            st.upserts_this_pass = 0;
            st.events.len()
        };
        let guard = self.enter(Mode::Sync, Some(settings.clone()));
        let result = C::sync(&settings, cursor);
        drop(guard);
        let cursor = json_round_trip(&result?)?;
        let st = self.state.borrow();
        let mut pass = SyncPass {
            cursor,
            upserts: Vec::new(),
            deletes: Vec::new(),
            checkpoints: Vec::new(),
        };
        for e in &st.events[first_event..] {
            match e {
                SinkEvent::Upsert(d) => pass.upserts.push(d.clone()),
                SinkEvent::Delete(k) => pass.deletes.push(k.clone()),
                SinkEvent::Checkpoint(c) => pass.checkpoints.push(c.clone()),
            }
        }
        Ok(pass)
    }

    /// `T::tools()`, checked like the host checks them (names `[a-z0-9_]`, 1 to 64 characters,
    /// unique; at most 64 tools; object input schemas). Panics on a bad spec.
    pub fn tools<T: Tools>(&self) -> Vec<Tool> {
        let tools = T::tools();
        assert!(tools.len() <= 64, "more than 64 tools");
        let mut seen = BTreeSet::new();
        for t in &tools {
            assert!(
                !t.name.is_empty()
                    && t.name.len() <= 64
                    && t.name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "tool name `{}` must be 1-64 chars of [a-z0-9_]",
                t.name
            );
            assert!(seen.insert(&t.name), "tool `{}` listed twice", t.name);
            assert!(
                t.input_schema.is_object(),
                "tool `{}`: input_schema must be a JSON object",
                t.name
            );
        }
        tools
    }

    /// Call tool `name` the way the host does (read-only: `GET`/`HEAD`, key-value and secret
    /// reads, no sink or files; input and output through JSON with the size cap).
    pub fn call<T: Tools>(&self, name: &str, input: Value) -> Result<Value> {
        self.require_kind(false)?;
        let max = self.state.borrow().limits.max_tool_io_bytes;
        let text = input.to_string();
        if text.len() > max {
            return Err(Error::invalid(format!("tool input exceeds {max} bytes")));
        }
        let input = crate::__rt::parse(&text)?;
        let guard = self.enter(Mode::Tool, None);
        let out = T::call(name, input);
        drop(guard);
        let out = out?.to_string();
        if out.len() > max {
            return Err(Error::invalid(format!("tool output exceeds {max} bytes")));
        }
        crate::__rt::parse(&out)
    }

    fn require_kind(&self, connector: bool) -> Result<()> {
        let caps = &self.state.borrow().caps;
        match connector {
            true if !caps.connector => Err(Error::Denied(
                "the manifest's kind does not list connector".into(),
            )),
            false if !caps.tool => Err(Error::Denied(
                "the manifest's kind does not list tool".into(),
            )),
            _ => Ok(()),
        }
    }
}

fn json_round_trip(v: &Value) -> Result<Value> {
    crate::__rt::parse(&v.to_string())
}

fn known_items_of(st: &State) -> Vec<(String, Option<String>)> {
    let mut items = st.seeded.clone();
    for (k, d) in &st.store {
        items.insert(k.clone(), d.version.clone());
    }
    items.into_iter().collect()
}

// ---- manifest ----

fn parse_manifest(text: &str) -> core::result::Result<Caps, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let v = serde_json::to_value(&table).map_err(|e| e.to_string())?;
    let id = v["id"].as_str().ok_or("missing `id`")?;
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    if id.len() > 64 || id.split('.').count() < 2 || !id.split('.').all(label_ok) {
        return Err(format!("plugin id `{id}` must be reverse-DNS"));
    }
    for field in ["name", "version", "description"] {
        if !v[field].is_string() {
            return Err(format!("missing `{field}`"));
        }
    }
    let kinds: Vec<&str> = v["kind"]
        .as_array()
        .ok_or("missing `kind`")?
        .iter()
        .map(|k| k.as_str().unwrap_or(""))
        .collect();
    if kinds.is_empty() || kinds.iter().any(|k| *k != "connector" && *k != "tool") {
        return Err("`kind` must list connector and/or tool".into());
    }
    let mut caps = Caps {
        connector: kinds.contains(&"connector"),
        tool: kinds.contains(&"tool"),
        ..Caps::default()
    };
    let c = &v["capabilities"];
    if let Some(obj) = c.as_object()
        && let Some(k) = obj
            .keys()
            .find(|k| !["hosts", "secrets", "kv_max_bytes", "file_roots"].contains(&k.as_str()))
    {
        return Err(format!("unknown capability `{k}`"));
    }
    let strings = |field: &str| -> core::result::Result<Vec<String>, String> {
        match &c[field] {
            Value::Null => Ok(Vec::new()),
            Value::Array(a) => a
                .iter()
                .map(|s| {
                    s.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("capabilities.{field} must be strings"))
                })
                .collect(),
            _ => Err(format!("capabilities.{field} must be an array")),
        }
    };
    let hosts = strings("hosts")?;
    if hosts.len() > 64 {
        return Err("more than 64 hosts".into());
    }
    caps.hosts = hosts
        .iter()
        .map(|h| HostRule::parse(h))
        .collect::<core::result::Result<_, _>>()?;
    for s in strings("secrets")? {
        if s.is_empty()
            || s.len() > 64
            || !s
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            return Err(format!(
                "secret name `{s}` must be 1-64 chars of [a-z0-9_-]"
            ));
        }
        caps.secrets.insert(s);
    }
    caps.kv_max_bytes = match &c["kv_max_bytes"] {
        Value::Null => 0,
        n => n
            .as_u64()
            .ok_or("kv_max_bytes must be a non-negative integer")?,
    };
    if caps.kv_max_bytes > 16 * 1024 * 1024 {
        return Err("kv_max_bytes exceeds 16 MiB".into());
    }
    let roots = strings("file_roots")?;
    if roots.len() > 8 {
        return Err("more than 8 file_roots".into());
    }
    for r in roots {
        if r.is_empty()
            || r.len() > 64
            || !r
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(format!("file root `{r}` must be 1-64 chars of [a-z0-9_]"));
        }
        if !caps.connector {
            return Err("capabilities.file_roots is only allowed when kind has connector".into());
        }
        if let Some(props) = v["connector"]["settings_schema"]["properties"].as_object()
            && !props.contains_key(&r)
        {
            return Err(format!(
                "file root `{r}` is not a property of connector.settings_schema"
            ));
        }
        caps.file_roots.insert(r);
    }
    match (v["connector"].is_object(), caps.connector) {
        (false, true) => return Err("a connector plugin needs a [connector] section".into()),
        (true, false) => return Err("[connector] is only allowed when kind has connector".into()),
        _ => {}
    }
    Ok(caps)
}

// ---- the native `sys` backend the SDK modules call ----

#[doc(hidden)]
pub(crate) mod sys {
    use super::*;

    fn current() -> Rc<RefCell<State>> {
        CURRENT
            .with(|c| c.borrow().clone())
            .unwrap_or_else(|| panic!("{NO_HOST}"))
    }

    fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
        let st = current();
        let mut st = st.borrow_mut();
        f(&mut st)
    }

    fn deny(msg: impl Into<String>) -> Error {
        Error::Denied(msg.into())
    }

    pub(crate) fn http_send(req: http::Request) -> Result<http::Response> {
        let state = current();
        let handler = {
            let mut st = state.borrow_mut();
            st.requests.push(req.clone());
            check_request(&mut st, &req)?;
            let method = req.method.to_ascii_uppercase();
            st.routes
                .iter()
                .find(|(m, u, _)| *m == method && *u == req.url)
                .map(|(_, _, h)| h.clone())
        };
        let Some(handler) = handler else {
            return Err(Error::Network(format!(
                "TestHost: no canned response for {} {}",
                req.method, req.url
            )));
        };
        let resp = handler(&req)?;
        let max = state.borrow().limits.max_http_response_bytes;
        if resp.body.len() > max {
            return Err(Error::invalid(format!("response body exceeds {max} bytes")));
        }
        Ok(resp)
    }

    fn check_request(st: &mut State, req: &http::Request) -> Result<()> {
        let method = req.method.to_ascii_uppercase();
        match st.mode {
            Mode::Validate => return Err(deny("no network access in validate")),
            Mode::Tool if method != "GET" && method != "HEAD" => {
                return Err(deny(format!(
                    "tools may only send GET or HEAD, not {method}"
                )));
            }
            _ => {}
        }
        if !["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"].contains(&method.as_str()) {
            return Err(Error::invalid(format!(
                "method `{}` is not allowed",
                req.method
            )));
        }
        let url = Url::parse(&req.url).ok_or_else(|| {
            Error::invalid(format!("`{}` is not an absolute http(s) URL", req.url))
        })?;
        if !st.caps.hosts.iter().any(|r| r.matches(&url)) {
            return Err(deny(format!(
                "{}://{}{} is not a host declared in the manifest",
                url.scheme,
                url.host,
                url.port.map(|p| format!(":{p}")).unwrap_or_default()
            )));
        }
        if req.headers.len() > 64 {
            return Err(Error::invalid("too many request headers"));
        }
        for (name, _) in &req.headers {
            let lower = name.to_ascii_lowercase();
            if FORBIDDEN_HEADERS.contains(&lower.as_str()) || lower.starts_with("proxy-") {
                return Err(deny(format!("header `{name}` may not be set by a plugin")));
            }
        }
        if req
            .body
            .as_ref()
            .is_some_and(|b| b.len() > st.limits.max_http_request_bytes)
        {
            return Err(Error::invalid(format!(
                "request body exceeds {} bytes",
                st.limits.max_http_request_bytes
            )));
        }
        let cap = match st.mode {
            Mode::Sync => st.limits.max_http_requests_per_sync,
            _ => st.limits.max_http_requests_per_call,
        };
        if st.requests_this_call >= cap {
            return Err(deny(format!("more than {cap} HTTP requests in one call")));
        }
        st.requests_this_call += 1;
        Ok(())
    }

    fn kv_allowed(st: &State, write: bool) -> Result<()> {
        match st.mode {
            Mode::Validate => Err(deny("no key-value store in validate")),
            Mode::Tool if write => Err(deny("tools may only read the key-value store")),
            _ if st.caps.kv_max_bytes == 0 => Err(deny("the manifest declares no key-value store")),
            _ => Ok(()),
        }
    }

    fn check_key(key: &str) -> Result<()> {
        if key.is_empty() || key.len() > MAX_KV_KEY_BYTES {
            return Err(Error::invalid(format!(
                "kv keys must be 1-{MAX_KV_KEY_BYTES} bytes"
            )));
        }
        Ok(())
    }

    pub(crate) fn kv_get(key: &str) -> Result<Option<Vec<u8>>> {
        with(|st| {
            kv_allowed(st, false)?;
            check_key(key)?;
            Ok(st.kv.get(key).cloned())
        })
    }

    pub(crate) fn kv_set(key: &str, value: &[u8]) -> Result<()> {
        with(|st| {
            kv_allowed(st, true)?;
            check_key(key)?;
            let used: u64 = st
                .kv
                .iter()
                .filter(|(k, _)| k.as_str() != key)
                .map(|(k, v)| (k.len() + v.len()) as u64)
                .sum();
            let after = used + (key.len() + value.len()) as u64;
            if after > st.caps.kv_max_bytes {
                return Err(Error::invalid(format!(
                    "key-value store is full ({after} bytes > the declared {})",
                    st.caps.kv_max_bytes
                )));
            }
            st.kv.insert(key.to_string(), value.to_vec());
            Ok(())
        })
    }

    pub(crate) fn kv_delete(key: &str) -> Result<()> {
        with(|st| {
            kv_allowed(st, true)?;
            check_key(key)?;
            st.kv.remove(key);
            Ok(())
        })
    }

    pub(crate) fn kv_keys(prefix: &str) -> Result<Vec<String>> {
        with(|st| {
            kv_allowed(st, false)?;
            Ok(st
                .kv
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect())
        })
    }

    fn secret_allowed(st: &State, name: &str, write: bool) -> Result<()> {
        match st.mode {
            Mode::Validate => return Err(deny("no secrets in validate")),
            Mode::Tool if write => return Err(deny("secrets are read-only outside a sync")),
            _ => {}
        }
        if !st.caps.secrets.contains(name) {
            return Err(deny(format!(
                "secret `{name}` is not declared in the manifest"
            )));
        }
        Ok(())
    }

    pub(crate) fn secret_get(name: &str) -> Result<Option<String>> {
        with(|st| {
            secret_allowed(st, name, false)?;
            Ok(st.secrets.get(name).cloned())
        })
    }

    pub(crate) fn secret_set(name: &str, value: &str) -> Result<()> {
        with(|st| {
            secret_allowed(st, name, true)?;
            if value.len() > 64 * 1024 {
                return Err(Error::invalid("secret is too large"));
            }
            st.secrets.insert(name.to_string(), value.to_string());
            Ok(())
        })
    }

    pub(crate) fn log(level: Level, msg: &str) {
        // Captured by the test harness; shown when a test fails.
        eprintln!("[plugin {level:?}] {msg}");
        if let Some(st) = CURRENT.with(|c| c.borrow().clone()) {
            st.borrow_mut().logs.push((level, msg.to_string()));
        }
    }

    pub(crate) fn now() -> i64 {
        with(|st| st.now)
    }

    // ---- fs ----

    /// Validate a relative `/` path into its segments.
    /// Windows device names, refused in every folder on every OS (as the host does).
    const WINDOWS_DEVICES: &[&str] = &[
        "con", "prn", "aux", "nul", "conin$", "conout$", "com0", "com1", "com2", "com3", "com4",
        "com5", "com6", "com7", "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
        "lpt6", "lpt7", "lpt8", "lpt9", "com¹", "com²", "com³", "lpt¹", "lpt²", "lpt³",
    ];

    /// Check a relative `/` path the way the host does and return its segments: no `.`/`..`,
    /// no empty segments (so no leading or trailing `/`), no `\`, `:` or control characters, no
    /// segment ending in `.` or a space, no Windows device names; at most 4096 bytes, 128
    /// segments of 255 bytes.
    fn segments(path: &str) -> Result<Vec<&str>> {
        if path.is_empty() {
            return Ok(Vec::new());
        }
        let bad = |why: &str| Error::invalid(format!("bad path `{path}`: {why}"));
        if path.len() > 4096 {
            return Err(bad("longer than 4096 bytes"));
        }
        let segs: Vec<&str> = path.split('/').collect();
        if segs.len() > 128 {
            return Err(bad("more than 128 segments"));
        }
        for seg in &segs {
            if seg.is_empty() || *seg == "." || *seg == ".." {
                return Err(bad(
                    "use a relative `/` path without empty, `.` or `..` segments",
                ));
            }
            if seg.len() > 255 {
                return Err(bad("a segment is longer than 255 bytes"));
            }
            if seg
                .chars()
                .any(|c| matches!(c, '\\' | ':') || c.is_control())
            {
                return Err(bad("no `\\`, `:` or control characters"));
            }
            if seg.ends_with('.') || seg.ends_with(' ') {
                return Err(bad("segments may not end with `.` or a space"));
            }
            let stem = seg
                .split('.')
                .next()
                .unwrap_or(seg)
                .trim_end()
                .to_lowercase();
            if WINDOWS_DEVICES.contains(&stem.as_str()) {
                return Err(bad("reserved device name"));
            }
        }
        Ok(segs)
    }

    /// The mount for `root`, checking the mode, the declaration, the settings field and the
    /// operation budget (one operation per call, as the host counts them).
    fn mount_of(st: &mut State, root: &str) -> Result<Mount> {
        if st.mode == Mode::Tool {
            return Err(deny(
                "file access is only available to connectors (validate and sync)",
            ));
        }
        if !st.caps.file_roots.contains(root) {
            return Err(deny(format!(
                "file root `{root}` is not declared in the manifest or not approved"
            )));
        }
        let max_ops = match st.mode {
            Mode::Sync => st.limits.max_fs_ops_per_sync,
            _ => st.limits.max_fs_ops_per_call,
        };
        if st.fs_ops_this_call >= max_ops {
            return Err(deny("the file operation budget for this call is spent"));
        }
        st.fs_ops_this_call += 1;
        // Like the host, the settings must name a path for the root (when the call has
        // settings); a mount then stands in for that path.
        let setting = match st.settings.as_ref().map(|s| &s[root]) {
            Some(Value::String(p)) if !p.is_empty() => Some(p.clone()),
            Some(Value::String(_)) | Some(Value::Null) => {
                return Err(Error::invalid(format!(
                    "the source setting `{root}` (a folder or file path) is not set"
                )));
            }
            Some(_) => {
                return Err(Error::invalid(format!(
                    "the source setting `{root}` must be a path string"
                )));
            }
            None => None,
        };
        if let Some(m) = st.mounts.get(root) {
            return Ok(m.clone());
        }
        match setting {
            Some(p) => Ok(Mount::Disk(PathBuf::from(p))),
            None => Err(Error::NotFound(format!(
                "TestHost: file root `{root}` is not mounted (use TestHost::mount) and no \
                 settings name a path for it"
            ))),
        }
    }

    fn not_found(root: &str, path: &str) -> Error {
        Error::NotFound(format!("{root}:{path}"))
    }

    fn ms(t: std::io::Result<std::time::SystemTime>) -> Option<i64> {
        let d = t.ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        i64::try_from(d.as_millis()).ok()
    }

    fn disk_entry(name: String, meta: &std::fs::Metadata) -> fs::DirEntry {
        fs::DirEntry {
            name,
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified_at: ms(meta.modified()),
        }
    }

    /// The canonical root and the canonical path of `segs` below it. Fails with `not-found`
    /// when it does not exist and `unauthorized` when a symlink leads out of the root.
    fn disk_resolve(
        base: &Path,
        segs: &[&str],
        root: &str,
        path: &str,
    ) -> Result<(PathBuf, PathBuf)> {
        if !base.is_absolute() {
            return Err(Error::invalid(format!(
                "the path for file root `{root}` must be absolute"
            )));
        }
        let base = base.canonicalize().map_err(|_| not_found(root, ""))?;
        let mut p = base.clone();
        for s in segs {
            p.push(s);
        }
        let canon = p.canonicalize().map_err(|_| not_found(root, path))?;
        if !canon.starts_with(&base) {
            // The host reports this as `denied`.
            return Err(Error::Denied(format!(
                "`{path}` leads outside the folder the user chose"
            )));
        }
        Ok((base, canon))
    }

    fn mem_entry(name: &str, node: &MemNode) -> fs::DirEntry {
        match node {
            MemNode::Dir { mtime } => fs::DirEntry {
                name: name.to_string(),
                is_dir: true,
                size: 0,
                modified_at: *mtime,
            },
            MemNode::File { data, mtime } => fs::DirEntry {
                name: name.to_string(),
                is_dir: false,
                size: data.len() as u64,
                modified_at: *mtime,
            },
        }
    }

    pub(crate) fn fs_read_dir(root: &str, path: &str) -> Result<Vec<fs::DirEntry>> {
        let (mount, max_entries) =
            with(|st| Ok::<_, Error>((mount_of(st, root)?, st.limits.max_fs_dir_entries)))?;
        let segs = segments(path)?;
        let mut out = match mount {
            Mount::Mem(m) => {
                let key = segs.join("/");
                if m.failing.contains(&key) {
                    return Err(Error::other(format!("{root}:{path}: permission denied")));
                }
                match m.nodes.get(&key) {
                    Some(MemNode::Dir { .. }) => {}
                    Some(MemNode::File { .. }) => {
                        return Err(Error::invalid(format!("{root}:{path} is not a folder")));
                    }
                    None => return Err(not_found(root, path)),
                }
                let prefix = if key.is_empty() {
                    String::new()
                } else {
                    format!("{key}/")
                };
                m.nodes
                    .iter()
                    .filter_map(|(p, n)| {
                        let rest = p.strip_prefix(&prefix)?;
                        (!rest.is_empty() && !rest.contains('/')).then(|| mem_entry(rest, n))
                    })
                    .collect::<Vec<_>>()
            }
            Mount::Disk(base) => {
                let (base, dir) = disk_resolve(&base, &segs, root, path)?;
                if !dir.is_dir() {
                    return Err(Error::invalid(format!("{root}:{path} is not a folder")));
                }
                let rd = std::fs::read_dir(&dir).map_err(|e| Error::other(e.to_string()))?;
                let mut out = Vec::new();
                for e in rd.flatten() {
                    let Ok(name) = e.file_name().into_string() else {
                        continue;
                    };
                    // Names a plugin could not address again are left out, as the host does.
                    if segments(&name).is_err() {
                        continue;
                    }
                    // Follow symlinks, but leave out the ones that leave the root.
                    let Ok(target) = e.path().canonicalize() else {
                        continue;
                    };
                    if !target.starts_with(&base) {
                        continue;
                    }
                    let Ok(meta) = std::fs::metadata(&target) else {
                        continue;
                    };
                    out.push(disk_entry(name, &meta));
                }
                out
            }
        };
        if out.len() > max_entries {
            return Err(Error::invalid(format!(
                "{root}:{path} has more than {max_entries} entries"
            )));
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub(crate) fn fs_read_file(root: &str, path: &str) -> Result<Vec<u8>> {
        let (mount, limits) = with(|st| Ok::<_, Error>((mount_of(st, root)?, st.limits.clone())))?;
        let segs = segments(path)?;
        let data = match mount {
            Mount::Mem(m) if m.failing.contains(&segs.join("/")) => {
                return Err(Error::other(format!("{root}:{path}: permission denied")));
            }
            Mount::Mem(m) => match m.nodes.get(&segs.join("/")) {
                Some(MemNode::File { data, .. }) => {
                    if data.len() as u64 > limits.max_fs_file_bytes {
                        return Err(too_big(root, path, &limits));
                    }
                    data.clone()
                }
                Some(MemNode::Dir { .. }) => {
                    return Err(Error::invalid(format!("{root}:{path} is a folder")));
                }
                None => return Err(not_found(root, path)),
            },
            Mount::Disk(base) => {
                let (_, file) = disk_resolve(&base, &segs, root, path)?;
                let meta = std::fs::metadata(&file).map_err(|_| not_found(root, path))?;
                if meta.is_dir() {
                    return Err(Error::invalid(format!("{root}:{path} is a folder")));
                }
                if meta.len() > limits.max_fs_file_bytes {
                    return Err(too_big(root, path, &limits));
                }
                std::fs::read(&file).map_err(|e| Error::other(e.to_string()))?
            }
        };
        with(|st| {
            let budget = match st.mode {
                Mode::Sync => limits.max_fs_read_bytes_per_sync,
                _ => limits.max_fs_read_bytes_per_call,
            };
            if st.fs_read_this_call + data.len() as u64 > budget {
                return Err(deny("the file read budget for this call is spent"));
            }
            st.fs_read_this_call += data.len() as u64;
            Ok(())
        })?;
        Ok(data)
    }

    fn too_big(root: &str, path: &str, limits: &Limits) -> Error {
        Error::invalid(format!(
            "{root}:{path} is larger than {} bytes",
            limits.max_fs_file_bytes
        ))
    }

    pub(crate) fn fs_stat(root: &str, path: &str) -> Result<fs::DirEntry> {
        let mount = with(|st| mount_of(st, root))?;
        let segs = segments(path)?;
        let name = segs.last().copied().unwrap_or("");
        match mount {
            Mount::Mem(m) => m
                .nodes
                .get(&segs.join("/"))
                .map(|n| mem_entry(name, n))
                .ok_or_else(|| not_found(root, path)),
            Mount::Disk(base) => {
                let (_, p) = disk_resolve(&base, &segs, root, path)?;
                let meta = std::fs::metadata(&p).map_err(|_| not_found(root, path))?;
                Ok(disk_entry(name.to_string(), &meta))
            }
        }
    }

    // ---- sink ----

    fn sink_allowed(st: &State) -> Result<()> {
        match st.mode {
            Mode::Sync | Mode::Free => Ok(()),
            _ => Err(deny("the sink is only available during a sync")),
        }
    }

    pub(crate) fn sink_upsert(doc: Document) -> Result<()> {
        with(|st| {
            sink_allowed(st)?;
            if doc.key.is_empty() || doc.key.len() > 1024 {
                return Err(Error::invalid("document keys must be 1-1024 bytes"));
            }
            if doc.mime.is_empty() || doc.mime.len() > 255 {
                return Err(Error::invalid("document mime must be 1-255 bytes"));
            }
            let size = match &doc.content {
                Content::Text(t) => t.len(),
                Content::Bytes(b) => b.len(),
            };
            if size > st.limits.max_document_bytes {
                return Err(Error::invalid(format!(
                    "document content exceeds {} bytes",
                    st.limits.max_document_bytes
                )));
            }
            if !doc.metadata.is_object() {
                return Err(Error::invalid("document metadata must be a JSON object"));
            }
            st.upserts_this_pass += 1;
            st.seeded.remove(&doc.key);
            st.store.insert(doc.key.clone(), doc.clone());
            st.events.push(SinkEvent::Upsert(doc));
            Ok(())
        })
    }

    pub(crate) fn sink_delete(key: &str) -> Result<()> {
        with(|st| {
            sink_allowed(st)?;
            if key.is_empty() || key.len() > 1024 {
                return Err(Error::invalid("document keys must be 1-1024 bytes"));
            }
            st.seeded.remove(key);
            st.store.remove(key);
            st.events.push(SinkEvent::Delete(key.to_string()));
            Ok(())
        })
    }

    pub(crate) fn sink_checkpoint(cursor: &str) -> Result<()> {
        with(|st| {
            sink_allowed(st)?;
            let v = crate::__rt::parse(cursor)?;
            st.events.push(SinkEvent::Checkpoint(v));
            Ok(())
        })
    }

    pub(crate) fn sink_known_items() -> Result<Vec<(String, Option<String>)>> {
        with(|st| {
            sink_allowed(st)?;
            Ok(known_items_of(st))
        })
    }

    pub(crate) fn sink_cancelled() -> bool {
        with(|st| {
            st.cancelled
                || st
                    .cancel_after_upserts
                    .is_some_and(|n| st.upserts_this_pass >= n)
        })
    }
}

#[cfg(test)]
mod tests;
