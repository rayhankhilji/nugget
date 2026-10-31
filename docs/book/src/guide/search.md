# Finding anything: filters, saved searches, timelines and previews

[Search](search-ask.md) finds the passages that match your words. This page covers the tools for
narrowing a search down, coming back to it later, and seeing everything about one person, project or
organisation in time order. All of it works on this device: no query, filter or result is sent
anywhere.

## Filters

After a search, the **Filters** panel next to the results lists what the results are made of, each
value with how many results it has:

- **Kind**: mail, event, chat or file. Mail is anything that came in as an email (IMAP, Gmail,
  Outlook, or an `.eml` file in a folder), events are calendar entries (CalDAV, Google or Microsoft
  calendars, `.ics` files), chats are messages from messaging apps, and everything else is a file.
- **Source**: the source (folder, account or app) each result came from.
- **People**: the people, organisations and projects from the [knowledge graph](knowledge.md) that
  the results mention. Choose one to keep only the results that mention them.
- **Date**: a **From** and **To** date, or a month from the results-by-month bars. Dates are the
  item's *own* date: when an event starts, when a mail was sent, otherwise when the file was last
  changed. A folder of old mail copied today keeps its real dates.

Tick several values in one group to see results matching any of them; filters in different groups
all apply. The counts in each group show what you would get if you changed that group, given the
other filters, so you can see where results are before you click. **Clear filters** removes them all.

Filters work on the best matches for your query (up to 300 items); when a query matches more than
that, the counts say so. Filtering never changes how results are ranked, only which are shown.

Everything is reachable from the keyboard: Tab moves through the filter groups, Space ticks a value,
and the up and down arrow keys move between results.

## Saved and recent searches

A search with its filters is kept as one short line of text (for example
`q=budget&kind=mail&from=2026-01-01`), so it can be saved and run again exactly.

- **Save search** (top of the Search page) asks for a name. Saved searches are listed on the Search
  page, in the sidebar under the pages, and in the command palette (<kbd>Ctrl</kbd>+<kbd>K</kbd>, or
  <kbd>⌘</kbd>+<kbd>K</kbd> on a Mac: type the name and press Enter). Delete one from the Search
  page.
- **Recent searches** show under the search box when it is empty: the last ten searches, newest
  first. **Clear recent** forgets them.

Saved and recent searches are stored in the app on this device only. They are not part of a
[backup](backup.md).

## Timelines

A timeline shows everything Nugget knows about one person, organisation or project in time order:
every item that mentions them (mails, events, notes, files, chats) and every fact about them, grouped
by month.

Open one from:

- the **Timeline** button on an entity's page in [Knowledge](knowledge.md), or
- a person under a search result, or the **Timeline** next to a value in the People filter.

Items are placed at their own date (see Filters above). Facts are placed at the date they apply from
when Nugget knows it, otherwise at the date of the source they came from. **Jump to month** moves to
any month in the timeline. A very long timeline shows the newest 300 events first; jumping to an
earlier month loads that part. **Back to results** returns to your search.

## Previews

Select a result's title (or press Enter on it) to open its preview beside the results: its kind, date
and source, the matching passage, the item's text with your search words highlighted, the people it
mentions (each opens their timeline) and the facts Nugget learned from it, with quotes. **Open in
app** opens the original. Escape or **Close preview** closes it and returns you to the result.

Items marked sensitive (for example ones containing personal identifiers) carry a **Sensitive**
badge. They are shown to you here because nothing leaves your device; they are still never sent to a
cloud model (see [Privacy](privacy.md)).

## For developers

The Search page uses three local daemon methods, all served by the `search_ui` feature
(`crates/nuggetd/src/features/search_ui`):

| Method | What it returns |
|---|---|
| `search.faceted` | Results for `query` with `filters` (`sources`, `kinds`, `people`, `from`, `to`), paged by `limit`/`offset`, plus facet counts (`sources`, `kinds`, `people`, `months`). |
| `search.timeline` | An entity's items and facts in time order, with month counts, optionally between `from` and `to`. |
| `search.preview` | One item's text (up to 20,000 characters), matching passage, people and facts. |

They make no network calls and call no model.
