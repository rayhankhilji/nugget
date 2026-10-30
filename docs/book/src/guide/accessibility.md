# Accessibility

Nugget's desktop app is meant to work without a mouse, with a screen reader, and with your
system's display preferences. This page says what works, which keys to use, and what doesn't work
yet.

## What works

- **Keyboard only.** Every control can be reached with Tab and Shift+Tab, and the keyboard focus is
  always visible. Dialogs (Add source, Merge and Split, a Today card's action, the command palette,
  confirmations) take focus when they open, keep it inside while open, and give it back to the
  control that opened them when they close. Escape closes any dialog. Nothing traps focus: the
  knowledge graph lets Tab step through entities once one is focused and leaves the canvas at
  either end.
- **Screen readers.** Every page has one main landmark named after the page, a named sidebar and
  navigation, one top-level heading and no skipped heading levels. Icon-only buttons have names.
  Streaming answers in Chat and Ask, and indexing progress on Sources, Status and the first-run
  setup, are announced when they start and when they finish (or fail), not token by token or tick
  by tick, so the reader isn't flooded. Errors are announced as alerts.
- **Reduced motion.** With your system's "reduce motion" setting on, the knowledge graph is laid out
  before it is drawn and doesn't animate, the view jumps instead of easing, spinners become a still
  ring (their text still says what is busy), and loading skeletons stop shimmering. The app follows
  the setting live, without a restart.
- **More contrast.** With "increase contrast" on, borders, secondary text, links and focus rings are
  stronger and focus rings are thicker. With Windows high-contrast themes (forced colours), focus
  uses the system highlight colour, buttons keep their borders and icons follow the text colour.
- **Zoom.** At 200% zoom (or a window about 680 pixels wide) the sidebar becomes a compact bar at
  the top that wraps, and no page scrolls sideways.
- **Light and dark.** The app follows your system's appearance; both themes meet WCAG 2.1 AA colour
  contrast.

## Keyboard shortcuts

On macOS, "Ctrl" below is ⌘. Shortcuts with a modifier work anywhere; the others work when you are
not typing in a field. Press `?` in the app for this list.

| Keys | What it does |
| --- | --- |
| Ctrl+K | Open the command palette (pages, actions, search) |
| Ctrl+1 … Ctrl+9 | Go to a page by its position in the sidebar |
| Ctrl+, | Open Status |
| `?` or Ctrl+/ | Show the keyboard shortcuts |
| Escape | Close a dialog or the palette; in Chat, stop a reply |
| Tab, Shift+Tab | Move between controls |
| ↑ ↓ | Move between sidebar items, palette results and entity lists |
| ← → Home End | Move between tabs (Knowledge: Entities, Graph) |
| Enter | In Chat, send (Shift+Enter for a new line); on a citation, show its source |
| Ctrl+Enter | Agent: start the task |

In the knowledge graph (focus the canvas first):

| Keys | What it does |
| --- | --- |
| Arrow keys | Move to the nearest entity in that direction |
| Shift+arrows | Pan |
| N, P (or Tab, Shift+Tab once an entity is focused) | Next and previous entity |
| `+` `-` | Zoom in and out |
| 0 or F | Fit the graph to the view |
| Enter or Space | Open the focused entity |
| C | Centre on the focused entity |
| Escape | Clear the focus (Tab then leaves the canvas) |

## How it is tested

The real-app end-to-end test (`pnpm run e2e` in `apps/desktop`, see its README) runs the real
daemon and the real interface in Chromium and has three accessibility scenarios:

- **a11y** runs [axe-core](https://github.com/dequelabs/axe-core) (WCAG 2.1 A and AA plus best
  practices, colour contrast included) on every page and on the main dialogs (Add source, Merge and
  Split, a Today card's sources and action, the command palette, every first-run step), in light and
  dark, and fails on any serious or critical violation. It also checks each page's accessibility
  tree: landmarks, headings, and names for every control and image.
- **keyboard** completes the core flows with the keyboard alone: first-run setup to Done, adding a
  folder, searching and opening a result, asking in Chat and following a citation, accepting a Today
  card's action through its confirmation (the test adds a Safety rule so the action asks first),
  stopping all agents from Safety (the kill switch acts at once, without a confirmation), and the
  command palette.
  It fails if a focused control has no visible focus, if focus is trapped, or if Escape doesn't close
  a dialog and return focus.
- **prefs** checks reduced motion (nothing animates, the graph stays still), more contrast and
  forced colours (the stronger styles apply), and 200% zoom (nothing overflows, no sideways
  scrolling) on every page.

Unit tests (`pnpm test`) also run axe over every page and first-run step against a mocked daemon,
and check that announcements happen only at the start and end of a reply or an indexing pass.

## Known gaps

- The knowledge graph is drawn on a canvas. Its entities are reachable with the keyboard and the
  focused one is announced, but a screen reader can't browse the drawing itself; the Entities tab
  lists the same entities as an ordinary list.
- Automated checks can't judge everything: hover colours, the wording of names and announcements,
  and how a particular screen reader reads a page still need people. The app has been checked with
  the accessibility tree Chromium exposes, not yet with every screen reader on every platform.
- The browser's own animation of an indeterminate progress bar can't be turned off by the app.
- The keyboard scenario doesn't yet open the Merge and Split dialogs or the shortcuts list; they
  use the same dialog focus handling as the dialogs it does open.
- The Double Tap overlay follows the same focus and contrast styles but isn't covered by the
  automated scenarios.

If something doesn't work for you, please open an issue; accessibility bugs are treated like any
other bug.
