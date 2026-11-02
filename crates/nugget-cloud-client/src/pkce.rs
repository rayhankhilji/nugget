//! PKCE (RFC 7636): a high-entropy verifier and its S256 challenge.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::{CloudError, Result};

/// Random bytes in a verifier: 64 bytes encode to 86 characters (RFC 7636 allows 43–128).
const VERIFIER_BYTES: usize = 64;

/// A verifier and its challenge. The verifier is a secret: it is never logged or shown.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .finish()
    }
}

impl Pkce {
    /// A fresh verifier from the OS CSPRNG.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; VERIFIER_BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|e| CloudError::Invalid(format!("no secure random numbers: {e}")))?;
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let challenge = challenge(&verifier);
        Ok(Self {
            verifier,
            challenge,
        })
    }
}

/// `BASE64URL(SHA256(ASCII(verifier)))`, without padding.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifiers_are_long_unreserved_and_unique() {
        let a = Pkce::generate().unwrap();
        let b = Pkce::generate().unwrap();
        assert!(
            (43..=128).contains(&a.verifier.len()),
            "{}",
            a.verifier.len()
        );
        assert!(
            a.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c))
        );
        assert_ne!(a.verifier, b.verifier);
        assert_eq!(a.challenge, challenge(&a.verifier));
        assert!(!a.challenge.contains('='));
        assert!(!format!("{a:?}").contains(&a.verifier));
    }
}
