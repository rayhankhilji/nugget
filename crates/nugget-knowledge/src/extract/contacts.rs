//! Contacts: vCards (`text/vcard` items, or `.vcf` keys) and contact records in metadata
//! (`metadata.{source,extracted}.contact` or `.contacts`: `{name, emails, phones, organization,
//! title, birthday}`). A contact card asserts its identifiers together, so entities holding
//! them may be merged (see [`EntitySpec::trusted_identifiers`]).

use nugget_graph::EntityKind;
use serde_json::{Value, json};

use super::{ItemCtx, strings};
use crate::spec::{ClaimSpec, EntitySpec, Extraction, OSpec};
use crate::text::{clean_person_name, is_email, truncate_chars};

const MAX_CONTACTS: usize = 2_000;
const MAX_VALUES: usize = 20;

#[derive(Debug, Default, PartialEq)]
struct Contact {
    name: Option<String>,
    emails: Vec<String>,
    phones: Vec<String>,
    organization: Option<String>,
    title: Option<String>,
    birthday: Option<String>,
    /// Source lines, for evidence quotes.
    lines: Vec<String>,
}

fn unescape(v: &str) -> String {
    v.replace("\\n", " ")
        .replace("\\N", " ")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

/// Parse the vCards in `text` (2.1, 3.0 and 4.0 property syntax; unknown properties ignored).
fn parse_vcards(text: &str) -> Vec<Contact> {
    // Unfold continuation lines.
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        if (raw.starts_with(' ') || raw.starts_with('\t'))
            && let Some(last) = lines.last_mut()
        {
            last.push_str(&raw[1..]);
            continue;
        }
        lines.push(raw.trim_end().to_owned());
    }
    let mut out = Vec::new();
    let mut cur: Option<Contact> = None;
    for line in lines {
        let upper = line.to_ascii_uppercase();
        if upper == "BEGIN:VCARD" {
            cur = Some(Contact::default());
            continue;
        }
        if upper == "END:VCARD" {
            if let Some(c) = cur.take()
                && out.len() < MAX_CONTACTS
            {
                out.push(c);
            }
            continue;
        }
        let Some(c) = cur.as_mut() else { continue };
        let Some((head, value)) = line.split_once(':') else {
            continue;
        };
        let name = head
            .split(';')
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let value = unescape(value.trim());
        if value.is_empty() {
            continue;
        }
        let keep = match name.as_str() {
            "FN" => {
                c.name = Some(value.clone());
                true
            }
            "N" if c.name.is_none() => {
                let parts: Vec<&str> = value.split(';').collect();
                let given = parts.get(1).copied().unwrap_or("").trim();
                let family = parts.first().copied().unwrap_or("").trim();
                let full = format!("{given} {family}").trim().to_owned();
                if !full.is_empty() {
                    c.name = Some(full);
                }
                true
            }
            "EMAIL" if c.emails.len() < MAX_VALUES => {
                let e = value.trim_start_matches("mailto:").to_lowercase();
                if is_email(&e) {
                    c.emails.push(e);
                }
                true
            }
            "TEL" if c.phones.len() < MAX_VALUES => {
                c.phones.push(value.trim_start_matches("tel:").to_owned());
                true
            }
            "ORG" => {
                let org = value.split(';').next().unwrap_or("").trim().to_owned();
                if !org.is_empty() {
                    c.organization = Some(org);
                }
                true
            }
            "TITLE" => {
                c.title = Some(value.clone());
                true
            }
            "BDAY" => {
                c.birthday = Some(value.clone());
                true
            }
            _ => false,
        };
        if keep {
            c.lines.push(format!("{name}:{value}"));
        }
    }
    out
}

fn from_json(v: &Value) -> Option<Contact> {
    let obj = v.as_object()?;
    let s = |k: &str| {
        obj.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
    };
    let name = s("name").or_else(|| s("display_name")).or_else(|| s("fn"));
    let mut emails: Vec<String> = strings(obj.get("emails"))
        .into_iter()
        .chain(strings(obj.get("email")))
        .map(|e| e.to_lowercase())
        .filter(|e| is_email(e))
        .collect();
    emails.dedup();
    let phones: Vec<String> = strings(obj.get("phones"))
        .into_iter()
        .chain(strings(obj.get("phone")))
        .collect();
    let mut c = Contact {
        name,
        emails,
        phones,
        organization: s("organization")
            .or_else(|| s("organisation"))
            .or_else(|| s("company")),
        title: s("title").or_else(|| s("job_title")),
        birthday: s("birthday"),
        lines: Vec::new(),
    };
    c.emails.truncate(MAX_VALUES);
    c.phones.truncate(MAX_VALUES);
    for (k, v) in [
        ("Name", c.name.clone()),
        ("Organization", c.organization.clone()),
        ("Title", c.title.clone()),
        ("Birthday", c.birthday.clone()),
    ] {
        if let Some(v) = v {
            c.lines.push(format!("{k}: {v}"));
        }
    }
    for e in &c.emails {
        c.lines.push(format!("Email: {e}"));
    }
    (c.name.is_some() || !c.emails.is_empty()).then_some(c)
}

