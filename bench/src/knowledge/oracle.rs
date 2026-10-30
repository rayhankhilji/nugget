//! A test-only [`KnowledgeSystem`] built from the gold labels, with switchable faults. It
//! checks the suite end to end: the oracle must meet every target, and each fault must move
//! exactly the metrics it should.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nugget_core::ids::{EntityId, ItemRef};
use nugget_graph::{
    ClaimObject, ClaimStatus, EntityKind, EntityQuery, Evidence, GraphStore, MemGraph, Mention,
    NewClaim, NewEntity,
};

use super::eval::frontmatter_end;
use super::fixture::{Fixture, GoldStatus, Ingested, quote_in};
use super::system::{
    ContextAnswer, KnowledgeSystem, McpProbe, RenderedFact, RenderedPage, RenderedVault,
};
use crate::runner::BenchError;

/// Ways the oracle can misbehave.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Faults {
    /// Also store every scripted LLM claim, hallucinated and injected ones included.
    pub trust_llm: bool,
    /// Ignore deletions.
    pub no_retraction: bool,
    /// Overwrite pages on re-render instead of merging.
    pub clobber_edits: bool,
    /// Render facts without citations.
    pub drop_citations: bool,
    /// Keep the superseded job active.
    pub no_supersede: bool,
}

pub struct Oracle {
    fx: Fixture,
    ing: Ingested,
    graph: MemGraph,
    faults: Faults,
    ids: HashMap<String, EntityId>,
    mcp: Option<FakeMcp>,
}

pub struct FakeMcp;

impl McpProbe for FakeMcp {
    fn get_context(&mut self, query: &str) -> Result<ContextAnswer, BenchError> {
        Ok(ContextAnswer {
            citations: 1,
            text_chars: query.len(),
        })
    }
}

const BEGIN: &str = "<!-- nugget:begin -->";
const END: &str = "<!-- nugget:end -->";

impl Oracle {
    pub fn new(fx: &Fixture, ing: &Ingested, faults: Faults) -> Self {
        Self {
            fx: fx.clone(),
            ing: ing.clone(),
            graph: MemGraph::new(),
            faults,
            ids: HashMap::new(),
            mcp: Some(FakeMcp),
        }
    }

    fn evidence(&self, item: &str, quote: Option<&str>) -> Evidence {
        let it = self.ing.get(item).expect("ingested item");
        let chunk = it
            .chunks
            .iter()
            .find(|(_, t)| quote.is_none_or(|q| quote_in(q, t)))
            .unwrap_or(&it.chunks[0]);
        Evidence {
            item: it.item.clone(),
            chunk_id: Some(chunk.0),
            quote: quote.map(str::to_owned),
        }
    }

    fn entity_by_name(&mut self, name: &str, kind: EntityKind) -> Result<EntityId, BenchError> {
        if let Some(id) = self
            .graph
            .find_by_name(name, Some(kind))?
            .into_iter()
            .next()
        {
            return Ok(id);
        }
        Ok(self.graph.create_entity(&NewEntity::new(kind, name))?)
    }
}

impl KnowledgeSystem for Oracle {
    fn label(&self) -> String {
        format!("oracle {:?}", self.faults)
    }

    fn extract(&mut self, _items: &[ItemRef]) -> Result<(), BenchError> {
        for e in self.fx.entities.iter().filter(|e| !e.optional) {
            let mut n = NewEntity::new(e.kind, e.name.clone());
            n.aliases = e
                .surfaces
                .iter()
                .filter(|s| !s.contains('@'))
                .cloned()
                .collect();
            n.identifiers = e.identifiers.clone();
            let id = self.graph.create_entity(&n)?;
            self.ids.insert(e.id.clone(), id);
        }
        for c in self.fx.claims.iter().filter(|c| !c.optional) {
            let object = match (&c.object.entity, &c.object.literal) {
                (Some(e), _) => ClaimObject::Entity(self.ids[e].clone()),
                (None, Some(l)) => ClaimObject::Literal(l.clone()),
                _ => continue,
            };
            let evidence = c
                .support
                .iter()
                .map(|s| self.evidence(&s.item, s.quote.as_deref()))
                .collect();
            let id = self.graph.add_claim(&NewClaim {
                subject: self.ids[&c.subject].clone(),
                predicate: c.predicate.clone(),
                object,
                text: c.id.clone(),
                confidence: 0.9,
                status: ClaimStatus::Active,
                evidence,
                valid_from: None,
                valid_to: None,
            })?;
            if c.status == GoldStatus::Superseded && !self.faults.no_supersede {
                self.graph.set_claim_status(id, ClaimStatus::Superseded)?;
            }
        }
        for m in &self.fx.mentions {
            let Some(id) = self.ids.get(&m.entity) else {
                continue;
            };
            let it = self.ing.get(&m.item).expect("ingested item");
            self.graph.add_mention(&Mention {
                entity: id.clone(),
                item: it.item.clone(),
                chunk_id: None,
                surface: m.surfaces[0].clone(),
                confidence: 1.0,
            })?;
        }
        if self.faults.trust_llm {
            for s in self.fx.script.clone() {
                for c in &s.claims {
                    let kind_of = |name: &str| {
                        s.entities
                            .iter()
                            .find(|e| e.name == name)
                            .map_or(EntityKind::Person, |e| e.kind)
                    };
                    let subject = self.entity_by_name(&c.subject, kind_of(&c.subject))?;
                    let object = if c.object_type == "entity" {
                        ClaimObject::Entity(self.entity_by_name(&c.object, kind_of(&c.object))?)
                    } else {
                        ClaimObject::Literal(c.object.clone())
                    };
                    let ev = self.evidence(&s.item, None);
                    self.graph.add_claim(&NewClaim {
                        subject,
                        predicate: c.predicate.clone(),
                        object,
                        text: c.text.clone(),
                        confidence: c.confidence,
                        status: ClaimStatus::Active,
                        evidence: vec![Evidence {
                            quote: Some(c.quote.clone()),
                            ..ev
                        }],
                        valid_from: None,
                        valid_to: None,
                    })?;
                }
            }
        }
        Ok(())
    }

