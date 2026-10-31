//! Write Nugget plugins (connectors and read-only agent tools) in Rust.
//!
//! A plugin is a WebAssembly component built for `wasm32-wasip2` (`crate-type = ["cdylib"]`)
//! that implements the `nugget:plugin/plugin` world. Implement [`Connector`] and/or [`Tools`]
//! and export them with [`export_plugin!`]:
//!
//! ```ignore
//! use nugget_plugin_sdk::{Connector, Result, Value, export_plugin, sink, http};
//!
//! struct MyFeed;
//!
//! impl Connector for MyFeed {
//!     fn sync(settings: &Value, cursor: Option<Value>) -> Result<Value> {
//!         let body = http::get(settings["url"].as_str().unwrap_or_default())?.text()?;
//!         sink::upsert(nugget_plugin_sdk::Document::text("only", "text/plain", body))?;
//!         Ok(Value::Null)
//!     }
//! }
//!
//! export_plugin!(connector: MyFeed);
//! ```
//!
//! ## What a plugin can do
//! Only what the host imports offer, each checked against the capabilities in
//! `nugget-plugin.toml`: [`http`] (declared hosts only, no redirects), [`kv`], [`secrets`]
//! (declared names only), [`log`](mod@log), [`now_ms`], [`fs`] (connectors only, below the
//! paths the user chose) and, during a sync, [`sink`].
//!
//! Rust `std` APIs that need WASI (files, environment variables, `SystemTime`, stdout, and the
//! random seed behind `std::collections::HashMap::new`) **trap**. Use [`now_ms`], [`log`](mod@log)
//! and `BTreeMap` instead.
//!
//! ## Testing natively
//! On any target other than `wasm32`, the host imports go to an in-process fake host instead:
//! [`testing::TestHost`]. Plugin logic can then be unit-tested with plain `cargo test` on the
//! host triple (`cargo test-native` in the SDK and plugin workspaces), with canned HTTP
//! responses, in-memory key-value store and secrets, a fake clock, file roots, and a sink that
//! records what the connector sent. No network, no wasm runtime.

pub use serde_json::{self, Value, json};

#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "../wit",
        world: "nugget:plugin/plugin",
        pub_export_macro: true,
        export_macro_name: "__export_world",
        default_bindings_module: "nugget_plugin_sdk::bindings",
    });
}

#[cfg(target_arch = "wasm32")]
#[path = "sys_wasm.rs"]
mod sys;

#[cfg(not(target_arch = "wasm32"))]
pub mod testing;
#[cfg(not(target_arch = "wasm32"))]
use testing::sys;

/// Why a call failed. Return [`Error::Cancelled`] when [`sink::cancelled`] says so.
///
/// The variants mirror the WIT `plugin-error`; the host maps them onto Nugget's error kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Bad input or settings.
    Invalid(String),
    /// A credential is missing or was rejected.
    Unauthorized(String),
    /// A network failure, or a 5xx from the remote.
    Network(String),
    /// The remote asked to back off. The payload is the hint, in milliseconds.
    RateLimited(Option<u64>),
    NotFound(String),
    /// The host denied a capability the plugin did not declare, or one the user did not approve.
    Denied(String),
    /// The sync or call was cancelled. Return promptly.
    Cancelled,
    Other(String),
}

pub type Result<T, E = Error> = core::result::Result<T, E>;

impl Error {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::Invalid(msg.into())
    }

    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Invalid(m) => write!(f, "invalid: {m}"),
            Error::Unauthorized(m) => write!(f, "unauthorized: {m}"),
            Error::Network(m) => write!(f, "network: {m}"),
            Error::RateLimited(Some(ms)) => write!(f, "rate limited (retry in {ms} ms)"),
            Error::RateLimited(None) => f.write_str("rate limited"),
            Error::NotFound(m) => write!(f, "not found: {m}"),
            Error::Denied(m) => write!(f, "denied: {m}"),
            Error::Cancelled => f.write_str("cancelled"),
            Error::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Invalid(format!("json: {e}"))
    }
}

/// A connector: one configured source, synced with a cursor.
pub trait Connector {
    /// Reject bad settings before the source is saved. No network access here.
    fn validate(settings: &Value) -> Result<()> {
        let _ = settings;
        Ok(())
    }

    /// One sync pass. `cursor` is what the previous pass returned; `None` asks for a reconciling
    /// full pass: upsert every current document and delete every [`sink::known_items`] key that
    /// is no longer in scope. Returns the cursor for the next pass.
    fn sync(settings: &Value, cursor: Option<Value>) -> Result<Value>;
}

