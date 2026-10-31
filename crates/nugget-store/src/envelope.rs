//! The store key sealed under a backup passphrase, so a backup can be restored on a machine that
//! never held the key.
//!
//! Format (`v` 1, serde JSON): Argon2id over the passphrase with a random 16-byte salt derives a
//! 256-bit key that seals the store key with XChaCha20-Poly1305 under a random 24-byte nonce.
//! The associated data binds the format version, the KDF and its parameters, the salt, the AEAD
//! and the key id, so editing any of them (for example lowering the KDF cost) makes opening fail.
//! The KDF parameters are read from the envelope (within bounds checked before the KDF runs),
//! so it can be opened whatever they were.
//!
//! The passphrase is NFC-normalised before key derivation, so the same passphrase typed on
//! systems that compose accented characters differently opens the envelope.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

use crate::key::StoreKey;

const VERSION: u32 = 1;
const KDF: &str = "argon2id";
const AEAD: &str = "xchacha20poly1305";
/// Default Argon2id memory cost (64 MiB), iterations and lanes.
const DEFAULT_M_KIB: u32 = 64 * 1024;
const DEFAULT_T: u32 = 3;
const DEFAULT_P: u32 = 1;
/// Upper bounds on parameters read from an envelope, so a crafted file cannot make opening it
/// allocate unbounded memory or spin for hours.
const MAX_M_KIB: u32 = 256 * 1024;
const MAX_T: u32 = 10;
const MAX_P: u32 = 16;
/// Least cost [`KeyEnvelope::seal_with`] accepts (8 MiB, 1 pass).
const MIN_M_KIB: u32 = 8 * 1024;
const MIN_T: u32 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const AD_DOMAIN: &str = "nugget-key-envelope";

/// The store key sealed under a passphrase (see the module docs). Serialize it to JSON to store
/// it; nothing in it is secret without the passphrase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEnvelope {
    v: u32,
    kdf: String,
    m_kib: u32,
    t: u32,
    p: u32,
    /// Hex.
    salt: String,
    aead: String,
    /// Hex.
    nonce: String,
    /// Hex: the sealed 32-byte key followed by the 16-byte tag.
    ct: String,
    /// [`StoreKey::key_id`] of the sealed key.
    key_id: String,
}

impl KeyEnvelope {
    /// Seal `key` under `passphrase` with the default Argon2id cost (64 MiB, 3 passes, 1 lane).
    pub fn seal(key: &StoreKey, passphrase: &str) -> Result<Self> {
        Self::seal_with(key, passphrase, DEFAULT_M_KIB, DEFAULT_T, DEFAULT_P)
    }

    /// Seal `key` under `passphrase` with explicit Argon2id parameters: memory `m_kib` KiB
    /// (8 MiB to 256 MiB), `t` passes (1 to 10), `p` lanes (1 to 16). `Error::Invalid` outside
    /// those bounds or for parameters Argon2 rejects.
    pub fn seal_with(key: &StoreKey, passphrase: &str, m_kib: u32, t: u32, p: u32) -> Result<Self> {
        if m_kib < MIN_M_KIB || t < MIN_T {
            return Err(Error::Invalid(format!(
                "key envelope KDF parameters m={m_kib} KiB, t={t} are below the minimum \
                 (m >= {MIN_M_KIB}, t >= {MIN_T})"
            )));
        }
        Self::seal_unchecked(key, passphrase, m_kib, t, p)
    }

    /// [`KeyEnvelope::seal_with`] without the minimum cost, so tests can seal in microseconds.
    /// Never use it for a real envelope.
    #[doc(hidden)]
    pub fn seal_insecure_for_tests(
        key: &StoreKey,
        passphrase: &str,
        m_kib: u32,
        t: u32,
        p: u32,
    ) -> Result<Self> {
        Self::seal_unchecked(key, passphrase, m_kib, t, p)
    }

