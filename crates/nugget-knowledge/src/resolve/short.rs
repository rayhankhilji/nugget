//! Short names of people: "Maggie", "M. Thistlewood", "Maggie T", "Bob Chen", or an address
//! like `mthistlewood@` with no display name, joined to the one full-name person they are short
//! for. A name match ([`crate::names`]) is only ever a precondition: the join also needs
//! corroboration, and exactly one corroborated candidate.
//!
//! **Candidates** are people (never the user) with a written-out given name and a surname that
//! the short side may be short for ([`crate::names::compatible`], or an address whose local part
//! spells the full name), with no conflicting strong identifier and no user split between them.
//!
//! **Corroboration** (at least one):
//! - `shared_email` / `shared_identifier`: an identifier both hold;
//! - `same_item`: the full-name person appears on an item the short side appears on (headers,
//!   attendees, frontmatter, other mentions of the item);
//! - `same_thread`: an item of each is in one mail thread (shared Message-ID, In-Reply-To or
//!   References); items without those headers give no thread signal (subjects are not used);
//! - `same_org`: the short side's address domain is one of the person's (non-webmail) mail
//!   domains, or an item of the short side mentions one of the person's organisations
//!   (`works_at`, or the organisation of their mail domain), or a member of one (another person
//!   who `works_at` it), or the short side itself `works_at` one of them.
//!
//! **Order independence.** Both directions apply the same decision ([`Resolver::full_for`]):
//! a short-form occurrence joins its full-name person when it is resolved (forward), and when a
//! full-name person is resolved, existing short-form people compatible with it are re-examined
//! and absorbed when it is *their* one corroborated candidate (reverse). The current item's
//! people are known from its extraction ([`ItemContext`]) before its mentions are written, so
//! both directions see the same evidence whichever item comes first. Corroboration a later
//! item supplies indirectly (a co-mentioned colleague's employer) is caught by an end-of-run
//! pass over the short-form people still on their own ([`Resolver::settle_pending`]).
//!
//! **Merge records** ([`MergeRecord`]) are indexed by the item they cite; when that item is
//! retracted, the records citing it are dropped ([`forget_item`]) and the merge stays. Their
//! `detail` names nobody and quotes nothing from the source ([`describe`]).

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use nugget_core::Result;
use nugget_core::ids::{EntityId, ItemRef};
use nugget_graph::{
    ClaimObject, Entity, EntityKind, EntityQuery, Identifier, MERGE_LOG_ATTR, MergeRecord,
    merge_log, normalize_name,
};
use nugget_store::Item;
use serde_json::{Value, json};

use super::{Resolver, has_strong, identifiers_conflict, is_strong};
use crate::feed::ItemFeed;
use crate::names::{LocalMatch, LocalPart, PersonName, compatible, given_name_variants};
use crate::spec::{EntitySpec, Extraction};

/// People scanned per name lookup.
const CAND_SCAN: usize = 200;
/// Mentions read per entity when looking for shared items.
const MENTION_SCAN: usize = 1_000;
/// Items per side whose thread headers are read.
const THREAD_SCAN: usize = 50;
/// Members of an organisation checked for a shared item.
const MEMBER_SCAN: usize = 100;
/// State key of the short-form people not placed yet ([`Resolver::settle_pending`]).
pub(crate) const PENDING_KEY: &str = "knowledge/short_names";
/// Short-form people remembered at most (oldest dropped).
const MAX_PENDING: usize = 5_000;
/// Merge records kept per entity (oldest dropped).
pub(crate) const MAX_MERGE_LOG: usize = 50;

/// What the item being processed says about who is on it, for corroboration.
pub(crate) struct ItemContext<'a> {
    pub item: ItemRef,
    feed: Option<&'a dyn ItemFeed>,
    threads: Vec<String>,
    /// People on the item: normalised names (with a written-out given name and a surname) and
    /// identifiers.
    people: Vec<(Vec<String>, Vec<Identifier>)>,
    /// Organisations on the item: normalised names and identifiers.
    orgs: Vec<(Vec<String>, Vec<Identifier>)>,
    /// Entities resolved from this item so far (their mentions are not written yet).
    resolved: Mutex<Vec<EntityId>>,
    threads_of: Mutex<HashMap<ItemRef, Vec<String>>>,
}

