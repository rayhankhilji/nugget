//! Plugins (M6): WebAssembly connectors and read-only agent tools from `nugget-plugin`, wired
//! into the daemon.
//!
//! - **Store and host.** One [`PluginStore`] under `<data>/plugins` and one [`PluginHost`] per
//!   data dir, kept in a process-wide map so the connectors registered before ingest starts
//!   ([`connector_factories`]) and the feature ([`start`]) share the same loaded plugins.
//! - **IPC.** `plugins.list`, `plugins.inspect`, `plugins.install` (verify and stage a local
//!   package for 10 minutes), `plugins.approve` (commit or discard it), `plugins.enable`,
//!   `plugins.remove`, `plugins.catalog` (packages available to install, see below). Every
//!   mutation is audited (`plugin.install`, `plugin.approve`, `plugin.reject`, `plugin.remove`,
//!   `plugin.enable`, `plugin.disable`).
//! - **Trust.** A package must be signed by a key in `[plugins] trusted_keys`. With
//!   `dev_mode = true`, an unsigned package (empty signature) is signed at install with the
//!   daemon's dev key: an ed25519 seed kept in the secret store under
//!   `plugins:dev-signing-key`, generated on first use. Dev-signed plugins are trusted only while
//!   `dev_mode` is on. Plugins whose signer is not trusted are listed with an error and not
//!   loaded.
//! - **Dev key before the keychain is reachable.** [`connector_factories`] runs before the daemon
//!   state (and its secret store) exists, so the dev *public* key is also cached in
//!   `<data>/plugins/dev-signing-key.pub`; it is only a cache: [`start`] reads the key from the
//!   secret store, rewrites (or deletes) the file when it differs, and reloads the plugins under
//!   the corrected trust. A public key is not secret, and anyone able to write the data dir can
//!   already edit `config.toml`.
//! - **Connectors.** Plugin connector kinds (`plugin:<id>`) are registered once, before ingest
//!   starts. Each sync runs the plugin as currently loaded, so an update, a disable or a removal
//!   takes effect on the next sync. **Known gap:** a connector plugin installed while the daemon
//!   runs becomes a source kind only after a daemon restart. Tools update live ([`agent_tools`]).
//! - **Catalog.** `plugins.catalog` lists the `*.nuggetplugin` files (non-recursive, at most
//!   [`MAX_CATALOG_ENTRIES`]) in the catalog directories: `[plugins] catalog_dirs` (absolute
//!   paths; default `<data>/plugin-catalog`) plus the entries of `NUGGET_PLUGIN_CATALOG` (a
//!   PATH-style list a package or launcher can set to ship plugins with the app). Each package is
//!   verified exactly as `plugins.install` would (signature against the trusted keys, manifest,
//!   update rules) but nothing is installed or staged; the UI installs one through the normal
//!   `plugins.install` / `plugins.approve` flow with the entry's path. A package that cannot be
//!   read or verified is listed with its error and never fails the listing.
//! - **Egress and secrets for tools** go to the daemon's ledger and secret store; until [`start`]
//!   binds them (no tool can run before), nothing is recorded and secrets are unavailable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nugget_connector::{
    ChangeSink, Connector, ConnectorContext, ConnectorFactory, KindInfo, SecretStore,
};
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ipc::{
    PluginApproveParams, PluginApproveResult, PluginCapabilities, PluginCatalogEntry,
    PluginCatalogResult, PluginEnableParams, PluginIdParams, PluginInfo, PluginInspectResult,
    PluginInstallParams, PluginInstallResult, PluginListResult, PluginRemoveResult,
    PluginSignature, plugin_method,
};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_ipc::{Notifier, RpcError, parse_params, to_result};
use nugget_plugin::{
    Capabilities, HostConfig, InstalledPlugin, Limits, LoadedPlugin, Manifest, PACKAGE_EXTENSION,
    PluginConnectorFactory, PluginHost, PluginKind, PluginPackage, PluginStore, SigningKey,
    VerifyingKey, parse_signing_key, parse_verifying_key, plugin_kind, secret_account,
};
use nugget_store::{AuditEntry, Store};
use serde::Deserialize;
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;
use tokio_util::sync::CancellationToken;

use super::{Feature, FeatureContext, section};
use crate::config::Config;
use crate::daemon::blocking;

/// Keychain account of the daemon's dev signing key (hex seed).
const DEV_KEY_ACCOUNT: &str = "plugins:dev-signing-key";
/// Cache of the dev public key in the store root (see the module docs).
const DEV_PUB_FILE: &str = "dev-signing-key.pub";
/// How long a staged install waits for `plugins.approve`.
const STAGE_TTL: Duration = Duration::from_secs(10 * 60);
/// Most staged installs at once; the oldest is dropped beyond this.
const MAX_STAGED: usize = 8;
/// Default catalog directory, under the data dir.
const CATALOG_DIR: &str = "plugin-catalog";
/// Extra catalog directories (a PATH-style list of absolute paths), e.g. set by a package.
const CATALOG_ENV: &str = "NUGGET_PLUGIN_CATALOG";
/// Most packages `plugins.catalog` reads (over all directories).
const MAX_CATALOG_ENTRIES: usize = 64;
/// Most directory entries `plugins.catalog` looks at per directory, packages or not.
const MAX_CATALOG_SCAN: usize = 4096;

const METHODS: &[&str] = &[
    plugin_method::LIST,
    plugin_method::INSPECT,
    plugin_method::INSTALL,
    plugin_method::APPROVE,
    plugin_method::REMOVE,
    plugin_method::ENABLE,
    plugin_method::CATALOG,
];

// ---- config ----

/// `[plugins]` as written in `config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct PluginsSection {
    enabled: bool,
    trusted_keys: Vec<String>,
    dev_mode: bool,
    catalog_dirs: Vec<PathBuf>,
}

impl Default for PluginsSection {
    fn default() -> Self {
        Self {
            enabled: true,
            trusted_keys: Vec::new(),
            dev_mode: false,
            catalog_dirs: Vec::new(),
        }
    }
}

/// `[plugins]`, validated.
#[derive(Debug, Clone)]
struct Settings {
    enabled: bool,
    trusted: Vec<VerifyingKey>,
    dev_mode: bool,
    /// Configured catalog directories (empty: the default one).
    catalog_dirs: Vec<PathBuf>,
}

fn settings(config: &Config) -> Result<Settings> {
    let raw: PluginsSection = section("plugins", &config.plugins)?;
    let trusted = raw
        .trusted_keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            parse_verifying_key(k)
                .map_err(|e| Error::Invalid(format!("[plugins] trusted_keys[{i}]: {e}")))
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(dir) = raw.catalog_dirs.iter().find(|d| !d.is_absolute()) {
        return Err(Error::Invalid(format!(
            "[plugins] catalog_dirs: not an absolute path: {}",
            dir.display()
        )));
    }
    Ok(Settings {
        enabled: raw.enabled,
        trusted,
        dev_mode: raw.dev_mode,
        catalog_dirs: raw.catalog_dirs,
    })
}

/// The catalog directories: the configured ones (or `<data>/plugin-catalog`), then the absolute
/// entries of `env` (the value of [`CATALOG_ENV`]), without duplicates.
fn catalog_dirs(
    configured: &[PathBuf],
    data: &Path,
    env: Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    let mut dirs = if configured.is_empty() {
        vec![data.join(CATALOG_DIR)]
    } else {
        configured.to_vec()
    };
    if let Some(env) = env {
        for dir in std::env::split_paths(&env) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            if !dir.is_absolute() {
                tracing::warn!(dir = %dir.display(), "{CATALOG_ENV}: ignoring a relative path");
                continue;
            }
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

// ---- late-bound egress and secrets ----

/// The daemon's egress ledger, bound by [`start`].
#[derive(Default)]
struct LateEgress(OnceLock<Arc<dyn EgressSink>>);

impl EgressSink for LateEgress {
    fn record(&self, event: EgressEvent) {
        match self.0.get() {
            Some(sink) => sink.record(event),
            None => tracing::warn!(host = %event.host, "plugin egress before the daemon was ready"),
        }
    }
}

/// The daemon's secret store, bound by [`start`].
#[derive(Default)]
struct LateSecrets(OnceLock<Arc<dyn SecretStore>>);

impl LateSecrets {
    fn inner(&self) -> Result<&Arc<dyn SecretStore>> {
        self.0
            .get()
            .ok_or_else(|| Error::Other("the keychain is not ready yet".into()))
    }
}

impl SecretStore for LateSecrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        self.inner()?.get(account)
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.inner()?.set(account, value)
    }
    fn delete(&self, account: &str) -> Result<()> {
        self.inner()?.delete(account)
    }
}

// ---- runtime ----

/// A plugin's place in the host.
#[derive(Clone)]
enum Slot {
    Loaded {
        plugin: Arc<LoadedPlugin>,
        /// Agent tools, once listed (`None` until [`Runtime::refresh_tools`]).
        tools: Option<Vec<Arc<dyn nugget_agent::Tool>>>,
        tools_error: Option<String>,
    },
    /// Enabled but not loaded, and why.
    Failed(String),
}

