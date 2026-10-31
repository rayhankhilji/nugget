/*
 * ProseMirror / TipTap-style editor (clean room). DOM as TipTap renders it:
 *   div.tiptap.ProseMirror[contenteditable=true][role=textbox][aria-multiline=true][translate=no]
 *     > p (text)            > p > br.ProseMirror-trailingBreak   (empty paragraph)
 *   empty document: p.is-empty.is-editor-empty[data-placeholder] (TipTap Placeholder)
 *
 * Input handling, as ProseMirror does it:
 *  - typing, insertText (with or without "\n"), deletion and spellcheck edits are done by
 *    the browser natively; a MutationObserver reads the changed DOM back into the document
 *    (every DOM block is a paragraph; whatever the browser made of a "\n" becomes paragraph
 *    breaks) and re-renders the canonical DOM if the browser's shape differs.
 *  - Enter is handled on keydown (splitBlock).
 *  - paste: the paste event is cancelled and text/plain is parsed into one paragraph per line
 *    ("\n\n" leaves an empty paragraph).
 *  - own history (prosemirror-history): historyUndo/Redo beforeinput and the chord.
 */
(function () {
  'use strict';
  const N = window.Nugget;
  function html(paras) {
    if (paras.length === 1 && paras[0] === '')
      return '<p class="is-empty is-editor-empty" data-placeholder="Write a message…"><br class="ProseMirror-trailingBreak"></p>';
    return paras.map((t) => (t === '' ? '<p><br class="ProseMirror-trailingBreak"></p>' : '<p>' + N.esc(t) + '</p>')).join('');
  }
  N.editors.prosemirror = {
    create(mount, env) {
      const root = N.h('div', {
        contenteditable: 'true',
        role: 'textbox',
        'aria-multiline': 'true',
        'aria-label': env.label,
        translate: 'no',
        class: 'tiptap ProseMirror',
        tabindex: '0',
      });
      mount.append(N.h('div', { class: 'tiptap-wrapper' }, root));
      return new N.RichEditor(root, { html, policy: 'observe', foreign: 'accept', onChange: env.onChange });
    },
  };
})();
