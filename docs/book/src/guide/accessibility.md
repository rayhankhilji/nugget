# Accessibility

## What works

- **Keyboard only.** Every control is reachable with Tab and Shift+Tab, with visible focus. Dialogs
  take focus, keep it while open, return it when closed, and close with Escape.
- **Screen readers.** Pages have landmarks, headings and named controls. Streaming answers and
  indexing progress are announced when they start and finish, not word by word. Errors are alerts.
- **Reduced motion.** Animations, the graph layout and spinners stop moving, live, without a
  restart.
- **More contrast** and Windows high-contrast themes strengthen borders, text and focus rings.
- **Zoom.** At 200% (or about 680 px wide) the sidebar becomes a top bar and nothing scrolls
  sideways.
- **Light and dark** follow your system; both meet WCAG 2.1 AA contrast.

## Keyboard shortcuts

On macOS, "Ctrl" is ⌘. Shortcuts without a modifier work when you are not typing in a field. Press
`?` in the app for this list.

| Keys | What it does |
| --- | --- |
| Ctrl+K | Open the command palette (pages, actions, search) |
| Ctrl+1 … Ctrl+9 | Go to a page: 1 Home, 2 Needs you, 3 Search, 4 Connections, 5 Chat, then the first Settings pages (Knowledge, Sources, Connected apps, Plugins) |
| Ctrl+, | Open Settings (at Status) |
| `?` or Ctrl+/ | Show the keyboard shortcuts |
| Escape | Close a dialog or the palette; in Chat, stop a reply |
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
| C | Center on the focused entity |
| Escape | Clear the focus (Tab then leaves the canvas) |

## Known gaps

- A screen reader can't browse the graph drawing; the **Entities** tab lists the same entities.
- The app is tested with automated checks and Chromium's accessibility tree, not yet with every
  screen reader on every platform.
- The browser's own animation of an indeterminate progress bar can't be turned off.
- The Double Tap overlay uses the same focus and contrast styles but isn't covered by automated
  tests.

If something doesn't work for you, please open an issue.