/// Store, host and loaded plugins of one data dir.
struct Runtime {
    store: PluginStore,
    host: PluginHost,
    settings: Settings,
    egress: Arc<LateEgress>,
    secrets: Arc<LateSecrets>,
    /// Public half of the dev key, when `dev_mode` is on and one exists.
    dev_key: Mutex<Option<VerifyingKey>>,
    /// Enabled plugins by id (disabled ones have no slot).
    slots: Mutex<HashMap<String, Slot>>,
    /// Where `plugins.catalog` looks for packages.
    catalog_dirs: Vec<PathBuf>,
    /// Verified catalog packages, so a page load does not re-read and re-hash every file.
    catalog_cache: Mutex<HashMap<PathBuf, CachedPackage>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runtimes by data dir.
fn runtimes() -> MutexGuard<'static, HashMap<PathBuf, Arc<Runtime>>> {
    static RUNTIMES: OnceLock<Mutex<HashMap<PathBuf, Arc<Runtime>>>> = OnceLock::new();
    lock(RUNTIMES.get_or_init(Default::default))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

impl Runtime {
    /// Open the store and the host (no plugin loaded yet). Synchronous.
    fn open(paths: &Paths, settings: Settings) -> Result<Self> {
        let store = PluginStore::open(paths)?;
        let egress = Arc::new(LateEgress::default());
        let secrets = Arc::new(LateSecrets::default());
        let host = PluginHost::new(HostConfig {
            limits: Limits::default(),
            state_dir: store.state_root(),
            egress: egress.clone(),
            secrets: Some(secrets.clone()),
            http_client: None,
            allow_private_network: false,
        })?;
        let catalog_dirs = catalog_dirs(
            &settings.catalog_dirs,
            &paths.data,
            std::env::var_os(CATALOG_ENV),
        );
        let dev_key = if settings.dev_mode {
            std::fs::read_to_string(store.root().join(DEV_PUB_FILE))
                .ok()
                .and_then(|s| parse_verifying_key(&s).ok())
        } else {
            None
        };
        Ok(Self {
            store,
            host,
            settings,
            egress,
            secrets,
            dev_key: Mutex::new(dev_key),
            slots: Mutex::new(HashMap::new()),
            catalog_dirs,
            catalog_cache: Mutex::new(HashMap::new()),
        })
    }

    fn dev_key(&self) -> Option<VerifyingKey> {
        *lock(&self.dev_key)
    }

    /// Record the dev public key (and its cache file). Returns whether it changed.
    fn set_dev_key(&self, key: Option<VerifyingKey>) -> bool {
        let mut cur = lock(&self.dev_key);
        if *cur == key {
            return false;
        }
        *cur = key;
        let path = self.store.root().join(DEV_PUB_FILE);
        let written = match key {
            Some(k) => std::fs::write(&path, hex(k.as_bytes())),
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        };
        if let Err(e) = written {
            tracing::warn!(path = %path.display(), error = %e, "cannot update the plugin dev key cache");
        }
        true
    }

    /// Keys a package may be signed with right now.
    fn trusted_keys(&self) -> Vec<VerifyingKey> {
        let mut keys = self.settings.trusted.clone();
        if self.settings.dev_mode
            && let Some(dev) = self.dev_key()
            && !keys.contains(&dev)
        {
            keys.push(dev);
        }
        keys
    }

    fn signature(&self, signer_hex: &str) -> PluginSignature {
        let is = |k: &VerifyingKey| hex(k.as_bytes()).eq_ignore_ascii_case(signer_hex);
        if self.settings.trusted.iter().any(is) {
            PluginSignature::Trusted
        } else if self.settings.dev_mode && self.dev_key().as_ref().is_some_and(is) {
            PluginSignature::Dev
        } else {
            PluginSignature::Untrusted
        }
    }

    /// (Re)load one installed plugin according to its enabled flag and its signer. Compiles the
    /// component: call from a blocking thread.
    fn load(&self, inst: &InstalledPlugin) {
        let id = inst.manifest.id.clone();
        let slot = if !inst.enabled {
            None
        } else if self.signature(&inst.signer) == PluginSignature::Untrusted {
            let mut why = format!(
                "signed by {}, which is not in [plugins] trusted_keys",
                inst.signer
            );
            if !self.settings.dev_mode {
                why.push_str(" (dev-signed plugins load only with dev_mode = true)");
            }
            Some(Slot::Failed(why))
        } else {
            // Re-checks the signature against the keys trusted now and the approval seal.
            match self
                .store
                .load_trusted(&id, &self.trusted_keys())
                .and_then(|v| self.host.load(&v))
            {
                Ok(plugin) => Some(Slot::Loaded {
                    plugin,
                    tools: None,
                    tools_error: None,
                }),
                Err(e) => {
                    tracing::warn!(plugin = %id, error = %e, "cannot load plugin");
                    Some(Slot::Failed(format!("cannot load: {e}")))
                }
            }
        };
        let mut slots = lock(&self.slots);
        match slot {
            Some(s) => {
                slots.insert(id, s);
            }
            None => {
                slots.remove(&id);
            }
        }
    }

    /// Load every installed plugin. Never fails: problems are logged and kept per plugin.
    fn load_all(&self) {
        match self.store.list() {
            Ok(list) => {
                let ids: Vec<&str> = list.iter().map(|p| p.manifest.id.as_str()).collect();
                lock(&self.slots).retain(|id, _| ids.contains(&id.as_str()));
                for inst in &list {
                    self.load(inst);
                }
            }
            Err(e) => tracing::error!(error = %e, "cannot list installed plugins"),
        }
    }

    fn unload(&self, id: &str) {
        lock(&self.slots).remove(id);
    }

    fn loaded(&self, id: &str) -> Option<Arc<LoadedPlugin>> {
        match lock(&self.slots).get(id) {
            Some(Slot::Loaded { plugin, .. }) => Some(plugin.clone()),
            _ => None,
        }
    }

    /// List the agent tools of loaded plugins that have not been asked yet.
    async fn refresh_tools(&self) {
        let pending: Vec<Arc<LoadedPlugin>> = lock(&self.slots)
            .values()
            .filter_map(|s| match s {
                Slot::Loaded {
                    plugin,
                    tools: None,
                    ..
                } => Some(plugin.clone()),
                _ => None,
            })
            .collect();
        for plugin in pending {
            let (tools, error) = match nugget_plugin::agent_tools(plugin.clone()).await {
                Ok(t) => (t, None),
                Err(e) => {
                    tracing::warn!(plugin = %plugin.id(), error = %e, "cannot list plugin tools");
                    (Vec::new(), Some(format!("cannot list tools: {e}")))
                }
            };
            if let Some(Slot::Loaded {
                plugin: current,
                tools: slot_tools,
                tools_error,
            }) = lock(&self.slots).get_mut(plugin.id())
                && Arc::ptr_eq(current, &plugin)
            {
                *slot_tools = Some(tools);
                *tools_error = error;
            }
        }
    }

    fn tools(&self) -> Vec<Arc<dyn nugget_agent::Tool>> {
        let slots = lock(&self.slots);
        let mut ids: Vec<&String> = slots.keys().collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| match &slots[id] {
                Slot::Loaded { tools: Some(t), .. } => Some(t.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn info(&self, inst: &InstalledPlugin) -> PluginInfo {
        let m = &inst.manifest;
        let (loaded, error, tools) = match lock(&self.slots).get(&m.id) {
            Some(Slot::Loaded {
                tools, tools_error, ..
            }) => (
                true,
                tools_error.clone(),
                tools
                    .iter()
                    .flatten()
                    .map(|t| t.name().to_string())
                    .collect(),
            ),
            Some(Slot::Failed(why)) => (false, Some(why.clone()), Vec::new()),
            None => (false, None, Vec::new()),
        };
        PluginInfo {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.to_string(),
            description: m.description.clone(),
            kinds: kinds(m),
            enabled: inst.enabled,
            loaded,
            error,
            signature: self.signature(&inst.signer),
            signer: inst.signer.clone(),
            approved: caps(&inst.approved),
            installed_at: inst.installed_at.format(&Rfc3339).unwrap_or_default(),
            connector_kind: m
                .provides(PluginKind::Connector)
                .then(|| plugin_kind(&m.id)),
            tools,
        }
    }
}

fn kinds(m: &Manifest) -> Vec<String> {
    m.kind
        .iter()
        .map(|k| match k {
            PluginKind::Connector => "connector".to_string(),
            PluginKind::Tool => "tool".to_string(),
        })
        .collect()
}

fn caps(c: &Capabilities) -> PluginCapabilities {
    PluginCapabilities {
        hosts: c.hosts.clone(),
        secrets: c.secrets.clone(),
        kv_max_bytes: c.kv_max_bytes,
        file_roots: c.file_roots.clone(),
    }
}

// ---- catalog ----

/// What a catalog scan found.
enum Found {
    Package(PathBuf),
    /// A catalog directory that exists but cannot be read, and why.
    Unreadable(PathBuf, String),
}

/// The package files in the catalog directories, at most [`MAX_CATALOG_ENTRIES`] in all (the
/// flag says whether there were more), sorted by name within each directory. Each directory is
/// read only until the cap (or [`MAX_CATALOG_SCAN`] entries) is reached, so beyond the cap which
/// packages are listed depends on the directory order. A directory that does not exist is
/// skipped; one that cannot be read is reported.
fn scan_catalog(dirs: &[PathBuf]) -> (Vec<Found>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    for dir in dirs {
        if out.len() >= MAX_CATALOG_ENTRIES {
            truncated = true;
            break;
        }
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                out.push(Found::Unreadable(
                    dir.clone(),
                    format!("cannot read the folder: {e}"),
                ));
                continue;
            }
        };
        let mut files = Vec::new();
        for (seen, entry) in read.enumerate() {
            if seen >= MAX_CATALOG_SCAN {
                truncated = true;
                break;
            }
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(PACKAGE_EXTENSION)
                // Follows symlinks: a packager may link packages in. Directories are not listed.
                || !std::fs::metadata(&path).is_ok_and(|m| m.is_file())
            {
                continue;
            }
            if out.len() + files.len() >= MAX_CATALOG_ENTRIES {
                truncated = true;
                break;
            }
            files.push(path);
        }
        files.sort();
        out.extend(files.into_iter().map(Found::Package));
    }
    (out, truncated)
}

/// File identity for the catalog cache: a changed file is read again.
#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: std::time::SystemTime,
}

/// The stamp of a file that may be cached: `None` when it cannot be read, or when it changed so
/// recently that a further same-size write could keep the same time on a filesystem with coarse
/// timestamps (as git does for its index).
fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let age = std::time::SystemTime::now().duration_since(modified).ok()?;
    (age >= Duration::from_secs(3)).then_some(FileStamp {
        len: meta.len(),
        modified,
    })
}

