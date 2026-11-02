# Heed v4: real-world pattern research (Mobbin)

Owner: MB1 (design research) · Branch: `ds/mobbin` · Date: 2026-11-02
Audience: DS1 (app restyle, `ds/brand`) and WEB1 (website, `web/site`).

**Read this first**

- This file contains only Mobbin links and our own written descriptions. No Mobbin images, screenshots or downloads are committed. The repo is public and the screens are copyrighted, so open the links to look at them.
- Mobbin links open for anyone with a Mobbin account.
- Founder direction is fixed and is assumed throughout: ultramarine `#1F3BFF` on cream `#F6F3EC`, ink `#14110F`, deep `#070798`, an ink-blue dark mode (`#0B0B1A`), Fraunces for display, Geist for UI, Geist Mono for numbers, calm white cards over sky or painterly imagery, and dither/engraved single-blue art. References below are for **structure, density, interaction and copy**. They are not for colour; most AI apps in the set use purple gradients, and Heed should not.
- Spacing tokens used in the specs: `4 / 8 / 12 / 16 / 24 / 32 / 48 / 64`. Radii: card 16, control 10, chip 999.

---

## Top 10 highest-leverage changes (ranked by impact on perceived quality)

1. **Needs you becomes a two-pane review queue, showing the real artifact.** Keep a list on the left with one-line summaries ("Reply to Jane: ship date moved"). The right pane shows the email, invite or post *exactly* as it will go out, with Approve, Edit and Reject pinned to the bottom. This one change makes the AI feel trustworthy, and trust is the product. (Rox, Linear Inbox, Customer.io)
2. **An editorial home headline.** Use a light Fraunces greeting and a "here's your focus" line at 40–48px, then brief prose with inline source chips, then 3–7 ranked actions. It replaces dashboard-widget sprawl. (Rox Home, Town hero, Fireflies)
3. **Inline citation chips everywhere** (brief, chat, quick ask). Put a small pill naming the source app or person right after the claim, plus an "N sources" stack at the end of the answer that opens a sources panel. (Dropbox Dash, ChatGPT, Gemini Notebook)
4. **The agent-progress card as a reusable primitive.** A vertical step list shows done steps ✓, the current step with a spinner and live sub-label, and pending steps muted. Add a thin progress bar and Pause / View draft. Use the same component for onboarding sync, imports and agent tasks. (Lindy, Relevance AI, Juicebox)
5. **One ⌘K surface for search and quick ask.** Results are grouped by type, with a right-aligned "Open ↵" on the selected row and a key legend in the footer. Tab turns the query into an Ask Heed question. (Whop, Juicebox, Magnific, Dropbox Dash)
6. **Onboarding is one floating card over full-bleed imagery.** Show one decision per screen. The Accessibility permission gets its own card that explains why it's needed and what Heed will and won't do. (Superhuman, Tana, Amie)
7. **The connections gallery shows sync health.** Use Perplexity-style tiles with Connected and Available filters. Per-app pages show a status pill, "Synced 4 min ago", a Sync now button and an inline repair banner. (Perplexity, Deel, Customer.io)
8. **Calm, honest usage and trial UI.** Use a single summary strip (Plan · Price · Renews · Usage bar) and a quiet "Trial ends <date>" row in the sidebar footer. Show a modal only at ≤3 days. (Rox, Langdock, Docusign, Slite)
9. **A two-plan pricing page with a Monthly/Annual segmented toggle**, with "Save x%" inside the Annual segment, a serif headline and a comparison table below the fold. (Jasper, Town, Opennote)
10. **Undo toasts and calm empty states.** Every approve, dismiss or snooze toast has Undo (⌘Z). Empty states get one engraved single-blue illustration, one line and nothing else. (ClickUp, Front, HubSpot)

---

## 1. First-run and onboarding

**References**

