/* Plain <textarea> chat composer: the browser does everything (native undo). */
(function () {
  'use strict';
  const N = window.Nugget;
  N.editors.textarea = {
    create(mount, env) {
      const ta = N.h('textarea', { class: 'chat-textarea', 'aria-label': env.label, placeholder: env.placeholder, rows: '3', spellcheck: 'true' });
      mount.append(ta);
      ta.addEventListener('input', env.onChange);
      return {
        root: ta,
        text: () => N.normNewlines(ta.value),
        undoDepth: () => -1,
        setInitial(t) {
          ta.value = t;
        },
        reset(t) {
          ta.value = t; // assigning value also clears the browser's undo stack for the field
          this.focusEnd();
        },
        focusEnd() {
          ta.focus({ preventScroll: true });
          ta.setSelectionRange(ta.value.length, ta.value.length);
        },
      };
    },
  };
})();