/// A catalog package as read and verified, before it is compared with what is installed.
struct CachedPackage {
    stamp: FileStamp,
    /// The dev key when it was read (an unsigned package is installed under it).
    dev_key: Option<VerifyingKey>,
    verified: std::result::Result<VerifiedCatalogPackage, String>,
}

#[derive(Clone)]
struct VerifiedCatalogPackage {
    manifest: Manifest,
    /// Hex key the package is (or, unsigned in dev mode, will be) signed with; `None` for an
    /// unsigned package before a dev key exists.
    signer: Option<String>,
    unsigned: bool,
}

impl Runtime {
    /// Describe one catalog package as `plugins.install` would see it, without installing it.
    /// Reads and verifies the file unless `cache` holds it unchanged: call from a blocking thread.
    fn catalog_entry(
        &self,
        path: &Path,
        cache: &mut HashMap<PathBuf, CachedPackage>,
    ) -> PluginCatalogEntry {
        let mut entry = PluginCatalogEntry {
            path: path.display().to_string(),
            file_name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            ..Default::default()
        };
        let dev_key = self.dev_key();
        let stamp = file_stamp(path);
        let cached = cache
            .get(path)
            .filter(|c| Some(&c.stamp) == stamp.as_ref() && c.dev_key == dev_key)
            .map(|c| c.verified.clone());
        let verified = match cached {
            Some(v) => v,
            None => {
                let v = self.verify_catalog_package(path).map_err(|e| e.to_string());
                match stamp {
                    Some(stamp) => {
                        cache.insert(
                            path.to_path_buf(),
                            CachedPackage {
                                stamp,
                                dev_key,
                                verified: v.clone(),
                            },
                        );
                    }
                    None => {
                        cache.remove(path);
                    }
                }
                v
            }
        };
        match verified {
            Ok(v) => {
                if let Err(e) = self.describe(&v, &mut entry) {
                    entry.error = Some(e.to_string());
                }
            }
            Err(e) => entry.error = Some(e),
        }
        entry
    }

    /// Read and verify a catalog package as `plugins.install` does (the signature before the
    /// manifest; unsigned packages only in dev mode).
    fn verify_catalog_package(&self, path: &Path) -> Result<VerifiedCatalogPackage> {
        let package = read_package(path)?;
        if package.signature.is_empty() {
            if !self.settings.dev_mode {
                return Err(Error::Unauthorized(
                    "the plugin package is unsigned; unsigned plugins install only with \
                     [plugins] dev_mode = true"
                        .into(),
                ));
            }
            // `plugins.install` signs it with the dev key (creating one if there is none).
            return Ok(VerifiedCatalogPackage {
                manifest: Manifest::parse(&package.manifest)?,
                signer: self.dev_key().map(|k| hex(k.as_bytes())),
                unsigned: true,
            });
        }
        let trusted = self.trusted_keys();
        if trusted.is_empty() {
            return Err(Error::Unauthorized(
                "no plugin signing key is trusted yet: add the publisher's public key to \
                 [plugins] trusted_keys"
                    .into(),
            ));
        }
        let verified = package.verify(&trusted).map_err(|e| {
            Error::Unauthorized(format!(
                "{e}: the publisher's key is not in [plugins] trusted_keys, or the file was \
                 changed after signing"
            ))
        })?;
        Ok(VerifiedCatalogPackage {
            manifest: verified.manifest,
            signer: Some(hex(verified.signer.as_bytes())),
            unsigned: false,
        })
    }

    /// Fill `entry` from a verified package and compare it with the installed version, with the
    /// rules of `plugins.install` (`PluginStore::preview`): a higher version signed by the same
    /// key, approved again only for access beyond what was approved. (`plugins.install` itself
    /// runs the full preview, including the approval seal.)
    fn describe(&self, v: &VerifiedCatalogPackage, entry: &mut PluginCatalogEntry) -> Result<()> {
        let m = &v.manifest;
        entry.signer = v.signer.clone().unwrap_or_default();
        entry.signature = if v.unsigned {
            PluginSignature::Dev
        } else {
            self.signature(&entry.signer)
        };
        entry.id = m.id.clone();
        entry.name = m.name.clone();
        entry.version = m.version.to_string();
        entry.description = m.description.clone();
        entry.kinds = kinds(m);
        entry.requested = caps(&m.capabilities);
        entry.connector_display_name = m.connector.as_ref().map(|c| c.display_name.clone());

        let installed = self.store.get(&m.id)?;
        entry.installed_version = installed.as_ref().map(|i| i.manifest.version.to_string());
        match installed {
            None => entry.needs_approval = true,
            Some(old) if old.manifest.version >= m.version => {}
            Some(old) => {
                let same_signer = v
                    .signer
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&old.signer));
                if !same_signer {
                    return Err(Error::Unauthorized(format!(
                        "this version is signed by a different key than the installed version \
                         ({}); remove the installed plugin first to switch publishers",
                        old.signer
                    )));
                }
                entry.update_available = true;
                entry.needs_approval = !m.capabilities.is_subset_of(&old.approved);
            }
        }
        Ok(())
    }

    /// `plugins.catalog`. Holds the cache lock throughout, so concurrent calls run one at a time.
    fn catalog(&self) -> PluginCatalogResult {
        let mut cache = lock(&self.catalog_cache);
        let (found, truncated) = scan_catalog(&self.catalog_dirs);
        cache.retain(|p, _| {
            found
                .iter()
                .any(|f| matches!(f, Found::Package(q) if q == p))
        });
        let entries = found
            .into_iter()
            .map(|f| match f {
                Found::Package(path) => self.catalog_entry(&path, &mut cache),
                Found::Unreadable(dir, error) => PluginCatalogEntry {
                    path: dir.display().to_string(),
                    error: Some(error),
                    ..Default::default()
                },
            })
            .collect();
        PluginCatalogResult {
            entries,
            dirs: self
                .catalog_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect(),
            truncated,
        }
    }
}

// ---- connectors ----

/// Plugin connector factories for the source registry, called once before ingest starts.
/// Loads every installed, enabled, trusted plugin. Never fails: a bad `[plugins]` section or a
/// plugin that does not load is logged and skipped.
pub(crate) fn connector_factories(
    paths: &Paths,
    config: &Config,
) -> Vec<Arc<dyn ConnectorFactory>> {
    runtimes().remove(&paths.data);
    let settings = match settings(config) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "plugins: no plugin connectors");
            return Vec::new();
        }
    };
    if !settings.enabled {
        return Vec::new();
    }
    let rt = match Runtime::open(paths, settings) {
        Ok(rt) => Arc::new(rt),
        Err(e) => {
            tracing::error!(error = %e, "plugins: cannot open the plugin store");
            return Vec::new();
        }
    };
    rt.load_all();
    runtimes().insert(paths.data.clone(), rt.clone());
    let loaded: Vec<Arc<LoadedPlugin>> = {
        let slots = lock(&rt.slots);
        let mut ids: Vec<&String> = slots.keys().collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| match &slots[id] {
                Slot::Loaded { plugin, .. } => Some(plugin.clone()),
                Slot::Failed(_) => None,
            })
            .collect()
    };
    loaded
        .into_iter()
        .filter(|p| p.manifest().provides(PluginKind::Connector))
        .filter_map(|p| match PluginConnectorFactory::new(p.clone()) {
            Ok(f) => Some(Arc::new(PluginKindFactory {
                id: p.id().to_string(),
                info: f.info(),
                rt: rt.clone(),
            }) as Arc<dyn ConnectorFactory>),
            Err(e) => {
                tracing::warn!(plugin = %p.id(), error = %e, "plugin connector skipped");
                None
            }
        })
        .collect()
}

/// A plugin source kind that always runs the plugin as currently loaded.
struct PluginKindFactory {
    id: String,
    info: KindInfo,
    rt: Arc<Runtime>,
}

fn current_factory(rt: &Runtime, id: &str) -> Result<PluginConnectorFactory> {
    let plugin = rt.loaded(id).ok_or_else(|| {
        Error::Unsupported(format!(
            "plugin `{id}` is disabled, removed or not loaded (see Plugins)"
        ))
    })?;
    PluginConnectorFactory::new(plugin)
}

impl ConnectorFactory for PluginKindFactory {
    fn info(&self) -> KindInfo {
        self.info.clone()
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        current_factory(&self.rt, &self.id)?.validate(settings)
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        if !settings.is_object() {
            return Err(Error::Invalid("settings must be a JSON object".into()));
        }
        let poll_interval = match current_factory(&self.rt, &self.id) {
            Ok(f) => f.build(settings, ctx.clone())?.poll_interval(),
            Err(_) => Duration::from_secs(1800),
        };
        Ok(Arc::new(PluginSource {
            id: self.id.clone(),
            kind: self.info.kind,
            settings: settings.clone(),
            ctx,
            poll_interval,
            rt: self.rt.clone(),
        }))
    }
}

struct PluginSource {
    id: String,
    kind: &'static str,
    settings: Value,
    ctx: ConnectorContext,
    poll_interval: Duration,
    rt: Arc<Runtime>,
}

#[async_trait]
impl Connector for PluginSource {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let connector =
            current_factory(&self.rt, &self.id)?.build(&self.settings, self.ctx.clone())?;
        connector.sync(cursor, sink, cancel).await
    }

    fn poll_interval(&self) -> Duration {
        self.poll_interval
    }
}

