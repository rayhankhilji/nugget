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
    /// Lost its evidence, or rejected by the user.
    Retracted,
    /// Written by the user in the vault; needs no source evidence.
    UserAsserted,
}

impl ClaimStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active | Self::UserAsserted)
    }
}

/// Where a claim or mention comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Evidence {
    pub item: ItemRef,
    /// Chunk the quote is in, when known.
    pub chunk_id: Option<i64>,
    /// Short supporting quote (≤ 300 chars) from the source.
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
