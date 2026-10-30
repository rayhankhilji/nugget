/*
 * Lexical-style editor (clean room). DOM as Lexical's rich-text setup renders it:
 *   div[contenteditable=true][role=textbox][spellcheck=true][data-lexical-editor=true]
 *     > p.editor-paragraph[dir=ltr|rtl] > span[data-lexical-text=true]   (text)
 *     > p.editor-paragraph > br                                          (empty paragraph)
 *
 * Input handling, as Lexical does it:
 *  - beforeinput is handled and cancelled for everything except a single typed character
 *    with the caret inside a text node, which Lexical lets the browser insert and then reads
 *    back from the characterData mutation (nativeSingleChar).
 *  - insertText whose data contains "\n" (multi-character text from AT insertion, a
 *    replacement, autocorrect): Lexical inserts the data as raw text into the text node, so
 *    the newline stays a line break inside the paragraph ('soft'; rendered by pre-wrap and
 *    read as "\n"). insertParagraph / Enter split the paragraph.
 *  - paste: the paste event is cancelled; text/plain is split on "\n" and every line break
 *    becomes a new paragraph ("\n\n" leaves an empty paragraph).
 *  - foreign mutations: text edits inside existing text nodes are accepted (Lexical syncs
 *    characterData mutations); any structural change nobody announced with an input event
 *    is reverted by re-rendering from the model.
 *  - own history (historyUndo/Redo beforeinput and the platform chord).
 */
(function () {
  'use strict';
  const N = window.Nugget;
  function html(paras) {
    return paras
      .map((t) => {
        if (t === '') return '<p class="editor-paragraph"><br></p>';
        const d = N.dirOf(t);
        return '<p class="editor-paragraph"' + (d ? ' dir="' + d + '"' : '') + '><span data-lexical-text="true">' + N.esc(t) + '</span></p>';
      })
      .join('');
  }
  N.editors.lexical = {
    create(mount, env) {
      const root = N.h('div', {
        class: 'lexical-content-editable',
        contenteditable: 'true',
        role: 'textbox',
        spellcheck: 'true',
        'data-lexical-editor': 'true',
        'aria-label': env.label,
        style: 'user-select: text; white-space: pre-wrap; word-break: break-word;',
      });
      const shell = N.h('div', { class: 'editor-shell lexical-shell' }, N.h('div', { class: 'editor-container' }, root));
      mount.append(shell);
      return new N.RichEditor(root, {
        html,
        policy: 'model',
        foreign: 'chardata',
        insertTextNewline: 'soft',
        nativeSingleChar: true,
        onChange: env.onChange,
      });
    },
  };
})();
