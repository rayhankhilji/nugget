//! Short names of people: "Maggie", "M. Thistlewood", "Maggie T", or an address like
//! `mthistlewood@` with no display name, joined to the one full-name person they are short for.
//! A name match ([`crate::names`]) is only ever a precondition: the join also needs
//! corroboration, and exactly one corroborated candidate.
//!
//! **Short side.** Only short forms are placed this way: every name one word or initialled
//! ([`crate::names::is_short_form`]), or no name but an address. A written-out given name and
//! surname ("Maggie Smith", "Nathan Lee") is a person of its own, never short for another full
//! name ("Margaret Smith", "Jonathan Lee"): only a shared strong identifier or a trusted record
//! joins two full names.
//!
//! **Candidates** are people with a written-out given name and a surname that the short side
//! may be short for ([`crate::names::compatible`], or an address whose local part spells the
//! full name). The search pages through every name match; past [`CAND_MAX`] rows the answer is
//! "ambiguous". The user is a candidate too, and a blocking one: a short name that fits the user
//! joins nobody. A candidate with a conflicting strong identifier, or split from the short side
//! by the user, cannot be picked (it still counts for uniqueness below).
//!
//! **Corroboration** (at least one):
//! - `shared_email` / `shared_identifier`: an identifier both hold;
//! - `same_item`: the full-name person appears on an item the short side appears on (headers,
//!   attendees, frontmatter, other occurrences of the item). Two *full* names on one item are
//!   two people; this only ever corroborates a short form;
//! - `same_thread`: an item of each is in one mail thread (shared Message-ID, In-Reply-To or
//!   References); items without those headers give no thread signal (subjects are not used);
//! - `same_org`: the short side's own address, or a header participant (From/To/Cc/Bcc) of one
//!   of its items, is at one of the person's mail domains, and no other name-compatible person
//!   is known at that domain. Webmail domains and the user's own domains (their addresses and
//!   employers) never count. In a body, an organisation counts only when the text attributes
//!   the short name to it ("Tamsin from Quillfeather", "at", "of"); merely named, it never does.
//!
//! **Order.** Both directions apply the same decision ([`Resolver::full_for`]): a short-form
//! occurrence joins when it is resolved (forward), and a full-name person absorbs existing short
//! forms for which it is the one corroborated candidate (reverse). During an item, a join is
//! made only when exactly one name-compatible candidate exists at all; otherwise the short form
//! gets an entity of its own and is decided at the end of the run ([`Resolver::settle_pending`]),
//! when every item of the run is applied. That pass also catches corroboration a later item
//! supplies. Joins are never undone: a competing candidate that only appears in a later run,
//! with corroboration of its own, does not split an earlier join (a known limit).
//!
//! **Job changes** ([`Resolver::settle_job_changes`]): the same full name under two conflicting
//! addresses is never merged on the name alone. It is merged (`same_name` + `job_change`) only
//! when an item *sent from* one address names the other address's organisation or domain (its
//! mail domain label, or the name of the organisation holding that domain), as Delphine
//! Corrigan's "my Vellum & Thistle address no longer works" from her new address does.
//!
//! **Merge records** ([`MergeRecord`]) cite the item that corroborated the join (same item,
//! else thread, else the item whose headers showed the organisation) and are indexed by it, one
//! state key per item; when the item is retracted the records citing it are dropped
//! ([`forget_item`]) and the merge stays. A user split drops the records of the merge it undid
//! ([`forget_split`]). `detail` names nobody and quotes nothing from the source ([`describe`]).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Mutex;

use nugget_core::Result;
use nugget_core::ids::{EntityId, ItemRef};
use nugget_graph::{
    Entity, EntityKind, EntityQuery, Identifier, MERGE_LOG_ATTR, MergeRecord, merge_log,
    normalize_name,
};
use nugget_store::Item;
use serde_json::{Value, json};

use super::{Resolver, identifiers_conflict, is_strong};
use crate::feed::ItemFeed;
use crate::names::{
    LocalMatch, LocalPart, PersonName, compatible, given_name_variants, is_short_form,
};
use crate::spec::{EntitySpec, Extraction};

