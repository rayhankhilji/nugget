//! End-to-end against the real llama.cpp build from the shipped manifest. Downloads the
//! llama-server release archive for this platform (20–40 MB) and the manifest's embedding model
//! (~150 MB), so it is ignored by default and never runs in CI:
//!
//! ```text
//! cargo test -p nugget-runtime --test real_llama -- --ignored --nocapture
//! ```

use std::sync::Arc;

use nugget_core::capability::Tier;
use nugget_core::egress::NullEgress;
use nugget_core::paths::Paths;
use nugget_infer::{ChatRequest, Message, build_provider_with};
use nugget_models::{ModelStore, default_manifest, detect_hardware, recommend};
use nugget_runtime::{Runtime, RuntimeRole, RuntimeState};

#[tokio::test]
#[ignore = "downloads the real llama-server and nomic-embed (~200 MB)"]
async fn real_llama_server_serves_nomic_embeddings() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path().to_path_buf());
    let store = Arc::new(ModelStore::open(&paths, Arc::new(NullEgress)).unwrap());
    let manifest = default_manifest().unwrap();
    let model = recommend(&manifest, &detect_hardware(), Tier::Embedding)
        .expect("an embedding model in the manifest")
        .clone();
    store.install(&model, |_| {}).await.unwrap();

    let rt = Runtime::new(&paths, store.clone(), Arc::new(NullEgress)).unwrap();
    let (bin, accel) = rt.install_runtime(|_| {}).await.unwrap();
    eprintln!("runtime: {} ({})", bin.display(), accel.as_str());
    let cfg = rt.ensure(RuntimeRole::Embedding, &model).await.unwrap();
    eprintln!("ready at {:?}", cfg.base_url);
    assert_eq!(rt.status()[0].state, RuntimeState::Ready);

    let secrets = rt.secrets();
    let c = cfg.clone();
    let provider = tokio::task::spawn_blocking(move || {
        build_provider_with(&c, Arc::new(NullEgress), &secrets).unwrap()
    })
    .await
    .unwrap();
    let texts = vec![
        "search_query: what is the capital of France?".to_string(),
        "search_document: Paris is the capital of France.".to_string(),
        "search_document: Bananas are rich in potassium.".to_string(),
    ];
    let v = provider.embed(&texts).await.unwrap();
    assert_eq!(v.len(), 3);
    assert_eq!(v[0].len(), 768, "nomic-embed-text-v1.5 has 768 dimensions");
    let norm: f32 = v[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-3, "normalized: {norm}");
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    assert!(
        dot(&v[0], &v[1]) > dot(&v[0], &v[2]),
        "the relevant document is closer"
    );

    // An embedding server does not chat.
    let chat = provider
        .chat(ChatRequest::new(vec![Message::user_text("hi")]))
        .await;
    assert!(
        chat.is_err() || {
            use futures::StreamExt;
            let mut s = chat.unwrap();
            matches!(s.next().await, Some(Err(_)) | None)
        }
    );

    let log = std::fs::read_to_string(rt.log_file(RuntimeRole::Embedding)).unwrap();
    assert!(
        !log.contains(secrets_key(&rt).as_str()),
        "API key leaked to the log"
    );
    rt.shutdown().await;
}

fn secrets_key(rt: &Runtime) -> String {
    use nugget_infer::SecretResolver;
    rt.secrets()
        .resolve(&nugget_runtime::api_key_ref(RuntimeRole::Embedding))
        .unwrap()
        .expose()
        .to_string()
}
