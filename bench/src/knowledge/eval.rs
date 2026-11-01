//! Scoring a knowledge graph and a rendered vault against the gold labels.
//!
//! - **Entities.** Each system entity is matched to gold entities of the same kind that share an
//!   identifier (2 points each, after [`Identifier::normalized`]) or whose surface forms include
//!   the entity's name or an alias (1 point each, after [`normalize_name`]). The strict matching
//!   is one-to-one, greedy by score: a second system entity for the same person (an unmerged
//!   duplicate) is a false positive, and one system entity covering two people leaves the second
//!   a false negative. Matches to `optional` gold entities count neither way.
//! - **Claims.** Exact triple match after resolution: each system entity maps to its best gold
//!   candidate (many-to-one, so relation scores do not double-count resolution errors), the
//!   predicate goes through [`canonical_predicate`] (synonyms, inverse forms, kind-aware
//!   fixes), literals through [`normalize_literal`], and a literal that names exactly one gold
//!   entity counts as that entity. Superseded gold claims are false positives when active.
//!   Only predicates the gold labels use are scored (the pipeline may add others, such as
//!   `corresponded_with` or `references`; they are counted, not judged).
//! - **Resolution.** B³ over the gold mentions the system detected (same item, a matching
//!   surface form), with mention recall reported separately.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use nugget_core::ids::{EntityId, ItemRef};
use nugget_graph::{
    Claim, ClaimObject, ClaimStatus, Entity, EntityKind, EntityQuery, Evidence, GraphStore,
    Identifier, Mention, normalize_name,
};

use super::fixture::{
    Fixture, GoldClaim, GoldMention, GoldStatus, Ingested, Unattributed, normalize_quote, quote_in,
};
use super::system::{RenderedPage, RenderedVault};
use crate::corpus::Rng;
use crate::runner::BenchError;

/// Upper bound for list calls (some stores treat a limit of 0 as "none").
const LIST_LIMIT: usize = 1_000_000;

// =============================================================================================
// Snapshot of the system's graph
// =============================================================================================

/// Everything in a graph, read through the [`GraphStore`] contract.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub entities: Vec<Entity>,
    /// Every claim, any status.
    pub claims: Vec<Claim>,
    pub mentions: Vec<Mention>,
    /// Merged-away ids → surviving id.
    merged: HashMap<EntityId, EntityId>,
    index: HashMap<EntityId, usize>,
}

impl Snapshot {
    pub fn take(g: &dyn GraphStore) -> Result<Self, BenchError> {
        let summaries = g.list_entities(&EntityQuery {
            limit: LIST_LIMIT,
            ..Default::default()
        })?;
        let mut entities = Vec::new();
        let mut claims = BTreeMap::new();
        let mut mentions = Vec::new();
        for s in summaries {
            let Some(e) = g.get_entity(&s.id)? else {
                continue;
            };
            if e.id != s.id || entities.iter().any(|x: &Entity| x.id == e.id) {
                continue;
            }
            for c in g.claims_for(&e.id, true)? {
                claims.entry(c.id).or_insert(c);
            }
            mentions.extend(g.mentions_for(&e.id, LIST_LIMIT)?);
            entities.push(e);
        }
        Ok(Self::from_parts(
            entities,
            claims.into_values().collect(),
            mentions,
        ))
    }

    pub fn from_parts(entities: Vec<Entity>, claims: Vec<Claim>, mentions: Vec<Mention>) -> Self {
        let mut merged = HashMap::new();
        let mut index = HashMap::new();
        for (i, e) in entities.iter().enumerate() {
            index.insert(e.id.clone(), i);
            for m in &e.merged_ids {
                merged.insert(m.clone(), e.id.clone());
            }
        }
        Self {
            entities,
            claims,
            mentions,
            merged,
            index,
        }
    }

    /// The live entity an id refers to (following merges).
    pub fn entity(&self, id: &EntityId) -> Option<&Entity> {
        let id = self.merged.get(id).unwrap_or(id);
        self.index.get(id).map(|&i| &self.entities[i])
    }

    pub fn active_claims(&self) -> impl Iterator<Item = &Claim> {
        self.claims.iter().filter(|c| c.status.is_active())
    }
}

// =============================================================================================
// Normalisation
// =============================================================================================

/// Lowercase snake_case: every run of non-alphanumerics becomes one `_`.
pub fn normalize_predicate(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_owned()
}

/// Predicates whose subject and object can be swapped.
pub const SYMMETRIC_PREDICATES: &[&str] = &[
    "sibling_of",
    "colleague_of",
    "knows",
    "married_to",
    "spouse_of",
];

