//! `knowledge`: the M2 knowledge graph, measured on a labelled fixture corpus.
//!
//! Ingests the fixture corpus, extracts it with [`KnowledgeSystem`] (scripted [`ScriptedLlm`] by
//! default), scores the graph against gold labels, renders the vault, applies user edits and
//! deletions, re-renders to check merge fidelity and retraction, and round-trips MCP. Without the
//! `pipeline` feature the suite reports `not_implemented`. With `--local-models`
//! ([`run_real_model`]) each item gets a single LLM attempt so deletion runs don't silently
//! re-extract failed items; `--knowledge-dump` writes FP/FN diagnostics ([`diagnose`]).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nugget_core::ids::ItemRef;
use nugget_core::ipc::RuntimeRole;
use nugget_graph::EntityKind;
use nugget_infer::Provider;
use nugget_store::Store;
use serde_json::json;

use crate::knowledge::diagnose::{self, Baseline};
use crate::knowledge::eval::{
    self, GoldIndex, Snapshot, distinct_identities, edits_preserved, gold_triples,
    injection_unsupported, match_entities, resolved_strictly, same_identity, score_claims,
    score_entities, score_resolution, score_retraction, score_support, score_vault, system_triples,
    unattributed,
};
use crate::knowledge::fake_llm::ScriptedLlm;
use crate::knowledge::fixture::{self, Corpus, Fixture, Ingested};
use crate::knowledge::metered::{Meter, MeteredProvider};
use crate::knowledge::system::{KnowledgeSystem, LlmSettings, SystemContext};
use crate::knowledge::wired::{self, Wiring};
use crate::local::LocalModels;
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, Benchmark, RunContext};
use crate::stats::summarize;
use crate::suites::secs;

/// Entity F1 the deterministic + fake-LLM path must reach. Real-model runs keep it unchanged.
pub const ENTITY_F1_TARGET: f64 = 0.8;

/// `--local-models`: wall-time limit per extraction request (the bench's; the pipeline's own
/// timeout is set [`PIPELINE_TIMEOUT_MARGIN`] later so this one always fires first).
pub const REAL_LLM_TIMEOUT: Duration = Duration::from_secs(180);

/// How much later than the bench's timeout the pipeline's own request timeout is set.
pub const PIPELINE_TIMEOUT_MARGIN: Duration = Duration::from_secs(30);

/// `--local-models`: items extracted per scale (`smoke` 8, `quick` 30, `standard` and `full`
/// the whole corpus). The scripted run always uses the whole corpus.
pub fn real_items(scale: crate::runner::Scale) -> usize {
    scale.pick(8, 30, usize::MAX, usize::MAX)
}

/// `knowledge` options (flattened into the CLI).
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct KnowledgeOptions {
    /// `knowledge --local-models`: items to extract with the real model instead of the scale's
    /// subset (`smoke` 8, `quick` 30, `standard` all). The scenario's items are always kept;
    /// `all`, or a number at or above the corpus size, runs everything.
    #[arg(long, value_name = "N|all", value_parser = parse_item_count)]
    pub knowledge_items: Option<usize>,
    /// `knowledge`: `all` items and hard cases (default), or the `original` pre-EE1 corpus (93
    /// items; same item selection and scored gold as the pre-EE1 bench, for before/after runs).
    #[arg(long, value_enum, default_value_t = Corpus::All)]
    pub knowledge_corpus: Corpus,
    /// `knowledge`: write the false-positive / false-negative entities and claims, and (with a
    /// real model) every extraction request with its answer and the quote gate's verdicts, to
    /// this JSONL file.
    #[arg(long, value_name = "FILE")]
    pub knowledge_dump: Option<std::path::PathBuf>,
}

/// `--knowledge-items`: a count, or `all` (the whole corpus, whatever its size).
fn parse_item_count(s: &str) -> Result<usize, String> {
    if s.eq_ignore_ascii_case("all") {
        return Ok(usize::MAX);
    }
    s.parse()
        .map_err(|e| format!("expected a number of items or `all`: {e}"))
}

/// The LLM behind the pipeline's extractor in a run.
#[derive(Clone, Copy)]
pub enum LlmUnderTest<'a> {
    /// The scripted fake: reports `llm.calls` and `llm.unmatched`.
    Scripted(&'a ScriptedLlm),
    /// A real model behind the metering decorator: reports `llm.requests`, failures, timeouts,
    /// unparseable answers and latency.
    Real(&'a MeteredProvider),
}

impl LlmUnderTest<'_> {
    fn meter(&self) -> Option<Meter> {
        match self {
            LlmUnderTest::Scripted(_) => None,
            LlmUnderTest::Real(p) => Some(p.snapshot()),
        }
    }
}

pub struct KnowledgeBench;

impl Benchmark for KnowledgeBench {
    fn name(&self) -> &'static str {
        "knowledge"
    }

    fn description(&self) -> &'static str {
        "Knowledge graph: entity, claim and resolution quality, citations, retraction, vault merge (M2)"
    }

    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        let fx = Fixture::builtin()?.corpus(ctx.config.knowledge.knowledge_corpus);
        if ctx.config.local.local_models {
            return run_local(ctx, &fx);
        }
        let baseline = baseline_if_dumping(ctx, &fx)?;
        let store = Store::open_in_memory()?;
        let ing = prepare(ctx, &fx, &store)?;
        let llm = Arc::new(ScriptedLlm::new(&fx));
        let sctx = system_context(
            &fx,
            &store,
            llm.clone(),
            "scripted".into(),
            LlmSettings::default(),
        )?;
        match wired::build(&sctx)? {
            Wiring::Ready(system) => run_with_system_and_baseline(
                ctx,
                &fx,
                &store,
                &ing,
                LlmUnderTest::Scripted(&llm),
                system,
                baseline.as_ref(),
            ),
            Wiring::Unavailable(why) => Err(BenchError::NotImplemented(why.into())),
        }
    }
}

