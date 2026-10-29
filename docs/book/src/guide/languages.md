# Languages

The desktop app is translated into nine languages:

| Language | Code | Direction |
|---|---|---|
| English | `en` | left to right |
| Español | `es` | left to right |
| Deutsch | `de` | left to right |
| Français | `fr` | left to right |
| Português (Brasil) | `pt-BR` | left to right |
| 简体中文 | `zh-CN` | left to right |
| 日本語 | `ja` | left to right |
| हिन्दी | `hi` | left to right |
| العربية | `ar` | right to left |

## Choosing a language

By default Nugget follows your operating system's language: the closest match wins (`pt-PT` gets
Brazilian Portuguese, `zh-TW` gets Simplified Chinese, `es-MX` gets Spanish), and anything else
gets English. To pick one yourself, use the **Language** menu at the bottom of the sidebar (it is
also in the first-run setup). **System** goes back to following the OS. The choice is stored on
this device only.

In Arabic the whole window is mirrored: the sidebar moves to the right, text is right-aligned and
arrows that mean "back" or "next" point the other way. Your own content (notes, mail, chat
messages, names) keeps its own direction, so an English note still reads left to right inside the
Arabic interface.

The daemon, the command line and the logs are in English. Your sources and answers are in whatever
language they were written in; models answer in the language you ask in.

## How the translations are made

Every message lives in a catalog under `apps/desktop/src/i18n/locales/`, one JSON file per
language, with English (`en.json`) as the source of truth. Messages use a small subset of ICU
MessageFormat: `{name}` inserts a value, `{n, plural, one {…} other {…}}` picks a plural form,
and `<code>…</code>` marks rich text. A missing message falls back to English, so a new string
never breaks the app.

Some translations were made by machine and have not yet been checked by a native speaker. They are
listed, per language, in `apps/desktop/src/i18n/locales/review/<code>.json`:

```json
{
  "wave": "Z2",
  "note": "Machine-translated in wave Z2; a native speaker should review these keys.",
  "keys": ["chat.generalAnswer", "search.indexingHint"]
}
```

## Reviewing a translation

You don't need to build anything to review a language, only a text editor and a little time.

1. Open `locales/review/<code>.json` for your language and `locales/<code>.json` next to
   `locales/en.json`.
2. For each key listed, compare the translation with the English. Fix what is wrong, awkward or
   inconsistent with the rest of the catalog: use the words the app already uses for the same
   things (for example the page names in `app.nav.*` and the button a message refers to).
3. Keep the structure: every `{name}`, every plural or select keyword (`plural`, `one`, `other`,
   `=0`, `#`) and every `<tag>` must stay as it is. Translate only the text, including the text
   inside plural branches. Add the plural forms your language needs (Arabic uses `zero`, `one`,
   `two`, `few`, `many` and `other`).
4. Remove the keys you checked from the review list (delete the file when the list is empty), and
   open a pull request. The catalog tests tell you if a placeholder or tag went missing.

If you are a native speaker and something reads oddly in the app itself, an issue with a screenshot
is just as welcome.

## The checks

`pnpm test` in `apps/desktop` runs the catalog tests (`src/i18n/catalog.test.ts`), which CI runs
on every change. For each language they check that:

- it has every English key and no others, and no empty messages;
- each message keeps the English message's placeholders and tags, and parses;
- each plural has the forms the language needs (`one` where it has one, and `other`);
- no message is still the English text, unless the key is allow-listed in
  `src/i18n/same-as-english.json` (brand names such as Gmail, keyboard keys, format-only strings
  such as `{from}–{to}`, and words a language really does share with English, like German *Status*);
- every key in a review list exists.

A lint test (`src/i18n/lint.test.ts`) fails when a component shows text that bypasses the catalog.

## Checking the layout

Translations are often longer than English (German by a third, sometimes more), and Arabic runs
right to left. Two tools catch layout problems before a translator or user does:

- **The pseudo-locale.** In a development build (`pnpm dev`) the Language menu also offers
  *Pseudo (en-XA)*: English with accented letters, padded about 35% longer, such as
  `[Šéáŕçĥ ýóúŕ šóúŕçéš ļóŕéḿ]`. Text that is cut off, overflows, or shows up unaccented (it bypasses
  the catalog) is easy to spot.
- **The overflow test.** `pnpm run e2e` (see `apps/desktop/e2e/README.md`) opens every page of the
  real app in the pseudo-locale, German, Japanese and Arabic, takes a screenshot of each, and fails
  when a labelled element (a button, link, heading, tab, label or badge) has text clipped by or
  overflowing its box. `E2E_I18N_LOCALES` picks the languages.

## Adding a language

1. Add the language to `LOCALES` in `apps/desktop/src/i18n/index.tsx` with its own name for itself
   and its direction.
2. Copy `en.json` to `<code>.json` and translate it, then list every key in
   `locales/review/<code>.json` until a native speaker has checked it.
3. Run `pnpm test` and `pnpm run e2e` with `E2E_I18N_LOCALES=<code>`, and fix what they report.