/// A tool for the agent. Tools are read-only: `GET`/`HEAD` only, key-value and secret reads only.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    /// `[a-z0-9_]`, 1 to 64 characters.
    pub name: String,
    pub description: String,
    /// JSON Schema of the input object.
    pub input_schema: Value,
}

impl Tool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// Read-only tools for the agent.
pub trait Tools {
    fn tools() -> Vec<Tool>;
    fn call(name: &str, input: Value) -> Result<Value>;
}

/// Placeholder for a plugin without a connector.
pub struct NoConnector;

impl Connector for NoConnector {
    fn validate(_: &Value) -> Result<()> {
        Err(Error::other("this plugin has no connector"))
    }

    fn sync(_: &Value, _: Option<Value>) -> Result<Value> {
        Err(Error::other("this plugin has no connector"))
    }
}

/// Placeholder for a plugin without tools.
pub struct NoTools;

impl Tools for NoTools {
    fn tools() -> Vec<Tool> {
        Vec::new()
    }

    fn call(name: &str, _: Value) -> Result<Value> {
        Err(Error::NotFound(format!("no tool `{name}`")))
    }
}

/// Export a connector and/or tools as the plugin's implementation of the world.
///
/// ```ignore
/// export_plugin!(connector: MyConnector, tools: MyTools);
/// export_plugin!(connector: MyConnector);
/// export_plugin!(tools: MyTools);
/// ```
///
/// On native targets (tests) this only checks the types; drive the plugin through
/// [`testing::TestHost`] instead.
#[macro_export]
macro_rules! export_plugin {
    (connector: $c:ty, tools: $t:ty $(,)?) => {
        $crate::__nugget_export_impl!($c, $t);
    };
    (connector: $c:ty $(,)?) => {
        $crate::__nugget_export_impl!($c, $crate::NoTools);
    };
    (tools: $t:ty $(,)?) => {
        $crate::__nugget_export_impl!($crate::NoConnector, $t);
    };
}

#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
#[macro_export]
macro_rules! __nugget_export_impl {
    ($c:ty, $t:ty) => {
        struct __NuggetPlugin;

        impl $crate::bindings::exports::nugget::plugin::connector::Guest for __NuggetPlugin {
            fn validate(settings: String) -> ::core::result::Result<(), $crate::__rt::WitError> {
                $crate::__rt::validate::<$c>(&settings)
            }

            fn sync(
                settings: String,
                cursor: Option<String>,
            ) -> ::core::result::Result<String, $crate::__rt::WitError> {
                $crate::__rt::sync::<$c>(&settings, cursor)
            }
        }

        impl $crate::bindings::exports::nugget::plugin::tools::Guest for __NuggetPlugin {
            fn list_tools() -> Vec<$crate::bindings::exports::nugget::plugin::tools::ToolSpec> {
                $crate::__rt::list_tools::<$t>()
            }

            fn call(
                name: String,
                input: String,
            ) -> ::core::result::Result<String, $crate::__rt::WitError> {
                $crate::__rt::call::<$t>(&name, &input)
            }
        }

        $crate::bindings::__export_world!(__NuggetPlugin with_types_in $crate::bindings);
    };
}

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __nugget_export_impl {
    ($c:ty, $t:ty) => {
        // Native builds export nothing; this only checks the types implement the traits.
        const _: fn() = || {
            $crate::__rt::check::<$c, $t>();
        };
    };
}

#[doc(hidden)]
pub mod __rt {
    use super::*;

    /// The JSON the host passes in: an empty string means `null`.
    pub fn parse(json: &str) -> Result<Value> {
        if json.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(json)?)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn check<C: Connector, T: Tools>() {}

    #[cfg(target_arch = "wasm32")]
    pub use wasm::*;

    #[cfg(target_arch = "wasm32")]
    mod wasm {
        use super::*;
        use crate::bindings::exports::nugget::plugin::tools::ToolSpec;

        pub use crate::sys::WitError;

