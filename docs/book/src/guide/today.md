# Today: proactive cards and the morning brief

> **Status:** the **Today** page shows the brief and your cards, and every action below can be
> previewed and run from it. The daemon serves `cards.list`, `cards.feedback`, `cards.preview`,
> `cards.execute`, `brief.get`, `proactive.metrics`, `proactive.refresh`, `proactive.learning` and
> `proactive.reset_learning`.

Nugget reads your indexed mail, calendar and knowledge graph and suggests what needs your attention
as **cards**. Each card says why it is there and offers an action you can preview before anything
happens.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/screenshots/today-dark.png">
  <img src="../images/screenshots/today-light.png" alt="The Today page: the morning brief and a card suggesting a reply to an unanswered email, with the reasons it is shown and its actions" loading="lazy">
</picture>

## Where cards come from

Cards start from fixed, deterministic rules, not from a model:

| Card | When |
|---|---|
| Unanswered thread | Mail from a person that you have not replied to, between 1 and 14 days old |
| Commitment | A commitment in the knowledge graph (yours, or owed to you) due within 7 days, or overdue by up to 30 |
| Meeting prep | A meeting with attendees from outside your organisation in the next 48 hours |
| Renewal | A subscription renewal or trial ending within 14 days |
| Follow-up | Something you promised in sent mail in the last 14 days and have not done |

Newsletters, mailing lists and automated senders (no-reply addresses, automatic replies) never count
as unanswered threads.

Every card carries its **evidence**: the items and quotes it came from. **Why?** on a card opens
them, together with the top factors behind its rank.

## How cards are ranked

A small scoring model weighs urgency, importance (how strong your relationship with the people
involved is, from the graph and your mail history), freshness and the kind of card. It learns from
what you do: accepting, dismissing and snoozing cards adjusts the weights.

An optional **judge** model can add a short summary, a suggested action and a confidence to the top
candidates (8 by default). It cannot create cards and only nudges the rank. It uses a local model by
default.

The inbox offers at most 12 open cards at once.

## Acting on a card

Each card offers typed actions:

| Action | What it does | Risk class |
|---|---|---|
| Draft reply | Drafts a reply in your voice. You copy it or send it from your mail app; Nugget never sends it | Edit |
| Create event | Adds an event to your calendar (through CalDAV when you have a CalDAV source, otherwise as an `.ics` file opened by your OS) | Edit; Send when it has attendees |
| Reminder | Stores a reminder and shows an OS notification | Edit |
| Archive thread | Archives the thread, only for sources that support it (IMAP, Gmail) | Destroy |
| Research note | Writes a note into your vault | Edit |

Every action follows the same path:

1. **Preview.** Nugget prepares the action (for example drafts the reply) and shows a **dry-run
   diff** of exactly what would change.
2. **Approve.** Your approval is bound to a digest of that exact preview. If anything changes, the
   approval no longer matches and nothing runs.
3. **Policy.** The [safety policy](safety.md) is asked, as for any other action; actions of Send risk
   and above need a confirmation by default.
4. **Execute.**

Nothing runs without an approval of the preview you saw.

## Card states

Cards move through new, seen, snoozed, accepted, dismissed, done and expired. A dismissed, done or
expired card is remembered for 90 days so the same situation does not come back as a new card. An
open card whose situation resolves itself (you replied, the meeting started) is closed for you.
Cards without a natural deadline expire after 14 days.

## The morning brief

The **Today** page opens with a brief for your local date: the top cards, today's meetings (with
their prep cards and external attendees), overdue commitments and promises, renewals in the next
7 days, and the open cards.

## Metrics

Nugget tracks how useful its cards are: precision (accepted cards out of cards shown), time to
action, and why cards were dismissed. The **Today** page shows the precision; all of them are available through
`proactive.metrics`.

## Privacy

- Generating, scoring and ranking cards happen on your device.
- The judge uses a local model by default. A cloud model is used only if you allow it, and never for
  a card whose evidence is sensitive. Its requests are recorded in the
  [egress ledger](privacy.md) with feature `proactive_judge`.
- Draft replies follow the [Double Tap](double-tap.md) rules (feature `draft`).
- Creating an event over CalDAV or archiving a thread is traffic to that source's own server, recorded
  like its syncs.
- Source content in cards is treated as untrusted data in any prompt.
