//! Who "me" is: the user's addresses and names.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

/// The user's identity. Configured by the daemon and completed by inference from sent mail
/// (see [`MeIdentity::infer`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeIdentity {
    /// Email addresses (compared case-insensitively).
    #[serde(default)]
    pub emails: Vec<String>,
    /// Display names, e.g. `Alex Morgan`. Used to recognise sign-offs like `Cheers,\nAlex`.
    #[serde(default)]
    pub names: Vec<String>,
}

/// A sent-mail share at or above this makes an address an alias of the user.
const ALIAS_SHARE: f32 = 0.25;

impl MeIdentity {
    pub fn new<E: Into<String>, N: Into<String>>(
        emails: impl IntoIterator<Item = E>,
        names: impl IntoIterator<Item = N>,
    ) -> Self {
        let mut me = Self::default();
        for e in emails {
            me.add_email(&e.into());
        }
        for n in names {
            me.add_name(&n.into());
        }
        me
    }

    pub fn is_empty(&self) -> bool {
        self.emails.is_empty() && self.names.is_empty()
    }

    /// Whether `email` (any case) is one of the user's addresses.
    pub fn has_email(&self, email: &str) -> bool {
        self.emails
            .iter()
            .any(|e| e.eq_ignore_ascii_case(email.trim()))
    }

    pub fn add_email(&mut self, email: &str) {
        let e = email.trim().to_lowercase();
        if !e.is_empty() && !self.has_email(&e) {
            self.emails.push(e);
        }
    }

    pub fn add_name(&mut self, name: &str) {
        let n = name.trim().trim_matches('"').trim();
        if !n.is_empty() && !self.names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
            self.names.push(n.to_owned());
        }
    }

    /// `self` plus everything in `other`.
    pub fn merged(&self, other: &MeIdentity) -> MeIdentity {
        let mut out = self.clone();
        for e in &other.emails {
            out.add_email(e);
        }
        for n in &other.names {
            out.add_name(n);
        }
        out
    }

    /// Infer the user from the `From` headers of mail they sent: the most frequent address,
    /// plus aliases with at least a quarter of the sent mail (and two messages). Names are the
    /// display names seen with those addresses.
    pub fn infer<'a>(sent_from: impl IntoIterator<Item = &'a str>) -> MeIdentity {
        let mut counts: HashMap<String, (u32, BTreeSet<String>)> = HashMap::new();
        let mut total = 0u32;
        for raw in sent_from {
            let (name, email) = parse_address(raw);
            let Some(email) = email else { continue };
            total += 1;
            let entry = counts.entry(email).or_default();
            entry.0 += 1;
            if let Some(n) = name {
                entry.1.insert(n);
            }
        }
        let mut ranked: Vec<_> = counts.into_iter().collect();
        // Most frequent first; ties by address for determinism.
        ranked.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
        let mut me = MeIdentity::default();
        for (i, (email, (n, names))) in ranked.into_iter().enumerate() {
            let share = n as f32 / total.max(1) as f32;
            if i == 0 || (share >= ALIAS_SHARE && n >= 2) {
                me.add_email(&email);
                for name in names {
                    me.add_name(&name);
                }
            }
        }
        me
    }

    /// Lowercase forms that identify the user at the end of a message: full names, first
    /// names, and initials (`al` for `Alex Lee`, `a`).
    pub(crate) fn name_variants(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |s: String| {
            if !s.is_empty() && !out.contains(&s) {
                out.push(s);
            }
        };
        for n in &self.names {
            let lower = n.to_lowercase();
            push(lower.clone());
            let parts: Vec<&str> = lower
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|p| !p.is_empty())
                .collect();
            if let Some(first) = first_name(n) {
                push(first.to_lowercase());
            }
            let initials: String = parts.iter().filter_map(|p| p.chars().next()).collect();
            push(initials.clone());
            if let Some(c) = initials.chars().next() {
                push(c.to_string());
            }
        }
        for e in &self.emails {
            if let Some(f) = first_name_from_email(e) {
                push(f.to_lowercase());
            }
        }
        out
    }
}

