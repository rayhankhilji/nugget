//! LL1: cursors of the paged list methods (`search.page`, `egress.page`).
//!
//! Cursors are opaque strings to clients. Their format is the daemon's business and may change:
//! - egress: `e1.<id>`, the ledger id of the last row of the previous page (keyset paging);
//! - search: `s1.<offset>.<query tag>`, the number of hits already returned and a tag of the
//!   query they answer, so a cursor from another query is refused.
//!
//! Parsing is strict: anything but a cursor this module made is an invalid-params error.

use nugget_ipc::RpcError;
use sha2::{Digest, Sha256};

/// Deepest a `search.page` ranking goes, in hits.
pub(crate) const MAX_SEARCH_DEPTH: usize = 500;

const EGRESS_PREFIX: &str = "e1.";
const SEARCH_PREFIX: &str = "s1.";
/// Longest cursor accepted, in bytes (ours are far shorter).
const MAX_CURSOR_LEN: usize = 64;

fn bad_cursor(why: &str) -> RpcError {
    RpcError::invalid_params(format!("cursor: {why}"))
}

/// A positive decimal number without sign, leading zeros or anything else.
fn parse_positive(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 19 || s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    s.parse().ok().filter(|&n| n > 0)
}

fn check_len(cursor: &str) -> Result<(), RpcError> {
    if cursor.len() > MAX_CURSOR_LEN {
        return Err(bad_cursor("not a cursor this daemon returned"));
    }
    Ok(())
}

/// The cursor that continues after the ledger row `id`.
pub(crate) fn egress_cursor(id: i64) -> String {
    format!("{EGRESS_PREFIX}{id}")
}

/// The ledger id an `egress.page` cursor continues after.
pub(crate) fn parse_egress_cursor(cursor: &str) -> Result<i64, RpcError> {
    check_len(cursor)?;
    cursor
        .strip_prefix(EGRESS_PREFIX)
        .and_then(parse_positive)
        .and_then(|n| i64::try_from(n).ok())
        .ok_or_else(|| bad_cursor("not a cursor this daemon returned for egress.page"))
}

/// 16 hex digits tying a search cursor to its query (not a secret: only a mix-up guard).
fn query_tag(query: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"nugget search.page cursor\0");
    h.update(query.as_bytes());
    hex::encode(&h.finalize()[..8])
}

/// The cursor that continues `query` after its first `offset` hits.
pub(crate) fn search_cursor(query: &str, offset: usize) -> String {
    format!("{SEARCH_PREFIX}{offset}.{}", query_tag(query))
}

/// The number of hits of `query` already returned before an `search.page` cursor.
pub(crate) fn parse_search_cursor(cursor: &str, query: &str) -> Result<usize, RpcError> {
    check_len(cursor)?;
    let bad = || bad_cursor("not a cursor this daemon returned for search.page");
    let rest = cursor.strip_prefix(SEARCH_PREFIX).ok_or_else(bad)?;
    let (offset, tag) = rest.split_once('.').ok_or_else(bad)?;
    let offset = parse_positive(offset)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&n| n < MAX_SEARCH_DEPTH)
        .ok_or_else(bad)?;
    if tag.len() != 16 || !tag.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(bad());
    }
    if tag != query_tag(query) {
        return Err(bad_cursor("it belongs to another query"));
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use nugget_ipc::code;

    use super::*;

    #[test]
    fn egress_cursors_round_trip_and_reject_anything_else() {
        for id in [1, 42, i64::MAX] {
            assert_eq!(parse_egress_cursor(&egress_cursor(id)).unwrap(), id);
        }
        for bad in [
            "",
            "e1.",
            "e1.0",
            "e1.-5",
            "e1.+5",
            "e1.007",
            "e1.5 ",
            "e1.5.1",
            "e1.9223372036854775808",
            "e1.99999999999999999999",
            "e2.5",
            "5",
            "s1.5.0011223344556677",
            "e1.٣",
        ] {
            let err = parse_egress_cursor(bad).unwrap_err();
            assert_eq!(err.code, code::INVALID_PARAMS, "{bad:?}");
        }
        assert!(parse_egress_cursor(&format!("e1.{}", "1".repeat(100))).is_err());
    }

    #[test]
    fn search_cursors_are_tied_to_their_query() {
        let c = search_cursor("herons", 25);
        assert_eq!(parse_search_cursor(&c, "herons").unwrap(), 25);
        let err = parse_search_cursor(&c, "swans").unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS);
        assert!(err.message.contains("another query"), "{}", err.message);
        // Queries differing only in spacing or case are other queries.
        assert!(parse_search_cursor(&c, "Herons").is_err());
        assert!(parse_search_cursor(&c, "herons ").is_err());

        let tag = &c[c.rfind('.').unwrap() + 1..];
        for bad in [
            String::new(),
            "s1.".into(),
            format!("s1.0.{tag}"),
            format!("s1.-1.{tag}"),
            format!("s1.025.{tag}"),
            format!("s1.{MAX_SEARCH_DEPTH}.{tag}"),
            format!("s1.99999999999999999999.{tag}"),
            format!("s1.25.{}", tag.to_uppercase()),
            format!("s1.25.{}", &tag[..15]),
            format!("s1.25.{tag}0"),
            format!("s1.25.{tag}.x"),
            format!("s1.25{tag}"),
            format!("s2.25.{tag}"),
            egress_cursor(25),
        ] {
            let err = parse_search_cursor(&bad, "herons").unwrap_err();
            assert_eq!(err.code, code::INVALID_PARAMS, "{bad:?}");
        }
        assert_eq!(
            parse_search_cursor(&search_cursor("herons", MAX_SEARCH_DEPTH - 1), "herons").unwrap(),
            MAX_SEARCH_DEPTH - 1
        );
    }
}
