//! `knowledge --knowledge-dump FILE`: what the system got wrong, one JSON object per line, so a
//! real-model run says where its precision and recall went.
//!
//! | `type` | One line per |
//! |---|---|
//! | `summary` | run (first line): counts of everything below by kind, source and reason |
//! | `entity_fp` | system entity that matches no gold entity (or is a second match: a duplicate) |
//! | `entity_fn` | gold entity no system entity matched |
//! | `claim_fp` | active system triple not in the gold labels (in the gold vocabulary) |
//! | `claim_fn` | active gold triple the system does not have |
//! | `request` | real-model extraction request: latency, tokens, the answer, and the quote gate's verdict on every proposed entity and claim |
//!
//! **Source.** With a deterministic baseline (the same corpus extracted with an LLM that
//! answers nothing, [`Baseline`]), each false positive says whether the deterministic
//! extractors already produce it (`deterministic`) or only the LLM run does (`llm`), and each
//! false negative whether the baseline found it (`in_baseline`). Entities compare by kind and
//! normalised name, triples by their canonical form with unmatched entities by name.
//!
//! **Reasons.** `entity_fp`: `duplicate` (its best gold entity is taken by another system
//! entity), `kind_mismatch` (a gold entity of another kind has this name), else `not_in_gold`.
//! `claim_fp`: `superseded`, `subject_unmatched`, `object_unmatched`, `wrong_object` (the gold
//! labels have this subject and predicate with another object), `wrong_predicate` (this subject
//! and object under another predicate), else `not_in_gold`. `claim_fn`: `subject_missing` /
//! `object_missing` (no system entity matched it), `wrong_predicate`, `wrong_object`, else
//! `absent`.
//!
//! Existing metric names are untouched: the dump only writes the file and records its path and
//! summary in the report's parameters.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use nugget_core::capability::Locality;
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use nugget_graph::{ClaimObject, EntityKind, normalize_name};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, StopReason,
};
use serde_json::{Value, json};

use super::eval::{EntityMatching, GoldIndex, GoldTriples, Node, Snapshot, Triple};
use super::fixture::Ingested;
use super::metered::RequestLog;
use crate::runner::BenchError;

/// An LLM that finds nothing: with it the pipeline runs its deterministic extractors only.
pub struct EmptyLlm(ProviderId);

impl Default for EmptyLlm {
    fn default() -> Self {
        Self(ProviderId::new("bench-empty-llm"))
    }
}

#[async_trait]
impl Provider for EmptyLlm {
    fn id(&self) -> &ProviderId {
        &self.0
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "empty".into(),
            locality: Locality::OnDevice,
            context_tokens: Some(32_768),
        }
    }

    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
        let events = vec![
            Ok(ChatEvent::TextDelta(
                json!({"entities": [], "claims": []}).to_string(),
            )),
            Ok(ChatEvent::Done {
                stop_reason: StopReason::EndTurn,
                input_tokens: None,
                output_tokens: None,
            }),
        ];
        Ok(Box::pin(stream::iter(events)))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported("the empty LLM does not embed".into()))
    }
}

/// What the deterministic extractors alone produce on the corpus.
#[derive(Debug, Clone, Default)]
pub struct Baseline {
    /// (kind, normalised name or alias) of every entity.
    entities: HashSet<(EntityKind, String)>,
    /// Gold entities strictly matched.
    matched: HashSet<String>,
    /// Canonical triple keys of active claims.
    triples: HashSet<String>,
}

impl Baseline {
    pub fn new(gold: &GoldIndex<'_>, snap: &Snapshot) -> Self {
        let m = super::eval::match_entities(gold, snap);
        let mut b = Baseline::default();
        for e in &snap.entities {
            for n in std::iter::once(&e.name).chain(&e.aliases) {
                b.entities.insert((e.kind, normalize_name(n)));
            }
        }
        b.matched = m.strict.values().cloned().collect();
        for t in super::eval::system_triples(gold, snap, &m).keys() {
            b.triples.insert(triple_key(t, snap));
        }
        b
    }

    fn has_entity(&self, kind: EntityKind, names: &[String]) -> bool {
        names
            .iter()
            .any(|n| self.entities.contains(&(kind, normalize_name(n))))
    }
}

