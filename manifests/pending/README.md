# Pending manifest entries (not shipped)

Entries here are proposed for `manifests/default.json` but are **not** in the signed manifest
the app embeds. Nothing reads this directory.

## TODO (orchestrator): the reranker, `rerank.json`

`bge-reranker-v2-m3-q4km` (BAAI bge-reranker-v2-m3, Apache-2.0; GGUF from `gpustack`, pinned to
a revision; size and SHA-256 from the Hugging Face API) for the `rerank` runtime role
(`[runtime] rerank_model = "bge-reranker-v2-m3-q4km"`, served with `llama-server --reranking`).
`family` is `bert`: llama.cpp converts XLM-RoBERTa classifiers to its `bert` architecture.

It cannot be merged as is: manifest validation needs a non-empty `tiers`, and
`nugget_core::capability::Tier` has no rerank tier. Tagging it `embedding` would let the
embedding recommender pick a reranker for embeddings. Signing it in needs a **contract change**:

1. Add `Tier::Rerank` (serde `rerank`), or another way to mark rerankers, to
   `nugget_core::capability`.
2. Keep rerankers out of chat and embedding everywhere tiers are read: `nugget_models::recommend`,
   `nugget-runtime` `check_role` / `provider_config` (which treat every non-embedding tier as a
   chat tier), and `nuggetd` `local::selected_models` / `slot_tiers`, plus the router's tier map
   and `ALL_TIERS`. Then `check_role` can require the tier for the rerank role (it accepts any
   explicitly configured model today).
3. Merge the entry into `manifests/default.json` and re-sign:

   ```sh
   NUGGET_MANIFEST_SIGNING_KEY_FILE=<dev key> \
     cargo run -p nugget-models --example sign-manifest -- manifests/default.json
   cargo run -p nugget-models --example sign-manifest -- --verify manifests/default.json
   ```

4. Delete `rerank.json` (and this file once empty).

Until then `[runtime] rerank_model` can only name an entry of the signed manifest, none of which
is a reranker; `[retrieve.reranker]` points search at a reranker you run yourself.
