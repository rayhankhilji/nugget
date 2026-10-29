//! The real pipeline behind [`KnowledgeSystem`] (cargo feature `pipeline`, on by default):
//!
//! - **Graph:** `nugget-store`'s SQLite graph ([`Store::graph`]) in the store the corpus was
//!   ingested into, so deleting an item retracts its provenance in the same transaction.
//! - **Extraction:** `nugget_knowledge::Knowledge` over that store, deterministic extractors plus
//!   the LLM extractor, with [`SystemContext::llm`] as its `Synthesis` provider (on-device: the
//!   scripted fake, or a real local model with `--local-models`), the user's name and addresses
//!   configured, and the context's LLM overrides (timeout, attempts) applied. `extract` and
//!   `items_deleted` are one `run_once(RunBudget::unlimited())` each (the pipeline diffs the
//!   store against its ledger).
//! - **Vault:** `nugget_vault::Vault` in the suite's temporary directory: `render_all` (three-way
//!   merging user edits on the second call) and `validate`. The suite's view of each page (its
//!   facts, footnotes and the evidence behind them) is parsed from the files, independently of the
//!   vault's own validator: fact lines (`- …`) in the `facts` block, footnote references that are
//!   defined in the `sources` block, and the claim marker `<!-- c:ID -->`, whose evidence is read
//!   from the graph (the renderer writes one footnote per evidence entry).
//! - **MCP:** an in-process `nugget_mcp::McpServer`, called through `call_tool("get_context")`
//!   for one client with default scopes. The daemon's `KnowledgeBackend` is wave-2 work (M2-G), so
//!   the backend here is minimal: `context` packs the store's full-text hits; the other methods
//!   return nothing. The latency therefore covers the MCP layer (scopes, rendering, audit) plus
//!   store full-text search, not `nugget-retrieve`'s hybrid search.
//!
//! Built without the feature, [`build`] reports the pipeline as unavailable and the suite
//! returns `not_implemented`.

use super::system::{KnowledgeSystem, SystemContext};
use crate::runner::BenchError;

/// Whether the real pipeline can be built.
pub enum Wiring {
    Ready(Box<dyn KnowledgeSystem>),
    /// Why not: which crates or APIs are missing.
    Unavailable(String),
}

/// Build the real pipeline, or say why it is not available in this build.
pub fn build(ctx: &SystemContext) -> Result<Wiring, BenchError> {
    #[cfg(feature = "pipeline")]
    {
        Ok(Wiring::Ready(Box::new(real::Wired::new(ctx)?)))
    }
    #[cfg(not(feature = "pipeline"))]
    {
        let _ = ctx;
        Ok(Wiring::Unavailable(
            "nugget-bench was built without the `pipeline` feature, so the knowledge suite has no \
             pipeline to measure (nugget-knowledge, nugget-vault, nugget-mcp). Rebuild with \
             default features."
                .into(),
        ))
    }
}

#[cfg(feature = "pipeline")]
mod real {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use async_trait::async_trait;
    use nugget_core::ids::{EntityId, ItemRef};
    use nugget_core::ipc::{McpClientInfo, McpScopes};
    use nugget_graph::GraphStore;
    use nugget_knowledge::{Knowledge, KnowledgeConfig, MeConfig, RunBudget, RunStats};
    use nugget_mcp::{
        ContextPack, ContextPassage, EntityRecord, EntitySummaryRecord, ItemRecord, McpConfig,
        McpServer, NeighbourhoodRecord, RateLimit, RecentChanges, ScopeFilter, SearchRecord,
        Transport,
    };
    use nugget_store::{SqliteGraph, Store};
    use nugget_vault::{Vault, VaultConfig};
    use serde_json::{Map, Value, json};
    use time::OffsetDateTime;

    use super::super::system::{
        ContextAnswer, KnowledgeSystem, McpProbe, RenderedFact, RenderedPage, RenderedVault,
        SystemContext, synthesis_router,
    };
    use crate::runner::BenchError;

    const FACTS_BEGIN: &str = "<!-- nugget:begin facts -->";
    const FACTS_END: &str = "<!-- nugget:end facts -->";
    const SOURCES_BEGIN: &str = "<!-- nugget:begin sources -->";
    const SOURCES_END: &str = "<!-- nugget:end sources -->";
    const CLAIM_MARK: &str = "<!-- c:";
    /// Passages `get_context` packs at most.
    const CONTEXT_PASSAGES: usize = 8;