fn node_key(n: &Node, snap: &Snapshot) -> String {
    match n {
        Node::Gold(g) => format!("gold:{g}"),
        Node::Literal(l) => format!("literal:{l}"),
        Node::Unmatched(id) => format!(
            "name:{}",
            snap.entity(id)
                .map(|e| normalize_name(&e.name))
                .unwrap_or_default()
        ),
    }
}

fn triple_key(t: &Triple, snap: &Snapshot) -> String {
    format!(
        "{} {} {}",
        node_key(&t.subject, snap),
        t.predicate,
        node_key(&t.object, snap)
    )
}

fn node_label(n: &Node, gold: &GoldIndex<'_>, snap: &Snapshot) -> String {
    match n {
        Node::Gold(g) => gold
            .fx
            .entity(g)
            .map_or_else(|| g.clone(), |e| e.name.clone()),
        Node::Literal(l) => format!("'{l}'"),
        Node::Unmatched(id) => snap
            .entity(id)
            .map_or_else(|| id.to_string(), |e| format!("{} (unmatched)", e.name)),
    }
}

/// Everything the dump is computed from.
pub struct DumpInput<'a> {
    pub gold: &'a GoldIndex<'a>,
    pub gt: &'a GoldTriples,
    pub snap: &'a Snapshot,
    pub matching: &'a EntityMatching,
    pub sys: &'a BTreeMap<Triple, Vec<i64>>,
    pub ing: &'a Ingested,
    pub baseline: Option<&'a Baseline>,
    /// Real-model requests with the gate's verdicts already attached (`request` lines).
    pub requests: Vec<Value>,
}

