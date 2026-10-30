# GG1 area A (classify): reading the field and thread, classification, refusals

Explore-layer notes. All data synthetic (`qa/GG1/world/A-classify.json`, persona `cls-pat`,
domains `*.example.com`). Product code read at branch `qa/doubletap`.

Files:
- `qa/GG1/world/A-classify.json`: persona `cls-pat` (Pat Okafor, ops lead; 4 email + 4 chat sent,
  4 vault items of which 2 sensitive: door code `4417#`, admin password `Fjord!river-73`).
- `qa/GG1/scenarios/A-classify.json`: 54 scenarios, area `classify`, ids `cls-*`.
- `qa/GG1/probes/A-classify/`: probe against `nugget-a11y` (quoted mail parsing, AT-SPI roles).

## How to reproduce everything

```sh
cd /home/user/nugget
G=/home/user/gg1-target/release/gg1
$G lint  --scenarios qa/GG1/scenarios/A-classify.json
$G draft --scenarios qa/GG1/scenarios/A-classify.json --out /tmp/a.jsonl
python3 qa/GG1/check.py /tmp/a.jsonl --scenarios qa/GG1/scenarios/A-classify.json
# read-ahead on: same verdicts, refused fields are skipped with reason "refused"
$G draft --scenarios qa/GG1/scenarios/A-classify.json --read-ahead on --label ra-on --out /tmp/a-ra.jsonl
# probe (nugget-a11y quoted mail + AT-SPI role mapping)
cd qa/GG1/probes/A-classify && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/home/user/gg1-target cargo run --release -q
```

