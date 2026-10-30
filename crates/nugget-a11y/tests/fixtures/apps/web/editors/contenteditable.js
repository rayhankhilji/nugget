/*
 * Plain contenteditable chat composer (no framework): text with <br> line breaks, as many
 * chat web apps keep it. The browser edits natively (its own <div>s / <br>s) and undoes
 * natively; the model is read from the DOM.
 */
(function () {
  'use strict';
  const N = window.Nugget;
  function html(paras) {
    // "a\nb" -> a<br>b ; a trailing empty line needs an extra <br> to be visible.
    let out = paras.map(N.esc).join('<br>');
    if (paras[paras.length - 1] === '') out += '<br>';
    return out;
  }
  N.editors.contenteditable = {
    create(mount, env) {
      const root = N.h('div', {
        class: 'input-message-input',
        contenteditable: 'true',
        role: 'textbox',
        'aria-multiline': 'true',
        'aria-label': env.label,
        'data-placeholder': env.placeholder,
        dir: 'auto',
      });
      mount.append(root);
      const ed = new N.NativeEditor(root, { html, onChange: env.onChange });
      ed.root = root;
      return ed;
    },
  };
})();
