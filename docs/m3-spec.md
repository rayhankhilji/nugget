# M3: Double Tap (drafting in your voice) spec

**Goal:** press a hotkey in any text field, in any app, on any OS, and get a reply in **your voice**, grounded in **cited
context**, inserted in place, and verified.

**Exit criteria** (PLAN.md §5):
- `bench draft`: p50 time to first inserted token **under 1.2 s with a cloud provider** and **under 3 s local** on a 16 GB machine with a 4B-class model.
- The drafting eval **beats the screenshot-plus-whole-vault baseline**, the Sentient-style approach, on voice match and factuality.
- Insertion is verified by reading the field back on all three OSes, with a fallback that keeps every clipboard type.
- CI is green.

**What we're beating** (`docs/research/sentient-os-teardown.md` §2.7):
- Sentient fires on a right-⌘ double-tap only, which excludes non-Apple keyboards (issue #288).
- It sends a screenshot plus the **entire vault** (up to 600 KB, silently truncated) through their relay by default.
- The model decides "is this a reply box" by returning a sentinel string.
- It pastes with the clipboard and Cmd-V, losing non-text clipboard formats and never checking delivery.
- Its voice comes from raw sample messages only.

## Architecture
```
desktop app (has the OS permissions)                       nuggetd
┌──────────────────────────────────────────┐   IPC    ┌────────────────────────────────────────┐
│ nugget-hotkey: configurable trigger      │ ───────▶ │ draft.begin(DraftContext)              │
│ nugget-a11y: read focused field + thread │          │  nugget-draft: classify field (rules)  │
│   (AX / UIA / AT-SPI), caret bounds      │          │   + nugget-voice profile & exemplars   │
│ overlay: preview near caret, accept/edit │ ◀─────── │   + retrieve.context (budgeted, cited) │
│ insert at selection → read back → verify │  stream  │   + Tier::Synthesis (local or cloud)   │
│   fallback: clipboard with full restore  │          │ draft.delta / draft.done               │
└──────────────────────────────────────────┘          └────────────────────────────────────────┘
```
The trigger, the accessibility read and the insertion live in the **desktop app**, because OS permissions (macOS
Accessibility, Input Monitoring) are granted to the app bundle. Drafting lives in the **daemon**.

## Work packages, wave 1 (parallel)
| WP | Crate(s) | Summary |
|---|---|---|
| **M3-A** | `nugget-a11y` | Cross-platform **read**: focused app, window, element role, value, selection, caret rect, and nearby thread text (walking the message list around the field). **Write**: insert at selection, then read back to verify. Backends: macOS AX (scoped `unsafe` FFI), Windows UI Automation, Linux AT-SPI (`atspi`/zbus). Includes a permission probe. M4 extends this crate for computer use |
| **M3-B** | `apps/desktop` (+ `nugget-hotkey`) | A configurable **global trigger**: double-tap of any modifier, or a chord, through platform-appropriate APIs (no keylogging; only the trigger key is observed). A **caret overlay** that streams the preview with accept (Enter), edit and cancel (Esc). Insertion flow with read-back verification, and a clipboard fallback that saves and restores **every** pasteboard type. Permission onboarding for each OS |
| **M3-C** | `nugget-draft` | Daemon-side engine. **Rule-based field classification** from role, app and field metadata (email body, chat composer, comment, search box…); no model sentinel. A prompt built from the voice profile, recipient and channel exemplars, the thread, and `retrieve.context` sized to the budget with citations. Streaming generation with post-processing (strip duplicate greetings and signatures, match the expected length). Cancellation. A cloud/local choice per app, with the sensitive-content gate |
| **M3-D** | `nugget-voice` | A **voice profile** built from the user's own sent messages (sent mail, chat messages with from-me flags) and refreshed incrementally. Stylometric stats per channel and recipient (length, greetings and sign-offs, formality, emoji and punctuation habits, language), plus exemplar selection by retrieval (similar recipient and topic) |
| **M3-E** | `nugget-infer` latency work | Prompt caching (Anthropic `cache_control`, OpenAI prompt caching), connection prewarm and keep-alive (HTTP/2), first-token timing in metrics, and a speculative early start on trigger press. For the local runtime: keep it warm, cache the prompt prefix (`cache_prompt`), and set slot affinity |
| **M3-F** | `bench` | The `draft` suite: held-out threads from synthetic personas, with known facts and voice. Metrics: voice similarity (stylometric distance plus pairwise LLM judge, optional), factuality against cited context, hallucination rate, and latency (first token and total) with fake and real providers. **Baseline arm:** screenshot-only context plus the whole-vault dump, to quantify the improvement over Sentient's approach |

## Wave 2
- **M3-G integration:** `draft.*` IPC (`draft.begin`, `draft.cancel`, `draft.delta`/`draft.done` notifications, `voice.status`), daemon wiring, the desktop end-to-end flow, and a settings page (trigger, provider per app, excluded apps such as password managers and banking, enabled by default).
- **Review:** security reviewers (field contents treated as untrusted prompt input, excluded apps, never reading password or secure fields, clipboard restore) and correctness reviewers, then fixes.

## Execution (as run)
- **Contracts on the main branch:**
  - `nugget_core::ipc` gains `draft.begin/cancel/feedback`, `voice.status`, `DraftContext`, `AppInfo`, `FieldInfo` and `ThreadMessage`.
  - Contract crates: `nugget-a11y` (`Accessibility`, `Focused`, `insert` with verification), `nugget-hotkey` (`Trigger`, `start`), `nugget-voice` (`VoiceHints`, `StyleStats`) and `nugget-draft` (`FieldKind`, `classify`).
- **Wave 1 runs as parallel cloud sessions** while M2 runs locally:
  - Each work package has its own branch, `m3/<wp>`, and owns only its crate.
  - The orchestrator merges the branches, regenerates `Cargo.lock` and verifies the workspace.