impl<'a> ItemContext<'a> {
    pub fn new(item: &Item, ex: &Extraction, feed: Option<&'a dyn ItemFeed>) -> Self {
        let mut people = Vec::new();
        let mut orgs = Vec::new();
        for e in &ex.entities {
            let names: Vec<String> = (!e.weak_name)
                .then_some(&e.name)
                .into_iter()
                .chain(&e.aliases)
                .map(|n| normalize_name(n))
                .filter(|n| !n.is_empty())
                .collect();
            match e.kind {
                EntityKind::Person => {
                    let full: Vec<String> = names
                        .into_iter()
                        .filter(|n| {
                            PersonName::parse(n)
                                .is_some_and(|p| p.given_full().is_some() && p.surname.is_some())
                        })
                        .collect();
                    people.push((full, e.identifiers.clone()));
                }
                EntityKind::Organization => orgs.push((names, e.identifiers.clone())),
                _ => {}
            }
        }
        Self {
            item: item.item.clone(),
            feed,
            threads: thread_keys(item),
            people,
            orgs,
            resolved: Mutex::new(Vec::new()),
            threads_of: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn feed(&self) -> Option<&'a dyn ItemFeed> {
        self.feed
    }

    pub(super) fn note_resolved(&self, id: &EntityId) {
        let mut r = self.resolved.lock().unwrap_or_else(|p| p.into_inner());
        if !r.contains(id) {
            r.push(id.clone());
        }
    }

    fn resolved(&self) -> Vec<EntityId> {
        self.resolved
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

/// The mail thread keys of an item: its Message-ID, In-Reply-To and References, normalised
/// (angle brackets dropped, lower case). Empty for items that are not mail.
pub(crate) fn thread_keys(item: &Item) -> Vec<String> {
    let Some(ex) = item.metadata.get("extracted") else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for key in ["message_id", "in_reply_to", "references"] {
        for s in crate::extract::strings(ex.get(key)) {
            for id in s.split([' ', ',', '\t', '\n']) {
                let id = id.trim().trim_matches(['<', '>']).to_lowercase();
                if id.len() >= 3 && id.len() <= 512 && !out.contains(&id) {
                    out.push(id);
                }
            }
        }
    }
    out
}

/// The short side of a possible join: an occurrence being resolved, or an existing entity.
pub(super) struct Short<'s> {
    /// Display names (not addresses).
    pub names: Vec<String>,
    /// Local parts of its addresses, used when it has no name.
    pub locals: Vec<LocalPart>,
    pub identifiers: Vec<Identifier>,
    pub entity: Option<&'s Entity>,
    /// It is on the item being processed.
    pub on_current: bool,
}

impl<'s> Short<'s> {
    pub fn of_spec(spec: &EntitySpec, on_current: bool) -> Self {
        let mut names: Vec<String> = Vec::new();
        if !spec.weak_name && !super::looks_like_address(&spec.name) {
            names.push(spec.name.clone());
        }
        names.extend(
            spec.aliases
                .iter()
                .filter(|a| !super::looks_like_address(a))
                .cloned(),
        );
        let locals = if names.is_empty() {
            emails(&spec.identifiers)
                .filter_map(LocalPart::parse)
                .collect()
        } else {
            Vec::new()
        };
        Self {
            names,
            locals,
            identifiers: spec.identifiers.clone(),
            entity: None,
            on_current,
        }
    }

    pub fn of_entity(e: &'s Entity, on_current: bool) -> Self {
        let names: Vec<String> = std::iter::once(&e.name)
            .chain(&e.aliases)
            .filter(|n| !super::looks_like_address(n) && !normalize_name(n).is_empty())
            .cloned()
            .collect();
        let locals = if names.is_empty() {
            emails(&e.identifiers)
                .filter_map(LocalPart::parse)
                .collect()
        } else {
            Vec::new()
        };
        Self {
            names,
            locals,
            identifiers: e.identifiers.clone(),
            entity: Some(e),
            on_current,
        }
    }

    /// Whether every name is a short form (one word, an initial): the side is worth trying
    /// to place even when it was found by an exact name or identifier.
    pub fn all_short(&self) -> bool {
        (!self.names.is_empty() || !self.locals.is_empty())
            && self
                .names
                .iter()
                .all(|n| PersonName::parse(n).is_some_and(|p| !p.is_complete()))
    }
}

fn emails(ids: &[Identifier]) -> impl Iterator<Item = &str> {
    ids.iter()
        .filter(|i| i.scheme == "email")
        .map(|i| i.value.as_str())
}

/// The one full-name person a short side belongs to, and why.
pub(super) struct Pick {
    pub entity: Entity,
    pub reasons: Vec<String>,
    pub detail: String,
    pub item: Option<ItemRef>,
}

/// Names a full-name person goes by that other forms can be short for.
fn full_names(e: &Entity) -> Vec<String> {
    std::iter::once(&e.name)
        .chain(&e.aliases)
        .filter(|n| {
            PersonName::parse(n).is_some_and(|p| p.given_full().is_some() && p.surname.is_some())
        })
        .cloned()
        .collect()
}

fn is_me_attr(e: &Entity) -> bool {
    e.attributes.get("is_me") == Some(&Value::Bool(true))
}

fn push_reason(v: &mut Vec<String>, r: &str) {
    if !v.iter().any(|x| x == r) {
        v.push(r.to_owned());
    }
}

/// One short sentence explaining a join from its reason codes. It names nobody and quotes
/// nothing from the source (the vault shows it next to the record's `name`).
pub(crate) fn describe(reasons: &[String]) -> String {
    let how: Vec<&str> = reasons
        .iter()
        .filter_map(|r| match r.as_str() {
            "first_name" => Some("first name"),
            "nickname" => Some("nickname"),
            "initial" => Some("initials"),
            "email_local_part" => Some("address spelling the name"),
            "same_name" => Some("same name"),
            _ => None,
        })
        .collect();
    let why: Vec<&str> = reasons
        .iter()
        .filter_map(|r| match r.as_str() {
            "shared_email" => Some("shared email address"),
            "shared_identifier" => Some("shared identifier"),
            "same_item" => Some("on the same item"),
            "same_thread" => Some("in the same mail thread"),
            "same_org" => Some("same organisation"),
            _ => None,
        })
        .collect();
    match (how.is_empty(), why.is_empty()) {
        (false, false) => format!("Matched by {}; {}.", how.join(" + "), why.join(", ")),
        (false, true) => format!("Matched by {}.", how.join(" + ")),
        (true, false) => format!("Joined: {}.", why.join(", ")),
        (true, true) => "Joined.".to_string(),
    }
}

impl Resolver<'_> {
    fn is_me(&self, e: &Entity) -> bool {
        Some(&e.id) == self.me.as_ref() || is_me_attr(e)
    }

    /// Items an entity is mentioned on, plus the current item when it was resolved from it.
    fn items_of(&self, e: &Entity, on_current: bool) -> Result<Vec<ItemRef>> {
        let mut out: Vec<ItemRef> = Vec::new();
        for m in self.graph.mentions_for(&e.id, MENTION_SCAN)? {
            if !out.contains(&m.item) {
                out.push(m.item);
            }
        }
        if on_current
            && let Some(c) = self.ctx
            && !out.contains(&c.item)
        {
            out.push(c.item.clone());
        }
        Ok(out)
    }

    fn threads_of(&self, items: &[ItemRef]) -> Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        let Some(ctx) = self.ctx else {
            return Ok(out);
        };
        for item in items.iter().rev().take(THREAD_SCAN) {
            if *item == ctx.item {
                out.extend(ctx.threads.iter().cloned());
                continue;
            }
            let cached = ctx
                .threads_of
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(item)
                .cloned();
            let keys = match cached {
                Some(k) => k,
                None => {
                    let k = match ctx.feed {
                        Some(f) => f
                            .get_item(item)?
                            .map(|i| thread_keys(&i))
                            .unwrap_or_default(),
                        None => Vec::new(),
                    };
                    let mut cache = ctx.threads_of.lock().unwrap_or_else(|p| p.into_inner());
                    if cache.len() > 10_000 {
                        cache.clear();
                    }
                    cache.insert(item.clone(), k.clone());
                    k
                }
            };
            out.extend(keys);
        }
        Ok(out)
    }