/// Split `Name <email>`, `"Last, First" <email>`, `email` or `Name` into its parts. The email
/// is lowercased.
pub fn parse_address(raw: &str) -> (Option<String>, Option<String>) {
    let raw = raw.trim();
    let clean_name = |s: &str| {
        let n = s.trim().trim_matches('"').trim().to_owned();
        (!n.is_empty()).then_some(n)
    };
    if let (Some(lt), Some(gt)) = (raw.rfind('<'), raw.rfind('>'))
        && lt < gt
    {
        let email = raw[lt + 1..gt].trim().to_lowercase();
        let email = (email.contains('@')).then_some(email);
        return (clean_name(&raw[..lt]), email);
    }
    if raw.contains('@') && !raw.contains(char::is_whitespace) {
        return (None, Some(raw.trim_matches(['<', '>']).to_lowercase()));
    }
    (clean_name(raw), None)
}

/// First name of a display name: `Sam Lee` → `Sam`, `Lee, Sam` → `Sam`.
pub(crate) fn first_name(name: &str) -> Option<String> {
    let name = name.trim().trim_matches('"');
    let part = match name.split_once(',') {
        Some((_, after)) if !after.trim().is_empty() => after,
        _ => name,
    };
    part.split_whitespace()
        .find(|w| w.chars().any(char::is_alphabetic))
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_owned())
        .filter(|w| !w.is_empty())
}

/// A plausible first name from an address local part: `sam.lee@` → `sam`.
pub(crate) fn first_name_from_email(email: &str) -> Option<String> {
    let local = email.split('@').next()?;
    let first = local.split(['.', '_', '-', '+']).next()?;
    (first.len() >= 2 && first.chars().all(|c| c.is_ascii_alphabetic())).then(|| first.to_owned())
}

/// Lowercase first names of recipients (from display names and address local parts).
pub(crate) fn recipient_first_names(recipients: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for r in recipients {
        let (name, email) = parse_address(r);
        let f = name
            .as_deref()
            .and_then(first_name)
            .or_else(|| email.as_deref().and_then(first_name_from_email));
        if let Some(f) = f {
            let f = f.to_lowercase();
            if !out.contains(&f) {
                out.push(f);
            }
        }
    }
    out
}

/// The key used to match recipients: the lowercased address, or the lowercased trimmed name
/// when there is no address (chat handles).
pub(crate) fn recipient_key(raw: &str) -> Option<String> {
    match parse_address(raw) {
        (_, Some(e)) => Some(e),
        (Some(n), None) => Some(n.to_lowercase()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!(
            parse_address("Sam Lee <Sam@Example.com>"),
            (Some("Sam Lee".into()), Some("sam@example.com".into()))
        );
        assert_eq!(
            parse_address("\"Lee, Sam\" <sam@example.com>"),
            (Some("Lee, Sam".into()), Some("sam@example.com".into()))
        );
        assert_eq!(
            parse_address("sam@example.com"),
            (None, Some("sam@example.com".into()))
        );
        assert_eq!(parse_address("Sam"), (Some("Sam".into()), None));
        assert_eq!(first_name("Lee, Sam").as_deref(), Some("Sam"));
        assert_eq!(
            first_name_from_email("jo.smith@x.org").as_deref(),
            Some("jo")
        );
    }

    #[test]
    fn infers_most_frequent_sender_and_aliases() {
        let from = [
            "Alex Morgan <alex@example.com>",
            "Alex Morgan <alex@example.com>",
            "Alex Morgan <alex@example.com>",
            "Alex <alex@work.example>",
            "Alex <alex@work.example>",
            "Delegate <assistant@example.com>",
        ];
        let me = MeIdentity::infer(from);
        assert_eq!(me.emails, vec!["alex@example.com", "alex@work.example"]);
        assert!(me.names.contains(&"Alex Morgan".to_string()));
        assert!(!me.has_email("assistant@example.com"));
        let v = me.name_variants();
        assert!(v.contains(&"alex".to_string()));
        assert!(v.contains(&"am".to_string()));
    }
}
