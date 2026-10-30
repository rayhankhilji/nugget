/*
 * Gmail-like compose body (no framework): contenteditable with browser-native editing and
 * native undo. As in Gmail, the first line is bare text in the editable and later lines are
 * <div>s; no pre-wrap, so edge spaces are NBSPs (reported as spaces).
 */
(function () {
  'use strict';
  const N = window.Nugget;
  function html(paras) {
    const line = (t) => (t === '' ? '<br>' : N.esc(N.nbspify(t)));
    return line(paras[0]) + paras.slice(1).map((t) => '<div>' + line(t) + '</div>').join('');
  }
  N.editors['mail-compose'] = {
    create(mount, env) {
      const root = N.h('div', {
        id: ':r7',
        class: 'Am aiL Al editable LW-avf tS-tW',
        hidefocus: 'true',
        'aria-label': 'Message Body',
        g_editable: 'true',
        role: 'textbox',
        'aria-multiline': 'true',
        contenteditable: 'true',
        tabindex: '1',
        spellcheck: 'false',
        itacorner: '6,7:1,1,0,0',
      });
      mount.append(root);
      const ed = new N.NativeEditor(root, { html, onChange: env.onChange });
      ed.root = root;
      return ed;
    },
  };
})();