/// People read per page of a name lookup.
const CAND_PAGE: usize = 200;
/// People read at most per name lookup; more means "ambiguous".
pub(crate) const CAND_MAX: usize = 2_000;
/// Mentions read per entity when looking for shared items.
const MENTION_SCAN: usize = 1_000;
/// Items per side whose headers are read (newest first).
const ITEM_SCAN: usize = 50;
/// Items of a short side whose text is read for an organisation attribution.
const ATTR_SCAN: usize = 10;
/// A full-name person's newest items whose thread headers are read.
const THREAD_SCAN: usize = 20;
/// State key of the short-form people not placed yet ([`Resolver::settle_pending`]).
pub(crate) const PENDING_KEY: &str = "knowledge/short_names";
/// Short-form people remembered at most (oldest dropped).
const MAX_PENDING: usize = 5_000;
/// Merge records kept per entity (oldest dropped).
pub(crate) const MAX_MERGE_LOG: usize = 50;
/// Longest name kept in a merge record.
pub(crate) const RECORD_NAME_CHARS: usize = 120;
/// Same-name entities beyond which a full name is not checked for a job change.
const MAX_NAMESAKES: usize = 4;
/// Shortest squashed organisation name or domain label a job-change statement may match.
const MIN_ORG_CHARS: usize = 6;

/// What an item's headers say: its thread keys, sender and participants.
#[derive(Debug, Clone, Default)]
pub(crate) struct ItemMeta {
    threads: Vec<String>,
    /// The sender's address, lower case.
    from: Option<String>,
    /// Every header address (From, To, Cc, Bcc), lower case.
    participants: Vec<String>,
}

impl ItemMeta {
    pub fn of(item: &Item) -> Self {
        let mut m = Self {
            threads: thread_keys(item),
            ..Default::default()
        };
        let Some(ex) = item.metadata.get("extracted") else {
            return m;
        };
        for key in ["from", "to", "cc", "bcc"] {
            for raw in crate::extract::strings(ex.get(key)).iter().take(100) {
                if let Some(e) = crate::text::parse_address(raw).email {
                    let e = e.to_lowercase();
                    if key == "from" && m.from.is_none() {
                        m.from = Some(e.clone());
                    }
                    if !m.participants.contains(&e) {
                        m.participants.push(e);
                    }
                }
            }
        }
        m
    }
}

/// What the item being processed says about who is on it, for corroboration.
pub(crate) struct ItemContext<'a> {
    pub item: ItemRef,
    feed: Option<&'a dyn ItemFeed>,
    meta: ItemMeta,
    /// People on the item: normalised names (with a written-out given name and a surname) and
    /// identifiers.
    people: Vec<(Vec<String>, Vec<Identifier>)>,
    /// Entities resolved from this item so far (their mentions are not written yet).
    resolved: Mutex<Vec<EntityId>>,
    metas: Mutex<HashMap<ItemRef, ItemMeta>>,
    /// The user's own domains, computed once per item.
    user_domains: Mutex<Option<BTreeSet<String>>>,
}

