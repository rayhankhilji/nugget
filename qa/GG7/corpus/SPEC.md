# GG7 corpus specification

Synthetic, hand-written mailboxes and notes for testing Nugget's AI features in the nine UI
languages. Everything here is invented: people, companies, addresses, numbers. Domains are
`example.com/.net/.org` or `*.example`. No real people.

## The user

The mailbox owner is **Sam Okafor** `<sam.okafor@example.net>` (works at "Lumen Pfad GmbH"-style
invented companies in each scenario as fits; any gender-neutral role). "Today" is
**Friday 2026-10-30**; mails are dated 2026-10-19 .. 2026-10-30, deadlines and meetings fall in
2026-11-02 .. 2026-11-27.

## Layout

```
qa/GG7/corpus/<lang>/NN-slug.eml   (or .md / .ics)
qa/GG7/gold/<lang>.json
```

`<lang>` is one of `en de es fr hi ja pt-BR zh-CN ar mixed`.

Each language folder has **6 files**, written natively in that language (not translated English;
use idioms, local formats for dates, numbers, currency, addresses, and native names):

| NN | kind | content |
|---|---|---|
| 01 | .eml | Formal incoming mail from a business contact asking Sam for something that needs a reply (meeting request with a concrete date + time + place). Formal register (Sie / usted / vous / आप / 敬語 / 您 / o senhor/a senhora / حضرتك). Names a person, their organisation, phone number. |
| 02 | .eml | Informal mail from a friend or close colleague (du / tú / tu / तुम / casual Japanese / 你 / você / informal Arabic) asking a question or proposing a plan; needs a reply. |
| 03 | .eml | Factual mail with a unique, checkable fact set: an invoice or booking (number like `RE-2026-0412`, amount in local format e.g. `1.284,50 €`, `₹ 12,450`, `¥38,500`, `R$ 2.390,00`, `١٬٢٥٠ ر.س` or `1,250 SAR`), a due date, an address. |
| 04 | .eml | A commitment mail: someone asks Sam to deliver something by a deadline expressed with a weekday + date in local style ("bis Freitag, 14. November", "antes del viernes 14 de noviembre", "11月14日（金）まで", "14 नवंबर तक", "下周五（11月14日）前", "até sexta-feira, 14/11", "قبل يوم الجمعة ١٤ نوفمبر"). Also mentions a second person by name. |
| 05 | .md  | A personal note (meeting notes, a recipe, a travel plan) with distinctive vocabulary for search: inflected forms, compounds, diacritics. |
| 06 | .eml | A reply thread: a mail that quotes an earlier message (`> ` quoting or "On ... wrote:" in the local mail-client style, e.g. "Am 28.10.2026 um 14:03 schrieb ...", "El mar, 28 oct 2026 a las 14:03, ... escribió:", "2026年10月28日(水) 14:03 ... :", "في الثلاثاء ٢٨ أكتوبر ٢٠٢٦ كتب ...:"). |

`mixed/` instead has 6 code-switching files: German/English thread, Spanish/English (Spanglish)
Slack-style mail, Hinglish (romanized Hindi + English) and a Hindi/English mail, Arabic with French
and English product names and Latin digits, Japanese with English tech terms (katakana and Latin),
and one `.ics` calendar invite whose SUMMARY/DESCRIPTION/LOCATION are in Chinese.

### .eml format

Real-looking RFC 5322 mail, UTF-8, CRLF or LF both fine:

```
From: Katharina Vogt <k.vogt@hausverwaltung-ried.example>
To: Sam Okafor <sam.okafor@example.net>
Date: Tue, 27 Oct 2026 09:14:00 +0100
Subject: =?UTF-8?B?...?=          (encoded word for non-ASCII subjects in about half the files; raw UTF-8 in the others)
Message-ID: <unique@host.example>
In-Reply-To / References         (for 06)
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8
Content-Transfer-Encoding: 8bit  (use quoted-printable in at least one file per language, base64 in one)
```

Use realistic time zones for the sender (+0100 de/fr in late Oct is CET after 25 Oct, +0530 India,
+0900 Japan, +0800 China, -0300 Brazil, +0300 Saudi Arabia, -0600 Mexico or +0100 Spain).
For `ja`, write file 03 in `charset=ISO-2022-JP` with `7bit` encoding (still common in Japanese
mail). Encode with Python, never by hand. Signatures in local style.

Bodies: 60–200 words, natural. Do not put the gold answers only in the subject.

## Gold file `gold/<lang>.json`

```json
{
  "lang": "de",
  "search": [
    {"id": "de-s1", "query": "Nebenkostenabrechnung", "expect": ["de/03-....eml"], "kind": "exact",
     "why": "plain word present in the body"}
  ],
  "ask": [
    {"id": "de-a1", "question": "Wie hoch ist die Rechnung von der Hausverwaltung und bis wann muss ich zahlen?",
     "facts": [["1.284,50", "1284,50", "1284.50"], ["14. November", "14.11", "14.11.2026"]],
     "cite": ["de/03-....eml"]}
  ],
  "draft": [
    {"id": "de-d1", "file": "de/01-....eml", "register": "formal", "instruction": "",
     "accept_lang": ["de"],
     "formal_markers": ["Sie", "Ihnen", "Ihr"], "informal_markers": ["du ", "dich", "dir", "dein"]}
  ],
  "extract": {
    "people": [{"name": "Katharina Vogt", "aliases": ["Frau Vogt"], "email": "k.vogt@hausverwaltung-ried.example", "file": "de/01-....eml"}],
    "orgs": [{"name": "Hausverwaltung Ried", "file": "de/01-....eml"}],
    "dates": [{"date": "2026-11-14", "time": "10:00", "text": "Freitag, 14. November, 10 Uhr", "file": "de/04-....eml", "what": "Abgabe Präsentation"}]
  },
  "proactive": [
    {"file": "de/04-....eml", "expect": "commitment", "due": "2026-11-14", "who": "Katharina Vogt"},
    {"file": "de/01-....eml", "expect": "reply_needed"}
  ]
}
```

Search: **8 queries** per language, each with a `kind` from
`exact` (word as written), `inflected` (another grammatical form than the text has: plural, case,
conjugation), `compound_part` (de: part of a compound; for others: a substring word inside a longer
form), `no_diacritics` (query typed without accents / with a variant spelling: Strasse for Straße,
reunion for reunión, ا for أ, no tashkeel, NFD vs NFC for Hindi, half-width vs full-width for ja),
`cjk_word` (ja/zh: a 2–4 character word in the middle of a sentence), `name` (a person's name,
for ja/zh/ar/hi in native script), `semantic` (a paraphrase with no shared words, needs embeddings),
`cross_lingual` (English query for a non-English item, e.g. "invoice from the property manager").
Pick the kinds that make sense for the language; at least one `exact`, one `name`, one `semantic`
and one `cross_lingual` everywhere.

Ask: **3 questions** in the language, plus **1 question in English** about a non-English item
(`"question_lang": "en"`, answer expected in English). `facts` is a list of alternatives lists:
each inner list is satisfied if any alternative appears in the answer.

Draft: **3 drafts** per language: file 01 (formal), 02 (informal), 06 (thread reply). Optionally an
`instruction` in the user's UI language or English (e.g. "accept, suggest Thursday instead").
`accept_lang` is the language codes the reply may be in (normally just the thread's).

Proactive: what a good assistant should surface from these mails.