        fn install_panic_hook() {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                // The default hook writes to stderr, which plugins do not have.
                std::panic::set_hook(Box::new(|info| {
                    crate::log::error(&format!("panic: {info}"));
                }));
            });
        }

        pub fn validate<C: Connector>(settings: &str) -> Result<(), WitError> {
            install_panic_hook();
            C::validate(&parse(settings)?).map_err(WitError::from)
        }

        pub fn sync<C: Connector>(
            settings: &str,
            cursor: Option<String>,
        ) -> Result<String, WitError> {
            install_panic_hook();
            let run = || -> Result<String> {
                let cursor = cursor.as_deref().map(parse).transpose()?;
                let next = C::sync(&parse(settings)?, cursor)?;
                Ok(next.to_string())
            };
            run().map_err(WitError::from)
        }

        pub fn list_tools<T: Tools>() -> Vec<ToolSpec> {
            install_panic_hook();
            T::tools()
                .into_iter()
                .map(|t| ToolSpec {
                    name: t.name,
                    description: t.description,
                    input_schema: t.input_schema.to_string(),
                })
                .collect()
        }

        pub fn call<T: Tools>(name: &str, input: &str) -> Result<String, WitError> {
            install_panic_hook();
            let run = || -> Result<String> { Ok(T::call(name, parse(input)?)?.to_string()) };
            run().map_err(WitError::from)
        }
    }
}

/// One document reported by a connector.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// Stable key within the source.
    pub key: String,
    pub title: Option<String>,
    pub mime: String,
    pub content: Content,
    /// Cheap change token; the pipeline skips unchanged versions.
    pub version: Option<String>,
    /// Milliseconds since the Unix epoch.
    pub modified_at: Option<i64>,
    pub url: Option<String>,
    /// A JSON object.
    pub metadata: Value,
}

/// Document content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Bytes(Vec<u8>),
}

impl Document {
    pub fn text(key: impl Into<String>, mime: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(key, mime, Content::Text(text.into()))
    }

    pub fn bytes(key: impl Into<String>, mime: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self::new(key, mime, Content::Bytes(bytes))
    }

    fn new(key: impl Into<String>, mime: impl Into<String>, content: Content) -> Self {
        Self {
            key: key.into(),
            title: None,
            mime: mime.into(),
            content,
            version: None,
            modified_at: None,
            url: None,
            metadata: Value::Object(Default::default()),
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn with_modified_at(mut self, ms: i64) -> Self {
        self.modified_at = Some(ms);
        self
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// The content as text, when it is [`Content::Text`].
    pub fn text_content(&self) -> Option<&str> {
        match &self.content {
            Content::Text(t) => Some(t),
            Content::Bytes(_) => None,
        }
    }
}

/// Host-mediated HTTP: only hosts declared in the manifest, no redirects, metered.
pub mod http {
    use super::*;

    /// An HTTP request. Build with [`Request::get`] or [`Request::new`], then [`send`](Request::send).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Request {
        pub method: String,
        pub url: String,
        pub headers: Vec<(String, String)>,
        pub body: Option<Vec<u8>>,
    }

    impl Request {
        pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
            Self {
                method: method.into(),
                url: url.into(),
                headers: Vec::new(),
                body: None,
            }
        }

        pub fn get(url: impl Into<String>) -> Self {
            Self::new("GET", url)
        }

        pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
            self.headers.push((name.into(), value.into()));
            self
        }

        pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
            self.body = Some(body.into());
            self
        }

        /// The first header named `name` (case-insensitive).
        pub fn header_value(&self, name: &str) -> Option<&str> {
            find_header(&self.headers, name)
        }

        pub fn send(self) -> Result<Response> {
            sys::http_send(self)
        }
    }

    /// A fully read response.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Response {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    impl Response {
        /// A response with `status`, no headers and an empty body (handy for tests).
        pub fn new(status: u16) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: Vec::new(),
            }
        }

        /// `200 OK` with `body`.
        pub fn ok(body: impl Into<Vec<u8>>) -> Self {
            Self::new(200).with_body(body)
        }

        pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
            self.headers.push((name.into(), value.into()));
            self
        }

        pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
            self.body = body.into();
            self
        }

        /// The first header named `name` (case-insensitive).
        pub fn header(&self, name: &str) -> Option<&str> {
            find_header(&self.headers, name)
        }

        pub fn text(&self) -> Result<String> {
            String::from_utf8(self.body.clone())
                .map_err(|_| Error::invalid("response body is not UTF-8"))
        }

        pub fn json(&self) -> Result<Value> {
            Ok(serde_json::from_slice(&self.body)?)
        }

        /// `Err` for anything but 2xx, mapped like the host's own connectors map them.
        pub fn error_for_status(self) -> Result<Self> {
            match self.status {
                200..=299 => Ok(self),
                401 | 403 => Err(Error::Unauthorized(format!("HTTP {}", self.status))),
                429 => Err(Error::RateLimited(
                    self.header("retry-after")
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(|s| s.saturating_mul(1000)),
                )),
                404 => Err(Error::NotFound(format!("HTTP {}", self.status))),
                500..=599 => Err(Error::Network(format!("HTTP {}", self.status))),
                s => Err(Error::Other(format!("HTTP {s}"))),
            }
        }
    }

    /// `GET url`.
    pub fn get(url: &str) -> Result<Response> {
        Request::get(url).send()
    }
}