    /// Registrable, non-webmail domains of a set of identifiers.
    fn org_domains(&self, ids: &[Identifier]) -> BTreeSet<String> {
        emails(ids)
            .filter_map(crate::text::domain_of)
            .filter(|d| !crate::text::is_freemail(d, &self.cfg.extra_freemail_domains))
            .map(|d| crate::text::registrable_domain(&d))
            .chain(
                ids.iter()
                    .filter(|i| i.scheme == "domain")
                    .map(|i| crate::text::registrable_domain(&i.value)),
            )
            .collect()
    }

    /// Organisations an entity `works_at` (active claims).
    fn employers(&self, e: &Entity) -> Result<BTreeSet<EntityId>> {
        let mut out = BTreeSet::new();
        for c in self.graph.claims_for(&e.id, false)? {
            if c.subject == e.id
                && c.predicate == "works_at"
                && c.status.is_active()
                && let ClaimObject::Entity(o) = &c.object
                && let Some(org) = self.canonical(o)?
            {
                out.insert(org.id);
            }
        }
        Ok(out)
    }

    /// A full-name person's organisations: employers, and the organisations of their mail
    /// domains.
    fn orgs_of(&self, f: &Entity, domains: &BTreeSet<String>) -> Result<BTreeSet<EntityId>> {
        let mut out = self.employers(f)?;
        for d in domains {
            if let Some(id) = self
                .graph
                .find_by_identifier(&Identifier::new("domain", d.as_str()))?
                && let Some(o) = self.canonical(&id)?
                && o.kind == EntityKind::Organization
            {
                out.insert(o.id);
            }
        }
        Ok(out)
    }

