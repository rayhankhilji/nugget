# Finding anything: filters, saved searches, timelines and previews

[Search](search-ask.md) finds passages that match your words. This page covers narrowing a search,
coming back to it, and seeing everything about one person, project or organisation in time order.
All of it runs on this device.

## Filters

After a search, the **Filters** panel lists what the results contain, with counts:

- **Kind**: mail (IMAP, Gmail, Outlook, `.eml`), event (calendars, `.ics`), chat, or file.
- **Source**: the folder, account or app.
- **People and projects** from the [knowledge graph](knowledge.md) that results mention.
- **Date**: **From** and **To**, or a month bar. Dates are the item's own (event start, mail sent,
  otherwise file modified), in your local time zone.

Values in one group match any; different groups all apply. **Clear filters** removes them. Filters
cover the best 300 matches (a note says when there are more) and never change ranking.

## Saved and recent searches

**Save search** names the query and its filters; saved searches appear on the Search page, in the
sidebar and in the command palette (<kbd>Ctrl</kbd>+<kbd>K</kbd> or <kbd>⌘</kbd>+<kbd>K</kbd>).
**Recent searches** (the last ten) show under an empty search box. Both stay on this device and are
not part of a [backup](backup.md).

## Timelines

A timeline shows every item and fact about a person, organisation or project, by month. Open one
with **Timeline** on an entity in [Knowledge](knowledge.md), a person under a search result, or a
People and projects filter value. Facts are placed at the date they apply from, when known. Long
timelines load the newest 300 events first; **Jump to month** loads earlier ones.

## Previews

Select a result's title (or press Enter) to preview it: the item's text with your words highlighted,
the people it mentions and the facts learned from it. **Open in app** opens the original; Escape
closes it. Sensitive items carry a **Sensitive** badge; they are shown locally but never sent to cloud
models (see [Privacy](privacy.md)).

## For developers

The page uses the local IPC methods `search.faceted` (results plus facet counts), `search.timeline`
and `search.preview`. They make no network calls and call no model.
