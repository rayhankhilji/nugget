//! Query embedding through the router's `Embedding` tier, with a locality gate.

use std::sync::Arc;

use nugget_core::Error;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_infer::{Provider, Router};

/// Embedding providers eligible for retrieval, in the order to try them, and whether any
/// provider was left out only because it is a cloud provider and cloud embedding is not allowed.
///
/// Order: providers probed with embeddings (healthy before cooling down, each in configured
/// order, as [`Router::candidates`] returns them), then providers assigned to the `Embedding`
/// tier that have not been probed yet (being in that tier is the configuration's claim that they
/// embed; a failure just means text-only search). Providers probed *without* embeddings are
/// never tried.
pub(crate) fn embedding_candidates(
    router: &Router,
    allow_cloud: bool,
) -> (Vec<Arc<dyn Provider>>, bool) {
    let required = Capabilities {
        embeddings: true,
        ..Default::default()
    };
    let mut ordered: Vec<Arc<dyn Provider>> = router.candidates(Tier::Embedding, &required);
    if let Some(ids) = router.tiers().get(&Tier::Embedding) {
        for id in ids {
            if router.capabilities(id).is_some() || ordered.iter().any(|p| p.id() == id) {
                continue;
            }
            if let Some(p) = router.provider(id) {
                ordered.push(p);
            }
        }
    }
    let mut cloud_skipped = false;
    let allowed = ordered
        .into_iter()
        .filter(|p| {
            let ok = allow_cloud || p.meta().locality != Locality::Cloud;
            cloud_skipped |= !ok;
            ok
        })
        .collect();
    (allowed, cloud_skipped)
}

/// Check an embedding response: one finite, non-empty vector per input, all the same length.
pub(crate) fn validate_vectors(vectors: &[Vec<f32>], expected: usize) -> Result<(), String> {
    if vectors.len() != expected {
        return Err(format!(
            "provider returned {} vectors for {expected} inputs",
            vectors.len()
        ));
    }
    let dim = vectors.first().map_or(0, Vec::len);
    for v in vectors {
        if v.is_empty() || v.len() != dim {
            return Err("provider returned empty or ragged vectors".into());
        }
        if v.iter().any(|x| !x.is_finite()) {
            return Err("provider returned a non-finite vector".into());
        }
    }
    Ok(())
}

/// Embed search queries ([`Provider::embed_query`]) with the first provider that succeeds. On
/// failure returns the last error.
pub(crate) async fn embed_first(
    router: &Router,
    providers: &[Arc<dyn Provider>],
    texts: &[String],
) -> Result<(Arc<dyn Provider>, Vec<Vec<f32>>), String> {
    let mut last = String::from("no embedding provider");
    for p in providers {
        match p.embed_query(texts).await {
            Ok(v) => match validate_vectors(&v, texts.len()) {
                Ok(()) => {
                    router.report_success(p.id());
                    return Ok((p.clone(), v));
                }
                Err(msg) => {
                    tracing::warn!(provider = %p.id(), %msg, "bad query embedding");
                    router.report_failure(p.id());
                    last = format!("{}: {msg}", p.id());
                }
            },
            Err(e) => {
                tracing::warn!(provider = %p.id(), error = %e, "query embedding failed");
                if !matches!(e, Error::Invalid(_) | Error::Cancelled) {
                    router.report_failure(p.id());
                }
                last = format!("{}: {e}", p.id());
            }
        }
    }
    Err(last)
}
