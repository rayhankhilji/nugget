//! `POST /v1/waitlist {email, source?}`: the website's waitlist while Heed Cloud is not open.
//!
//! Unauthenticated, so it is built not to leak: every well-formed JSON body gets the same empty
//! `202`, whether the address is new, already listed or invalid (invalid ones are simply not
//! stored), so the endpoint cannot tell anyone who signed up. Only malformed JSON gets `400`.
//! Rate-limited per client IP; the body is capped at [`MAX_BODY`]; the IP is kept only as a
//! salted hash (`NUGGET_WAITLIST_SALT`), or not at all without a salt.

use sha2::{Digest, Sha256};

use super::*;

pub const MAX_BODY: usize = 1024;

#[derive(Deserialize)]
struct Body {
    #[serde(default)]
    email: Value,
    #[serde(default)]
    source: Value,
}

/// The address, trimmed and lowercased, if it is plausible: at most 254 characters, exactly
/// one `@` with something on both sides, a dot in the domain, and no spaces or control
/// characters.
pub fn normalise_email(raw: &str) -> Option<String> {
    let e = raw.trim().to_lowercase();
    let (local, domain) = e.split_once('@')?;
    let ok = (3..=254).contains(&e.chars().count())
        && !local.is_empty()
        && local.chars().count() <= 64
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !e.chars().any(|c| c.is_control() || c.is_whitespace());
    ok.then_some(e)
}

/// A short, safe label for where the sign-up came from (`site` by default).
fn source_label(v: &Value) -> String {
    v.as_str()
        .map(str::trim)
        .filter(|s| {
            (1..=32).contains(&s.len())
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        })
        .unwrap_or("site")
        .to_owned()
}

pub fn ip_hash(salt: &str, ip: std::net::IpAddr) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update([0]);
    h.update(ip.to_string().as_bytes());
    format!("{:x}", h.finalize())
}

pub(super) async fn join(State(s): State<Arc<AppState>>, req: Request) -> ApiResult<StatusCode> {
    let ip = client_ip(&s, &req);
    // Clients we cannot place share one bucket.
    let key = ip.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    s.limits
        .waitlist
        .check(&key)
        .map_err(|w| ApiError::rate_limited("waitlist", w))?;
    let body = axum::body::to_bytes(req.into_body(), MAX_BODY)
        .await
        .map_err(|_| ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "body too large"))?;
    let b: Body = serde_json::from_slice(&body).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "expected JSON like {\"email\": \"…\"}",
        )
    })?;
    if let Some(email) = b.email.as_str().and_then(normalise_email) {
        let hash = match (&s.waitlist_salt, ip) {
            (Some(salt), Some(ip)) => Some(ip_hash(salt, ip)),
            _ => None,
        };
        s.db.waitlist_add(&email, &source_label(&b.source), hash.as_deref())
            .await?;
    }
    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails() {
        assert_eq!(
            normalise_email("  Ada@Example.COM ").as_deref(),
            Some("ada@example.com")
        );
        for bad in [
            "",
            "ada",
            "@example.com",
            "ada@",
            "a@b@c.com",
            "ada@example",
            "ada@.com",
            "ada@example.com.",
            "a da@example.com",
            "ada@exa\u{0}mple.com",
            "ada\n@example.com",
        ] {
            assert_eq!(normalise_email(bad), None, "{bad:?}");
        }
        let long = format!("{}@{}.com", "a".repeat(64), "b".repeat(190));
        assert!(long.len() > 254 && normalise_email(&long).is_none());
        assert!(normalise_email(&format!("{}@example.com", "a".repeat(65))).is_none());
    }

    #[test]
    fn sources_and_hashes() {
        assert_eq!(source_label(&json!("pricing-page")), "pricing-page");
        assert_eq!(source_label(&json!("<script>")), "site");
        assert_eq!(source_label(&Value::Null), "site");
        let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();
        let h = ip_hash("salt", ip);
        assert_eq!(h.len(), 64);
        assert_ne!(h, ip_hash("other", ip), "the salt matters");
        assert!(!h.contains("203"));
    }
}
