//! The adapter seam between the `knowledge` suite and the system under test.
//!
//! The suite drives a [`KnowledgeSystem`]: extract the ingested items into a graph, render a
//! vault, re-render after user edits and deletions, run the vault validators, and optionally
//! answer MCP `get_context` calls. [`super::wired`] builds the real one (`nugget-knowledge`,
//! `nugget-vault`, `nugget-mcp`); tests use an oracle built from the gold labels.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nugget_core::capability::{Capabilities, Tier};
use nugget_core::ids::{EntityId, ItemRef};
use nugget_graph::{ClaimId, Evidence, GraphStore};
use nugget_infer::{Provider, Router};
use nugget_store::Store;

use crate::runner::BenchError;

/// What a system is built from.
#[derive(Clone)]
pub struct SystemContext {
    /// The store the corpus was ingested into (items, text and chunks; no embeddings).
    pub store: Store,
    /// The LLM the pipeline's extractor uses: the scripted fake
    /// ([`super::fake_llm::ScriptedLlm`]) by default, or a real local model behind
    /// [`super::metered::MeteredProvider`] (`--local-models`). [`synthesis_router`] routes the
    /// `Synthesis` (and `Light`) tier to it.
    pub llm: Arc<dyn Provider>,
    /// What `llm` is, for the system label (`scripted`, or the local model's label).
    pub llm_label: String,
    /// Overrides of the pipeline's LLM settings; `None` keeps `nugget-knowledge`'s default.
    pub llm_settings: LlmSettings,
    /// The user's name and mail addresses, as the daemon's configuration would hold them.
    pub me_name: String,
    pub me_emails: Vec<String>,
    /// Webmail domains of the corpus (`Scenario::webmail_domains`).
    pub webmail_domains: Vec<String>,
}

/// Overrides of `nugget_knowledge::LlmConfig` for a run (the default run sets none).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LlmSettings {
    /// `LlmConfig::timeout_ms`.
    pub timeout: Option<Duration>,
    /// `LlmConfig::max_attempts`.
    pub max_attempts: Option<u32>,
}

/// A router with `p` as the only provider of the `Synthesis` and `Light` tiers (where
/// `nugget-knowledge` sends LLM extraction), declared to stream and to honour JSON schemas.
pub fn synthesis_router(p: Arc<dyn Provider>) -> Router {
    let id = p.id().clone();
    let router = Router::new(
        vec![p],
        HashMap::from([
            (Tier::Synthesis, vec![id.clone()]),
            (Tier::Light, vec![id.clone()]),
        ]),
    );
    router.set_capabilities(
        &id,
        Capabilities {
            streaming: true,
            json_schema: true,
            ..Default::default()
        },
    );
    router
}

/// One fact as rendered on a vault page, with the citations (footnotes) attached to it.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFact {
    /// The claim it renders, when the renderer says so.
    pub claim: Option<ClaimId>,
    pub text: String,
    /// Footnote references on the fact that resolve to a footnote defined on the page.
    pub footnotes: usize,
    /// The evidence behind those footnotes, checked against the store.
    pub citations: Vec<Evidence>,
    /// A history line (a superseded claim shown under "Earlier"), not a current fact.
    pub history: bool,
}

/// One rendered vault page.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedPage {
    pub entity: EntityId,
    /// Relative to the vault directory.
    pub path: PathBuf,
    pub facts: Vec<RenderedFact>,
}

/// The result of one render (or re-render) of the vault.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderedVault {
    pub pages: Vec<RenderedPage>,
}

impl RenderedVault {
    /// Current facts (history lines excluded).
    pub fn facts(&self) -> impl Iterator<Item = &RenderedFact> {
        self.pages
            .iter()
            .flat_map(|p| &p.facts)
            .filter(|f| !f.history)
    }

    /// History lines: superseded claims the page still shows, with their citations.
    pub fn history(&self) -> impl Iterator<Item = &RenderedFact> {
        self.pages
            .iter()
            .flat_map(|p| &p.facts)
            .filter(|f| f.history)
    }
}

/// One `get_context` answer from the MCP server.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextAnswer {
    /// Citations (source references) in the answer.
    pub citations: usize,
    pub text_chars: usize,
}

/// An in-process MCP client connected to the system's MCP server.
pub trait McpProbe {
    fn get_context(&mut self, query: &str) -> Result<ContextAnswer, BenchError>;
}

/// The knowledge pipeline under test.
pub trait KnowledgeSystem {
    /// What is being measured (crate names and versions, configuration), for the report.
    fn label(&self) -> String;
    /// Extract entities, claims and mentions from these items (already in the store).
    fn extract(&mut self, items: &[ItemRef]) -> Result<(), BenchError>;
    /// These items were deleted from the store: retract what only they supported.
    fn items_deleted(&mut self, items: &[ItemRef]) -> Result<(), BenchError>;
    /// The graph the pipeline wrote.
    fn graph(&self) -> &dyn GraphStore;
    /// Render the vault into `dir`. A later call re-renders over the same directory and must
    /// three-way merge user edits made since the last render.
    fn render(&mut self, dir: &Path) -> Result<RenderedVault, BenchError>;
    /// Run the vault validators (links resolve, frontmatter schema, citation coverage) over
    /// `dir`; returns the problems found.
    fn validate(&mut self, dir: &Path) -> Result<Vec<String>, BenchError>;
    /// The MCP server, when the system has one in process.
    fn mcp(&mut self) -> Option<&mut dyn McpProbe> {
        None
    }
    /// The system's own counters (e.g. claims its quote gate dropped), recorded in the report's
    /// `params.system_stats`.
    fn stats(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}