/// Map a predicate to the gold vocabulary: `(canonical, inverted)`. `inverted` means the
/// subject and object must be swapped (e.g. a project's `owner` is the inverse of `leads`).
/// Kinds disambiguate (`manages` a project is `leads`; `manages` a person is the inverse of
/// `reports_to`). Unknown predicates pass through normalised.
///
/// Gold vocabulary: `works_at`, `has_role`, `member_of`, `leads`, `works_on`, `part_of`,
/// `attended`, `held_at`, `starts_at`, `committed_to`, `due_on`, `prefers`, `reports_to`,
/// `lives_in`, `sibling_of`.
pub fn canonical_predicate(
    raw: &str,
    subject: Option<EntityKind>,
    object: Option<EntityKind>,
) -> (String, bool) {
    use EntityKind::{Commitment, Event, Organization, Person, Project};
    let p = normalize_predicate(raw);
    let fwd = |c: &str| (c.to_owned(), false);
    let inv = |c: &str| (c.to_owned(), true);
    match p.as_str() {
        "works_at" | "works_for" | "employed_by" | "employed_at" | "employer" | "works_in"
        | "company" | "affiliated_with" | "affiliation" | "org" | "organization"
        | "organisation" => match subject {
            Some(Project) => fwd("part_of"),
            _ => fwd("works_at"),
        },
        "has_role" | "has_title" | "role" | "title" | "job_title" | "position" | "job" => {
            fwd("has_role")
        }
        "leads" | "lead" | "owns" | "owner_of" | "runs" | "heads" | "manages" | "managing"
        | "leading" => match (subject, object) {
            (Some(Person), Some(Person)) => inv("reports_to"),
            _ => fwd("leads"),
        },
        "owner" | "owned_by" | "led_by" | "lead_by" | "managed_by" | "run_by" | "headed_by" => {
            match subject {
                Some(Commitment) => inv("committed_to"),
                Some(Person) => fwd("reports_to"),
                _ => inv("leads"),
            }
        }
        "works_on" | "contributes_to" | "contributor_to" | "involved_in" | "participates_in"
        | "assigned_to" => match (subject, object) {
            (Some(Commitment), _) => inv("committed_to"),
            (_, Some(Event)) => fwd("attended"),
            _ => fwd("works_on"),
        },
        "members" | "member" | "team" | "team_member" | "team_members" | "contributors"
        | "has_member" => match subject {
            Some(Project) => inv("works_on"),
            Some(Organization) => inv("member_of"),
            _ => fwd("member_of"),
        },
        "part_of" | "belongs_to" | "project_of" => match subject {
            Some(Person) => fwd("member_of"),
            _ => fwd("part_of"),
        },
        "member_of" | "club" | "membership" => match subject {
            Some(Project) => fwd("part_of"),
            _ => fwd("member_of"),
        },
        "attended" | "attends" | "attending" | "attendee_of" | "participant_in"
        | "participated_in" | "invited_to" | "will_attend" | "speaking_at" | "speaker_at"
        | "speaks_at" | "organizer_of" | "organiser_of" | "organizes" | "organises"
        | "organized" | "organised" => fwd("attended"),
        "attendee" | "attendees" | "participant" | "participants" | "organizer" | "organiser"
        | "has_attendee" | "invitee" | "invitees" => inv("attended"),
        "held_at" | "location" | "venue" | "takes_place_at" | "at" | "place" | "held_in" => {
            fwd("held_at")
        }
        "located_at" | "located_in" => match subject {
            Some(Event) => fwd("held_at"),
            _ => fwd(&p),
        },
        "hosts" | "hosted" => inv("held_at"),
        "starts_at" | "start" | "starts" | "start_time" | "start_date" | "date"
        | "scheduled_for" | "scheduled_at" | "dtstart" => fwd("starts_at"),
        "committed_to" | "commitment" | "commits_to" | "promised" | "promises" | "owes"
        | "responsible_for" | "will_do" | "has_commitment" | "committed" => fwd("committed_to"),
        "committed_by" | "promised_by" => inv("committed_to"),
        "due_on" | "due" | "due_date" | "deadline" | "due_by" => fwd("due_on"),
        "prefers" | "likes" | "preference" | "prefers_to" | "favours" | "favors" | "prefer"
        | "has_preference" => fwd("prefers"),
        "reports_to" | "manager" | "reports_into" | "boss" => fwd("reports_to"),
        "manager_of" | "direct_report" | "direct_reports" => inv("reports_to"),
        "lives_in" | "resides_in" | "home" | "lives_at" | "home_town" => fwd("lives_in"),
        "based_in" => match subject {
            Some(Person) => fwd("lives_in"),
            _ => fwd(&p),
        },
        "sibling_of" | "brother_of" | "sister_of" | "sibling" | "brother" | "sister" => {
            fwd("sibling_of")
        }
        _ => (p, false),
    }
}

/// Literal comparison form: an ISO date or date-time (`2026-12-01`, `2026-12-01T09:00:00Z`,
/// `20261201T090000Z`) becomes `YYYY-MM-DD`; anything else goes through [`normalize_name`].
pub fn normalize_literal(s: &str) -> String {
    let t = s.trim();
    let b = t.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.len() >= r.end && b[r].iter().all(u8::is_ascii_digit);
    if digits(0..4)
        && b.len() >= 10
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
    {
        return t[..10].to_owned();
    }
    if digits(0..8) && (b.len() == 8 || b[8] == b'T') {
        return format!("{}-{}-{}", &t[..4], &t[4..6], &t[6..8]);
    }
    normalize_name(t)
}

/// A node of a canonical triple.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Node {
    /// A gold entity id.
    Gold(String),
    /// A system entity that matches no gold entity.
    Unmatched(EntityId),
    /// A normalised literal.
    Literal(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Triple {
    pub subject: Node,
    pub predicate: String,
    pub object: Node,
}

// =============================================================================================
// Gold index and entity matching
// =============================================================================================

fn normalize_identifier(i: &Identifier) -> Identifier {
    let mut n = i.normalized();
    if n.scheme == "mailto" {
        n.scheme = "email".into();
    }
    if let Some(v) = n.value.strip_prefix("mailto:") {
        n.value = v.to_owned();
    }
    n
}

/// Lookups over the gold entities.
pub struct GoldIndex<'a> {
    pub fx: &'a Fixture,
    kind: HashMap<&'a str, EntityKind>,
    optional: HashSet<&'a str>,
    by_ident: HashMap<Identifier, Vec<&'a str>>,
    by_name: HashMap<(EntityKind, String), Vec<&'a str>>,
    by_surface: HashMap<String, BTreeSet<&'a str>>,
}

impl<'a> GoldIndex<'a> {
    pub fn new(fx: &'a Fixture) -> Self {
        let mut ix = Self {
            fx,
            kind: HashMap::new(),
            optional: HashSet::new(),
            by_ident: HashMap::new(),
            by_name: HashMap::new(),
            by_surface: HashMap::new(),
        };
        for e in &fx.entities {
            ix.kind.insert(&e.id, e.kind);
            if e.optional {
                ix.optional.insert(&e.id);
            }
            for i in &e.identifiers {
                ix.by_ident
                    .entry(normalize_identifier(i))
                    .or_default()
                    .push(&e.id);
            }
            let mut names: BTreeSet<String> =
                e.surfaces.iter().map(|s| normalize_name(s)).collect();
            names.insert(normalize_name(&e.name));
            for n in names.into_iter().filter(|n| !n.is_empty()) {
                ix.by_name
                    .entry((e.kind, n.clone()))
                    .or_default()
                    .push(&e.id);
                ix.by_surface.entry(n).or_default().insert(&e.id);
            }
        }
        ix
    }

    pub fn kind(&self, id: &str) -> Option<EntityKind> {
        self.kind.get(id).copied()
    }

    pub fn is_optional(&self, id: &str) -> bool {
        self.optional.contains(id)
    }

