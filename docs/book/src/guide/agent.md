# The agent and computer use

> **Status:** the agent runs from the **Agent** page, with browser tools (through the Nugget browser
> extension), computer-use tools for desktop apps and tools from your plugins, all under the
> [safety policy](safety.md). The daemon serves `agent.run`, `agent.cancel`, `agent.confirm`,
> `agent.answer`, `agent.list` and `agent.get`. A task that was waiting for your answer when the
> daemon stopped is not resumed; it shows as interrupted.

The agent carries out a multi-step task for you: you give it a goal, and it works through your apps
and your browser one step at a time, under a [safety policy](safety.md) that you control.

## Running a task

On the **Agent** page:

1. Type a goal, for example "find the invoice from Acme last month and add its due date to my
   calendar".
2. Watch the **timeline**: each step shows what the agent observed and the action it chose.
3. Answer **confirmations** when they appear (see below).
4. Read the **outcome** and whether it was verified.

Past tasks are listed in the history. **Explain** turns a task's action log into a readable report:
what was asked, every action, every decision and how each change verified.

## What the agent can use

| Tools | What they do |
|---|---|
| Built-in | `finish` (end the task, saying whether it succeeded), `ask_user` (ask you a question), `note` (keep a working note) |
| Browser | Snapshot a page, click, type, select, scroll, navigate, submit a form, wait for an element. Only on sites you have allowed in the Nugget browser extension |
| Computer use | Reading and acting on desktop apps through the accessibility APIs: observe a window, click, type, press a key, set a value, scroll, open an app, wait for an element. It acts on elements, never on screen coordinates. On by default; `computer_use = false` under `[agent]` turns it off |
| Plugins | Read-only tools from installed [plugins](plugins.md) |

### The browser extension

The Nugget extension (Chrome, Chromium, Edge, Firefox 128+) talks only to a native-messaging host
that relays to the daemon over the local socket. It has no network access of its own and opens no
ports. Nothing is granted at install: you allow a site from the extension's popup, one site at a
time. The extension acts in your own browser profile, shows an overlay with a **Stop** button while
it works, and only you can let it act again after a Stop.

## Every action is checked

Before every action, the agent asks the safety policy, which answers **allow**, **confirm** or
**deny**. A denied action is reported back to the model, which can try something else. The
[Safety](safety.md) page explains the rules and risk classes.

### Confirmations

When an action needs your approval, a dialog shows its **risk class**, its **target** (the app or
site) and a **preview** of what will happen. You can:

- **Approve once**: this action only.
- **Approve for this task**: this risk class on this target, for the rest of the task. For spending
  money and security changes this counts as approve once.
- **Deny**.

If you do not answer within 2 minutes, the confirmation counts as a denial.

## Budgets

A task stops when it reaches any of its budgets. The defaults:

| Budget | Default |
|---|---|
| Steps | 30 |
| Wall clock | 10 minutes |
| Model tokens | 500 000 |
| Failed verifications in a row | 3 |

There are also per-task limits on how many actions of each risk class may run (see
[Safety](safety.md#rate-limits)).

## "Done" means verified

An action that changes something (an edit or anything riskier) is checked afterwards by observing
the result. A task is reported as **succeeded** only when the agent says it finished and every change
passed verification. Otherwise the outcome is **unverified** or **failed**, and the timeline shows
which step did not check out.

## Prompt injection

Web pages, emails and app content can contain text written to steer an AI. The agent:

- wraps every tool result in a fenced block that the model is told to treat as data, never as
  instructions;
- says so in its system prompt;
- runs a detector over observations, and when one reads like instructions aimed at the agent, asks
  you before the next action of edit risk or higher, even if the policy would allow it.

This reduces the risk; it does not remove it. Keep risky actions on **confirm**.

## Stopping it

- **Escape three times** (each press within half a second of the last) stops every agent at once.
  This is the [kill switch](safety.md#the-kill-switch).
- **Stop all agents** in the tray menu does the same.
- **Cancel** on the Agent page stops one task.

## Privacy

The agent's model calls normally go to the provider for the `agentic` tier. With a cloud provider, the
goal, the observations (page snapshots, app text) and any context from your data are sent to that
provider, and each request is recorded in the [egress ledger](privacy.md) with feature `agent`.
Browser actions run in your own browser; the sites you visit see your browser's traffic as usual,
which Nugget does not record. Every action is in the hash-chained action log, with secure-field text
and card numbers redacted.
