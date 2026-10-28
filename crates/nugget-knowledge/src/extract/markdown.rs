//! Markdown notes: frontmatter, `#tags` and `[[wikilinks]]`.
//!
//! - Tags and frontmatter `topic(s)` become topics; frontmatter `project(s)` become projects;
//!   `people` / `attendees` / `owner` become people; `company` / `organization` become
//!   organisations.
//! - A note whose frontmatter `type` (or `kind`) names an entity kind *is* that entity, named
//!   after the note's title.
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

/// Frontmatter list values, with `[[wikilink]]` brackets and `#` removed.
fn names(m: &Map<String, Value>, keys: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in strings(get_ci(m, keys)) {
        let v = v
            .trim()
            .trim_start_matches("[[")
            .trim_end_matches("]]")
            .trim_start_matches('#');
        let v = v.split('|').next().unwrap_or(v).trim();
        if !v.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(v)) {
            out.push(truncate_chars(v, 200));
        }
        if out.len() >= MAX_LIST {
            break;
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
    let fm_quote = |key: &str, value: &str| format!("{key}: {value}");

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
                if crate::text::is_email(&e) {
                    spec = spec.ident("email", &e);
                }
            }
            for p in names(fm, &["phone", "phones"]) {
                spec = spec.ident("phone", &p);
            }
        }
        if kind == EntityKind::Organization {
            for d in names(fm, &["domain", "website", "url"]) {
                if let Some(h) = host_of(&d) {
                    spec = spec.ident("domain", &crate::text::registrable_domain(&h));
                }
            }
        }
        ex.entity(spec)
    });

    let mut projects = Vec::new();
    for p in names(fm, &["project", "projects"]) {
        let r = ex.entity(EntitySpec::new(EntityKind::Project, p.clone()).mention(&p, None, 0.8));
        if Some(r) != note {
            projects.push((r, p));
        }
    }
    let mut topics = Vec::new();
    for t in names(fm, &["topic", "topics"]) {
        let r = ex.entity(EntitySpec::new(EntityKind::Topic, t.clone()).mention(&t, None, 0.8));
        if Some(r) != note {
            topics.push((r, t.clone(), ctx.evidence(None, &fm_quote("topics", &t))));
        }
    }
    let fm_topic_count = topics.len();
    for tag in strings(ctx.meta("tags")).into_iter().take(MAX_LIST) {
        let tag = tag.trim().trim_start_matches('#').to_owned();
        if tag.is_empty() {
            continue;
        }
        let ev = crate::text::locate(ctx.chunks, &format!("#{tag}"));
        let chunk = ev.as_ref().map(|(id, _)| *id);
        let r = ex.entity(
            EntitySpec::new(EntityKind::Topic, tag.clone())
                .ident("tag", &tag.to_lowercase())
                .mention(&format!("#{tag}"), chunk, 0.8),
        );
        let evidence = ctx.evidence(
            chunk,
            &ev.map_or_else(|| fm_quote("tags", &tag), |(_, line)| line),
        );
        if Some(r) != note && !topics.iter().any(|(x, _, _)| *x == r) {
            topics.push((r, tag, evidence));
        }
    }

    let mut people = Vec::new();
    for p in names(
        fm,
        &[
            "people",
            "person",
            "attendees",
            "participants",
            "with",
            "owner",
            "owners",
        ],
    ) {
        if let Some(name) = crate::text::clean_person_name(&p) {
            let r = ex.entity(
                EntitySpec::new(EntityKind::Person, name.clone()).mention(&name, None, 0.7),
            );
            if Some(r) != note {
                people.push((r, name));
            }
        }
    }
    let orgs: Vec<(ERef, String)> =
        names(fm, &["company", "organization", "organisation", "employer"])
            .into_iter()
            .map(|o| {
                (
                    ex.entity(
                        EntitySpec::new(EntityKind::Organization, o.clone()).mention(&o, None, 0.7),
                    ),
                    o,
                )
            })
            .filter(|(r, _)| Some(*r) != note)
            .collect();

    let ev = |q: &str| ctx.evidence(None, q);
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
            for (p, pn) in &projects {
                claim(
                    ex,
                    n,
                    "part_of",
                    *p,
                    format!("{nn} is part of {pn}."),
                    0.8,
                    ev(&fm_quote("project", pn)),
                );
            }
            for (p, pn) in &people {
                let (pred, text) = match kind {
                    EntityKind::Project => ("works_on", format!("{pn} works on {nn}.")),
                    EntityKind::Event => ("attended", format!("{pn} attended {nn}.")),
                    _ => ("related_to", format!("{pn} is related to {nn}.")),
                };
                claim(ex, *p, pred, n, text, 0.7, ev(&fm_quote("people", pn)));
            }
            for (o, on) in &orgs {
                if kind == EntityKind::Person {
                    claim(
                        ex,
                        n,
                        "works_at",
                        *o,
                        format!("{nn} works at {on}."),
                        0.8,
                        ev(&fm_quote("company", on)),
                    );
                } else {
                    claim(
                        ex,
                        n,
                        "related_to",
                        *o,
                        format!("{nn} is related to {on}."),
                        0.6,
                        ev(&fm_quote("organization", on)),
                    );
                }
            }
        }
        _ => {
            for (p, pn) in &projects {
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
                for (person, name) in &people {
                    claim(
                        ex,
                        *person,
                        "works_on",
                        *p,
                        format!("{name} works on {pn}."),
                        0.6,
                        ev(&fm_quote("people", name)),
                    );
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
