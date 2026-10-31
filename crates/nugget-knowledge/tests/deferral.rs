//! Model-extracted facts are not replaced by a run that cannot use the model (QA GG5-8), runs
//! stop on interrupt, and `pending_items` counts work a carried-over plan does not cover yet
//! (QA GG5-7).

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use common::*;
use nugget_core::capability::{Locality, Tier};
use nugget_graph::EntityKind;
use nugget_knowledge::{Knowledge, KnowledgeConfig, LlmConfig, RunBudget};
use nugget_store::{NewItem, content_hash};
use serde_json::json;

const MEMO: &str = "Meeting notes. Priya Patel joined Initech as head of research in March.";

fn memo(version: &str, extra: &str) -> (NewItem, String) {
    let text = format!("{MEMO}{extra}");
    let item = NewItem::new("files:notes", "memo.txt", content_hash(&text))
        .with_title("Notes")
        .with_mime("text/plain")
        .with_version(version)
        .with_metadata(json!({"source": {}, "mime": "text/plain", "junk": false}));
    (item, text)
}

fn put_memo(env: &Env, version: &str, extra: &str) {
    let (item, text) = memo(version, extra);
    env.put(item, &[&text]);
}

fn reply() -> String {
    json!({
        "entities": [
            {"ref": "e1", "kind": "person", "name": "Priya Patel"},
            {"ref": "e2", "kind": "organization", "name": "Initech"}
        ],
        "claims": [
            {"subject": "e1", "predicate": "works_at", "object": "e2",
             "text": "Priya Patel works at Initech.", "confidence": 0.9,
             "quotes": [{"chunk": 0, "text": "Priya Patel joined Initech as head of research"}]}
        ]
    })
    .to_string()
}

fn llm_cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        llm: LlmConfig {
            enabled: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn no_model_budget() -> RunBudget {
    RunBudget {
        max_llm_requests: 0,
        ..RunBudget::unlimited()
    }
}

fn works_at(env: &Env) -> usize {
    let priya = env.entity(EntityKind::Person, "Priya Patel");
    env.active(&priya, "works_at").len()
}

async fn pending(k: &Knowledge) -> u64 {
    let k = k.clone();
    tokio::task::spawn_blocking(move || k.pending_items())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn a_changed_item_waits_for_the_model_instead_of_losing_its_model_facts() {
    let env = Env::new();
    put_memo(&env, "1", "");
    let p = FakeProvider::new("local", Locality::OnDevice, |_| reply());
    let k = env.knowledge(llm_cfg(), Some(router(Tier::Synthesis, vec![p.clone()])));
    let s = run(&k).await;
    assert_eq!((s.items_processed, p.calls()), (1, 1));
    assert_eq!(works_at(&env), 1);

    // The file is re-saved while the model cannot be used (power constrained: no requests).
    put_memo(&env, "2", " Re-saved.");
    let s = k.run_once(no_model_budget()).await.unwrap();
    assert_eq!(s.items_deferred, 1, "{s:?}");
    assert_eq!(s.items_processed, 0);
    assert_eq!(s.claims_retracted, 0);
    assert_eq!(p.calls(), 1);
    assert_eq!(works_at(&env), 1, "the model's fact is kept");
    assert_eq!(pending(&k).await, 1, "the deferred item is pending");

    // Still deferred on the next constrained run, then extracted once the model may run.
    let s = k.run_once(no_model_budget()).await.unwrap();
    assert_eq!(s.items_deferred, 1);
    let s = run(&k).await;
    assert_eq!((s.items_processed, s.items_deferred), (1, 0), "{s:?}");
    assert_eq!(p.calls(), 2);
    assert_eq!(works_at(&env), 1);
    assert_eq!(pending(&k).await, 0);
    let s = run(&k).await;
    assert_eq!(s.planned, 0, "nothing left over: {s:?}");
}

#[tokio::test]
async fn an_item_without_model_facts_is_still_extracted_without_the_model() {
    let env = Env::new();
    put_memo(&env, "1", "");
    let p = FakeProvider::new("local", Locality::OnDevice, |_| reply());
    let k = env.knowledge(llm_cfg(), Some(router(Tier::Synthesis, vec![p.clone()])));
    // Never had the model: deterministic extraction goes ahead and the model is retried later.
    let s = k.run_once(no_model_budget()).await.unwrap();
    assert_eq!((s.items_processed, s.items_deferred), (1, 0));
    let s = run(&k).await;
    assert_eq!(s.items_processed, 1);
    assert_eq!(p.calls(), 1);
    assert_eq!(works_at(&env), 1);
}

#[tokio::test]
async fn interrupt_keeps_model_facts_of_an_item_whose_request_was_cut_off() {
    let env = Env::new();
    put_memo(&env, "1", "");
    let k_cell: Arc<OnceLock<Knowledge>> = Arc::new(OnceLock::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let (cell, n) = (k_cell.clone(), calls.clone());
    let p = FakeProvider::new("local", Locality::OnDevice, move |_| {
        if n.fetch_add(1, Ordering::SeqCst) == 1 {
            // The daemon stops while this request is in flight; the answer is lost.
            cell.get().unwrap().interrupt();
            return "connection closed".into();
        }
        reply()
    });
    let k = env.knowledge(llm_cfg(), Some(router(Tier::Synthesis, vec![p.clone()])));
    let _ = k_cell.set(k.clone());
    run(&k).await;
    assert_eq!(works_at(&env), 1);

    put_memo(&env, "2", " Re-saved.");
    let s = run(&k).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!((s.items_processed, s.items_deferred), (0, 1), "{s:?}");
    assert_eq!(works_at(&env), 1, "nothing retracted");

    // While interrupted, runs do nothing.
    let s = run(&k).await;
    assert_eq!((s.planned, s.items_processed), (0, 0));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    k.resume();
    let s = run(&k).await;
    assert_eq!(s.items_processed, 1, "{s:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(works_at(&env), 1);
}

#[tokio::test]
async fn pending_items_counts_changes_a_carried_plan_does_not_cover() {
    let env = Env::new();
    let put = |key: &str| {
        let text = format!("Note {key}. Ada Lovelace met Charles Babbage.");
        env.put(
            NewItem::new("files:notes", key, content_hash(&text))
                .with_title(key)
                .with_mime("text/plain")
                .with_version("1")
                .with_metadata(json!({"source": {}, "mime": "text/plain"})),
            &[&text],
        );
    };
    for k in ["a.txt", "b.txt", "c.txt"] {
        put(k);
    }
    let k = env.knowledge(KnowledgeConfig::default(), None);
    let one = RunBudget {
        max_items: 1,
        ..RunBudget::unlimited()
    };
    // First run: full plan of 3, one done, two carried over.
    let s = k.run_once(one).await.unwrap();
    assert_eq!(s.remaining, 2);
    assert_eq!(pending(&k).await, 2);
    // Committed while the plan is carried over: the carried plan does not have it.
    put("d.txt");
    assert_eq!(pending(&k).await, 3);
    let s = k.run_once(one).await.unwrap();
    assert_eq!(s.remaining, 1);
    let s = k.run_once(one).await.unwrap();
    assert_eq!(s.remaining, 0, "the carried chain ends");
    // ... and the new item is still pending (the worker plans again after a chain).
    assert_eq!(pending(&k).await, 1);
    let s = run(&k).await;
    assert_eq!(s.items_processed, 1);
    assert_eq!(pending(&k).await, 0);
}