    fn seal_unchecked(
        key: &StoreKey,
        passphrase: &str,
        m_kib: u32,
        t: u32,
        p: u32,
    ) -> Result<Self> {
        let mut salt = [0u8; SALT_LEN];
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut salt)
            .and_then(|()| getrandom::fill(&mut nonce))
            .map_err(|e| Error::Other(format!("OS random number generator failed: {e}")))?;
        let mut env = Self {
            v: VERSION,
            kdf: KDF.to_owned(),
            m_kib,
            t,
            p,
            salt: hex::encode(salt),
            aead: AEAD.to_owned(),
            nonce: hex::encode(nonce),
            ct: String::new(),
            key_id: key.key_id(),
        };
        let kek = env.derive(passphrase, &salt)?;
        let ct = XChaCha20Poly1305::new(kek.as_slice().into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: key.as_bytes(),
                    aad: env.associated_data().as_bytes(),
                },
            )
            .map_err(|_| Error::Other("cannot seal the store key".into()))?;
        env.ct = hex::encode(ct);
        Ok(env)
    }

    /// Recover the store key. `Error::Integrity` when the passphrase is wrong or the envelope was
    /// modified (the two cannot be told apart), `Error::Unsupported` for an unknown format, KDF or
    /// cipher, `Error::Invalid` for out-of-range parameters.
    pub fn open(&self, passphrase: &str) -> Result<StoreKey> {
        if self.v != VERSION || self.kdf != KDF || self.aead != AEAD {
            return Err(Error::Unsupported(format!(
                "key envelope v{} ({}, {}) is not supported by this version",
                self.v, self.kdf, self.aead
            )));
        }
        let malformed = || Error::Integrity("key envelope is malformed".into());
        let salt = hex::decode(&self.salt).map_err(|_| malformed())?;
        let nonce: [u8; NONCE_LEN] = hex::decode(&self.nonce)
            .ok()
            .and_then(|n| n.try_into().ok())
            .ok_or_else(malformed)?;
        let ct = hex::decode(&self.ct).map_err(|_| malformed())?;
        let kek = self.derive(passphrase, &salt)?;
        let plain = Zeroizing::new(
            XChaCha20Poly1305::new(kek.as_slice().into())
                .decrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &ct,
                        aad: self.associated_data().as_bytes(),
                    },
                )
                .map_err(|_| {
                    Error::Integrity(
                        "wrong passphrase, or the key envelope was modified or damaged".into(),
                    )
                })?,
        );
        let mut bytes = Zeroizing::new([0u8; 32]);
        if plain.len() != bytes.len() {
            return Err(malformed());
        }
        bytes.copy_from_slice(&plain);
        let key = StoreKey::from_bytes(*bytes);
        if key.key_id() != self.key_id {
            return Err(Error::Integrity(
                "key envelope holds a key other than the one it names".into(),
            ));
        }
        Ok(key)
    }

    /// [`StoreKey::key_id`] of the sealed key (readable without the passphrase).
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The Argon2id key-encryption key for `passphrase`, with this envelope's parameters.
    fn derive(&self, passphrase: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
        if self.m_kib > MAX_M_KIB || self.t > MAX_T || self.p > MAX_P {
            return Err(Error::Invalid(format!(
                "key envelope KDF parameters m={} KiB, t={}, p={} exceed the limits (m <= {MAX_M_KIB}, \
                 t <= {MAX_T}, p <= {MAX_P})",
                self.m_kib, self.t, self.p
            )));
        }
        let params = Params::new(self.m_kib, self.t, self.p, Some(32))
            .map_err(|e| Error::Invalid(format!("key envelope KDF parameters: {e}")))?;
        let normalized = Zeroizing::new(passphrase.nfc().collect::<String>());
        let mut kek = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(normalized.as_bytes(), salt, kek.as_mut_slice())
            .map_err(|e| Error::Invalid(format!("key envelope KDF: {e}")))?;
        Ok(kek)
    }

    /// Every field except the nonce and ciphertext, length-unambiguous (`|` never occurs in
    /// them: hex, fixed names and decimal numbers).
    fn associated_data(&self) -> String {
        format!(
            "{AD_DOMAIN}|v={}|kdf={}|m_kib={}|t={}|p={}|salt={}|aead={}|key_id={}",
            self.v, self.kdf, self.m_kib, self.t, self.p, self.salt, self.aead, self.key_id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small Argon2 parameters so tests run fast.
    fn seal(key: &StoreKey, pass: &str) -> KeyEnvelope {
        KeyEnvelope::seal_insecure_for_tests(key, pass, 64, 1, 1).unwrap()
    }

    #[test]
    fn round_trip_through_json() {
        let key = StoreKey::generate().unwrap();
        let env = seal(&key, "correct horse battery");
        assert_eq!(env.key_id(), key.key_id());
        let json = serde_json::to_string(&env).unwrap();
        assert!(!json.contains(&hex::encode(key.as_bytes())));
        let back: KeyEnvelope = serde_json::from_str(&json).unwrap();
        let opened = back.open("correct horse battery").unwrap();
        assert_eq!(opened.as_bytes(), key.as_bytes());
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        for field in [
            "v", "kdf", "m_kib", "t", "p", "salt", "aead", "nonce", "ct", "key_id",
        ] {
            assert!(v.get(field).is_some(), "missing {field}: {json}");
        }
        assert_eq!(v["kdf"], "argon2id");
        assert_eq!(v["aead"], "xchacha20poly1305");
    }

    #[test]
    fn default_parameters_are_recorded() {
        let env = KeyEnvelope::seal(&StoreKey::from_bytes([3; 32]), "passphrase").unwrap();
        assert_eq!((env.m_kib, env.t, env.p), (64 * 1024, 3, 1));
        assert_eq!(
            env.open("passphrase").unwrap().as_bytes(),
            StoreKey::from_bytes([3; 32]).as_bytes()
        );
    }

    #[test]
    fn cost_bounds_are_enforced() {
        let key = StoreKey::from_bytes([2; 32]);
        for (m, t) in [
            (64, 1),
            (MIN_M_KIB - 1, 1),
            (MIN_M_KIB, 0),
            (MAX_M_KIB + 1, 1),
            (MIN_M_KIB, MAX_T + 1),
        ] {
            assert!(
                matches!(
                    KeyEnvelope::seal_with(&key, "passphrase", m, t, 1),
                    Err(Error::Invalid(_))
                ),
                "m={m} t={t}"
            );
        }
        // Opening checks the bounds before running the KDF.
        let env = seal(&key, "passphrase");
        for bad in [
            KeyEnvelope {
                m_kib: MAX_M_KIB + 1,
                ..env.clone()
            },
            KeyEnvelope {
                t: MAX_T + 1,
                ..env.clone()
            },
            KeyEnvelope {
                p: MAX_P + 1,
                ..env.clone()
            },
        ] {
            let started = std::time::Instant::now();
            assert!(matches!(bad.open("passphrase"), Err(Error::Invalid(_))));
            assert!(started.elapsed() < std::time::Duration::from_secs(1));
        }
    }

    #[test]
    fn passphrases_are_nfc_normalised() {
        let key = StoreKey::from_bytes([6; 32]);
        let composed = "caf\u{e9} cr\u{e8}me";
        let decomposed = "cafe\u{301} cre\u{300}me";
        assert_ne!(composed, decomposed);
        let env = seal(&key, composed);
        assert_eq!(env.open(decomposed).unwrap().as_bytes(), key.as_bytes());
    }

    #[test]
    fn wrong_passphrase_is_an_integrity_error() {
        let env = seal(&StoreKey::from_bytes([1; 32]), "right passphrase");
        let err = env.open("wrong passphrase").unwrap_err();
        assert!(
            matches!(&err, Error::Integrity(m) if m.contains("wrong passphrase")),
            "{err}"
        );
    }

    #[test]
    fn sealing_twice_differs() {
        let key = StoreKey::from_bytes([1; 32]);
        let (a, b) = (seal(&key, "pass phrase"), seal(&key, "pass phrase"));
        assert_ne!((a.salt, a.nonce, a.ct), (b.salt, b.nonce, b.ct));
    }

    #[test]
    fn tampering_fails_to_open() {
        let key = StoreKey::from_bytes([9; 32]);
        let env = seal(&key, "pass phrase");
        let flip = |s: &str| {
            let mut b = hex::decode(s).unwrap();
            b[0] ^= 1;
            hex::encode(b)
        };
        let tampered: Vec<(&str, KeyEnvelope)> = vec![
            (
                "ct",
                KeyEnvelope {
                    ct: flip(&env.ct),
                    ..env.clone()
                },
            ),
            (
                "nonce",
                KeyEnvelope {
                    nonce: flip(&env.nonce),
                    ..env.clone()
                },
            ),
            (
                "salt",
                KeyEnvelope {
                    salt: flip(&env.salt),
                    ..env.clone()
                },
            ),
            (
                "t",
                KeyEnvelope {
                    t: 2,
                    ..env.clone()
                },
            ),
            (
                "m_kib",
                KeyEnvelope {
                    m_kib: 128,
                    ..env.clone()
                },
            ),
            (
                "key_id",
                KeyEnvelope {
                    key_id: StoreKey::from_bytes([8; 32]).key_id(),
                    ..env.clone()
                },
            ),
            (
                "truncated",
                KeyEnvelope {
                    ct: env.ct[..env.ct.len() - 2].to_owned(),
                    ..env.clone()
                },
            ),
        ];
        for (what, t) in tampered {
            let err = t.open("pass phrase").unwrap_err();
            assert!(matches!(err, Error::Integrity(_)), "{what}: {err}");
        }
        let huge = KeyEnvelope {
            m_kib: u32::MAX,
            ..env.clone()
        };
        assert!(matches!(huge.open("pass phrase"), Err(Error::Invalid(_))));
        let v2 = KeyEnvelope {
            v: 2,
            ..env.clone()
        };
        assert!(matches!(v2.open("pass phrase"), Err(Error::Unsupported(_))));
        assert_eq!(env.open("pass phrase").unwrap().as_bytes(), key.as_bytes());
    }
}
