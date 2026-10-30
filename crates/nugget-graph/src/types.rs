use nugget_core::ids::{EntityId, ItemRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

pub type ClaimId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Person,
    Organization,
    Project,
    Commitment,
    Preference,
    Event,
    Place,
    Topic,
}

impl EntityKind {
    pub const ALL: [EntityKind; 8] = [
        Self::Person,
        Self::Organization,
        Self::Project,
        Self::Commitment,
        Self::Preference,
        Self::Event,
        Self::Place,
        Self::Topic,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Organization => "organization",
            Self::Project => "project",
            Self::Commitment => "commitment",
            Self::Preference => "preference",
            Self::Event => "event",
            Self::Place => "place",
            Self::Topic => "topic",
        }
    }
}

/// A strong identifier: two entities with the same identifier are the same entity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identifier {
    /// `email`, `phone`, `url`, `domain`, `handle:<service>`, `ical_uid`, ...
    pub scheme: String,
    pub value: String,
}

impl Identifier {
    pub fn new(scheme: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            scheme: scheme.into(),
            value: value.into(),
        }
    }

    /// Canonical form used for lookups: lowercase scheme; lowercase trimmed value for
    /// case-insensitive schemes (email, domain, url host), digits-and-plus for phones.
    pub fn normalized(&self) -> Self {
        let scheme = self.scheme.trim().to_ascii_lowercase();
        let v = self.value.trim();
        let value = match scheme.as_str() {
            "phone" => v
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '+')
                .collect(),
            "email" | "domain" | "url" => v.to_lowercase(),
            _ => v.to_owned(),
        };
        Self { scheme, value }
    }
}

/// Normalise a name or alias for matching: lowercase, whitespace collapsed, surrounding
/// punctuation trimmed.
pub fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewEntity {
    pub kind: EntityKind,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub identifiers: Vec<Identifier>,
    /// Kind-specific attributes (e.g. a commitment's due date). JSON object.
    #[serde(default)]
    pub attributes: Value,
}

impl NewEntity {
    pub fn new(kind: EntityKind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
            aliases: Vec::new(),
            identifiers: Vec::new(),
            attributes: Value::Object(Default::default()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub kind: EntityKind,
    pub name: String,
    pub aliases: Vec<String>,
    pub identifiers: Vec<Identifier>,
    pub attributes: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Ids merged into this entity (they resolve here).
    pub merged_ids: Vec<EntityId>,
}

/// Fields to change; `None` leaves a field as is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EntityPatch {
    pub name: Option<String>,
    pub kind: Option<EntityKind>,
    /// Merged into the existing attributes (top-level keys replaced).
    pub attributes: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EntityQuery {
    /// Substring match on name and aliases (case-insensitive).
    pub text: Option<String>,
    pub kind: Option<EntityKind>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntitySummary {
    pub id: EntityId,
    pub kind: EntityKind,
    pub name: String,
    pub claims: u64,
    pub mentions: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ClaimObject {
    Entity(EntityId),
    Literal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Active,
    /// Replaced by a newer claim (e.g. a changed job title).
    Superseded,
    /// Lost its evidence (its sources changed or were deleted).
    Retracted,
    /// Written by the user in the vault; needs no source evidence.
    UserAsserted,
    /// Rejected by the user. Sticky: re-extracting its sources adds evidence to it but never
    /// makes it active again, and losing its evidence does not turn it into `Retracted`. Only an
    /// explicit status change (or a user assertion of the same triple) revives it.
    Rejected,
}

impl ClaimStatus {
    pub const ALL: [ClaimStatus; 5] = [
        Self::Active,
        Self::Superseded,
        Self::Retracted,
        Self::UserAsserted,
        Self::Rejected,
    ];

    /// Shown as a current fact: `Active` or `UserAsserted`.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active | Self::UserAsserted)
    }

    /// Statuses that take part in triple de-duplication: at most one claim per
    /// `(subject, predicate, object)` is `Active`, `UserAsserted` or `Rejected` at any time.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Active | Self::UserAsserted | Self::Rejected)
    }

    /// A decision the user made (it needs no source evidence).
    pub fn is_user_decision(self) -> bool {
        matches!(self, Self::UserAsserted | Self::Rejected)
    }

    /// The wire name (`active`, `superseded`, `retracted`, `user_asserted`, `rejected`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Retracted => "retracted",
            Self::UserAsserted => "user_asserted",
            Self::Rejected => "rejected",
        }
    }

