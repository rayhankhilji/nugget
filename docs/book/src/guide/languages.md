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

The daemon, command line and logs are in English. Models answer in the language you ask in.

## Translations

Catalogs are in `apps/desktop/src/i18n/locales/`, one JSON file per language, with `en.json` as the
source. A missing message falls back to English. The translations other than English were made by
machine and have not all been checked by a native speaker.

To review one, fix `locales/<code>.json` against `en.json`, keeping every `{name}`, plural keyword
and `<tag>`, and open a pull request (`pnpm test` in `apps/desktop` checks the catalogs). An issue
with a screenshot is just as welcome.

To add a language, add it to `LOCALES` in `apps/desktop/src/i18n/index.tsx`, copy `en.json` to
`<code>.json` and translate it, and run `pnpm test` and `pnpm run e2e` with
`E2E_I18N_LOCALES=<code>`.
