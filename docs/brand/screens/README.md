# Heed screenshots

Real screenshots of the desktop app, for the founder deck and the website hero. They are taken at
1440×900 @1x, and each is under 400 KB.

Shortcut hints show the Mac labels (⌘↵, ⌘K), as on the founder's Mac. Every name, company, address and message in them is fictional. The data comes from one working
day in `apps/desktop/e2e/fixtures/day.mjs`: Tuesday 3 November 2026 at 08:14 in London, signed in
to Heed Cloud. Regenerate them after a UI change:

    cd apps/desktop
    node e2e/brand-screens.mjs        # SHOTS_ONLY=home-light,needs-you to retake some

| File | What it shows |
| --- | --- |
| `home-light.png`, `home-dark.png` | Home: greeting, ask box, suggested questions and the written morning brief with its sources |
| `needs-you.png` | Needs you: one queue (Waiting for your approval, then Needs a reply or decision) beside one pane; the selected approval shows exactly what it will send, with Approve ⌘↵ and Reject ⌫ |
| `search.png` | Search across the connected apps, with source filters |
| `search-cmdk.png` | ⌘K over Home: live results in sections (People, Docs, Mail, Messages), "Open ↵" on the active row, and the key legend |
| `connections.png` | Heed Cloud connections: four connected apps, Slack still importing, and the app gallery |
| `account.png` | Account: Heed on trial, the smart-ask usage meter and Upgrade |
| `welcome.png` | First run: sign in to Heed Cloud over the sky and dither backdrop |
| `quickask-dark.png` | The quick ask window, dark, with a cited answer |
| `ghost-text.png` | Ghost text in a mail compose window. The window is a neutral mock drawn by the script; the suggestion and its Tab hint use the app's real ghost styles |

## Open core (real screens)

Taken from the app like the rest, light and dark:

| File | What it shows |
| --- | --- |
| `open-core-fork-*.png` | First run: "How do you want to run Heed?", the Heed Cloud and Run it yourself cards over the sky |
| `open-core-choose-ai-*.png` | Choose your AI, running it yourself. Light: Sign in with ChatGPT selected and signed in. Dark: OpenAI API key open with its key form |
| `open-core-provider-status-*.png` | Settings → Models → Your AI: ChatGPT signed in, an OpenAI key working, an OpenRouter key not working, each with its model and one action |

The design spec they follow is `docs/brand/open-core.html`.
