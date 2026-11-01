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

Per capability, worst language first. Models: `qwen3-4b-q4km` and `qwen3-8b-q4km` (chat),
`nomic-embed-text-v1.5-q8` (embeddings) on `ubuntu-latest` (4 vCPU EPYC 7763, 16 GB). Numbers are
run 5 (integration `44b9e11`) unless marked; run 1 (`fd101a3`) gave the same picture.

| Capability | Verdict | Evidence |
|---|---|---|
| Ask/Chat in the item's language | **Works** (all 9 + mixed) | Facts and citation right in 33/34 own-language questions with the 4B and with the 8B (de, es, fr, hi, ja, zh-CN, ar 3/3; pt-BR 2/3; en 4/4); answers in the question's language 40/43. |
| Ask/Chat across languages (English question, non-English mail) | **Broken** | 2/8 (4B) and 3/8 (8B) answered; de, hi, ja, zh-CN "I couldn't find that"; pt-BR answered from the wrong (English) mail. Same in run 1. GG7-2. |
| Double Tap: reply language and register | **Works** | Thread language 32/33 with both models (the miss: a German/English thread answered in English); register 30/33 (4B), 28/33 (8B). |
| Double Tap: writing Sam's reply, not the correspondent's | **Broken** outside English | Wrong side in 10/30 (4B) and 6/30 (8B) non-English drafts, 0/3 English; ja 3/3 (4B), ar 2/3. Run 1: 12/30. GG7-1. |
| Search, hybrid (with embeddings) | **Works with caveats** | hit@5 69/82 (84%): fr 8/8, ar/de/en/es/pt-BR/zh-CN 7/8, hi 5/8, ja 5/8. Width variants, Arabic clitics, compound parts and cross-lingual queries (5/12) miss. GG7-6, GG7-2. |
| Search, lexical (no model) | **Works with caveats** | 57/82; CJK words now found (DD4, GG1-9 fixed: 3/3, was 0/3); ja 3/8, ar 5/8, de 4/8. GG7-6. |
| Proactive "needs a reply" | **Broken** for de, ja, zh-CN, ar; caveats es, hi | Reply cards on gold mails: en, fr, pt-BR 3/3; es 2/2; hi 2/3; de, ja, zh-CN 1/3; ar 0/2. Same with and without a model. GG7-3, GG7-7, GG7-8. |
| Proactive deadlines, promises, renewals | **Broken** outside English | English twins get follow-up and renewal cards, German twins none; no commitment or deadline card in any language from the corpus. GG7-4. |
| Card, brief and action text | **Broken** (English in every UI language) | GG7-5, GG7-10, GG7-15. |
| Extraction of people | **Works with caveats** | Sender display names in every script (run 1, 4B: zh-CN 6/6, es 6/7, ja 4/5, ar 4/7, de 5/9); body-only names and honorifics miss; companies become people. GG7-16. <!-- KNOW --> |
| Extraction of dates | **Broken** | 1 of 14 extracted due dates normalised; non-English dates kept raw or turned into entities (`3 نوفمبر`, `下周五（11月6日）`). GG7-4. |
| Sensitivity marking (rules) | **Broken** outside English | 5/5 en; de, pt-BR, ja 0/5; es 2/5; fr, hi, zh-CN, ar 1/5; no false positives. <!-- SENS --> GG7-S. |
| UI around AI output (RTL, dates, numbers) | **Works with caveats** | `<html dir>` and Intl dates/relative times correct in all 9 locales; Arabic text starting with a Latin word laid out LTR; ISO dates in card titles; Arabic plurals 11–99. GG7-9, GG7-10, GG7-18. |

Per language, in one line each: **en** works (baseline). **fr, pt-BR, es** work for Ask, Double Tap
and search; proactive and sensitivity rules mostly miss. **de** like fr plus compound/umlaut search
misses and 1/3 reply cards. **hi** search 5/8, Hindi gender guessed. **ja** Ask works; Double Tap
wrong side in 3/3 (4B); width-variant search misses; 1/3 reply cards. **zh-CN** Ask and search
work since DD4; Double Tap 1/3 wrong side; 1/3 reply cards. **ar** Ask works; Double Tap 1–2/3
wrong side; RTL layout of Latin-led text; clitic search misses; 0/2 reply cards. **Mixed** mailbox:
Ask 6/7; a German/English thread is answered in English.

### Runs

- Run 1, `fd101a3`: https://github.com/rayhankhilji/nugget/actions/runs/41237753200 (4B all
  suites; 8B Ask only before the 340-minute limit: Ask and Double Tap competed with background
  extraction on 4 CPUs, median Ask 249 s with the 4B).
- Run 5, `44b9e11`: https://github.com/rayhankhilji/nugget/actions/runs/41267364723 (4B search,
  Ask, Double Tap; 8B Ask; 8B Double Tap, which drafted before triage finished and so had no
  retrieved context; the knowledge jobs extracted nothing because they ran before triage cleared
  the fail-closed items, a harness ordering issue fixed for run 7).
- <!-- RUN7 -->
- Scores and trimmed rows: `qa/GG7/results/` (`run1-fd101a3-4b`, `run5-44b9e11-*`, `lexical`).

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
