# Pending manifest entries

Files here propose model entries for [`../default.json`](../default.json) that have not been
signed into it yet. Each file has the manifest's shape, `{"models": [...]}`, and every entry is
exactly what would be copied into `default.json`'s `models` array: pinned upstream revision,
exact size and SHA-256 from the source, licence, tiers and RAM floor. Signing is the
orchestrator's job (`cargo run -p nugget-models --example sign-manifest`, see
[`docs/release.md`](../../docs/release.md)); nobody working on a proposal touches a signing key.

**`nuggetd` never reads this directory.** The app only knows the signed manifest
(`nugget_models::default_manifest`, which embeds `default.json` and checks its signature), so
an entry here cannot be installed, recommended or run by the app until it is signed into the
manifest. The only reader is the benchmark harness, and only when asked to.

## How the bench evaluates a pending entry

`nugget-bench --local-models --local-pending manifests/pending/<file> --local-chat-model <id>`
loads the file, checks every entry with `ModelEntry::validate()` (the same rules as a signed
entry: exact commit revision, a URL that names it, 64-hex SHA-256, non-empty tiers and licence),
and looks `--local-chat-model` up in the signed manifest first, then in the pending file. The
model store installs it like any other model: download from the pinned URL, then the size and
SHA-256 are verified before the file is used. Reports record `local.chat.pending: true` and the
pending file it came from. Without `--local-chat-model` the bench still picks the signed
manifest's smallest chat model, so existing results stay comparable. `bench/src/local.rs`'s
tests check that every file here parses, every entry validates, and no id collides with the
signed manifest.

`.github/workflows/evals.yml` (`doubletap` job) runs `doubletap` with each of these models and
with `qwen3-4b-q4km`, with and without read-ahead, on the same runner, and `draft --draft-judge
model` with each for drafting quality. A model should be recommended as the Double Tap default
only if its drafting quality holds against `qwen3-4b-q4km` (voice match, hallucination, fact
recall; sensitive leaks must stay 0). See `bench/README.md`, "Real-model results".

## `doubletap-small.json`

Small instruct models proposed for local Double Tap (`[draft.local] model`), not for Chat. CC1
proposed two; `qwen3-1.7b-q8` has since been signed into `default.json` (its size and SHA-256
re-checked against the Hugging Face API at signing). `qwen3-0.6b-q8` stays here, not recommended:
its drafts recalled 0.40–0.50 of the needed facts against 0.70–0.80 for the larger models
(`bench/README.md`, "A small Double Tap model and read-ahead").

| id | Upstream | Revision | File | Size (bytes) | SHA-256 | Licence |
|---|---|---|---|---|---|---|
| `qwen3-1.7b-q8` (signed) | [Qwen/Qwen3-1.7B-GGUF](https://huggingface.co/Qwen/Qwen3-1.7B-GGUF) | `90862c4b9d2787eaed51d12237eafdfe7c5f6077` | `Qwen3-1.7B-Q8_0.gguf` | 1,834,426,016 | `061b54daade076b5d3362dac252678d17da8c68f07560be70818cace6590cb1a` | Apache-2.0 |
| `qwen3-0.6b-q8` | [Qwen/Qwen3-0.6B-GGUF](https://huggingface.co/Qwen/Qwen3-0.6B-GGUF) | `23749fefcc72300e3a2ad315e1317431b06b590a` | `Qwen3-0.6B-Q8_0.gguf` | 639,446,688 | `9465e63a22add5354d9bb4b99e90117043c7124007664907259bd16d043bb031` | Apache-2.0 |

Sizes and SHA-256 come from the Hugging Face API for the pinned revisions (the LFS object of
each file); both repositories are published by the model's authors under Apache-2.0, the same
licence and family as the manifest's `qwen3-4b-q4km` and `qwen3-8b-q4km`, so the runtime's chat
template handling (thinking off) applies unchanged. Q8_0 rather than Q4_K_M: at this size the
quantisation error of 4-bit weights costs proportionally more quality, and the files stay small.

### Why `tiers` = triage, light, synthesis and `min_ram_gb` = 8

Signing these entries into the manifest must not change which model `nuggetd` picks for Chat on
any machine. `selected_models` (crates/nuggetd/src/local.rs) picks the Chat model with
`recommend` (crates/nugget-models/src/hardware.rs) over the triage tier, then synthesis, and
`recommend` takes, among the entries of a tier whose `min_ram_gb` fits the machine, the one with
the largest `(min_ram_gb, size)`. With `min_ram_gb = 8`, the same floor as `qwen3-4b-q4km`:

- below 8 GB nothing fits, as today (no local chat model is recommended);
- from 8 GB, the tie on `min_ram_gb` is broken by size, and `qwen3-4b-q4km` (2.5 GB) is larger
  than both (1.8 and 0.6 GB), so it stays the triage, light and synthesis pick; from 16 GB
  `qwen3-8b-q4km` stays the synthesis and agentic pick.

So `recommend` gives the same answer for every tier on every amount of RAM, and the
`recommended` flag of `models.list` and of the model-fit explanation (`nugget_models::explain`) is unchanged too (bench test
`pending_entries_do_not_change_recommendations`). Both models can honestly run on less than
8 GB; the floor is a recommendation threshold, not a requirement, and they are only meant to be
named explicitly in `[draft.local] model`. The tiers are the chat tiers the 4B model serves
(drafting routes by synthesis), so either model can also be named as `runtime.chat_model` for an
experiment; neither is ever chosen on its own.

## `multilingual-embed.json`

A multilingual embedder for GG7-2 (an English question about non-English mail is not found with
the English-only `nomic-embed-text-v1.5-q8`). Proposed as the **embedding default for new
stores**; measurements and the comparison with other candidates are in `bench/README.md`, "A
multilingual embedder (GG7-2)".

| id | Upstream | Revision | File | Size (bytes) | SHA-256 | Licence |
|---|---|---|---|---|---|---|
| `nomic-embed-text-v2-moe-q8` | [nomic-ai/nomic-embed-text-v2-moe-GGUF](https://huggingface.co/nomic-ai/nomic-embed-text-v2-moe-GGUF) | `ffbcf4c99e5d617dda10ec8c0e9f75754b0cbb80` | `nomic-embed-text-v2-moe.Q8_0.gguf` | 512,225,120 | `06e7a7e594a26985523c18383aba4aad39fe6e14f08ffc6ab5b554e1ccdc3cff` | Apache-2.0 |

Size and SHA-256 come from the Hugging Face API for the pinned revision (the file's LFS object),
and a download was checked against both. The repository is the model authors' own. The model
uses the same task prefixes as v1.5, so `EMBED_PREFIXES`' `nomic-embed-text` row covers it; it
needs a llama.cpp build with the `nomic-bert-moe` architecture, which the manifest's `b11940`
runtime has.

### Why signing it changes the embedding pick, and why that is safe

With `min_ram_gb = 4` and a larger size than v1.5, `recommend` picks it for the embedding tier on
every machine where v1.5 fits (bench test `pending_entries_do_not_change_recommendations` checks
that this is the only change; chat picks are unchanged). That pick only applies to stores without
vectors: `nuggetd` keeps a store that has vectors on the model that built them (recorded by the
store, or `nomic-embed-text-v1.5-q8` for older stores), because two models' vectors are not
comparable even at the same dimension. Bench runs keep using the smallest embedding model (v1.5)
unless `--local-embed-model` names another.
