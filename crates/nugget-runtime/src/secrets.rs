//! Per-process API keys for local servers, handed to `nugget-infer` through a secret resolver.

use std::sync::Arc;

use nugget_core::ipc::RuntimeRole;
use nugget_core::{Error, Result};
use nugget_infer::{ApiKeyRef, Secret, SecretResolver, SystemSecrets};

/// The `api_key` reference written into a runtime's [`nugget_infer::ProviderConfig`]:
/// `env:NUGGET_RUNTIME_API_KEY_CHAT` or `env:NUGGET_RUNTIME_API_KEY_EMBEDDING`.
///
/// The variable is **not** set in the environment (that would need `unsafe` and would leak to
/// every child). Only [`RuntimeSecrets`] answers it, from memory; a plain [`SystemSecrets`]
/// reports it as unset, so a config used without the runtime's resolver fails closed.
pub fn api_key_ref(role: RuntimeRole) -> ApiKeyRef {
    ApiKeyRef::Env(api_key_var(role).to_string())
}

fn api_key_var(role: RuntimeRole) -> &'static str {
    match role {
        RuntimeRole::Chat => "NUGGET_RUNTIME_API_KEY_CHAT",
        RuntimeRole::Embedding => "NUGGET_RUNTIME_API_KEY_EMBEDDING",
    }
}

/// 256 random bits, hex encoded.
pub(crate) fn random_key() -> Result<Secret> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| Error::Other(format!("no OS randomness: {e}")))?;
    let mut s = String::with_capacity(64);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    b.fill(0);
    Ok(Secret::new(s))
}

/// Resolves the runtime's [`api_key_ref`]s from memory and delegates every other reference to a
/// fallback (the OS environment and keychain by default). Pass it to
/// [`nugget_infer::build_provider_with`].
#[derive(Clone)]
pub struct RuntimeSecrets {
    keys: Arc<[(&'static str, Secret); 2]>,
    fallback: Arc<dyn SecretResolver>,
}

impl std::fmt::Debug for RuntimeSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSecrets").finish_non_exhaustive()
    }
}

impl RuntimeSecrets {
    pub(crate) fn new(chat: Secret, embedding: Secret) -> Self {
        Self {
            keys: Arc::new([
                (api_key_var(RuntimeRole::Chat), chat),
                (api_key_var(RuntimeRole::Embedding), embedding),
            ]),
            fallback: Arc::new(SystemSecrets),
        }
    }

    /// Delegate references that are not the runtime's own keys to `fallback`.
    pub fn with_fallback(mut self, fallback: Arc<dyn SecretResolver>) -> Self {
        self.fallback = fallback;
        self
    }

    pub(crate) fn key(&self, role: RuntimeRole) -> &Secret {
        let var = api_key_var(role);
        &self
            .keys
            .iter()
            .find(|(n, _)| *n == var)
            .expect("both roles have a key")
            .1
    }
}

impl SecretResolver for RuntimeSecrets {
    fn resolve(&self, key: &ApiKeyRef) -> Result<Secret> {
        if let ApiKeyRef::Env(name) = key
            && let Some((_, s)) = self.keys.iter().find(|(n, _)| n == name)
        {
            return Ok(s.clone());
        }
        self.fallback.resolve(key)
    }
}
