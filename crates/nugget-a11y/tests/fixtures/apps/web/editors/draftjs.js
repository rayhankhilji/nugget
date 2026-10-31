/*
 * Draft.js-style editor (clean room). DOM as Draft renders it:
 *   div.DraftEditor-root > div.DraftEditor-editorContainer
 *     > div.notranslate.public-DraftEditor-content[contenteditable=true][role=textbox][aria-multiline=true]
 *       > div[data-contents=true]
 *         > div[data-block=true][data-editor][data-offset-key]
 *           > div.public-DraftStyleDefault-block.public-DraftStyleDefault-ltr[data-offset-key]
 *             > span[data-offset-key] > span[data-text=true]     (empty block: br[data-text=true])
 *
 * Input handling, as Draft does it:
 *  - Draft owns every edit: text insertion is cancelled and applied to its immutable state,
 *    then React re-renders the blocks. (Draft listens to React's onBeforeInput; the fixture
 *    uses the native beforeinput event, which carries the same data in every browser.)
 *  - insertText data containing "\n": Draft's replaceText keeps the characters, so the
 *    newline becomes a soft newline inside the block's text ('soft'; Draft renders block text
 *    with pre-wrap). Enter / insertParagraph split the block.
 *  - paste: cancelled; text/plain is split into one block per line ("\n\n" leaves an empty
 *    block).
 *  - an input event Draft did not cancel (spellcheck, autocorrect, execCommand from an
 *    assistive tool) is read back from the DOM (editOnInput). DOM mutations no input event
 *    announced are discarded: the next render restores the blocks from the state.
 *  - own history (undo stack in EditorState).
 */
(function () {
  'use strict';
  const N = window.Nugget;
  const editorKey = '9c7a1';
  // Stable pseudo-random block keys, like Draft's genRandomKey().
  const keyFor = (i) => ((i * 7919 + 40503) % 0xfffff).toString(36).padStart(5, 'a').slice(-5);
  function html(paras) {
    return (
      '<div data-contents="true">' +
      paras
        .map((t, i) => {
          const k = keyFor(i) + '-0-0';
          const dir = N.dirOf(t) === 'rtl' ? 'rtl' : 'ltr';
          const leaf =
            t === ''
              ? '<span data-offset-key="' + k + '"><br data-text="true"></span>'
              : '<span data-offset-key="' + k + '"><span data-text="true">' + N.esc(t) + '</span></span>';
          return (
            '<div data-block="true" data-editor="' + editorKey + '" data-offset-key="' + k + '">' +
            '<div data-offset-key="' + k + '" class="public-DraftStyleDefault-block public-DraftStyleDefault-' + dir + '">' +
            leaf +
            '</div></div>'
          );
        })
        .join('') +
      '</div>'
    );
  }
  N.editors.draftjs = {
    create(mount, env) {
      const content = N.h('div', {
        class: 'notranslate public-DraftEditor-content',
        contenteditable: 'true',
        role: 'textbox',
        'aria-multiline': 'true',
        'aria-label': env.label,
        spellcheck: 'false',
        style: 'outline: none; user-select: text; white-space: pre-wrap; overflow-wrap: break-word;',
      });
      const rootEl = N.h('div', { class: 'DraftEditor-root' }, N.h('div', { class: 'DraftEditor-editorContainer' }, content));
      mount.append(rootEl);
      return new N.RichEditor(content, {
        html,
        container: (r) => r.firstElementChild || r,
        policy: 'model',
        foreign: 'revert',
        insertTextNewline: 'soft',
        onChange: env.onChange,
      });
    },
  };
})();