    /// The single gold entity (any kind) a literal names, if exactly one.
    pub fn literal_entity(&self, lit: &str) -> Option<&'a str> {
        let set = self.by_surface.get(&normalize_name(lit))?;
        (set.len() == 1).then(|| *set.iter().next().expect("one"))
    }

    /// Gold candidates for a system entity with scores, best first.
    pub fn candidates(&self, e: &Entity) -> Vec<(u32, &'a str)> {
        let mut score: BTreeMap<&'a str, u32> = BTreeMap::new();
        for i in &e.identifiers {
            for g in self
                .by_ident
                .get(&normalize_identifier(i))
                .into_iter()
                .flatten()
            {
                if self.kind(g) == Some(e.kind) {
                    *score.entry(g).or_default() += 2;
                }
            }
        }
        let mut names: BTreeSet<String> = e.aliases.iter().map(|a| normalize_name(a)).collect();
        names.insert(normalize_name(&e.name));
        for n in names {
            for g in self.by_name.get(&(e.kind, n)).into_iter().flatten() {
                *score.entry(g).or_default() += 1;
            }
        }
        let mut v: Vec<(u32, &str)> = score.into_iter().map(|(g, s)| (s, g)).collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        v
    }

    fn gold_node(&self, id: &str) -> Node {
        Node::Gold(id.to_owned())
    }

    fn literal_node(&self, lit: &str) -> Node {
        match self.literal_entity(lit) {
            Some(g) => self.gold_node(g),
            None => Node::Literal(normalize_literal(lit)),
        }
    }

    /// The canonical triple of a gold claim.
    pub fn gold_triple(&self, c: &GoldClaim) -> Triple {
        let object = match (&c.object.entity, &c.object.literal) {
            (Some(e), _) => self.gold_node(e),
            (None, Some(l)) => self.literal_node(l),
            (None, None) => Node::Literal(String::new()),
        };
        let okind = c.object.entity.as_deref().and_then(|e| self.kind(e));
        canonical_triple(
            self.gold_node(&c.subject),
            self.kind(&c.subject),
            &c.predicate,
            object,
            okind,
        )
    }
}

fn canonical_triple(
    s: Node,
    skind: Option<EntityKind>,
    predicate: &str,
    o: Node,
    okind: Option<EntityKind>,
) -> Triple {
    let (p, inverted) = canonical_predicate(predicate, skind, okind);
    let (mut s, mut o) = if inverted && !matches!(o, Node::Literal(_)) {
        (o, s)
    } else {
        (s, o)
    };
    if SYMMETRIC_PREDICATES.contains(&p.as_str()) && o < s {
        std::mem::swap(&mut s, &mut o);
    }
    Triple {
        subject: s,
        predicate: p,
        object: o,
    }
}

/// System entities matched to gold entities.
#[derive(Debug, Clone, Default)]
pub struct EntityMatching {
    /// One-to-one (for precision and recall).
    pub strict: HashMap<EntityId, String>,
    /// Each system entity's best candidate (for claims and hard cases).
    pub loose: HashMap<EntityId, String>,
    /// Matched to an optional gold entity: counted neither way.
    pub ignored: HashSet<EntityId>,
}

pub fn match_entities(gold: &GoldIndex<'_>, snap: &Snapshot) -> EntityMatching {
    let mut m = EntityMatching::default();
    let mut pairs = Vec::new();
    for e in &snap.entities {
        let cands = gold.candidates(e);
        if let Some((_, best)) = cands.first() {
            m.loose.insert(e.id.clone(), (*best).to_owned());
        }
        if !cands.is_empty() && cands.iter().all(|(_, g)| gold.is_optional(g)) {
            m.ignored.insert(e.id.clone());
        }
        for (score, g) in cands {
            pairs.push((score, g, e.id.clone()));
        }
    }
    pairs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)).then(a.2.cmp(&b.2)));
    let mut taken = HashSet::new();
    for (_, g, sys) in pairs {
        if m.strict.contains_key(&sys) || taken.contains(g) {
            continue;
        }
        taken.insert(g);
        if gold.is_optional(g) {
            m.ignored.insert(sys);
        } else {
            m.strict.insert(sys, g.to_owned());
        }
    }
    m
}

/// Precision, recall and F1.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Prf {
    pub tp: usize,
    pub fp: usize,
    pub fn_: usize,
}

impl Prf {
    pub fn precision(&self) -> f64 {
        ratio(self.tp, self.tp + self.fp)
    }
    pub fn recall(&self) -> f64 {
        ratio(self.tp, self.tp + self.fn_)
    }
    pub fn f1(&self) -> f64 {
        f1(self.precision(), self.recall())
    }
}

pub fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

pub fn f1(p: f64, r: f64) -> f64 {
    if p + r == 0.0 {
        0.0
    } else {
        2.0 * p * r / (p + r)
    }
}

/// Entity precision/recall overall and per gold kind.
#[derive(Debug, Clone, Default)]
pub struct EntityScores {
    pub overall: Prf,
    pub by_kind: BTreeMap<EntityKind, Prf>,
    /// System entities read from the graph.
    pub system_entities: usize,
}

pub fn score_entities(gold: &GoldIndex<'_>, snap: &Snapshot, m: &EntityMatching) -> EntityScores {
    let mut s = EntityScores {
        system_entities: snap.entities.len(),
        ..Default::default()
    };
    for e in &snap.entities {
        if m.ignored.contains(&e.id) {
            continue;
        }
        let k = s.by_kind.entry(e.kind).or_default();
        if m.strict.contains_key(&e.id) {
            k.tp += 1;
            s.overall.tp += 1;
        } else {
            k.fp += 1;
            s.overall.fp += 1;
        }
    }
    let matched: HashSet<&str> = m.strict.values().map(String::as_str).collect();
    for g in gold.fx.entities.iter().filter(|g| !g.optional) {
        if !matched.contains(g.id.as_str()) {
            s.by_kind.entry(g.kind).or_default().fn_ += 1;
            s.overall.fn_ += 1;
        }
    }
    s
}

// =============================================================================================
// Claims
// =============================================================================================

/// The canonical triple of a system claim.
pub fn system_triple(
    gold: &GoldIndex<'_>,
    snap: &Snapshot,
    m: &EntityMatching,
    c: &Claim,
) -> Triple {
    let node = |id: &EntityId| -> (Node, Option<EntityKind>) {
        let Some(e) = snap.entity(id) else {
            return (Node::Unmatched(id.clone()), None);
        };
        match m.loose.get(&e.id) {
            Some(g) => (Node::Gold(g.clone()), gold.kind(g)),
            None => (Node::Unmatched(e.id.clone()), Some(e.kind)),
        }
    };
    let (s, skind) = node(&c.subject);
    let (o, okind) = match &c.object {
        ClaimObject::Entity(id) => {
            let (n, k) = node(id);
            match (&n, snap.entity(id)) {
                // A preference or commitment the system kept as an unmatched entity can still
                // name a gold entity, or equal a gold literal.
                (Node::Unmatched(_), Some(e)) => (gold.literal_node(&e.name), k),
                _ => (n, k),
            }
        }
        ClaimObject::Literal(l) => {
            let n = gold.literal_node(l);
            let k = match &n {
                Node::Gold(g) => gold.kind(g),
                _ => None,
            };
            (n, k)
        }
    };
    canonical_triple(s, skind, &c.predicate, o, okind)
}