    fn items_deleted(&mut self, items: &[ItemRef]) -> Result<(), BenchError> {
        if !self.faults.no_retraction {
            for i in items {
                self.graph.retract_item(i)?;
            }
        }
        Ok(())
    }

    fn graph(&self) -> &dyn GraphStore {
        &self.graph
    }

    fn render(&mut self, dir: &Path) -> Result<RenderedVault, BenchError> {
        let mut out = RenderedVault::default();
        let entities = self.graph.list_entities(&EntityQuery::default())?;
        for (n, e) in entities.iter().enumerate() {
            let path = PathBuf::from(e.kind.as_str()).join(format!("{n:03}.md"));
            let mut facts = Vec::new();
            let mut region = format!("{BEGIN}\n## Facts\n\n");
            let mut notes = String::new();
            for c in self.graph.claims_for(&e.id, false)? {
                if c.subject != e.id {
                    continue;
                }
                let citations = if self.faults.drop_citations {
                    Vec::new()
                } else {
                    c.evidence.clone()
                };
                let mut line = format!("- {} {}", c.predicate, c.id);
                for (k, ev) in citations.iter().enumerate() {
                    line.push_str(&format!("[^{}-{k}]", c.id));
                    notes.push_str(&format!("[^{}-{k}]: {}\n", c.id, ev.item.key));
                }
                region.push_str(&line);
                region.push('\n');
                facts.push(RenderedFact {
                    claim: Some(c.id),
                    text: line,
                    footnotes: citations.len(),
                    citations,
                    history: false,
                });
            }
            region.push_str(&format!("\n{notes}{END}\n"));
            let front = format!("---\nid: {}\nkind: {}\n---\n", e.id, e.kind.as_str());
            let full = dir.join(&path);
            std::fs::create_dir_all(full.parent().expect("parent"))?;
            let text = match std::fs::read_to_string(&full) {
                Ok(old) if !self.faults.clobber_edits => {
                    let body = &old[frontmatter_end(&old)..];
                    let (a, b) = match (body.find(BEGIN), body.find(END)) {
                        (Some(a), Some(b)) => (a, b + END.len() + 1),
                        _ => (body.len(), body.len()),
                    };
                    format!(
                        "{front}{}{region}{}",
                        &body[..a],
                        &body[b.min(body.len())..]
                    )
                }
                _ => format!("{front}# {}\n\n{region}", e.name),
            };
            std::fs::write(&full, text)?;
            out.pages.push(RenderedPage {
                entity: e.id.clone(),
                path,
                facts,
            });
        }
        Ok(out)
    }

    fn validate(&mut self, dir: &Path) -> Result<Vec<String>, BenchError> {
        let mut problems = Vec::new();
        for kind in EntityKind::ALL {
            let Ok(rd) = std::fs::read_dir(dir.join(kind.as_str())) else {
                continue;
            };
            for f in rd {
                let p = f?.path();
                if frontmatter_end(&std::fs::read_to_string(&p)?) == 0 {
                    problems.push(format!("{}: no frontmatter", p.display()));
                }
            }
        }
        Ok(problems)
    }

    fn mcp(&mut self) -> Option<&mut dyn McpProbe> {
        self.mcp.as_mut().map(|m| m as &mut dyn McpProbe)
    }
}
