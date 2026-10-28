//! Mail headers (`metadata.extracted` of `message/rfc822` items): people by address,
//! organisations by corporate domain, `works_at` (penalised) and `corresponded_with`.

use nugget_graph::EntityKind;
use serde_json::Value;

use super::{ItemCtx, strings};
use crate::spec::{ClaimSpec, ERef, EntitySpec, Extraction, OSpec};
use crate::text::{
    Address, domain_of, is_automated, is_freemail, org_name_from_domain, parse_address,
    registrable_domain,
};

/// Participants considered per message at most.
const MAX_PARTICIPANTS: usize = 100;

pub(super) fn applies(ctx: &ItemCtx<'_>) -> bool {
    let has_headers = ctx.extracted("from").is_some()
        && (ctx.extracted("to").is_some()
            || ctx.extracted("subject").is_some()
            || ctx.extracted("message_id").is_some());
    ctx.mime() == "message/rfc822" || has_headers
}

/// Whether the message sits in the user's sent mail (IMAP folder name, Gmail label).
fn in_sent_folder(ctx: &ItemCtx<'_>) -> bool {
    let labels = strings(ctx.source_meta("labels"));
    if labels.iter().any(|l| l.eq_ignore_ascii_case("SENT")) {
        return true;
    }
    let Some(folder) = ctx.source_meta("folder").and_then(Value::as_str) else {
        return false;
    };
    let leaf = folder
        .rsplit(['/', '.', '\\'])
        .next()
        .unwrap_or(folder)
        .to_lowercase();
    const SENT: &[&str] = &[
        "sent",
        "sent mail",
        "sent items",
        "sent messages",
        "sent-mail",
        "gesendet",
        "gesendete elemente",
        "gesendete objekte",
        "envoyés",
        "éléments envoyés",
        "enviados",
        "elementos enviados",
        "inviata",
        "posta inviata",
        "verzonden",
        "verzonden items",
        "skickat",
        "sendt",
        "lähetetyt",
        "wysłane",
    ];
    SENT.contains(&leaf.as_str())
}

struct Participant {
    role: &'static str,
    raw: String,
    addr: Address,
}

pub(super) fn extract(ctx: &ItemCtx<'_>, ex: &mut Extraction) {
    let mut parts: Vec<Participant> = Vec::new();
    let from_raw = strings(ctx.extracted("from"));
    for (role, list) in [
        ("From", from_raw.clone()),
        ("To", strings(ctx.extracted("to"))),
        ("Cc", strings(ctx.extracted("cc"))),
        ("Bcc", strings(ctx.extracted("bcc"))),
    ] {
        for raw in list {
            if parts.len() >= MAX_PARTICIPANTS {
                break;
            }
            let addr = parse_address(&raw);
            let Some(email) = addr.email.clone() else {
                continue;
            };
            if parts
                .iter()
                .any(|p| p.addr.email.as_deref() == Some(email.as_str()))
            {
                continue;
            }
            parts.push(Participant { role, raw, addr });
        }
    }
    let Some(sender) = parts.first().filter(|p| p.role == "From") else {
        return;
    };

    // The user's own address, learned from sent mail.
    if ctx.cfg.me.infer_from_sent
        && in_sent_folder(ctx)
        && from_raw.len() == 1
        && let Some(email) = sender.addr.email.clone()
        && !is_automated(&email)
    {
        if !ctx.me_emails.contains(&email) {
            ex.me_emails.push(email);
        }
        if ex.me_name.is_none() {
            ex.me_name = sender.addr.name.clone();
        }
    }
    let learned = ex.me_emails.clone();
    let me_known = !ctx.me_emails.is_empty() || !learned.is_empty();

    let mut people: Vec<(ERef, &Participant)> = Vec::new();
    for p in &parts {
        let email = p.addr.email.as_deref().unwrap_or_default();
        if ctx.is_me(email, &learned) {
            people.push((ERef::Me, p));
            continue;
        }
        if is_automated(email) {
            continue;
        }
        let quote = format!("{}: {}", p.role, p.raw);
        let (name, weak) = match &p.addr.name {
            Some(n) => (n.clone(), false),
            None => (email.to_owned(), true),
        };
        let mut spec = EntitySpec::new(EntityKind::Person, name.clone())
            .ident("email", email)
            .mention(&name, None, 0.9);
        spec.weak_name = weak;
        let person = ex.entity(spec);
        people.push((person, p));

        // Organisation from a corporate domain.
        let Some(domain) = domain_of(email) else {
            continue;
        };
        if is_freemail(&domain, &ctx.cfg.extra_freemail_domains) {
            continue;
        }
        let reg = registrable_domain(&domain);
        let org_name = org_name_from_domain(&reg);
        let org = ex.entity(
            EntitySpec::new(EntityKind::Organization, org_name.clone())
                .ident("domain", &reg)
                .mention(&reg, None, 0.6),
        );
        let who = ex.name_of(person, ctx.me_label);
        ex.claim(ClaimSpec {
            subject: person,
            predicate: "works_at".into(),
            object: OSpec::Entity(org),
            text: format!("{who} works at {org_name} (has a {reg} address)."),
            confidence: ctx.cfg.mail_works_at_confidence.clamp(0.0, 1.0),
            evidence: ctx.evidence(None, &quote),
            valid_from: None,
        });
    }

    // corresponded_with: the user and the people on the message.
    let me_involved = people.iter().any(|(r, _)| *r == ERef::Me);
    let crowd = parts.len() > ctx.cfg.max_correspondents;
    for (r, p) in &people {
        if *r == ERef::Me {
            continue;
        }
        let is_sender = p.role == "From";
        let confidence = if me_involved {
            if is_sender || !crowd {
                0.9
            } else {
                continue;
            }
        } else if is_sender {
            // The message is in the user's mailbox, so they received it from the sender.
            if me_known { 0.5 } else { 0.6 }
        } else {
            continue;
        };
        let who = ex.name_of(*r, ctx.me_label);
        ex.claim(ClaimSpec {
            subject: ERef::Me,
            predicate: "corresponded_with".into(),
            object: OSpec::Entity(*r),
            text: format!("{} corresponded with {who} by email.", ctx.me_label),
            confidence,
            evidence: ctx.evidence(None, &format!("{}: {}", p.role, p.raw)),
            valid_from: None,
        });
    }
}