/// Claim precision/recall over active claims.
#[derive(Debug, Clone, Default)]
pub struct ClaimScores {
    pub prf: Prf,
    /// Active system triples equal to a superseded gold claim.
    pub superseded_active: usize,
    pub system_active: usize,
    /// Active system triples whose predicate the gold labels do not use (`corresponded_with`,
    /// `references`, ...): not scored either way.
    pub out_of_vocabulary: usize,
}

/// Gold triples by status.
pub struct GoldTriples {
    pub active: HashSet<Triple>,
    pub optional: HashSet<Triple>,
    pub superseded: HashSet<Triple>,
    /// Gold claim id → triple.
    pub by_id: HashMap<String, Triple>,
    /// Canonical predicates the gold labels use. Claims with other predicates are not scored.
    pub vocabulary: HashSet<String>,
}

pub fn gold_triples(gold: &GoldIndex<'_>) -> GoldTriples {
    let mut t = GoldTriples {
        active: HashSet::new(),
        optional: HashSet::new(),
        superseded: HashSet::new(),
        by_id: HashMap::new(),
        vocabulary: HashSet::new(),
    };
    for c in &gold.fx.claims {
        let tr = gold.gold_triple(c);
        t.vocabulary.insert(tr.predicate.clone());
        t.by_id.insert(c.id.clone(), tr.clone());
        if c.optional {
            t.optional.insert(tr);
        } else if c.status == GoldStatus::Superseded {
            t.superseded.insert(tr);
        } else {
            t.active.insert(tr);
        }
    }
    // A triple both superseded and re-asserted elsewhere stays active.
    let active = t.active.clone();
    t.superseded.retain(|x| !active.contains(x));
    t
}

/// Active system claims as canonical triples (with the claims behind each).
pub fn system_triples(
    gold: &GoldIndex<'_>,
    snap: &Snapshot,
    m: &EntityMatching,
) -> BTreeMap<Triple, Vec<i64>> {
    let mut out: BTreeMap<Triple, Vec<i64>> = BTreeMap::new();
    for c in snap.active_claims() {
        out.entry(system_triple(gold, snap, m, c))
            .or_default()
            .push(c.id);
    }
    out
}

pub fn score_claims(
    gt: &GoldTriples,
    sys: &BTreeMap<Triple, Vec<i64>>,
    snap: &Snapshot,
) -> ClaimScores {
    let mut s = ClaimScores {
        system_active: snap.active_claims().count(),
        ..Default::default()
    };
    for t in sys.keys() {
        if !gt.vocabulary.contains(&t.predicate) {
            s.out_of_vocabulary += 1;
            continue;
        }
        if gt.active.contains(t) {
            s.prf.tp += 1;
        } else if gt.optional.contains(t) {
            continue;
        } else {
            if gt.superseded.contains(t) {
                s.superseded_active += 1;
            }
            s.prf.fp += 1;
        }
    }
    s.prf.fn_ = gt.active.len() - s.prf.tp;
    s
}

// =============================================================================================
// Evidence
// =============================================================================================

/// Evidence checks out: the item exists (and was not deleted), the chunk belongs to it, and the
/// quote, if any, is in that chunk. Evidence that names no chunk (structural: headers,
/// frontmatter) must quote one chunk, the item text, or one of the item's extracted fields as a
/// `key: value` line (see [`super::fixture::field_lines`]).
pub fn evidence_ok(ev: &Evidence, ing: &Ingested, deleted: &HashSet<ItemRef>) -> bool {
    if deleted.contains(&ev.item) {
        return false;
    }
    let Some(item) = ing.by_ref(&ev.item) else {
        return false;
    };
    let text = match ev.chunk_id {
        Some(cid) => match item.chunks.iter().find(|(id, _)| *id == cid) {
            Some((_, t)) => t.clone(),
            None => return false,
        },
        // Structural evidence (headers, frontmatter, calendar fields): anywhere in the item as
        // stored, one line at a time.
        None => {
            return ev.quote.as_deref().is_none_or(|q| {
                item.chunks.iter().any(|(_, t)| quote_in(q, t))
                    || quote_in(q, &item.text)
                    || item.fields.lines().any(|l| quote_in(q, l))
            });
        }
    };
    ev.quote.as_deref().is_none_or(|q| quote_in(q, &text))
}

