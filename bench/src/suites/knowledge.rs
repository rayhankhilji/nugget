//! `knowledge`: the M2 knowledge graph, measured on a labelled fixture corpus.
//!
//! 1. **Ingest.** The fixture corpus (mail, calendar, notes; `bench/fixtures/knowledge/`) is
//!    extracted, chunked and stored in a `nugget-store` the way `nugget-ingest` stores it.
//! 2. **Extract.** The pipeline under test ([`KnowledgeSystem`], built by
//!    [`wired::build`]) extracts every item, with a scripted fake LLM
//!    ([`ScriptedLlm`]) as its `Synthesis` provider. Timed for throughput.
//! 3. **Score the graph** against the gold labels: entities, claims (exact triple after
//!    resolution), resolution (B³), unsupported claims (quotes not in the cited chunk),
//!    injection resistance and the hard cases.
//! 4. **Render** the vault; check citation coverage, citations and the vault validators.
//! 5. **User edits** on a seeded sample of pages, then **delete** the scenario's items and
//!    tell the pipeline; check retraction.
//! 6. **Re-render**; check that every user edit survived (merge fidelity) and the citations
//!    again.
//! 7. **MCP** `get_context` round trips, when the system has an in-process MCP server.
//!
//! Built without the `pipeline` feature, [`wired::build`] says so and the suite reports
//! `not_implemented` with that message (corpus statistics are still recorded in `params`).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use nugget_core::ids::ItemRef;
use nugget_graph::EntityKind;
use nugget_store::Store;

use crate::knowledge::eval::{
    self, GoldIndex, Snapshot, distinct_identities, edits_preserved, gold_triples,
    injection_unsupported, match_entities, same_identity, score_claims, score_entities,
    score_resolution, score_retraction, score_support, score_vault, system_triples,
};
use crate::knowledge::fake_llm::ScriptedLlm;
use crate::knowledge::fixture::{self, Fixture, Ingested};
use crate::knowledge::system::{KnowledgeSystem, SystemContext};
use crate::knowledge::wired::{self, Wiring};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, Benchmark, RunContext};
use crate::stats::summarize;
use crate::suites::secs;

/// Entity F1 the deterministic + fake-LLM path must reach.
pub const ENTITY_F1_TARGET: f64 = 0.8;

pub struct KnowledgeBench;

impl Benchmark for KnowledgeBench {
    fn name(&self) -> &'static str {
        "knowledge"
    }

    fn description(&self) -> &'static str {
        "Knowledge graph: entity, claim and resolution quality, citations, retraction, vault merge (M2)"
    }

    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        let fx = Fixture::builtin()?;
        let store = Store::open_in_memory()?;
        let ing = prepare(ctx, &fx, &store)?;
        let llm = Arc::new(ScriptedLlm::new(&fx));
        let me = fx
            .entity(&fx.scenario.me)
            .ok_or_else(|| BenchError::Failed("scenario.me is not an entity".into()))?;
        let sctx = SystemContext {
            store: store.clone(),
            llm: llm.clone(),
            me_name: me.name.clone(),
            me_emails: me
                .identifiers
                .iter()
                .filter(|i| i.scheme == "email")
                .map(|i| i.value.clone())
                .collect(),
            webmail_domains: fx.scenario.webmail_domains.clone(),
        };
        match wired::build(&sctx)? {
            Wiring::Ready(system) => run_with_system(ctx, &fx, &store, &ing, &llm, system),
            Wiring::Unavailable(why) => Err(BenchError::NotImplemented(why.into())),
        }
    }
}

/// Ingest the corpus and record its statistics as parameters.
pub fn prepare(ctx: &mut RunContext, fx: &Fixture, store: &Store) -> Result<Ingested, BenchError> {
    let s = fx.stats();
    ctx.param("corpus", fx.name.clone());
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
    ctx.param("scripted_hallucinated_quotes", s.hallucinated_quotes);
    ctx.param("scripted_injected_claims", s.injected_claims);
    ctx.param("deleted_items", fx.scenario.delete.clone());
    ctx.param("injection_items", fx.scenario.injection.clone());
    ctx.param("user_edit_pages", fx.scenario.user_edit_pages);
    ctx.param(
        "store",
        "nugget-store in memory (unencrypted), ingest metadata layout, no embeddings",
    );
    ctx.param(
        "llm",
        "scripted fake (bench/fixtures/knowledge/llm_script.json), on-device",
    );
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
    llm: &ScriptedLlm,
    mut system: Box<dyn KnowledgeSystem>,
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
    let job_change = hc
        .job_change
        .superseded
        .iter()
        .all(|id| !sys1.contains_key(&gt.by_id[id]))
        && hc
            .job_change
            .current
            .iter()
            .all(|id| sys1.contains_key(&gt.by_id[id]));

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
        "Extraction and resolution throughput over the corpus (fake LLM: model time excluded)",
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
    for (name, ok, desc) in [
        (
            "nickname_identity",
            same_identity(&gold, &res, &hc.nickname_identity),
            "One person under a nickname, a personal address and calendar attendee names resolves to one entity",
        ),
        (
            "same_first_name",
            distinct_identities(&gold, &res, &hc.same_first_name),
            "Two people who share a first name stay separate entities",
        ),
        (
            "job_change",
            job_change,
            "The old employer and role are not active; the new ones are",
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
        out.push(m(&format!("hard_cases.{name}"), flag(ok), "pass", Up, desc));
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

    ctx.param("system_stats", system.stats());
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
    Ok(out)
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
        run_with_system(&mut ctx, &fx, &store, &ing, &llm, sys).unwrap()
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

    /// The real pipeline (default build): the suite completes, every gated metric is reported,
    /// and the scripted LLM tied every request to a corpus item. The values themselves are the
    /// pipeline's results, not asserted here.
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

    #[cfg(not(feature = "pipeline"))]
    #[test]
    fn without_the_pipeline_the_suite_is_not_implemented() {
        let r: Report = run_benchmark(
            &KnowledgeBench,
            &RunConfig {
                scale: Scale::Smoke,
                ..RunConfig::default()
            },
        );
        assert_eq!(r.status, Status::NotImplemented, "{:?}", r.message);
        assert!(r.message.unwrap().contains("pipeline"));
        assert!(r.config.params["chunks"].as_u64().unwrap() > 0);
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
        assert_eq!(get(&ms, "claims.superseded_active"), 3.0);
        assert!(get(&ms, "claims.precision") < 1.0);
    }
}
