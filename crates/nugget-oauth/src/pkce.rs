use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nugget_core::{Error, Result};
use sha2::{Digest, Sha256};

/// A PKCE verifier and its S256 challenge (RFC 7636).
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// A fresh verifier: 64 characters of base64url from 48 random bytes.
    pub fn generate() -> Result<Self> {
        Ok(Self::from_verifier(random_token(48)?))
    }

    pub fn from_verifier(verifier: impl Into<String>) -> Self {
        let verifier = verifier.into();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }

    pub fn method(&self) -> &'static str {
        "S256"
    }
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

/// `n` random bytes from the OS, base64url-encoded without padding.
pub(crate) fn random_token(n: usize) -> Result<String> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| Error::Other(format!("OS random source: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// Constant-time string comparison (for `state`).
pub(crate) fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
