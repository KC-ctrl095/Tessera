// Runs in Tessera's isolated world of a page (the page's own scripts can't
// see or change it): a compact outline of the page with element refs for
// the agent, and the element lookups behind click/type/select.
(() => {
  if (globalThis.__tsr) return;
  const refs = new Map(); // "e12" -> WeakRef(element)
  const ids = new WeakMap(); // element -> "e12"
  let next = 1;

  const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'HEAD', 'META', 'LINK', 'TITLE', 'svg', 'SVG', 'CANVAS', 'VIDEO', 'AUDIO', 'MAP', 'OBJECT', 'EMBED']);
  const INLINE = new Set(['A', 'SPAN', 'B', 'I', 'EM', 'STRONG', 'SMALL', 'CODE', 'ABBR', 'CITE', 'Q', 'SUB', 'SUP', 'TIME', 'MARK', 'LABEL', 'U', 'S', 'FONT', 'KBD', 'VAR', 'BDI', 'BDO', 'DATA', 'WBR', 'DEL', 'INS', 'DFN', 'SAMP', 'TT', 'BIG', 'STRIKE', 'NOBR', 'IMG', 'PICTURE']);
  const ACTIVE = new Set(['link', 'button', 'checkbox', 'radio', 'switch', 'textbox', 'searchbox', 'password', 'combobox', 'listbox', 'option', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'tab', 'slider', 'spinbutton', 'editor', 'file', 'treeitem']);
  const TYPED = new Set(['textbox', 'searchbox', 'password', 'combobox', 'editor', 'spinbutton']);
  const SECRET_AC = /cc-number|cc-csc|cc-exp|one-time-code|new-password|current-password/i;
  const SENSITIVE = /\b(buy|purchase|order now|place (your )?order|pay|checkout|check out|subscribe|donate|send|post|publish|tweet|reply|transfer|delete|book now|reserve|sign up|create account|confirm (order|purchase|payment))\b/i;

  const clean = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const cut = (s, n) => (s.length > n ? s.slice(0, n - 1) + '…' : s);

  function refOf(el) {
    let r = ids.get(el);
    if (!r) {
      r = 'e' + next++;
      ids.set(el, r);
    }
    refs.set(r, new WeakRef(el));
    return r;
  }
  function byRef(r) {
    const w = refs.get(String(r).replace(/^\[|\]$/g, ''));
    const el = w && w.deref();
    return el && el.isConnected ? el : null;
  }

  function role(el) {
    const r = (el.getAttribute('role') || '').trim().split(/\s+/)[0].toLowerCase();
    if (r && r !== 'presentation' && r !== 'none' && r !== 'generic') return r;
    switch (el.tagName) {
      case 'A':
        return el.hasAttribute('href') ? 'link' : '';
      case 'BUTTON':
      case 'SUMMARY':
        return 'button';
      case 'SELECT':
        return el.multiple || el.size > 1 ? 'listbox' : 'combobox';
      case 'TEXTAREA':
        return 'textbox';
      case 'INPUT': {
        const t = (el.type || 'text').toLowerCase();
        if (['button', 'submit', 'reset', 'image'].includes(t)) return 'button';
        if (t === 'checkbox') return 'checkbox';
        if (t === 'radio') return 'radio';
        if (t === 'range') return 'slider';
        if (t === 'number') return 'spinbutton';
        if (t === 'hidden') return 'hidden';
        if (t === 'file') return 'file';
        if (t === 'search') return 'searchbox';
        if (t === 'password') return 'password';
        return 'textbox';
      }
      case 'H1': case 'H2': case 'H3': case 'H4': case 'H5': case 'H6':
        return 'heading';
    }
    if (el.isContentEditable && !(el.parentElement && el.parentElement.isContentEditable)) return 'editor';
    return '';
  }

  function textOf(el) {
    return clean(el.innerText || el.textContent || '');
  }

  function labelOf(el) {
    const by = el.getAttribute('aria-labelledby');
    if (by) {
      const t = clean(by.split(/\s+/).map((id) => {
        const n = el.ownerDocument.getElementById(id);
        return n ? n.innerText || n.textContent : '';
      }).join(' '));
      if (t) return t;
    }
    const al = clean(el.getAttribute('aria-label'));
    if (al) return al;
    if (el.labels && el.labels.length) {
      const t = clean(Array.from(el.labels).map((l) => l.innerText).join(' '));
      if (t) return t;
    }
    return '';
  }

  function nameOf(el, r) {
    let n = labelOf(el);
    if (!n) {
      if (TYPED.has(r)) {
        n = el.getAttribute('placeholder') || el.getAttribute('title') || el.getAttribute('name') || '';
      } else if (el.tagName === 'INPUT') {
        n = el.value || el.getAttribute('title') || el.getAttribute('alt') || '';
      } else {
        n = textOf(el);
        if (!n) {
          const img = el.querySelector('img[alt]');
          n = (img && img.getAttribute('alt')) || el.getAttribute('title') || '';
          if (!n) {
            const t = el.querySelector('svg title');
            n = t ? t.textContent : '';
          }
        }
      }
    }
    return cut(clean(n), 80);
  }

  function shown(el) {
    try {
      if (!el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) return false;
    } catch (e) {}
    if (el.getAttribute('aria-hidden') === 'true') return false;
    return true;
  }
  function boxed(el) {
    const r = el.getBoundingClientRect();
    return r.width >= 2 && r.height >= 2;
  }

  // Position of an element's box in the top page's viewport (same-origin
  // frames add their offsets).
  function rectOf(el) {
    const r = el.getBoundingClientRect();
    let x = r.left, y = r.top;
    let win = el.ownerDocument.defaultView;
    while (win && win !== window && win.frameElement) {
      const fe = win.frameElement;
      const fr = fe.getBoundingClientRect();
      x += fr.left + fe.clientLeft;
      y += fr.top + fe.clientTop;
      win = win.parent;
    }
    return { x, y, w: r.width, h: r.height };
  }

  function formInfo(el) {
    const form = el.form || el.closest('form');
    if (!form) return null;
    const method = (form.getAttribute('method') || 'get').toLowerCase();
    const fields = Array.from(form.elements).filter((f) => f.tagName !== 'BUTTON' && f.type !== 'hidden' && f.type !== 'submit');
    const search = form.getAttribute('role') === 'search' || !!form.closest('[role=search]') ||
      (fields.length <= 2 && fields.some((f) => f.type === 'search' || /^(q|query|search|s)$/i.test(f.name || '')));
    return {
      method,
      password: !!form.querySelector('input[type=password]'),
      search,
      name: cut(clean(form.getAttribute('aria-label') || form.getAttribute('name') || form.id || ''), 40),
    };
  }

  function info(el) {
    const r = role(el);
    const tag = el.tagName.toLowerCase();
    const type = tag === 'input' ? (el.type || 'text').toLowerCase() : '';
    const form = formInfo(el);
    const submit = (tag === 'button' && (el.type || 'submit') === 'submit' && !!form) ||
      (tag === 'input' && (type === 'submit' || type === 'image'));
    const name = nameOf(el, r);
    const href = el.closest('a[href]');
    return {
      ref: refOf(el),
      role: r || tag,
      name,
      tag,
      type,
      password: type === 'password' || SECRET_AC.test(el.getAttribute('autocomplete') || ''),
      editable: TYPED.has(r) && r !== 'combobox' || (tag === 'input' && !['button', 'submit', 'reset', 'image', 'checkbox', 'radio', 'range', 'file', 'hidden', 'color'].includes(type)) || el.isContentEditable,
      disabled: !!el.disabled || el.getAttribute('aria-disabled') === 'true',
      form,
      submit,
      sensitive: SENSITIVE.test(name) || SENSITIVE.test(el.getAttribute('value') || ''),
      href: href ? href.href : '',
      download: !!(href && href.hasAttribute('download')),
      file: type === 'file',
    };
  }

  // The interactive element at or around a node.
  function actionable(node) {
    let el = node && node.nodeType === 1 ? node : node && node.parentElement;
    for (let e = el; e; e = e.parentElement || (e.getRootNode && e.getRootNode().host)) {
      const r = role(e);
      if (ACTIVE.has(r) || e.tagName === 'LABEL') return e;
    }
    return el;
  }

  // Where to click an element: scroll it into view, then its centre, and
  // what (if anything) covers that point.
  function aim(el) {
    let r = rectOf(el);
    const vw = innerWidth, vh = innerHeight;
    if (r.y < 0 || r.x < 0 || r.y + r.h > vh || r.x + r.w > vw) {
      el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
      r = rectOf(el);
    }
    // A link that wraps onto two lines: aim at its first line.
    const local = el.getBoundingClientRect();
    const rects = el.getClientRects();
    const first = rects.length > 1 ? rects[0] : local;
    const dx = first.left - local.left + first.width / 2;
    const dy = first.top - local.top + first.height / 2;
    const x = Math.round(Math.min(Math.max(r.x + dx, 1), vw - 1));
    const y = Math.round(Math.min(Math.max(r.y + dy, 1), vh - 1));
    let covered = '';
    const own = el.ownerDocument;
    const hit = own.elementFromPoint(local.left + dx, local.top + dy);
    if (hit && hit !== el && !el.contains(hit) && !hit.contains(el)) {
      const a = actionable(hit);
      covered = (role(a) || a.tagName.toLowerCase()) + ' "' + nameOf(a, role(a)) + '"';
    }
    return { x, y, w: Math.round(r.w), h: Math.round(r.h), left: Math.round(r.x), top: Math.round(r.y), covered };
  }

  // What an action changed (see `watch`); the outlines `flash` draws are
  // not the page's.
  let watching = null;
  const ours = new WeakSet();

  // The state an action on `el` may change, as words.
  function stateOf(el) {
    if (!el) return '';
    const parts = [];
    const t = (el.type || '').toLowerCase();
    if (el.tagName === 'INPUT' && (t === 'checkbox' || t === 'radio')) parts.push(el.checked ? 'checked' : 'unchecked');
    for (const a of ['aria-checked', 'aria-pressed', 'aria-expanded', 'aria-selected']) {
      const v = el.getAttribute(a);
      if (v !== null) parts.push(a.slice(5) + ': ' + v);
    }
    if (el.tagName === 'SELECT') parts.push('value: ' + JSON.stringify(cut(clean(el.value), 40)));
    if (el.tagName === 'DETAILS' || el.tagName === 'DIALOG') parts.push(el.open ? 'open' : 'closed');
    if (el.disabled) parts.push('disabled');
    return parts.join(', ');
  }

  // Content scripts can't see changes in: a canvas, video or plug-in, or
  // another site's frame.
  function opaque(el) {
    if (['CANVAS', 'VIDEO', 'EMBED', 'OBJECT'].includes(el.tagName)) return true;
    if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') {
      try {
        return !el.contentDocument;
      } catch (e) {
        return true;
      }
    }
    return false;
  }

  // `button "Next" [e7]`
  function label(el) {
    const r = role(el);
    const n = nameOf(el, r);
    return (r || el.tagName.toLowerCase()) + (n ? ' ' + JSON.stringify(n) : '') + ' [' + refOf(el) + ']';
  }

  function findByText(text) {
    const want = clean(text).toLowerCase();
    if (!want) return null;
    const all = [];
    const walk = (root) => {
      for (const el of root.querySelectorAll('*')) {
        const r = role(el);
        if ((ACTIVE.has(r) || el.tagName === 'LABEL') && shown(el) && boxed(el)) all.push([el, r]);
        if (el.shadowRoot) walk(el.shadowRoot);
      }
    };
    walk(document);
    const named = all.map(([el, r]) => [el, nameOf(el, r).toLowerCase()]);
    const inView = (el) => {
      const b = rectOf(el);
      return b.y + b.h > 0 && b.y < innerHeight;
    };
    const pick = (list) => list.find(([el]) => inView(el)) || list[0];
    let hit = pick(named.filter(([, n]) => n === want));
    if (!hit) hit = pick(named.filter(([, n]) => n.includes(want)));
    if (hit) return hit[0];
    // Plain text on the page (e.g. a div that acts as a button).
    const tw = document.createTreeWalker(document.body || document.documentElement, NodeFilter.SHOW_TEXT);
    for (let n = tw.nextNode(); n; n = tw.nextNode()) {
      if (clean(n.textContent).toLowerCase().includes(want) && n.parentElement && shown(n.parentElement)) {
        return actionable(n.parentElement);
      }
    }
    return null;
  }

  function target(t) {
    if (t.ref) {
      const el = byRef(t.ref);
      if (!el) throw new Error(`no element ${t.ref} on this page now (refs come from the latest snapshot; take a new one)`);
      return el;
    }
    if (t.text) {
      const el = findByText(t.text);
      if (!el) throw new Error(`nothing on the page matches "${t.text}"`);
      return el;
    }
    if (t.x !== undefined) {
      if (!Number.isFinite(t.x) || !Number.isFinite(t.y)) throw new Error('give x and y as numbers');
      const el = document.elementFromPoint(t.x, t.y);
      if (!el) throw new Error('that point is outside the page');
      return actionable(el);
    }
    throw new Error('give a ref, text or x and y');
  }

  function valueOf(el, r) {
    const t = (el.type || '').toLowerCase();
    if (r === 'password' || SECRET_AC.test(el.getAttribute('autocomplete') || '')) {
      return el.value ? ' (filled)' : ' (empty)';
    }
    if (r === 'checkbox' || r === 'radio' || r === 'switch' || r === 'menuitemcheckbox' || r === 'menuitemradio') {
      const on = el.tagName === 'INPUT' ? el.checked : el.getAttribute('aria-checked') === 'true';
      return on ? ' [x]' : ' [ ]';
    }
    if (el.tagName === 'SELECT') {
      const sel = Array.from(el.selectedOptions).map((o) => clean(o.label || o.text));
      const opts = Array.from(el.options).map((o) => clean(o.label || o.text)).filter(Boolean);
      let s = sel.length ? ' = ' + JSON.stringify(cut(sel.join(', '), 60)) : '';
      s += opts.length <= 8 ? ' {' + opts.map((o) => cut(o, 30)).join(' | ') + '}' : ` (${opts.length} options)`;
      return s;
    }
    if (r === 'editor') {
      const v = cut(clean(el.innerText), 80);
      return v ? ' = ' + JSON.stringify(v) : ' (empty)';
    }
    if (TYPED.has(r) || r === 'slider') {
      if (t === 'file') return '';
      const v = el.value || '';
      return v ? ' = ' + JSON.stringify(cut(v, 80)) : ' (empty)';
    }
    return '';
  }

  function shortHref(a) {
    const h = a.getAttribute('href') || '';
    if (!h || h.startsWith('#') || /^javascript:/i.test(h)) return '';
    try {
      const u = new URL(a.href);
      if (u.origin === location.origin) return u.pathname + u.search;
      return cut(u.host.replace(/^www\./, '') + u.pathname.replace(/\/$/, '') + u.search, 70);
    } catch (e) {
      return '';
    }
  }

  function line(el, r, depth) {
    const tag = el.tagName;
    let kind = r;
    const t = tag === 'INPUT' ? (el.type || 'text').toLowerCase() : '';
    if (r === 'textbox' && t && !['text', ''].includes(t)) kind = 'textbox[' + t + ']';
    if (tag === 'TEXTAREA') kind = 'textbox[multiline]';
    const name = nameOf(el, r);
    let s = '  '.repeat(depth) + '[' + refOf(el) + '] ' + kind + (name ? ' ' + JSON.stringify(name) : '');
    s += valueOf(el, r);
    if (r === 'link') {
      const h = shortHref(el);
      if (h && h !== name) s += ' → ' + h;
    }
    if ((tag === 'BUTTON' && (el.type || 'submit') === 'submit' && el.form) || (tag === 'INPUT' && t === 'submit')) s += ' (submit)';
    const exp = el.getAttribute('aria-expanded');
    if (exp) s += exp === 'true' ? ' (expanded)' : ' (collapsed)';
    if (el.getAttribute('aria-selected') === 'true' || el.getAttribute('aria-current') && el.getAttribute('aria-current') !== 'false') s += ' (current)';
    if (el.disabled || el.getAttribute('aria-disabled') === 'true') s += ' (disabled)';
    if (el.required) s += ' (required)';
    if (el === deepActive()) s += ' (focused)';
    return s;
  }

  function deepActive() {
    let a = document.activeElement;
    for (;;) {
      if (a && a.shadowRoot && a.shadowRoot.activeElement) a = a.shadowRoot.activeElement;
      else if (a && a.tagName === 'IFRAME') {
        try {
          const d = a.contentDocument;
          if (!d || !d.activeElement) break;
          a = d.activeElement;
        } catch (e) {
          break;
        }
      } else break;
    }
    return a;
  }

  function snapshot(opts) {
    const max = opts.max || 12000;
    const filter = clean(opts.filter || '').toLowerCase();
    const out = [];
    let size = 0, dropped = 0, nodes = 0;
    let buf = '';
    let below = false;
    const vh = innerHeight;
    const push = (s, el) => {
      if (!s) return;
      if (filter && !s.toLowerCase().includes(filter)) return;
      if (!below && el && !filter) {
        const r = rectOf(el);
        if (r.y > vh + 1) {
          below = true;
          push('— below the visible area —');
        }
      }
      if (out.length && out[out.length - 1] === s) return;
      if (size + s.length > max) {
        dropped++;
        return;
      }
      out.push(s);
      size += s.length + 1;
    };
    let blockEl = null;
    const flush = (depth) => {
      const t = clean(buf);
      buf = '';
      if (t) push('  '.repeat(depth) + cut(t, 300), blockEl);
    };
    const kids = (n) => {
      if (n.shadowRoot) return n.shadowRoot.childNodes;
      if (n.tagName === 'SLOT') {
        const a = n.assignedNodes({ flatten: true });
        return a.length ? a : n.childNodes;
      }
      return n.childNodes;
    };
    const walk = (node, depth) => {
      for (const c of kids(node)) {
        if (++nodes > 40000) return;
        if (c.nodeType === 3) {
          buf += c.textContent;
          continue;
        }
        if (c.nodeType !== 1) continue;
        const el = c;
        const tag = el.tagName;
        if (SKIP.has(tag)) continue;
        if (tag === 'BR') {
          buf += ' ';
          continue;
        }
        if (!shown(el)) continue;
        const r = role(el);
        if (r === 'hidden') continue;
        if (r === 'heading') {
          flush(depth);
          const lvl = Number(el.getAttribute('aria-level')) || Number(tag.slice(1)) || 2;
          const t = cut(textOf(el), 150);
          if (t) push('  '.repeat(depth) + '#'.repeat(Math.min(lvl, 6)) + ' ' + t, el);
          continue;
        }
        if (ACTIVE.has(r)) {
          if (!boxed(el) && r !== 'radio' && r !== 'checkbox') continue;
          flush(depth);
          push(line(el, r, depth), el);
          // Options of a listbox or menu are listed as their own lines.
          if (r === 'listbox' && tag !== 'SELECT') walk(el, depth + 1);
          continue;
        }
        // A label names its control (shown on the control's line): only
        // the controls inside it are listed.
        if (tag === 'LABEL' && el.control) {
          for (const inner of el.querySelectorAll('*')) {
            const ir = role(inner);
            if (ACTIVE.has(ir) && shown(inner) && (boxed(inner) || ir === 'radio' || ir === 'checkbox')) {
              flush(depth);
              push(line(inner, ir, depth), inner);
            }
          }
          continue;
        }
        if (tag === 'IMG') {
          const alt = clean(el.getAttribute('alt'));
          if (alt && boxed(el) && el.width >= 32) buf += ' [image: ' + cut(alt, 80) + '] ';
          continue;
        }
        if (tag === 'IFRAME' || tag === 'FRAME') {
          flush(depth);
          let doc = null;
          try {
            doc = el.contentDocument;
          } catch (e) {}
          if (doc && doc.body) {
            push('  '.repeat(depth) + 'frame ' + JSON.stringify(cut(clean(el.title || el.name || ''), 60)) + ':', el);
            walk(doc.body, depth + 1);
            flush(depth + 1);
          } else if (boxed(el)) {
            const b = rectOf(el);
            push('  '.repeat(depth) + `frame ${JSON.stringify(cut(clean(el.title || el.name || ''), 60))} at x=${Math.round(b.x + b.w / 2)} y=${Math.round(b.y + b.h / 2)} (another site's frame: use screenshot and click x/y)`, el);
          }
          continue;
        }
        // A click target without a role (e.g. a card that is a link).
        if (!r && (el.hasAttribute('onclick') || el.tabIndex >= 0 && el.hasAttribute('tabindex')) && boxed(el) && !el.querySelector('a[href],button,input,select,textarea,[role]')) {
          flush(depth);
          push('  '.repeat(depth) + '[' + refOf(el) + '] clickable ' + JSON.stringify(cut(textOf(el), 80)), el);
          continue;
        }
        const section = tag === 'FORM' || tag === 'DIALOG' || r === 'dialog' || r === 'alertdialog' || r === 'form' || r === 'search';
        if (section) {
          flush(depth);
          const nm = cut(clean(labelOf(el) || el.getAttribute('name') || el.id || ''), 50);
          push('  '.repeat(depth) + (tag === 'FORM' || r === 'form' || r === 'search' ? 'form' : 'dialog') + (nm ? ' ' + JSON.stringify(nm) : '') + ':', el);
          walk(el, depth + 1);
          flush(depth + 1);
          continue;
        }
        if (INLINE.has(tag)) {
          walk(el, depth);
        } else {
          flush(depth);
          const prev = blockEl;
          blockEl = el;
          walk(el, depth);
          flush(depth);
          blockEl = prev;
        }
      }
    };
    if (document.body) walk(document.body, 0);
    flush(0);
    const se = document.scrollingElement || document.documentElement;
    const head = {
      title: document.title,
      url: location.href,
      vw: innerWidth,
      vh: innerHeight,
      sy: Math.round(scrollY),
      sh: Math.round(se ? se.scrollHeight : 0),
      dropped,
    };
    return { head, text: out.join('\n') };
  }

  globalThis.__tsr = {
    snapshot,
    // What an action would act on (and where), without acting.
    probe(t, scroll) {
      const point = t.x !== undefined && !t.ref && !t.text;
      if (point && (t.x < 0 || t.y < 0 || t.x >= innerWidth || t.y >= innerHeight)) {
        throw new Error(`(${Math.round(t.x)}, ${Math.round(t.y)}) is outside the visible page (${innerWidth}×${innerHeight} CSS pixels); take a new screenshot`);
      }
      const el = target(t);
      const i = info(el);
      if (point) {
        // A point is clicked exactly there (on a canvas, a map or another
        // site's frame there is no element to aim at); the element under
        // it only says what the click is on (for the approval checks).
        Object.assign(i, { x: t.x, y: t.y, w: 16, h: 16, left: t.x - 8, top: t.y - 8, covered: '' });
      } else if (scroll !== false) Object.assign(i, aim(el));
      return i;
    },
    // Start noting what an action changes: the page's content, the focus
    // and the state of the element acted on (`changes` tells).
    watch(t) {
      let el = null;
      try {
        el = t ? target(t) : null;
      } catch (e) {}
      if (el && el.tagName === 'LABEL' && el.control) el = el.control;
      if (watching) watching.obs.forEach((o) => o.disconnect());
      const w = { count: 0, obs: [], url: location.href, sy: Math.round(scrollY), subject: el ? new WeakRef(el) : null, state: stateOf(el), focus: new WeakRef(deepActive() || document), opaque: !!el && opaque(el) };
      const count = (recs) => {
        w.count += recs.filter((r) => !ours.has(r.target) && ![...r.addedNodes, ...r.removedNodes].some((n) => ours.has(n))).length;
      };
      const docs = new Set([document]);
      if (el && el.ownerDocument) docs.add(el.ownerDocument);
      for (const d of docs) {
        const o = new MutationObserver(count);
        o.observe(d, { subtree: true, childList: true, attributes: true, characterData: true });
        w.obs.push(o);
      }
      w.flush = () => w.obs.forEach((o) => count(o.takeRecords()));
      watching = w;
      return true;
    },
    changes() {
      const w = watching;
      watching = null;
      // A new document since: what the page shows now is all new.
      if (!w) return { fresh: true };
      w.flush();
      w.obs.forEach((o) => o.disconnect());
      const out = { mutations: w.count, opaque: w.opaque };
      const subject = w.subject && w.subject.deref();
      if (subject) {
        const now = subject.isConnected ? stateOf(subject) : 'gone';
        if (now !== w.state) out.state = [w.state, now];
        else if (now) out.unchanged = now;
      }
      const f = deepActive();
      // Report the actual transition, including focus gained by the field
      // that was clicked. A focus-only click is not a missed click.
      if (f !== (w.focus && w.focus.deref())) out.focus = f && f !== document.body && f !== document.documentElement ? label(f) : 'the page';
      if (Math.round(scrollY) !== w.sy) out.scrolled = true;
      if (location.href !== w.url) out.url = location.href;
      return out;
    },
    focus(t) {
      const el = target(t);
      const r = role(el);
      if (!(TYPED.has(r) || el.isContentEditable || el.tagName === 'INPUT' || el.tagName === 'TEXTAREA')) {
        throw new Error(`${r || el.tagName.toLowerCase()} "${nameOf(el, r)}" isn't a text field`);
      }
      if (el.disabled || el.readOnly) throw new Error('that field is disabled or read-only');
      el.scrollIntoView({ block: 'center', behavior: 'instant' });
      el.focus();
      if (el.isContentEditable) {
        const range = el.ownerDocument.createRange();
        range.selectNodeContents(el);
        const s = el.ownerDocument.getSelection();
        s.removeAllRanges();
        s.addRange(range);
      } else if (typeof el.select === 'function') {
        el.select();
      }
      return info(el);
    },
    select(t, wanted) {
      const el = target(t);
      if (el.tagName !== 'SELECT') throw new Error('not a <select>: click it, then click the option');
      const want = wanted.map((w) => clean(w).toLowerCase());
      const opts = Array.from(el.options);
      const chosen = [];
      for (const w of want) {
        const o = opts.find((o) => o.value.toLowerCase() === w || clean(o.label || o.text).toLowerCase() === w) ||
          opts.find((o) => clean(o.label || o.text).toLowerCase().includes(w));
        if (!o) throw new Error(`no option "${w}"; options: ${opts.slice(0, 20).map((o) => clean(o.label || o.text)).join(' | ')}`);
        chosen.push(o);
      }
      if (!el.multiple && chosen.length > 1) throw new Error('this list takes one option');
      for (const o of opts) o.selected = chosen.includes(o);
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.dispatchEvent(new Event('change', { bubbles: true }));
      return chosen.map((o) => clean(o.label || o.text));
    },
    // Scroll an element into view (the page itself when no target).
    reveal(t) {
      const el = target(t);
      el.scrollIntoView({ block: 'center', behavior: 'instant' });
      return info(el);
    },
    async waitText(text, ms, gone) {
      const want = clean(text).toLowerCase();
      const end = Date.now() + ms;
      for (;;) {
        const has = clean(document.body ? document.body.innerText : '').toLowerCase().includes(want);
        if (has !== !!gone) return true;
        if (Date.now() > end) return false;
        await new Promise((r) => setTimeout(r, 200));
      }
    },
    focused() {
      const a = deepActive();
      if (!a || a === document.body) return null;
      return info(a);
    },
    // A brief outline around what the agent is about to act on, for the
    // people watching.
    flash(x, y, w, h) {
      const d = document.createElement('div');
      ours.add(d);
      d.style.cssText = `position:fixed;left:${x - 3}px;top:${y - 3}px;width:${w + 6}px;height:${h + 6}px;border:2px solid #d4ad5e;border-radius:6px;box-shadow:0 0 0 3px rgba(212,173,94,.35);pointer-events:none;z-index:2147483647;transition:opacity .4s`;
      document.documentElement.appendChild(d);
      setTimeout(() => (d.style.opacity = '0'), 500);
      setTimeout(() => d.remove(), 950);
      return true;
    },
  };
})();