/// With `--knowledge-dump`: what the deterministic extractors alone make of `fx` (a separate
/// store, an LLM that answers nothing), to attribute errors to their source.
fn baseline_if_dumping(ctx: &RunContext, fx: &Fixture) -> Result<Option<Baseline>, BenchError> {
    if ctx.config.knowledge.knowledge_dump.is_none() {
        return Ok(None);
    }
    let store = Store::open_in_memory()?;
    let _ing = fixture::ingest(&store, fx)?;
    let sctx = system_context(
        fx,
        &store,
        diagnose::empty_llm(),
        "none (deterministic baseline)".into(),
        LlmSettings::default(),
    )?;
    let Wiring::Ready(mut system) = wired::build(&sctx)? else {
        return Ok(None);
    };
    let refs: Vec<ItemRef> = fx.items.iter().map(|i| i.item_ref()).collect();
    system.extract(&refs)?;
    let snap = Snapshot::take(system.graph())?;
    Ok(Some(Baseline::new(&GoldIndex::new(fx), &snap)))
}

fn system_context(
    fx: &Fixture,
    store: &Store,
    llm: Arc<dyn Provider>,
    llm_label: String,
    llm_settings: LlmSettings,
) -> Result<SystemContext, BenchError> {
    let me = fx
        .entity(&fx.scenario.me)
        .ok_or_else(|| BenchError::Failed("scenario.me is not an entity".into()))?;
    Ok(SystemContext {
        store: store.clone(),
        llm,
        llm_label,
        llm_settings,
        me_name: me.name.clone(),
        me_emails: me
            .identifiers
            .iter()
            .filter(|i| i.scheme == "email")
            .map(|i| i.value.clone())
            .collect(),
        webmail_domains: fx.scenario.webmail_domains.clone(),
        as_of: fx.scenario.as_of,
    })
}

/// `--local-models`: start the local chat model as the app does, record it, and run.
fn run_local(ctx: &mut RunContext, fx: &Fixture) -> Result<Vec<Metric>, BenchError> {
    let local = LocalModels::start(&ctx.config.local, &[RuntimeRole::Chat])?;
    local.record(ctx);
    let chat = local.chat()?;
    let label = local.chat_label();
    let out = run_real_model(ctx, fx, chat, &label, REAL_LLM_TIMEOUT);
    drop(local);
    out
}

/// Run the suite with `provider` (a real model) as the extractor's LLM, on the scale's item
/// subset of `full`, each request bounded by `timeout`.
pub fn run_real_model(
    ctx: &mut RunContext,
    full: &Fixture,
    provider: Arc<dyn Provider>,
    label: &str,
    timeout: Duration,
) -> Result<Vec<Metric>, BenchError> {
    let n = ctx
        .config
        .knowledge
        .knowledge_items
        .unwrap_or_else(|| real_items(ctx.scale()));
    let keep = full.select_items(n);
    let fx = if keep.len() < full.items.len() {
        full.subset(&keep)
    } else {
        full.clone()
    };
    ctx.param("corpus_items", full.items.len());
    if let Some(n) = ctx.config.knowledge.knowledge_items {
        if n >= full.items.len() {
            ctx.param("knowledge_items", "all");
        } else {
            ctx.param("knowledge_items", n);
        }
    }
    ctx.param(
        "subset",
        json!({
            "items": keep.len(),
            "rule": "scenario items (deleted, injection, job change, nickname, same first name) \
                     plus items evenly spaced through the manifest; gold restricted to what these \
                     items support (see Fixture::subset)",
            "ids": if keep.len() < full.items.len() { json!(keep) } else { json!("all") },
        }),
    );
    let baseline = baseline_if_dumping(ctx, &fx)?;
    let store = Store::open_in_memory()?;
    let ing = prepare_corpus(ctx, &fx, &store, Some(label))?;
    ctx.param("llm_timeout_s", timeout.as_secs_f64());
    #[cfg(feature = "pipeline")]
    ctx.param(
        "llm_limits",
        json!({
            "note": "nugget-knowledge LlmConfig defaults; `local` applies to on-device providers",
            "config": serde_json::to_value(nugget_knowledge::LlmConfig::default()).unwrap_or_default(),
        }),
    );
    ctx.param("llm_max_attempts", 1);
    ctx.param(
        "targets_note",
        "targets were set for the deterministic + scripted-LLM path; kept unchanged for the real \
         model, so a miss reads MISSED",
    );
    let metered = Arc::new(MeteredProvider::new(provider, timeout));
    let sctx = system_context(
        &fx,
        &store,
        metered.clone(),
        label.to_owned(),
        LlmSettings {
            timeout: Some(timeout + PIPELINE_TIMEOUT_MARGIN),
            max_attempts: Some(1),
        },
    )?;
    match wired::build(&sctx)? {
        Wiring::Ready(system) => run_with_system_and_baseline(
            ctx,
            &fx,
            &store,
            &ing,
            LlmUnderTest::Real(&metered),
            system,
            baseline.as_ref(),
        ),
        Wiring::Unavailable(why) => Err(BenchError::NotImplemented(why.into())),
    }
}

