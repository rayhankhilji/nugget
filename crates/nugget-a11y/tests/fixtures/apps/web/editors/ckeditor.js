/*
 * CKEditor 5-style classic editor (clean room). DOM as CKEditor 5 renders it:
 *   div.ck.ck-reset.ck-editor.ck-rounded-corners[role=application][lang=en][dir=ltr]
 *     > div.ck.ck-editor__top > div.ck.ck-toolbar[role=toolbar]   (formatting toolbar)
 *     > div.ck.ck-editor__main[role=presentation]
 *       > div.ck.ck-content.ck-editor__editable.ck-rounded-corners.ck-editor__editable_inline
 *            .ck-focused|.ck-blurred[contenteditable=true][role=textbox]
 *            [aria-label="Editor editing area: main"][lang=en][dir=ltr]
 *         > p (text; spaces at run ends and doubled spaces rendered as NBSP, no pre-wrap)
 *         > p > br[data-cke-filler=true]   (empty paragraph)
 *
 * Input handling, as CKEditor 5 does it:
 *  - every beforeinput it knows (insertText, insertReplacementText, insertParagraph,
 *    insertLineBreak, delete*, insertFromPaste/Drop) is cancelled and applied to its model.
 *  - insertText data containing "\n": the fixture splits it into paragraphs ('split'),
 *    like CKEditor's Enter handling (its model has no newline characters in text).
 *    insertLineBreak (Shift+Enter, a soft break in CKEditor) is also a paragraph split here;
 *    the plain-text model is the same.
 *  - paste: cancelled; text/plain becomes one paragraph per line ("\n\n" leaves an empty one).
 *  - foreign mutations: text changes inside existing text nodes are taken into the model
 *    (CKEditor's mutation observer handles text mutations), structural ones are re-rendered.
 *  - own history (Undo plugin).
 */
(function () {
  'use strict';
  const N = window.Nugget;
  function html(paras) {
    return paras.map((t) => (t === '' ? '<p><br data-cke-filler="true"></p>' : '<p>' + N.esc(N.nbspify(t)) + '</p>')).join('');
  }
  N.editors.ckeditor = {
    create(mount, env) {
      const h = N.h;
      const btn = (label, cls) =>
        h('button', { class: 'ck ck-button ck-off ' + cls, type: 'button', tabindex: '-1', 'aria-label': label, 'aria-pressed': 'false' }, h('span', { class: 'ck ck-button__label' }, label));
      const editable = h('div', {
        class: 'ck-blurred ck ck-content ck-editor__editable ck-rounded-corners ck-editor__editable_inline',
        lang: 'en',
        dir: 'ltr',
        role: 'textbox',
        'aria-label': 'Editor editing area: main',
        contenteditable: 'true',
      });
      const ed = h(
        'div',
        { class: 'ck ck-reset ck-editor ck-rounded-corners', role: 'application', dir: 'ltr', lang: 'en', 'aria-labelledby': 'ck-editor__label_main' },
        h('label', { class: 'ck ck-label ck-voice-label', id: 'ck-editor__label_main' }, 'Rich Text Editor'),
        h(
          'div',
          { class: 'ck ck-editor__top ck-reset_all', role: 'presentation' },
          h(
            'div',
            { class: 'ck ck-sticky-panel' },
            h(
              'div',
              { class: 'ck ck-toolbar', role: 'toolbar', 'aria-label': 'Editor toolbar' },
              h('div', { class: 'ck ck-toolbar__items' }, btn('Bold', 'ck-bold'), btn('Italic', 'ck-italic'), btn('Link', 'ck-link'), btn('Bulleted List', 'ck-list'))
            )
          )
        ),
        h('div', { class: 'ck ck-editor__main', role: 'presentation' }, editable)
      );
      mount.append(ed);
      editable.addEventListener('focus', () => {
        editable.classList.replace('ck-blurred', 'ck-focused');
      });
      editable.addEventListener('blur', () => {
        editable.classList.replace('ck-focused', 'ck-blurred');
      });
      return new N.RichEditor(editable, {
        html,
        policy: 'model',
        foreign: 'chardata',
        insertTextNewline: 'split',
        onChange: env.onChange,
      });
    },
  };
})();
