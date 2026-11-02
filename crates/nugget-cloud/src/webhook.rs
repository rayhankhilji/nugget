//! Composio webhook verification: HMAC-SHA256 over `{webhook-id}.{webhook-timestamp}.{body}`,
//! base64, in `webhook-signature` (optionally `v1,<sig>`, several separated by spaces).

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Seconds a timestamp may differ from now.
pub const TOLERANCE_SECS: i64 = 300;

pub fn verify(
    secret: &str,
    id: &str,
    timestamp: &str,
    body: &[u8],
    signature_header: &str,
    now: i64,
) -> bool {
    let Ok(ts) = timestamp.trim().parse::<i64>() else {
        return false;
    };
    if (now - ts).abs() > TOLERANCE_SECS || id.is_empty() {
        return false;
    }
    signature_header.split_whitespace().any(|candidate| {
        let sig = candidate.split_once(',').map_or(candidate, |(_, s)| s);
        let Ok(expected) = base64::engine::general_purpose::STANDARD.decode(sig) else {
            return false;
        };
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
            return false;
        };
        mac.update(id.as_bytes());
        mac.update(b".");
        mac.update(timestamp.trim().as_bytes());
        mac.update(b".");
        mac.update(body);
        mac.verify_slice(&expected).is_ok() // constant time
    })
}

#[cfg(test)]
pub fn sign(secret: &str, id: &str, timestamp: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{id}.{timestamp}.").as_bytes());
    mac.update(body);
    format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_rejects_tampering_and_stale() {
        let body = br#"{"type":"composio.trigger.message"}"#;
        let sig = sign("s3cret", "msg_1", "1000", body);
        assert!(verify("s3cret", "msg_1", "1000", body, &sig, 1100));
        let bare = sig.split_once(',').unwrap().1;
        assert!(
            verify("s3cret", "msg_1", "1000", body, bare, 1100),
            "prefix optional"
        );
        assert!(
            verify(
                "s3cret",
                "msg_1",
                "1000",
                body,
                &format!("v1,AAAA {sig}"),
                1100
            ),
            "any of several"
        );
        assert!(!verify("other", "msg_1", "1000", body, &sig, 1100));
        assert!(!verify("s3cret", "msg_2", "1000", body, &sig, 1100));
        assert!(!verify("s3cret", "msg_1", "1000", b"{}", &sig, 1100));
        assert!(!verify(
            "s3cret",
            "msg_1",
            "1000",
            body,
            &sig,
            1000 + TOLERANCE_SECS + 1
        ));
        assert!(!verify("s3cret", "msg_1", "x", body, &sig, 1100));
        assert!(!verify("s3cret", "msg_1", "1000", body, "v1,!!!", 1100));
    }
}
