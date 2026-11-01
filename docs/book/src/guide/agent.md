# The agent and computer use

The agent carries out a multi-step task for you: you give it a goal, and it works through your apps
and browser one step at a time, under a [safety policy](safety.md) that you control.

## Running a task

On the **Agent** page:

1. Type a goal, for example "find the invoice from Acme last month and add its due date to my
   calendar".
2. Watch the **timeline**: each step shows what the agent saw and what it chose to do.
3. Answer **confirmations** when they appear.
4. Read the **outcome** and whether it was verified.

**Explain** on a past task turns its action log into a readable report. A task that was waiting for
you when the daemon stopped is not resumed; it shows as interrupted.

## What the agent can use

| Tools | What they do |
|---|---|
| Built-in | `finish` (end the task), `ask_user` (ask you a question), `note` (keep a working note) |
| Browser | Read a page, click, type, select, scroll, navigate, submit, wait. Only on sites you allowed in the Nugget browser extension |
| Computer use | Act on desktop apps through the accessibility APIs (elements, never screen coordinates). On by default; `computer_use = false` under `[agent]` turns it off |
| Plugins | Read-only tools from installed [plugins](plugins.md) |

**The browser extension** (Chrome, Chromium, Edge, Firefox 128+) has no network access of its own
and talks to the daemon only through a local native-messaging host. You allow sites one at a time
from its popup. While it acts, your browser shows an overlay with a **Stop** button.

## Confirmations

Every action is checked by the [safety policy](safety.md) first: **allow**, **confirm** or **deny**.
When an action needs your approval, a dialog shows its risk class, target and a preview. You can:

- **Approve once**.
- **Approve for this task**: this risk class on this target for the rest of the task (spending and
  security changes always count as approve once).
- **Deny**.

No answer within 2 minutes counts as a denial.

## Budgets and verification

A task stops at 30 steps, 10 minutes, 500 000 model tokens or 3 failed verifications in a row,
whichever comes first, plus per-class limits (see [Safety](safety.md#rate-limits)).

Every change the agent makes is checked afterwards. A task is **succeeded** only when every change
verified; otherwise it is **unverified** or **failed**, and the timeline shows which step.

## Prompt injection

Web pages, emails and app content can contain text written to steer an AI. Tool results are fenced
as data, and when an observation reads like instructions to the agent, it asks you before its next
edit-or-riskier action. This reduces the risk; it does not remove it. Keep risky actions on
**confirm**.

## Stopping it

- **Escape three times** (each within half a second) stops every agent: the
  [kill switch](safety.md#the-kill-switch). **Stop all agents** in the tray or on the **Safety** page
  does the same.
- **Cancel** on the Agent page stops one task.

## Privacy

Agent model calls use the `agentic` tier provider. With a cloud provider, the goal, what the agent
sees and any context from your data are sent there and recorded in the [egress ledger](privacy.md)
with feature `agent`. Every action is in the action log, with secure-field text and card numbers
redacted.