/// The plugin's key-value store (declared size; read-only in tools).
pub mod kv {
    use super::*;

    pub fn get(key: &str) -> Result<Option<Vec<u8>>> {
        sys::kv_get(key)
    }

    pub fn set(key: &str, value: &[u8]) -> Result<()> {
        sys::kv_set(key, value)
    }

    pub fn delete(key: &str) -> Result<()> {
        sys::kv_delete(key)
    }

    /// Every key that starts with `prefix`, in order.
    pub fn keys(prefix: &str) -> Result<Vec<String>> {
        sys::kv_keys(prefix)
    }

    pub fn get_json(key: &str) -> Result<Option<Value>> {
        get(key)?
            .map(|b| serde_json::from_slice(&b).map_err(Error::from))
            .transpose()
    }

    pub fn set_json(key: &str, value: &Value) -> Result<()> {
        set(key, value.to_string().as_bytes())
    }
}

/// The plugin's own keychain secrets (declared names only).
pub mod secrets {
    use super::*;

    pub fn get(name: &str) -> Result<Option<String>> {
        sys::secret_get(name)
    }

    pub fn set(name: &str, value: &str) -> Result<()> {
        sys::secret_set(name, value)
    }
}

/// Logging into Nugget's log.
pub mod log {
    /// Severity of a log line.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum Level {
        Trace,
        Debug,
        Info,
        Warn,
        Error,
    }

    pub fn log(level: Level, msg: &str) {
        super::sys::log(level, msg)
    }

    pub fn trace(msg: &str) {
        log(Level::Trace, msg)
    }
    pub fn debug(msg: &str) {
        log(Level::Debug, msg)
    }
    pub fn info(msg: &str) {
        log(Level::Info, msg)
    }
    pub fn warn(msg: &str) {
        log(Level::Warn, msg)
    }
    pub fn error(msg: &str) {
        log(Level::Error, msg)
    }
}

/// Milliseconds since the Unix epoch (the only clock a plugin has).
pub fn now_ms() -> i64 {
    sys::now()
}

/// Read-only files and folders the user chose for this source (connectors only). `root` is the
/// name of a settings field listed in the manifest's `capabilities.file_roots`; `path` is
/// relative to it, `/`-separated, and `""` means the root itself.
pub mod fs {
    use super::*;

    /// One file or folder.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DirEntry {
        pub name: String,
        pub is_dir: bool,
        pub size: u64,
        /// Milliseconds since the Unix epoch.
        pub modified_at: Option<i64>,
    }

    /// A folder's entries, sorted by name.
    pub fn read_dir(root: &str, path: &str) -> Result<Vec<DirEntry>> {
        sys::fs_read_dir(root, path)
    }

    /// A whole file.
    pub fn read_file(root: &str, path: &str) -> Result<Vec<u8>> {
        sys::fs_read_file(root, path)
    }

    /// A whole file as UTF-8 text (invalid sequences replaced).
    pub fn read_to_string(root: &str, path: &str) -> Result<String> {
        Ok(String::from_utf8_lossy(&read_file(root, path)?).into_owned())
    }

    /// One entry's metadata.
    pub fn stat(root: &str, path: &str) -> Result<DirEntry> {
        sys::fs_stat(root, path)
    }

    /// `parent` joined with `name` (`""` is the root).
    pub fn join(parent: &str, name: &str) -> String {
        if parent.is_empty() {
            name.to_string()
        } else {
            format!("{parent}/{name}")
        }
    }
}

/// Where a connector sends its changes (during `sync` only).
pub mod sink {
    use super::*;

    /// Queue an insert or update.
    pub fn upsert(doc: Document) -> Result<()> {
        sys::sink_upsert(doc)
    }

    /// Queue a delete.
    pub fn delete(key: &str) -> Result<()> {
        sys::sink_delete(key)
    }

    /// Commit `cursor` once everything sent before it is applied.
    pub fn checkpoint(cursor: &Value) -> Result<()> {
        sys::sink_checkpoint(&cursor.to_string())
    }

    /// `(key, version)` of every item the store holds for this source.
    pub fn known_items() -> Result<Vec<(String, Option<String>)>> {
        sys::sink_known_items()
    }

    /// Whether the host asked this sync to stop.
    pub fn cancelled() -> bool {
        sys::sink_cancelled()
    }
}