/// Ingest the corpus and record its statistics as parameters (scripted LLM).
pub fn prepare(ctx: &mut RunContext, fx: &Fixture, store: &Store) -> Result<Ingested, BenchError> {
    prepare_corpus(ctx, fx, store, None)
}

/// Ingest the corpus and record its statistics; `real_llm` is the real model's label, `None`
/// for the scripted fake (whose script statistics are recorded too).
fn prepare_corpus(
    ctx: &mut RunContext,
    fx: &Fixture,
    store: &Store,
    real_llm: Option<&str>,
) -> Result<Ingested, BenchError> {
    let s = fx.stats();
    ctx.param("corpus", fx.name.clone());
    ctx.param(
        "knowledge_corpus",
        ctx.config.knowledge.knowledge_corpus.as_str(),
    );
    ctx.param("items", s.items);
    ctx.param(
        "items_by_kind",
        serde_json::to_value(&s.items_by_kind).unwrap_or_default(),
    );
    ctx.param("gold_entities", s.entities);
    ctx.param(
        "gold_entities_by_kind",
        serde_json::to_value(&s.entities_by_kind).unwrap_or_default(),
    );
    ctx.param("gold_claims", s.claims);
    ctx.param("gold_superseded_claims", s.superseded_claims);
    ctx.param("gold_mentions", s.mentions);
    if real_llm.is_none() {
        ctx.param("scripted_hallucinated_quotes", s.hallucinated_quotes);
        ctx.param("scripted_injected_claims", s.injected_claims);
    }
    ctx.param("deleted_items", fx.scenario.delete.clone());
    ctx.param("injection_items", fx.scenario.injection.clone());
    ctx.param("user_edit_pages", fx.scenario.user_edit_pages);
    ctx.param(
        "store",
        "nugget-store in memory (unencrypted), ingest metadata layout, no embeddings",
    );
    match real_llm {
        None => ctx.param(
            "llm",
            "scripted fake (bench/fixtures/knowledge/llm_script.json), on-device",
        ),
        Some(label) => ctx.param("llm", format!("{label}, on-device")),
    }
    let ing = fixture::ingest(store, fx)?;
    ctx.param("chunks", ing.chunk_count());
    Ok(ing)
}

fn m(name: &str, value: f64, unit: &str, dir: Direction, desc: &str) -> Metric {
    Metric::new(name, value, unit, dir, desc)
}

