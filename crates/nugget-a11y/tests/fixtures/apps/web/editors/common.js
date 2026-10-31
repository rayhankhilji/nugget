/*
 * Shared engine for the Nugget app-matrix web fixtures (see ../../PROTOCOL.md).
 *
 * Clean room: written for this project from general knowledge of how the real editors
 * behave; no code from any editor framework. Three kinds of editors are built on it:
 *
 *  - NativeEditor: the browser edits, the page only reads the DOM (plain contenteditable,
 *    mail compose). Native undo.
 *  - RichEditor with policy "observe": the browser edits natively, a MutationObserver
 *    re-reads the DOM into the model and re-renders it in canonical form (ProseMirror,
 *    Quill). Own history.
 *  - RichEditor with policy "model": every beforeinput is cancelled and applied to the
 *    model, the DOM is re-rendered from the model, foreign DOM mutations are reverted
 *    (Draft.js, Lexical, CKEditor 5). Own history.
 *
 * The model is a list of paragraph strings (UTF-16, like the DOM). A paragraph may contain a
 * literal "\n" where the framework keeps a soft line break inside one block (Draft.js,
 * Lexical); the plain-text model is the paragraphs joined with "\n" either way.
 */
(function () {
  'use strict';
  const N = (window.Nugget = window.Nugget || {});
  N.editors = N.editors || {};

  /* ---------- small helpers ---------- */

  N.h = function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
      else el.setAttribute(k, v === true ? '' : String(v));
    }
    for (const c of kids.flat(Infinity)) {
      if (c == null || c === false) continue;
      el.append(c.nodeType ? c : String(c));
    }
    return el;
  };

  const esc = (N.esc = (s) =>
    String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/ /g, '&nbsp;'));
  N.escAttr = (s) => esc(s).replace(/"/g, '&quot;');
  N.normText = (s) => s.replace(/ /g, ' ');
  N.normNewlines = (s) => String(s).replace(/\r\n?/g, '\n');
  N.isMac = /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent);

  /** Space handling of editors without `white-space: pre-wrap` (CKEditor's view, Gmail):
   *  a space at either end of a text run, or after another space, becomes NBSP. */
  N.nbspify = function (t) {
    let out = '';
    for (let i = 0; i < t.length; i++) {
      const c = t[i];
      if (c === ' ' && (i === 0 || i === t.length - 1 || out[out.length - 1] === ' ' || out[out.length - 1] === ' '))
        out += ' ';
      else out += c;
    }
    return out;
  };

  /** Base direction from the first strong character (null when there is none). */
  const RTL = /[֐-ࣿיִ-﷿ﹰ-﻿]/;
  const LTR = /[A-Za-zÀ-ʯͰ-ϿЀ-ԯ]/;
  N.dirOf = function (t) {
    for (const ch of t) {
      if (RTL.test(ch)) return 'rtl';
      if (LTR.test(ch)) return 'ltr';
    }
    return null;
  };

  /* ---------- graphemes (emoji with modifiers, ZWJ sequences, combining marks) ---------- */

  const seg = typeof Intl !== 'undefined' && Intl.Segmenter ? new Intl.Segmenter(undefined, { granularity: 'grapheme' }) : null;
  function prevBoundary(s, o) {
    if (o <= 0) return 0;
    if (seg) {
      let last = 0;
      for (const g of seg.segment(s)) {
        if (g.index >= o) break;
        last = g.index;
      }
      return last;
    }
    const c = s.charCodeAt(o - 1);
    return c >= 0xdc00 && c <= 0xdfff && o >= 2 ? o - 2 : o - 1;
  }
  function nextBoundary(s, o) {
    if (o >= s.length) return s.length;
    if (seg) {
      for (const g of seg.segment(s)) if (g.index > o) return g.index;
      return s.length;
    }
    const c = s.charCodeAt(o);
    return c >= 0xd800 && c <= 0xdbff ? o + 2 : o + 1;
  }
  N.graphemeCount = (s) => (seg ? [...seg.segment(s)].length : [...s].length);

  /* ---------- DOM -> paragraphs ---------- */

  const BLOCKS = new Set(['P', 'DIV', 'LI', 'UL', 'OL', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'BLOCKQUOTE', 'PRE', 'SECTION', 'ARTICLE', 'TABLE', 'TR']);
  const isBlock = (n) => n && n.nodeType === 1 && BLOCKS.has(n.nodeName);
  const skipped = (n) => n.nodeName === 'STYLE' || n.nodeName === 'SCRIPT' || n.getAttribute('contenteditable') === 'false' || n.hasAttribute('data-nugget-ignore');

  function hasContent(n) {
    if (n.nodeType === 3) return n.data.length > 0;
    if (n.nodeType !== 1 || skipped(n)) return false;
    return isBlock(n) || n.nodeName === 'BR' || n.textContent.length > 0 || !!n.querySelector('br');
  }

  /** A <br> that only keeps an empty line (or the end of a line) open: nothing but a block
   *  boundary follows it inside its block. Such a <br> adds no line of its own. */
  function isTrailingBr(br, root) {
    let n = br;
    while (n && n !== root) {
      for (let s = n.nextSibling; s; s = s.nextSibling) if (hasContent(s)) return isBlock(s);
      n = n.parentNode;
      if (isBlock(n)) return true;
    }
    return true;
  }

  /**
   * Read the paragraphs of an editable element the way it renders: blocks start new
   * paragraphs, a <br> ends a line unless it is a placeholder, an empty block is an empty
   * paragraph, NBSP reads as a space. `points` (DOM boundary points) are mapped to
   * {p, o} model positions in the same pass, so selection mapping works on any DOM shape.
   */
  N.readParas = function readParas(root, points) {
    points = points || [];
    const paras = [];
    let cur = null;
    const pos = points.map(() => null);
    const open = () => {
      if (cur === null) cur = '';
    };
    const close = () => {
      if (cur !== null) {
        paras.push(cur);
        cur = null;
      }
    };
    const mark = (node, k) => {
      for (let i = 0; i < points.length; i++)
        if (pos[i] === null && points[i].node === node && points[i].offset === k)
          pos[i] = { p: paras.length, o: cur === null ? 0 : cur.length };
    };
    function walk(el) {
      const kids = el.childNodes;
      for (let i = 0; i < kids.length; i++) {
        mark(el, i);
        const c = kids[i];
        if (c.nodeType === 3) {
          for (let j = 0; j < points.length; j++)
            if (pos[j] === null && points[j].node === c) {
              open();
              pos[j] = { p: paras.length, o: cur.length + Math.min(points[j].offset, c.data.length) };
            }
          if (c.data.length) {
            open();
            cur += c.data;
          }
        } else if (c.nodeType === 1) {
          if (skipped(c)) continue;
          if (c.nodeName === 'BR') {
            open();
            if (!isTrailingBr(c, root)) {
              paras.push(cur);
              cur = '';
            }
          } else if (isBlock(c)) {
            close();
            const n = paras.length;
            walk(c);
            if (cur === null && paras.length === n) cur = '';
            close();
          } else walk(c);
        }
      }
      mark(el, kids.length);
    }
    walk(root);
    close();
    if (!paras.length) paras.push('');
    const out = paras.map(N.normText);
    const clamp = (q) => {
      if (!q) return null;
      if (q.p >= out.length) return { p: out.length - 1, o: out[out.length - 1].length };
      return { p: q.p, o: Math.min(q.o, out[q.p].length) };
    };
    return { paras: out, pos: pos.map(clamp) };
  };

  const cmpPt = (a, b) => (a.p - b.p) || (a.o - b.o);
  const samePts = (a, b) => cmpPt(a, b) === 0;
  const eqParas = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);
  const cloneSel = (s) => ({ a: { ...s.a }, f: { ...s.f } });

  /* ---------- history ---------- */

  class History {
    constructor() {
      this.clear();
    }
    clear() {
      this.undo = [];
      this.redo = [];
      this.lastSingle = false;
      this.lastTime = 0;
    }
    /** `before` is the state before the change. One input is one step; single-character
     *  insertText inputs within 500 ms of each other merge into one step. */
    record(before, kind, data) {
      const now = Date.now();
      const single = kind === 'insertText' && typeof data === 'string' && data !== '\n' && N.graphemeCount(data) === 1;
      const merge = single && this.lastSingle && now - this.lastTime < 500 && this.undo.length > 0;
      if (!merge) {
        this.undo.push(before);
        if (this.undo.length > 100) this.undo.shift();
      }
      this.lastSingle = single;
      this.lastTime = now;
      this.redo.length = 0;
    }
    breakMerge() {
      this.lastSingle = false;
    }
  }
  N.History = History;

  const DELETE_TYPES = /^delete/;
  const PASTE_TYPES = new Set(['insertFromPaste', 'insertFromDrop', 'insertFromYank', 'insertFromPasteAsQuotation']);

  /* ---------- rich editors with their own model ---------- */

  /**
   * options:
   *  html(paras)        canonical innerHTML of the root for the model
   *  container(root)    element whose children are the paragraph blocks (default root)
   *  policy             'model' | 'observe'
   *  foreign            what to do with DOM mutations no input event announced:
   *                     'accept' (re-read), 'chardata' (accept text edits inside existing
   *                     text nodes, revert anything else), 'revert'
   *  insertTextNewline  'split' (each "\n" in insertText data starts a paragraph) or
   *                     'soft' (kept as a literal line break inside the paragraph)
   *  nativeSingleChar   let the browser insert a typed character when the caret is inside a
   *                     text node, and pick it up from the input event (Lexical)
   *  decorate(ed)       after every render/sync (placeholder classes etc.)
   *  onChange()         model changed
   */
  class RichEditor {
    constructor(root, o) {
      this.root = root;
      this.o = Object.assign({ policy: 'model', foreign: 'revert', insertTextNewline: 'split' }, o);
      this.paras = [''];
      this.sel = { a: { p: 0, o: 0 }, f: { p: 0, o: 0 } };
      this.hist = new History();
      this.composing = false;
      this.pending = null; // {type, data, selBefore} for a native edit in flight
      this.rendering = false;
      this.mo = new MutationObserver(() => this.onMutations());
      this.mo.observe(root, { childList: true, characterData: true, subtree: true });
      root.addEventListener('beforeinput', (e) => this.onBeforeInput(e));
      root.addEventListener('input', (e) => this.onInput(e));
      root.addEventListener('keydown', (e) => this.onKeyDown(e));
      root.addEventListener('paste', (e) => this.onPaste(e));
      root.addEventListener('compositionstart', () => (this.composing = true));
      root.addEventListener('compositionend', (e) => {
        this.composing = false;
        this.sync('insertCompositionText', e.data);
      });
      document.addEventListener('selectionchange', () => {
        if (this.rendering || !this.hasFocus()) return;
        const s = this.readDomSel();
        if (s) this.sel = s;
      });
      this.render(this.sel);
    }

    /* model <-> DOM */
    text() {
      return this.paras.join('\n');
    }
    undoDepth() {
      return this.hist.undo.length;
    }
    hasFocus() {
      return this.root.contains(document.activeElement);
    }
    blocks() {
      return Array.from((this.o.container ? this.o.container(this.root) : this.root).children);
    }
    canonical() {
      return this.root.innerHTML === this.o.html(this.paras);
    }
    render(sel) {
      this.rendering = true;
      this.root.innerHTML = this.o.html(this.paras);
      if (this.o.decorate) this.o.decorate(this);
      this.mo.takeRecords();
      this.rendering = false;
      if (sel) {
        this.sel = sel;
        if (this.hasFocus()) this.applySel();
      }
    }
    readDomSel() {
      const s = window.getSelection();
      if (!s || !s.rangeCount || !this.root.contains(s.anchorNode) || !this.root.contains(s.focusNode)) return null;
      const { pos } = N.readParas(this.root, [
        { node: s.anchorNode, offset: s.anchorOffset },
        { node: s.focusNode, offset: s.focusOffset },
      ]);
      return pos[0] && pos[1] ? { a: pos[0], f: pos[1] } : null;
    }
    rangeToModel(r) {
      const { pos } = N.readParas(this.root, [
        { node: r.startContainer, offset: r.startOffset },
        { node: r.endContainer, offset: r.endOffset },
      ]);
      return pos[0] && pos[1] ? { a: pos[0], f: pos[1] } : null;
    }
    modelToPoint(pt) {
      const b = this.blocks()[pt.p];
      if (!b) return { node: this.root, offset: 0 };
      const tw = document.createTreeWalker(b, NodeFilter.SHOW_TEXT);
      let acc = 0;
      let t;
      let last = null;
      while ((t = tw.nextNode())) {
        last = t;
        if (pt.o <= acc + t.data.length) return { node: t, offset: pt.o - acc };
        acc += t.data.length;
      }
      if (last) return { node: last, offset: last.data.length };
      let el = b;
      while (el.firstElementChild && el.firstElementChild.nodeName !== 'BR') el = el.firstElementChild;
      return { node: el, offset: 0 };
    }
    applySel() {
      const a = this.modelToPoint(this.sel.a);
      const f = this.modelToPoint(this.sel.f);
      const s = window.getSelection();
      if (s) s.setBaseAndExtent(a.node, a.offset, f.node, f.offset);
    }
    endPoint() {
      const p = this.paras.length - 1;
      return { p, o: this.paras[p].length };
    }

    /* commands */
    reset(text) {
      this.paras = N.normNewlines(text).split('\n');
      this.hist.clear();
      this.pending = null;
      const e = this.endPoint();
      this.render({ a: e, f: { ...e } });
      this.focusEnd();
    }
    setInitial(text) {
      this.paras = N.normNewlines(text).split('\n');
      const e = this.endPoint();
      this.render({ a: e, f: { ...e } });
    }
    focusEnd() {
      this.root.focus({ preventScroll: true });
      const e = this.endPoint();
      this.sel = { a: e, f: { ...e } };
      this.applySel();
    }

    /* model edits */
    ordered(sel) {
      return cmpPt(sel.a, sel.f) <= 0 ? [sel.a, sel.f] : [sel.f, sel.a];
    }
    /** Replace [s, e) with text; "\n" starts a paragraph when split, else stays literal. */
    replace(s, e, text, split) {
      text = N.normNewlines(text);
      const before = this.paras[s.p].slice(0, s.o);
      const after = this.paras[e.p].slice(e.o);
      const lines = split ? text.split('\n') : [text];
      const last = lines.length - 1;
      const mid = lines.map((l, i) => (i === 0 ? before : '') + l + (i === last ? after : ''));
      this.paras.splice(s.p, e.p - s.p + 1, ...mid);
      const caret = { p: s.p + last, o: (last === 0 ? before.length : 0) + lines[last].length };
      this.sel = { a: caret, f: { ...caret } };
    }
    edit(kind, data, fn) {
      const before = { paras: this.paras.slice(), sel: cloneSel(this.sel) };
      fn();
      if (!eqParas(before.paras, this.paras)) {
        this.hist.record(before, kind, data);
        this.render(this.sel);
        this.changed();
      } else this.render(this.sel);
    }
    changed() {
      if (this.o.onChange) this.o.onChange();
    }
    deleteFrom(kind, s, e) {
      if (samePts(s, e)) {
        // nothing selected: delete one grapheme / word / line in the direction of the input
        const t = this.paras[s.p];
        const back = /Backward/.test(kind) || kind === 'deleteContent';
        if (back) {
          if (s.o === 0) {
            if (s.p === 0) return;
            s = { p: s.p - 1, o: this.paras[s.p - 1].length };
          } else if (/Word/.test(kind)) {
            let o = s.o;
            while (o > 0 && /\s/.test(t[o - 1])) o--;
            while (o > 0 && !/\s/.test(t[o - 1])) o--;
            s = { p: s.p, o };
          } else if (/Line/.test(kind)) s = { p: s.p, o: 0 };
          else s = { p: s.p, o: prevBoundary(t, s.o) };
        } else {
          if (e.o >= t.length) {
            if (e.p >= this.paras.length - 1) return;
            e = { p: e.p + 1, o: 0 };
          } else if (/Word/.test(kind)) {
            let o = e.o;
            while (o < t.length && /\s/.test(t[o])) o++;
            while (o < t.length && !/\s/.test(t[o])) o++;
            e = { p: e.p, o };
          } else if (/Line/.test(kind)) e = { p: e.p, o: t.length };
          else e = { p: e.p, o: nextBoundary(t, e.o) };
        }
      }
      this.replace(s, e, '', true);
    }
    undo() {
      const s = this.hist.undo.pop();
      this.hist.breakMerge();
      if (!s) return;
      this.hist.redo.push({ paras: this.paras.slice(), sel: cloneSel(this.sel) });
      this.paras = s.paras;
      this.render(s.sel);
      this.changed();
    }
    redo() {
      const s = this.hist.redo.pop();
      this.hist.breakMerge();
      if (!s) return;
      this.hist.undo.push({ paras: this.paras.slice(), sel: cloneSel(this.sel) });
      this.paras = s.paras;
      this.render(s.sel);
      this.changed();
    }
    pasteText(text, kind) {
      // Pasted plain text: every "\n" starts a paragraph, "\n\n" leaves an empty one.
      const [s, e] = this.ordered(this.sel);
      this.edit(kind, null, () => this.replace(s, e, text, true));
    }

    /* events */
    onKeyDown(e) {
      const mod = N.isMac ? e.metaKey : e.ctrlKey;
      const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
      if (mod && !e.altKey && k === 'z') {
        e.preventDefault();
        e.shiftKey ? this.redo() : this.undo();
        return;
      }
      if (mod && !N.isMac && !e.altKey && k === 'y') {
        e.preventDefault();
        this.redo();
        return;
      }
      // All five frameworks handle Enter themselves on keydown (split the block).
      if (e.key === 'Enter' && !mod && !e.altKey && !e.isComposing && e.keyCode !== 229) {
        e.preventDefault();
        this.sel = this.readDomSel() || this.sel;
        const [s, en] = this.ordered(this.sel);
        this.edit(e.shiftKey ? 'insertLineBreak' : 'insertParagraph', null, () => this.replace(s, en, '\n', true));
      }
    }
    onPaste(e) {
      const dt = e.clipboardData;
      if (!dt) return;
      e.preventDefault();
      this.sel = this.readDomSel() || this.sel;
      this.pasteText(dt.getData('text/plain') || '', 'insertFromPaste');
    }
    eventText(e) {
      if (e.data != null) return e.data;
      if (e.dataTransfer) return e.dataTransfer.getData('text/plain');
      return null;
    }
    onBeforeInput(e) {
      const t = e.inputType;
      if (t === 'historyUndo' || t === 'historyRedo') {
        e.preventDefault();
        t === 'historyUndo' ? this.undo() : this.redo();
        return;
      }
      const domSel = this.readDomSel() || this.sel;
      if (PASTE_TYPES.has(t) && e.dataTransfer) {
        // Reached only when no paste event was handled (e.g. a synthetic insertFromPaste).
        e.preventDefault();
        this.sel = domSel;
        this.pasteText(this.eventText(e) || '', t);
        return;
      }
      if (this.o.policy === 'observe' || t === 'insertCompositionText' || !e.cancelable) {
        // Native edit; picked up from the DOM afterwards.
        this.pending = { type: t, data: e.data, selBefore: domSel };
        return;
      }
      // policy 'model': the editor owns the edit.
      let sel = domSel;
      const ranges = e.getTargetRanges ? e.getTargetRanges() : [];
      if (ranges.length) sel = this.rangeToModel(ranges[0]) || sel;
      let [s, en] = this.ordered(sel);
      if (t === 'insertText' || t === 'insertReplacementText') {
        const data = this.eventText(e);
        if (data == null) return;
        if (
          t === 'insertText' &&
          this.o.nativeSingleChar &&
          !this.composing &&
          samePts(s, en) &&
          !data.includes('\n') &&
          N.graphemeCount(data) === 1 &&
          this.caretInTextNode()
        ) {
          this.pending = { type: t, data, selBefore: domSel };
          return; // the browser inserts it; onInput syncs
        }
        e.preventDefault();
        this.sel = { a: s, f: en };
        this.edit(t, data, () => this.replace(s, en, data, this.o.insertTextNewline === 'split'));
        return;
      }
      if (t === 'insertParagraph' || t === 'insertLineBreak') {
        e.preventDefault();
        this.edit(t, null, () => this.replace(s, en, '\n', true));
        return;
      }
      if (DELETE_TYPES.test(t)) {
        e.preventDefault();
        this.sel = { a: s, f: en };
        this.edit(t, null, () => this.deleteFrom(t, s, en));
        return;
      }
      if (/^format/.test(t)) {
        e.preventDefault(); // no formatting in these fixtures
        return;
      }
      // Anything else (lists, links, transposition, ...) goes native and is re-read.
      this.pending = { type: t, data: e.data, selBefore: domSel };
    }
    caretInTextNode() {
      const s = window.getSelection();
      return !!(s && s.isCollapsed && s.anchorNode && s.anchorNode.nodeType === 3 && this.root.contains(s.anchorNode));
    }
    onInput(e) {
      if (this.rendering) return;
      // An input event announces a native edit: take it into the model.
      this.sync(e.inputType, e.data);
    }
    onMutations() {
      if (this.rendering) return;
      const recs = this.mo.takeRecords();
      // takeRecords() returns only records queued since the callback was scheduled; the
      // callback's own batch is gone, so decide from the DOM rather than the records.
      void recs;
      if (this.o.foreign === 'accept' || this.composing) return this.sync(null, null);
      if (this.o.foreign === 'chardata' && this.sameStructure()) return this.sync('mutation', null);
      // Foreign mutation nobody announced: re-render from the model.
      this.render(this.sel);
    }
    /** True when only text inside the existing text nodes changed. */
    sameStructure() {
      const tpl = document.createElement('div');
      tpl.innerHTML = this.o.html(this.paras);
      const shape = (n) =>
        Array.from(n.childNodes)
          .map((c) => (c.nodeType === 3 ? '#' : c.nodeName + '(' + shape(c) + ')'))
          .join(',');
      return shape(tpl) === shape(this.root);
    }
    sync(kind, data) {
      this.mo.takeRecords();
      const s = window.getSelection();
      const pts =
        s && s.rangeCount && this.root.contains(s.anchorNode)
          ? [
              { node: s.anchorNode, offset: s.anchorOffset },
              { node: s.focusNode, offset: s.focusOffset },
            ]
          : [];
      const { paras, pos } = N.readParas(this.root, pts);
      const sel = pos[0] && pos[1] ? { a: pos[0], f: pos[1] } : this.sel;
      const changed = !eqParas(paras, this.paras);
      if (changed) {
        const p = this.pending;
        const before = { paras: this.paras.slice(), sel: cloneSel((p && p.selBefore) || this.sel) };
        this.hist.record(before, kind || (p && p.type) || 'mutation', data != null ? data : p && p.data);
        this.paras = paras;
      }
      this.pending = null;
      this.sel = sel;
      if (!this.composing && !this.canonical()) this.render(sel);
      else if (this.o.decorate) this.o.decorate(this);
      if (changed) this.changed();
    }
  }
  N.RichEditor = RichEditor;

  /* ---------- browser-native editors ---------- */

  class NativeEditor {
    /** html(paras) renders the initial DOM for reset; the browser does all editing. */
    constructor(root, o) {
      this.root = root;
      this.o = o;
      const fire = () => o.onChange && o.onChange();
      root.addEventListener('input', fire);
      new MutationObserver(fire).observe(root, { childList: true, characterData: true, subtree: true });
    }
    text() {
      return N.readParas(this.root).paras.join('\n');
    }
    undoDepth() {
      return -1;
    }
    setInitial(text) {
      this.root.innerHTML = this.o.html(N.normNewlines(text).split('\n'));
    }
    reset(text) {
      this.setInitial(text);
      this.focusEnd();
    }
    focusEnd() {
      this.root.focus({ preventScroll: true });
      const tw = document.createTreeWalker(this.root, NodeFilter.SHOW_TEXT);
      let last = null;
      let t;
      while ((t = tw.nextNode())) last = t;
      const s = window.getSelection();
      if (last) s.setBaseAndExtent(last, last.data.length, last, last.data.length);
      else {
        let el = this.root;
        while (el.lastElementChild && el.lastElementChild.nodeName !== 'BR') el = el.lastElementChild;
        s.setBaseAndExtent(el, 0, el, 0);
      }
    }
  }
  N.NativeEditor = NativeEditor;
})();
