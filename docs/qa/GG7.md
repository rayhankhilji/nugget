# GG7: AI features in other languages

QA package GG7 (brief: `docs/waves/wave-gg/GG7.md`). Question: do search, Ask/Chat, Double Tap,
knowledge extraction and proactive cards work when the user's mail and notes are in the nine UI
languages (en, de, es, fr, hi, ja, pt-BR, zh-CN, ar), in mixed-language mailboxes and in
right-to-left text, and does the UI around AI output hold up in those locales?

- Branch `qa/multilingual`. Code under test: integration `fd101a3` (run 1), `5a33258` (DD4's CJK
  search; lexical results), `44b9e11` (run 5 and the sensitivity and verification runs), merged up
  to `c88e43f` at the end.
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
| Search, hybrid (with embeddings) | **Works with caveats** | hit@5 69/82 (84%): fr 8/8, ar/de/en/es/pt-BR/zh-CN 7/8, hi 5/8, ja 5/8. Width variants, Arabic clitics, compound parts and cross-lingual queries (5/12) miss. GG7-7, GG7-2. |
| Search, lexical (no model) | **Works with caveats** | 57/82; CJK words now found (DD4, GG1-9 fixed: 3/3, was 0/3); ja 3/8, ar 5/8, de 4/8. GG7-7. |
| Proactive "needs a reply" | **Broken** for de, ja, zh-CN, ar; caveats es, hi | Reply cards on gold mails: en, fr, pt-BR 3/3; es 2/2; hi 2/3; de, ja, zh-CN 1/3; ar 0/2. Same with and without a model. GG7-4, GG7-8, GG7-9. |
| Proactive deadlines, promises, renewals | **Broken** outside English | English twins get follow-up and renewal cards, German twins none; no commitment or deadline card in any language from the corpus. GG7-5. |
| Card, brief and action text | **Broken** (English in every UI language) | GG7-6, GG7-11, GG7-16. |
| Extraction of people | **Works with caveats** | Sender display names in every script (run 1, 4B: zh-CN 6/6, es 6/7, ja 4/5, ar 4/7, de 5/9); body-only names and honorifics miss; companies become people. GG7-17. |
| Extraction of dates | **Broken** | 1 of 14 extracted due dates normalised; non-English dates kept raw or turned into entities (`3 نوفمبر`, `下周五（11月6日）`). GG7-5. |
| Sensitivity marking (rules) | **Broken** outside English | 5/5 en; de, pt-BR, ja 0/5; es 2/5; fr, hi, zh-CN, ar 1/5; no false positives. The local triage model, which decides everything the rules do not flag, was not measured per language (Actions unavailable, see Runs). GG7-3. |
| UI around AI output (RTL, dates, numbers) | **Works with caveats** | `<html dir>` and Intl dates/relative times correct in all 9 locales; Arabic text starting with a Latin word laid out LTR; ISO dates in card titles; Arabic plurals 11–99. GG7-10, GG7-11, GG7-19. |

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
- Run 7 (`c88e43f`, harness waiting for triage; adds `4b-sens`/`8b-sens`, which run
  `qa/GG7/scenarios/sensitive/run.py --chat-model …` to measure the triage model's verdicts per
  language, and knowledge jobs that wait for a post-triage extraction): every job in the
  repository, this one included, failed at start from 07:58 UTC on 2026-11-01 (no runner
  assigned, no log), so these were not measured. The jobs are ready in `qa-GG7.yml`
  (`workflow_dispatch`).
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

Conventions: `H=qa/GG7/harness`, the base run is
`S=/tmp/g; python3 $H/gg7.py run --nuggetd target/debug/nuggetd --home $S/h --out $S/o
--no-models --suites search,knowledge,proactive && python3 $H/score.py $S/o` (use a short
`--home`). Real-model rows come from the workflow runs listed under "Runs".

### GG7-1 · High · Double Tap writes the correspondent's mail instead of the user's reply, in other languages · Verified