// ---- agent tools ----

/// Agent tools of the loaded, enabled plugins of this data dir (empty when the feature is off).
// Registered by the agent feature (`features::agent::register`) at startup.
pub(crate) async fn agent_tools(paths: &Paths) -> Vec<Arc<dyn nugget_agent::Tool>> {
    let rt = runtimes().get(&paths.data).cloned();
    match rt {
        Some(rt) => {
            rt.refresh_tools().await;
            rt.tools()
        }
        None => Vec::new(),
    }
}

// ---- feature ----

/// A verified package waiting for `plugins.approve`.
struct Staged {
    package: PluginPackage,
    id: String,
    expires: Instant,
}

struct Plugins {
    rt: Arc<Runtime>,
    /// For audit entries.
    store: Store,
    secrets: Arc<dyn SecretStore>,
    staged: Mutex<HashMap<String, Staged>>,
    /// Serializes mutations, so the host follows the store.
    ops: tokio::sync::Mutex<()>,
}

/// Start the feature, reusing the runtime [`connector_factories`] opened.
pub(crate) async fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    let settings = settings(cx.config)?;
    if !settings.enabled {
        return Err(Error::Unsupported(
            "disabled by [plugins] enabled = false".into(),
        ));
    }
    let state = cx.state;
    let rt = runtime_for(&state.paths, settings).await?;
    let f = Plugins::open(
        rt,
        state.store.clone(),
        state.secrets.clone(),
        Arc::new(state.store.clone()),
    )
    .await;
    Ok(Arc::new(f))
}

/// The runtime of `paths`, or a new one with every plugin loaded.
async fn runtime_for(paths: &Paths, settings: Settings) -> Result<Arc<Runtime>> {
    if let Some(rt) = runtimes().get(&paths.data).cloned() {
        return Ok(rt);
    }
    let p = paths.clone();
    let rt = blocking(move || {
        let rt = Arc::new(Runtime::open(&p, settings)?);
        rt.load_all();
        Ok(rt)
    })
    .await?;
    runtimes().insert(paths.data.clone(), rt.clone());
    Ok(rt)
}

impl Plugins {
    async fn open(
        rt: Arc<Runtime>,
        store: Store,
        secrets: Arc<dyn SecretStore>,
        egress: Arc<dyn EgressSink>,
    ) -> Self {
        let _ = rt.egress.0.set(egress);
        let _ = rt.secrets.0.set(secrets.clone());
        if rt.settings.dev_mode {
            let s = secrets.clone();
            let key = blocking(move || match s.get(DEV_KEY_ACCOUNT)? {
                Some(seed) => Ok(Some(parse_signing_key(&seed)?.verifying_key())),
                None => Ok(None),
            })
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cannot read the plugin dev key");
                None
            });
            if rt.set_dev_key(key) {
                let r = rt.clone();
                if let Err(e) = blocking(move || {
                    r.load_all();
                    Ok(())
                })
                .await
                {
                    tracing::warn!(error = %e, "cannot reload plugins");
                }
            }
        }
        rt.refresh_tools().await;
        Self {
            rt,
            store,
            secrets,
            staged: Mutex::new(HashMap::new()),
            ops: tokio::sync::Mutex::new(()),
        }
    }

    async fn audit(&self, action: &str, target: &str, detail: Value) {
        let store = self.store.clone();
        let entry = AuditEntry {
            actor: "user".into(),
            action: action.into(),
            target: target.into(),
            detail,
        };
        if let Err(e) = blocking(move || store.append_audit(&entry).map(|_| ())).await {
            tracing::warn!(action, error = %e, "cannot append audit entry");
        }
    }

    async fn installed(&self, id: &str) -> Result<Option<InstalledPlugin>> {
        let rt = self.rt.clone();
        let id = id.to_string();
        blocking(move || rt.store.get(&id)).await
    }

    async fn info_of(&self, id: &str) -> Result<PluginInfo> {
        let inst = self
            .installed(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("plugin `{id}` is not installed")))?;
        Ok(self.rt.info(&inst))
    }

    /// (Re)load `inst` into the host and list its tools.
    async fn reload(&self, inst: InstalledPlugin) -> Result<()> {
        let rt = self.rt.clone();
        blocking(move || {
            rt.load(&inst);
            Ok(())
        })
        .await?;
        self.rt.refresh_tools().await;
        Ok(())
    }

    async fn list(&self) -> Result<PluginListResult> {
        let rt = self.rt.clone();
        let list = blocking(move || rt.store.list()).await?;
        Ok(PluginListResult {
            plugins: list.iter().map(|p| self.rt.info(p)).collect(),
        })
    }

    async fn catalog(&self) -> Result<PluginCatalogResult> {
        let rt = self.rt.clone();
        blocking(move || Ok(rt.catalog())).await
    }

    async fn inspect(&self, p: PluginIdParams) -> Result<PluginInspectResult> {
        let inst = self
            .installed(&p.id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("plugin `{}` is not installed", p.id)))?;
        Ok(PluginInspectResult {
            plugin: self.rt.info(&inst),
            requested: caps(&inst.manifest.capabilities),
            connector_display_name: inst
                .manifest
                .connector
                .as_ref()
                .map(|c| c.display_name.clone()),
        })
    }

    /// The dev signing key, created (and stored in the keychain) on first use.
    async fn dev_signing_key(&self) -> Result<SigningKey> {
        let secrets = self.secrets.clone();
        let key = blocking(move || {
            if let Some(seed) = secrets.get(DEV_KEY_ACCOUNT)? {
                return parse_signing_key(&seed);
            }
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed)
                .map_err(|e| Error::Other(format!("no OS randomness: {e}")))?;
            let key = SigningKey::from_bytes(&seed);
            let stored = secrets.set(DEV_KEY_ACCOUNT, &hex(&seed));
            seed.fill(0);
            stored?;
            Ok(key)
        })
        .await?;
        let rt = self.rt.clone();
        let public = key.verifying_key();
        blocking(move || {
            rt.set_dev_key(Some(public));
            Ok(())
        })
        .await?;
        Ok(key)
    }

    fn take_staged(&self, token: &str) -> Option<Staged> {
        let mut staged = lock(&self.staged);
        let now = Instant::now();
        staged.retain(|_, s| s.expires > now);
        staged.remove(token)
    }

    fn stage(&self, token: String, s: Staged) {
        let mut staged = lock(&self.staged);
        let now = Instant::now();
        staged.retain(|_, s| s.expires > now);
        while staged.len() >= MAX_STAGED {
            let oldest = staged
                .iter()
                .min_by_key(|(_, s)| s.expires)
                .map(|(t, _)| t.clone());
            match oldest {
                Some(t) => {
                    staged.remove(&t);
                }
                None => break,
            }
        }
        staged.insert(token, s);
    }

    async fn install(&self, p: PluginInstallParams) -> Result<PluginInstallResult> {
        let _ops = self.ops.lock().await;
        let path = PathBuf::from(&p.path);
        if !path.is_absolute() {
            return Err(Error::Invalid(format!(
                "plugin package path must be absolute: {}",
                p.path
            )));
        }
        let mut package = blocking(move || read_package(&path)).await?;
        if package.signature.is_empty() {
            if !self.rt.settings.dev_mode {
                return Err(Error::Unauthorized(
                    "the plugin package is unsigned; unsigned plugins install only with \
                     [plugins] dev_mode = true"
                        .into(),
                ));
            }
            let key = self.dev_signing_key().await?;
            package = PluginPackage::sign(package.manifest, package.wasm, &key);
        }
        let preview = {
            let (rt, pkg) = (self.rt.clone(), package.clone());
            blocking(move || rt.store.preview(&pkg, &rt.trusted_keys())).await?
        };
        let m = &preview.manifest;
        let signer = hex(preview.signer.as_bytes());
        let mut result = PluginInstallResult {
            token: String::new(),
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.to_string(),
            description: m.description.clone(),
            kinds: kinds(m),
            signature: self.rt.signature(&signer),
            signer: signer.clone(),
            requested: caps(&m.capabilities),
            previously_approved: preview.previously_approved.as_ref().map(caps),
            installed_version: preview.installed_version.as_ref().map(|v| v.to_string()),
            needs_approval: preview.needs_approval,
            installed: false,
        };
        if !preview.needs_approval && preview.installed_version.is_some() {
            let inst = self.commit(package).await?;
            result.installed = true;
            self.audit(
                "plugin.install",
                &inst.manifest.id,
                json!({
                    "version": inst.manifest.version.to_string(),
                    "signer": inst.signer,
                    "approved": caps(&inst.approved),
                    "from_version": result.installed_version,
                    "committed": true,
                }),
            )
            .await;
            return Ok(result);
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.stage(
            token.clone(),
            Staged {
                package,
                id: m.id.clone(),
                expires: Instant::now() + STAGE_TTL,
            },
        );
        self.audit(
            "plugin.install",
            &m.id,
            json!({
                "version": result.version,
                "signer": signer,
                "requested": result.requested,
                "from_version": result.installed_version,
                "committed": false,
            }),
        )
        .await;
        result.token = token;
        Ok(result)
    }

    /// Install or update a verified package (its capabilities were approved) and load it.
    async fn commit(&self, package: PluginPackage) -> Result<InstalledPlugin> {
        let (rt, secrets) = (self.rt.clone(), self.secrets.clone());
        let inst = blocking(move || {
            let trusted = rt.trusted_keys();
            let id = package.verify(&trusted)?.manifest.id;
            let Some(old) = rt.store.get(&id)? else {
                return rt.store.install(&package, &trusted, &mut |_| true);
            };
            let new = rt.store.update(&package, &trusted, &mut |_| true)?;
            // Secrets the new version no longer declares leave the keychain.
            for name in &old.approved.secrets {
                if !new.approved.secrets.contains(name)
                    && let Err(e) = secrets.delete(&secret_account(&id, name))
                {
                    tracing::warn!(plugin = %id, secret = %name, error = %e, "cannot delete plugin secret");
                }
            }
            Ok(new)
        })
        .await?;
        self.reload(inst.clone()).await?;
        Ok(inst)
    }

    async fn approve(&self, p: PluginApproveParams) -> Result<PluginApproveResult> {
        let _ops = self.ops.lock().await;
        let staged = self.take_staged(&p.token).ok_or_else(|| {
            Error::NotFound("unknown or expired plugin install token; install again".into())
        })?;
        if !p.approve {
            self.audit("plugin.reject", &staged.id, json!({})).await;
            return Ok(PluginApproveResult { plugin: None });
        }
        let inst = self.commit(staged.package).await?;
        self.audit(
            "plugin.approve",
            &inst.manifest.id,
            json!({
                "version": inst.manifest.version.to_string(),
                "signer": inst.signer,
                "approved": caps(&inst.approved),
            }),
        )
        .await;
        Ok(PluginApproveResult {
            plugin: Some(self.rt.info(&inst)),
        })
    }

    async fn remove(&self, p: PluginIdParams) -> Result<PluginRemoveResult> {
        let _ops = self.ops.lock().await;
        let Some(inst) = self.installed(&p.id).await? else {
            return Ok(PluginRemoveResult { removed: false });
        };
        self.rt.unload(&p.id);
        let (rt, secrets, id) = (self.rt.clone(), self.secrets.clone(), p.id.clone());
        let mut names = inst.manifest.capabilities.secrets.clone();
        names.extend(inst.approved.secrets.iter().cloned());
        names.sort();
        names.dedup();
        let removed = blocking(move || {
            let removed = rt.store.remove(&id)?;
            for name in &names {
                if let Err(e) = secrets.delete(&secret_account(&id, name)) {
                    tracing::warn!(plugin = %id, secret = %name, error = %e, "cannot delete plugin secret");
                }
            }
            Ok(removed)
        })
        .await?;
        if removed {
            self.audit(
                "plugin.remove",
                &p.id,
                json!({
                    "version": inst.manifest.version.to_string(),
                    "signer": inst.signer,
                    "approved": caps(&inst.approved),
                }),
            )
            .await;
        }
        Ok(PluginRemoveResult { removed })
    }

    async fn enable(&self, p: PluginEnableParams) -> Result<PluginInfo> {
        let _ops = self.ops.lock().await;
        let (rt, id, enabled) = (self.rt.clone(), p.id.clone(), p.enabled);
        let inst = blocking(move || {
            if !rt.store.set_enabled(&id, enabled)? {
                return Err(Error::NotFound(format!("plugin `{id}` is not installed")));
            }
            rt.store
                .get(&id)?
                .ok_or_else(|| Error::NotFound(format!("plugin `{id}` is not installed")))
        })
        .await?;
        self.reload(inst.clone()).await?;
        self.audit(
            if p.enabled {
                "plugin.enable"
            } else {
                "plugin.disable"
            },
            &p.id,
            json!({
                "version": inst.manifest.version.to_string(),
                "signer": inst.signer,
                "approved": caps(&inst.approved),
            }),
        )
        .await;
        self.info_of(&p.id).await
    }
}

