//! `nugget-mcp`: Nugget's [Model Context Protocol](https://modelcontextprotocol.io) server.
//!
//! It lets MCP apps (Claude Desktop, Claude Code, Cursor, ...) query the user's knowledge, and
//! nothing more:
//!
//! - **Read-only tools:** `search`, `get_context` (`[n]`-cited passages), `find_entities`,
//!   `get_entity` (facts with citations), `get_neighbourhood`, `get_item` (size-capped) and
//!   `recent_changes`. **Resources:** entity pages `nugget://entity/<id>` (Markdown) and items
//!   `nugget://item/<source>/<key>`.
//! - **Local only.** stdio (through the `nuggetd mcp --client <id>` bridge, see [`stdio`]) and
//!   streamable HTTP bound to 127.0.0.1 ([`http`]). There is no remote transport in M2.
//! - **Per-client tokens** (`ngt_…`, stored only as a keyed hash), revocation and `last_used`
//!   ([`auth`]); Host/Origin checks against DNS rebinding, closed CORS, bearer auth on every
//!   request and no secrets in URLs; per-client rate limits ([`rate`]).
//! - **Scopes on every result** ([`scope`]): a sources allow-list, a tools allow-list, and
//!   sensitive items excluded unless the client has `include_sensitive`, down to individual fact
//!   evidence and context passages.
//! - **Audit**: one entry per call through a callback, with an argument summary and item
//!   references, never content ([`audit`]).
//! - **Prompt-injection hygiene**: source content is returned inside source-attributed
//!   `<untrusted-content>` blocks that content cannot break out of, and every tool description
//!   says to treat it as data ([`render`]).
//!
//! The daemon provides the data through the [`KnowledgeBackend`] trait; [`FakeBackend`] is an
//! in-memory implementation for tests. Built on the official Rust SDK, `rmcp`.
//!
//! ```no_run
//! # async fn demo(backend: std::sync::Arc<dyn nugget_mcp::KnowledgeBackend>) -> nugget_core::Result<()> {
//! use std::sync::Arc;
//! use nugget_mcp::{ClientRegistry, HttpConfig, McpServer, MemClientStore, ServerKey};
//!
//! let registry = Arc::new(ClientRegistry::new(Arc::new(MemClientStore::new()), ServerKey::generate()?));
//! let server = McpServer::builder(backend)
//!     .registry(registry.clone())
//!     .audit(|entry| println!("{entry:?}"))
//!     .build();
//! let http = server.serve_http(HttpConfig::default()).await?;
//! println!("MCP on {}", http.url());
//! # Ok(()) }
//! ```

pub mod audit;
pub mod auth;
pub mod backend;
pub mod fake;
pub mod http;
pub mod rate;
pub mod render;
pub mod scope;
pub mod server;
pub mod snippets;
pub mod stdio;
pub mod tools;

pub use audit::{AuditEntry, AuditSink, Transport};
pub use auth::{ClientRecord, ClientRegistry, ClientStore, ClientToken, MemClientStore, ServerKey};
pub use backend::*;
pub use fake::FakeBackend;
pub use http::{HttpConfig, HttpHandle};
pub use rate::RateLimit;
pub use scope::{DetailOrigins, Scope, ScopeFilter, VAULT_SOURCE};
pub use server::{McpConfig, McpHandler, McpServer, McpServerBuilder};
pub use snippets::{SnippetOptions, client_created, setup_snippets};
pub use stdio::{TOKEN_ENV, accept_bridge, bridge_connect, serve_stdio};

/// Re-export of the MCP SDK, for callers that drive [`McpHandler`] directly.
pub use rmcp;
