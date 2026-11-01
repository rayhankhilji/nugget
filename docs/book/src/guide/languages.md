# Languages

The desktop app is available in nine languages:

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

Nugget follows your OS language by default, using the closest match (`pt-PT` gets Brazilian
Portuguese, `zh-TW` Simplified Chinese) or English. To choose, use the **Language** menu at the
bottom of the sidebar or in first-run setup; **System** goes back to the OS. The choice is stored on
this device only.

In Arabic the window is mirrored. Your own content keeps its own direction, and answers and Double
Tap drafts follow the script of each paragraph. Dates in generated text use your language's format.

Mail replies are separated from quoted text in each supported language's mail-client style, so only
the new text counts as the message.

The daemon, command line and logs are in English. Models answer in the language you ask in.

## Translations

Catalogs are in `apps/desktop/src/i18n/locales/`, one JSON file per language, with `en.json` as the
source. A missing message falls back to English. Some translations are machine-made and not yet
checked by a native speaker; they are listed in `locales/review/<code>.json`.

To review one:

1. Compare the keys listed in `locales/review/<code>.json` against `en.json`, using the words the app
   already uses elsewhere.
2. Keep every `{name}`, plural keyword (`plural`, `one`, `other`, `=0`, `#`) and `<tag>`; translate
   only the text, and add the plural forms your language needs.
3. Remove the checked keys from the review list and open a pull request. `pnpm test` in
   `apps/desktop` checks placeholders, tags and plurals.

An issue with a screenshot of anything that reads oddly is just as welcome.

To add a language, add it to `LOCALES` in `apps/desktop/src/i18n/index.tsx`, copy `en.json` to
`<code>.json`, list every key for review, and run `pnpm test` and `pnpm run e2e` with
`E2E_I18N_LOCALES=<code>`.
