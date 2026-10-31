/*
 * Quill-style editor (clean room). DOM as Quill (snow theme) renders it:
 *   div.ql-container.ql-snow
 *     > div.ql-editor[contenteditable=true][role=textbox][aria-multiline=true][data-gramm=false]
 *         (.ql-blank + data-placeholder while empty) > p (text) | p > br (empty line)
 *     > div.ql-clipboard[contenteditable=true][tabindex=-1]   (Quill 1's paste catcher, off-screen)
 *     > div.ql-tooltip.ql-hidden                               (link editor, hidden)
 *
 * Input handling, as Quill does it:
 *  - typing and other native edits (insertText with or without "\n", deletion, spellcheck)
 *    happen in the DOM; Quill's scroll observer reads the mutations back into its Delta and
 *    normalises the lines (every block is a line; a "\n" the browser turned into a block or
 *    <br> becomes a new line).
 *  - Enter is handled on keydown by the keyboard module.
 *  - paste: the clipboard module cancels the paste event and inserts text/plain, one line per
 *    "\n" ("\n\n" leaves an empty line).
 *  - own history module (historyUndo/Redo and the chord).
 */
(function () {
  'use strict';
  const N = window.Nugget;
  const html = (paras) => paras.map((t) => (t === '' ? '<p><br></p>' : '<p>' + N.esc(t) + '</p>')).join('');
  N.editors.quill = {
    create(mount, env) {
      const h = N.h;
      const root = h('div', {
        class: 'ql-editor',
        'data-gramm': 'false',
        contenteditable: 'true',
        role: 'textbox',
        'aria-multiline': 'true',
        'aria-label': env.label,
        'data-placeholder': env.placeholder,
      });
      const container = h(
        'div',
        { class: 'ql-container ql-snow' },
        root,
        h('div', { class: 'ql-clipboard', contenteditable: 'true', tabindex: '-1' }),
        h(
          'div',
          { class: 'ql-tooltip ql-hidden' },
          h('a', { class: 'ql-preview', rel: 'noopener noreferrer', target: '_blank', href: 'about:blank' }),
          h('input', { type: 'text', 'data-formula': 'e=mc^2', 'data-link': 'https://example.com', 'data-video': 'Embed URL' }),
          h('a', { class: 'ql-action' }),
          h('a', { class: 'ql-remove' })
        )
      );
      mount.append(container);
      return new N.RichEditor(root, {
        html,
        policy: 'observe',
        foreign: 'accept',
        onChange: env.onChange,
        decorate(ed) {
          root.classList.toggle('ql-blank', ed.paras.length === 1 && ed.paras[0] === '');
        },
      });
    },
  };
})();