fn contacts(ctx: &ItemCtx<'_>) -> Vec<Contact> {
    let mut out = Vec::new();
    for holder in [
        ctx.meta("source"),
        ctx.meta("extracted"),
        Some(&ctx.item.metadata),
    ] {
        let Some(h) = holder else { continue };
        if let Some(c) = h.get("contact").and_then(from_json) {
            out.push(c);
        }
        if let Some(Value::Array(list)) = h.get("contacts") {
            out.extend(list.iter().take(MAX_CONTACTS).filter_map(from_json));
        }
    }
    let mime = ctx.mime();
    let key = ctx.item.item.key.as_str().to_ascii_lowercase();
    if mime == "text/vcard"
        || mime == "text/x-vcard"
        || mime == "text/directory"
        || key.ends_with(".vcf")
    {
        let text = match &ctx.item.text {
            Some(t) => t.clone(),
            None => ctx
                .chunks
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        out.extend(parse_vcards(&text));
    }
    out.truncate(MAX_CONTACTS);
    out
}

pub(super) fn extract(ctx: &ItemCtx<'_>, ex: &mut Extraction) {
    for c in contacts(ctx) {
        let line = |needle: &str| {
            c.lines
                .iter()
                .find(|l| l.contains(needle))
                .cloned()
                .unwrap_or_else(|| needle.to_owned())
        };
        let (name, weak) = match c.name.as_deref().and_then(clean_person_name) {
            Some(n) => (n, false),
            None => match c.emails.first() {
                Some(e) => (e.clone(), true),
                None => continue,
            },
        };
        if c.emails.iter().any(|e| ctx.is_me(e, &[])) {
            continue; // the user's own card
        }
        let chunk = crate::text::locate(ctx.chunks, &name).map(|(id, _)| id);
        let mut spec = EntitySpec::new(EntityKind::Person, name.clone())
            .mention(&name, chunk, 0.95)
            .attr("title", json!(c.title))
            .attr("birthday", json!(c.birthday));
        for e in &c.emails {
            spec = spec.ident("email", e);
        }
        for p in &c.phones {
            spec = spec.ident("phone", p);
        }
        spec.weak_name = weak;
        spec.trusted_identifiers = true;
        let person = ex.entity(spec);
        if let Some(org) = &c.organization {
            let org = truncate_chars(org, 200);
            let o = ex.entity(
                EntitySpec::new(EntityKind::Organization, org.clone()).mention(&org, chunk, 0.8),
            );
            ex.claim(ClaimSpec {
                subject: person,
                predicate: "works_at".into(),
                object: OSpec::Entity(o),
                text: format!("{name} works at {org}."),
                confidence: 0.85,
                evidence: ctx.evidence(chunk, &line(&org)),
                valid_from: None,
            });
        }
        if let Some(title) = &c.title {
            ex.claim(ClaimSpec {
                subject: person,
                predicate: "has_title".into(),
                object: OSpec::Literal(truncate_chars(title, 200)),
                text: format!("{name}'s job title is {title}."),
                confidence: 0.85,
                evidence: ctx.evidence(chunk, &line(title)),
                valid_from: None,
            });
        }
        if let Some(b) = &c.birthday {
            ex.claim(ClaimSpec {
                subject: person,
                predicate: "born_on".into(),
                object: OSpec::Literal(truncate_chars(b, 40)),
                text: format!("{name} was born on {b}."),
                confidence: 0.9,
                evidence: ctx.evidence(chunk, &line(b)),
                valid_from: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vcards_parse() {
        let v = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Dana Scully\r\nN:Scully;Dana;;;\r\nEMAIL;TYPE=work:dana@fbi.gov\r\nEMAIL:dana.s@gmail.com\r\nTEL;TYPE=cell:+1 555 0100\r\nORG:FBI;X-Files\r\nTITLE:Special\r\n  Agent\r\nEND:VCARD\r\n";
        let cs = parse_vcards(v);
        assert_eq!(cs.len(), 1);
        let c = &cs[0];
        assert_eq!(c.name.as_deref(), Some("Dana Scully"));
        assert_eq!(c.emails, vec!["dana@fbi.gov", "dana.s@gmail.com"]);
        assert_eq!(c.organization.as_deref(), Some("FBI"));
        assert_eq!(c.title.as_deref(), Some("Special Agent"));
    }
}