- [Superhuman: welcome over landscape, "Getting ready…"](https://mobbin.com/screens/468dc187-d613-4926-80df-ff2dbbef6c12): a full-bleed landscape photo with dimmed overlay and two centered lines of copy. A tiny spinner and "Getting ready…" sit near the bottom. This is the closest match to the "calm over painterly sky" direction. Take it for the moment after sign-in, while the first sync starts.
- [Superhuman: "Tell us a bit about yourself"](https://mobbin.com/screens/f8dcc467-e9c6-4deb-b933-69189c31bb66): a thin top progress bar spans the window, with underline-only inputs, a narrow ~380px column and a soft gradient wash. Take the progress bar and the narrow column.
- [Superhuman: practice inbox](https://mobbin.com/screens/144fbdb4-f999-4903-b0eb-1067daba2876): the real app is blurred behind a bottom sheet that reads "Hit [enter] to continue", using an inline key-cap. Use this for teaching Tab-to-accept with a sandbox field rather than explaining it.
- [Tana: mic and camera permission card](https://mobbin.com/screens/55dad85f-97df-4a49-9a76-cbc1ceddf713): a translucent card over a soft background. Each permission is one row with an icon, a bold name and a one-line *why*, and one primary button covers everything. This is the model for the Accessibility step.
- [Amie onboarding flow](https://mobbin.com/flows/f64326bd-25e6-48a3-a2bf-7c749ef1f218): a split card, with the form on the left and an illustration on the right on a grey canvas. "Connect accounts / You can add more later in Settings" lowers commitment. A footer row offers "Request other mail clients".
- [Clay: linking a Google account](https://mobbin.com/flows/136d29ec-f8bc-46ed-8c93-d362d338a14b): before handing off to Google OAuth, a dialog shows per-scope toggles (Calendar, Email) and the line "Clay cannot edit or delete your data". A pre-OAuth reassurance like this reduces drop-off.
- Sign-in: [Searchable](https://mobbin.com/screens/f9fb2da0-0199-4208-b478-2837aa62494c) has a serif headline ("Let's get you cited.") on cream with stacked Google and magic-link buttons. Close to Heed's palette. [Vanta](https://mobbin.com/screens/ffbe5cda-fe8f-44eb-a38e-d51fac61a2b7) confirms a sent magic link with a toast ("A magic link has been sent to your email!") and doesn't navigate away. [MagicPath](https://mobbin.com/screens/04dfddcb-83de-444e-a878-4e84589d4fbc) puts "We'll send you a magic link to sign in" under the button as helper text.

**Heed should**

- **Sequence:** Welcome, then Sign in (Google first, magic link second), then Connect (Gmail + Calendar as one Google step; Slack optional), then Accessibility permission, then Teach Tab (sandbox), then First brief generating (agent-progress card), then the aha (first brief with 3 cards). Keep it to 6 screens or fewer and show a 2px ultramarine progress bar at the very top.
- **Layout:** one white card, 440px wide, 32px padding, radius 16, centered over full-bleed sky or painterly art. Fraunces 32/1.1 light headline, Geist 15/1.5 body in ink at 70%. Use one primary ultramarine button only, with "Skip for now" as a text link.
- **Magic link:** after sending, swap the card in place to "Check your inbox" with the email address in bold. Show "Open Gmail" (deep-link) and "Resend in 30s", the countdown set in Geist Mono. Error: "That link expired. Send a new one."
- **Accessibility card:** three rows: *What it does* ("Lets Heed suggest text in any app"), *What it doesn't* ("Never reads password fields; nothing leaves your Mac without you"), and *How* ("System Settings → Privacy → Accessibility"). The primary button reads "Open System Settings". Poll for the grant and auto-advance with a ✓ when it arrives; don't make the user click Continue. Secondary: "I'll do this later". Ghost text then stays off, and a reminder card goes into the brief.
- **The aha:** don't show an empty home. Run the agent-progress card ("Reading 30 days of email · Finding open loops · Writing your brief") and reveal the brief with a 400ms fade-up. The first card should be something real that's waiting on the user.
- **Keys:** ↵ continues on every step and Esc goes back. Show the ↵ key-cap inside the primary button.

## 2. Home / daily brief

**References**

- [Rox: Home](https://mobbin.com/screens/62b4d10c-498e-49eb-bf78-3b944290f407): "Afternoon, <name>" with "Here's your focus for today" as a large two-line greeting. Below it, "Recommended Actions (7)" are rows with source logo, bold title, one-line summary, a tag and relative time. A right rail holds Today's calendar and Tasks with a dotted-grid empty state. The overall structure is a near-perfect match.
- [Town (site): app hero](https://mobbin.com/sites/sections/a6486bf8-a881-4bf5-a61b-13dd87b89e95): the headline is a sentence about the user's state ("several threads across work and planning are waiting for your attention this evening"), followed by "Time-sensitive" rows and a floating ask bar. Take the sentence-as-headline approach.
- [Fireflies: Home](https://mobbin.com/screens/cb1796cb-0cb7-4abb-a56d-0e5937a344a7): a sky-photo band behind "Good Evening, Alex" with white stat cards overlapping the band edge. A sticky "Ask anything from your meetings…" bar sits at the bottom. Take the cards overlapping the imagery.
- [Microsoft Copilot: Discover](https://mobbin.com/screens/5b0ca1bf-3ae7-45f1-8a98-a4b0acb7bb91): big-radius cards on warm off-white with "6 sources" favicon stacks, a "Copilot Daily · 3 min" audio brief card, and a floating composer. Take the favicon stack for citations and the idea of an optional listen-to-brief.
- [Dropbox Dash: answer with chips and follow-ups](https://mobbin.com/screens/fc54be30-29ff-430c-9554-7858c47373e8): file chips sit inline after each bullet. "Ask a follow-up" shows 3 pill questions. A "1 source" stack and a privacy footnote under the composer.

**Heed should**

- **Hierarchy:** a date eyebrow in Geist Mono 12, caps, 60% ink. Then the Fraunces headline (SOFT 100, weight 300, 44/1.05) as one sentence about the day. Then brief prose of 3–5 sentences at Geist 17/1.6, max 640px, with inline citation chips. Below that, the cards grid, then Suggested questions as 3 pills.
- **Citation chip:** 20px tall, radius 999, 1px hairline border, app glyph at 12px plus a short label ("Jane · Gmail"). Hover shows a 320px preview card with sender, timestamp and a 2-line excerpt. Click opens the source in its app. The ⌥-click alternative opens it in a side panel.
- **Cards:** white, radius 16, 20px padding, at most 2 columns at ≥1100px. Each card has a source glyph, a title (Geist 15 semibold), a one-line why, and at most 1 primary action. Dismiss (×) appears on hover only.
- **States:** Loading shows skeleton prose (3 lines at 92/84/60%) with a shimmer under 1.2s, then a cross-fade. Stale data adds "Updated 7:02 · Refresh" in Mono at the top right. Error shows the last good brief plus an amber line ("Couldn't reach Gmail, showing this morning's brief. Reconnect"). Empty is covered in §11.
- **Keys:** J/K move between cards, ↵ opens, E does the primary action, ⌫ dismisses, / focuses the ask bar.

## 3. Needs you: the approval queue

**References**

- [Rox: draft email side panel](https://mobbin.com/screens/3d205a5d-5ead-4787-af2c-9b0a36ed2fc7): the chat on the left shows a compact "Draft Email · Preview ›" card, and the right pane is a real email editor (To, Subject, body) with formatting and "Connect Email". Take the compact proposal card that expands into the real artifact.
- [Attio: chat-drafted email card](https://mobbin.com/screens/f56c62bf-1ca6-4fa5-bd1d-f9f0baf7cc52): a one-line "1 tool used ›" disclosure above an email preview card (subject plus first line), then "You can review and edit it before sending." Below sit three quick refinements: "Make it more concise", "Add specific action items" and "Draft email to another recipient". Take the quick refinements as edit shortcuts.
- [Customer.io: Approve / Change rail](https://mobbin.com/screens/cf81940a-6915-4481-9893-c9a27f490081): the artifact renders at full fidelity in the centre. The right rail has "Approve" and "Change" side by side, with an activity log ("Sam Lee approved this a few seconds ago"). Take the paired buttons and the audit trail.
- [Linear: Inbox with snooze menu](https://mobbin.com/screens/17c8ce50-57d6-4a36-aeba-700bcb563fc6): a two-pane list and detail layout. Snooze has a natural-language field ("Try: 4 pm, 2 days, in 5 weeks…") above presets with resolved dates on the right ("Tomorrow, Thu 9 Apr 9:00"). Take this snooze menu exactly as a pattern.
- [Front: personal draft over inbox](https://mobbin.com/screens/4e61d3ab-758d-441f-8d9a-cdc58ad3c548): a "Personal draft · Only visible to you" banner, an inline AI toolbar (Adjust tone, Fix grammar, Expand, Shorten) and a split "Send & archive ▾" button.

**Heed should**

- **Layout:** the list is 360px wide; rows are 56px with an app glyph, a verb-first title ("Send reply to Jane"), a target in Mono ("jane@acme.com · in 2h") and a risk dot (none / low / review). The detail pane shows the artifact rendered as it will appear: an email with headers, a calendar invite with time block and attendees, or a Slack post with channel and avatar. Above it sits a 1-line *why* ("Jane asked for the deck by Friday; you said yes in Slack [chip]").
- **Diff mode:** when Heed edits an existing thing (an event moved, a doc changed), show before and after with struck-through removed text and ultramarine 12%-tint inserted text, like Notion Mail's inline diff (see §8).
- **Action bar**, pinned bottom and 64px tall: **Approve** (primary, ⌘↵), **Edit** (E, which makes the artifact editable in place without opening a modal), **Reject** (⌫, with an optional reason chip: "Wrong person", "Not now", "Tone"), then a ⋯ menu for **Snooze** (H, Linear-style menu) and **Dismiss** (D).
- **After approve:** the row collapses with a 180ms height animation and a toast reads "Sent to Jane · Undo" (6s undo window; actually send at T+6s). The next item auto-selects.
- **States:** Empty is "Nothing needs you." with a single engraved illustration. Expired shows a grey "Window passed" pill, with Approve disabled and Dismiss available. Failure keeps the row and adds an inline red line ("Gmail rejected the send: reconnect") with a Retry button.
- **Batch:** with ⇧-select, a floating bottom bar appears ("3 selected · Approve all · Snooze · Dismiss"), as in [Linear's selection bar](https://mobbin.com/screens/c61980d9-a5a7-4ccf-aac8-b3ba125e299a).

## 4. Agent and sync progress

**References**

- [Lindy: agent step list](https://mobbin.com/screens/9f4affd5-f387-4149-860e-95c83f9bbba5): a checklist of completed steps (✓ + label + chevron to expand) ending with a bold "Processing…". A collapsible "Use Computer" tool card sits at the end. Take the step rows with a chevron to expand detail.
- [Relevance AI: "Running automated checks"](https://mobbin.com/screens/d0dc629f-1593-4454-bc1c-2f759c5777ff): a modal with a soft gradient header, a title plus "Please wait while we validate…", and 5 named steps, each with its own spinner.
- [Juicebox: "Your agent is sourcing for you"](https://mobbin.com/screens/421d4c19-563c-4f3d-aa71-56f19cc50c39): a serif headline, four short lines of what will happen and when, a thick progress bar with % in Mono, and a "View message history" link. Close to the Heed tone.
- [Contractbook: background import](https://mobbin.com/screens/f4654d56-94ee-455f-93a7-afd17b24b2ce): a top-bar pill reads "Importing… (33%)" and stays visible app-wide. A sidebar item mirrors it, and an info banner reads "You'll be notified via email once the import is complete." Take the persistent global pill for background work.
- [Attio: workflow runs](https://mobbin.com/screens/ac501533-24e3-49b1-a564-cf0ec2a1b145): nodes on a canvas with "Completed" badges, and a right rail with KPIs (In progress / Completed / Failed / Avg runtime) and a run history list. Use this for a future "Activity" view.

**Heed should**

- **Agent-progress card** (as on the moodboard): white card, 20px padding. A title in Geist 15 semibold ("Drafting replies to 4 threads"), a 4px progress bar (ultramarine on 8% ink, radius 999) with "2 of 4" in Mono on the right, then a step list.
- **Step row**, 28px tall: icon (✓ in ultramarine / 14px spinner / hollow ring at 30%), label, and on the active row a muted live sub-label ("Reading thread with Jane…"). Done rows have a chevron that expands to the tool output.
- **Footer:** **Pause** (secondary) and **View draft** (primary, enabled once the first artifact exists). While paused, the bar turns 40% and the button changes to Resume.
- **Background sync:** a tray and titlebar pill reads "Syncing Gmail · 62%". Clicking it opens a popover with one row per app. Never block the UI.
- **Durations:** under 2s, show nothing. 2–10s, show the spinner with step labels. Over 10s, show the progress bar and an ETA in Mono ("~1 min"). Past 5 min, collapse to the pill and notify on completion.
- **Error:** the failed step gets a red ring and the inline reason, with Retry step / Skip / Cancel. Completed steps are never lost.

## 5. Search

**References**

- [Whop: ⌘K with type tabs and counts](https://mobbin.com/screens/45edfe65-a5a2-4a15-8f2d-50760e0c5740): segmented filters (All · Businesses · People · Chats · DMs), group headers with counts and "See all", matched text highlighted, and a footer showing "9,525 results found" next to ↑↓ Navigate · ↵ Select · esc Close.
- [Juicebox: grouped palette](https://mobbin.com/screens/84169c57-5cd6-4495-bb71-93d37266f4c4): small-caps group labels, an accent left-bar on the selected row, and a footer legend with "Tab to jump sections". Take Tab to jump between groups.
- [Magnific: "Press Tab to Ask"](https://mobbin.com/screens/6ad219c3-e308-4d95-b4dc-70859de1e02d): a chip inside the input turns the query into an AI question, and Quick actions show shortcuts on the right. This is the exact bridge between search and quick ask.
- [Dropbox Dash: search-as-you-type across apps](https://mobbin.com/screens/151b0b43-dc4d-464f-8e33-daebd2c53199): each result has an app glyph, the bold match, "· Updated 2 days ago" and a collection chip. The footer offers "/new Create".
- [Gumloop: typed sections](https://mobbin.com/screens/97f3400e-39b6-469b-b208-addb2d51e850): Teams / Agents / Skills / Files / Links groups with breadcrumb results ("Personal › …") and "Open ↵" on the selected row.

**Heed should**

- **Palette:** 640px wide, max-height 520px, radius 16, centered at 20vh. The input is 52px with Geist 17. Each row is 44px: app glyph, title with the match in ink and the rest at 70%, a meta line in Mono ("Gmail · Jane Doe · 3d"), and "↵" shown on the selected row only.
- **Groups**, in order: Top hit, People, Mail, Events, Messages (Slack), Docs, Actions. Show at most 3 per group with "See all (n)".
- **Filters:** typed prefixes (`from:`, `in:gmail`, `before:`) render as removable chips in the input. A segmented bar sits under the input.
- **Ask bridge:** if the query looks like a question (it ends in "?" or starts with who/what/when), the first row is "Ask Heed: '<query>'". Tab always switches to Ask mode.
- **States:** empty input shows Recent (5) and Suggested actions. No results reads "Nothing in your connected apps for 'x'" with "Ask Heed instead ⇥". Indexing in progress shows "Still syncing Slack, so results may be incomplete" in a subtle amber.
- **Keys:** ⌘K opens, ↑↓ move, Tab moves to the next group, ⇧Tab to the previous, ↵ opens, ⌘↵ opens in its native app, ⌘C copies a link, Esc clears and then closes.

## 6. Connections

**References**

- [Perplexity: Connectors gallery](https://mobbin.com/flows/eb4d488b-02f4-48f9-bbe7-d72715fc1e67): a title plus "Connect services so Perplexity can access and act on your data". Filter pills (Discover · All · Connected · Available), a search box, and 3-column tiles (logo, name, one line). Once connected, a "Connected" section rises to the top with account email and ⋯. The tile layout is the main thing to take.
- [Deel: per-app (Slack) page](https://mobbin.com/screens/7b7a9f07-cad9-4c17-807a-d9e2df6f45ac): a big app logo and name, a "Connected" pill, "Synced: Dec 17th, 06:22 PM", a "Sync Slack" button, and an amber banner with a count of the problem and a fix button ("Match accounts").
- [Customer.io: integrations table](https://mobbin.com/screens/e95bd92b-7d41-4dff-aa5d-3eef55514906): status pills (Error / No recent data), a data-flow direction badge (Data in / Data out) and Last activity.
- [Sana: integration detail](https://mobbin.com/screens/cb148035-69d7-4c1b-ada8-a845fd5ef540): an asset table with status filters (Processing issue · Too large · Unsupported format) and a side card with Connected by, created date, Status "Syncing" and Access.
- [Motion: connect calendars](https://mobbin.com/flows/afb6188a-6313-41c8-a2b6-c19e7f876c8d): a single "Connect account" button that drops a provider menu, plus "We'll never sell or share your data." right above the button.

**Heed should**

- **Gallery:** Connected first (a rich row for each), then Available as tiles (3 columns ≥1000px, 2 below; each 88px tall with a 32px logo, name and one-line value "Brief, search and drafts from your inbox"). Add filter pills and search.
- **Connected row:** logo, name and account in Mono, a **health pill** (● Healthy / ● Syncing 62% / ● Needs attention / ● Paused), "Synced 4 min ago", and ⋯ (Sync now, Pause, Permissions, Disconnect).
- **Connect flow:** a pre-OAuth sheet lists "Heed will read / Heed will draft / Heed never sends without you" as three rows. The sheet's button is "Continue with Google", which opens the Composio/Google OAuth. On return, the sheet becomes the agent-progress card ("Importing last 30 days").
- **Detail page:** a header with a status pill, a scope list with toggles (Read mail, Draft replies, Send after approval), data stats in Mono (messages indexed, last error), and a danger zone ("Disconnect and delete Heed's copy").
- **Error:** an amber banner in the page and a red dot on the sidebar item, with a single fix action ("Reconnect Google"). Never use just a red badge without a verb.

## 7. Quick ask and chat with sources

**References**

- [Dropbox Dash: Home ask with follow-ups](https://mobbin.com/screens/fc54be30-29ff-430c-9554-7858c47373e8): inline file chips per bullet, "Ask a follow-up" pills, and a privacy note under the composer.
- [ChatGPT: deep research activity panel](https://mobbin.com/screens/73833b79-1dd5-4354-8fc4-a2e99c33a75e): an "Activity | 23 Sources" segmented panel on the right showing reasoning steps and "Read <domain>". Domain chips sit inline in the prose and a favicon stack + "Sources" pill closes the answer.
- [Gemini Notebook: numbered citations](https://mobbin.com/screens/05def46a-970f-46b2-8bf0-e0e83c7dd49e): small numbered bubbles after each claim, a checkbox list of sources on the left, and "8 sources" shown in the composer.
- [Customer.io: references popover](https://mobbin.com/screens/fafcbfb9-fa85-4b4d-b17f-54df962305b5): "Thought for 2 seconds ›" and "Searched docs ›" disclosures, and a "5 references" popover with title, URL and ↗.
- [Perplexity: centered ask with autocomplete](https://mobbin.com/screens/e2a22c28-7919-4445-8f17-5d998344f92a) and [Rox: saved commands](https://mobbin.com/screens/299f4d12-0cb1-4693-beb8-ada8d98ad231): the ask box shows query completions below it, and saved prompt cards ("Prep for Meetings", "Recap The Week") work as one-click recipes.

**Heed should**

- **Quick ask overlay** (global hotkey ⌥Space): 680px wide, radius 16, white at 96% with a 24px backdrop blur over the desktop. The input row is 56px. Under it, show context chips for the frontmost app ("Using: Mail · thread with Jane", removable). The answer streams inline below and grows to at most 60vh, then offers "Continue in Heed ⌘↵".
- **Answer anatomy:** prose, inline chips after claims, then a "Sources (n)" stack at the end. Clicking it opens a right panel listing each source with glyph, title, time and a 2-line excerpt. If tools ran, show one collapsed "Checked Gmail, Calendar ›" line *above* the answer, not a wall of reasoning.
- **Actions under the answer:** Copy, Insert into <app> (when invoked from an app), Make it a draft (which sends it to Needs you), plus thumbs.
- **Follow-ups:** 3 pills, plus saved recipes in the empty state ("Prep my next meeting", "What did I promise this week?").
- **Keys:** ⌥Space toggles, ↵ sends, ⇧↵ adds a newline, ⌘↑ edits the last question, ⌘C copies the answer, Esc closes (the draft persists for 10 minutes).

## 8. Inline ghost-text autocomplete

Mobbin has few true ghost-text captures, because the pattern lives in desktop editors (Cursor, Copilot) that Mobbin doesn't catalogue well. The references below are the nearest real patterns for the **hint**, the **accept/discard controls** and the **diff**.

**References**

- [Delphi: "TAB" key-cap inside the composer](https://mobbin.com/screens/f224b7d4-a925-45a8-b0b0-26b0eb74f3ec): a quiet uppercase TAB badge at the right edge of the input marks that a completion is available. This is the minimal acceptance hint.
- [Aside (site): "Tab to switch" inside the ask bar](https://mobbin.com/sites/sections/bdad14df-7d77-4d9d-8c30-dfd826d134d1): a key-cap with a 3-word label inline in the field. It's the same idea in a desktop-AI context.
- [Notion Mail: inline AI rewrite with Accept / Discard / Try again](https://mobbin.com/screens/c76eb3bd-f70d-4235-baf4-f24c06421205): removed text is struck through in grey, inserted text is tinted, and a small menu sits under the cursor (✓ Accept, × Discard, ↻ Try again). This is the model for multi-word or rewrite suggestions.
- [Superhuman: "Write with AI" onboarding](https://mobbin.com/screens/3588719c-3ac1-4bd3-9996-f366f2c0241e): it teaches "sounds like you" by collecting role and company first, and floating pills (Shorten, Simplify, Rewrite in my voice) show what the AI can do.
- [Magnific: "Press Tab to Ask Magnific"](https://mobbin.com/screens/6ad219c3-e308-4d95-b4dc-70859de1e02d): the Tab affordance is a bordered chip with an icon, which reads more clearly than text alone.

**Heed should**

- **Ghost text:** the same font as the host field, ink at 38% opacity (in dark mode, cream at 40%). No underline and no background. Appear only after a 350ms typing pause, and never mid-word unless the confidence is high.
- **Hint:** the first 5 times, and then whenever the user hovers, show a 18px floating key-cap pill just after the ghost text: `Tab` (accept) · `⌥→` (word). After that, the ghost text alone is the hint.
- **Partial accept:** ⌥→ accepts the next word and ⌘→ accepts to the end of the line. Tab accepts everything. Typing a character that matches the ghost consumes it without dismissing.
- **Dismiss:** Esc, typing a non-matching character, or moving the caret. After 3 dismissals in a row in the same app, back off for 10 minutes and say so once in the tray ("Pausing suggestions in Notes for a bit").
- **Provenance:** on hover or ⌥, a 280px tooltip shows "From: Jane's email, Tue [chip] · Calendar: Fri 3pm". If there is no source: "Based on your writing style." Never show provenance unprompted.
- **Per-app control:** the menubar has "Suggestions in <frontmost app>: On / Off" with ⌃⌥G.
- **Never** show suggestions in password, payment or secure-input fields, and show nothing while screen-sharing (detect it and pause, with a tray note).

## 9. Account, plan, billing, usage, upgrade, trial

**References**

- [Rox: Billing & Usage with upgrade modal](https://mobbin.com/screens/969b438c-3312-4bc2-bbaa-90a5a8c1e2d2): a summary strip (Current Plan · Cost · Renewal Date · Usage Level bar "1,535 / 1,000 Actions"), a usage chart, and a small confirm modal that makes the new price and the immediate billing explicit. A top banner reads "You have no Agent Actions remaining."
- [Langdock: Billing with trial row](https://mobbin.com/screens/bbde540f-a255-4566-bda1-7f455791a952): "Free" with a "Trial" badge, credits used in plain numbers, a "Trial · Ends on 4/27/26 · Upgrade ↗" row, and plan cards with a Monthly/Annual "20% off" toggle.
- [Kajabi: plan usage meters](https://mobbin.com/screens/328744fa-7f77-4991-a1bb-ac62d6ce1bfd): a grid of meters, each with "x of y" and "n remaining". A meter at its limit turns red with "(limit reached)" and an inline "Upgrade plan" link.
- [Docusign: "Your trial ends in 3 days"](https://mobbin.com/screens/8eac7e3e-bec5-4243-b237-b2f78832147a): a modal with an Annual/Monthly segment and per-plan "Switch to annual to save 44% ($240 in total)" in green. "View All Plans" and "Next".
- [Slite: trial-ends modal](https://mobbin.com/screens/1da3d0dc-a8a8-47e8-aca5-e10f7d761e9a): "Your free trial ends in 13 days". The current plan has a "Current free trial" tag, and the sidebar also shows a quiet trial meter.

**Heed should**

- **Billing page:** a summary strip of Plan (Heed / Heed Max) · £39 or £99/mo · Renews 14 Dec · Payment •••• 4242 (all in Mono). Below it, usage meters: Agent actions, Drafts and Ghost-text suggestions (Max: "Unlimited"). Each meter is 6px, ink at 80% for the fill; at 80% it turns amber with "Running low"; at 100% it turns red with "Upgrade to Max for more".
- **Upgrade Heed → Max:** a side-by-side diff of only what changes (3–5 rows), then "£99/mo, prorated today: £41.20" in Mono. The button is "Upgrade to Max". After upgrading, show a toast and a short confetti-free ultramarine shimmer on the plan name.
- **Trial ladder:** days 14–4 show a sidebar footer row ("Trial · 9 days left") and nothing else. Days 3–1 add a dismissible brief card ("Your trial ends Friday. Keep Heed"). Day 0 shows a full modal over a blurred app with both plans. Read-only mode keeps the brief visible but locks actions.
- **Checkout handoff:** "Continue to secure checkout" (Stripe), with "You'll come back here" as helper text. On return, show the "You're on Heed Max" card.
- **Cancel:** show what they'll lose using *their* numbers ("312 drafts, 41 approvals this month"), offer to pause for 1 month, then confirm. No guilt copy.

## 10. Settings: privacy, data, delete account, devices

**References**

- [Motion: Privacy page with type-to-confirm](https://mobbin.com/screens/95f76425-db9e-4e88-96f3-c1f854d876ed): two rows only, "Download your data / Request data" and "Delete information / Delete data". The confirm requires typing "CONFIRM DELETE" and links to the refund policy.
- [Greptile: type DELETE](https://mobbin.com/screens/11692b68-9278-4637-8c80-b47d194ee043): "This action is permanent… All your data, linked accounts, and preferences will be deleted." A "Danger Zone" section sits at the bottom of the page with a muted red button.
- [Hume AI: privacy toggles plus delete](https://mobbin.com/screens/85860faa-5a54-418b-b188-83ced4c93b90): "Zero data retention" and "Opt out of data training" toggles sit above a separate Delete account card.
- [Etsy: delete with "Nevermind, keep my account"](https://mobbin.com/screens/7c4dc1db-e59e-4ec9-89d0-fe33545f13a3): the safe option is the filled, primary-looking button. It also states that deletion takes up to two weeks.
- Devices: [Revolut: "Log out your devices"](https://mobbin.com/screens/84d28ae7-c75a-4f39-879d-8c25af96abbe) shows device rows with "This device", last-active time and location, and checkboxes. [Podia: sessions](https://mobbin.com/screens/4ab27657-e4b0-45cf-b3a5-8bf64bdf75bf) shows "Chrome on Mac 10.15.7 · Nov 12 at 4:52 AM" with ×.

**Heed should**

- **Privacy & data**, in this order: (1) *What Heed stores*, a plain-language table (Email bodies: on device · Embeddings: on device/cloud · Drafts: 30 days); (2) toggles for "Use my data to improve Heed" (**off by default**) and "Keep chat history"; (3) Export (JSON + Markdown, emailed link); (4) per-connection Forget (deletes Heed's copy of one app's data).
- **Delete account:** a danger-zone card at the bottom. The dialog lists exactly what is deleted, offers "Export first" as an inline link, and requires typing your email (not DELETE). The cancel button is visually primary and reads "Keep my account". If billing is active, add "Your subscription ends today; no further charges."
- **Devices:** rows show a device glyph, "MacBook Pro · This Mac", "Last active 2 min ago · London" and Sign out. Above the list sits "Sign out of all other devices".
- **Ghost-text privacy:** a blocklist of apps where suggestions are off (password managers are pre-filled), and a "Pause everything for 1 hour" option.

## 11. Empty states, toasts, tray / menubar notifications

**References**

- [ClickUp: Inbox Zero plus undo toast](https://mobbin.com/screens/c64ec457-7285-4a1f-886d-a149b8a0d49b): a centered small illustration with "Inbox Zero" and a one-line congrats. The dark toast at the bottom-left reads "Notification cleared · Undo ⌘Z" with the shortcut shown.
- [Front: "Mission accomplished!"](https://mobbin.com/screens/d45e3c73-35a6-4917-b5fa-50c84e4b65d2): a two-level empty state, with "Inbox zero / No conversations" in the list and a bigger illustration plus "Sit back and relax… until the next message" in the detail pane.
- [HubSpot: "You're all caught up."](https://mobbin.com/screens/55e0d664-417f-45d0-93ed-e8be79200ee0): a line illustration (person in a lounger), the title in a light weight and a muted sub-line.
- [Grok: restored toast with "View"](https://mobbin.com/screens/b9b0f1cf-4d6d-4634-a662-e30d3ededdb1): a top-centered pill toast with an icon, message and a secondary "View" action.
- [GitBook: "Welcome back!" restore toast](https://mobbin.com/screens/9e06cd74-e5a9-4913-b907-5fc7476065be): a bottom-right card toast with a title and body, where the title is warm and the body is precise.

**Heed should**

- **Empty states:** one engraved/dither single-blue illustration (max 160px), a Fraunces 24 light title, one Geist 15 line, and at most one action. No confetti. Give each surface its own line (see the microcopy appendix).
- **Toasts:** bottom-centre in the main window, 40px tall, radius 999, ink background with cream text (in dark mode, cream background with ink text). Layout is icon, message, then an action ("Undo", "View"), with the key-cap ⌘Z shown. They auto-dismiss after 6s, pause on hover, and stack at most 3. Success toasts use no green; the ✓ is ultramarine.
- **Menubar / tray:** a glyph-only icon with a 6px ultramarine dot when something needs you. The popover is 360px wide, with "Needs you (3)" rows offering inline Approve / Open, a sync status line in Mono, a "Pause suggestions" toggle and an "Open Heed" button.
- **System notifications:** only for Needs-you items with a deadline under 2h and for finished long tasks. The title is the verb ("Reply to Jane is ready"), the body is the why, and the actions are Approve / Open. Batch into one notification if more than 3 arrive within 10 minutes.

## 12. Website

**References**

- **Hero, AI product:** [Aside](https://mobbin.com/sites/sections/bdad14df-7d77-4d9d-8c30-dfd826d134d1) puts a cloud-sky background behind a two-line headline, one "Download for macOS" pill, and the real app window floating over the sky. It's almost exactly the founder's "white cards over sky" direction. [Town hero](https://mobbin.com/sites/sections/bbcbaabb-fcaf-45f2-acd4-13c24ffd23ed) uses a serif display headline with app logos orbiting and a "Meet your Townie" CTA.
- **Product-in-context hero:** [Town "a keystroke away"](https://mobbin.com/sites/sections/a6486bf8-a881-4bf5-a61b-13dd87b89e95) shows a macOS window on a painterly blue backdrop, with the in-app headline as the hero copy. [Square AI](https://mobbin.com/sites/sections/4d088f68-033d-4090-8176-e40bfadfb673) uses a light serif "Make smart decisions, fast" over a warm photographic scene, with a small caps eyebrow.
- **Feature band / how it works:** [Symbolic](https://mobbin.com/sites/sections/2fc41d06-f203-4934-973e-aa25289d0f1a) is closest to the Heed palette: three cream cards, each holding a mini UI vignette, with "1. Research / 2. Write / 3. Publish" and a serif headline. [Browserbase](https://mobbin.com/sites/sections/687d0833-191b-4fba-8963-368492c97a10) has three numbered steps over *dithered pixel landscapes*, so take its dither treatment. [Opennote](https://mobbin.com/sites/sections/3d883373-ca42-4eed-a07e-d929c6daeaae) uses a serif headline on cream with engraved/sketch illustrations scattered around it. [Kastle](https://mobbin.com/sites/sections/a14eaef1-524c-4927-bc6f-ab54dd2aecff) has a "HOW IT WORKS" mono pill eyebrow over a light serif.
- **Privacy section:** [Dovetail](https://mobbin.com/sites/sections/28d71638-531f-486e-93e4-ce96407a5ea7): "One last thing" with three cards of single-colour line illustrations and one claim each (no training on your data, model choice, own infrastructure). [Shop](https://mobbin.com/sites/sections/581a9bd7-9e53-487a-a8ad-1f63b99768d1) has a 2×2 grid of big headlines ("Personalized and private") with links, including "Delete your account →", right in the marketing page. [Serus](https://mobbin.com/sites/sections/d674ddba-26f4-40ad-a39f-f710227c0085) pairs "Privacy, by design." with honest badges ("Certification in progress").
- **Pricing:** [Jasper](https://mobbin.com/sites/sections/fcc5ee8c-19a1-4350-b3b1-e046f5220b54) uses a big segmented Monthly / Yearly toggle with sub-labels ("No contracts, cancel anytime" / "Save ~20%"). [Jasper, serif variant](https://mobbin.com/sites/sections/fef23146-6a20-4575-8a24-78cc1e6777e0) sets the price in Mono-ish type ("$39 month/seat"). [Town compare plans](https://mobbin.com/sites/sections/877510f7-314b-4c5c-9926-f098efaced15) has a "Pay Annually · Save 20%" pill and a cream comparison table. [Opennote](https://mobbin.com/sites/sections/fc48effd-cfc9-4f70-bc45-8a559e4baba1) combines a serif "Pick the plan that suits you best", engraved plant illustrations per tier and a ✓/× table.
- **Signup:** [Searchable sign-in](https://mobbin.com/screens/f9fb2da0-0199-4208-b478-2837aa62494c) (cream + serif) and [MagicPath modal over the landing page](https://mobbin.com/screens/04dfddcb-83de-444e-a878-4e84589d4fbc) (sign-up as a modal that keeps the hero visible behind it).
- **Footer:** [Structured](https://mobbin.com/sites/sections/cf954a98-4f2e-41d3-b2a1-168e4ba0108d) uses a full-width serif wordmark as the footer, with tiny caps links. It's ideal for a Fraunces "Heed". [The New Yorker](https://mobbin.com/sites/sections/84724b3c-961d-4245-a46a-f176fe4bec67) has an engraved seal and calm columns.

**Heed should (WEB1)**

- **Hero:** full-bleed sky/painterly art, a nav in a cream pill, and a Fraunces headline at 72–88px, light, max 2 lines, with WONK on for one word at most. A one-line sub, then "Download for Mac" (ultramarine) and "See how it works" (text). The real app window floats at 1100px and overlaps the fold by 120px. Inside the window, show the brief headline and an agent-progress card mid-run.
- **Feature band:** three cream cards, each with a live mini-vignette (a brief with chips, a Needs-you approve row, ghost text being Tab-accepted) and a Mono "01 / 02 / 03" eyebrow.
- **How it works:** Connect → Heed reads and drafts → You approve. Each step gets a dither single-blue illustration, as in Browserbase's dither but monochrome ultramarine.
- **Privacy:** a full-width ink-blue (`#0B0B1A`) band with 3 claims and line illustrations, plus "Read the privacy page →" and "Delete anytime →" links, like Shop.
- **Pricing:** two cards, Heed £39 and Max £99, with the Max card in ink-blue. The toggle reads Monthly | Annual · 2 months free (with the save badge inside the segment). The price is in Geist Mono and "14-day free trial, no card" sits under the CTA. The comparison table follows, then an FAQ.
- **Checkout handoff:** CTA, then email (if not signed in), then Stripe Checkout, then a "Heed is downloading" page with the 3-step install (drag to Applications → open → sign in) and a manual download link.
- **Footer:** an oversized Fraunces "Heed" wordmark (SOFT 100, ~22vw) in ultramarine on cream, with small Geist caps links above it and a © line in Mono.

---

## Appendix A: Motion

Mobbin shows static captures, so the durations below are **targets**. They're based on how these apps feel in hands-on use, and DS1 should verify them in the live apps before locking tokens. Use one easing: `cubic-bezier(0.2, 0.8, 0.2, 1)` (ease-out-quint-ish) for entrances, and `cubic-bezier(0.4, 0, 1, 1)` for exits.

| Transition | Who does it well | Target | Notes for Heed |
|---|---|---|---|
| Command palette open | Linear, Raycast | 120–150ms, scale 0.98→1 + fade | No backdrop blur animation; blur is instant. Close in 100ms. |
| Palette row selection | Raycast, Linear | 0ms (instant) | Highlight must never lag the keyboard. |
| Toast in / out | Linear, Vercel | 200ms in (y +8→0), 150ms out | Undo window 6s; progress is implied, not drawn. |
| Approve row collapse | Superhuman (archive) | 180ms height + 120ms fade, next row auto-selects | Superhuman's "done" speed is the benchmark: it should feel faster than reading. |
| Brief reveal after generation | Granola (notes "enhance") | 400ms fade-up 8px, stagger paragraphs 60ms | Only on first reveal, never on revisit. |
| Agent step tick | Lindy, ChatGPT agent | ✓ draws in 220ms; bar eases 300ms | Don't animate pending rows. |
| Side panel (sources, detail) | Linear, Notion Calendar | 220ms slide from right 24px + fade | Content stays put; panel overlays at <1100px. |
| Onboarding step change | Superhuman, Arc | 300ms cross-fade, card stays fixed | Background imagery does a very slow 20s Ken Burns at most. |
| Ghost text appear | Cursor, GitHub Copilot | 80ms opacity 0→0.38 | Disappear on reject in 0ms. Never slide. |
| Quick-ask overlay | Raycast, Spotlight | 140ms scale 0.97→1 + fade; answer height animates 160ms | Respect Reduce Motion: fade only. |
| Upgrade success | Linear (plan changes are quiet) | 600ms single shimmer across the plan name | No confetti. |

Reduce Motion: replace all translations and scales with 120ms opacity fades. Keep progress bars, since they carry information.

## Appendix B: Microcopy (inspiration only; never copy verbatim)

Patterns seen, then Heed-voice lines written fresh. Tone: calm, specific, never chirpy. Sentence case.

**Brief**
- Seen: a sentence about the user's state as the headline (Town), "Here's your focus for today" (Rox).
- Heed: "Three people are waiting on you. One of them is Jane." / "A quiet morning. Two things are worth five minutes." / "You said you'd send the deck by Friday. Today is Thursday."
- Suggested questions: "What did I promise this week?" · "Who haven't I replied to?" · "Prep me for 2pm."

**Approvals (Needs you)**
- Seen: "You can review and edit it before sending." (Attio), "Personal draft · Only visible to you" (Front).
- Heed: "Ready when you are." / "Nothing goes out until you say so." / Toasts: "Sent to Jane · Undo" · "Snoozed until tomorrow 9:00" · "Dismissed. Heed won't suggest this again."
- Reject reasons: "Wrong person" · "Not now" · "Not how I'd say it".

**Progress**
- Seen: "Your agent is sourcing for you… This should only take a few minutes." (Juicebox)
- Heed: "Reading your last 30 days. You can leave this open or go do something else." / "Done. 4 drafts are waiting in Needs you."

**Empty states**
- Seen: "Sit back and relax… until the next message" (Front), "You're all caught up." (HubSpot)
- Heed: Needs you: "Nothing needs you." · Search with no results: "Nothing in your apps for that. Ask Heed instead?" · Connections: "Heed gets better with every app you connect. Start with your inbox." · Chat: "Ask about anything in your mail, calendar or Slack."

**Permissions and onboarding**
- Seen: "We'll never sell or share your data." (Motion), "You can add more later in Settings" (Amie)
- Heed: "Heed suggests, you decide. It never sends on its own." / "Accessibility lets Heed finish your sentences in any app. It never reads password fields."

**Pricing and trial**
- Seen: "No contracts, cancel anytime" (Jasper), "Switch to annual to save 44% ($240 in total)" (Docusign)
- Heed: "14 days free. No card." / "Annual: two months on us." / Trial: "9 days left in your trial." → "Your trial ends Friday. Everything you've set up stays." / Cancel: "You approved 41 drafts this month. Pause instead?"

---

## Conflicts and notes for the founder direction

- **No hard conflicts.** Every recommendation fits the fixed palette and type.
- **Watch-out:** the AI reference set leans heavily on purple and pink gradients (Superhuman, Relevance, Gemini, beehiiv). Don't let that leak in. Heed's single-ultramarine accent plus cream is already more distinctive than any reference here.
- **Watch-out:** success green is everywhere in the references (Langdock, Docusign, Customer.io). Heed should use ultramarine ✓ for success and keep green out of the palette, reserving amber and red for warning and error only.
- **Gap:** Mobbin has thin coverage of true ghost-text and menubar UI. §8 and §11 are partly first-principles, so validate with a 5-person hallway test of the Tab and ⌥→ hint.
