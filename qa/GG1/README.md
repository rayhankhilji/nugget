# GG1: Double Tap QA (harness, scenarios, results)

Report: `docs/qa/GG1.md`. Everything here is synthetic and written for GG1.

- `harness/`: the `gg1` binary (its own Cargo workspace). `gg1 draft` runs scenarios through the
  real `nugget_draft::Drafter`, wired as `nuggetd` wires it (real `VoiceProfile` from the
  persona's sent messages, in-memory store with the persona's vault, prefix warmer, blocking
  retrieval, product `DraftConfig` defaults). `--local-models` serves real manifest models via
  nugget-runtime (as `nugget-bench`); without it a scripted provider streams each scenario's
  `fake_reply`. `gg1 judge` scores drafts with an 8B judge against the rubric in `main.rs`.
  `gg1 lint` prints each scenario's classification and gate.
- `world/*.json`: personas (`id`, `name`, `emails`, `voice`, `sent[]`, `vault[]`).
- `scenarios/*.json`: `{"scenarios": [{id, persona, area, description, request: DraftRequest,
  fake_reply?, config?: {include_sensitive, grounding, grounding_check, excluded_apps, allow_terminals}, expect}]}`.
- `check.py`: deterministic checks of `expect` (`refuse`, `kind`, `language`, `gold`,
  `forbidden`, `secrets`, `secrets_not_in_prompt`, `injection_markers`, `min_words`,
  `max_words`, `single_line`, `no_markdown`, `no_greeting`, `no_repeat_prefix`,
  `no_signoff_name`, `forbidden_regex`, `required_regex`, `allow_empty`), latency and judge
  summaries.
- `results/`: captured outputs of the runs cited in the report.

Run locally (scripted provider):

```sh
cd qa/GG1/harness && CARGO_INCREMENTAL=0 cargo build --release && cd ../../..
qa/GG1/harness/target/release/gg1 draft --out /tmp/d.jsonl && python3 qa/GG1/check.py /tmp/d.jsonl
```

Real models: `.github/workflows/qa-GG1.yml` (workflow_dispatch).
