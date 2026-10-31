# GG7: AI features in other languages

QA package GG7 (brief: `docs/waves/wave-gg/GG7.md`). Question: do search, Ask/Chat, Double Tap,
knowledge extraction and proactive cards work when the user's mail and notes are in the nine UI
languages (en, de, es, fr, hi, ja, pt-BR, zh-CN, ar), in mixed-language mailboxes and in
right-to-left text, and does the UI around AI output hold up in those locales?

- Branch `qa/multilingual`. Code under test: integration `fd101a3` (first real-model run) and
  `5a33258` (DD4's CJK search, second real-model run and every deterministic result below).
- Evidence: `qa/GG7/` (corpus, gold, harness, results, probes). Raw model transcripts are the
  workflow artifacts (links below); the trimmed, scored rows are in `qa/GG7/results/`.

## Verdict

<!-- VERDICT-TABLE -->

## How this was tested

- **Corpus** (`qa/GG7/corpus/`, spec in `SPEC.md`): 61 hand-written items, 6 per language plus a
  7-item mixed mailbox (German/English thread, Spanglish, Hinglish, Hindi/English, Arabic with
  French and English product names, Japanese with English tech terms, a Chinese `.ics`). Formal and
  informal mails, an invoice, a deadline mail, a note and a quoted reply thread per language, in
  local formats (`2.159,85 €`, `₹ 1,24,500`, `￥38,500`, `١٢٬٤٢٠ ر.س`, `11月13日（金）`), local
  quote headers, RFC 2047 subjects, quoted-printable, base64 and one ISO-2022-JP mail. Natural
  text written per language, not translated English. Everything is invented.
- **Gold** (`qa/GG7/gold/<lang>.json`): 82 search queries by kind (exact, inflected, compound part,
  no-diacritics/variant spelling, CJK word, name, semantic, cross-lingual), 43 Ask questions (3 in
  the language and 1 in English per language) with required facts and the file to cite, 33 Double
  Tap threads with the expected reply language and register markers, people/orgs/dates to extract,
  and the proactive situations a good assistant would surface.
- **Harness** (`qa/GG7/harness/gg7.py`): starts the shipped `nuggetd` with its own `config.toml`
  (no test hooks), installs the manifest's models over IPC (`models.install`), adds the corpus as a
  `files` source, then calls `search`, `retrieve.ask`, `draft.begin` (a Mail-like context with the
  thread as the mail client shows it), `knowledge.*`, `proactive.refresh`, `cards.list`,
  `brief.get`. `score.py` scores every row (language id by script and stopwords, register markers,
  fact alternatives after NFKC and Arabic-Indic digit folding, citation file, card kind).
- **Real models**: `.github/workflows/qa-GG7.yml` (dispatch only) on `ubuntu-latest` CPU runners,
  `qwen3-4b-q4km` and `qwen3-8b-q4km` as the chat model, `nomic-embed-text-v1.5-q8` (the manifest's
  only embedding model) for embeddings.
- **Deterministic layer** (no model): the same harness with `--no-models`, plus a probe crate over
  `nugget-proactive`'s text functions (`qa/GG7/explore/proactive/probe`) and desktop probes
  (`qa/GG7/explore/ui`: vitest renders of Today, the overlay and Chat in de/ja/ar, then Chromium
  layout with the app's CSS).
- **Two layers of subagents**: explore agents per area (proactive and knowledge; UI around AI
  output) wrote and ran scenarios; a fresh verification agent re-ran every finding from its written
  repro alone. Findings marked **Verified** reproduced there; nothing that failed to reproduce is in
  this report.
- Existing reports were checked first. Already-reported multilingual issues are referenced, not
  re-reported: GG1-2 (non-English password fields), GG1-8 (Japanese length), GG1-13, GG1-14,
  GG1-29 (greetings), GG1-31 (voice stats), GG1-34 (reply language never named). GG1-9 (CJK
  search) is **fixed** by DD4 and confirmed here.

## Findings

<!-- FINDINGS -->

## What works well

<!-- WORKS -->