fn flag(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

/// Run every phase against `system` and score it.
pub fn run_with_system(
    ctx: &mut RunContext,
    fx: &Fixture,
    store: &Store,
    ing: &Ingested,
    llm: LlmUnderTest<'_>,
    system: Box<dyn KnowledgeSystem>,
) -> Result<Vec<Metric>, BenchError> {
    run_with_system_and_baseline(ctx, fx, store, ing, llm, system, None)
}

/// [`run_with_system`], with a deterministic baseline for `--knowledge-dump`'s attribution.
pub fn run_with_system_and_baseline(
    ctx: &mut RunContext,
    fx: &Fixture,
    store: &Store,
    ing: &Ingested,
    llm: LlmUnderTest<'_>,
    mut system: Box<dyn KnowledgeSystem>,
    baseline: Option<&Baseline>,
) -> Result<Vec<Metric>, BenchError> {
    use Direction::{HigherIsBetter as Up, Info, LowerIsBetter as Down};
    ctx.param("system", system.label());
    let vault_tmp = tempfile::tempdir()?;
    let vault = vault_tmp.path();
    let gold = GoldIndex::new(fx);
    let gt = gold_triples(&gold);
    let mut out = Vec::new();

    // ---- extract ----
    let refs = ing.refs();
    let t = Instant::now();
    system.extract(&refs)?;
    let extract_s = secs(t.elapsed());
    let meter_extract = llm.meter();

    // ---- graph quality ----
    let snap = Snapshot::take(system.graph())?;
    let matching = match_entities(&gold, &snap);
    let ents = score_entities(&gold, &snap, &matching);
    let sys1 = system_triples(&gold, &snap, &matching);
    let claims = score_claims(&gt, &sys1, &snap);
    let none = HashSet::new();
    let support = score_support(fx, &snap, ing, &none);
    let res = score_resolution(&gold, &snap, ing);
    let (inj_bad, inj_citing) = injection_unsupported(&gold, &snap, &matching, ing, &none);
    let hc = &fx.scenario.hard_cases;
    let job_changes: Vec<bool> = hc
        .job_change
        .iter()
        .map(|jc| {
            jc.superseded
                .iter()
                .all(|id| !sys1.contains_key(&gt.by_id[id]))
                && jc.current.iter().all(|id| sys1.contains_key(&gt.by_id[id]))
        })
        .collect();

    if let Some(path) = ctx.config.knowledge.knowledge_dump.clone() {
        let requests = match llm {
            LlmUnderTest::Real(p) => request_lines(fx, store, ing, &p.requests()),
            LlmUnderTest::Scripted(_) => Vec::new(),
        };
        let lines = diagnose::lines(&diagnose::DumpInput {
            gold: &gold,
            gt: &gt,
            snap: &snap,
            matching: &matching,
            sys: &sys1,
            ing,
            baseline,
            requests,
        });
        diagnose::write(&path, &lines)?;
        ctx.param(
            "knowledge_dump",
            json!({"path": path.display().to_string(), "summary": lines[0]["counts"]}),
        );
    }

    out.push(
        m("entities.f1", ents.overall.f1(), "ratio", Up,
          "Entity F1: one-to-one match to gold by identifier or surface form, same kind (duplicates are false positives)")
            .with_target(ENTITY_F1_TARGET),
    );
    out.push(m(
        "entities.precision",
        ents.overall.precision(),
        "ratio",
        Up,
        "Entity precision",
    ));
    out.push(m(
        "entities.recall",
        ents.overall.recall(),
        "ratio",
        Up,
        "Entity recall",
    ));
    for kind in EntityKind::ALL {
        if let Some(p) = ents.by_kind.get(&kind) {
            out.push(m(
                &format!("entities.{}.f1", kind.as_str()),
                p.f1(),
                "ratio",
                Info,
                &format!("Entity F1 for kind {}", kind.as_str()),
            ));
        }
    }
    out.push(m(
        "entities.system",
        ents.system_entities as f64,
        "entities",
        Info,
        "Live entities in the system's graph",
    ));
    out.push(m("claims.f1", claims.prf.f1(), "ratio", Up,
               "Claim F1: exact (subject, predicate, object) match after resolution and predicate normalisation, active claims"));
    out.push(m(
        "claims.precision",
        claims.prf.precision(),
        "ratio",
        Up,
        "Claim precision (superseded gold claims count as false positives)",
    ));
    out.push(m(
        "claims.recall",
        claims.prf.recall(),
        "ratio",
        Up,
        "Claim recall over active, non-optional gold claims",
    ));
    out.push(m(
        "claims.superseded_active",
        claims.superseded_active as f64,
        "claims",
        Down,
        "Active claims that the corpus later supersedes (a job change)",
    ));
    out.push(
        m("claims.unsupported_rate", support.unsupported_rate(), "ratio", Down,
          "Share of active claims with no evidence, or with evidence whose item, chunk or quote does not check out")
            .with_target(0.0),
    );
    out.push(m(
        "claims.hallucinated_kept",
        support.hallucinated_kept as f64,
        "claims",
        Down,
        "Active claims citing a scripted hallucinated or injected quote",
    ));
    out.push(m(
        "claims.out_of_vocabulary",
        claims.out_of_vocabulary as f64,
        "triples",
        Info,
        "Active triples with a predicate the gold labels do not use (not scored)",
    ));
    out.push(m(
        "claims.system_active",
        claims.system_active as f64,
        "claims",
        Info,
        "Active claims in the system's graph",
    ));
    out.push(m(
        "resolution.b3.f1",
        res.b3_f1(),
        "ratio",
        Up,
        "B³ F1 over detected gold mentions",
    ));
    out.push(m(
        "resolution.b3.precision",
        res.b3_precision,
        "ratio",
        Up,
        "B³ precision",
    ));
    out.push(m(
        "resolution.b3.recall",
        res.b3_recall,
        "ratio",
        Up,
        "B³ recall",
    ));
    out.push(m(
        "resolution.mention_recall",
        res.mention_recall(),
        "ratio",
        Up,
        "Share of gold mentions the system recorded (same item, a gold surface form)",
    ));
    out.push(
        m("injection.unsupported_claims", inj_bad as f64, "claims", Down,
          "Active claims from the prompt-injection item that are not quote-supported, or equal to an injected triple")
            .with_target(0.0),
    );
    out.push(m(
        "injection.citing_claims",
        inj_citing as f64,
        "claims",
        Info,
        "Active claims citing the prompt-injection item",
    ));
    out.push(m(
        "throughput.items_per_s",
        refs.len() as f64 / extract_s,
        "items/s",
        Up,
        match llm {
            LlmUnderTest::Scripted(_) => {
                "Extraction and resolution throughput over the corpus (fake LLM: model time excluded)"
            }
            LlmUnderTest::Real(_) => {
                "Extraction and resolution throughput over the processed items, real local model time included"
            }
        },
    ));
    out.push(m(
        "throughput.extract_s",
        extract_s,
        "s",
        Info,
        "Wall time of the extraction phase",
    ));

    // ---- render ----
    let t = Instant::now();
    let r1 = system.render(vault)?;
    let render_ms = t.elapsed().as_secs_f64() * 1e3;
    let mut problems = system.validate(vault)?.len();
    let mut vs = score_vault(vault, &r1, ing, &none);

    // ---- user edits, deletion ----
    let eligible = |p: &crate::knowledge::system::RenderedPage| {
        snap.entity(&p.entity)
            .and_then(|e| matching.loose.get(&e.id))
            .and_then(|g| gold.kind(g))
            .is_some_and(|k| {
                matches!(
                    k,
                    EntityKind::Person | EntityKind::Organization | EntityKind::Project
                )
            })
    };
    let edits = eval::apply_user_edits(
        vault,
        &r1.pages,
        &eligible,
        fx.scenario.user_edit_pages,
        ctx.seed(),
    )?;
    let deleted_ids: HashSet<String> = fx.scenario.delete.iter().cloned().collect();
    let deleted: HashSet<ItemRef> = fx
        .scenario
        .delete
        .iter()
        .filter_map(|id| fx.item(id).map(|i| i.item_ref()))
        .collect();
    let deleted_list: Vec<ItemRef> = deleted.iter().cloned().collect();
    for r in &deleted_list {
        store.delete_item(r)?;
    }
    system.items_deleted(&deleted_list)?;
    let snap2 = Snapshot::take(system.graph())?;
    let matching2 = match_entities(&gold, &snap2);
    let sys2 = system_triples(&gold, &snap2, &matching2);
    let retr = score_retraction(&gt, &gold, &sys1, &sys2, &snap2, &deleted_ids, &deleted);
    let support2 = score_support(fx, &snap2, ing, &deleted);

    // ---- re-render ----
    let r2 = system.render(vault)?;
    problems += system.validate(vault)?.len();
    let vs2 = score_vault(vault, &r2, ing, &deleted);
    vs.add(&vs2);
    let preserved = edits_preserved(vault, &edits);

    out.push(
        m("vault.citation_coverage", vs.coverage(), "ratio", Up,
          "Share of rendered facts with at least one citation, over the render and the re-render (M2 exit criterion)")
            .with_target(1.0),
    );
    out.push(
        m("vault.invalid_citations", vs.invalid_citations as f64, "citations", Down,
          "Citations whose item is unknown or deleted, whose chunk is not the item's, or whose quote is not in it")
            .with_target(0.0),
    );
    out.push(
        m("vault.validator_errors", (problems + vs.missing_pages) as f64, "problems", Down,
          "Problems the vault validators report, plus rendered pages missing on disk, over both renders")
            .with_target(0.0),
    );
    out.push(m(
        "vault.pages",
        r1.pages.len() as f64,
        "pages",
        Info,
        "Pages in the first render",
    ));
    out.push(m(
        "vault.facts",
        r1.facts().count() as f64,
        "facts",
        Info,
        "Facts in the first render",
    ));
    out.push(m(
        "vault.render_ms",
        render_ms,
        "ms",
        Info,
        "Wall time of the first render",
    ));
    out.push(
        m("merge.fidelity", eval::ratio(preserved, edits.len()), "ratio", Up,
          "Share of simulated user edits still present verbatim after deleting items and re-rendering")
            .with_target(1.0),
    );
    out.push(m(
        "merge.edits",
        edits.len() as f64,
        "edits",
        Info,
        "Simulated user edits (two per edited page)",
    ));
    out.push(
        m("retraction.correctness", retr.correctness(), "ratio", Up,
          "After deleting items: claims supported only by them are gone, claims with support left stay active")
            .with_target(1.0),
    );
    out.push(
        m(
            "retraction.dangling_refs",
            retr.dangling as f64,
            "refs",
            Down,
            "Active claims citing, and mentions in, deleted items",
        )
        .with_target(0.0),
    );
    out.push(m(
        "retraction.checks",
        retr.checks as f64,
        "checks",
        Info,
        "Retraction checks evaluated",
    ));
    out.push(m(
        "claims.unsupported_rate_after_delete",
        support2.unsupported_rate(),
        "ratio",
        Down,
        "Unsupported-claim rate after the deletions",
    ));

    // ---- hard cases ----
    let deleted_ok = retr.correctness() == 1.0 && retr.dangling == 0;
    let scope = HardCaseScope::of(fx);
    let nicknames: Vec<bool> = hc
        .nickname_identity
        .iter()
        .map(|c| {
            resolved_strictly(&gold, &res, &snap, ing, &c.entity)
                && unattributed(
                    &gold,
                    &res,
                    &snap,
                    ing,
                    std::slice::from_ref(&c.entity),
                    &c.unattributed,
                )
        })
        .collect();
    let first_names: Vec<bool> = hc
        .same_first_name
        .iter()
        .map(|c| {
            // EE1 pairs also need each person resolved (the context-identified bare first name
            // included); the original pair (the Sams) keeps the M2 rule: separate is enough.
            distinct_identities(&gold, &res, &c.entities)
                && unattributed(&gold, &res, &snap, ing, &c.entities, &c.unattributed)
                && (c.added.is_none()
                    || c.entities
                        .iter()
                        .all(|e| resolved_strictly(&gold, &res, &snap, ing, e)))
        })
        .collect();
    let mut details = Vec::new();
    let detail = |kind: &str, case: String, in_scope: bool, ok: bool| {
        json!({
            "kind": kind,
            "case": case,
            "pass": if in_scope { json!(ok) } else { json!("not_in_subset") },
        })
    };
    for ((c, ok), s) in hc
        .nickname_identity
        .iter()
        .zip(&nicknames)
        .zip(&scope.nickname)
    {
        let mut d = detail("nickname_identity", c.entity.clone(), *s, *ok);
        // Not part of the pass: the pre-EE1 rule (majority vote over each mention's surfaces),
        // for comparison with runs of the pre-EE1 bench.
        d["majority_vote"] = json!(same_identity(&gold, &res, &c.entity));
        details.push(d);
    }
    for ((c, ok), s) in hc
        .same_first_name
        .iter()
        .zip(&first_names)
        .zip(&scope.first_name)
    {
        let mut d = detail("same_first_name", c.entities.join(" / "), *s, *ok);
        // Whether each person's mentions (bare first names resolved by context included) end
        // up on one entity of their own: part of the pass for EE1 pairs only.
        d["one_entity_each"] = json!(
            c.entities
                .iter()
                .all(|e| resolved_strictly(&gold, &res, &snap, ing, e))
        );
        details.push(d);
    }
    for ((c, ok), s) in hc
        .job_change
        .iter()
        .zip(&job_changes)
        .zip(&scope.job_change)
    {
        let who = fx
            .claims
            .iter()
            .find(|x| c.current.first() == Some(&x.id))
            .map_or_else(String::new, |x| x.subject.clone());
        details.push(detail("job_change", who, *s, *ok));
    }
    ctx.param("hard_case_details", details);
    // A kind passes when every case of it in scope passes.
    let all_in_scope = |oks: &[bool], s: &[bool]| oks.iter().zip(s).all(|(ok, s)| *ok || !*s);
    let scored = scope.kinds();
    let mut not_scored = Vec::new();
    for (name, ok, desc) in [
        (
            "nickname_identity",
            all_in_scope(&nicknames, &scope.nickname),
            "Every person under nicknames, display-name variants, initials or several addresses resolves to one entity holding nobody else's mentions, with every system mention under one of their surfaces in their items on it, and the same nickname with nothing to identify its bearer is not given to them",
        ),
        (
            "same_first_name",
            all_in_scope(&first_names, &scope.first_name),
            "People who share a first name stay separate entities (EE1 pairs: each also resolved to one entity, context-identified bare first names included), and a bare first name nothing identifies is given to neither",
        ),
        (
            "job_change",
            all_in_scope(&job_changes, &scope.job_change),
            "For every job change, the old employer and role are not active; the new ones are",
        ),
        (
            "deleted_item",
            deleted_ok,
            "Deleting items retracts exactly what only they supported",
        ),
        (
            "injection",
            inj_bad == 0,
            "No unsupported claim from the prompt-injection mail",
        ),
    ] {
        if scored.contains(&name) {
            out.push(m(&format!("hard_cases.{name}"), flag(ok), "pass", Up, desc));
        } else {
            not_scored.push(name);
        }
    }
    if !not_scored.is_empty() {
        ctx.param("hard_cases_not_in_subset", not_scored);
    }

    // ---- MCP ----
    let reps = ctx.scale().pick(1, 3, 5, 10);
    if let Some(mcp) = system.mcp() {
        let mut lat = Vec::new();
        let mut cited = 0;
        let mut calls = 0;
        for _ in 0..reps {
            for q in &fx.scenario.mcp_queries {
                let t = Instant::now();
                let a = mcp.get_context(q)?;
                lat.push(t.elapsed().as_secs_f64() * 1e3);
                calls += 1;
                if a.citations > 0 {
                    cited += 1;
                }
            }
        }
        if let Some(s) = summarize(&lat) {
            out.push(m(
                "mcp.get_context.p50_ms",
                s.p50,
                "ms",
                Down,
                "In-process MCP get_context round trip, p50",
            ));
            out.push(m(
                "mcp.get_context.p95_ms",
                s.p95,
                "ms",
                Down,
                "In-process MCP get_context round trip, p95",
            ));
        }
        out.push(m(
            "mcp.get_context.cited_rate",
            eval::ratio(cited, calls),
            "ratio",
            Up,
            "Share of get_context answers carrying at least one citation",
        ));
        ctx.param("mcp_calls", calls);
    } else {
        ctx.param("mcp", "not available (no in-process nugget-mcp server)");
    }

    let stats = system.stats();
    ctx.param("system_stats", stats.clone());
    match llm {
        LlmUnderTest::Scripted(llm) => {
            out.push(m(
                "llm.calls",
                llm.calls() as f64,
                "calls",
                Info,
                "Chat requests to the scripted LLM",
            ));
            out.push(m(
                "llm.unmatched",
                llm.unmatched() as f64,
                "calls",
                Info,
                "Chat requests the scripted LLM could not tie to a corpus item (answered with nothing)",
            ));
        }
        LlmUnderTest::Real(p) => {
            let during = meter_extract.unwrap_or_default();
            let total = p.snapshot();
            ctx.param("llm_meter", during.to_json());
            ctx.param(
                "llm_requests_after_extract",
                total.requests - during.requests,
            );
            real_llm_metrics(&mut out, &during, refs.len(), &stats);
        }
    }
    Ok(out)
}

/// `request` lines of the dump: each logged request with the quote gate's verdicts on its answer
/// (re-run on the chunks it was sent, which are still in the store before the deletions).
fn request_lines(
    fx: &Fixture,
    store: &Store,
    ing: &Ingested,
    log: &[crate::knowledge::metered::RequestLog],
) -> Vec<serde_json::Value> {
    let me = fx.entity(&fx.scenario.me).map(|e| e.name.clone());
    log.iter()
        .map(|r| {
            let item = r.items.first();
            let id = item.and_then(|i| ing.by_ref(i)).map(|s| s.id.clone());
            #[cfg(feature = "pipeline")]
            let verdicts = item.filter(|_| !r.answer.is_empty()).and_then(|i| {
                let it = store.get_item(i).ok()??;
                let chunks = store.get_chunks_by_ord(i, &r.chunk_ords).ok()?;
                let v = nugget_knowledge::llm::review(
                    &r.answer,
                    &it,
                    &chunks,
                    &nugget_knowledge::LlmConfig::default(),
                    me.as_deref(),
                );
                serde_json::to_value(v).ok()
            });
            #[cfg(not(feature = "pipeline"))]
            let verdicts = {
                let _ = (store, &me);
                None
            };
            diagnose::request_line(r, id.as_deref(), verdicts)
        })
        .collect()
}

/// Which hard cases the (possibly subset) corpus can decide; all of them on the full corpus.
struct HardCaseScope {
    nickname: Vec<bool>,
    first_name: Vec<bool>,
    job_change: Vec<bool>,
    deleted_item: bool,
    injection: bool,
}

impl HardCaseScope {
    fn of(fx: &Fixture) -> Self {
        let hc = &fx.scenario.hard_cases;
        let mentions = |e: &str| fx.mentions.iter().filter(|m| m.entity == e).count();
        let claim_in = |id: &String| fx.claims.iter().any(|c| &c.id == id && !c.optional);
        Self {
            nickname: hc
                .nickname_identity
                .iter()
                .map(|c| mentions(&c.entity) >= 2)
                .collect(),
            first_name: hc
                .same_first_name
                .iter()
                .map(|c| c.entities.iter().all(|e| mentions(e) > 0))
                .collect(),
            job_change: hc
                .job_change
                .iter()
                .map(|j| j.superseded.iter().chain(&j.current).all(claim_in))
                .collect(),
            deleted_item: !fx.scenario.delete.is_empty(),
            injection: !fx.scenario.injection.is_empty(),
        }
    }

    /// The hard-case kinds with at least one case in scope.
    fn kinds(&self) -> Vec<&'static str> {
        let any = |v: &[bool]| v.iter().any(|b| *b);
        [
            ("nickname_identity", any(&self.nickname)),
            ("same_first_name", any(&self.first_name)),
            ("job_change", any(&self.job_change)),
            ("deleted_item", self.deleted_item),
            ("injection", self.injection),
        ]
        .into_iter()
        .filter_map(|(k, b)| b.then_some(k))
        .collect()
    }
}