    pub struct Wired {
        llm_label: String,
        store: Store,
        graph: Arc<SqliteGraph>,
        knowledge: Knowledge,
        rt: Arc<tokio::runtime::Runtime>,
        vault: Option<(PathBuf, Vault)>,
        mcp: McpRoundTrip,
        runs: Vec<RunStats>,
    }

    impl Wired {
        pub fn new(ctx: &SystemContext) -> Result<Self, BenchError> {
            let rt = Arc::new(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?,
            );
            let graph = Arc::new(ctx.store.graph());
            let router = Arc::new(synthesis_router(ctx.llm.clone()));
            let mut config = KnowledgeConfig {
                me: MeConfig {
                    name: Some(ctx.me_name.clone()),
                    emails: ctx.me_emails.clone(),
                    infer_from_sent: true,
                },
                extra_freemail_domains: ctx.webmail_domains.clone(),
                ..KnowledgeConfig::default()
            };
            if let Some(t) = ctx.llm_settings.timeout {
                config.llm.timeout_ms = u64::try_from(t.as_millis()).unwrap_or(u64::MAX).max(1);
            }
            if let Some(n) = ctx.llm_settings.max_attempts {
                config.llm.max_attempts = n;
            }
            let knowledge = Knowledge::new(
                ctx.store.clone(),
                graph.clone() as Arc<dyn GraphStore>,
                Some(router),
                config,
            );
            let server = McpServer::builder(Arc::new(StoreBackend {
                store: ctx.store.clone(),
            }))
            .config(McpConfig {
                rate_limit: RateLimit {
                    burst: 1 << 20,
                    per_minute: 1 << 30,
                },
            })
            .build();
            let client = McpClientInfo {
                id: "bench".into(),
                name: "nugget-bench".into(),
                scopes: McpScopes {
                    sources: None,
                    tools: Vec::new(),
                    include_sensitive: false,
                },
                created_at: OffsetDateTime::now_utc(),
                last_used: None,
            };
            Ok(Self {
                llm_label: ctx.llm_label.clone(),
                store: ctx.store.clone(),
                graph,
                knowledge,
                rt: rt.clone(),
                vault: None,
                mcp: McpRoundTrip { rt, server, client },
                runs: Vec::new(),
            })
        }

        fn run_once(&mut self) -> Result<(), BenchError> {
            let stats = self
                .rt
                .block_on(self.knowledge.run_once(RunBudget::unlimited()))?;
            if let Some(e) = &stats.last_error {
                return Err(BenchError::Failed(format!("knowledge run: {e}")));
            }
            self.runs.push(stats);
            Ok(())
        }
    }

    impl KnowledgeSystem for Wired {
        fn label(&self) -> String {
            format!(
                "nugget-knowledge + nugget-vault + nugget-mcp {} (SQLite graph, deterministic + {} LLM extractors, extractor v{})",
                env!("CARGO_PKG_VERSION"),
                self.llm_label,
                nugget_knowledge::EXTRACTOR_VERSION
            )
        }

        fn extract(&mut self, _items: &[ItemRef]) -> Result<(), BenchError> {
            self.run_once()
        }

        fn items_deleted(&mut self, _items: &[ItemRef]) -> Result<(), BenchError> {
            // The suite deleted them from the store (which drops their graph provenance in the
            // same transaction); the pipeline's own delete path finds them by its ledger diff.
            self.run_once()
        }

        fn graph(&self) -> &dyn GraphStore {
            self.graph.as_ref()
        }

        fn render(&mut self, dir: &Path) -> Result<RenderedVault, BenchError> {
            if self.vault.as_ref().is_none_or(|(d, _)| d != dir) {
                let v = Vault::new(
                    dir,
                    self.graph.clone() as Arc<dyn GraphStore>,
                    Arc::new(self.store.clone()),
                    VaultConfig::default(),
                )?;
                self.vault = Some((dir.to_owned(), v));
            }
            let (_, vault) = self.vault.as_ref().expect("vault");
            vault.render_all()?;
            let root = vault.root().to_owned();
            let mut files = Vec::new();
            walk(&root, Path::new(""), &mut files)?;
            let mut out = RenderedVault::default();
            for rel in files {
                let text = std::fs::read_to_string(root.join(&rel))?;
                if let Some(page) = parse_page(&rel, &text, self.graph.as_ref())? {
                    out.pages.push(page);
                }
            }
            Ok(out)
        }

        fn validate(&mut self, dir: &Path) -> Result<Vec<String>, BenchError> {
            let Some((d, vault)) = &self.vault else {
                return Ok(vec![format!("{}: never rendered", dir.display())]);
            };
            if d != dir {
                return Ok(vec![format!("{}: not the rendered vault", dir.display())]);
            }
            Ok(vault
                .validate()?
                .issues
                .into_iter()
                .map(|i| format!("{} {:?}: {}", i.page, i.kind, i.message))
                .collect())
        }

