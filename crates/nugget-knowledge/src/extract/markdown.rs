//! Markdown notes: frontmatter, `#tags` and `[[wikilinks]]`.
//!
//! - Tags and frontmatter `topic(s)` become topics; frontmatter `project(s)` become projects;
//!   `people` / `attendees` / `members` / `owner` (and a person's `reports_to` / `manager`)
//!   become people; `org` / `company` / `organization` / `club` become organisations.
//! - A note whose frontmatter `type` (or `kind`) names an entity kind *is* that entity, named
//!   after the note's title. A project's `owner` leads it and its `members` work on it; a
//!   person's `org` is where they work and their `club` one they are a member of; an
//!   organisation-valued key of a project is what the project is part of.
//! - Every frontmatter claim quotes the `key: value` line it came from, exactly as stored.
//! - The note's subjects (its own entity, else its projects, else its topics) are what its
//!   wikilinks connect: a link becomes a `references` claim from each subject to the linked
//!   note's subjects (or an entity of that name) once the target resolves.

use nugget_graph::{EntityKind, Evidence};
use serde_json::{Map, Value};

use super::{ItemCtx, strings};
use crate::spec::{ClaimSpec, ERef, EntitySpec, Extraction, LinkSpec, OSpec};
use crate::text::truncate_chars;

/// Tags used as link subjects at most (when the note has no type, project or topic).
const MAX_TAG_SUBJECTS: usize = 3;
const MAX_LINKS: usize = 200;
const MAX_LIST: usize = 50;

/// Frontmatter keys naming the people who lead the note's subject.
const LEAD_KEYS: &[&str] = &["owner", "owners", "lead", "leads"];
/// Frontmatter keys naming the people involved in the note's subject.
const MEMBER_KEYS: &[&str] = &[
    "people",
    "person",
    "attendees",
    "participants",
    "with",
    "members",
    "member",
    "team",
];
/// Frontmatter keys of a person note naming who they report to.
const MANAGER_KEYS: &[&str] = &["reports_to", "manager"];
/// Frontmatter keys naming an employer or owning organisation.
const ORG_KEYS: &[&str] = &["org", "company", "organization", "organisation", "employer"];
/// Frontmatter keys naming an organisation someone is a member of.
const CLUB_KEYS: &[&str] = &["club", "clubs"];

pub(super) fn applies(ctx: &ItemCtx<'_>) -> bool {
    let mime = ctx.mime();
    mime == "text/markdown"
        || mime == "text/x-markdown"
        || ctx.extracted("frontmatter").is_some()
        || ctx.extracted("wikilinks").is_some()
}

fn get_ci<'a>(m: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    m.iter()
        .find(|(k, _)| keys.iter().any(|x| k.eq_ignore_ascii_case(x)))
        .map(|(_, v)| v)
}

/// A frontmatter value and the `key: value` line it was read from: the key as written and the
/// value as stored, so the line is the source's own text and serves as evidence quote.
#[derive(Debug, Clone, PartialEq)]
struct FmName {
    /// The value with `[[wikilink]]` brackets, a `|alias` and a leading `#` removed.
    name: String,
    quote: String,
}

/// Frontmatter list values of every key in `keys` (case-insensitive), de-duplicated by name.
fn names(m: &Map<String, Value>, keys: &[&str]) -> Vec<FmName> {
    let mut out: Vec<FmName> = Vec::new();
    for (key, value) in m
        .iter()
        .filter(|(k, _)| keys.iter().any(|x| k.eq_ignore_ascii_case(x)))
    {
        for raw in strings(Some(value)) {
            let raw = raw.trim();
            let v = raw
                .trim_start_matches("[[")
                .trim_end_matches("]]")
                .trim_start_matches('#');
            let v = v.split('|').next().unwrap_or(v).trim();
            if v.is_empty() || out.iter().any(|o| o.name.eq_ignore_ascii_case(v)) {
                continue;
            }
            out.push(FmName {
                name: truncate_chars(v, 200),
                quote: format!("{key}: {raw}"),
            });
            if out.len() >= MAX_LIST {
                return out;
            }
        }
    }
    out
}

fn kind_of(t: &str) -> Option<EntityKind> {
    Some(match t.trim().to_lowercase().as_str() {
        "person" | "people" | "contact" => EntityKind::Person,
        "project" => EntityKind::Project,
        "organization" | "organisation" | "org" | "company" => EntityKind::Organization,
        "topic" | "area" | "concept" => EntityKind::Topic,
        "place" | "location" => EntityKind::Place,
        "event" | "meeting" => EntityKind::Event,
        _ => return None,
    })
}