/// The dump's lines: a `summary` first, then the rest.
pub fn lines(d: &DumpInput<'_>) -> Vec<Value> {
    let (gold, snap, m) = (d.gold, d.snap, d.matching);
    let item_id = |r: &nugget_core::ids::ItemRef| {
        d.ing
            .by_ref(r)
            .map_or_else(|| r.key.to_string(), |i| i.id.clone())
    };
    let mut out: Vec<Value> = Vec::new();

    // ---- entities ----
    let taken: HashSet<&str> = m.strict.values().map(String::as_str).collect();
    let mut mention_items: HashMap<&nugget_core::ids::EntityId, Vec<String>> = HashMap::new();
    for mn in &snap.mentions {
        if let Some(e) = snap.entity(&mn.entity) {
            let v = mention_items.entry(&e.id).or_default();
            let id = item_id(&mn.item);
            if !v.contains(&id) {
                v.push(id);
            }
        }
    }
    for e in &snap.entities {
        if m.ignored.contains(&e.id) || m.strict.contains_key(&e.id) {
            continue;
        }
        let names: Vec<String> = std::iter::once(e.name.clone())
            .chain(e.aliases.clone())
            .collect();
        let cands = gold.candidates(e);
        let other_kind: Vec<String> = gold
            .fx
            .entities
            .iter()
            .filter(|g| g.kind != e.kind)
            .filter(|g| {
                let gs: HashSet<String> = std::iter::once(&g.name)
                    .chain(&g.surfaces)
                    .map(|s| normalize_name(s))
                    .collect();
                names.iter().any(|n| gs.contains(&normalize_name(n)))
            })
            .map(|g| format!("{} ({})", g.id, g.kind.as_str()))
            .collect();
        let (reason, near) = if let Some((_, g)) = cands.first() {
            ("duplicate", json!(g))
        } else if !other_kind.is_empty() {
            ("kind_mismatch", json!(other_kind))
        } else {
            ("not_in_gold", Value::Null)
        };
        let source = d.baseline.map(|b| {
            if b.has_entity(e.kind, &names) {
                "deterministic"
            } else {
                "llm"
            }
        });
        out.push(json!({
            "type": "entity_fp",
            "kind": e.kind.as_str(),
            "name": e.name,
            "aliases": e.aliases,
            "identifiers": e.identifiers.iter().map(|i| format!("{}:{}", i.scheme, i.value)).collect::<Vec<_>>(),
            "items": mention_items.get(&e.id).cloned().unwrap_or_default(),
            "reason": reason,
            "near": near,
            "source": source,
        }));
    }
    for g in gold.fx.entities.iter().filter(|g| !g.optional) {
        if taken.contains(g.id.as_str()) {
            continue;
        }
        let surfaces: HashSet<String> = std::iter::once(&g.name)
            .chain(&g.surfaces)
            .map(|s| normalize_name(s))
            .collect();
        let kind_mismatch: Vec<String> = snap
            .entities
            .iter()
            .filter(|e| e.kind != g.kind)
            .filter(|e| {
                std::iter::once(&e.name)
                    .chain(&e.aliases)
                    .any(|n| surfaces.contains(&normalize_name(n)))
            })
            .map(|e| format!("{} ({})", e.name, e.kind.as_str()))
            .collect();
        let mut items: Vec<&str> = gold
            .fx
            .mentions
            .iter()
            .filter(|mn| mn.entity == g.id)
            .map(|mn| mn.item.as_str())
            .collect();
        items.dedup();
        out.push(json!({
            "type": "entity_fn",
            "kind": g.kind.as_str(),
            "gold": g.id,
            "name": g.name,
            "items": items,
            "reason": if kind_mismatch.is_empty() { "absent" } else { "kind_mismatch" },
            "near": kind_mismatch,
            "in_baseline": d.baseline.map(|b| b.matched.contains(&g.id)),
        }));
    }

    // ---- claims ----
    let claims_by_id: HashMap<i64, &nugget_graph::Claim> =
        snap.claims.iter().map(|c| (c.id, c)).collect();
    let gold_sp: HashSet<(&Node, &str)> =
        d.gt.active
            .iter()
            .map(|t| (&t.subject, t.predicate.as_str()))
            .collect();
    let gold_so: HashSet<(&Node, &Node)> =
        d.gt.active
            .iter()
            .map(|t| (&t.subject, &t.object))
            .collect();
    for (t, ids) in d.sys {
        if !d.gt.vocabulary.contains(&t.predicate)
            || d.gt.active.contains(t)
            || d.gt.optional.contains(t)
        {
            continue;
        }
        let reason = if d.gt.superseded.contains(t) {
            "superseded"
        } else if matches!(t.subject, Node::Unmatched(_)) {
            "subject_unmatched"
        } else if matches!(t.object, Node::Unmatched(_)) {
            "object_unmatched"
        } else if gold_sp.contains(&(&t.subject, t.predicate.as_str())) {
            "wrong_object"
        } else if gold_so.contains(&(&t.subject, &t.object)) {
            "wrong_predicate"
        } else {
            "not_in_gold"
        };
        let claims: Vec<&nugget_graph::Claim> = ids
            .iter()
            .filter_map(|id| claims_by_id.get(id).copied())
            .collect();
        let mut items: Vec<String> = claims
            .iter()
            .flat_map(|c| &c.evidence)
            .map(|e| item_id(&e.item))
            .collect();
        items.sort();
        items.dedup();
        let quote = claims
            .iter()
            .flat_map(|c| &c.evidence)
            .find_map(|e| e.quote.clone());
        let raw: Vec<String> = claims
            .iter()
            .map(|c| {
                let o = match &c.object {
                    ClaimObject::Entity(id) => snap
                        .entity(id)
                        .map_or_else(|| id.to_string(), |e| e.name.clone()),
                    ClaimObject::Literal(l) => format!("'{l}'"),
                };
                let s = snap
                    .entity(&c.subject)
                    .map_or_else(|| c.subject.to_string(), |e| e.name.clone());
                format!("{s} {} {o}", c.predicate)
            })
            .collect();
        out.push(json!({
            "type": "claim_fp",
            "predicate": t.predicate,
            "subject": node_label(&t.subject, gold, snap),
            "object": node_label(&t.object, gold, snap),
            "as_stored": raw,
            "items": items,
            "quote": quote,
            "reason": reason,
            "source": d.baseline.map(|b| if b.triples.contains(&triple_key(t, snap)) { "deterministic" } else { "llm" }),
        }));
    }
    let matched_gold: HashSet<&str> = m.loose.values().map(String::as_str).collect();
    let present = |n: &Node| match n {
        Node::Gold(g) => matched_gold.contains(g.as_str()),
        _ => true,
    };
    for c in gold.fx.claims.iter().filter(|c| !c.optional) {
        let Some(t) = d.gt.by_id.get(&c.id) else {
            continue;
        };
        if !d.gt.active.contains(t) || d.sys.contains_key(t) {
            continue;
        }
        let near: Vec<String> = d
            .sys
            .keys()
            .filter(|s| {
                s.subject == t.subject && (s.object == t.object || s.predicate == t.predicate)
            })
            .map(|s| {
                format!(
                    "{} {} {}",
                    node_label(&s.subject, gold, snap),
                    s.predicate,
                    node_label(&s.object, gold, snap)
                )
            })
            .collect();
        let reason = if !present(&t.subject) {
            "subject_missing"
        } else if !present(&t.object) {
            "object_missing"
        } else if d
            .sys
            .keys()
            .any(|s| s.subject == t.subject && s.object == t.object)
        {
            "wrong_predicate"
        } else if d
            .sys
            .keys()
            .any(|s| s.subject == t.subject && s.predicate == t.predicate)
        {
            "wrong_object"
        } else {
            "absent"
        };
        let items: Vec<&str> = c.support.iter().map(|s| s.item.as_str()).collect();
        out.push(json!({
            "type": "claim_fn",
            "gold": c.id,
            "predicate": t.predicate,
            "subject": node_label(&t.subject, gold, snap),
            "object": node_label(&t.object, gold, snap),
            "items": items,
            "reason": reason,
            "near": near,
            "in_baseline": d.baseline.map(|b| b.triples.contains(&triple_key(t, snap))),
        }));
    }
    out.extend(d.requests.iter().cloned());
    let mut all = vec![summary(&out)];
    all.extend(out);
    all
}

