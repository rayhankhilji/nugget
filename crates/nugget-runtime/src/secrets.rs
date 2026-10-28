//! Per-process API keys for local servers, handed to `nugget-infer` through a secret resolver.
//!
//! Every (re)start of a server gets a fresh key ([`RuntimeSecrets::rotate`]), so a key that
//! leaked to whatever answered on the port while the server was down (or to a stale client)
//! is useless against the next server. Configs name the key *generation* they were issued for
//! (`env:NUGGET_RUNTIME_API_KEY_CHAT_<n>`): a config handed out before a restart therefore no
//! longer resolves (fail closed), and it differs from the new one, so callers that rebuild their
//! provider when the config changes pick up the new key.

use std::sync::{Arc, Mutex, PoisonError};

use nugget_core::ipc::RuntimeRole;
use nugget_core::{Error, Result};
use nugget_infer::{ApiKeyRef, Secret, SecretResolver, SystemSecrets};

/// The `api_key` reference for `role`'s *current* key, whatever its generation:
/// `env:NUGGET_RUNTIME_API_KEY_CHAT` or `env:NUGGET_RUNTIME_API_KEY_EMBEDDING`.
///
/// Configs returned by [`crate::Runtime::ensure`] carry a generation-specific reference instead
/// (this name plus `_<n>`), which stops resolving once the server restarts with a new key.
///
/// The variable is **not** set in the environment (that would need `unsafe` and would leak to
/// every child). Only [`RuntimeSecrets`] answers it, from memory; a plain [`SystemSecrets`]
/// reports it as unset, so a config used without the runtime's resolver fails closed.
pub fn api_key_ref(role: RuntimeRole) -> ApiKeyRef {
    ApiKeyRef::Env(api_key_var(role).to_string())
}

/// The reference for generation `generation` of `role`'s key.
pub(crate) fn api_key_ref_gen(role: RuntimeRole, generation: u64) -> ApiKeyRef {
    ApiKeyRef::Env(format!("{}_{generation}", api_key_var(role)))
}

fn api_key_var(role: RuntimeRole) -> &'static str {
    match role {
        RuntimeRole::Chat => "NUGGET_RUNTIME_API_KEY_CHAT",
        RuntimeRole::Embedding => "NUGGET_RUNTIME_API_KEY_EMBEDDING",
    }
}

const ROLES: [RuntimeRole; 2] = [RuntimeRole::Chat, RuntimeRole::Embedding];

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

/// One role's current key.
#[derive(Clone)]
struct Slot {
    generation: u64,
    key: Secret,
}

/// Resolves the runtime's [`api_key_ref`]s from memory and delegates every other reference to a
/// fallback (the OS environment and keychain by default). Pass it to
/// [`nugget_infer::build_provider_with`]. Clones share the keys, so they see every rotation.
#[derive(Clone)]
pub struct RuntimeSecrets {
    keys: Arc<[Mutex<Slot>; 2]>,
    fallback: Arc<dyn SecretResolver>,
}

impl std::fmt::Debug for RuntimeSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSecrets").finish_non_exhaustive()
    }
}

fn slot_idx(role: RuntimeRole) -> usize {
    match role {
        RuntimeRole::Chat => 0,
        RuntimeRole::Embedding => 1,
    }
}

impl RuntimeSecrets {
    pub(crate) fn new(chat: Secret, embedding: Secret) -> Self {
        Self {
            keys: Arc::new([
                Mutex::new(Slot {
                    generation: 0,
                    key: chat,
                }),
                Mutex::new(Slot {
                    generation: 0,
                    key: embedding,
                }),
            ]),
            fallback: Arc::new(SystemSecrets),
        }
    }

    /// Delegate references that are not the runtime's own keys to `fallback`.
    pub fn with_fallback(mut self, fallback: Arc<dyn SecretResolver>) -> Self {
        self.fallback = fallback;
        self
    }

    fn slot(&self, role: RuntimeRole) -> std::sync::MutexGuard<'_, Slot> {
        self.keys[slot_idx(role)]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The current key of `role` and its generation.
    pub(crate) fn current(&self, role: RuntimeRole) -> (u64, Secret) {
        let s = self.slot(role);
        (s.generation, s.key.clone())
    }

    /// Replace `role`'s key with a fresh one (for the next server process). Returns the new
    /// generation and key.
    pub(crate) fn rotate(&self, role: RuntimeRole) -> Result<(u64, Secret)> {
        let key = random_key()?;
        let mut s = self.slot(role);
        s.generation += 1;
        s.key = key;
        Ok((s.generation, s.key.clone()))
    }
}

impl SecretResolver for RuntimeSecrets {
    fn resolve(&self, key: &ApiKeyRef) -> Result<Secret> {
        if let ApiKeyRef::Env(name) = key {
            for role in ROLES {
                let var = api_key_var(role);
                if name == var {
                    return Ok(self.current(role).1);
                }
                if let Some(generation) = name
                    .strip_prefix(var)
                    .and_then(|rest| rest.strip_prefix('_'))
                {
                    // Never let the OS environment answer one of our names.
                    let (current, secret) = self.current(role);
                    return match generation.parse::<u64>() {
                        Ok(g) if g == current => Ok(secret),
                        _ => Err(Error::Unauthorized(format!(
                            "{name}: the local runtime restarted with a new key; \
                             call Runtime::ensure for a fresh config"
                        ))),
                    };
                }
            }
        }
        self.fallback.resolve(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_retires_old_generations() {
        let s = RuntimeSecrets::new(random_key().unwrap(), random_key().unwrap());
        let (g0, k0) = s.current(RuntimeRole::Chat);
        let r0 = api_key_ref_gen(RuntimeRole::Chat, g0);
        assert_eq!(s.resolve(&r0).unwrap().expose(), k0.expose());
        let (g1, k1) = s.rotate(RuntimeRole::Chat).unwrap();
        assert_ne!(g0, g1);
        assert_ne!(k0.expose(), k1.expose());
        assert!(matches!(s.resolve(&r0), Err(Error::Unauthorized(_))));
        let r1 = api_key_ref_gen(RuntimeRole::Chat, g1);
        assert_eq!(s.resolve(&r1).unwrap().expose(), k1.expose());
        // The generation-less name follows the current key; the other role is untouched.
        let cur = s.resolve(&api_key_ref(RuntimeRole::Chat)).unwrap();
        assert_eq!(cur.expose(), k1.expose());
        assert_eq!(s.current(RuntimeRole::Embedding).0, 0);
        // Garbage generations fail closed instead of reaching the environment.
        let bad = ApiKeyRef::Env("NUGGET_RUNTIME_API_KEY_CHAT_x".into());
        assert!(s.resolve(&bad).is_err());
    }
}