Scripted-provider result: `check.py` as shipped reports 28/54 failing, but 8 of those are a
**checker bug** (H1 below: correct search and excluded-app refusals are reported as "refused
differently"). With the refusal mapping fixed, **34/54 pass**; the 20 failures are all
product-determined (classification or gate outcome; no model involved) and are the findings
below. Read-ahead on gives the same 34/54; refused fields are skipped by read-ahead (19 x
`skipped: refused`), so no refused field was ever prefilled.

Note on `fake_reply`: fakes are written to *pass* the content checks wherever the outcome depends
on the model, so a scripted failure always means a gate/classification/prompt problem. Where
a refused field is (wrongly) drafted, the fake is the marker `DRAFTED-INTO-SECURE-FIELD` or a
plausible but harmful text (a shell command for terminals) to make the consequence visible.

## Candidate findings (most severe first)

### A1. Password fields labelled in other languages are drafted into, and the typed password goes into the prompt (High)
- Repro: `gg1 draft --only cls-pw-contrasena-typed,cls-pw-label-contrasena,cls-pw-label-mot-de-passe`
  (and `gg1 lint`).
- Context: custom web widget, `secure=false`, label `Contraseña` (also `Mot de passe`); in
  `cls-pw-contrasena-typed` the user already typed `Fjord!river-73`.
- Expected: refused `secure` (same as `Password`/`Passwort`, which are refused), field value
  never in a prompt.
- Actual: `gate=ok:form_field`; the provider received
  `<existing_text>\nFjord!river-73‸\n</existing_text>` and the draft was inserted after it
  (`inserted_field: "Fjord!river-73DRAFTED-INTO-SECURE-FIELD"`). Read-ahead `done` for it too,
  i.e. the password is also prefilled into the local model cache. With an app routed to a
  cloud provider (`app_providers`) it would leave the machine.
- Why: `is_secure` (`crates/nugget-draft/src/classify.rs` ~L224) has English phrases plus
  `passwort` only. Missing: `contraseña`, `mot de passe`, `kennwort`, `wachtwoord`, `senha`,
  `parola`, `hasło`, `пароль`, `パスワード`, `密码`/`密碼`, `비밀번호`, `كلمة المرور`, etc. The shell
  (`apps/desktop/src-tauri/src/doubletap/flow.rs` L227) only stops on the OS flag, so it reads
  and sends the value of such a field.
- Fix direction: a multilingual credential lexicon in `is_secure` (and share it with the
  shell so the value is not even read), plus an autocomplete/`inputmode` hint from the a11y layer
  where browsers expose it (`autocomplete=current-password|one-time-code|cc-csc`).

### A2. One-time-code and card fields with common real labels are not detected as secure (High)
- Repro: `gg1 draft --only cls-pw-otp-6digit,cls-pw-otp-typed,cls-pw-cvv2,cls-pw-card-de`.
- Expected: refused `secure`. Actual: `gate=ok:form_field`, drafted; `cls-pw-otp-typed` puts the
  typed code `482913` into `<existing_text>` of the prompt.
- Labels used: `Enter the 6-digit code we texted you` (placeholder `123456`), `CVV2`,
  `Kartennummer`. `CVV`, `One-time code`, `PIN` are refused correctly.
- Why: `WORDS` matches whole tokens, so `cvv2`/`cvc2` miss; there is no rule for
  "N-digit code"/"code we sent"/"enter code"; no non-English card terms (`Kartennummer`,
  `número de tarjeta`, `numéro de carte`, `Prüfnummer`).
- Owner: `crates/nugget-draft/src/classify.rs` `is_secure`.
- Fix direction: token prefix match for `cvv|cvc|cid`, a regex for `\b\d-digit code\b|code (we|that we) (sent|texted|emailed)|enter (the )?code`, multilingual card terms; prefer an
  `autocomplete` signal from the a11y layer.

### A3. Non-English search boxes in mail web apps are classified as an email body and drafted into (High)
- Repro: `gg1 draft --only cls-search-gmail-german,cls-search-outlook-spanish`.
- Expected: refused `not_draftable` (search). Actual: `classify=email_body gate=ok:email_body`, a full
  email (`"Hallo Lena, anbei die Infos. Viele Grüße Pat"`) is generated for Gmail's search combo
  box `In E-Mails suchen`, and for Outlook web's `Buscar` text field.
- Why: `is_search` (`classify.rs` ~L709) has only English prefixes/phrases; when it misses, the
  mail host family (rule 3) makes *every* non-header field on `mail.google.com` /
  `outlook.office.com` an email body, including single-line `combo_box`/`text_field` inputs.
- Fix direction: localized search terms (`suchen`, `buscar`, `rechercher`, `cerca`, `pesquisar`,
  `zoeken`, `検索`, `搜索`, `بحث`...); and in rule 3, a single-line `combo_box` should never become
  an `email_body`/`document` (fall to form field or refuse). Ideally the a11y layer reports
  ARIA `role=search`/`searchbox` ancestors (Gmail's search sits inside a `form role=search`).

### A4. Typing in VS Code's integrated terminal, PuTTY or Git Bash is not refused as a terminal (High)
- Repro: `gg1 draft --only cls-terminal-vscode-integrated,cls-terminal-putty,cls-terminal-mintty`.
- Expected: refused `terminal` ("inserted text could run as a command").
- Actual: `gate=ok:unknown` (VS Code, label `Terminal 1, bash`), `gate=ok:document` (`putty.exe`,
  `mintty.exe`); the scripted text `sudo systemctl restart nginx` / `git push --force` would be
  inserted into a live shell.
- Why: `is_terminal` (`classify.rs` L167) looks only at the app id / app name. `DEFAULT_TERMINAL_APPS`
  lacks `putty`, `kitty`-like Windows/SSH clients (`putty`, `mintty`, `mobaxterm`, `termius`,
  `securecrt`, `guake`, `yakuake`, `tilda`, `cool-retro-term`, `rio`, `contour`, `waveterm`); the
  field label is not consulted; and on Linux `nugget-a11y` maps the AT-SPI role `terminal` to
  `TextArea` (`crates/nugget-a11y/src/role.rs` L355: `"text" | "terminal" => Role::TextArea`),
  so an embedded VTE terminal (Guake, Tilda, GNOME Builder, Geany) loses its only signal
  (confirmed by the probe output below). The shell's own `settings::is_terminal` has the same
  app-only view.
- Fix direction: keep a `terminal` field role (or a `terminal: bool` on `FieldInfo`) from AT-SPI
  and UIA (`ControlType` + class `TermControl`/xterm.js `xterm-helper-textarea`), refuse a field
  whose label/placeholder starts with `Terminal` in IDEs, and extend the app list.

### A5. The local prompt keeps the END of each thread message, so the question at the top of a long email is cut (High)
- Repro: `gg1 draft --only cls-thread-long-latest-message,cls-outlook-desktop-quoted` then read
  `result.prompt.user` in the jsonl.
- Expected: the model sees the newest message's question ("what is the venue deposit amount,
  and is it still due on the 14th?", "Can you confirm the venue deposit amount before I send the
  PO?").
- Actual (`cls-thread-long-latest-message`): the only message in `<thread>` starts with
  `…g. Riverside Lofts quoted a higher room rate ...` and ends `...once I have the two numbers above.`
  The question itself is gone. In `cls-outlook-desktop-quoted` (a reply whose new text is at the
  top and the quoted original below, which is exactly what `nugget-a11y::thread::quoted_messages`
  and Outlook/Apple Mail reading panes produce) the model sees only the tail of Lena's *old*
  quoted mail (`…orty cars, the AV package includes...`), nothing of Omar's question, nor even that
  Omar wrote anything.
- Why: `user_text` in `crates/nugget-draft/src/prompt.rs` (~L460) uses
  `untrusted_tail(m.text, limits.message_chars)`; `PromptLimits::local()` sets
  `message_chars: 600`, `thread_chars: 1_200`. Tail-keeping is right for the field's text before
  the caret, wrong for messages, where the new content leads and quoted history trails.
- Scripted run passes (the fake contains the answer); with a real 4B model the `gold` facts
  should be missing. Real-model run needed to quantify.
- Fix direction: keep the head of each message (or head + tail with an ellipsis), and strip
  quoted history (`> ` lines, `On ... wrote:`, `From:/Sent:` blocks) from thread messages before
  budgeting.

### A6. Gmail's "To recipients" box and Outlook's "Add a subject" box are classified as email bodies (Medium)
- Repro: `gg1 lint --scenarios qa/GG1/scenarios/A-classify.json` -> `cls-gmail-to-field`,
  `cls-outlook-subject-placeholder`.
- Expected: `form_field` (single line). Actual: `classify=email_body`; the scripted Outlook draft
  is a whole multi-line email with greeting and sign-off going into the subject line, and an
  email body prompt is built for Gmail's To box.
- Why: `is_header_field` (`classify.rs` ~L799) matches only exact `to`/`cc`/`subject`/`title`/...
  or a `subject`/`title` prefix. Gmail labels its recipients combo box `To recipients`
  (`Cc recipients`, `Bcc recipients`), and new Outlook shows only a placeholder `Add a subject`.
- Fix direction: header match on first word (`to`, `cc`, `bcc`, `subject`, `add a subject`,
  `recipients`), and generally never classify a single-line `text_field`/`combo_box` in a mail
  family as `email_body`.

### A7. Ordinary chats are refused as "password and secure fields" when the channel or person name contains a credential word (Medium)
- Repro: `gg1 draft --only cls-fp-slack-password-channel,cls-fp-slack-mfa-channel,cls-fp-teams-dm-pin`.
- Expected: `chat_message` drafts (Slack `Message #it-password-help`, `Message #mfa-rollout`, Teams
  `Message Pin Li`). Actual: `error: invalid input: drafting is refused in password and secure fields`.
- Why: `is_secure` checks the whole label/placeholder for `password` (substring) and whole words
  `pin`, `mfa`, `otp`, `ssn`, `iban`. Composer labels embed channel/person names. IT/security
  teams (the people most likely to live in `#password-reset`, `#mfa`, `#sso`) lose the feature in
  their main channels, with a misleading message.
- Fix direction: run the label rule only for single-line `text_field`s (composers are
  `text_area`), or strip the `Message #channel`/`Message <name>` target before matching; keep the
  OS secure flag as the strong signal.

### A8. Helpdesk ticket replies without a mail-family host are classified as chat (Medium)
- Repro: `gg1 lint` -> `cls-freshdesk-ticket` (`classify=chat_message`, want `email_body`).
  Compare `cls-zendesk-ticket`, which is `email_body` only because the scenario supplies a subject
  and recipients.
- Why: for unknown web apps only `subject`/`recipients` mean email, and the shell fills those
  only for mail families (`nugget-a11y::thread::extract` reads recipients/subject only when
  `family.is_mail()`), so a real Zendesk/Freshdesk/Help Scout/Intercom reply arrives with a thread
  only and gets the chat prompt ("no sign-off", "one short paragraph"). Support agents sign
  every reply. Note `cls-zendesk-ticket` therefore overstates what the desktop would send.
- Fix direction: a `Support` host list (`zendesk.com`, `freshdesk.com`, `helpscout.net`,
  `intercom.com`, `front.com`, `gorgias.com`, `hubspot.com/help-desk`...) mapped to email-style
  replies, and labels `Public reply`/`Reply to customer`.

### A9. Own messages are not recognised in Slack/Outlook/Mail threads, and the prompt never names the user (Medium)
- Repro: `gg1 draft --only cls-slack-web-dm`, read `result.prompt`.
- Evidence: the thread is
  `<message author="Jordan Reyes" ...>rota for next week is up in #ops</message>` and
  `<message author="Pat Okafor" ...>thx. are you covering friday or is Omar?</message>` with no
  `from` attribute, and neither the system nor the user prompt says the user is Pat Okafor.
  The model cannot tell that the last message is the user's own unanswered question and will
  tend to answer it ("i'm covering friday"), as Jordan.
- Why: `nugget-a11y::thread::ThreadLimits::self_names` sets `from_me` from author names, but no
  caller sets it (`grep -rn self_names apps crates` finds only `thread.rs`); only apps that label
  the user "You" (iMessage, WhatsApp, Teams, Gmail) get `from_me`. Slack, Outlook desktop, Apple
  Mail, Thunderbird and every quoted-mail thread (`from_me: None` always in `quoted_messages`) have
  none.
- Fix direction: pass the user's names/emails (they exist: persona `emails`, voice `MeIdentity`)
  into `ThreadLimits::self_names` from the shell, and/or add "The user is <name> <email>" to the
  prompt so the model can tell.

### A10. Empty field with no thread in an unknown app: drafts "a reply that fits the conversation" with no conversation (Medium)
- Repro: `gg1 draft --only cls-empty-unknown-app`, read `result.prompt.user`:
  `Draft a reply that fits the conversation. The field is in Journal (field kind: unknown). ...
  Write the unknown now, as the user, in about 12 words.`
- Expected (proposal): either refuse with a clear "nothing to reply to here" (cheapest, no
  invention), or a neutral "continue writing" prompt. Asking a 4B model for a *reply* with no
  thread invites invented context ("Thanks for reaching out, as discussed I've attached...").
  The `forbidden_regex` in the scenario checks exactly this on real models.
- Also: the literal instruction `Write the unknown now` (from `input.kind.as_str()`) is broken
  English for `unknown` (and `Write the form field now` for forms); see `prompt.rs` end of
  `user_text`.
- Same path for `cls-form-why-job` (`Why do you want this job?` textarea -> `unknown`): no kind for
  long-form answers, so a free-text application answer is asked for as "a reply".
- Owner: `classify.rs` (rule 5 fallthrough), `prompt.rs` `kind_guidance`/`user_text`.

### A11. Unlabelled browser address bar while on a mail site becomes an email body (Low/Medium, needs on-device confirmation)
- Repro: `gg1 draft --only cls-search-safari-omnibox-unlabelled`.
- Context: `com.apple.Safari`, `role=text_field`, no label, value = the URL, page URL
  `mail.google.com`. Actual `classify=email_body`, drafted. Labelled omniboxes (`Address and search
  bar`, `Search or enter address`) are correctly refused (`cls-search-chrome-omnibox`).
- Why: the page URL (`app.url`) is applied to every focused field in the browser window,
  including browser chrome. Whether Safari/Arc/Edge expose an unlabelled omnibox needs checking on
  a Mac; the rule gap is real regardless.
- Fix direction: a11y layer marks fields outside the web area (`web_area` ancestor absent) so the
  page host is not used for them; or a single-line field whose value is a URL is never a body.

### A12. The daemon's excluded-app defaults are narrower than the shell's deny list (Low)
- Repro: `gg1 draft --only cls-excluded-google-passwords` -> `gate=ok:unknown`, drafted
  (`passwords.google.com`).
- The desktop shell refuses this via `nugget_a11y::DenyList::defaults()` (`host:passwords.google.com`,
  `host:dashlane.com`, banking, system settings: `crates/nugget-a11y/src/deny.rs` ~L86-L120), but
  `nugget_draft::DEFAULT_EXCLUDED_APPS` (used by `draft.begin`/`draft.read_ahead` in nuggetd) has
  neither, so any other IPC client (CLI, e2e, future integrations) is not protected. Two lists that
  drift. Fix: one list (the a11y `DenyList`) used by both.

### A13. Refusal message for an excluded web host names the browser (Low)
- Repro: `gg1 draft --only cls-excluded-bitwarden-web` -> `drafting is disabled in Google Chrome`.
  The user is told Double Tap is disabled in Chrome, which is false (it works in Gmail in the same
  window). `display_app` in `classify.rs` should name the matched host (`vault.bitwarden.com`).

### A14. Mail quoted-history parsing is English/German/French-only and flattens nested quotes (Low)
- Repro: probe `qa/GG1/probes/A-classify` (output below).
- `quoted_messages` recognises `wrote:`, `schrieb:`, `a écrit :` and `From:`+`Sent:|Date:`;
  Spanish `escribió:`, Outlook German `Von:`/`Gesendet:`, and other locales give no thread at all
  (draft without context). A nested chain becomes one message whose text contains the deeper
  `On ... wrote:` lines (and, with A5, only its oldest tail reaches the model).
- Owner: `crates/nugget-a11y/src/thread.rs` `quoted_messages`.

### A15. Prewarm runs for triggers on secure fields (Low, informational)
- `fire()` in `flow.rs` prewarms from `a11y.frontmost()` (app only) before the field is read,
  and `Drafter::prewarm` gates on an app-only hint, so a trigger in a browser password field still
  warms the local model's system prefix (voice examples). Only local, no field data; noting it
  because the module docs say nothing is done for secure fields.

### Harness notes (for the QA lead, not product findings)
- H1 `check.py` refusal matching: `re.search(want_refuse.replace("_", "[ _]"), error)` never
  matches the engine's strings for `not_draftable` (`nothing to draft in a search field`) and
  `excluded_app` (`drafting is disabled in 1Password`), so every *correct* search/excluded
  refusal is reported as `refused differently`. Suggested mapping:
  `{"not_draftable": "nothing to draft", "excluded_app": "drafting is disabled in (?!terminals)", "secure": "secure", "terminal": "terminals"}`.
  (The stable codes exist only in nuggetd `refusal_kind`; the harness could also emit
  `gate.code` using the same mapping.)
- H2 `check.py` returns early for `refuse` scenarios, so `secrets_not_in_prompt` is not evaluated
  when a refusal is missed; the "a prompt reached the model" problem covers it, but quoting the
  leaked value would make A1/A2 visible in the summary.
- H3 `detect_language` returns `None` for short English chat replies without stopwords
  (`yep go ahead, thx for the fix`), so I left `language` off short chat scenarios.

## What works well (protect these)
- Gate order is right: secure -> excluded -> terminal -> classify, and refused contexts never
  reach a provider: in all 19 correctly refused scenarios `result.prompt` is absent and read-ahead
  reports `skipped: refused` (no prefill).
- OS secure flag always wins (`cls-pw-secure-flag`); English and German credential labels
  (`Password`, `Passwort`, `PIN`, `One-time code`, `CVV`) are refused even without the flag.
- Search detection in English is solid across disguises: `combo_box` "Search mail", placeholder-only
  "Search", Chrome omnibox "Address and search bar", Slack "Jump to…" and "Search Northwind", and an
  ARIA `search_field` with a misleading label ("Messages").
- Password managers refused by bundle id and by web host (`com.1password.1password`,
  `vault.bitwarden.com`); iTerm2 and Windows Terminal refused.
- Mail/chat/comment classification for mainstream apps: Gmail body, Gmail subject (form, single
  line), Outlook mac and `OUTLOOK.EXE` (document role), Slack desktop/web, Teams 2, LinkedIn
  `/messaging` (chat, not "comment" as the plain linkedin.com host would give), GitHub comment, web
  form "Company name", Spanish Gmail body label (host decides).
- Long threads: 27-message Slack thread keeps the newest 11 messages, oldest first, with
  `(earlier messages omitted)`, and the question in the newest message is present.
- `from`, `author`, `time` attributes are emitted and sanitised (`<`/`>` to `‹`/`›`, quotes
  stripped), and Teams' "You" maps to `from="user"`.

## Probe output
`cargo run --release -q` in `qa/GG1/probes/A-classify` (2026-10-30):

```
== en gmail: 1 message(s)
   author=Some("Lena Park <lena.park@venue.example.com>") ts=Some("Thu, 29 Oct 2026 at 10:00") text[..80]="Can you confirm the deposit?"
== es gmail: 0 message(s)
== en outlook: 1 message(s)
   author=Some("Lena Park <lena.park@venue.example.com>") ts=Some("Thursday, October 29, 2026 10:00 AM") text[..80]="Can you confirm the deposit?"
== de outlook: 0 message(s)
== nested en: 1 message(s)
   author=Some("Omar Haddad <omar.haddad@northwind.example.com>") ts=Some("Thu, 29 Oct 2026") text[..80]="What is the deposit amount?\n\nOn Mon, 12 Oct 2026, Lena Park <lena.park@venue.exa"
== AT-SPI role 'terminal' -> TextArea
== AT-SPI role 'password text' -> TextField
```

Spanish `escribió:` and Outlook German `Von:/Gesendet:` give 0 messages (A14); the nested chain is one
message containing the inner `On ... wrote:` (A14, A5); AT-SPI `terminal` becomes `TextArea` (A4).