    /// Inverse of [`ClaimStatus::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Status of the survivor when live claims of one triple fold together: `UserAsserted` over
    /// `Rejected` over `Active`.
    pub fn strongest(self, other: Self) -> Self {
        let rank = |s: Self| match s {
            Self::UserAsserted => 3,
            Self::Rejected => 2,
            Self::Active => 1,
            _ => 0,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// Longest supporting quote kept in [`Evidence::quote`], in characters. Longer quotes are cut
/// (at a character boundary) by every [`GraphStore`](crate::GraphStore) implementation.
pub const MAX_QUOTE_CHARS: usize = 300;

/// `q` cut to [`MAX_QUOTE_CHARS`] characters.
pub fn clamp_quote(q: &str) -> String {
    match q.char_indices().nth(MAX_QUOTE_CHARS) {
        Some((i, _)) => q[..i].to_owned(),
        None => q.to_owned(),
    }
}

impl Evidence {
    /// This evidence with its quote cut to [`MAX_QUOTE_CHARS`].
    pub fn clamped(&self) -> Evidence {
        Evidence {
            item: self.item.clone(),
            chunk_id: self.chunk_id,
            quote: self.quote.as_deref().map(clamp_quote),
        }
    }
}

/// Where a claim or mention comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Evidence {
    pub item: ItemRef,
    /// Chunk the quote is in, when known.
    pub chunk_id: Option<i64>,
    /// Short supporting quote from the source, at most [`MAX_QUOTE_CHARS`] characters (stores
    /// cut longer ones).
    pub quote: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewClaim {
    pub subject: EntityId,
    /// snake_case relation, e.g. `works_at`, `prefers`, `due_on`, `attended`, `owes`.
    pub predicate: String,
    pub object: ClaimObject,
    /// One human-readable sentence stating the fact.
    pub text: String,
    /// 0.0 – 1.0.
    pub confidence: f32,
    pub status: ClaimStatus,
    pub evidence: Vec<Evidence>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub valid_from: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub valid_to: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub id: ClaimId,
    pub subject: EntityId,
    pub predicate: String,
    pub object: ClaimObject,
    pub text: String,
    pub confidence: f32,
    pub status: ClaimStatus,
    pub evidence: Vec<Evidence>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub valid_from: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub valid_to: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// An entity appearing in a chunk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mention {
    pub entity: EntityId,
    pub item: ItemRef,
    pub chunk_id: Option<i64>,
    /// The surface text, e.g. "Sam" for entity "Samantha Lee".
    pub surface: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: EntityId,
    pub kind: EntityKind,
    pub name: String,
    /// Hops from the centre.
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub claim: ClaimId,
    pub from: EntityId,
    pub to: EntityId,
    pub predicate: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Set when `limit` cut the traversal short.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphCounts {
    pub entities: u64,
    pub claims: u64,
    pub mentions: u64,
}

/// Fields of a claim to change with [`GraphStore::update_claim`](crate::GraphStore::update_claim);
/// `None` leaves a field as is. `valid_from` / `valid_to` take `Some(None)` to clear the bound.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaimPatch {
    /// 0.0 – 1.0. Replaces the confidence (it may go down).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_opt_time"
    )]
    pub valid_from: Option<Option<OffsetDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_opt_time"
    )]
    pub valid_to: Option<Option<OffsetDateTime>>,
    /// The human-readable sentence. Must not be blank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl ClaimPatch {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// `Option<Option<OffsetDateTime>>` as "absent / null / RFC 3339 string".
mod opt_opt_time {
    use serde::{Deserializer, Serializer};
    use time::OffsetDateTime;

    pub fn serialize<S: Serializer>(
        v: &Option<Option<OffsetDateTime>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => time::serde::rfc3339::option::serialize(inner, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<OffsetDateTime>>, D::Error> {
        time::serde::rfc3339::option::deserialize(d).map(Some)
    }
}

/// An alias or identifier of an entity, as a provenance key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum EntityDetail {
    /// Matched by [`normalize_name`].
    Alias(String),
    /// Matched by [`Identifier::normalized`].
    Identifier(Identifier),
}

impl EntityDetail {
    /// The form provenance is keyed by: `(kind, scheme, value)` with the value normalised.
    pub fn key(&self) -> (&'static str, String, String) {
        match self {
            Self::Alias(a) => ("alias", String::new(), normalize_name(a)),
            Self::Identifier(i) => {
                let n = i.normalized();
                ("identifier", n.scheme, n.value)
            }
        }
    }
}

/// Which items supplied each alias and identifier of an entity
/// ([`GraphStore::entity_provenance`](crate::GraphStore::entity_provenance)). Every current alias
/// and identifier is listed, in the entity's order; an empty item list means no item is recorded
/// for it (added by the user, by a merge or split, before provenance was kept, or its items were
/// retracted).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EntityProvenance {
    pub aliases: Vec<(String, Vec<ItemRef>)>,
    pub identifiers: Vec<(Identifier, Vec<ItemRef>)>,
}

