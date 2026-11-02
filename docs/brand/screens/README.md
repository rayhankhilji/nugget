# Heed screenshots

Real screenshots of the desktop app, for the founder deck and the website hero. They are taken at
1440×900 @1x, and each is under 400 KB.

Every name, company, address and message in them is fictional. The data comes from one working
day in `apps/desktop/e2e/fixtures/day.mjs`: Tuesday 3 November 2026 at 08:14 in London, signed in
to Heed Cloud. Regenerate them after a UI change:

    cd apps/desktop
    node e2e/brand-screens.mjs        # SHOTS_ONLY=home-light,needs-you to retake some

| File | What it shows |
| --- | --- |
| `home-light.png`, `home-dark.png` | Home: greeting, ask box, suggested questions and the written morning brief with its sources |
| `needs-you.png` | Needs you: the card list above the approval queue (an email reply and a calendar invite, each as an agent-progress card) |
| `search.png` | Search across the connected apps, with source filters |
| `connections.png` | Heed Cloud connections: four connected apps, Slack still importing, and the app gallery |
| `account.png` | Account: Heed Pro on trial, the smart-ask usage meter and Upgrade |
| `welcome.png` | First run: sign in to Heed Cloud over the sky and dither backdrop |
| `quickask-dark.png` | The quick ask window, dark, with a cited answer |
| `ghost-text.png` | Ghost text in a mail compose window. The window is a neutral mock drawn by the script; the suggestion and its Tab hint use the app's real ghost styles |

## Open-core design specs

`open-core-*.png` are not app screenshots. They are renders of `docs/brand/open-core.html`, the
design spec for the first-run fork, Choose your AI and provider status. That page uses the app's
own stylesheets, so its classes (`.ob-fork`, `.ai-choice`, `.key-field`, `.provider-status`) are
the ones the real views use.