/// The note's file name without extension (`notes/Project Apollo.md` → `Project Apollo`).
pub(crate) fn file_stem(key: &str) -> String {
    let leaf = key.rsplit(['/', '\\']).next().unwrap_or(key);
    match leaf.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
        _ => leaf.to_owned(),
    }
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest
        .split(['/', '?', '#', ':'])
        .next()?
        .trim_start_matches("www.");
    host.contains('.').then(|| host.to_lowercase())
}

/// How a person named in frontmatter relates to the note's subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersonRole {
    Lead,
    Member,
    /// The (person) note's subject reports to them.
    Manager,
}

/// How an organisation named in frontmatter relates to the note's subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrgRole {
    Employer,
    Club,
}

pub(super) fn extract(ctx: &ItemCtx<'_>, ex: &mut Extraction) {
    let empty = Map::new();
    let fm = match ctx.extracted("frontmatter") {
        Some(Value::Object(m)) => m,
        _ => &empty,
    };
    let stem = file_stem(ctx.item.item.key.as_str());
    let title = ctx
        .item
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| stem.clone());
    let aliases = strings(ctx.extracted("aliases"));
    let ev = |f: &FmName| ctx.evidence(None, &f.quote);

    // The note itself, when typed.
    let typed = get_ci(fm, &["type", "kind", "category"])
        .and_then(Value::as_str)
        .and_then(kind_of);
    let note = typed.map(|kind| {
        let mut spec = EntitySpec::new(kind, truncate_chars(&title, 200)).mention(
            &title,
            ctx.chunks.first().map(|c| c.id),
            0.9,
        );
        spec.aliases = aliases.iter().map(|a| truncate_chars(a, 200)).collect();
        for key in [
            "status", "due", "deadline", "start", "end", "date", "role", "url", "website",
        ] {
            if let Some(v) = get_ci(fm, &[key]).filter(|v| v.is_string() || v.is_number()) {
                spec = spec.attr(key, v.clone());
            }
        }
        if kind == EntityKind::Person {
            for e in names(fm, &["email", "emails"]) {
                if crate::text::is_email(&e.name) {
                    spec = spec.ident("email", &e.name);
                }
            }
            for p in names(fm, &["phone", "phones"]) {
                spec = spec.ident("phone", &p.name);
            }
        }
        if kind == EntityKind::Organization {
            for d in names(fm, &["domain", "website", "url"]) {
                if let Some(h) = host_of(&d.name) {
                    spec = spec.ident("domain", &crate::text::registrable_domain(&h));
                }
            }
        }
        ex.entity(spec)
    });

    let mut projects = Vec::new();
    for p in names(fm, &["project", "projects"]) {
        let r = ex.entity(
            EntitySpec::new(EntityKind::Project, p.name.clone()).mention(&p.name, None, 0.8),
        );
        if Some(r) != note {
            projects.push((r, p));
        }
    }
    let mut topics = Vec::new();
    for t in names(fm, &["topic", "topics"]) {
        let r = ex
            .entity(EntitySpec::new(EntityKind::Topic, t.name.clone()).mention(&t.name, None, 0.8));
        if Some(r) != note {
            let e = ev(&t);
            topics.push((r, t.name, e));
        }
    }
    let fm_topic_count = topics.len();
    let fm_tags = names(fm, &["tags", "tag"]);
    for tag in strings(ctx.meta("tags")).into_iter().take(MAX_LIST) {
        let tag = tag.trim().trim_start_matches('#').to_owned();
        if tag.is_empty() {
            continue;
        }
        let ev_at = crate::text::locate(ctx.chunks, &format!("#{tag}"));
        let chunk = ev_at.as_ref().map(|(id, _)| *id);
        let r = ex.entity(
            EntitySpec::new(EntityKind::Topic, tag.clone())
                .ident("tag", &tag.to_lowercase())
                .mention(&format!("#{tag}"), chunk, 0.8),
        );
        // The body line with the tag, else the frontmatter line listing it, else no quote.
        let evidence = match ev_at {
            Some((id, line)) => ctx.evidence(Some(id), &line),
            None => fm_tags
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case(&tag))
                .map_or_else(|| ctx.evidence(None, ""), ev),
        };
        if Some(r) != note && !topics.iter().any(|(x, _, _)| *x == r) {
            topics.push((r, tag, evidence));
        }
    }

    let mut people: Vec<(ERef, String, PersonRole, FmName)> = Vec::new();
    let person_groups: &[(&[&str], PersonRole)] = if typed == Some(EntityKind::Person) {
        &[
            (LEAD_KEYS, PersonRole::Lead),
            (MEMBER_KEYS, PersonRole::Member),
            (MANAGER_KEYS, PersonRole::Manager),
        ]
    } else {
        &[
            (LEAD_KEYS, PersonRole::Lead),
            (MEMBER_KEYS, PersonRole::Member),
        ]
    };
    for (keys, role) in person_groups {
        for p in names(fm, keys) {
            if let Some(name) = crate::text::clean_person_name(&p.name) {
                let r = ex.entity(
                    EntitySpec::new(EntityKind::Person, name.clone()).mention(&name, None, 0.7),
                );
                if Some(r) != note {
                    people.push((r, name, *role, p));
                }
            }
        }
    }
    let mut orgs: Vec<(ERef, OrgRole, FmName)> = Vec::new();
    for (keys, role) in [(ORG_KEYS, OrgRole::Employer), (CLUB_KEYS, OrgRole::Club)] {
        for o in names(fm, keys) {
            let r = ex.entity(
                EntitySpec::new(EntityKind::Organization, o.name.clone())
                    .mention(&o.name, None, 0.7),
            );
            if Some(r) != note {
                orgs.push((r, role, o));
            }
        }
    }

    let claim =
        |ex: &mut Extraction, s: ERef, p: &str, o: ERef, text: String, c: f32, e: Evidence| {
            ex.claim(ClaimSpec {
                subject: s,
                predicate: p.into(),
                object: OSpec::Entity(o),
                text,
                confidence: c,
                evidence: e,
                valid_from: None,
            });
        };

    match (note, typed) {
        (Some(n), Some(kind)) => {
            let nn = ex.name_of(n, ctx.me_label);
            for (t, tn, q) in &topics {
                claim(
                    ex,
                    n,
                    "has_topic",
                    *t,
                    format!("{nn} is about {tn}."),
                    0.8,
                    q.clone(),
                );
            }
            for (p, f) in &projects {
                let pn = &f.name;
                claim(
                    ex,
                    n,
                    "part_of",
                    *p,
                    format!("{nn} is part of {pn}."),
                    0.8,
                    ev(f),
                );
            }
            for (p, pn, role, f) in &people {
                let (s, pred, o, text, c) = match (kind, role) {
                    (EntityKind::Person, PersonRole::Manager) => {
                        (n, "reports_to", *p, format!("{nn} reports to {pn}."), 0.8)
                    }
                    (EntityKind::Project, PersonRole::Lead) => {
                        (*p, "leads", n, format!("{pn} leads {nn}."), 0.8)
                    }
                    (EntityKind::Project, _) => {
                        (*p, "works_on", n, format!("{pn} works on {nn}."), 0.7)
                    }
                    (EntityKind::Event, _) => {
                        (*p, "attended", n, format!("{pn} attended {nn}."), 0.7)
                    }
                    _ => (
                        *p,
                        "related_to",
                        n,
                        format!("{pn} is related to {nn}."),
                        0.7,
                    ),
                };
                claim(ex, s, pred, o, text, c, ev(f));
            }
            for (o, role, f) in &orgs {
                let on = &f.name;
                let (pred, text, c) = match (kind, role) {
                    (EntityKind::Person, OrgRole::Employer) => {
                        ("works_at", format!("{nn} works at {on}."), 0.8)
                    }
                    (EntityKind::Person, OrgRole::Club) => {
                        ("member_of", format!("{nn} is a member of {on}."), 0.8)
                    }
                    (EntityKind::Project, _) => ("part_of", format!("{nn} is part of {on}."), 0.8),
                    _ => ("related_to", format!("{nn} is related to {on}."), 0.6),
                };
                claim(ex, n, pred, *o, text, c, ev(f));
            }
        }
        _ => {
            for (p, pf) in &projects {
                let pn = &pf.name;
                for (t, tn, q) in &topics {
                    claim(
                        ex,
                        *p,
                        "has_topic",
                        *t,
                        format!("{pn} involves {tn}."),
                        0.6,
                        q.clone(),
                    );
                }
                for (person, name, role, f) in &people {
                    let (pred, text) = match role {
                        PersonRole::Lead => ("leads", format!("{name} leads {pn}.")),
                        _ => ("works_on", format!("{name} works on {pn}.")),
                    };
                    claim(ex, *person, pred, *p, text, 0.6, ev(f));
                }
            }
        }
    }

    // Subjects for links: the note's own entity, else its projects, else its topics.
    let subjects: Vec<ERef> = if let Some(n) = note {
        vec![n]
    } else if !projects.is_empty() {
        projects.iter().map(|(r, _)| *r).collect()
    } else if fm_topic_count > 0 {
        topics[..fm_topic_count]
            .iter()
            .map(|(r, _, _)| *r)
            .collect()
    } else {
        topics
            .iter()
            .take(MAX_TAG_SUBJECTS)
            .map(|(r, _, _)| *r)
            .collect()
    };
    ex.note_subjects = subjects.clone();
    let mut note_names = vec![title.clone(), stem.clone()];
    note_names.extend(aliases.iter().cloned());
    note_names.dedup();
    ex.note_names = note_names;

    let Some(Value::Array(links)) = ctx.extracted("wikilinks") else {
        return;
    };
    let mut seen: Vec<String> = Vec::new();
    for l in links.iter().take(MAX_LINKS) {
        let Some(target) = l.get("target").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        let embed = l.get("embed").and_then(Value::as_bool) == Some(true);
        let lower = target.to_lowercase();
        if target.is_empty()
            || (embed && lower.contains('.') && !lower.ends_with(".md"))
            || seen.contains(&lower)
        {
            continue;
        }
        seen.push(lower);
        let target = target.trim_end_matches(".md").to_owned();
        let evidence = ctx.evidence_at(&format!("[[{target}"), &format!("[[{target}]]"));
        for s in &subjects {
            ex.links.push(LinkSpec {
                from: *s,
                target: target.clone(),
                evidence: evidence.clone(),
                confidence: 0.7,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use nugget_core::ids::{ItemKey, ItemRef, SourceId};
    use nugget_store::{Chunk, Item};
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;
    use crate::config::KnowledgeConfig;

    const BODY: &str = "# Note\n\nSome text about #rowing.";

    fn item(key: &str, frontmatter: Value) -> (Item, Vec<Chunk>) {
        let r = ItemRef {
            source: SourceId::new("notes"),
            key: ItemKey::new(key),
        };
        let item = Item {
            item: r.clone(),
            title: Some(file_stem(key)),
            content_hash: "h".into(),
            mime: Some("text/markdown".into()),
            text: Some(BODY.into()),
            metadata: json!({
                "tags": ["rowing", "budget"],
                "extracted": { "frontmatter": frontmatter },
            }),
            source_modified_at: None,
            version: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        };
        let chunks = vec![Chunk {
            id: 1,
            item: r,
            ord: 0,
            text: BODY.into(),
            title: None,
        }];
        (item, chunks)
    }

    fn run(key: &str, frontmatter: Value) -> Extraction {
        let (item, chunks) = item(key, frontmatter);
        let me = HashSet::new();
        let cfg = KnowledgeConfig::default();
        let ctx = ItemCtx {
            item: &item,
            chunks: &chunks,
            me_emails: &me,
            me_label: "Me",
            cfg: &cfg,
        };
        let mut ex = Extraction::default();
        extract(&ctx, &mut ex);
        ex
    }

    /// `(subject, predicate, object, quote)` of every entity-valued claim.
    fn triples(ex: &Extraction) -> Vec<(String, String, String, Option<String>)> {
        ex.claims
            .iter()
            .filter_map(|c| match &c.object {
                OSpec::Entity(o) => Some((
                    ex.name_of(c.subject, "Me"),
                    c.predicate.clone(),
                    ex.name_of(*o, "Me"),
                    c.evidence.quote.clone(),
                )),
                OSpec::Literal(_) => None,
            })
            .collect()
    }

    /// The `key: value` lines of the frontmatter as stored (the bench's notion of field text).
    fn field_lines(fm: &Value) -> Vec<String> {
        let mut out = Vec::new();
        for (k, v) in fm.as_object().unwrap() {
            for s in strings(Some(v)) {
                out.push(format!("{k}: {s}"));
            }
        }
        out
    }

    fn assert_quotes_are_source_text(ex: &Extraction, fm: &Value) {
        let lines = field_lines(fm);
        for c in &ex.claims {
            let Some(q) = c.evidence.quote.as_deref() else {
                continue;
            };
            let in_body = c.evidence.chunk_id.is_some() && BODY.contains(q);
            assert!(
                in_body || lines.iter().any(|l| l == q),
                "quote {q:?} of {} is not source text",
                c.text
            );
        }
    }

    fn t(s: &str, p: &str, o: &str, q: &str) -> (String, String, String, Option<String>) {
        (s.into(), p.into(), o.into(), Some(q.into()))
    }

    #[test]
    fn project_owner_and_members_quote_their_own_lines() {
        let fm = json!({
            "type": "project",
            "org": "[[Glimmerfen Labs]]",
            "owner": "[[Felix Ravensworth]]",
            "members": ["[[Ada Wexford]]", "[[Robin Ashgrove|Robin]]"],
            "tags": ["budget"],
        });
        let ex = run("projects/Driftwood API.md", fm.clone());
        let got = triples(&ex);
        for want in [
            t(
                "Felix Ravensworth",
                "leads",
                "Driftwood API",
                "owner: [[Felix Ravensworth]]",
            ),
            t(
                "Ada Wexford",
                "works_on",
                "Driftwood API",
                "members: [[Ada Wexford]]",
            ),
            t(
                "Robin Ashgrove",
                "works_on",
                "Driftwood API",
                "members: [[Robin Ashgrove|Robin]]",
            ),
            t(
                "Driftwood API",
                "part_of",
                "Glimmerfen Labs",
                "org: [[Glimmerfen Labs]]",
            ),
            t("Driftwood API", "has_topic", "budget", "tags: budget"),
        ] {
            assert!(got.contains(&want), "missing {want:?} in {got:#?}");
        }
        assert!(
            !got.iter()
                .any(|(_, _, _, q)| q.as_deref().is_some_and(|q| q.starts_with("people:"))),
            "{got:#?}"
        );
        assert_quotes_are_source_text(&ex, &fm);
    }

    #[test]
    fn person_org_club_and_manager() {
        let fm = json!({
            "type": "person",
            "email": ["robin@ashgrove.test"],
            "org": "[[Glimmerfen Labs]]",
            "club": "[[Marrowbank Rowing Club]]",
            "reports_to": "[[Theo Brannigan]]",
            "role": "Product Lead",
        });
        let ex = run("people/Robin Ashgrove.md", fm.clone());
        let got = triples(&ex);
        for want in [
            t(
                "Robin Ashgrove",
                "works_at",
                "Glimmerfen Labs",
                "org: [[Glimmerfen Labs]]",
            ),
            t(
                "Robin Ashgrove",
                "member_of",
                "Marrowbank Rowing Club",
                "club: [[Marrowbank Rowing Club]]",
            ),
            t(
                "Robin Ashgrove",
                "reports_to",
                "Theo Brannigan",
                "reports_to: [[Theo Brannigan]]",
            ),
        ] {
            assert!(got.contains(&want), "missing {want:?} in {got:#?}");
        }
        assert_quotes_are_source_text(&ex, &fm);
    }

    #[test]
    fn every_key_variant_quotes_the_key_it_was_read_from() {
        let fm = json!({
            "type": "project",
            "Company": "Acme",
            "Owners": ["Dana Scully"],
            "people": ["Fox Mulder"],
            "projects": ["[[Apollo]]"],
            "topic": "#space",
        });
        let ex = run("projects/Gemini.md", fm.clone());
        let got = triples(&ex);
        for want in [
            t("Gemini", "part_of", "Acme", "Company: Acme"),
            t("Dana Scully", "leads", "Gemini", "Owners: Dana Scully"),
            t("Fox Mulder", "works_on", "Gemini", "people: Fox Mulder"),
            t("Gemini", "part_of", "Apollo", "projects: [[Apollo]]"),
            t("Gemini", "has_topic", "space", "topic: #space"),
        ] {
            assert!(got.contains(&want), "missing {want:?} in {got:#?}");
        }
        assert_quotes_are_source_text(&ex, &fm);
    }

    #[test]
    fn untyped_note_quotes_people_lines() {
        let fm = json!({
            "project": "[[Harbourline]]",
            "owner": "[[Robin Ashgrove]]",
            "with": ["Sam Okafor"],
        });
        let ex = run("journal/Standup.md", fm.clone());
        let got = triples(&ex);
        for want in [
            t(
                "Robin Ashgrove",
                "leads",
                "Harbourline",
                "owner: [[Robin Ashgrove]]",
            ),
            t("Sam Okafor", "works_on", "Harbourline", "with: Sam Okafor"),
            // Body tag: quoted from the chunk.
            t(
                "Harbourline",
                "has_topic",
                "rowing",
                "Some text about #rowing.",
            ),
        ] {
            assert!(got.contains(&want), "missing {want:?} in {got:#?}");
        }
        // A tag found neither in the body nor in the frontmatter carries no invented quote.
        assert!(
            got.contains(&(
                "Harbourline".into(),
                "has_topic".into(),
                "budget".into(),
                None
            )),
            "{got:#?}"
        );
        assert_quotes_are_source_text(&ex, &fm);
    }
}
