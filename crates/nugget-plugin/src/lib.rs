//! Third-party extensions (M6): WebAssembly components with declared, user-approved
//! capabilities, used for community connectors and read-only agent tools.
//!
//! - **Interface.** The WIT world `nugget:plugin/plugin` (`wit/plugin.wit`). A plugin exports a
//!   `connector` (cursor-based sync that emits upserts, deletes and checkpoints) and/or `tools`.
//!   It imports only `http`, `kv`, `secrets`, `log`, `clock` and, during a sync, `sink`. There is
//!   no filesystem, socket, environment or other clock: WASI imports are linked to traps.
//! - **Capabilities** ([`Capabilities`]) are declared in `nugget-plugin.toml`, approved by the
//!   user at install, and enforced by the host on every import call (deny by default).
//! - **Limits** ([`Limits`]): fuel, an epoch deadline and a `tokio` timeout per call, and a cap
//!   on linear memory. Every call gets a fresh instance from a cached, pre-linked component.
//! - **Packages** ([`PluginPackage`]) are signed with ed25519 over the SHA-256 of the manifest and
//!   of the component; [`PluginStore`] installs, updates, removes and lists them in a
//!   content-addressed directory under the data dir.
//! - **Integration.** [`PluginConnectorFactory`] implements
//!   [`nugget_connector::ConnectorFactory`], so plugin connectors run in the existing ingest
//!   pipeline; [`LoadedPlugin::tools`] and [`LoadedPlugin::call_tool`] serve the agent.
//!
//! The guest-side SDK lives in `sdk/` (its own cargo workspace, built for `wasm32-wasip2`).

mod connector;
mod host;
mod kv;
mod manifest;
mod package;
mod store;

pub use connector::{PluginConnector, PluginConnectorFactory, plugin_kind};
pub use ed25519_dalek::{SigningKey, VerifyingKey};
pub use host::{HostConfig, Limits, LoadedPlugin, PluginHost, ToolSpec, secret_account};
pub use manifest::{
    Capabilities, ConnectorAuth, ConnectorSection, HostRule, MANIFEST_FILE, Manifest, PluginKind,
};
pub use package::{
    MAX_WASM_BYTES, PluginPackage, SIGNATURE_FILE, VerifiedPackage, WASM_FILE, decode_signature,
    parse_signing_key, parse_verifying_key, sign_package, signing_payload,
};
pub use store::{ApprovalRequest, InstalledPlugin, PluginStore, VerifiedPlugin};

/// The WIT source of the `nugget:plugin` world.
pub const WIT: &str = include_str!("../wit/plugin.wit");

#[cfg(test)]
mod tests;