    /// The live entity of a roster entry (by identifier, then by name).
    fn roster_entity(
        &self,
        kind: EntityKind,
        names: &[String],
        ids: &[Identifier],
    ) -> Result<Vec<Entity>> {
        let mut out: Vec<Entity> = Vec::new();
        for i in ids {
            if let Some(id) = self.graph.find_by_identifier(i)?
                && let Some(e) = self.canonical(&id)?
                && e.kind == kind
                && !out.iter().any(|o| o.id == e.id)
            {
                out.push(e);
            }
        }
        if out.is_empty() {
            for n in names {
                for id in self.graph.find_by_name(n, Some(kind))? {
                    if let Some(e) = self.canonical(&id)?
                        && !out.iter().any(|o| o.id == e.id)
                    {
                        out.push(e);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Why the short side and `f` are the same person, beyond their names (empty: nothing
    /// corroborates it), and the item that shows it.
    fn corroborate(
        &self,
        short: &Short<'_>,
        f: &Entity,
        f_on_current: bool,
    ) -> Result<(Vec<String>, Option<ItemRef>)> {
        let mut why: Vec<String> = Vec::new();
        let mut item: Option<ItemRef> = None;
        let current = self.ctx.map(|c| c.item.clone());
        // Shared identifiers.
        for i in short.identifiers.iter().filter(|i| is_strong(&i.scheme)) {
            if f.identifiers.contains(i) {
                push_reason(
                    &mut why,
                    if i.scheme == "email" {
                        "shared_email"
                    } else {
                        "shared_identifier"
                    },
                );
            }
        }
        // Same item.
        let s_items: Vec<ItemRef> = match short.entity {
            Some(e) => self.items_of(e, short.on_current)?,
            None => current
                .iter()
                .filter(|_| short.on_current)
                .cloned()
                .collect(),
        };
        let f_items = self.items_of(f, f_on_current)?;
        let mut on_item = s_items.iter().find(|i| f_items.contains(i)).cloned();
        if on_item.is_none()
            && short.on_current
            && let Some(ctx) = self.ctx
        {
            let f_names: Vec<String> = full_names(f).iter().map(|n| normalize_name(n)).collect();
            for (names, ids) in &ctx.people {
                let by_id = ids.iter().any(|i| f.identifiers.contains(i));
                let by_name = names.iter().any(|n| f_names.contains(n));
                if by_id || by_name {
                    on_item = Some(ctx.item.clone());
                    break;
                }
            }
        }
        if let Some(i) = on_item {
            push_reason(&mut why, "same_item");
            item = Some(i);
        }
        // Same thread.
        let s_threads = self.threads_of(&s_items)?;
        if !s_threads.is_empty() {
            let f_threads = self.threads_of(&f_items)?;
            if !s_threads.is_disjoint(&f_threads) {
                push_reason(&mut why, "same_thread");
            }
        }
        // Same organisation.
        let f_domains = self.org_domains(&f.identifiers);
        let f_orgs = self.orgs_of(f, &f_domains)?;
        let mut same_org = !self.org_domains(&short.identifiers).is_disjoint(&f_domains);
        if !same_org && let Some(e) = short.entity {
            same_org = !self.employers(e)?.is_disjoint(&f_orgs);
        }
        if !same_org
            && short.on_current
            && let Some(ctx) = self.ctx
        {
            'roster: for (names, ids) in &ctx.orgs {
                if !self.org_domains(ids).is_disjoint(&f_domains) {
                    same_org = true;
                    break;
                }
                for o in self.roster_entity(EntityKind::Organization, names, ids)? {
                    if f_orgs.contains(&o.id) {
                        same_org = true;
                        break 'roster;
                    }
                }
            }
            if !same_org {
                'people: for (names, ids) in &ctx.people {
                    let found = self.roster_entity(EntityKind::Person, names, ids)?;
                    // The user is on everything; `f` itself counts as the same item.
                    if found.iter().any(|p| self.is_me(p) || p.id == f.id)
                        || ids.iter().any(|i| f.identifiers.contains(i))
                    {
                        continue;
                    }
                    if !self.org_domains(ids).is_disjoint(&f_domains) {
                        same_org = true;
                        break;
                    }
                    for p in found {
                        if p.id != f.id && !self.employers(&p)?.is_disjoint(&f_orgs) {
                            same_org = true;
                            break 'people;
                        }
                    }
                }
            }
        }
        if !same_org && let Some(s) = short.entity {
            let s_old: std::collections::HashSet<ItemRef> = self
                .graph
                .mentions_for(&s.id, MENTION_SCAN)?
                .into_iter()
                .map(|m| m.item)
                .collect();
            if !s_old.is_empty() {
                'orgs: for o in &f_orgs {
                    let mut related: Vec<EntityId> = vec![o.clone()];
                    for c in self.graph.claims_for(o, false)?.into_iter() {
                        if related.len() > MEMBER_SCAN {
                            break;
                        }
                        if c.predicate == "works_at"
                            && c.status.is_active()
                            && c.object == ClaimObject::Entity(o.clone())
                            && c.subject != s.id
                            && c.subject != f.id
                            && Some(&c.subject) != self.me.as_ref()
                        {
                            related.push(c.subject);
                        }
                    }
                    for r in related {
                        if self
                            .graph
                            .mentions_for(&r, MENTION_SCAN)?
                            .iter()
                            .any(|m| s_old.contains(&m.item))
                        {
                            same_org = true;
                            break 'orgs;
                        }
                    }
                }
            }
        }
        if same_org {
            push_reason(&mut why, "same_org");
        }
        if item.is_none() && !why.is_empty() {
            item = if short.on_current {
                current
            } else {
                s_items.iter().next_back().cloned()
            };
        }
        Ok((why, item))
    }

    /// Why the short side's names fit `f` (empty: they do not).
    fn name_fit(short: &Short<'_>, f: &Entity) -> Vec<String> {
        let fulls = full_names(f);
        let mut reasons: Vec<String> = Vec::new();
        for s in &short.names {
            for full in &fulls {
                if let Some(m) = compatible(s, full) {
                    for r in m.reasons {
                        push_reason(&mut reasons, r);
                    }
                }
            }
        }
        if reasons.is_empty() {
            for l in &short.locals {
                if fulls.iter().any(|n| l.matches(n) == Some(LocalMatch::Full)) {
                    push_reason(&mut reasons, "email_local_part");
                }
            }
        } else {
            // An address of the short side, or of `f`, spelling the other's name.
            let s_local = emails(&short.identifiers)
                .filter_map(LocalPart::parse)
                .any(|l| fulls.iter().any(|n| l.matches(n).is_some()));
            let f_local = emails(&f.identifiers)
                .filter_map(LocalPart::parse)
                .any(|l| short.names.iter().any(|n| l.matches(n).is_some()));
            if s_local || f_local {
                push_reason(&mut reasons, "email_local_part");
            }
        }
        reasons
    }

    /// Text to search the graph for full-name candidates of the short side.
    fn needles(short: &Short<'_>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in &short.names {
            let Some(p) = PersonName::parse(n) else {
                continue;
            };
            match (p.given_full(), p.surname_full()) {
                (_, Some(s)) if p.given_full().is_none() => out.push(s.to_owned()),
                (Some(g), _) => out.extend(given_name_variants(g)),
                _ => {}
            }
        }
        for l in &short.locals {
            for w in &l.words {
                if w.chars().count() >= 3 {
                    out.push(w.clone());
                }
            }
            if let Some(h) = l.hints()
                && let Some(g) = h.given_full()
            {
                out.extend(given_name_variants(g));
            }
        }
        out.retain(|n| n.chars().count() >= 2);
        out.sort();
        out.dedup();
        out
    }

    /// The decision both directions share: the one full-name person the short side may be
    /// short for **and** that something corroborates. `None` when there is none or more than
    /// one. `f_current`: an entity known to be on the current item (being resolved from it).
    pub(super) fn full_for(
        &self,
        short: &Short<'_>,
        f_current: Option<&EntityId>,
    ) -> Result<Option<Pick>> {
        if short.names.is_empty() && short.locals.is_empty() {
            return Ok(None);
        }
        let live = self.live()?;
        let resolved = self.ctx.map(ItemContext::resolved).unwrap_or_default();
        let mut seen: BTreeSet<EntityId> = BTreeSet::new();
        let mut picks: Vec<Pick> = Vec::new();
        for needle in Self::needles(short) {
            for s in self.graph.list_entities(&EntityQuery {
                text: Some(needle),
                kind: Some(EntityKind::Person),
                limit: CAND_SCAN,
                offset: 0,
            })? {
                let Some(f) = self.canonical(&s.id)? else {
                    continue;
                };
                if !seen.insert(f.id.clone())
                    || f.kind != EntityKind::Person
                    || self.is_me(&f)
                    || short.entity.is_some_and(|e| e.id == f.id)
                    || identifiers_conflict(&short.identifiers, &f.identifiers)
                    || short
                        .entity
                        .is_some_and(|e| crate::splits::apart(&live, &e.id, &f.id))
                {
                    continue;
                }
                let names = Self::name_fit(short, &f);
                if names.is_empty() {
                    continue;
                }
                let on_current = f_current == Some(&f.id) || resolved.contains(&f.id);
                let (why, item) = self.corroborate(short, &f, on_current)?;
                if why.is_empty() {
                    continue;
                }
                let mut reasons = names;
                for w in why {
                    push_reason(&mut reasons, &w);
                }
                let detail = describe(&reasons);
                picks.push(Pick {
                    entity: f,
                    reasons,
                    detail,
                    item,
                });
                if picks.len() > 1 {
                    return Ok(None); // ambiguous (two Sams)
                }
            }
        }
        Ok(picks.pop())
    }

    /// Short-form people already in the graph that `f` may be the full name of: lookups of
    /// the forms its names can be shortened to.
    fn short_forms_of(&self, f: &Entity) -> Result<Vec<Entity>> {
        let mut forms: BTreeSet<String> = BTreeSet::new();
        for n in full_names(f) {
            let Some(p) = PersonName::parse(&n) else {
                continue;
            };
            let Some(g) = p.given_full() else { continue };
            let si = p.surname.as_ref().and_then(|s| match s {
                crate::names::Part::Full(x) => x.chars().next(),
                crate::names::Part::Initial(c) => Some(*c),
            });
            let sur = p.surname_full();
            for v in given_name_variants(g) {
                forms.insert(v.clone());
                if let Some(c) = si {
                    forms.insert(format!("{v} {c}"));
                    forms.insert(format!("{v} {c}."));
                }
                if let Some(s) = sur
                    && v != g
                {
                    forms.insert(format!("{v} {s}"));
                }
            }
            if let (Some(s), Some(gi)) = (sur, g.chars().next()) {
                forms.insert(format!("{gi}. {s}"));
                forms.insert(format!("{gi} {s}"));
            }
        }
        let mut out: Vec<Entity> = Vec::new();
        for form in forms {
            for id in self.graph.find_by_name(&form, Some(EntityKind::Person))? {
                if let Some(e) = self.canonical(&id)?
                    && e.id != f.id
                    && !self.is_me(&e)
                    && !out.iter().any(|o| o.id == e.id)
                    && !identifiers_conflict(&e.identifiers, &f.identifiers)
                {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }

    /// Reverse direction: absorb into `f` the short-form people for whom `f` is the one
    /// corroborated full name. Returns the merged-away ids.
    pub(super) fn absorb_short_forms(&self, f_id: &EntityId) -> Result<Vec<EntityId>> {
        let Some(f) = self.canonical(f_id)? else {
            return Ok(Vec::new());
        };
        if f.kind != EntityKind::Person || self.is_me(&f) || full_names(&f).is_empty() {
            return Ok(Vec::new());
        }
        let resolved = self.ctx.map(ItemContext::resolved).unwrap_or_default();
        let mut merged = Vec::new();
        for s in self.short_forms_of(&f)? {
            let short = Short::of_entity(&s, resolved.contains(&s.id));
            let Some(pick) = self.full_for(&short, Some(&f.id))? else {
                continue;
            };
            if pick.entity.id != f.id {
                continue;
            }
            if self.merge(
                &f.id,
                &s.id,
                pick.reasons.clone(),
                pick.detail.clone(),
                pick.item.clone(),
            )? {
                merged.push(s.id.clone());
            }
        }
        Ok(merged)
    }

    /// The end-of-run pass. Short-form people left on their own are remembered (under
    /// [`PENDING_KEY`]); once a run's writes are all applied, each one that the run may have
    /// changed the evidence for (it was touched, a touched entity shares one of its items, or
    /// a touched person has a name it may be short for) is placed again with the same decision
    /// as during resolution. This catches corroboration that only a later item supplies, such
    /// as a co-mentioned colleague's employer, so the outcome does not depend on item order.
    /// Returns the merges done, as `(into, from)`.
    pub(crate) fn settle_pending(
        &self,
        feed: &dyn ItemFeed,
        touched: &BTreeSet<EntityId>,
    ) -> Result<Vec<(EntityId, EntityId)>> {
        let mut pending: Vec<EntityId> = match feed.get_state(PENDING_KEY)? {
            Some(v) => serde_json::from_value(v).unwrap_or_default(),
            None => Vec::new(),
        };
        let mut touched_items: std::collections::HashSet<ItemRef> =
            std::collections::HashSet::new();
        let mut touched_people: Vec<Entity> = Vec::new();
        for t in touched {
            let Some(e) = self.canonical(t)? else {
                continue;
            };
            if e.kind == EntityKind::Person && !self.is_me(&e) {
                if Short::of_entity(&e, false).all_short() {
                    pending.push(e.id.clone());
                } else if !full_names(&e).is_empty() {
                    touched_people.push(e.clone());
                }
            }
            for m in self.graph.mentions_for(&e.id, MENTION_SCAN)? {
                touched_items.insert(m.item);
            }
        }
        let mut keep: Vec<EntityId> = Vec::new();
        let mut merges = Vec::new();
        let mut seen: BTreeSet<EntityId> = BTreeSet::new();
        for id in pending {
            let Some(s) = self.canonical(&id)? else {
                continue;
            };
            if !seen.insert(s.id.clone())
                || s.kind != EntityKind::Person
                || self.is_me(&s)
                || !Short::of_entity(&s, false).all_short()
            {
                continue;
            }
            let short = Short::of_entity(&s, false);
            let relevant = touched.contains(&s.id)
                || touched.contains(&id)
                || self
                    .graph
                    .mentions_for(&s.id, MENTION_SCAN)?
                    .iter()
                    .any(|m| touched_items.contains(&m.item))
                || touched_people
                    .iter()
                    .any(|f| !Self::name_fit(&short, f).is_empty());
            if relevant
                && let Some(pick) = self.full_for(&short, None)?
                && self.merge(&pick.entity.id, &s.id, pick.reasons, pick.detail, pick.item)?
            {
                merges.push((pick.entity.id, s.id));
                continue;
            }
            keep.push(s.id);
        }
        let drop = keep.len().saturating_sub(MAX_PENDING);
        keep.drain(..drop);
        feed.set_state(PENDING_KEY, serde_json::to_value(&keep)?)?;
        Ok(merges)
    }

    /// Forward direction for an existing entity found by an exact name or identifier: when all
    /// its names are short forms, merge it into its one corroborated full-name person. Returns
    /// that person's id when merged.
    pub(super) fn settle_short(&self, e_id: &EntityId) -> Result<Option<EntityId>> {
        let Some(e) = self.canonical(e_id)? else {
            return Ok(None);
        };
        if e.kind != EntityKind::Person || self.is_me(&e) {
            return Ok(None);
        }
        let short = Short::of_entity(&e, true);
        if !short.all_short() {
            return Ok(None);
        }
        let Some(pick) = self.full_for(&short, None)? else {
            return Ok(None);
        };
        if self.merge(&pick.entity.id, &e.id, pick.reasons, pick.detail, pick.item)? {
            Ok(Some(pick.entity.id))
        } else {
            Ok(None)
        }
    }

    /// Append a merge record to `id`'s log (unless already there) and index it by its item.
    pub(super) fn record(&self, id: &EntityId, rec: MergeRecord) -> Result<bool> {
        let item = rec.item.clone();
        let added = append_record(self.graph, id, rec)?;
        if added {
            self.index_record(item.as_ref(), id)?;
        }
        Ok(added)
    }

    /// Remember that `id` holds a merge record citing `item` ([`forget_item`]).
    pub(super) fn index_record(&self, item: Option<&ItemRef>, id: &EntityId) -> Result<()> {
        if let (Some(feed), Some(item)) = (self.feed, item) {
            index_record(feed, item, id)?;
        }
        Ok(())
    }

    /// Record on `id` that occurrences were folded into it before resolution (no entity was
    /// merged).
    pub(super) fn record_folds(&self, id: &EntityId, spec: &EntitySpec) -> Result<bool> {
        let mut any = false;
        for f in &spec.folds {
            any |= self.record(
                id,
                MergeRecord {
                    from: None,
                    name: f.name.clone(),
                    reasons: f.reasons.clone(),
                    detail: f.detail.clone(),
                    item: self.item.cloned(),
                    at: Some(time::OffsetDateTime::now_utc()),
                },
            )?;
        }
        Ok(any)
    }
}

/// Whether a short-form occurrence should be tried against full names before a new entity is
/// made for it: a person with no strong identifier held by anyone yet (it found no entity).
pub(super) fn wants_full(spec: &EntitySpec) -> bool {
    spec.kind == EntityKind::Person && (!spec.weak_name || has_strong(&spec.identifiers))
}

/// Two records describe the same event (time aside).
fn same_record(a: &MergeRecord, b: &MergeRecord) -> bool {
    a.from == b.from && a.name == b.name && a.reasons == b.reasons && a.item == b.item
}

/// Write an entity's merge log: de-duplicated, oldest first, at most [`MAX_MERGE_LOG`].
pub(crate) fn write_log(
    graph: &dyn nugget_graph::GraphStore,
    id: &EntityId,
    log: Vec<MergeRecord>,
) -> Result<()> {
    let mut out: Vec<MergeRecord> = Vec::with_capacity(log.len());
    for r in log {
        if !out.iter().any(|x| same_record(x, &r)) {
            out.push(r);
        }
    }
    out.sort_by_key(|r| r.at);
    let drop = out.len().saturating_sub(MAX_MERGE_LOG);
    out.drain(..drop);
    let arr: Vec<Value> = out
        .iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .collect();
    graph.update_entity(
        id,
        &nugget_graph::EntityPatch {
            attributes: Some(json!({ MERGE_LOG_ATTR: arr })),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// Append one record to an entity's merge log unless the same one is there. Returns whether
/// it was added.
pub(crate) fn append_record(
    graph: &dyn nugget_graph::GraphStore,
    id: &EntityId,
    rec: MergeRecord,
) -> Result<bool> {
    let Some(e) = graph.get_entity(id)? else {
        return Ok(false);
    };
    let mut log = merge_log(&e);
    if log.iter().any(|x| same_record(x, &rec)) {
        return Ok(false);
    }
    log.push(rec);
    write_log(graph, &e.id, log)?;
    Ok(true)
}

/// Merge `from` into `into` and keep both merge logs plus a record of this merge on the
/// survivor (the graph keeps the survivor's attribute on a key conflict, so the union is
/// written after the merge).
pub(crate) fn merge_recorded(
    graph: &dyn nugget_graph::GraphStore,
    into: &EntityId,
    from: &EntityId,
    reasons: Vec<String>,
    detail: String,
    item: Option<ItemRef>,
) -> Result<()> {
    let a = graph.get_entity(into)?;
    let b = graph.get_entity(from)?;
    graph.merge(into, from)?;
    let mut log = a.as_ref().map(merge_log).unwrap_or_default();
    if let Some(b) = &b {
        log.extend(merge_log(b));
        log.push(MergeRecord {
            from: Some(b.id.clone()),
            name: b.name.clone(),
            reasons,
            detail,
            item,
            at: Some(time::OffsetDateTime::now_utc()),
        });
    }
    let survivor = a.map(|a| a.id).unwrap_or_else(|| into.clone());
    write_log(graph, &survivor, log)
}

/// State key of the index from items to the entities whose merge log cites them.
pub(crate) const MERGE_ITEMS_KEY: &str = "knowledge/merge_log_items";
/// Items indexed at most (oldest dropped; their records then outlive a retraction).
const MAX_INDEXED_ITEMS: usize = 20_000;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct IndexEntry {
    item: ItemRef,
    entities: Vec<EntityId>,
}

fn load_index(feed: &dyn ItemFeed) -> Result<Vec<IndexEntry>> {
    Ok(match feed.get_state(MERGE_ITEMS_KEY)? {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        None => Vec::new(),
    })
}

/// Index a merge record of `id` by the item it cites.
pub(crate) fn index_record(feed: &dyn ItemFeed, item: &ItemRef, id: &EntityId) -> Result<()> {
    let mut index = load_index(feed)?;
    match index.iter_mut().find(|e| &e.item == item) {
        Some(e) if e.entities.contains(id) => return Ok(()),
        Some(e) => e.entities.push(id.clone()),
        None => index.push(IndexEntry {
            item: item.clone(),
            entities: vec![id.clone()],
        }),
    }
    let drop = index.len().saturating_sub(MAX_INDEXED_ITEMS);
    index.drain(..drop);
    feed.set_state(MERGE_ITEMS_KEY, serde_json::to_value(&index)?)
}

/// An item was retracted (deleted, or changed and about to be extracted again): drop every
/// merge record citing it, so no record outlives its source. The merges themselves stay (a
/// retraction does not split entities); when the item comes back changed, resolving it again
/// writes the record again if it still corroborates the join. Returns the entities whose log
/// changed.
pub(crate) fn forget_item(
    graph: &dyn nugget_graph::GraphStore,
    feed: &dyn ItemFeed,
    item: &ItemRef,
) -> Result<Vec<EntityId>> {
    let mut index = load_index(feed)?;
    let Some(pos) = index.iter().position(|e| &e.item == item) else {
        return Ok(Vec::new());
    };
    let entry = index.remove(pos);
    let mut changed: Vec<EntityId> = Vec::new();
    for id in entry.entities {
        let Some(e) = graph.get_entity(&id)? else {
            continue;
        };
        if changed.contains(&e.id) {
            continue;
        }
        let log = merge_log(&e);
        let kept: Vec<MergeRecord> = log
            .iter()
            .filter(|r| r.item.as_ref() != Some(item))
            .cloned()
            .collect();
        if kept.len() != log.len() {
            write_log(graph, &e.id, kept)?;
            changed.push(e.id);
        }
    }
    feed.set_state(MERGE_ITEMS_KEY, serde_json::to_value(&index)?)?;
    Ok(changed)
}
