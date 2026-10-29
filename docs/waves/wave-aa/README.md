# Wave AA: real-model quality

Starts from 50e9b75. Y2's first real-model numbers (bench/README.md, "Real-model results") missed
the local Double Tap latency, computer-use success, drafting hallucination and extraction targets on
a CPU runner. This wave closes those gaps and measures again, on Linux CPU and Apple Silicon.

| WP | Branch | Package |
|---|---|---|
| AA1 | `aa/doubletap-latency` | local Double Tap under 3 seconds |
| AA2 | `aa/agent-quality` | computer use that works with small local models |
| AA3 | `aa/grounding` | grounded drafts and better extraction with real models |