        fn mcp(&mut self) -> Option<&mut dyn McpProbe> {
            Some(&mut self.mcp)
        }

        fn stats(&self) -> Value {
            json!({
                "runs": self.runs,
                "status": self.knowledge.status(),
            })
        }
    }

    /// Vault pages (`.md`, outside `.nugget/`, not the root index), relative to the root.
    fn walk(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<(), BenchError> {
        let mut entries: Vec<_> = std::fs::read_dir(root.join(rel))?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name();
            let r = rel.join(&name);
            let ty = e.file_type()?;
            if ty.is_dir() {
                if name != ".nugget" {
                    walk(root, &r, out)?;
                }
            } else if ty.is_file()
                && r.extension().is_some_and(|x| x == "md")
                && r != Path::new("README.md")
            {
                out.push(r);
            }
        }
        Ok(())
    }

    fn unquote(v: &str) -> String {
        let v = v.trim();
        serde_json::from_str::<String>(v).unwrap_or_else(|_| v.trim_matches('"').to_owned())
    }

    /// Lines strictly between `begin` and `end`.
    fn block<'a>(text: &'a str, begin: &str, end: &str) -> Vec<&'a str> {
        let mut inside = false;
        let mut out = Vec::new();
        for line in text.lines() {
            let t = line.trim();
            if t == begin {
                inside = true;
            } else if t == end {
                inside = false;
            } else if inside {
                out.push(line);
            }
        }
        out
    }

    /// `[^label]` references in a line (not definitions).
    fn footnote_refs(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = line;
        while let Some(at) = rest.find("[^") {
            let after = &rest[at + 2..];
            let Some(close) = after.find(']') else {
                break;
            };
            if !after[close + 1..].starts_with(':') {
                out.push(after[..close].to_owned());
            }
            rest = &after[close + 1..];
        }
        out
    }

    /// One page's facts, or `None` for stubs and files that are not entity pages.
    fn parse_page(
        rel: &Path,
        text: &str,
        graph: &dyn GraphStore,
    ) -> Result<Option<RenderedPage>, BenchError> {
        let end = crate::knowledge::eval::frontmatter_end(text);
        if end == 0 {
            return Ok(None);
        }
        let mut id = None;
        for line in text[..end].lines() {
            if let Some(v) = line.strip_prefix("nugget_id:") {
                id = Some(EntityId::new(unquote(v)));
            }
            if line.starts_with("merged_into:") || line.starts_with("status:") {
                return Ok(None);
            }
        }
        let Some(entity) = id else {
            return Ok(None);
        };
        let defined: Vec<String> = block(text, SOURCES_BEGIN, SOURCES_END)
            .into_iter()
            .filter_map(|l| {
                let l = l.trim_start().strip_prefix("[^")?;
                let close = l.find("]:")?;
                Some(l[..close].to_owned())
            })
            .collect();
        let mut facts = Vec::new();
        for line in block(text, FACTS_BEGIN, FACTS_END) {
            let t = line.trim_start();
            if !t.starts_with("- ") || t.contains(" (you)") {
                continue;
            }
            let footnotes = footnote_refs(t)
                .iter()
                .filter(|r| defined.contains(r))
                .count();
            let claim = t.find(CLAIM_MARK).and_then(|at| {
                let digits: String = t[at + CLAIM_MARK.len()..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits.parse::<i64>().ok()
            });
            let citations = match claim {
                Some(c) if footnotes > 0 => {
                    graph.get_claim(c)?.map(|c| c.evidence).unwrap_or_default()
                }
                _ => Vec::new(),
            };
            facts.push(RenderedFact {
                claim,
                text: t.to_owned(),
                footnotes,
                citations,
            });
        }
        Ok(Some(RenderedPage {
            entity,
            path: rel.to_owned(),
            facts,
        }))
    }

    /// `get_context` through the in-process MCP server.
    struct McpRoundTrip {
        rt: Arc<tokio::runtime::Runtime>,
        server: McpServer,
        client: McpClientInfo,
    }

    impl McpProbe for McpRoundTrip {
        fn get_context(&mut self, query: &str) -> Result<ContextAnswer, BenchError> {
            let mut args = Map::new();
            args.insert("query".into(), json!(query));
            let out = self
                .rt
                .block_on(self.server.call_tool(
                    &self.client,
                    Transport::Stdio,
                    "get_context",
                    &args,
                ))
                .map_err(|f| BenchError::Failed(format!("mcp get_context: {f:?}")))?;
            Ok(ContextAnswer {
                citations: out.structured["citations"].as_array().map_or(0, Vec::len),
                text_chars: out.text.chars().count(),
            })
        }
    }

    /// A minimal [`nugget_mcp::KnowledgeBackend`] over the store's full-text search; the daemon
    /// provides the real one in wave 2.
    struct StoreBackend {
        store: Store,
    }

    #[async_trait]
    impl nugget_mcp::KnowledgeBackend for StoreBackend {
        async fn search(
            &self,
            query: &str,
            limit: usize,
            _filter: &ScopeFilter,
        ) -> nugget_core::Result<Vec<SearchRecord>> {
            Ok(self
                .store
                .search_text(query, limit)?
                .into_iter()
                .map(|h| SearchRecord {
                    item: h.item,
                    chunk_id: Some(h.chunk_id),
                    title: h.title,
                    url: None,
                    snippet: h.snippet,
                    score: h.score,
                    sensitive: false,
                })
                .collect())
        }

        async fn context(
            &self,
            query: &str,
            _budget_tokens: u32,
            _filter: &ScopeFilter,
        ) -> nugget_core::Result<ContextPack> {
            let hits = self.store.search_text(query, CONTEXT_PASSAGES)?;
            let ids: Vec<i64> = hits.iter().map(|h| h.chunk_id).collect();
            let passages = self
                .store
                .get_chunks(&ids)?
                .into_iter()
                .map(|c| ContextPassage {
                    item: c.item,
                    chunk_id: c.id,
                    title: c.title,
                    url: None,
                    sensitive: false,
                    text: c.text,
                })
                .collect();
            Ok(ContextPack {
                passages,
                excluded_sensitive: 0,
            })
        }

        async fn find_entities(
            &self,
            _text: Option<&str>,
            _kind: Option<&str>,
            _limit: usize,
            _offset: usize,
            _filter: &ScopeFilter,
        ) -> nugget_core::Result<Vec<EntitySummaryRecord>> {
            Ok(Vec::new())
        }

        async fn get_entity(
            &self,
            _id: &EntityId,
            _filter: &ScopeFilter,
        ) -> nugget_core::Result<Option<EntityRecord>> {
            Ok(None)
        }

        async fn neighbourhood(
            &self,
            _id: &EntityId,
            _depth: u32,
            _limit: usize,
            _filter: &ScopeFilter,
        ) -> nugget_core::Result<NeighbourhoodRecord> {
            Ok(NeighbourhoodRecord {
                nodes: Vec::new(),
                edges: Vec::new(),
                truncated: false,
            })
        }

        async fn get_item(
            &self,
            _item: &ItemRef,
            _filter: &ScopeFilter,
        ) -> nugget_core::Result<Option<ItemRecord>> {
            Ok(None)
        }

        async fn recent_changes(
            &self,
            _since: OffsetDateTime,
            _limit: usize,
            _filter: &ScopeFilter,
        ) -> nugget_core::Result<RecentChanges> {
            Ok(RecentChanges::default())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn page_parsing() {
            let page = "---\nnugget_id: \"ent_000007\"\nkind: \"person\"\n---\n\
<!-- nugget:begin facts -->\n## Facts\n\n### Works at <!-- p:works_at -->\n\
- Sam works at [[Organization/Acme (x)|Acme]]. [^1][^2] <!-- c:12 -->\n\
- Likes tea. (you) <!-- c:13 -->\n- No source. <!-- c:14 -->\n- Undefined note [^9] <!-- c:15 -->\n\
<!-- nugget:end facts -->\n\n<!-- nugget:begin sources -->\n[^1]: [Mail](nugget://x): \"q\"\n\
[^2]: [Note](nugget://y)\n<!-- nugget:end sources -->\n\n## Notes\n";
            let g = nugget_graph::MemGraph::new();
            let p = parse_page(Path::new("Person/Sam.md"), page, &g)
                .unwrap()
                .unwrap();
            assert_eq!(p.entity.as_str(), "ent_000007");
            let f: Vec<(Option<i64>, usize)> =
                p.facts.iter().map(|f| (f.claim, f.footnotes)).collect();
            assert_eq!(f, [(Some(12), 2), (Some(14), 0), (Some(15), 0)]);
            let stub = "---\nnugget_id: \"ent_1\"\nmerged_into: \"ent_2\"\n---\n";
            assert!(parse_page(Path::new("a.md"), stub, &g).unwrap().is_none());
            assert_eq!(footnote_refs("a [^1] b [^x]: c"), ["1"]);
        }
    }
}