- **Repro**: workflow `qa-GG7.yml` job `4b-gen` (or harness `run --suites draft --chat-model
  qwen3-4b-q4km`); `draft.begin` with a Mail context whose thread is the incoming mail
  (`from_me: false`, `author` = the sender), e.g. `qa/GG7/corpus/zh-CN/01-qinghe-huimian-yaoqing.eml`
  with instruction `接受邀请，但希望改到下午两点`. `score.py` marks a draft *wrong side* when its
  opening line greets the user (Sam / سام), it is signed with the sender's name, or more than 70%
  of it repeats the incoming mail.
- **Expected**: Sam's reply to the sender, in the thread's language.
- **Actual** (run 1, 4B): 
  - `zh-CN-d1`: `尊敬的 Sam Okafor： 您好！感谢您的回复。关于11月19日的面谈安排，我这边可以调整到下午两点。… 陈婉清 / 市场部 商务合作经理` (written to Sam, signed by the sender).
  - `es-d1`: `Estimado/a Sam Okafor: Agradezco mucho su mensaje y confirmo la reunión … en nuestras oficinas`.
  - `ja-d2`, `zh-CN-d2`, `ar-d2`, `mixed-d6`: the incoming mail repeated almost verbatim (6-gram overlap 0.73–1.0), e.g. `هلا والله يا سام، شخبارك؟ …`.
  - `ja-d1` starts with the instruction itself (`お受けする。ただし開始を15時にしてほしいと伝える。`) and is signed `森田 由美子`.
  - `ar-d1` pulls the order `ORD-2026-5531` from a different sender's mail (`mixed/05`) into the reply.