/// A claim is supported when it has evidence and every piece of it checks out. User-asserted
/// claims need none (the vault edit is their evidence).
pub fn claim_supported(c: &Claim, ing: &Ingested, deleted: &HashSet<ItemRef>) -> bool {
    if c.status == ClaimStatus::UserAsserted {
        return true;
    }
    !c.evidence.is_empty() && c.evidence.iter().all(|e| evidence_ok(e, ing, deleted))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SupportScores {
    /// Active, non-user-asserted claims.
    pub claims: usize,
    pub unsupported: usize,
    /// Active claims citing a scripted hallucinated or injected quote.
    pub hallucinated_kept: usize,
}

impl SupportScores {
    pub fn unsupported_rate(&self) -> f64 {
        ratio(self.unsupported, self.claims)
    }
}

pub fn score_support(
    fx: &Fixture,
    snap: &Snapshot,
    ing: &Ingested,
    deleted: &HashSet<ItemRef>,
) -> SupportScores {
    let bad_quotes: HashSet<String> = fx
        .script
        .iter()
        .flat_map(|s| &s.claims)
        .filter(|c| c.hallucinated || c.injected)
        .map(|c| normalize_quote(&c.quote))
        .collect();
    let mut s = SupportScores::default();
    for c in snap
        .active_claims()
        .filter(|c| c.status != ClaimStatus::UserAsserted)
    {
        s.claims += 1;
        if !claim_supported(c, ing, deleted) {
            s.unsupported += 1;
        }
        if c.evidence.iter().any(|e| {
            e.quote
                .as_deref()
                .is_some_and(|q| bad_quotes.contains(&normalize_quote(q)))
        }) {
            s.hallucinated_kept += 1;
        }
    }
    s
}

/// Active claims from the prompt-injection items that are not quote-supported: any claim
/// citing an injection item with a quote that is not in it, plus any claim equal to one of the
/// injected triples the compromised model emitted (whatever its evidence).
pub fn injection_unsupported(
    gold: &GoldIndex<'_>,
    snap: &Snapshot,
    m: &EntityMatching,
    ing: &Ingested,
    deleted: &HashSet<ItemRef>,
) -> (usize, usize) {
    let fx = gold.fx;
    let inj_refs: HashSet<ItemRef> = fx
        .scenario
        .injection
        .iter()
        .filter_map(|id| fx.item(id).map(|i| i.item_ref()))
        .collect();
    let node = |name: &str| match gold.literal_entity(name) {
        Some(g) => (Node::Gold(g.to_owned()), gold.kind(g)),
        None => (Node::Literal(normalize_literal(name)), None),
    };
    let injected: HashSet<Triple> = fx
        .script
        .iter()
        .filter(|s| fx.scenario.injection.contains(&s.item))
        .flat_map(|s| &s.claims)
        .filter(|c| c.injected)
        .map(|c| {
            let (s, sk) = node(&c.subject);
            let (o, ok) = if c.object_type == "entity" {
                node(&c.object)
            } else {
                (Node::Literal(normalize_literal(&c.object)), None)
            };
            canonical_triple(s, sk, &c.predicate, o, ok)
        })
        .collect();
    let mut citing = 0;
    let mut bad = 0;
    for c in snap.active_claims() {
        let from_inj: Vec<&Evidence> = c
            .evidence
            .iter()
            .filter(|e| inj_refs.contains(&e.item))
            .collect();
        if !from_inj.is_empty() {
            citing += 1;
        }
        let bad_quote = from_inj
            .iter()
            .any(|e| e.quote.is_some() && !evidence_ok(e, ing, deleted));
        if bad_quote || injected.contains(&system_triple(gold, snap, m, c)) {
            bad += 1;
        }
    }
    (bad, citing)
}

// =============================================================================================
// Resolution
// =============================================================================================

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolutionScores {
    pub gold_mentions: usize,
    pub detected: usize,
    pub b3_precision: f64,
    pub b3_recall: f64,
    /// Gold mention index → system entity it was assigned to.
    pub assignment: HashMap<usize, EntityId>,
}

impl ResolutionScores {
    pub fn mention_recall(&self) -> f64 {
        ratio(self.detected, self.gold_mentions)
    }
    pub fn b3_f1(&self) -> f64 {
        f1(self.b3_precision, self.b3_recall)
    }
}

/// B³ over detected gold mentions. A gold mention (item, entity, surfaces) is detected when the
/// system has a mention in that item whose surface is one of the gold surfaces; it is assigned
/// to the system entity with most such mentions (ties: lowest id).
pub fn score_resolution(gold: &GoldIndex<'_>, snap: &Snapshot, ing: &Ingested) -> ResolutionScores {
    let mentions: Vec<(usize, &GoldMention)> = gold
        .fx
        .mentions
        .iter()
        .enumerate()
        .filter(|(_, m)| !gold.is_optional(&m.entity))
        .collect();
    // (item id, normalised surface) → system entities (with multiplicity).
    let mut sys: HashMap<(String, String), Vec<EntityId>> = HashMap::new();
    for m in &snap.mentions {
        let Some(item) = ing.by_ref(&m.item) else {
            continue;
        };
        let Some(e) = snap.entity(&m.entity) else {
            continue;
        };
        sys.entry((item.id.clone(), normalize_name(&m.surface)))
            .or_default()
            .push(e.id.clone());
    }
    let mut r = ResolutionScores {
        gold_mentions: mentions.len(),
        ..Default::default()
    };
    for (i, gm) in &mentions {
        let mut votes: BTreeMap<EntityId, usize> = BTreeMap::new();
        let surfaces: BTreeSet<String> = gm.surfaces.iter().map(|s| normalize_name(s)).collect();
        for s in surfaces {
            for e in sys.get(&(gm.item.clone(), s)).into_iter().flatten() {
                *votes.entry(e.clone()).or_default() += 1;
            }
        }
        // Ties go to the entity whose name sorts first: entity ids are random per run, so
        // breaking ties on them would make the score vary between identical runs.
        let name = |e: &EntityId| snap.entity(e).map(|x| x.name.clone()).unwrap_or_default();
        if let Some((e, _)) = votes.into_iter().max_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| name(&b.0).cmp(&name(&a.0)))
                .then(b.0.cmp(&a.0))
        }) {
            r.assignment.insert(*i, e);
        }
    }
    r.detected = r.assignment.len();
    if r.detected == 0 {
        return r;
    }
    let gold_of = |i: usize| gold.fx.mentions[i].entity.as_str();
    let mut by_gold: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut by_sys: HashMap<&EntityId, Vec<usize>> = HashMap::new();
    for (i, e) in &r.assignment {
        by_gold.entry(gold_of(*i)).or_default().push(*i);
        by_sys.entry(e).or_default().push(*i);
    }
    let (mut p, mut rc) = (0.0, 0.0);
    for (i, e) in &r.assignment {
        let g = &by_gold[gold_of(*i)];
        let s = &by_sys[e];
        let both = s.iter().filter(|j| gold_of(**j) == gold_of(*i)).count() as f64;
        p += both / s.len() as f64;
        rc += both / g.len() as f64;
    }
    r.b3_precision = p / r.detected as f64;
    r.b3_recall = rc / r.detected as f64;
    r
}

/// The system entities a gold entity's detected mentions were assigned to.
pub fn assigned_entities(
    gold: &GoldIndex<'_>,
    res: &ResolutionScores,
    gold_id: &str,
) -> BTreeSet<EntityId> {
    res.assignment
        .iter()
        .filter(|(i, _)| gold.fx.mentions[**i].entity == gold_id)
        .map(|(_, e)| e.clone())
        .collect()
}

/// Hard case: every detected mention of the person resolves to one system entity, which holds
/// no other gold entity's mentions.
pub fn same_identity(gold: &GoldIndex<'_>, res: &ResolutionScores, gold_id: &str) -> bool {
    let ents = assigned_entities(gold, res, gold_id);
    let total = gold
        .fx
        .mentions
        .iter()
        .filter(|m| m.entity == gold_id)
        .count();
    let detected = res
        .assignment
        .keys()
        .filter(|i| gold.fx.mentions[**i].entity == gold_id)
        .count();
    let Some(e) = ents.iter().next() else {
        return false;
    };
    ents.len() == 1
        && detected == total
        && res
            .assignment
            .iter()
            .all(|(i, x)| x != e || gold.fx.mentions[*i].entity == gold_id)
}

