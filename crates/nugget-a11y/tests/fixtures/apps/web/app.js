/*
 * Fixture page shell (see ../PROTOCOL.md): loads /truth.json, renders the conversation
 * context (chat or mail) with its decoys, mounts the editor picked by ?editor=<id>, and runs
 * the command channel (GET /cmd) and the state reports (POST /state).
 */
(async function () {
  'use strict';
  const N = window.Nugget;
  const h = N.h;
  const PASSWORD = 'fixture-pass-2026'; // preset, never reported (only its length)

  const id = new URLSearchParams(location.search).get('editor') || 'textarea';
  const truth = await (await fetch('/truth.json', { cache: 'no-store' })).json();
  const spec = truth.editors.find((e) => e.id === id);
  const app = document.getElementById('app');
  if (!spec) {
    app.textContent = 'Unknown editor: ' + id;
    return;
  }
  document.title = 'Nugget fixture: ' + spec.name;
  await new Promise((ok, fail) => {
    const s = document.createElement('script');
    s.src = 'editors/' + encodeURIComponent(id) + '.js';
    s.onload = ok;
    s.onerror = fail;
    document.head.append(s);
  });

  /* ---------- context rendering ---------- */

  const initials = (name) => name.split(/\s+/).map((w) => w[0]).join('').slice(0, 2).toUpperCase();
  const emailOf = (name) => name.toLowerCase().replace(/[^a-z]+/g, '.') + '@example.com';
  /** Message text with URLs as links (link text equals the URL, so text is unchanged). */
  const richText = (t) =>
    t.split(/(https?:\/\/\S+)/).map((part, i) => (i % 2 ? h('a', { href: part, target: '_blank', rel: 'noopener' }, part) : part));
  const isoTime = (t) => {
    const m = /^(\d{1,2}):(\d{2})\s*(AM|PM)$/.exec(t);
    if (!m) return null;
    let hh = +m[1] % 12;
    if (m[3] === 'PM') hh += 12;
    return '2026-10-30T' + String(hh).padStart(2, '0') + ':' + m[2];
  };

  function passwordPanel() {
    const pw = h('input', { id: 'reauth-password', type: 'password', name: 'password', autocomplete: 'current-password', class: 'reauth-input' });
    pw.value = PASSWORD;
    const panel = h(
      'form',
      { class: 'reauth', 'aria-labelledby': 'reauth-title', onsubmit: (e) => e.preventDefault() },
      h('div', { id: 'reauth-title', class: 'reauth-title' }, 'Re-enter password to send'),
      h('label', { for: 'reauth-password' }, 'Password'),
      pw
    );
    return { panel, pw };
  }

  function renderChat(ctx) {
    const channel = '#offsite-planning';
    const last = ctx.messages[ctx.messages.length - 1];
    const conv = (name, preview, opts) =>
      h(
        'li',
        { class: 'conv' + (opts && opts.current ? ' current' : '') },
        h(
          'a',
          { href: '#', 'aria-current': opts && opts.current ? 'page' : null },
          h('span', { class: 'conv-name' }, name),
          opts && opts.unread ? h('span', { class: 'badge', 'aria-label': opts.unread + ' unread' }, String(opts.unread)) : null,
          h('span', { class: 'conv-preview' }, preview)
        )
      );
    const sidebar = h(
      'nav',
      { class: 'sidebar', 'aria-label': 'Conversations' },
      h('div', { class: 'workspace' }, 'Northwind Studio'),
      h('input', { type: 'search', class: 'side-search', 'aria-label': 'Search conversations', placeholder: 'Search Northwind Studio' }),
      h('div', { class: 'side-head' }, 'Channels'),
      h(
        'ul',
        { class: 'conv-list' },
        conv('#general', 'Rina Okafor: Reminder, the all-hands moved to 3pm', { unread: 2 }),
        conv('#design-crit', 'Tomas Ruiz: New mocks are up on the board'),
        conv(channel, last.author + ': ' + last.text.slice(0, 40) + '…', { current: true })
      ),
      h('div', { class: 'side-head' }, 'Direct messages'),
      h(
        'ul',
        { class: 'conv-list' },
        conv('Priya Natarajan', 'Can you review my slides before lunch?', { unread: 1 }),
        conv('Diego Alvarez', 'Sent you the quote as a PDF')
      )
    );

    // Messages: one listitem each; consecutive messages of one author are grouped (only the
    // first shows avatar and name, like Slack).
    const items = ctx.messages.map((m, i) => {
      const prev = ctx.messages[i - 1];
      const first = !prev || prev.author !== m.author;
      const who = m.from_me ? 'You' : m.author;
      const time = h('time', { datetime: isoTime(m.time), class: 'msg-time' }, m.time);
      return h(
        'div',
        { role: 'listitem', class: 'msg' + (first ? ' msg-first' : ' msg-cont') + (m.from_me ? ' msg-me' : '') },
        first ? h('div', { class: 'avatar', 'aria-hidden': 'true' }, initials(m.from_me ? 'You' : m.author)) : h('div', { class: 'avatar-gap' }),
        h(
          'div',
          { class: 'msg-main' },
          first ? h('div', { class: 'msg-head' }, h('span', { class: 'msg-author' }, who), ' ', time) : h('div', { class: 'msg-head msg-head-cont' }, time),
          h('div', { class: 'msg-body', dir: 'auto' }, richText(m.text))
        )
      );
    });
    const history = h(
      'section',
      { class: 'history', 'aria-label': 'Message history' },
      h('div', { class: 'day-divider', role: 'separator', 'aria-label': 'Today' }, h('span', null, 'Today')),
      h('div', { role: 'list', 'aria-label': 'Messages', class: 'messages' }, items)
    );
    const header = h(
      'header',
      { class: 'channel-header' },
      h('h1', { class: 'channel-name' }, channel),
      h('span', { class: 'channel-topic' }, 'Planning the Q3 team offsite'),
      h(
        'div',
        { role: 'toolbar', 'aria-label': 'Channel actions', class: 'toolbar' },
        h('button', { type: 'button' }, 'Huddle'),
        h('button', { type: 'button' }, 'Pins'),
        h('button', { type: 'button' }, 'Search in channel')
      )
    );
    const presence = h('div', { class: 'presence', role: 'status' }, 'Maya Chen and Diego Alvarez are active');
    const editorMount = h('div', { class: 'editor-mount' });
    const { panel, pw } = passwordPanel();
    const composer = h(
      'div',
      { class: 'composer', role: 'group', 'aria-label': 'Message composer' },
      h(
        'div',
        { role: 'toolbar', 'aria-label': 'Formatting', class: 'fmt-toolbar' },
        ['Bold', 'Italic', 'Strikethrough', 'Link', 'Bulleted list'].map((b) => h('button', { type: 'button', tabindex: '-1', 'aria-label': b }, b[0]))
      ),
      editorMount,
      h('div', { class: 'composer-foot' }, h('button', { type: 'button', class: 'attach' }, 'Attach'), panel, h('button', { type: 'button', class: 'send' }, 'Send now'))
    );
    const typing = h('div', { class: 'typing', role: 'status', 'aria-live': 'polite' }, 'Diego Alvarez is typing…');
    app.className = 'app chat';
    app.append(sidebar, h('main', { class: 'main' }, header, presence, history, composer, typing));
    requestAnimationFrame(() => (history.scrollTop = history.scrollHeight));
    return { editorMount, pw, label: 'Message ' + channel, placeholder: 'Message ' + channel };
  }

  function renderMail(ctx) {
    const nav = h(
      'nav',
      { class: 'mail-nav', 'aria-label': 'Mail folders' },
      h('div', { class: 'brand' }, 'Fernmail'),
      h('button', { type: 'button', class: 'compose-btn' }, 'Compose'),
      h(
        'ul',
        { class: 'folders' },
        [
          ['Inbox', '12'],
          ['Starred', ''],
          ['Snoozed', ''],
          ['Sent', ''],
          ['Drafts', '2'],
          ['Offsite', '3'],
        ].map(([f, n], i) => h('li', null, h('a', { href: '#', 'aria-current': i === 0 ? 'page' : null }, h('span', null, f), n ? h('span', { class: 'count' }, n) : null)))
      )
    );
    const people = [...new Set(ctx.messages.map((m) => m.author))];
    const msgs = ctx.messages.map((m, i) => {
      const to = ['me'].concat(people.filter((p) => p !== m.author));
      const prev = ctx.messages[i - 1];
      const latest = i === ctx.messages.length - 1;
      const quote =
        latest && prev
          ? h(
              'div',
              { class: 'gmail_quote' },
              h('div', { class: 'gmail_attr' }, 'On ' + prev.time + ' ' + prev.author + ' <' + emailOf(prev.author) + '> wrote:'),
              h('blockquote', { class: 'gmail_quote' }, richText(prev.text))
            )
          : null;
      return h(
        'div',
        { role: 'listitem', class: 'mail-msg' + (latest ? ' latest' : '') },
        h(
          'div',
          { class: 'mail-head' },
          h('div', { class: 'avatar', 'aria-hidden': 'true' }, initials(m.author)),
          h('h3', { class: 'sender' }, h('span', { class: 'sender-name' }, m.from_me ? 'me' : m.author)),
          h('span', { class: 'sender-email' }, '<' + emailOf(m.author) + '>'),
          h('span', { class: 'mail-date', title: m.time }, m.time)
        ),
        h('div', { class: 'mail-to' }, 'to ' + to.join(', ')),
        h('div', { class: 'mail-body', dir: 'auto' }, h('div', null, richText(m.text)), quote)
      );
    });
    const top = h(
      'header',
      { class: 'mail-top' },
      h('input', { type: 'search', 'aria-label': 'Search mail', placeholder: 'Search mail', class: 'mail-search' }),
      h(
        'div',
        { role: 'toolbar', 'aria-label': 'Thread actions', class: 'toolbar' },
        ['Archive', 'Report spam', 'Delete', 'Mark as unread', 'Move to'].map((b) => h('button', { type: 'button' }, b))
      )
    );
    const editorMount = h('div', { class: 'editor-mount' });
    const { panel, pw } = passwordPanel();
    const chip = (name) =>
      h('div', { role: 'option', 'aria-selected': 'false', class: 'chip', 'data-hovercard-id': emailOf(name), title: emailOf(name) }, h('span', null, name));
    const compose = h(
      'div',
      { class: 'compose', role: 'region', 'aria-label': 'Reply' },
      h(
        'div',
        { class: 'compose-row' },
        h('span', { class: 'row-label', id: 'to-label' }, 'To'),
        h('div', { role: 'listbox', 'aria-labelledby': 'to-label', 'aria-orientation': 'horizontal', class: 'chips' }, ctx.recipients.map(chip)),
        h('input', { type: 'text', class: 'to-input', 'aria-label': 'To recipients', autocomplete: 'off' })
      ),
      h(
        'div',
        { class: 'compose-row' },
        h('input', { type: 'text', name: 'subjectbox', class: 'subject-input', 'aria-label': 'Subject', placeholder: 'Subject', value: 'Re: ' + ctx.subject })
      ),
      editorMount,
      h('div', { class: 'compose-foot' }, h('button', { type: 'button', class: 'send' }, 'Send'), panel, h('div', { class: 'draft-status', role: 'status' }, 'Draft saved'))
    );
    const threadScroll = h(
      'div',
      { class: 'thread-scroll' },
      h('div', { class: 'thread-head' }, h('h2', { class: 'subject' }, ctx.subject), h('span', { class: 'label-chip' }, 'Inbox')),
      h('div', { role: 'list', 'aria-label': 'Conversation', class: 'thread' }, msgs)
    );
    const status = h('div', { class: 'mail-status', role: 'status' }, 'Last account activity: 12 minutes ago');
    app.className = 'app mail';
    app.append(nav, h('div', { class: 'mail-col' }, top, h('main', { class: 'mail-main', role: 'main' }, threadScroll, compose), status));
    requestAnimationFrame(() => (threadScroll.scrollTop = threadScroll.scrollHeight));
    return { editorMount, pw, label: 'Message Body', placeholder: '' };
  }

  const ctx = truth.contexts[spec.context];
  const page = spec.context === 'mail' ? renderMail(ctx) : renderChat(ctx);
  app.removeAttribute('aria-busy');

  /* ---------- editor ---------- */

  const editor = N.editors[id].create(page.editorMount, {
    label: page.label,
    placeholder: page.placeholder,
    context: spec.context,
    onChange: () => report(),
  });
  editor.setInitial(truth.initial_text);

  const inputs = [];
  document.addEventListener(
    'beforeinput',
    (e) => {
      if (!editor.root.contains(e.target)) return;
      inputs.push(e.inputType);
      if (inputs.length > 20) inputs.shift();
      report();
    },
    true
  );
  document.addEventListener('focusin', () => report());
  document.addEventListener('focusout', () => setTimeout(report, 0));

  /* ---------- state reports ---------- */

  let ack = 0;
  let n = 0;
  // Identifies this page load to the harness, which follows only the first page after a launch.
  const token = Math.random().toString(36).slice(2) + Date.now().toString(36);
  let lastPost = 0;
  let timer = null;
  function active() {
    const a = document.activeElement;
    if (a && editor.root.contains(a)) return 'editor';
    if (a === page.pw) return 'password';
    return 'other';
  }
  function state() {
    const raw = N.normNewlines(editor.text());
    return {
      editor: id,
      token,
      n: ++n,
      ack,
      text: N.normText(raw),
      text_raw: raw,
      label: editor.root.getAttribute('aria-label') || '',
      active: active(),
      password_len: page.pw.value.length,
      undo_depth: editor.undoDepth(),
      inputs: inputs.slice(),
    };
  }
  function post() {
    timer = null;
    lastPost = Date.now();
    fetch('/state', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(state()) }).catch(() => {});
  }
  /** At most one post per 50 ms; a change during the wait is covered by the pending post. */
  function report() {
    if (timer) return;
    timer = setTimeout(post, Math.max(0, 50 - (Date.now() - lastPost)));
  }

  /* ---------- command channel ---------- */

  function run(c) {
    if (c.cmd === 'reset') editor.reset(c.text != null ? c.text : truth.initial_text);
    else if (c.cmd === 'focus-editor') editor.focusEnd();
    else if (c.cmd === 'focus-password') page.pw.focus({ preventScroll: true });
  }
  async function poll() {
    try {
      const r = await fetch('/cmd?token=' + token, { cache: 'no-store' });
      const c = await r.json();
      if (c && c.cmd && c.seq > ack) {
        try {
          run(c);
        } finally {
          ack = c.seq;
          report();
        }
      }
    } catch (_) {
      /* server gone or not ready: keep polling */
    }
    setTimeout(poll, 100);
  }
  window.__nuggetFixture = { editor, state }; // for local debugging only
  report();
  poll();
})();