impl<'a> ItemContext<'a> {
    pub fn new(item: &Item, ex: &Extraction, feed: Option<&'a dyn ItemFeed>) -> Self {
        let mut people = Vec::new();
        for e in ex.entities.iter().filter(|e| e.kind == EntityKind::Person) {
            let full: Vec<String> = (!e.weak_name)
                .then_some(&e.name)
                .into_iter()
                .chain(&e.aliases)
                .map(|n| normalize_name(n))
                .filter(|n| {
                    PersonName::parse(n)
                        .is_some_and(|p| p.given_full().is_some() && p.surname.is_some())
                })
                .collect();
            people.push((full, e.identifiers.clone()));
        }
        Self {
            item: item.item.clone(),
            feed,
            meta: ItemMeta::of(item),
            people,
            resolved: Mutex::new(Vec::new()),
            metas: Mutex::new(HashMap::new()),
            user_domains: Mutex::new(None),
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

    /// Whether this side is a short form: every name is one ([`is_short_form`]), or it has no
    /// name but an address with a local part. A single written-out full name rules it out.
    pub fn all_short(&self) -> bool {
        if self.names.is_empty() {
            return !self.locals.is_empty();
        }
        self.names.iter().all(|n| is_short_form(n))
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

/// The outcome of [`Resolver::full_for`].
pub(super) struct Decision {
    /// The one corroborated candidate, if there is exactly one.
    pub pick: Option<Pick>,
    /// Name-compatible candidates seen (`usize::MAX`: too many, or the user is one).
    pub compatible: usize,
}

impl Decision {
    fn ambiguous() -> Self {
        Self {
            pick: None,
            compatible: usize::MAX,
        }
    }

    /// The pick, when no other candidate could compete with it (joins made mid-item).
    pub fn sole(self) -> Option<Pick> {
        if self.compatible == 1 {
            self.pick
        } else {
            None
        }
    }
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

/// Lower-case letters and digits only (`Vellum & Thistle` → `vellumthistle`).
fn squash(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
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
            "job_change" => Some("one address's mail names the other's organisation"),
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

/// What corroborates one candidate.
struct Corrob {
    why: Vec<String>,
    /// The item that showed it, per reason kind.
    item_shared: Option<ItemRef>,
    item_same: Option<ItemRef>,
    item_thread: Option<ItemRef>,
    /// Organisation domains shared with the short side, and the item that showed each.
    org: Vec<(String, Option<ItemRef>)>,
}

impl Resolver<'_> {
    fn is_me(&self, e: &Entity) -> bool {
        Some(&e.id) == self.me.as_ref() || is_me_attr(e)
    }

    /// Items an entity is mentioned on (newest last), plus the current item when it was
    /// resolved from it.
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

    /// An item's headers: the current item's from its context, others from the feed.
    fn meta_of(&self, item: &ItemRef) -> Result<ItemMeta> {
        if let Some(ctx) = self.ctx {
            if *item == ctx.item {
                return Ok(ctx.meta.clone());
            }
            if let Some(m) = ctx
                .metas
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(item)
            {
                return Ok(m.clone());
            }
        }
        let m = match self.feed {
            Some(f) => f
                .get_item(item)?
                .map(|i| ItemMeta::of(&i))
                .unwrap_or_default(),
            None => ItemMeta::default(),
        };
        if let Some(ctx) = self.ctx {
            let mut cache = ctx.metas.lock().unwrap_or_else(|p| p.into_inner());
            if cache.len() > 10_000 {
                cache.clear();
            }
            cache.insert(item.clone(), m.clone());
        }
        Ok(m)
    }

    /// The user's own registrable domains: of their addresses and of their employers'
    /// domains. Never evidence of a shared organisation (the user is on every item).
    fn user_domains(&self) -> Result<BTreeSet<String>> {
        if let Some(d) = self
            .user_domains
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            return Ok(d.clone());
        }
        let cached = self.ctx.and_then(|c| {
            c.user_domains
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        });
        if let Some(d) = cached {
            *self.user_domains.lock().unwrap_or_else(|p| p.into_inner()) = Some(d.clone());
            return Ok(d);
        }
        let mut out = BTreeSet::new();
        if let Some(me) = self
            .me
            .as_ref()
            .map(|m| self.canonical(m))
            .transpose()?
            .flatten()
        {
            for e in emails(&me.identifiers) {
                if let Some(d) = crate::text::domain_of(e) {
                    out.insert(crate::text::registrable_domain(&d));
                }
            }
            for c in self.graph.claims_for(&me.id, false)? {
                if c.subject == me.id
                    && c.predicate == "works_at"
                    && c.status.is_active()
                    && let nugget_graph::ClaimObject::Entity(o) = &c.object
                    && let Some(org) = self.canonical(o)?
                {
                    for i in org.identifiers.iter().filter(|i| i.scheme == "domain") {
                        out.insert(crate::text::registrable_domain(&i.value));
                    }
                }
            }
        }
        *self.user_domains.lock().unwrap_or_else(|p| p.into_inner()) = Some(out.clone());
        if let Some(c) = self.ctx {
            *c.user_domains.lock().unwrap_or_else(|p| p.into_inner()) = Some(out.clone());
        }
        Ok(out)
    }

    /// Registrable domains of addresses that can show an organisation: not webmail, not the
    /// user's.
    fn org_domains<'x>(
        &self,
        addrs: impl IntoIterator<Item = &'x str>,
    ) -> Result<BTreeSet<String>> {
        let user = self.user_domains()?;
        Ok(addrs
            .into_iter()
            .filter_map(crate::text::domain_of)
            .filter(|d| !crate::text::is_freemail(d, &self.cfg.extra_freemail_domains))
            .map(|d| crate::text::registrable_domain(&d))
            .filter(|d| !user.contains(d))
            .collect())
    }

    /// What corroborates that the short side and `f` are the same person, beyond their names.
    fn corroborate(
        &self,
        short: &Short<'_>,
        s_items: &[ItemRef],
        f: &Entity,
        f_on_current: bool,
    ) -> Result<Corrob> {
        let mut c = Corrob {
            why: Vec::new(),
            item_shared: None,
            item_same: None,
            item_thread: None,
            org: Vec::new(),
        };
        let latest = s_items.last().cloned();
        // Shared identifiers.
        for i in short.identifiers.iter().filter(|i| is_strong(&i.scheme)) {
            if f.identifiers.contains(i) {
                push_reason(
                    &mut c.why,
                    if i.scheme == "email" {
                        "shared_email"
                    } else {
                        "shared_identifier"
                    },
                );
                c.item_shared = latest.clone();
            }
        }
        // Same item.
        let f_items = self.items_of(f, f_on_current)?;
        c.item_same = s_items.iter().rev().find(|i| f_items.contains(i)).cloned();
        if c.item_same.is_none()
            && short.on_current
            && let Some(ctx) = self.ctx
        {
            let f_names: Vec<String> = full_names(f).iter().map(|n| normalize_name(n)).collect();
            if ctx.people.iter().any(|(names, ids)| {
                ids.iter().any(|i| f.identifiers.contains(i))
                    || names.iter().any(|n| f_names.contains(n))
            }) {
                c.item_same = Some(ctx.item.clone());
            }
        }
        if c.item_same.is_some() {
            push_reason(&mut c.why, "same_item");
        }
        // Same thread.
        let mut s_threads: Vec<(ItemRef, Vec<String>)> = Vec::new();
        for i in s_items.iter().rev().take(ITEM_SCAN) {
            let m = self.meta_of(i)?;
            if !m.threads.is_empty() {
                s_threads.push((i.clone(), m.threads));
            }
        }
        if !s_threads.is_empty() {
            let mut f_threads: HashSet<String> = HashSet::new();
            for i in f_items.iter().rev().take(THREAD_SCAN) {
                f_threads.extend(self.meta_of(i)?.threads);
            }
            if let Some((i, _)) = s_threads
                .iter()
                .find(|(_, t)| t.iter().any(|k| f_threads.contains(k)))
            {
                c.item_thread = Some(i.clone());
                push_reason(&mut c.why, "same_thread");
            }
        }
        // Same organisation: the short side's address, or a header participant of one of its
        // items (`f`'s own addresses aside), at one of `f`'s organisation domains.
        let f_domains = self.org_domains(emails(&f.identifiers))?;
        if !f_domains.is_empty() {
            let f_addrs: Vec<String> = emails(&f.identifiers).map(str::to_lowercase).collect();
            for d in self.org_domains(emails(&short.identifiers))? {
                if f_domains.contains(&d) {
                    c.org.push((d, latest.clone()));
                }
            }
            for i in s_items.iter().rev().take(ITEM_SCAN) {
                let m = self.meta_of(i)?;
                let others = m
                    .participants
                    .iter()
                    .filter(|a| !f_addrs.contains(a))
                    .map(String::as_str);
                for d in self.org_domains(others)? {
                    if f_domains.contains(&d) && !c.org.iter().any(|(x, _)| *x == d) {
                        c.org.push((d, Some(i.clone())));
                    }
                }
            }
        }
        // Same organisation, from the text: an item of the short side attributes the name to
        // one of `f`'s organisations ("Tamsin from Quillfeather").
        if c.org.is_empty() && !short.names.is_empty() {
            'attr: for d in &f_domains {
                for i in s_items.iter().rev().take(ATTR_SCAN) {
                    if self.attributes_to_org(i, &short.names, d)? {
                        c.org.push((d.clone(), Some(i.clone())));
                        break 'attr;
                    }
                }
            }
        }
        Ok(c)
    }

    /// Why the short side's names fit `f` (empty: they do not). A written-out full name on the
    /// short side fits nothing else.
    fn name_fit(short: &Short<'_>, f: &Entity) -> Vec<String> {
        if short
            .names
            .iter()
            .any(|n| PersonName::parse(n).is_some_and(|p| p.is_complete()))
        {
            return Vec::new();
        }
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
        if short.names.is_empty() {
            for l in &short.locals {
                if fulls.iter().any(|n| l.matches(n) == Some(LocalMatch::Full)) {
                    push_reason(&mut reasons, "email_local_part");
                }
            }
        } else if !reasons.is_empty() {
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
                (None, Some(s)) => out.push(s.to_owned()),
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
        // A substring search for "sam" already finds "samuel" and "samantha".
        let all = out.clone();
        out.retain(|n| !all.iter().any(|m| m != n && n.contains(m.as_str())));
        out
    }

    /// The decision both directions share: the one full-name person the short side may be
    /// short for **and** that something corroborates. No pick when there is none or more than
    /// one, when the user fits the name, or when the lookup runs past [`CAND_MAX`] rows.
    /// `f_current`: an entity known to be on the current item (being resolved from it).
    pub(super) fn full_for(
        &self,
        short: &Short<'_>,
        f_current: Option<&EntityId>,
    ) -> Result<Decision> {
        if !short.all_short() {
            return Ok(Decision {
                pick: None,
                compatible: 0,
            });
        }
        let live = self.live()?;
        let resolved = self.ctx.map(ItemContext::resolved).unwrap_or_default();
        // Every name-compatible person, eligible or not.
        let mut seen: BTreeSet<EntityId> = BTreeSet::new();
        let mut cands: Vec<(Entity, Vec<String>, bool)> = Vec::new();
        for needle in Self::needles(short) {
            let mut offset = 0;
            loop {
                let page = self.graph.list_entities(&EntityQuery {
                    text: Some(needle.clone()),
                    kind: Some(EntityKind::Person),
                    limit: CAND_PAGE,
                    offset,
                })?;
                let n = page.len();
                for s in page {
                    let Some(f) = self.canonical(&s.id)? else {
                        continue;
                    };
                    if !seen.insert(f.id.clone())
                        || f.kind != EntityKind::Person
                        || short.entity.is_some_and(|e| e.id == f.id)
                    {
                        continue;
                    }
                    let names = Self::name_fit(short, &f);
                    if names.is_empty() {
                        continue;
                    }
                    if self.is_me(&f) {
                        return Ok(Decision::ambiguous()); // the user's own nickname
                    }
                    let eligible = !identifiers_conflict(&short.identifiers, &f.identifiers)
                        && !short
                            .entity
                            .is_some_and(|e| crate::splits::apart(&live, &e.id, &f.id));
                    cands.push((f, names, eligible));
                }
                if n < CAND_PAGE {
                    break;
                }
                offset += CAND_PAGE;
                if offset >= CAND_MAX {
                    return Ok(Decision::ambiguous());
                }
            }
        }
        let compatible = cands.len();
        if compatible == 0 {
            return Ok(Decision {
                pick: None,
                compatible,
            });
        }
        // Organisation domains of every compatible person, for uniqueness.
        let mut domain_count: HashMap<String, usize> = HashMap::new();
        for (f, _, _) in &cands {
            for d in self.org_domains(emails(&f.identifiers))? {
                *domain_count.entry(d).or_default() += 1;
            }
        }
        let s_items: Vec<ItemRef> = match short.entity {
            Some(e) => self.items_of(e, short.on_current)?,
            None => self
                .ctx
                .filter(|_| short.on_current)
                .map(|c| vec![c.item.clone()])
                .unwrap_or_default(),
        };
        let mut picks: Vec<Pick> = Vec::new();
        for (f, names, eligible) in cands {
            if !eligible {
                continue;
            }
            let on_current = f_current == Some(&f.id) || resolved.contains(&f.id);
            let mut c = self.corroborate(short, &s_items, &f, on_current)?;
            // An organisation only tells people apart when this name is unique in it.
            c.org.retain(|(d, _)| domain_count.get(d) == Some(&1));
            let item_org = c.org.first().and_then(|(_, i)| i.clone());
            if !c.org.is_empty() {
                push_reason(&mut c.why, "same_org");
            }
            if c.why.is_empty() {
                continue;
            }
            let mut reasons = names;
            for w in &c.why {
                push_reason(&mut reasons, w);
            }
            let item = c.item_shared.or(c.item_same).or(c.item_thread).or(item_org);
            picks.push(Pick {
                detail: describe(&reasons),
                entity: f,
                reasons,
                item,
            });
            if picks.len() > 1 {
                return Ok(Decision {
                    pick: None,
                    compatible,
                }); // ambiguous (two Sams)
            }
        }
        Ok(Decision {
            pick: picks.pop(),
            compatible,
        })
    }

    /// Short-form people already in the graph that `f` may be the full name of: lookups of
    /// the short forms its names can take (first name or nickname, with or without the
    /// surname's initial; given initial and surname).
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
            for v in given_name_variants(g) {
                forms.insert(v.clone());
                if let Some(c) = si {
                    forms.insert(format!("{v} {c}"));
                    forms.insert(format!("{v} {c}."));
                }
            }
            if let (Some(s), Some(gi)) = (p.surname_full(), g.chars().next()) {
                forms.insert(format!("{gi}. {s}"));
                forms.insert(format!("{gi} {s}"));
            }
        }
        let mut out: Vec<Entity> = Vec::new();
        for form in forms.into_iter().filter(|f| is_short_form(f)) {
            for id in self.graph.find_by_name(&form, Some(EntityKind::Person))? {
                if let Some(e) = self.canonical(&id)?
                    && e.id != f.id
                    && !self.is_me(&e)
                    && !out.iter().any(|o| o.id == e.id)
                    && !identifiers_conflict(&e.identifiers, &f.identifiers)
                    && Short::of_entity(&e, false).all_short()
                {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }

    /// Reverse direction: absorb into `f` the short-form people for whom `f` is the one
    /// corroborated full name and the only name-compatible one (others are left to the
    /// end-of-run pass). Returns the merged-away ids.
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
            let Some(pick) = self.full_for(&short, Some(&f.id))?.sole() else {
                continue;
            };
            if pick.entity.id == f.id
                && self.merge(&f.id, &s.id, pick.reasons, pick.detail, pick.item)?
            {
                merged.push(s.id.clone());
            }
        }
        Ok(merged)
    }

    /// Given names (and surnames) of a full-name person, as short forms would search for them.
    fn name_keys(f: &Entity) -> Vec<String> {
        let mut out = Vec::new();
        for n in full_names(f) {
            if let Some(p) = PersonName::parse(&n) {
                if let Some(g) = p.given_full() {
                    out.extend(given_name_variants(g));
                }
                if let Some(s) = p.surname_full() {
                    out.push(s.to_owned());
                }
            }
        }
        out
    }

    /// The end-of-run pass. Short-form people left on their own are remembered (under
    /// [`PENDING_KEY`]); once a run's writes are all applied, each one that the run may have
    /// changed the evidence for (it was touched, a touched entity shares one of its items, or
    /// a touched person has a name it may be short for) is decided again: it joins its one
    /// corroborated candidate, however many name-compatible ones there are. Returns the merges
    /// done, as `(into, from)`.
    pub(crate) fn settle_pending(
        &self,
        feed: &dyn ItemFeed,
        touched: &BTreeSet<EntityId>,
    ) -> Result<Vec<(EntityId, EntityId)>> {
        let mut pending: Vec<EntityId> = match feed.get_state(PENDING_KEY)? {
            Some(v) => serde_json::from_value(v).unwrap_or_default(),
            None => Vec::new(),
        };
        let mut touched_items: HashSet<ItemRef> = HashSet::new();
        let mut touched_names: HashSet<String> = HashSet::new();
        for t in touched {
            let Some(e) = self.canonical(t)? else {
                continue;
            };
            if e.kind == EntityKind::Person && !self.is_me(&e) {
                if Short::of_entity(&e, false).all_short() {
                    pending.push(e.id.clone());
                } else {
                    touched_names.extend(Self::name_keys(&e));
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
            if !seen.insert(s.id.clone()) || s.kind != EntityKind::Person || self.is_me(&s) {
                continue;
            }
            let short = Short::of_entity(&s, false);
            if !short.all_short() {
                continue;
            }
            let relevant = touched.contains(&s.id)
                || touched.contains(&id)
                || Self::needles(&short)
                    .iter()
                    .any(|n| touched_names.contains(n))
                || self
                    .graph
                    .mentions_for(&s.id, MENTION_SCAN)?
                    .iter()
                    .any(|m| touched_items.contains(&m.item));
            if relevant
                && let Some(pick) = self.full_for(&short, None)?.pick
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

    /// The same full name under two conflicting addresses, joined only on a job-change
    /// statement (see the module docs). Checked for the people a run touched, once all its
    /// writes are applied. Returns the merges done, as `(into, from)`.
    pub(crate) fn settle_job_changes(
        &self,
        touched: &BTreeSet<EntityId>,
    ) -> Result<Vec<(EntityId, EntityId)>> {
        let live = self.live()?;
        let mut merges = Vec::new();
        for t in touched {
            let Some(p) = self.canonical(t)? else {
                continue;
            };
            if p.kind != EntityKind::Person
                || self.is_me(&p)
                || emails(&p.identifiers).next().is_none()
                || !PersonName::parse(&p.name).is_some_and(|n| n.is_complete())
            {
                continue;
            }
            let namesakes = self.graph.find_by_name(&p.name, Some(EntityKind::Person))?;
            if namesakes.len() > MAX_NAMESAKES {
                continue; // a common name: several people, not one person twice
            }
            for qid in namesakes {
                let (Some(p), Some(q)) = (self.canonical(&p.id)?, self.canonical(&qid)?) else {
                    continue;
                };
                if p.id == q.id
                    || self.is_me(&q)
                    || emails(&q.identifiers).next().is_none()
                    || !identifiers_conflict(&p.identifiers, &q.identifiers)
                    || crate::splits::apart(&live, &p.id, &q.id)
                    || super::single_valued_conflict(&p, &q)
                {
                    continue;
                }
                let (into, from, item) = match self.states_other_employer(&p, &q)? {
                    Some(i) => (p, q, i),
                    None => match self.states_other_employer(&q, &p)? {
                        Some(i) => (q, p, i),
                        None => continue,
                    },
                };
                let reasons = vec!["same_name".to_string(), "job_change".to_string()];
                if self.merge(
                    &into.id,
                    &from.id,
                    reasons.clone(),
                    describe(&reasons),
                    Some(item),
                )? {
                    // Name-only twins of the same name now have one person to join.
                    let twin = EntitySpec::new(EntityKind::Person, into.name.clone());
                    let more = self.absorb(&into.id, &twin)?;
                    merges.push((into.id.clone(), from.id));
                    merges.extend(more.into_iter().map(|m| (into.id.clone(), m)));
                }
            }
        }
        Ok(merges)
    }

    /// How text may name the organisation of a mail domain, squashed ([`squash`]): the
    /// domain's label, and the names of the organisation holding the domain (legal suffix
    /// dropped too). Too-short forms are left out.
    fn org_needles(&self, domain: &str) -> Result<BTreeSet<String>> {
        let mut needles: BTreeSet<String> = BTreeSet::new();
        needles.insert(squash(&super::domain_label(domain)));
        if let Some(oid) = self
            .graph
            .find_by_identifier(&Identifier::new("domain", domain))?
            && let Some(o) = self.canonical(&oid)?
        {
            for n in std::iter::once(&o.name).chain(&o.aliases) {
                let words: Vec<&str> = n.split_whitespace().collect();
                needles.insert(squash(n));
                if let Some((last, rest)) = words.split_last()
                    && !rest.is_empty()
                    && matches!(
                        squash(last).as_str(),
                        "llp" | "ltd" | "inc" | "llc" | "plc" | "gmbh" | "co" | "corp"
                    )
                {
                    needles.insert(squash(&rest.join(" ")));
                }
            }
        }
        needles.retain(|n| n.len() >= MIN_ORG_CHARS);
        Ok(needles)
    }

    /// Whether an item's text attributes the short name to the organisation of `domain`
    /// with "NAME from ORG", "NAME at ORG" or "NAME of ORG" ("Tamsin from Quillfeather"). A
    /// statement about the person, not an organisation merely named somewhere in the body.
    fn attributes_to_org(&self, item: &ItemRef, names: &[String], domain: &str) -> Result<bool> {
        let Some(feed) = self.feed else {
            return Ok(false);
        };
        let needles = self.org_needles(domain)?;
        if needles.is_empty() {
            return Ok(false);
        }
        let given: Vec<String> = names
            .iter()
            .filter_map(|n| PersonName::parse(n))
            .filter_map(|p| p.given_full().map(str::to_owned))
            .collect();
        if given.is_empty() {
            return Ok(false);
        }
        for chunk in feed.chunks(item, 16)? {
            let text = chunk.text.to_lowercase();
            for g in &given {
                for (at, _) in text.match_indices(g.as_str()) {
                    let before_ok = text[..at]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !c.is_alphanumeric());
                    let rest = &text[at + g.len()..];
                    let Some(after) = [" from ", " at ", " of "]
                        .iter()
                        .find_map(|sep| rest.strip_prefix(sep))
                    else {
                        continue;
                    };
                    let head: String = squash(&after.chars().take(80).collect::<String>());
                    if before_ok && needles.iter().any(|n| head.starts_with(n.as_str())) {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// An item sent from one of `a`'s addresses that names the organisation of one of `b`'s
    /// addresses (its domain label, or a name of the organisation holding the domain).
    fn states_other_employer(&self, a: &Entity, b: &Entity) -> Result<Option<ItemRef>> {
        let Some(feed) = self.feed else {
            return Ok(None);
        };
        let a_addrs: Vec<String> = emails(&a.identifiers).map(str::to_lowercase).collect();
        let a_domains = self.org_domains(a_addrs.iter().map(String::as_str))?;
        let mut needles: BTreeSet<String> = BTreeSet::new();
        for d in emails(&b.identifiers)
            .filter_map(crate::text::domain_of)
            .filter(|d| !crate::text::is_freemail(d, &self.cfg.extra_freemail_domains))
            .map(|d| crate::text::registrable_domain(&d))
        {
            if a_domains.contains(&d) {
                continue; // same organisation: nothing changed
            }
            needles.extend(self.org_needles(&d)?);
        }
        if needles.is_empty() {
            return Ok(None);
        }
        for item in self.items_of(a, false)?.iter().rev().take(ITEM_SCAN) {
            let m = self.meta_of(item)?;
            if !m.from.as_ref().is_some_and(|f| a_addrs.contains(f)) {
                continue;
            }
            let text: String = feed
                .chunks(item, 16)?
                .iter()
                .map(|c| squash(&c.text))
                .collect();
            if needles.iter().any(|n| text.contains(n.as_str())) {
                return Ok(Some(item.clone()));
            }
        }
        Ok(None)
    }

    /// Forward direction for an existing entity found by an exact name or identifier: when it
    /// is a short form and the only name-compatible candidate is corroborated, merge it in.
    /// Returns that person's id when merged.
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
        let Some(pick) = self.full_for(&short, None)?.sole() else {
            return Ok(None);
        };
        if self.merge(&pick.entity.id, &e.id, pick.reasons, pick.detail, pick.item)? {
            Ok(Some(pick.entity.id))
        } else {
            Ok(None)
        }
    }

    /// Append a merge record to `id`'s log (unless already there) and index it by its item.
    pub(super) fn record(&self, id: &EntityId, mut rec: MergeRecord) -> Result<bool> {
        rec.name = crate::text::truncate_chars(&rec.name, RECORD_NAME_CHARS);
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

/// Whether an occurrence that found no entity by identifier or name should be tried against
/// full names: a person known only by short forms ([`Short::all_short`]).
pub(super) fn wants_full(spec: &EntitySpec) -> bool {
    spec.kind == EntityKind::Person && Short::of_spec(spec, false).all_short()
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
            name: crate::text::truncate_chars(&b.name, RECORD_NAME_CHARS),
            reasons,
            detail,
            item,
            at: Some(time::OffsetDateTime::now_utc()),
        });
    }
    let survivor = a.map(|a| a.id).unwrap_or_else(|| into.clone());
    write_log(graph, &survivor, log)
}

/// Prefix of the state keys indexing merge records by item, one key per item.
pub(crate) const MERGE_ITEMS_KEY: &str = "knowledge/merge_log_items";
/// Entities indexed per item at most.
const MAX_INDEXED_PER_ITEM: usize = 256;

fn index_key(item: &ItemRef) -> String {
    format!("{MERGE_ITEMS_KEY}/{}\u{1f}{}", item.source, item.key)
}

/// Index a merge record of `id` by the item it cites (one small state value per item).
pub(crate) fn index_record(feed: &dyn ItemFeed, item: &ItemRef, id: &EntityId) -> Result<()> {
    let key = index_key(item);
    let mut ids: Vec<EntityId> = match feed.get_state(&key)? {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        None => Vec::new(),
    };
    if ids.contains(id) {
        return Ok(());
    }
    ids.push(id.clone());
    let drop = ids.len().saturating_sub(MAX_INDEXED_PER_ITEM);
    ids.drain(..drop);
    feed.set_state(&key, serde_json::to_value(&ids)?)
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
    let key = index_key(item);
    let Some(v) = feed.get_state(&key)? else {
        return Ok(Vec::new());
    };
    let ids: Vec<EntityId> = serde_json::from_value(v).unwrap_or_default();
    let mut changed: Vec<EntityId> = Vec::new();
    for id in ids {
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
    feed.clear_state(&key)?;
    Ok(changed)
}

/// The user split `split.to` off `split.from`: the records of the merges that joined them no
/// longer hold. Drops from `from`'s log every record of an entity now resolving to `to`, or
/// named by one of `to`'s names. Returns whether the log changed.
pub(crate) fn forget_split(
    graph: &dyn nugget_graph::GraphStore,
    split: &crate::splits::Split,
) -> Result<bool> {
    let Some(a) = graph.get_entity(&split.from)? else {
        return Ok(false);
    };
    let to = graph.get_entity(&split.to)?.map(|b| b.id);
    let log = merge_log(&a);
    let mut kept = Vec::with_capacity(log.len());
    for r in &log {
        let named = split.names.contains(&normalize_name(&r.name));
        let moved = match (&r.from, &to) {
            (Some(f), Some(t)) => graph.get_entity(f)?.is_some_and(|e| &e.id == t),
            _ => false,
        };
        if !named && !moved {
            kept.push(r.clone());
        }
    }
    if kept.len() == log.len() {
        return Ok(false);
    }
    write_log(graph, &a.id, kept)?;
    Ok(true)
}