/// Hard case: the people resolve to pairwise different system entities.
pub fn distinct_identities(gold: &GoldIndex<'_>, res: &ResolutionScores, ids: &[String]) -> bool {
    let sets: Vec<BTreeSet<EntityId>> = ids
        .iter()
        .map(|g| assigned_entities(gold, res, g))
        .collect();
    sets.iter().all(|s| !s.is_empty())
        && sets
            .iter()
            .enumerate()
            .all(|(i, a)| sets[i + 1..].iter().all(|b| a.is_disjoint(b)))
}

/// Hard case: a name nothing identifies (a bare nickname or first name) is not attributed to
/// any of `people`. Fails when the system records, in that item, any mention (whatever its
/// surface) of an entity that `people`'s gold mentions resolve to: the items hold nobody else
/// the people could be. Optional gold mentions are not in [`ResolutionScores`], so this reads
/// the system's mentions directly.
pub fn unattributed(
    gold: &GoldIndex<'_>,
    res: &ResolutionScores,
    snap: &Snapshot,
    ing: &Ingested,
    people: &[String],
    cases: &[Unattributed],
) -> bool {
    let theirs: BTreeSet<EntityId> = people
        .iter()
        .flat_map(|p| assigned_entities(gold, res, p))
        .collect();
    cases.iter().all(|u| {
        !snap.mentions.iter().any(|m| {
            ing.by_ref(&m.item).is_some_and(|i| i.id == u.item)
                && snap
                    .entity(&m.entity)
                    .is_some_and(|e| theirs.contains(&e.id))
        })
    })
}

/// Hard case, strict form of [`same_identity`]: besides the majority vote, every system mention
/// in an item where the person has a gold mention, under one of that gold mention's surfaces,
/// is on the person's single entity. A nickname kept as an entity of its own ("Bob" next to
/// "Robert Ferrand" in the same item) fails here even when the vote over the item's surfaces
/// goes to the right entity.
pub fn resolved_strictly(
    gold: &GoldIndex<'_>,
    res: &ResolutionScores,
    snap: &Snapshot,
    ing: &Ingested,
    gold_id: &str,
) -> bool {
    if !same_identity(gold, res, gold_id) {
        return false;
    }
    let Some(entity) = assigned_entities(gold, res, gold_id).into_iter().next() else {
        return false;
    };
    gold.fx
        .mentions
        .iter()
        .filter(|g| g.entity == gold_id)
        .all(|g| {
            let surfaces: BTreeSet<String> = g.surfaces.iter().map(|s| normalize_name(s)).collect();
            snap.mentions.iter().all(|m| {
                !(ing.by_ref(&m.item).is_some_and(|i| i.id == g.item)
                    && surfaces.contains(&normalize_name(&m.surface)))
                    || snap.entity(&m.entity).is_some_and(|e| e.id == entity)
            })
        })
}

// =============================================================================================
// Retraction
// =============================================================================================

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RetractionScores {
    pub checks: usize,
    pub passed: usize,
    /// Active claims with evidence, and mentions, on deleted items.
    pub dangling: usize,
}

impl RetractionScores {
    pub fn correctness(&self) -> f64 {
        ratio(self.passed, self.checks)
    }
}

/// After deleting items: a gold claim supported only by deleted items must be gone; one with
/// support left that was active before must still be active; nothing may still cite or mention
/// a deleted item.
pub fn score_retraction(
    gt: &GoldTriples,
    gold: &GoldIndex<'_>,
    before: &BTreeMap<Triple, Vec<i64>>,
    after: &BTreeMap<Triple, Vec<i64>>,
    snap_after: &Snapshot,
    deleted_ids: &HashSet<String>,
    deleted: &HashSet<ItemRef>,
) -> RetractionScores {
    let mut r = RetractionScores::default();
    for c in gold
        .fx
        .claims
        .iter()
        .filter(|c| !c.optional && c.status == GoldStatus::Active)
    {
        let t = &gt.by_id[&c.id];
        let left = c.support.iter().any(|s| !deleted_ids.contains(&s.item));
        if !left {
            r.checks += 1;
            if !after.contains_key(t) {
                r.passed += 1;
            }
        } else if before.contains_key(t) && c.support.iter().any(|s| deleted_ids.contains(&s.item))
        {
            r.checks += 1;
            if after.contains_key(t) {
                r.passed += 1;
            }
        }
    }
    r.dangling = snap_after
        .active_claims()
        .filter(|c| c.evidence.iter().any(|e| deleted.contains(&e.item)))
        .count()
        + snap_after
            .mentions
            .iter()
            .filter(|m| deleted.contains(&m.item))
            .count();
    r
}

// =============================================================================================
// Vault
// =============================================================================================

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VaultScores {
    pub pages: usize,
    pub facts: usize,
    pub cited_facts: usize,
    /// Citations that do not check out (deleted or unknown item, foreign chunk, missing quote),
    /// on facts and history lines alike.
    pub invalid_citations: usize,
    /// History lines (superseded claims), not counted in `facts`.
    pub history: usize,
    /// History lines with at least one citation.
    pub cited_history: usize,
    /// Pages the render reported that are not on disk.
    pub missing_pages: usize,
}

impl VaultScores {
    pub fn coverage(&self) -> f64 {
        ratio(self.cited_facts, self.facts)
    }

    /// Share of history lines that carry a citation.
    pub fn history_coverage(&self) -> f64 {
        ratio(self.cited_history, self.history)
    }

    pub fn add(&mut self, o: &VaultScores) {
        self.pages += o.pages;
        self.facts += o.facts;
        self.cited_facts += o.cited_facts;
        self.invalid_citations += o.invalid_citations;
        self.history += o.history;
        self.cited_history += o.cited_history;
        self.missing_pages += o.missing_pages;
    }
}

pub fn score_vault(
    dir: &Path,
    v: &RenderedVault,
    ing: &Ingested,
    deleted: &HashSet<ItemRef>,
) -> VaultScores {
    let mut s = VaultScores {
        pages: v.pages.len(),
        ..Default::default()
    };
    for p in &v.pages {
        if !dir.join(&p.path).is_file() {
            s.missing_pages += 1;
        }
    }
    for f in v.facts() {
        s.facts += 1;
        if f.footnotes > 0 {
            s.cited_facts += 1;
        }
        s.invalid_citations += f
            .citations
            .iter()
            .filter(|e| !evidence_ok(e, ing, deleted))
            .count();
    }
    for f in v.history() {
        s.history += 1;
        if f.footnotes > 0 {
            s.cited_history += 1;
        }
        s.invalid_citations += f
            .citations
            .iter()
            .filter(|e| !evidence_ok(e, ing, deleted))
            .count();
    }
    s
}