/// One write in a [`GraphBatch`]. Each mirrors the [`GraphStore`](crate::GraphStore) method of
/// the same name and has the same rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GraphOp {
    CreateEntity {
        entity: NewEntity,
    },
    UpdateEntity {
        entity: EntityId,
        patch: EntityPatch,
    },
    AddAlias {
        entity: EntityId,
        alias: String,
    },
    AddIdentifier {
        entity: EntityId,
        identifier: Identifier,
    },
    /// Record that `item` supplied `detail` of `entity`.
    AddDetailSource {
        entity: EntityId,
        detail: EntityDetail,
        item: ItemRef,
    },
    AddClaim {
        claim: NewClaim,
    },
    AddEvidence {
        claim: ClaimId,
        evidence: Evidence,
    },
    SetClaimStatus {
        claim: ClaimId,
        status: ClaimStatus,
    },
    UpdateClaim {
        claim: ClaimId,
        patch: ClaimPatch,
    },
    AddMention {
        mention: Mention,
    },
    RetractItem {
        item: ItemRef,
    },
}

/// Writes applied together by [`GraphStore::apply_batch`](crate::GraphStore::apply_batch), in
/// order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphBatch {
    pub ops: Vec<GraphOp>,
}

impl GraphBatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, op: GraphOp) -> usize {
        self.ops.push(op);
        self.ops.len() - 1
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// What one [`GraphOp`] returned, at the same index as the op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum BatchResult {
    /// Ops without a result.
    Done,
    /// `CreateEntity`.
    Entity(EntityId),
    /// `AddClaim`: the claim the triple landed in.
    Claim(ClaimId),
    /// `RetractItem`: the affected claims.
    Claims(Vec<ClaimId>),
}

impl BatchResult {
    pub fn claim(&self) -> Option<ClaimId> {
        match self {
            Self::Claim(c) => Some(*c),
            _ => None,
        }
    }

    pub fn entity(&self) -> Option<&EntityId> {
        match self {
            Self::Entity(e) => Some(e),
            _ => None,
        }
    }
}

/// Entity attribute holding [`MergeRecord`]s: why other entities were merged into this one.
pub const MERGE_LOG_ATTR: &str = "merge_log";

/// Why entity resolution merged one entity (or occurrence) into another, kept in the surviving
/// entity's attributes under [`MERGE_LOG_ATTR`] so the vault can show it and the user can split
/// the two again (`knowledge.split`). Written by `nugget-knowledge`; text fields come from
/// source content and are untrusted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeRecord {
    /// The merged-away entity (now a tombstone resolving to the survivor), or `None` when an
    /// occurrence was folded in before it became an entity of its own.
    #[serde(default)]
    pub from: Option<EntityId>,
    /// The name the merged entity or occurrence went by.
    pub name: String,
    /// Machine-readable reasons, e.g. `shared_email`, `trusted_record`, `nickname`, `initial`,
    /// `email_local_part`, `same_item`, `same_thread`, `same_org`, `same_name`.
    #[serde(default)]
    pub reasons: Vec<String>,
    /// One human-readable sentence.
    #[serde(default)]
    pub detail: String,
    /// The item that supplied the corroborating evidence, when there is one.
    #[serde(default)]
    pub item: Option<ItemRef>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub at: Option<OffsetDateTime>,
}

/// The merge records of an entity (empty when none or malformed), oldest first.
pub fn merge_log(e: &Entity) -> Vec<MergeRecord> {
    merge_log_of(&e.attributes)
}

/// [`merge_log`] over raw attributes.
pub fn merge_log_of(attributes: &Value) -> Vec<MergeRecord> {
    attributes
        .get(MERGE_LOG_ATTR)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod merge_log_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_log_reads_records_and_skips_malformed_ones() {
        let attrs = json!({MERGE_LOG_ATTR: [
            {"from": "ent_a", "name": "Maggie", "reasons": ["nickname", "same_item"],
             "detail": "x", "at": "2026-10-01T00:00:00Z"},
            {"bogus": true},
            {"name": "M. T"}
        ]});
        let log = merge_log_of(&attrs);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].from.as_ref().map(EntityId::as_str), Some("ent_a"));
        assert_eq!(log[0].reasons, ["nickname", "same_item"]);
        assert!(log[1].from.is_none() && log[1].reasons.is_empty());
        assert!(merge_log_of(&json!({})).is_empty());
        assert!(merge_log_of(&json!({MERGE_LOG_ATTR: "nope"})).is_empty());
    }
}