- **How often**: run 1 (4B) 12/30 non-English drafts, 0/3 English (score.py). Run 5 (4B,
  `44b9e11`): 10/30 by score.py; the verification agent, judging all 66 run-5 drafts by hand
  against their mails, found 14/30 (4B) and 6/30 (8B) non-English drafts on the wrong side and
  0/3 English for both (fr-d2 ends `Bisous, Camille.`; hi-d3 `हाँ, धन्यवाद, सम जी … सुधीर त्रिपाठी।`;
  8B mixed-d5 is signed with another mail's sender). Worst: ja (4B 3/3), ar, zh-CN. English replies
  to non-English mail were on the right side. The 8B also produced one degenerate draft
  (`हम आपके आवेदन के बारे में बहुत खुश हैं।` nine times, mixed-d4). Only 3 drafts per model are
  English, so the English baseline is thin; GG1's English Double Tap scenarios did not show this.
- **Owner**: `crates/nugget-draft/src/prompt.rs` (`user_text`: "Write the … now, as the user"; the
  user is never named and the reply language is never stated), EE2 follow-up; related to GG1-34.
- **Fix direction**: name the user and the person being answered ("You are Sam Okafor. Write Sam's
  reply to Chen Wanqing's last message"), state the reply language (GG1-34), put the instruction
  in a role-neutral form ("Sam wants to: …"), and reject a draft that greets the user, is signed by
  the other author or repeats most of the last message (post-check in `post.rs`), retrying once.

### GG7-2 · High · Asking in English about mail in another language fails: "I couldn't find that" · Verified

- **Repro**: `retrieve.ask` with the English question of each language's gold (`question_lang:
  "en"`), e.g. `How much is the exhibition booth construction quote, and by when must the design be
  confirmed?` about `zh-CN/06-rongchuang-zhanwei-re.eml`, `--chat-model qwen3-4b-q4km` or
  `qwen3-8b-q4km`, embeddings `nomic-embed-text-v1.5-q8`.
- **Expected**: the answer from the Chinese mail (`48,600`, `11月10日`) with its citation.
- **Actual**: `I couldn't find that in your items.` for de, hi, ja, zh-CN with both models; the
  citations are English items (`04-q4-forecast-deadline.eml`, `05-thanksgiving-plan.md`). For
  pt-BR the 4B answered from the English offsite mail: `The year-end party is on Thursday, November
  19 [2]. The deposit is $500 [2]` (gold: 27 November, R$ 1.500,00, 6/11). Questions asked in the
  item's own language pass (31/35 4B facts).
- **How often**: 5 of 8 English questions about non-English mail fail with the 4B and with the 8B
  (run 1, and again in run 5 on `44b9e11`: the same 5 with both models; in all 5 the gold mail is not among the 10 citations, while own-language questions cite it 24/24 per model). Search: `cross_lingual` queries hit@5 4/12 (lexical and hybrid alike).
- **Owner**: the manifest's only embedding model is `nomic-embed-text-v1.5-q8`
  (`manifests/default.json`), an English model; `crates/nugget-retrieve` (hybrid ranking), DD4
  follow-up; the model choice is the models/manifest owner's.
- **Fix direction**: ship a multilingual embedder as the default (e.g. a multilingual E5/BGE-M3
  class model; the manifest already carries `bge-reranker-v2-m3`, which is multilingual, as a
  reranker), re-embed on change; until then, translate the query into the languages present in
  the index with the local chat model (the follow-up rewrite step already calls it) and search
  each.

### GG7-3 · High · Sensitivity rules miss 37 of 40 clearly sensitive non-English items · Verified

- **Repro**: `python3 qa/GG7/scenarios/sensitive/run.py --home /tmp/gg7s` (63 natively
  written mails, `qa/GG7/scenarios/sensitive/items.py`: per language a lab result with a diagnosis,
  a therapy session, a payslip, a bank statement or loan, a one-time code, and two ordinary mails;
  the runner sets `[ingest] assume_unflagged_safe = true` to measure the rule layer and reads
  `sensitive` from `search.faceted`). `--default` measures the shipped fail-closed default.
- **Expected**: about the English result in every language.
- **Actual** (sensitive situations marked /5; ordinary mails wrongly marked /2):
  en 5/5 · 0/2; de 0/5; es 2/5; fr 1/5; pt-BR 0/5; hi 1/5; ja 0/5; zh-CN 1/5; ar 1/5; ordinary 0/2
  everywhere; the 61-item GG7 corpus 0/61. The hi, zh-CN and ar hits are only the payslip from an
  `hr@` sender. English is caught by `hba1c`/`diabetes` (:38, :46), `anxiety` (:48), `salary` (:66),
  `loan` (:74), `verification code` (:127); their counterparts miss: `Befund … Schilddrüsenunterfunktion`,
  `hypothyroïdie`, `मधुमेह`, `糖尿病と診断`, `确诊为2型糖尿病`, `تشخيص السكري`; `Psychotherapeutin`,
  `psychologue … séance`, `psicóloga … sesión`, `カウンセリング`, `心理咨询`, `أخصائية نفسية`;
  `Bruttogehalt`/`Gehaltsabrechnung` (`gehalt` matches whole tokens only), `salário` (≠ `salario`),
  `給与明細` from `jinji@` (role mailboxes are English: `hr`, :144); `Kontoauszug`, `Darlehen`,
  `préstamo`, `prêt immobilier`, `empréstimo`, `गृह ऋण`, `住宅ローン`, `个人贷款`, `القرض`;
  `Bestätigungscode`, `código de verificación`, `code de vérification`, `सत्यापन कोड`,
  `ワンタイムパスワード`, `验证码`, `رمز التحقق`.
- **Why it matters**: without a model every unjudged item stays sensitive (fail-closed,
  `nugget-ingest/src/pipeline.rs:526-556`), which is safe. But once the local triage model runs,
  only a rule hit is sticky (`triage.rs:683-700`); everything else gets the verdict of the
  English-only `TRIAGE_SYSTEM` prompt (`ai.rs:101`), and with `assume_unflagged_safe` the rules are
  all there is. A German lab result or a Japanese one-time code then reaches cloud Ask and Double
  Tap context like any mail. (The model part was not measured per language here.)
- **CJK can never match** (verified by test): `tokens()` (:213-220) splits only on
  non-alphanumeric characters, so a CJK run up to the next punctuation is one token. Through the
  same daemon path: `ご本人確認のためverification code：482913をご入力ください` → not sensitive, with
  spaces around `verification code` → sensitive; `HbA1cは7.2%でした` → not, `HbA1c 7.2% でした` →
  sensitive; `您的OTP为482913` → not, `您的 OTP 为482913` → sensitive. So adding Japanese or Chinese
  words to the lists would not help by itself.
- **Owner**: `crates/nugget-ingest/src/sensitivity.rs` (`WORDS`, `PHRASES`, `ROLE_MAILBOXES`,
  `tokens` :213), HH2.
- **Fix direction**: native lists for the nine languages per category, diacritic folding before
  matching, German compound parts (prefix/suffix match for listed stems), substring matching for
  CJK terms, localised role mailboxes (`personal`, `personalabteilung`, `rrhh`, `rh`, `jinji`,
  `renshi`), and a multilingual triage prompt (or say "the item may be in any language" with
  examples).

### GG7-4 · High · "Needs a reply" only recognises English requests (or a question mark) · Verified

- **Repro**: base run; or the probe crate `qa/GG7/explore/proactive/probe` (copy the workspace
  `Cargo.lock` in, `CARGO_TARGET_DIR=target cargo build --offline --manifest-path
  qa/GG7/explore/proactive/probe/Cargo.toml`, `target/debug/gg7-proactive-probe 2026-10-28 <
  body.txt`); twins in `qa/GG7/scenarios/twins/`.
- **Expected**: a polite request without `?` asks for a reply in every language.
- **Actual**: `asks_something=false` for `Bitte teilen Sie mir mit, ob Ihnen der Termin passt.`,
  `Por favor, confírmame la dirección antes del miércoles.`, `Merci de me confirmer d'ici
  vendredi.`, `Pode me confirmar até sexta.`, `कृपया बताइए कि आपको कवर वाला विकल्प चाहिए या नहीं।`,
  `ご検討のほどよろしくお願い申し上げます。`; `true` for `Please send me the signed form by Monday,
  November 2.`. Daemon: the English twin gets `unanswered_thread | Reply to Lena Park: Signed form`,
  the German twin (`Bitte schicken Sie mir das unterschriebene Formular …`) no card.
- **How often**: gold "reply needed" mails with a reply card: en 3/3, fr 3/3, pt-BR 3/3, es 2/2,
  hi 2/3, de 1/3, ja 1/3, zh-CN 1/3, ar 0/2 (identical with the 4B; the generator uses no model).
  13 of the 17 language-caused misses are this rule.
- **Owner**: `crates/nugget-proactive/src/text.rs` (`ASKS`, the `please` rule), BB2 follow-up.
- **Fix direction**: per-language request markers (bitte, por favor, merci de, pode, कृपया,
  ください/いただけ…/お願い, 请/麻烦, يرجى/نرجو/أرجو) and, when a local model is up, the triage
  classifier.

### GG7-5 · High · Deadlines, promises and renewals are only understood in English: no cards, raw dates in the graph · Verified (rules)

- **Repro**: twins `qa/GG7/scenarios/twins/` with `--suites proactive`; probe `find_dates`.
- **Actual**: English twins give `follow_up | You told Mia Chen: "I'll send you the revised slides
  by Monday, November 2."` and `renewal | Pixelboard renews 2026-11-03 — $12.99`; the German twins
  (`Ich schicke dir die überarbeiteten Folien bis Montag, 2. November.`, `Zahlung erhalten: 12,99
  €.`, `Ihr Abonnement verlängert sich am 3. November 2026.`) give none. `find_dates` returns `[]`
  for `bis 3. Dezember`, `13.11.2026`, `11月13日`, `el 13 de noviembre`, `le 13 novembre`,
  `١٣ نوفمبر`, `13 नवंबर` (`on November 13` → `[2026-11-13]`).
- **With the 4B extractor** (run 1): 14 `due_on` claims, 1 normalised (`2026-11-16`, from German
  `16. November 2026`); the rest stay raw (`18 نوفمبر 2026`, `7/11`, `jeudi 19 novembre, même
  heure`, `11月19日（周四）上午 10:00`). Dates become *event entities*: `3 نوفمبر`, `15 نوفمبر`,
  `الأربعاء`, `الثلاثاء`, `نوفمبر 2026`, `11月9日（周一）前`, `下周五（11月6日）`. No commitment or
  deadline card appeared for any language, so the gold invoice/deadline situations (03, 04 in
  every language) are never surfaced.
- **Owner**: `crates/nugget-proactive/src/text.rs` (`MONTHS`, `WEEKDAYS`, `PROMISES`,
  `deadline_in`, `relative_days`), `generators.rs` (`RENEWAL_WORDS`, `RECEIPT_WORDS`), BB2
  follow-up; `crates/nugget-knowledge/src/llm.rs` (`normalize_date`, English month table), EE1
  follow-up.
- **Fix direction**: one shared multilingual date parser (month/weekday names for the nine
  languages, `dd.mm.yyyy`, `dd/mm`, `M月D日`, `D de mes`, Arabic-Indic digits, relative words) used
  by proactive, knowledge and retrieval (`nugget-retrieve/src/dates.rs` has a third English-only
  table); ask the extractor for ISO dates and drop date-only entity names.

### GG7-6 · High · Card, brief and action text is English in every UI language · Verified

- **Repro**: base run (`out/proactive.jsonl`) and the UI probe: `cd apps/desktop && pnpm install
  --frozen-lockfile && npx vitest run --config ../../qa/GG7/explore/ui/vitest.config.mjs`.
- **Actual**: all 23 cards are `Reply to <name>: <subject>` with `… wrote N days ago and hasn't
  heard back.`; the German UI renders `["Reply to محمد العلي: عرض السعر 2026","You asked 3 days ago
  and haven't heard back.","Fällig übermorgen","Pixelboard renews 2026-11-03 — $49.00"]` and the
  confirmation `Bitte bestätigen: this sends on your behalf`; brief summary `डॉ. अनुराधा क़ुरैशी
  wrote 5 days ago and hasn't heard back.`. Only kind badges and action verbs are translated.
- **Owner**: `crates/nugget-proactive/src/generators.rs` (titles and details at :196–198, :437,
  :877, :999, :1068, :1096), `actions.rs:225-287`, BB2 follow-up; `apps/desktop/src/pages/Today.tsx`
  (:323, :415, :430), `proactiveNotifications.ts:31`, Z2/W4 follow-up. Not GG4-20 (raw RFC 3339 in
  action labels only).
- **Fix direction**: the daemon sends the kind and structured fields (who, subject, days, due,
  amount) and the app formats them with ICU messages per kind; keep `title` as a fallback.

### GG7-7 · Medium · Search misses everyday spelling variants outside English · Verified

- **Repro**: base run, then with the daemon up on the same home: `cd qa/GG7/harness && python3
  probe_search.py $S/h/data/ipc/nuggetd.sock` (output: `qa/GG7/results/lexical/probe_search-5a33258.txt`).
- **Actual** (lexical, 5a33258), 0 hits each, while the written form is found:
  - Japanese width: `LED照明` (mail: `ＬＥＤ照明`), `ｷｬﾝﾍﾟｰﾝ` (mail: `キャンペーン`); Chinese
    Traditional `紅燒肉` for `红烧肉`.
  - Arabic: `الكراسي` and `كراسي` for `والكراسي` (attached و), `فاتوره` for `فاتورة` (ة/ه).
  - German: `Koenigstrasse`, `Konigstrasse` for `Königstraße`; `Ausschuss` in
    `Lenkungsausschuss`. Hindi `गाड़ी` in `रेलगाड़ी`; Spanish `sofreír` in `sofreírlo`.
  - With embeddings (run 1, 4B) the hybrid search recovers some: hit@5 overall 67/82 (81%) against
    57/82 lexical, but the variant/inflected kinds stay at 11/14 and the Arabic, Japanese and
    Hindi variant queries still miss.
- **Owner**: `crates/nugget-store/src/fts.rs`, `schema.rs` (`porter unicode61 remove_diacritics
  2`; the DD4 CJK bigram index), DD4 follow-up.
- **Fix direction**: NFKC-normalise text and queries before both indexes (width), fold ß→ss and
  ä/ö/ü→ae/oe/ue as an extra form for German, Arabic light normalisation (strip tashkeel/tatweel,
  unify أإآ→ا and ة→ه, ى→ي) plus proclitic stripping (و، ف، ب، ل، ال) at index and query time;
  prefix queries (`Ausschuss*` does not help a compound tail, so consider trigram fallback for
  Latin when the unicode61 query has no hits).

### GG7-8 · Medium · Arabic `؟` and CJK/Devanagari sentence ends are not recognised · Verified

- **Repro**: probe crate. `asks_something=false` for `هل يناسبك الموعد؟`, `你那个周末有空吗？我得在11月6日之前订房。`,
  `週末は空いていますか？よろしくお願いします。`.
- **Cause**: `sentence_asks` knows `?`, `？` and a leading `¿` only; `sentences` splits on `. ! ?`
  followed by whitespace, so `؟ ？ 。 ।` inside a line never end a sentence.
- **How often**: zh-CN/02, ar/02, mixed/06 (3 gold mails). **Owner**: `nugget-proactive/src/text.rs`,
  BB2 follow-up. **Fix**: treat `؟ ？ ！ 。 ।` as sentence ends without whitespace, `؟` as a question,
  Japanese `か。` / `でしょうか` as a question.

### GG7-9 · Medium · Localised reply headers are not stripped, so an old quoted question raises a new card · Verified

- **Repro**: probe crate on `Danke, erledigt.\nAm 27.10.2026 um 16:05 schrieb Paul Weber
  <p@x.example>:\nKönnen Sie mir das Formular schicken?` → `own_text: 3/3 lines kept;
  asks_something=true`; English twin (`On Tue, 27 Oct 2026 at 16:05, … wrote:`) → `1/3 lines kept;
  asks_something=false`. Same for Outlook `Von:/Gesendet:` blocks and `-----Ursprüngliche
  Nachricht-----`.
- **Owner**: `own_text` in `nugget-proactive/src/text.rs` (also feeds promises, renewals and card
  quotes), BB2 follow-up. **Fix**: localised "wrote" patterns (schrieb, a écrit, escribió, escreveu,
  ने लिखा, 写道, كتب, `年…日 … :`) and localised Outlook header blocks.

### GG7-10 · Medium · Arabic drafts and answers that start with a Latin word are laid out left-to-right · Verified

- **Repro**: UI probe, then `cd apps/desktop && node ../../qa/GG7/explore/ui/bidi.mjs` (Chromium,
  app CSS, `<html dir=rtl lang=ar>`).
- **Actual**: overlay draft `iPhone 15 سيصل يوم الخميس، والفاتورة ١٢٠ دينار.` → `dir=ltr`, left
  aligned, full stop at the right edge; Chat answer `**Nugget Pro** هو الاشتراك الحالي
  [1].\n\nيتجدد في 3 نوفمبر …` → the second, pure-Arabic paragraph is also `ltr`. Real drafts start
  this way (run 1: `ORD-2026-5531`, `Maison Clairière`, `Excel` inside Arabic replies).
- **Cause**: one `dir="auto"` on the whole container decides by the first strong character
  (`Chat.tsx:799`, `Search.tsx:390`, `overlay/Overlay.tsx:141,149`); `Markdown.tsx:495` gives
  `<p>`/`<li>` no `dir`. **Owner**: Z2 follow-up (and II3 for the overlay). **Fix**: `dir="auto"`
  per block; for drafts decide by the majority script or the thread language.

### GG7-11 · Medium · ISO dates inside card and brief titles are not localised · Verified

- `Today.tsx:322,414,424` render title/detail raw: `renews 2026-11-03`, `Due 2026-11-03: …` in every
  locale, though `formatDatesInText` (used by `knowledge.ts:65`) gives `03.11.2026`, `2026/11/03`,
  `03‏/11‏/2026`. Amounts are daemon-formatted (`$49.00`). Owner: Today page (W4/Z2 follow-up).
  Superseded by the structured fields of GG7-6, but a one-line fix today.

### GG7-12 · Low · English words leak into 4B drafts in other languages · Verified (rare)

- **Actual** (run 1, 4B): `Für die Anreise ist alles booked.` (de-d1), `J'aimerais également savoir
  si there is a parking for visitors` (fr-d1), `O Dudu vai estar there` (pt-BR-d2), `تأجيل الموعد
  إلى الساعة Twelve ظهرًا` (ar-d1), Ask `invoice का अमOUNT ₹ 2,45,600` (mixed-a3).
- Run 5 (verification agent): 4B drafts `Wir können den Termin indeed.` (de-d1), `الساعة Twelve ظهرًا`
  (ar-d1) and an English placeholder `[Your Name]` in a Spanish draft (es-d3); none with the 8B.
  Rare, 4B only.
- **Owner**: drafting prompt and post-processing, EE2 follow-up (`nugget-draft/src/post.rs`).
- **Fix direction**: name the output language (GG1-34); flag drafts whose non-name Latin tokens
  are English stopwords in a non-English reply and retry once.

### GG7-13 · Low · Drafts assume the user is male · Verified

- Run 5: `Estoy encantado` (es, 4B), `acepto encantado`, `atento`, `je suis ravi`, `Vou acompanhado`,
  `estou animado`, `कर सकता हूं` (8B); one hedge `Je serai présent(e)`. Run 1's 4B once used the
  feminine `स्वीकार करती हूं` (hi-d1) and the masculine `पूछना चाहता हूं` (hi-d2) for the same user.
  Arabic and some Hindi drafts sidestep it with the plural. Gendered first-person forms are needed in
  hi, ar, es, fr, pt-BR; there is no setting, and the voice profile does not learn it.
- **Fix**: a "how should drafts refer to you" setting (or learn it from sent mail) passed to the
  prompt; until then prefer gender-neutral constructions.

### GG7-14 · Low · Spanish and French "mar"/"mar." (Tuesday) read as March · Verified

- `find_dates("Le mar. 3 nov.")` → `[2027-03-03]`; the es/06 and fr/06 quote headers give
  `2027-03-27`. `month_of` accepts any ≤4-letter prefix. Owner `nugget-proactive/src/text.rs`.

### GG7-15 · Low · Local money formats are not parsed · Verified

- `find_amounts` returns nothing for `2.159,85 €`, `R$ 4.704,00`, `￥38,500`, `١٢٬٤٢٠ ر.س`
  (`₹ 1,24,500` → `₹124500.00`). Space before a trailing symbol, BRL/SAR/CNY, full-width `￥`
  unknown. Owner `nugget-proactive/src/text.rs` (`SYMBOLS`, `CODES`).

### GG7-16 · Low · The card quote is the greeting line for non-English mail · Verified

- `lead()` (`generators.rs:87-94`) skips only `hi`, `hello`, `dear`: card quotes are `Hallo Sam,`,
  `Bonjour,`, `Estimado/a Sam Okafor:`, `尊敬的 Sam Okafor：`, `आदरणीय Sam Okafor जी,` (every
  non-English card; in English `Hey Sam,` once). Use the multilingual greeting lists from GG1-29.

### GG7-17 · Low · Company senders become people · Verified

- Person entities `Sanitär Wöhrle GmbH`, `Reformas Peñalver S.L.`, `株式会社さくら印刷 経理部`,
  `姑苏雅苑酒店` (senders `rechnung@`, `administracion@`, `keiri@`, `booking@`); English invoices from
  `billing@` are skipped (`is_automated`, `nugget-knowledge/src/text.rs:323`, English role
  addresses only). Deterministic names also keep honorifics (`Dr. Annette Brückner`,
  `डॉ. अनुराधा क़ुरैशी`). Owner `nugget-knowledge/src/extract/mail.rs:132`, EE1 follow-up. Fix:
  legal-form cues (GmbH, S.L., S.A., Ltda., 株式会社, 有限公司, شركة, مؤسسة) and localised role
  local-parts.

### GG7-18 · Low · Text cuts split graphemes; an emoji at the reminder cut-off yields a lone surrogate · Verified (cut), inferred (lost notification)

- `truncateText(hi, 10)` → `प्रियंकाप्…` (dangling virama), ZWJ emoji → `👩‍…`
  (`knowledge.ts:114`, `GraphCanvas.tsx:93`). `proactiveNotifications.ts:31` `.slice(0, 300)` leaves
  `lastCodeUnit: d83d wellFormed=false` when position 300 is inside an emoji; serde_json rejects a
  lone `\ud83d` escape, so the notification would be dropped (not run through the Tauri shell).
  Fix: `Intl.Segmenter` grapheme cuts.

### GG7-19 · Low · Arabic counts 11–99 use the `other` plural form · Verified

- `today.evidence` for 11 renders `عرض 11 مصدر` (should be `11 مصدرًا`); 44 of 70 Arabic plural
  messages in `apps/desktop/src/i18n/locales/ar.json` have no `many` branch
  (`doubletap.overlay.citations`, `knowledge.evidenceSummary`, `today.notify.briefTitle`…);
  `catalog.test.ts:81` tolerates it. Z2 follow-up.

### Observations from code reading (not reproduced, so not counted as findings)

- `is_sent_folder` (`nugget-proactive/src/corpus.rs:327`) knows only `sent`, `sent items`, `sent
  mail`; without `[proactive.engine] user_addresses` a German IMAP user's `Gesendete Elemente`
  would not identify their own mail. Knowledge has a localised list (`extract/mail.rs:41`) without
  ja/zh/ar/hi; the IMAP connector passes the folder name, not the `\Sent` flag it detects.
- Raw English daemon errors in Chat (`Chat.tsx:864`, e.g. `no provider is available for the
  Synthesis tier`) and the overlay (`src-tauri/src/doubletap/errors.rs:51-58`); a `disabled`
  refusal shows as "draft failed" (`errors.rs:42`). Related to GG4-23.
- The LLM extractor's `HONORIFICS` (`nugget-knowledge/src/llm.rs`) has no Frau/Herr/Sr./Mme/डॉ./様/
  先生/السيد (GG1-31 lists the same).


## What works well

- **Ask in the item's language** is solid across all nine languages and both models: the right
  mail cited first to fifth in 24/24 own-language questions per model, facts complete, answers in
  the question's language, local formats kept (`2.159,85 €`, `₹ 2,45,600`, `١٢٬٤٢٠ ر.س`,
  `11月20日`). Protect: retrieval plus the grounded prompt.
- **Double Tap follows the thread's language and register**: 32/33 in the right language with both
  models; Sie/du, vous/tu, usted/tú, o senhor/você, 敬語/casual, 您/你 and Gulf dialect vs MSA are
  matched (e.g. `Sehr geehrte Frau Sommerfeld`, `Salut Camille`, `هلا والله`); English replies to
  English-quoting mixed threads stay English.
- **CJK search works since DD4** (GG1-9 fixed): `湯豆腐`, `パンフレット`, `糖色`, `民宿` found
  mid-sentence; hybrid search 84% hit@5 overall, names in every script 13/13.
- **Ingest decodes real-world mail**: RFC 2047 subjects, quoted-printable, base64 and ISO-2022-JP
  bodies index correctly; every sender display name becomes a person in Latin, Arabic, Devanagari
  and CJK; `>`-quoted history is dropped in every language.
- **Fail-closed sensitivity**: without a model every unjudged item is sensitive in every language,
  and the rules never flagged an ordinary item (0/18 ordinary, 0/61 corpus).
- **Desktop locale plumbing**: `<html lang dir>` follows the locale in both windows; all 8
  catalogs have every English key; relative times and due dates use `Intl` (`Fällig übermorgen`,
  `期限 明後日`, `مستحق بعد الغد`); user and AI content carries `dir="auto"`; paths, ids and key
  combos are isolated with `<bdi dir="ltr">`; truncation is code-point safe.
- **Question marks**: `?` incl. the French spaced ` ?`, `¿…?`, a line-final `？` and Hindi `?` are
  detected, so formal meeting requests in en, es, fr, pt-BR, hi and zh-CN get reply cards.