/// A simulated user edit: text the user wrote into a vault page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserEdit {
    pub path: PathBuf,
    pub text: String,
}

/// Pick up to `n` pages (seeded) among those of people, organisations and projects, and add two
/// user blocks to each: a paragraph right after the frontmatter and a `## My notes` section at
/// the end. Returns the edits, to check after the re-render.
pub fn apply_user_edits(
    dir: &Path,
    pages: &[RenderedPage],
    eligible: &dyn Fn(&RenderedPage) -> bool,
    n: usize,
    seed: u64,
) -> Result<Vec<UserEdit>, BenchError> {
    let mut cands: Vec<&RenderedPage> = pages.iter().filter(|p| eligible(p)).collect();
    cands.sort_by(|a, b| a.path.cmp(&b.path));
    let mut rng = Rng::new(seed ^ 0x6b6e_6f77);
    for i in (1..cands.len()).rev() {
        let j = rng.below(i + 1);
        cands.swap(i, j);
    }
    let mut edits = Vec::new();
    for (k, p) in cands.into_iter().take(n).enumerate() {
        let path = dir.join(&p.path);
        let text = std::fs::read_to_string(&path)?;
        let top =
            format!("User note kb-edit-{k}-top: I met them at the spring offsite; follow up.");
        let bottom = format!("kb-edit-{k}-bottom: remember to ask about the Q1 roadmap.");
        let at = frontmatter_end(&text);
        let mut out = String::with_capacity(text.len() + 160);
        out.push_str(&text[..at]);
        if at > 0 && !text[..at].ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("\n{top}\n\n"));
        out.push_str(text[at..].trim_start_matches('\n'));
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("\n## My notes\n\n{bottom}\n"));
        std::fs::write(&path, out)?;
        edits.push(UserEdit {
            path: p.path.clone(),
            text: top,
        });
        edits.push(UserEdit {
            path: p.path.clone(),
            text: bottom,
        });
    }
    Ok(edits)
}

/// Byte offset just past a leading `---` … `---` frontmatter block (0 when there is none).
pub fn frontmatter_end(text: &str) -> usize {
    let Some(rest) = text.strip_prefix("---") else {
        return 0;
    };
    let Some(first_nl) = rest.find('\n') else {
        return 0;
    };
    if !rest[..first_nl].trim().is_empty() {
        return 0;
    }
    let mut pos = 3 + first_nl + 1;
    for line in text[pos..].split_inclusive('\n') {
        pos += line.len();
        if line.trim_end() == "---" {
            return pos;
        }
    }
    0
}

