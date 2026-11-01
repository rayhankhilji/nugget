//! What extractors produce: entity occurrences and claims about them, still unresolved.

use std::collections::HashMap;

use nugget_graph::{EntityKind, Evidence, Identifier, normalize_name};
use serde_json::{Map, Value};
use time::OffsetDateTime;

/// A reference to an entity within one item's extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ERef {
    /// The user.
    Me,
    /// Index into [`Extraction::entities`].
    Local(usize),
}

/// Where an entity appears.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MentionSpec {
    pub surface: String,
    pub chunk_id: Option<i64>,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EntitySpec {
    pub kind: EntityKind,
    pub name: String,
    pub aliases: Vec<String>,
    /// Normalised.
    pub identifiers: Vec<Identifier>,
    pub attributes: Map<String, Value>,
    pub mentions: Vec<MentionSpec>,
    /// The name is only a placeholder (an address): never match on it, and replace it once a
    /// real name is known.
    pub weak_name: bool,
    /// The identifiers were asserted together by a trusted record (a contact card): entities
    /// holding them may be merged.
    pub trusted_identifiers: bool,
    /// Other occurrences folded into this one by a short-name rule ("Maggie" into "Maggie T" on
    /// the same item): each becomes a merge record on the entity this one resolves to.
    pub folds: Vec<NameFold>,
}

/// An occurrence folded into another before resolution, and why.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NameFold {
    pub name: String,
    pub reasons: Vec<String>,
    pub detail: String,
}

impl EntitySpec {
    pub fn new(kind: EntityKind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
            aliases: Vec::new(),
            identifiers: Vec::new(),
            attributes: Map::new(),
            mentions: Vec::new(),
            weak_name: false,
            trusted_identifiers: false,
            folds: Vec::new(),
        }
    }

    pub fn ident(mut self, scheme: &str, value: &str) -> Self {
        let i = Identifier::new(scheme, value).normalized();
        if !i.value.is_empty() && !self.identifiers.contains(&i) {
            self.identifiers.push(i);
        }
        self
    }

    pub fn mention(mut self, surface: &str, chunk_id: Option<i64>, confidence: f32) -> Self {
        self.mentions.push(MentionSpec {
            surface: surface.to_owned(),
            chunk_id,
            confidence,
        });
        self
    }

    pub fn attr(mut self, key: &str, value: Value) -> Self {
        if !value.is_null() {
            self.attributes.insert(key.to_owned(), value);
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OSpec {
    Entity(ERef),
    Literal(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClaimSpec {
    pub subject: ERef,
    pub predicate: String,
    pub object: OSpec,
    pub text: String,
    pub confidence: f32,
    pub evidence: Evidence,
    pub valid_from: Option<OffsetDateTime>,
}

/// A wikilink from a note's subject entity to another note or entity, resolved at apply time.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LinkSpec {
    pub from: ERef,
    pub target: String,
    pub evidence: Evidence,
    pub confidence: f32,
}

/// One item's extraction. Entities are de-duplicated as they are added (same identifier, or
/// same kind and normalised name).
#[derive(Debug, Default, Clone)]
pub(crate) struct Extraction {
    pub entities: Vec<EntitySpec>,
    pub claims: Vec<ClaimSpec>,
    pub links: Vec<LinkSpec>,
    /// Addresses learned to be the user's (from a sent folder).
    pub me_emails: Vec<String>,
    /// A display name for the user learned from sent mail.
    pub me_name: Option<String>,
    /// Subject entities of this note, for wikilink resolution from other notes.
    pub note_subjects: Vec<ERef>,
    /// Names other notes may use to link here (title, file stem, aliases).
    pub note_names: Vec<String>,
    by_ident: HashMap<Identifier, usize>,
    by_name: HashMap<(EntityKind, String), usize>,
}

impl Extraction {
    /// Add an entity occurrence, merging it into an earlier one that shares an identifier or
    /// (when neither has a weak name) its kind and normalised name.
    pub fn entity(&mut self, spec: EntitySpec) -> ERef {
        let existing = spec
            .identifiers
            .iter()
            .find_map(|i| self.by_ident.get(i).copied())
            .filter(|&i| self.entities[i].kind == spec.kind)
            .or_else(|| {
                (!spec.weak_name)
                    .then(|| {
                        self.by_name
                            .get(&(spec.kind, normalize_name(&spec.name)))
                            .copied()
                    })
                    .flatten()
                    .filter(|&i| !conflicting(&self.entities[i], &spec))
            });
        let idx = match existing {
            Some(i) => {
                let e = &mut self.entities[i];
                if e.weak_name && !spec.weak_name {
                    let old = std::mem::replace(&mut e.name, spec.name.clone());
                    e.aliases.push(old);
                    e.weak_name = false;
                } else if normalize_name(&e.name) != normalize_name(&spec.name) && !spec.weak_name {
                    e.aliases.push(spec.name.clone());
                }
                for a in spec.aliases {
                    if !e.aliases.contains(&a) {
                        e.aliases.push(a);
                    }
                }
                for id in spec.identifiers {
                    if !e.identifiers.contains(&id) {
                        e.identifiers.push(id);
                    }
                }
                for (k, v) in spec.attributes {
                    e.attributes.entry(k).or_insert(v);
                }
                e.mentions.extend(spec.mentions);
                e.trusted_identifiers |= spec.trusted_identifiers;
                for f in spec.folds {
                    if !e.folds.contains(&f) {
                        e.folds.push(f);
                    }
                }
                i
            }
            None => {
                self.entities.push(spec);
                self.entities.len() - 1
            }
        };
        let e = &self.entities[idx];
        for id in &e.identifiers {
            self.by_ident.entry(id.clone()).or_insert(idx);
        }
        if !e.weak_name {
            self.by_name
                .entry((e.kind, normalize_name(&e.name)))
                .or_insert(idx);
        }
        ERef::Local(idx)
    }

    pub fn claim(&mut self, c: ClaimSpec) {
        if let OSpec::Entity(o) = &c.object
            && *o == c.subject
        {
            return; // no self-loops
        }
        self.claims.push(c);
    }

    /// Display name of a reference, for claim text.
    pub fn name_of(&self, r: ERef, me: &str) -> String {
        match r {
            ERef::Me => me.to_owned(),
            ERef::Local(i) => self.entities[i].name.clone(),
        }
    }

    /// Append another extraction (e.g. the LLM's) to this one, re-mapping its references.
    pub fn absorb(&mut self, other: Extraction) {
        let mut map = Vec::with_capacity(other.entities.len());
        for e in other.entities {
            map.push(self.entity(e));
        }
        let remap = |r: ERef| match r {
            ERef::Me => ERef::Me,
            ERef::Local(i) => map[i],
        };
        for mut c in other.claims {
            c.subject = remap(c.subject);
            if let OSpec::Entity(o) = &mut c.object {
                *o = remap(*o);
            }
            self.claim(c);
        }
        for mut l in other.links {
            l.from = remap(l.from);
            self.links.push(l);
        }
    }
}

/// Two occurrences conflict when they carry different values for a strong identifier scheme.
pub(crate) fn conflicting(a: &EntitySpec, b: &EntitySpec) -> bool {
    crate::resolve::identifiers_conflict(&a.identifiers, &b.identifiers)
}