/// Counts of the lines by type, then by kind or predicate, source and reason.
pub fn summary(lines: &[Value]) -> Value {
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for l in lines {
        let ty = l["type"].as_str().unwrap_or("?").to_owned();
        let bucket = counts.entry(ty.clone()).or_default();
        *bucket.entry("total".into()).or_default() += 1;
        for key in ["kind", "predicate", "reason", "source", "outcome"] {
            if let Some(v) = l[key].as_str() {
                *bucket.entry(format!("{key}:{v}")).or_default() += 1;
            }
        }
        if ty == "request" {
            for v in l["verdicts"].as_array().into_iter().flatten() {
                if v["kept"] == false {
                    let r = v["reason"].as_str().unwrap_or("?");
                    let w = v["what"].as_str().unwrap_or("?");
                    *bucket.entry(format!("dropped {w}: {r}")).or_default() += 1;
                }
            }
            for key in ["output_tokens", "input_tokens"] {
                if let Some(n) = l[key].as_u64() {
                    *bucket.entry(key.to_owned()).or_default() += n as usize;
                }
            }
        }
    }
    json!({"type": "summary", "counts": counts})
}

/// One `request` line: the logged request, with the gate's `verdicts` when given.
pub fn request_line(r: &RequestLog, item: Option<&str>, verdicts: Option<Value>) -> Value {
    const MAX_ANSWER: usize = 8_000;
    let answer: String = r.answer.chars().take(MAX_ANSWER).collect();
    json!({
        "type": "request",
        "item": item,
        "chunks": r.chunk_ords,
        "outcome": r.outcome,
        "latency_s": (r.latency_s * 100.0).round() / 100.0,
        "input_tokens": r.input_tokens,
        "output_tokens": r.output_tokens,
        "answer_chars": r.answer.chars().count(),
        "answer": answer,
        "verdicts": verdicts,
    })
}

/// Write the lines as JSONL.
pub fn write(path: &Path, lines: &[Value]) -> std::result::Result<(), BenchError> {
    use std::io::Write;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for l in lines {
        writeln!(f, "{l}")?;
    }
    f.flush()?;
    Ok(())
}

/// The provider used for a deterministic baseline.
pub fn empty_llm() -> Arc<dyn Provider> {
    Arc::new(EmptyLlm::default())
}