/// Real-model counters over the extraction phase, plus the pipeline's quote-gate counts.
fn real_llm_metrics(out: &mut Vec<Metric>, mt: &Meter, items: usize, stats: &serde_json::Value) {
    use Direction::{Info, LowerIsBetter as Down};
    out.push(m(
        "llm.requests",
        mt.requests as f64,
        "requests",
        Info,
        "Extraction requests the pipeline sent to the real model",
    ));
    out.push(m(
        "llm.items",
        items as f64,
        "items",
        Info,
        "Items processed (the scale's subset of the corpus)",
    ));
    for (name, v, desc) in [
        (
            "llm.failures",
            mt.failures,
            "Requests that failed: provider or stream error, no Done, refusal",
        ),
        (
            "llm.timeouts",
            mt.timeouts,
            "Requests with no complete answer within the per-request timeout",
        ),
        (
            "llm.unparseable",
            mt.unparseable,
            "Complete answers with no JSON object in them",
        ),
        (
            "llm.schema_invalid",
            mt.schema_invalid,
            "JSON answers not valid against the extraction schema (the pipeline may salvage parts)",
        ),
    ] {
        out.push(m(name, v as f64, "requests", Down, desc));
    }
    out.push(m(
        "llm.truncated",
        mt.truncated as f64,
        "requests",
        Info,
        "Answers that stopped at the output-token cap",
    ));
    out.push(m(
        "llm.lost_rate",
        eval::ratio(mt.lost(), mt.requests),
        "ratio",
        Down,
        "Share of requests that gave the pipeline nothing: failures, timeouts, unparseable",
    ));
    if let Some(s) = summarize(&mt.latencies_s) {
        out.push(m(
            "llm.latency.p50",
            s.p50,
            "s",
            Down,
            "Wall time per extraction request, p50 (timeouts count at the timeout)",
        ));
        out.push(m(
            "llm.latency.p95",
            s.p95,
            "s",
            Down,
            "Wall time per extraction request, p95 (timeouts count at the timeout)",
        ));
    }
    let run = &stats["runs"][0];
    for (key, name, desc) in [
        (
            "llm_claims_accepted",
            "llm.gate.claims_accepted",
            "Model claims the pipeline's quote gate accepted",
        ),
        (
            "llm_claims_dropped",
            "llm.gate.claims_dropped",
            "Model claims the quote gate dropped (no verbatim, relevant quote, or malformed)",
        ),
        (
            "llm_entities_dropped",
            "llm.gate.entities_dropped",
            "Model entities dropped (not named in the text, or malformed)",
        ),
    ] {
        if let Some(v) = run[key].as_u64() {
            out.push(m(name, v as f64, "count", Info, desc));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::oracle::{Faults, Oracle};
    use crate::report::{Report, Status};
    use crate::runner::{RunConfig, Scale, run_benchmark};

    fn run(faults: Faults) -> Vec<Metric> {
        let fx = Fixture::builtin().unwrap();
        let store = Store::open_in_memory().unwrap();
        let mut ctx = RunContext::new(RunConfig {
            scale: Scale::Smoke,
            ..RunConfig::default()
        });
        let ing = prepare(&mut ctx, &fx, &store).unwrap();
        let llm = ScriptedLlm::new(&fx);
        let sys = Box::new(Oracle::new(&fx, &ing, faults));
        run_with_system(
            &mut ctx,
            &fx,
            &store,
            &ing,
            LlmUnderTest::Scripted(&llm),
            sys,
        )
        .unwrap()
    }

    fn get(ms: &[Metric], name: &str) -> f64 {
        ms.iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("{name}"))
            .value
    }

    fn missed(ms: &[Metric]) -> Vec<&str> {
        ms.iter()
            .filter(|m| m.target_met == Some(false))
            .map(|m| m.name.as_str())
            .collect()
    }

    /// The real pipeline completes, reports every gated metric, and every LLM request matched an
    /// item. Metric values are not asserted.
    #[cfg(feature = "pipeline")]
    #[test]
    fn real_pipeline_smoke() {
        let r: Report = run_benchmark(
            &KnowledgeBench,
            &RunConfig {
                scale: Scale::Smoke,
                ..RunConfig::default()
            },
        );
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        let fx = Fixture::builtin().unwrap();
        assert_eq!(r.config.params["items"], fx.items.len());
        assert!(r.config.params["chunks"].as_u64().unwrap() > 0);
        assert!(
            r.config.params["system"]
                .as_str()
                .unwrap()
                .contains("nugget-knowledge")
        );
        assert!(
            r.config.params["system_stats"]["runs"]
                .as_array()
                .unwrap()
                .len()
                >= 2
        );
        let get = |n: &str| r.metric(n).unwrap_or_else(|| panic!("{n}")).value;
        assert_eq!(get("llm.calls"), fx.items.len() as f64);
        assert_eq!(get("llm.unmatched"), 0.0);
        // The scripted run is the whole corpus, every hard case scored, no real-model metrics.
        for n in [
            "llm.requests",
            "llm.failures",
            "llm.latency.p50",
            "llm.items",
        ] {
            assert!(r.metric(n).is_none(), "{n}");
        }
        assert_eq!(
            r.metrics
                .iter()
                .filter(|m| m.name.starts_with("hard_cases."))
                .count(),
            5
        );
        assert!(r.config.params.contains_key("scripted_hallucinated_quotes"));
        assert!(!r.config.params.contains_key("subset"));
        assert!(
            r.metric("throughput.items_per_s")
                .unwrap()
                .description
                .contains("model time excluded")
        );
        assert!(get("throughput.items_per_s") > 0.0);
        assert!(get("vault.pages") > 0.0 && get("vault.facts") > 0.0);
        assert!(get("mcp.get_context.p95_ms") >= get("mcp.get_context.p50_ms"));
        for t in [
            "entities.f1",
            "claims.unsupported_rate",
            "injection.unsupported_claims",
            "vault.citation_coverage",
            "vault.invalid_citations",
            "vault.validator_errors",
            "merge.fidelity",
            "retraction.correctness",
            "retraction.dangling_refs",
        ] {
            assert!(r.targets.iter().any(|c| c.metric == t), "{t}");
        }
    }

    #[test]
    fn oracle_meets_every_target() {
        let ms = run(Faults::default());
        assert!(missed(&ms).is_empty(), "{:?}", missed(&ms));
        for name in [
            "entities.f1",
            "claims.f1",
            "resolution.b3.f1",
            "resolution.mention_recall",
            "vault.citation_coverage",
            "merge.fidelity",
            "retraction.correctness",
            "hard_cases.nickname_identity",
            "hard_cases.same_first_name",
            "hard_cases.job_change",
            "hard_cases.deleted_item",
            "hard_cases.injection",
            "mcp.get_context.cited_rate",
        ] {
            assert_eq!(get(&ms, name), 1.0, "{name}");
        }
        assert_eq!(get(&ms, "claims.unsupported_rate"), 0.0);
        assert_eq!(get(&ms, "claims.unsupported_rate_after_delete"), 0.0);
        assert!(get(&ms, "retraction.checks") >= 2.0);
        assert_eq!(get(&ms, "merge.edits"), 16.0);
        assert!(get(&ms, "throughput.items_per_s") > 0.0);
        assert!(get(&ms, "vault.facts") > 100.0);
    }

    #[test]
    fn trusting_the_llm_shows_up_as_unsupported_and_injected_claims() {
        let ms = run(Faults {
            trust_llm: true,
            ..Faults::default()
        });
        assert!(get(&ms, "claims.unsupported_rate") > 0.0);
        let s = Fixture::builtin().unwrap().stats();
        assert_eq!(
            get(&ms, "claims.hallucinated_kept"),
            (s.hallucinated_quotes + s.injected_claims) as f64
        );
        assert!(get(&ms, "injection.unsupported_claims") >= 3.0);
        assert_eq!(get(&ms, "hard_cases.injection"), 0.0);
        assert!(get(&ms, "claims.precision") < 1.0);
        assert_eq!(get(&ms, "claims.recall"), 1.0);
        // Every scripted fabricated quote lands in a rendered citation that does not check out.
        assert!(get(&ms, "vault.invalid_citations") > 0.0);
        assert!(missed(&ms).contains(&"claims.unsupported_rate"));
    }

    #[test]
    fn ignoring_deletes_fails_retraction() {
        let ms = run(Faults {
            no_retraction: true,
            ..Faults::default()
        });
        assert!(get(&ms, "retraction.correctness") < 1.0);
        assert!(get(&ms, "retraction.dangling_refs") > 0.0);
        assert_eq!(get(&ms, "hard_cases.deleted_item"), 0.0);
        assert!(
            get(&ms, "vault.invalid_citations") > 0.0,
            "re-render cites deleted items"
        );
        assert!(get(&ms, "claims.unsupported_rate_after_delete") > 0.0);
    }

    #[test]
    fn clobbering_edits_and_dropping_citations_fail_their_targets() {
        let ms = run(Faults {
            clobber_edits: true,
            ..Faults::default()
        });
        assert_eq!(get(&ms, "merge.fidelity"), 0.0);
        let ms = run(Faults {
            drop_citations: true,
            ..Faults::default()
        });
        assert_eq!(get(&ms, "vault.citation_coverage"), 0.0);
        assert_eq!(missed(&ms), ["vault.citation_coverage"]);
    }

    #[test]
    fn keeping_the_old_job_fails_the_job_change() {
        let ms = run(Faults {
            no_supersede: true,
            ..Faults::default()
        });
        assert_eq!(get(&ms, "hard_cases.job_change"), 0.0);
        let superseded = Fixture::builtin().unwrap().stats().superseded_claims;
        assert_eq!(get(&ms, "claims.superseded_active"), superseded as f64);
        assert!(get(&ms, "claims.precision") < 1.0);
    }
}