/// How many edits survive verbatim in their page.
pub fn edits_preserved(dir: &Path, edits: &[UserEdit]) -> usize {
    edits
        .iter()
        .filter(|e| {
            std::fs::read_to_string(dir.join(&e.path)).is_ok_and(|t| t.contains(e.text.trim()))
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_graph::{GraphStore, MemGraph, NewClaim, NewEntity};
    use nugget_store::Store;

    use crate::knowledge::fixture::{Fixture, ingest};

    #[test]
    fn predicates_canonicalise() {
        use EntityKind::*;
        let c = |p, s, o| canonical_predicate(p, Some(s), Some(o));
        assert_eq!(
            c("works for", Person, Organization),
            ("works_at".into(), false)
        );
        assert_eq!(
            c("Employed-By", Person, Organization),
            ("works_at".into(), false)
        );
        assert_eq!(c("org", Project, Organization), ("part_of".into(), false));
        assert_eq!(c("owner", Project, Person), ("leads".into(), true));
        assert_eq!(c("manages", Person, Project), ("leads".into(), false));
        assert_eq!(c("manages", Person, Person), ("reports_to".into(), true));
        assert_eq!(c("members", Project, Person), ("works_on".into(), true));
        assert_eq!(c("attendees", Event, Person), ("attended".into(), true));
        assert_eq!(c("location", Event, Place), ("held_at".into(), false));
        assert_eq!(c("deadline", Commitment, Topic), ("due_on".into(), false));
        assert_eq!(
            c("brother_of", Person, Person),
            ("sibling_of".into(), false)
        );
        assert_eq!(
            c("Resigned From", Person, Organization),
            ("resigned_from".into(), false)
        );
    }

    #[test]
    fn literals_normalise() {
        assert_eq!(normalize_literal("2026-12-01T09:00:00Z"), "2026-12-01");
        assert_eq!(normalize_literal("20261201T090000Z"), "2026-12-01");
        assert_eq!(normalize_literal("20261201"), "2026-12-01");
        assert_eq!(normalize_literal("  Head of  Data. "), "head of data");
        assert_eq!(normalize_literal("2026"), "2026");
    }

    #[test]
    fn frontmatter_end_finds_the_block() {
        assert_eq!(frontmatter_end("---\na: 1\n---\n# T\n"), 13);
        assert_eq!(frontmatter_end("# T\n---\n"), 0);
        assert_eq!(frontmatter_end("---\na: 1\n"), 0);
    }

    fn entity(g: &MemGraph, kind: EntityKind, name: &str, emails: &[&str]) -> EntityId {
        let mut n = NewEntity::new(kind, name);
        n.identifiers = emails
            .iter()
            .map(|e| Identifier::new("email", *e))
            .collect();
        g.create_entity(&n).unwrap()
    }

    fn claim(g: &MemGraph, s: &EntityId, p: &str, o: ClaimObject, ev: Evidence) -> i64 {
        g.add_claim(&NewClaim {
            subject: s.clone(),
            predicate: p.into(),
            object: o,
            text: "t".into(),
            confidence: 0.9,
            status: ClaimStatus::Active,
            evidence: vec![ev],
            valid_from: None,
            valid_to: None,
        })
        .unwrap()
    }

    #[test]
    fn entity_matching_counts_duplicates_and_merges() {
        let fx = Fixture::builtin().unwrap();
        let gold = GoldIndex::new(&fx);
        let g = MemGraph::new();
        // Maggie twice (an unmerged duplicate by her personal address), both Sams as one.
        let a = entity(
            &g,
            EntityKind::Person,
            "Margaret Thistlewood",
            &["margaret.thistlewood@cindergate.test"],
        );
        let b = entity(&g, EntityKind::Person, "Maggie T", &["maggie.t@mail.test"]);
        let sam = entity(&g, EntityKind::Person, "Sam", &[]);
        let junk = entity(&g, EntityKind::Person, "Nobody Atall", &[]);
        let prize = entity(
            &g,
            EntityKind::Person,
            "Prize Desk",
            &["winner@prize-desk.test"],
        );
        let snap = Snapshot::take(&g).unwrap();
        let m = match_entities(&gold, &snap);
        assert_eq!(m.strict.get(&a).map(String::as_str), Some("p-maggie"));
        assert!(!m.strict.contains_key(&b), "duplicate is not a hit");
        assert_eq!(m.loose.get(&b).map(String::as_str), Some("p-maggie"));
        assert!(m.strict.contains_key(&sam));
        assert!(!m.strict.contains_key(&junk) && !m.loose.contains_key(&junk));
        assert!(m.ignored.contains(&prize));
        let s = score_entities(&gold, &snap, &m);
        assert_eq!(s.overall.tp, 2);
        assert_eq!(s.overall.fp, 2, "duplicate Maggie and the unknown name");
        assert_eq!(s.overall.fn_, fx.stats().entities - 2);
    }

    #[test]
    fn vault_history_lines_are_checked_but_not_counted_as_facts() {
        use crate::knowledge::system::RenderedFact;
        let fx = Fixture::builtin().unwrap();
        let store = Store::open_in_memory().unwrap();
        let ing = ingest(&store, &fx).unwrap();
        let item = fx.item("mail-022").unwrap().item_ref();
        let chunk = ing.get("mail-022").unwrap().chunks[0].0;
        let ev = |q: &str| Evidence {
            item: item.clone(),
            chunk_id: Some(chunk),
            quote: Some(q.into()),
        };
        let fact = |history: bool, citations: Vec<Evidence>| RenderedFact {
            claim: Some(1),
            text: "- x".into(),
            footnotes: citations.len(),
            citations,
            history,
        };
        let v = RenderedVault {
            pages: vec![RenderedPage {
                entity: EntityId::new("ent_1"),
                path: "missing.md".into(),
                facts: vec![
                    fact(false, vec![ev("as Head of Data")]),
                    fact(true, vec![ev("as Head of Data")]),
                    fact(true, vec![ev("not in the mail at all")]),
                    fact(true, Vec::new()),
                ],
            }],
        };
        let s = score_vault(Path::new("/nonexistent"), &v, &ing, &HashSet::new());
        assert_eq!((s.facts, s.cited_facts), (1, 1));
        assert_eq!((s.history, s.cited_history), (3, 2));
        assert_eq!(s.invalid_citations, 1);
        assert_eq!(v.facts().count(), 1);
    }

    #[test]
    fn claims_score_after_resolution_and_supersession() {
        let fx = Fixture::builtin().unwrap();
        let store = Store::open_in_memory().unwrap();
        let ing = ingest(&store, &fx).unwrap();
        let gold = GoldIndex::new(&fx);
        let gt = gold_triples(&gold);
        let g = MemGraph::new();
        let priya = entity(
            &g,
            EntityKind::Person,
            "Priya Castellane",
            &["priya.castellane@cindergate.test"],
        );
        let cinder = entity(&g, EntityKind::Organization, "Cindergate", &[]);
        let quill = entity(&g, EntityKind::Organization, "Quillfeather Systems", &[]);
        let item = fx.item("mail-022").unwrap().item_ref();
        let chunk = ing.get("mail-022").unwrap().chunks[0].0;
        let ev = |q: &str| Evidence {
            item: item.clone(),
            chunk_id: Some(chunk),
            quote: Some(q.into()),
        };
        claim(
            &g,
            &priya,
            "employed by",
            ClaimObject::Entity(quill),
            ev("I've joined Quillfeather Systems as Head of Data"),
        );
        claim(
            &g,
            &priya,
            "job title",
            ClaimObject::Literal("head of data".into()),
            ev("as Head of Data"),
        );
        // Still active: the superseded employer, with a quote that is not in the chunk.
        claim(
            &g,
            &priya,
            "works_at",
            ClaimObject::Entity(cinder),
            ev("Priya works at Cindergate"),
        );
        let snap = Snapshot::take(&g).unwrap();
        let m = match_entities(&gold, &snap);
        let sys = system_triples(&gold, &snap, &m);
        let s = score_claims(&gt, &sys, &snap);
        assert_eq!(s.prf.tp, 2);
        assert_eq!(s.prf.fp, 1);
        assert_eq!(s.superseded_active, 1);
        let sup = score_support(&fx, &snap, &ing, &HashSet::new());
        assert_eq!((sup.claims, sup.unsupported), (3, 1));
        // Deleting the item makes every claim unsupported.
        let gone: HashSet<ItemRef> = [item].into_iter().collect();
        assert_eq!(score_support(&fx, &snap, &ing, &gone).unsupported, 3);
    }

    #[test]
    fn b3_rewards_merging_and_penalises_overmerging() {
        let fx = Fixture::builtin().unwrap();
        let store = Store::open_in_memory().unwrap();
        let ing = ingest(&store, &fx).unwrap();
        let gold = GoldIndex::new(&fx);
        // Perfect: one system entity per gold entity, a mention for every gold mention.
        let perfect = MemGraph::new();
        let mut ids = HashMap::new();
        for e in fx.entities.iter().filter(|e| !e.optional) {
            ids.insert(e.id.clone(), entity(&perfect, e.kind, &e.name, &[]));
        }
        for m in &fx.mentions {
            let Some(id) = ids.get(&m.entity) else {
                continue;
            };
            perfect
                .add_mention(&Mention {
                    entity: id.clone(),
                    item: fx.item(&m.item).unwrap().item_ref(),
                    chunk_id: None,
                    surface: m.surfaces[0].clone(),
                    confidence: 1.0,
                })
                .unwrap();
        }
        let snap = Snapshot::take(&perfect).unwrap();
        let r = score_resolution(&gold, &snap, &ing);
        assert_eq!(r.mention_recall(), 1.0);
        assert_eq!((r.b3_precision, r.b3_recall), (1.0, 1.0));
        assert!(same_identity(&gold, &r, "p-maggie"));
        let sams = vec!["p-samo".to_owned(), "p-samw".to_owned()];
        assert!(distinct_identities(&gold, &r, &sams));
        // Over-merge the two Sams.
        perfect.merge(&ids["p-samo"], &ids["p-samw"]).unwrap();
        let snap = Snapshot::take(&perfect).unwrap();
        let r = score_resolution(&gold, &snap, &ing);
        assert!(r.b3_precision < 1.0 && r.b3_recall == 1.0);
        assert!(!distinct_identities(&gold, &r, &sams));
    }
}
