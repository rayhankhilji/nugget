# App-matrix fixtures: protocol (FF3)

The app matrix (`tests/apps_matrix.rs`, run by `.github/workflows/a11y-apps.yml`) drives real
browsers and native editors and measures `nugget-a11y` against ground truth. This file is the
contract between the web fixtures (`web/`), the ground truth (`truth.json`) and the harness.
All fixtures are written for this project (clean room): they reproduce the DOM and ARIA
structure and the input handling of common editors, without any of their code.

## Serving

The harness serves `web/` and `truth.json` over plain HTTP on `127.0.0.1:<random port>`
(`GET /truth.json` returns `truth.json`). Nothing loads from the network.

A fixture page is opened as `http://127.0.0.1:<port>/index.html?editor=<id>`. Its document
title is `Nugget fixture: <editor name>`.

## Ground truth (`truth.json`)

```json
{
  "version": 1,
  "initial_text": "Draft: ",
  "editors": [
    {"id": "textarea", "name": "Plain textarea", "context": "chat", "kind": "plain"}
  ],
  "contexts": {
    "chat": {
      "messages": [{"author": "Maya Chen", "time": "10:02 AM", "text": "…", "from_me": false}],
      "recipients": [],
      "subject": null
    },
    "mail": {"messages": [], "recipients": ["Jordan Lee"], "subject": "Q3 offsite venue"}
  }
}
```

- `editors[].id` is one of `textarea`, `contenteditable`, `mail-compose`, `prosemirror`,
  `quill`, `draftjs`, `lexical`, `ckeditor`. `kind` is `plain` (textarea) or `rich`.
- `contexts.<c>.messages` are oldest first; `text` is the message body as a reader sees it
  (whitespace collapsed, quoted history excluded). The page also renders decoys (a sidebar of
  other conversations with previews, a toolbar, a status line) that are **not** messages.

## Editor text model (the oracle)

Every editor reports its own plain-text model: paragraphs joined with `\n` (an empty paragraph
is an empty line), no trailing newline, NBSP reported as a space. For `textarea` it is the value
with `\r\n` turned into `\n`.

## Command channel

The page polls `GET /cmd` every 100 ms. The response is `{"seq": <n>}` (nothing new) or
`{"seq": <n>, "cmd": "<cmd>", "text": "<optional>"}`. Each `seq` is executed once, in order:

| cmd | effect |
|---|---|
| `reset` | set the editor's content to `text` (split on `\n` into paragraphs), clear its undo history, focus the editor, caret at the end |
| `focus-editor` | focus the editor, caret at the end |
| `focus-password` | focus the password field (its value is preset by the page and never reported) |

After executing a command the page posts its state with `ack` set to that `seq`.

## State reports

`POST /state` with a JSON body, on every model change, on focus changes and after every command
(debounced to at most one post per 50 ms, and always one post after the last change):

```json
{
  "editor": "lexical",
  "ack": 3,
  "text": "Draft: hello",
  "active": "editor",
  "password_len": 15,
  "undo_depth": 1,
  "inputs": ["insertText", "insertFromPaste"]
}
```

- `active` is `editor`, `password` or `other` (where `document.activeElement` is).
- `undo_depth` is the number of steps in the editor's own history (`-1` for editors that use the
  browser's native undo: `textarea`, `contenteditable`, `mail-compose`).
- `inputs` lists the last 20 `beforeinput` `inputType`s seen by the editor (diagnostics).

## Undo

Editors with their own history (`prosemirror`, `quill`, `draftjs`, `lexical`, `ckeditor`)
handle `historyUndo` / `historyRedo` `beforeinput` events and the platform chord (Cmd+Z on
macOS, Ctrl+Z elsewhere; Shift for redo) on `keydown`, like the real frameworks. One
`insertText` or `insertFromPaste` or `insertReplacementText` input is one undo step; consecutive
`insertText` inputs of single characters typed within 500 ms merge into one step.
