# GG8 area E: real models, the real internet and the desktop app on GitHub runners

Workflow `.github/workflows/qa-GG8.yml`, run [41117729320](https://github.com/rayhankhilji/nugget/actions/runs/41117729320)
at 931f1e8 (integration 3aeda23 plus GG8 files), ubuntu-latest (AMD EPYC 7763, 4 vCPU, 16 GB), release
build. Models from the signed manifest: `qwen3-1.7b-q8` (chat) and `nomic-embed-text-v1.5-q8`
(embedding), served by the managed `llama-server`. Summaries in `real/results/`.

| Scenario | What | Verdict |
|---|---|---|
| s60 onboarding (internet mode) | models.list offline; install chat + embedding; runtime download | Works: DNS saw exactly github.com, release-assets.githubusercontent.com, huggingface.co, us.aws.cdn.hf.co, each with `model_download`/`runtime_download` rows (6 rows, 2.0 GB received). First chat right after install failed (`no provider for tier Synthesis has the required capabilities`), a functional issue for GG6 |
| s61 local session with real llama-server, 300 s idle | files, search, Ask, chat, read-ahead + draft, knowledge, proactive, MCP | Works: no DNS query, no new ledger row, no IPv4 packet left the namespace (the only packets were the kernel's own IPv6 router solicitations / MLD reports on the veth, which the check counted). llama-server served over a Unix socket (`--host .../chat.sock`, `--offline`) |
| s62 local models + cloud provider (sinkhole) | read-ahead local and cloud, draft, Ask local-first, follow-up, fallback | Read-ahead reached only the local server, and was `skipped: "cloud"` when the draft would go to the cloud; the cloud draft excluded 3 sensitive items; every cloud request ledgered. **Failed: the follow-up Ask sent the local model's earlier answer, which quoted the card number, to the cloud as history** (GG8-1) |
| s65 desktop app (Tauri, sinkhole) | 150 s with default settings, 150 s with the update channel `off` | With defaults the app GETs `github.com/rayhankhilji/nugget/releases/download/updater-feed/stable.json`, unledgered (GG8-3). Channel `off`: silent |
| macOS (pktap) | onboarding + local session | **Not covered**: the job ran 45 s and uploaded an empty artifact; the capture script failed and was not iterated within the shared usage limit |

The desktop job's artifact upload failed (`EACCES` on root-owned logs); its results are in the job
log's GG8-SUMMARY block.