/// Read a package from a `.nuggetplugin` file or a package directory.
fn read_package(path: &Path) -> Result<PluginPackage> {
    let meta = std::fs::metadata(path).map_err(|e| {
        Error::Invalid(format!(
            "cannot read plugin package {}: {e}",
            path.display()
        ))
    })?;
    if !meta.is_dir() && path.extension().and_then(|e| e.to_str()) != Some(PACKAGE_EXTENSION) {
        return Err(Error::Invalid(format!(
            "not a plugin package (expected a .{PACKAGE_EXTENSION} file or a package directory): {}",
            path.display()
        )));
    }
    PluginPackage::from_path(path)
}

#[async_trait]
impl Feature for Plugins {
    fn name(&self) -> &'static str {
        "plugins"
    }

    fn methods(&self) -> &'static [&'static str] {
        METHODS
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        _notify: Notifier,
    ) -> Result<Value, RpcError> {
        match method {
            plugin_method::LIST => to_result(self.list().await?),
            plugin_method::INSPECT => to_result(self.inspect(parse_params(params)?).await?),
            plugin_method::INSTALL => to_result(self.install(parse_params(params)?).await?),
            plugin_method::APPROVE => to_result(self.approve(parse_params(params)?).await?),
            plugin_method::REMOVE => to_result(self.remove(parse_params(params)?).await?),
            plugin_method::ENABLE => to_result(self.enable(parse_params(params)?).await?),
            plugin_method::CATALOG => to_result(self.catalog().await?),
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_ingest::MemorySecrets;
    use nugget_plugin::PluginPackage;

    /// The smallest valid component (no imports, no exports): installs, but does not load.
    const EMPTY_COMPONENT: &[u8] = b"\0asm\x0d\x00\x01\x00";

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn pubhex(seed: u8) -> String {
        hex(key(seed).verifying_key().as_bytes())
    }

    fn manifest(id: &str, version: &str, kind: &str, caps: &str) -> String {
        let connector = if kind.contains("connector") {
            "\n[connector]\ndisplay_name = \"Test\"\n"
        } else {
            ""
        };
        format!(
            "id = \"{id}\"\nname = \"Test plugin\"\nversion = \"{version}\"\ndescription = \"test\"\nkind = {kind}\n\n[capabilities]\n{caps}\n{connector}"
        )
    }

    struct Fx {
        _tmp: Option<tempfile::TempDir>,
        root: PathBuf,
        paths: Paths,
        store: Store,
        secrets: Arc<MemorySecrets>,
        f: Plugins,
    }

    impl Fx {
        async fn new(cfg: &str) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let mut fx = Self::at(tmp.path(), cfg).await;
            fx._tmp = Some(tmp);
            fx
        }

        /// A feature over `root` as a daemon start would build it.
        async fn at(root: &Path, cfg: &str) -> Self {
            let paths = Paths {
                data: root.join("data"),
                config: root.join("config"),
                cache: root.join("cache"),
            };
            let config = Config::parse(cfg).unwrap();
            let _ = connector_factories(&paths, &config);
            let rt = runtime_for(&paths, settings(&config).unwrap())
                .await
                .unwrap();
            let store = Store::open_in_memory().unwrap();
            let secrets = Arc::new(MemorySecrets::new());
            let f =
                Plugins::open(rt, store.clone(), secrets.clone(), Arc::new(store.clone())).await;
            Self {
                _tmp: None,
                root: root.to_path_buf(),
                paths,
                store,
                secrets,
                f,
            }
        }

        /// Write a package file and return its absolute path.
        fn write(&self, name: &str, pkg: &PluginPackage) -> String {
            let path = self.root.join(format!("{name}.{PACKAGE_EXTENSION}"));
            pkg.write_file(&path).unwrap();
            path.display().to_string()
        }

        async fn install(&self, path: &str) -> Result<PluginInstallResult> {
            self.f
                .install(PluginInstallParams {
                    path: path.to_string(),
                })
                .await
        }

        async fn approve(&self, token: &str, approve: bool) -> Result<PluginApproveResult> {
            self.f
                .approve(PluginApproveParams {
                    token: token.to_string(),
                    approve,
                })
                .await
        }

        fn actions(&self) -> Vec<String> {
            let mut a: Vec<_> = self
                .store
                .recent_audit(100)
                .unwrap()
                .into_iter()
                .map(|r| r.action)
                .collect();
            a.reverse();
            a
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            runtimes().remove(&self.paths.data);
        }
    }

    fn signed(id: &str, version: &str, caps: &str, seed: u8) -> PluginPackage {
        PluginPackage::sign(
            manifest(id, version, "[\"tool\"]", caps).into_bytes(),
            EMPTY_COMPONENT.to_vec(),
            &key(seed),
        )
    }

    fn trusted_cfg(seed: u8) -> String {
        format!("[plugins]\ntrusted_keys = [\"{}\"]\n", pubhex(seed))
    }

    #[test]
    fn config_defaults_and_errors() {
        let s = settings(&Config::parse("").unwrap()).unwrap();
        assert!(s.enabled && !s.dev_mode && s.trusted.is_empty());
        let s = settings(&Config::parse(&trusted_cfg(1)).unwrap()).unwrap();
        assert_eq!(s.trusted, vec![key(1).verifying_key()]);
        let bad = Config::parse("[plugins]\ntrusted_keys = [\"zz\"]\n").unwrap();
        let err = settings(&bad).unwrap_err().to_string();
        assert!(err.contains("[plugins]"), "{err}");
        let typo = Config::parse("[plugins]\ndevmode = true\n").unwrap();
        let err = settings(&typo).unwrap_err().to_string();
        assert!(err.contains("[plugins]"), "{err}");
        // A bad section never fails or panics the connector registration.
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: tmp.path().join("d"),
            config: tmp.path().join("c"),
            cache: tmp.path().join("k"),
        };
        assert!(connector_factories(&paths, &bad).is_empty());
        assert!(!runtimes().contains_key(&paths.data));
    }

    #[tokio::test]
    async fn bad_paths_are_refused() {
        let fx = Fx::new(&trusted_cfg(1)).await;
        let rel = fx.install("relative/x.nuggetplugin").await.unwrap_err();
        assert!(rel.to_string().contains("absolute"), "{rel}");
        let missing = fx.root.join("missing.nuggetplugin");
        assert!(fx.install(&missing.display().to_string()).await.is_err());
        let txt = fx.root.join("x.txt");
        std::fs::write(&txt, b"hi").unwrap();
        let err = fx.install(&txt.display().to_string()).await.unwrap_err();
        assert!(err.to_string().contains("nuggetplugin"), "{err}");
        let junk = fx.root.join("junk.nuggetplugin");
        std::fs::write(&junk, b"not a package").unwrap();
        assert!(fx.install(&junk.display().to_string()).await.is_err());
        assert!(fx.actions().is_empty());
    }

    #[tokio::test]
    async fn signed_install_stage_approve_update_disable_remove() {
        let fx = Fx::new(&trusted_cfg(1)).await;
        assert!(fx.f.list().await.unwrap().plugins.is_empty());

        // Untrusted signer: refused.
        let other = fx.write("other", &signed("dev.test.other", "1.0.0", "", 9));
        assert!(fx.install(&other).await.is_err());

        // Stage, reject, the token is spent.
        let caps = "secrets = [\"token\"]";
        let v1 = fx.write("v1", &signed("dev.test.tool", "1.0.0", caps, 1));
        let r = fx.install(&v1).await.unwrap();
        assert!(r.needs_approval && !r.installed && !r.token.is_empty());
        assert_eq!(r.signature, PluginSignature::Trusted);
        assert_eq!(r.signer, pubhex(1));
        assert_eq!(r.requested.secrets, ["token"]);
        assert_eq!(r.kinds, ["tool"]);
        assert!(fx.approve(&r.token, false).await.unwrap().plugin.is_none());
        let err = fx.approve(&r.token, true).await.unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err}");
        assert!(fx.f.list().await.unwrap().plugins.is_empty());

        // Stage and approve: installed, enabled, trusted, but this component cannot load.
        let r = fx.install(&v1).await.unwrap();
        let info = fx.approve(&r.token, true).await.unwrap().plugin.unwrap();
        assert_eq!(info.id, "dev.test.tool");
        assert!(info.enabled && !info.loaded);
        assert!(info.error.as_deref().unwrap().contains("cannot load"));
        assert_eq!(info.signature, PluginSignature::Trusted);
        assert_eq!(info.approved.secrets, ["token"]);
        assert_eq!(info.connector_kind, None);
        let inspected =
            fx.f.inspect(PluginIdParams {
                id: "dev.test.tool".into(),
            })
            .await
            .unwrap();
        assert_eq!(inspected.plugin, info);
        assert_eq!(inspected.requested.secrets, ["token"]);

        // An update within the approved capabilities commits at once (and drops the secret
        // it no longer declares).
        let account = secret_account("dev.test.tool", "token");
        fx.secrets.set(&account, "s3cret").unwrap();
        let v2 = fx.write("v2", &signed("dev.test.tool", "1.1.0", "", 1));
        let r = fx.install(&v2).await.unwrap();
        assert!(!r.needs_approval && r.installed);
        assert_eq!(r.installed_version.as_deref(), Some("1.0.0"));
        assert_eq!(fx.f.list().await.unwrap().plugins[0].version, "1.1.0");
        assert_eq!(fx.secrets.get(&account).unwrap(), None);
        // Same version again: refused.
        assert!(fx.install(&v2).await.is_err());
        // Growing capabilities asks again.
        let v3 = fx.write(
            "v3",
            &signed("dev.test.tool", "1.2.0", "hosts = [\"example.com\"]", 1),
        );
        let r = fx.install(&v3).await.unwrap();
        assert!(r.needs_approval && !r.installed);
        assert_eq!(r.previously_approved.unwrap().secrets, Vec::<String>::new());

        // Disable / enable.
        let off =
            fx.f.enable(PluginEnableParams {
                id: "dev.test.tool".into(),
                enabled: false,
            })
            .await
            .unwrap();
        assert!(!off.enabled && !off.loaded && off.error.is_none());
        let on =
            fx.f.enable(PluginEnableParams {
                id: "dev.test.tool".into(),
                enabled: true,
            })
            .await
            .unwrap();
        assert!(on.enabled && on.error.is_some());
        let missing =
            fx.f.enable(PluginEnableParams {
                id: "dev.test.none".into(),
                enabled: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(missing, Error::NotFound(_)));

        // Remove deletes the plugin's secrets too.
        let v4 = fx.write(
            "v4",
            &signed("dev.test.tool", "1.3.0", "secrets = [\"token\"]", 1),
        );
        let r = fx.install(&v4).await.unwrap();
        fx.approve(&r.token, true).await.unwrap();
        fx.secrets.set(&account, "s3cret").unwrap();
        let removed =
            fx.f.remove(PluginIdParams {
                id: "dev.test.tool".into(),
            })
            .await
            .unwrap();
        assert!(removed.removed);
        assert_eq!(fx.secrets.get(&account).unwrap(), None);
        let again =
            fx.f.remove(PluginIdParams {
                id: "dev.test.tool".into(),
            })
            .await
            .unwrap();
        assert!(!again.removed);

        assert_eq!(
            fx.actions(),
            [
                "plugin.install",
                "plugin.reject",
                "plugin.install",
                "plugin.approve",
                "plugin.install",
                "plugin.install",
                "plugin.disable",
                "plugin.enable",
                "plugin.install",
                "plugin.approve",
                "plugin.remove",
            ]
        );
    }

    #[tokio::test]
    async fn staged_installs_expire_and_are_capped() {
        let fx = Fx::new(&trusted_cfg(1)).await;
        let mut tokens = Vec::new();
        for i in 0..MAX_STAGED + 2 {
            let p = fx.write(
                &format!("p{i}"),
                &signed(&format!("dev.test.p{i}"), "1.0.0", "", 1),
            );
            tokens.push(fx.install(&p).await.unwrap().token);
        }
        assert_eq!(lock(&fx.f.staged).len(), MAX_STAGED);
        // The oldest were dropped.
        assert!(fx.approve(&tokens[0], true).await.is_err());
        // Expire one.
        let last = tokens.last().unwrap();
        lock(&fx.f.staged).get_mut(last).unwrap().expires = Instant::now();
        let err = fx.approve(last, true).await.unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err}");
        // A live one still works.
        assert!(fx.approve(&tokens[3], true).await.unwrap().plugin.is_some());
    }

    #[tokio::test]
    async fn unsigned_packages_need_dev_mode() {
        let unsigned = PluginPackage {
            manifest: manifest("dev.test.dev", "1.0.0", "[\"connector\"]", "").into_bytes(),
            wasm: EMPTY_COMPONENT.to_vec(),
            signature: Vec::new(),
        };

        let tmp = tempfile::tempdir().unwrap();
        let fx = Fx::at(tmp.path(), &trusted_cfg(1)).await;
        let p = fx.write("unsigned", &unsigned);
        let err = fx.install(&p).await.unwrap_err().to_string();
        assert!(err.contains("dev_mode"), "{err}");
        drop(fx);

        let fx = Fx::at(tmp.path(), "[plugins]\ndev_mode = true\n").await;
        let p = fx.write("unsigned", &unsigned);
        let r = fx.install(&p).await.unwrap();
        assert_eq!(r.signature, PluginSignature::Dev);
        let info = fx.approve(&r.token, true).await.unwrap().plugin.unwrap();
        assert_eq!(info.signature, PluginSignature::Dev);
        assert_eq!(info.connector_kind.as_deref(), Some("plugin:dev.test.dev"));
        let seed = fx.secrets.get(DEV_KEY_ACCOUNT).unwrap().unwrap();
        let dev_pub = hex(parse_signing_key(&seed).unwrap().verifying_key().as_bytes());
        assert_eq!(info.signer, dev_pub);
        let cached = std::fs::read_to_string(fx.paths.data.join("plugins").join(DEV_PUB_FILE));
        assert_eq!(cached.unwrap(), dev_pub);

        // A restart without dev_mode: listed as untrusted, not loaded, with the reason.
        drop(fx);
        let fx = Fx::at(tmp.path(), &trusted_cfg(1)).await;
        let info = &fx.f.list().await.unwrap().plugins[0];
        assert_eq!(info.signature, PluginSignature::Untrusted);
        assert!(!info.loaded);
        assert!(info.error.as_deref().unwrap().contains("dev_mode"));
    }

    fn catalog_cfg(seed: u8, dir: &Path) -> String {
        format!(
            "[plugins]\ntrusted_keys = [\"{}\"]\ncatalog_dirs = [{}]\n",
            pubhex(seed),
            toml_str(dir)
        )
    }

    /// A TOML basic string holding `p` (Windows paths have backslashes).
    fn toml_str(p: &Path) -> String {
        serde_json::to_string(&p.display().to_string()).unwrap()
    }

    #[test]
    fn catalog_dirs_default_config_and_env() {
        let data = std::env::temp_dir().join("nugget-data");
        let a = std::env::temp_dir().join("cat-a");
        let b = std::env::temp_dir().join("cat-b");
        assert_eq!(catalog_dirs(&[], &data, None), [data.join(CATALOG_DIR)]);
        assert_eq!(
            catalog_dirs(std::slice::from_ref(&a), &data, None),
            std::slice::from_ref(&a)
        );
        let env = std::env::join_paths([b.clone(), a.clone(), PathBuf::from("relative")]).unwrap();
        assert_eq!(
            catalog_dirs(std::slice::from_ref(&a), &data, Some(env)),
            [a.clone(), b.clone()]
        );
        assert_eq!(
            catalog_dirs(&[], &data, Some(std::ffi::OsString::new())),
            [data.join(CATALOG_DIR)]
        );
        // Relative catalog dirs are a config error.
        let bad = Config::parse("[plugins]\ncatalog_dirs = [\"plugins\"]\n").unwrap();
        let err = settings(&bad).unwrap_err().to_string();
        assert!(err.contains("catalog_dirs"), "{err}");
        let ok = Config::parse(&catalog_cfg(1, &a)).unwrap();
        assert_eq!(settings(&ok).unwrap().catalog_dirs, [a]);
    }

    #[tokio::test]
    async fn catalog_lists_verified_packages_without_installing() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = tmp.path().join("catalog");
        // A missing catalog dir is an empty catalog.
        let fx = Fx::at(tmp.path(), &catalog_cfg(1, &cat)).await;
        let r = fx.f.catalog().await.unwrap();
        assert!(r.entries.is_empty() && !r.truncated);
        assert_eq!(r.dirs, [cat.display().to_string()]);

        std::fs::create_dir_all(cat.join("dir.nuggetplugin")).unwrap();
        std::fs::write(cat.join("notes.txt"), b"ignored").unwrap();
        std::fs::write(cat.join("junk.nuggetplugin"), b"not a package").unwrap();
        let write = |name: &str, pkg: &PluginPackage| {
            pkg.write_file(&cat.join(format!("{name}.{PACKAGE_EXTENSION}")))
                .unwrap()
        };
        write(
            "a-tool",
            &signed("dev.test.tool", "1.0.0", "hosts = [\"example.com\"]", 1),
        );
        write("b-other", &signed("dev.test.other", "1.0.0", "", 9));
        write(
            "c-unsigned",
            &PluginPackage {
                manifest: manifest("dev.test.raw", "1.0.0", "[\"tool\"]", "").into_bytes(),
                wasm: EMPTY_COMPONENT.to_vec(),
                signature: Vec::new(),
            },
        );

        let r = fx.f.catalog().await.unwrap();
        let names: Vec<&str> = r.entries.iter().map(|e| e.file_name.as_str()).collect();
        assert_eq!(
            names,
            [
                "a-tool.nuggetplugin",
                "b-other.nuggetplugin",
                "c-unsigned.nuggetplugin",
                "junk.nuggetplugin"
            ]
        );
        let tool = &r.entries[0];
        assert_eq!(tool.error, None);
        assert_eq!(tool.id, "dev.test.tool");
        assert_eq!(tool.name, "Test plugin");
        assert_eq!(tool.version, "1.0.0");
        assert_eq!(tool.kinds, ["tool"]);
        assert_eq!(tool.requested.hosts, ["example.com"]);
        assert_eq!(tool.signature, PluginSignature::Trusted);
        assert_eq!(tool.signer, pubhex(1));
        assert!(tool.needs_approval && !tool.update_available);
        assert_eq!(tool.installed_version, None);
        assert!(Path::new(&tool.path).is_absolute());
        // Untrusted, unsigned (no dev mode) and corrupt packages: listed with an error, and
        // nothing from an unverified manifest is shown.
        for e in &r.entries[1..] {
            assert!(e.error.is_some(), "{e:?}");
            assert_eq!(e.signature, PluginSignature::Untrusted);
            assert!(e.id.is_empty() && e.name.is_empty(), "{e:?}");
        }
        assert!(
            r.entries[1]
                .error
                .as_deref()
                .unwrap()
                .contains("trusted_keys")
        );
        assert!(r.entries[2].error.as_deref().unwrap().contains("dev_mode"));
        // Listing installs, stages and audits nothing.
        assert!(fx.f.list().await.unwrap().plugins.is_empty());
        assert!(lock(&fx.f.staged).is_empty());
        assert!(fx.actions().is_empty());

        // Installing an entry goes through the normal flow with its path.
        let staged = fx.install(&tool.path).await.unwrap();
        assert!(staged.needs_approval);
        fx.approve(&staged.token, true)
            .await
            .unwrap()
            .plugin
            .unwrap();
        let r = fx.f.catalog().await.unwrap();
        let tool = &r.entries[0];
        assert_eq!(tool.installed_version.as_deref(), Some("1.0.0"));
        assert!(!tool.update_available && !tool.needs_approval && tool.error.is_none());

        // A newer version is an update; one within the approved access needs no approval.
        write(
            "a-tool",
            &signed("dev.test.tool", "1.1.0", "hosts = [\"example.com\"]", 1),
        );
        let tool = &fx.f.catalog().await.unwrap().entries[0];
        assert_eq!(tool.version, "1.1.0");
        assert_eq!(tool.installed_version.as_deref(), Some("1.0.0"));
        assert!(tool.update_available && !tool.needs_approval, "{tool:?}");
        write(
            "a-tool",
            &signed("dev.test.tool", "1.2.0", "secrets = [\"t\"]", 1),
        );
        let tool = &fx.f.catalog().await.unwrap().entries[0];
        assert!(tool.update_available && tool.needs_approval, "{tool:?}");
        // An older version than installed is not offered.
        write("a-tool", &signed("dev.test.tool", "0.9.0", "", 1));
        let tool = &fx.f.catalog().await.unwrap().entries[0];
        assert!(!tool.update_available && tool.error.is_none(), "{tool:?}");
        assert_eq!(fx.actions(), ["plugin.install", "plugin.approve"]);
    }

    #[tokio::test]
    async fn catalog_in_dev_mode_offers_unsigned_packages_and_caps_the_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = tmp.path().join("catalog");
        std::fs::create_dir_all(&cat).unwrap();
        let cfg = format!(
            "[plugins]\ndev_mode = true\ncatalog_dirs = [{}]\n",
            toml_str(&cat)
        );
        let fx = Fx::at(tmp.path(), &cfg).await;
        for i in 0..MAX_CATALOG_ENTRIES + 3 {
            PluginPackage {
                manifest: manifest(&format!("dev.test.p{i:03}"), "1.0.0", "[\"tool\"]", "")
                    .into_bytes(),
                wasm: EMPTY_COMPONENT.to_vec(),
                signature: Vec::new(),
            }
            .write_file(&cat.join(format!("p{i:03}.{PACKAGE_EXTENSION}")))
            .unwrap();
        }
        let r = fx.f.catalog().await.unwrap();
        assert!(r.truncated);
        assert_eq!(r.entries.len(), MAX_CATALOG_ENTRIES);
        // Which 64 depends on the directory order; they are sorted and distinct.
        let names: Vec<&str> = r.entries.iter().map(|e| e.file_name.as_str()).collect();
        assert!(names.windows(2).all(|w| w[0] < w[1]), "{names:?}");
        let first = &r.entries[0];
        assert!(first.id.starts_with("dev.test.p"));
        assert_eq!(first.signature, PluginSignature::Dev);
        assert!(first.error.is_none() && first.needs_approval && first.signer.is_empty());
        // No dev key was created just by listing.
        assert_eq!(fx.secrets.get(DEV_KEY_ACCOUNT).unwrap(), None);
    }

    #[tokio::test]
    async fn catalog_checks_the_signer_of_unsigned_updates_in_dev_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = tmp.path().join("catalog");
        std::fs::create_dir_all(&cat).unwrap();
        let cfg = format!(
            "[plugins]\ndev_mode = true\ntrusted_keys = [\"{}\"]\ncatalog_dirs = [{}]\n",
            pubhex(1),
            toml_str(&cat)
        );
        let fx = Fx::at(tmp.path(), &cfg).await;
        let unsigned = |id: &str, version: &str| PluginPackage {
            manifest: manifest(id, version, "[\"tool\"]", "").into_bytes(),
            wasm: EMPTY_COMPONENT.to_vec(),
            signature: Vec::new(),
        };
        let write = |name: &str, pkg: &PluginPackage| {
            pkg.write_file(&cat.join(format!("{name}.{PACKAGE_EXTENSION}")))
                .unwrap()
        };

        // Installed from a publisher; an unsigned (dev-signed) newer version cannot update it.
        let p = fx.write("pub", &signed("dev.test.pub", "1.0.0", "", 1));
        let r = fx.install(&p).await.unwrap();
        fx.approve(&r.token, true).await.unwrap();
        write("a-pub", &unsigned("dev.test.pub", "1.1.0"));
        let e = &fx.f.catalog().await.unwrap().entries[0];
        assert!(!e.update_available, "{e:?}");
        assert!(
            e.error.as_deref().unwrap().contains("different key"),
            "{e:?}"
        );
        // And install agrees.
        assert!(fx.install(&e.path).await.is_err());

        // Installed dev-signed: an unsigned newer version is an update.
        let p = fx.write("dev", &unsigned("dev.test.dev", "1.0.0"));
        let r = fx.install(&p).await.unwrap();
        fx.approve(&r.token, true).await.unwrap();
        write("b-dev", &unsigned("dev.test.dev", "1.1.0"));
        let r = fx.f.catalog().await.unwrap();
        let e = r.entries.iter().find(|e| e.id == "dev.test.dev").unwrap();
        assert!(e.update_available && e.error.is_none(), "{e:?}");
        assert_eq!(
            e.signer,
            fx.f.rt.dev_key().map(|k| hex(k.as_bytes())).unwrap()
        );
        let r = fx.install(&e.path).await.unwrap();
        assert!(
            r.installed,
            "an update within the approved access commits at once"
        );
    }

    #[tokio::test]
    async fn catalog_caches_verified_packages_until_the_file_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = tmp.path().join("catalog");
        std::fs::create_dir_all(&cat).unwrap();
        let fx = Fx::at(tmp.path(), &catalog_cfg(1, &cat)).await;
        let path = cat.join(format!("a.{PACKAGE_EXTENSION}"));
        signed("dev.test.tool", "1.0.0", "", 1)
            .write_file(&path)
            .unwrap();
        let set_modified = |t: std::time::SystemTime| {
            let f = std::fs::File::options().write(true).open(&path).unwrap();
            f.set_modified(t).unwrap();
        };
        // A file changed just now is not cached.
        assert!(file_stamp(&path).is_none());
        let modified = std::time::SystemTime::now() - Duration::from_secs(60);
        set_modified(modified);
        let e = &fx.f.catalog().await.unwrap().entries[0];
        assert!(e.error.is_none(), "{e:?}");
        let len = std::fs::metadata(&path).unwrap().len() as usize;

        // Same size and time: served from the cache, the file is not read again.
        std::fs::write(&path, vec![b'x'; len]).unwrap();
        set_modified(modified);
        let e = &fx.f.catalog().await.unwrap().entries[0];
        assert!(e.error.is_none() && e.id == "dev.test.tool", "{e:?}");
        // A new time: read again (and now it is junk).
        set_modified(modified + Duration::from_secs(5));
        let e = &fx.f.catalog().await.unwrap().entries[0];
        assert!(e.error.is_some() && e.id.is_empty(), "{e:?}");
        // A removed file leaves the cache.
        std::fs::remove_file(&path).unwrap();
        assert!(fx.f.catalog().await.unwrap().entries.is_empty());
        assert!(lock(&fx.f.rt.catalog_cache).is_empty());
    }

    #[tokio::test]
    async fn catalog_and_install_report_file_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = tmp.path().join("catalog");
        std::fs::create_dir_all(&cat).unwrap();
        let fx = Fx::at(tmp.path(), &catalog_cfg(1, &cat)).await;
        let manifest = "id = \"dev.test.vault\"\nname = \"Vault\"\nversion = \"1.0.0\"\n\
            description = \"test\"\nkind = [\"connector\"]\n\n[capabilities]\n\
            file_roots = [\"vault\"]\n\n[connector]\ndisplay_name = \"Vault\"\n\
            settings_schema = { type = \"object\", properties = { vault = { type = \"string\" } } }\n";
        let path = cat.join(format!("vault.{PACKAGE_EXTENSION}"));
        PluginPackage::sign(manifest.into(), EMPTY_COMPONENT.to_vec(), &key(1))
            .write_file(&path)
            .unwrap();
        let r = fx.f.catalog().await.unwrap();
        let e = &r.entries[0];
        assert_eq!(e.error, None);
        assert_eq!(e.requested.file_roots, ["vault"]);
        assert_eq!(e.connector_display_name.as_deref(), Some("Vault"));
        let v = serde_json::to_value(e).unwrap();
        assert_eq!(v["requested"]["file_roots"], json!(["vault"]));
        let staged = fx.install(&e.path).await.unwrap();
        assert_eq!(staged.requested.file_roots, ["vault"]);
        let info = fx
            .approve(&staged.token, true)
            .await
            .unwrap()
            .plugin
            .unwrap();
        assert_eq!(info.approved.file_roots, ["vault"]);
    }

    #[tokio::test]
    async fn catalog_method_is_declared() {
        let fx = Fx::new(&trusted_cfg(1)).await;
        assert!(fx.f.methods().contains(&plugin_method::CATALOG));
        let v = serde_json::to_value(fx.f.catalog().await.unwrap()).unwrap();
        assert_eq!(v["entries"], json!([]));
        assert_eq!(v["truncated"], json!(false));
    }

    /// Signed first-party packages (CI: `plugins/build.sh` with a throwaway key) install from a
    /// catalog directory through the normal flow and load. Runs only when
    /// `NUGGET_FIRST_PARTY_PLUGINS` names the directory of `.nuggetplugin` files; the trusted
    /// key is `NUGGET_FIRST_PARTY_PLUGINS_PUBKEY` (hex) or derived from
    /// `NUGGET_PLUGIN_SIGNING_KEY`.
    #[tokio::test]
    async fn first_party_plugins_install_from_the_catalog() {
        let Some(dir) = std::env::var_os("NUGGET_FIRST_PARTY_PLUGINS") else {
            eprintln!("NUGGET_FIRST_PARTY_PLUGINS is not set; skipping");
            return;
        };
        let dir = std::fs::canonicalize(PathBuf::from(dir)).unwrap();
        let public = match std::env::var("NUGGET_FIRST_PARTY_PLUGINS_PUBKEY") {
            Ok(k) => parse_verifying_key(&k).unwrap(),
            Err(_) => parse_signing_key(
                &std::env::var("NUGGET_PLUGIN_SIGNING_KEY")
                    .expect("set NUGGET_FIRST_PARTY_PLUGINS_PUBKEY or NUGGET_PLUGIN_SIGNING_KEY"),
            )
            .unwrap()
            .verifying_key(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let cfg = format!(
            "[plugins]\ntrusted_keys = [\"{}\"]\ncatalog_dirs = [{}]\n",
            hex(public.as_bytes()),
            toml_str(&dir)
        );
        let fx = Fx::at(tmp.path(), &cfg).await;
        let catalog = fx.f.catalog().await.unwrap();
        assert!(
            catalog.entries.len() >= 3,
            "expected the first-party plugins in {}: {:?}",
            dir.display(),
            catalog.entries
        );
        for e in &catalog.entries {
            assert_eq!(e.error, None, "{}", e.file_name);
            assert_eq!(e.signature, PluginSignature::Trusted, "{}", e.file_name);
            assert!(e.needs_approval && e.installed_version.is_none());
            let staged = fx.install(&e.path).await.unwrap();
            assert_eq!(staged.id, e.id);
            assert_eq!(staged.requested, e.requested);
            let info = fx
                .approve(&staged.token, true)
                .await
                .unwrap()
                .plugin
                .unwrap();
            assert!(info.loaded, "{} does not load: {:?}", e.id, info.error);
            eprintln!("installed {} {} ({:?})", e.id, e.version, e.kinds);
        }
        let after = fx.f.catalog().await.unwrap();
        for e in &after.entries {
            assert_eq!(e.installed_version.as_deref(), Some(e.version.as_str()));
            assert!(!e.update_available && e.error.is_none());
        }
        assert_eq!(
            fx.f.list().await.unwrap().plugins.len(),
            after.entries.len()
        );
    }

    /// An example plugin, when the guests were built (`cargo test -p nugget-plugin` with
    /// the `wasm32-wasip2` target installed builds them into `target/plugin-guests`).
    fn guest(name: &str) -> Option<Vec<u8>> {
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()?
                    .ancestors()
                    .nth(3)
                    .map(PathBuf::from)
            })?;
        let path = target
            .join("plugin-guests")
            .join("wasm32-wasip2")
            .join("release")
            .join(format!("{name}.wasm"));
        match std::fs::read(&path) {
            Ok(b) => Some(b),
            Err(_) => {
                eprintln!(
                    "WARNING: {} not built; skipping the plugin tool flow test",
                    path.display()
                );
                None
            }
        }
    }

    #[tokio::test]
    async fn tool_plugin_loads_and_offers_agent_tools() {
        let Some(wasm) = guest("text_tools") else {
            return;
        };
        let manifest = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../nugget-plugin/sdk/examples/text-tools/nugget-plugin.toml"),
        )
        .unwrap();
        let fx = Fx::new(&trusted_cfg(1)).await;
        let p = fx.write("tt", &PluginPackage::sign(manifest, wasm, &key(1)));
        let r = fx.install(&p).await.unwrap();
        let info = fx.approve(&r.token, true).await.unwrap().plugin.unwrap();
        assert!(info.loaded, "{:?}", info.error);
        assert!(!info.tools.is_empty());
        assert!(info.tools.iter().all(|t| t.starts_with("plugin.")));
        let tools = agent_tools(&fx.paths).await;
        let names: Vec<_> = tools.iter().map(|t| t.name().to_string()).collect();
        assert_eq!(names, info.tools);
        fx.f.enable(PluginEnableParams {
            id: info.id.clone(),
            enabled: false,
        })
        .await
        .unwrap();
        assert!(agent_tools(&fx.paths).await.is_empty());
    }

    #[tokio::test]
    async fn connector_plugins_register_after_restart_and_follow_disable() {
        let Some(wasm) = guest("feed_reader") else {
            return;
        };
        let manifest = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../nugget-plugin/sdk/examples/feed-reader/nugget-plugin.toml"),
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = trusted_cfg(1);
        let fx = Fx::at(tmp.path(), &cfg).await;
        let p = fx.write("feeds", &PluginPackage::sign(manifest, wasm, &key(1)));
        let r = fx.install(&p).await.unwrap();
        let info = fx.approve(&r.token, true).await.unwrap().plugin.unwrap();
        assert!(info.loaded, "{:?}", info.error);
        assert_eq!(
            info.connector_kind.as_deref(),
            Some("plugin:dev.nugget.example.feeds")
        );
        drop(fx);

        // The next daemon start registers the kind.
        let paths = Paths {
            data: tmp.path().join("data"),
            config: tmp.path().join("config"),
            cache: tmp.path().join("cache"),
        };
        let factories = connector_factories(&paths, &Config::parse(&cfg).unwrap());
        assert_eq!(factories.len(), 1);
        let f = factories[0].clone();
        assert_eq!(f.info().kind, "plugin:dev.nugget.example.feeds");
        let settings = json!({ "url": "https://feeds.example.com/a.xml" });
        f.validate(&settings).unwrap();
        // Disabling it stops the kind without a restart.
        let fx = Fx::at(tmp.path(), &cfg).await;
        let factories = connector_factories(&paths, &Config::parse(&cfg).unwrap());
        let f = factories[0].clone();
        let rt = runtimes().get(&paths.data).cloned().unwrap();
        let fx2 = Plugins::open(
            rt,
            fx.store.clone(),
            fx.secrets.clone(),
            Arc::new(fx.store.clone()),
        )
        .await;
        fx2.enable(PluginEnableParams {
            id: "dev.nugget.example.feeds".into(),
            enabled: false,
        })
        .await
        .unwrap();
        assert!(f.validate(&settings).is_err());
    }
}
