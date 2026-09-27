// The browser side of JavaScript in EverBrowser: document, elements,
// events, timers, fetch and friends, built on __native(op, ...args),
// which reaches the page's DOM in Rust (web/script.rs).
(function () {
'use strict';
const N = __native;
const G = globalThis;
const wrappers = new Map();
const ELEMENT = 1, TEXT = 3, DOC = 9, FRAGMENT = 11;

function wrap(id) {
  if (id === null || id === undefined || id < 0) return null;
  let w = wrappers.get(id);
  if (w) return w;
  const t = N('type', id);
  if (t === DOC) return document;
  if (t === TEXT) w = Object.create(Text.prototype);
  else {
    const tag = N('tag', id);
    const cls = tag === '#fragment' ? DocumentFragment
      : tag === 'input' ? HTMLInputElement
      : tag === 'form' ? HTMLFormElement
      : tag === 'select' ? HTMLSelectElement
      : tag === 'a' ? HTMLAnchorElement
      : tag === 'img' ? HTMLImageElement
      : tag === 'script' ? HTMLScriptElement
      : tag === 'template' ? HTMLTemplateElement
      : HTMLElement;
    w = Object.create(cls.prototype);
  }
  Object.defineProperty(w, '_id', { value: id });
  wrappers.set(id, w);
  return w;
}
const ids = s => (s ? String(s).split(',').map(Number) : []);
const list = s => ids(s).map(wrap);
const idOf = n => {
  if (n === null || n === undefined) return null;
  if (typeof n === 'string') return N('ctext', n);
  if (n === document) return 0;
  return n._id;
};

// ---- events -------------------------------------------------------------------
class Event {
  constructor(type, init) {
    init = init || {};
    this.type = String(type);
    this.bubbles = !!init.bubbles;
    this.cancelable = !!init.cancelable;
    this.composed = !!init.composed;
    this.defaultPrevented = false;
    this.target = null;
    this.currentTarget = null;
    this.eventPhase = 0;
    this.timeStamp = performance.now();
    this.isTrusted = false;
    this._stop = false;
    this._stopNow = false;
  }
  preventDefault() { if (this.cancelable !== false) this.defaultPrevented = true; }
  stopPropagation() { this._stop = true; }
  stopImmediatePropagation() { this._stop = true; this._stopNow = true; }
  composedPath() { return this._path || []; }
  get returnValue() { return !this.defaultPrevented; }
  set returnValue(v) { if (!v) this.preventDefault(); }
  get srcElement() { return this.target; }
  initEvent(type, bubbles, cancelable) { this.type = type; this.bubbles = bubbles; this.cancelable = cancelable; }
}
Event.NONE = 0; Event.CAPTURING_PHASE = 1; Event.AT_TARGET = 2; Event.BUBBLING_PHASE = 3;
class CustomEvent extends Event { constructor(t, i) { super(t, i); this.detail = i && i.detail !== undefined ? i.detail : null; } }
class UIEvent extends Event { constructor(t, i) { super(t, i); this.view = G; this.detail = 0; } }
class MouseEvent extends UIEvent {
  constructor(t, i) {
    super(t, i); i = i || {};
    this.clientX = i.clientX || 0; this.clientY = i.clientY || 0;
    this.pageX = this.clientX; this.pageY = this.clientY; this.screenX = this.clientX; this.screenY = this.clientY;
    this.offsetX = 0; this.offsetY = 0;
    this.button = i.button || 0; this.buttons = i.buttons || 0;
    this.ctrlKey = !!i.ctrlKey; this.shiftKey = !!i.shiftKey; this.altKey = !!i.altKey; this.metaKey = !!i.metaKey;
    this.relatedTarget = i.relatedTarget || null;
  }
}
class PointerEvent extends MouseEvent { constructor(t, i) { super(t, i); this.pointerId = 1; this.pointerType = 'mouse'; this.isPrimary = true; } }
class KeyboardEvent extends UIEvent {
  constructor(t, i) {
    super(t, i); i = i || {};
    this.key = i.key || ''; this.code = i.code || ''; this.keyCode = i.keyCode || 0; this.which = this.keyCode; this.charCode = i.charCode || 0;
    this.ctrlKey = !!i.ctrlKey; this.shiftKey = !!i.shiftKey; this.altKey = !!i.altKey; this.metaKey = !!i.metaKey; this.repeat = false;
  }
}
class FocusEvent extends UIEvent {}
class InputEvent extends UIEvent { constructor(t, i) { super(t, i); this.data = i && i.data || null; this.inputType = i && i.inputType || ''; } }
class SubmitEvent extends Event { constructor(t, i) { super(t, i); this.submitter = i && i.submitter || null; } }
class ErrorEvent extends Event { constructor(t, i) { super(t, i); this.message = i && i.message || ''; this.error = i && i.error; } }
class ProgressEvent extends Event { constructor(t, i) { super(t, i); this.loaded = 0; this.total = 0; this.lengthComputable = false; } }
class MessageEvent extends Event { constructor(t, i) { super(t, i); this.data = i && i.data; } }
class PopStateEvent extends Event { constructor(t, i) { super(t, i); this.state = i && i.state; } }

class EventTarget {
  addEventListener(type, fn, opts) {
    if (!fn) return;
    const capture = typeof opts === 'boolean' ? opts : !!(opts && opts.capture);
    const once = !!(opts && typeof opts === 'object' && opts.once);
    if (!this._l) Object.defineProperty(this, '_l', { value: {}, writable: true });
    const arr = this._l[type] || (this._l[type] = []);
    if (!arr.some(l => l.fn === fn && l.capture === capture)) arr.push({ fn, capture, once });
    if (opts && typeof opts === 'object' && opts.signal) opts.signal.addEventListener('abort', () => this.removeEventListener(type, fn, opts));
  }
  removeEventListener(type, fn, opts) {
    const capture = typeof opts === 'boolean' ? opts : !!(opts && opts.capture);
    const arr = this._l && this._l[type];
    if (arr) this._l[type] = arr.filter(l => !(l.fn === fn && l.capture === capture));
  }
  dispatchEvent(ev) {
    if (!(ev instanceof Event)) { const e = new Event(ev.type || ev); ev = e; }
    ev.target = this;
    const path = [];
    if (this !== G && this !== document && this._id !== undefined) {
      for (let n = this; n; n = n.parentNode) path.push(n);
      if (path[path.length - 1] === document) path.push(G);
    } else {
      path.push(this);
      if (this === document) path.push(G);
    }
    ev._path = path;
    for (let i = path.length - 1; i > 0 && !ev._stop; i--) fire(path[i], ev, 1);
    if (!ev._stop) fire(this, ev, 2);
    if (ev.bubbles) for (let i = 1; i < path.length && !ev._stop; i++) fire(path[i], ev, 3);
    ev.currentTarget = null;
    ev.eventPhase = 0;
    return !ev.defaultPrevented;
  }
}
function fire(node, ev, phase) {
  ev.currentTarget = node;
  ev.eventPhase = phase;
  const arr = node._l && node._l[ev.type];
  if (arr) {
    for (const l of arr.slice()) {
      if (phase === 1 && !l.capture) continue;
      if (phase === 3 && l.capture) continue;
      if (l.once) node.removeEventListener(ev.type, l.fn, l.capture);
      try {
        if (typeof l.fn === 'function') { if (l.fn.call(node, ev) === false) ev.preventDefault(); }
        else if (l.fn && typeof l.fn.handleEvent === 'function') l.fn.handleEvent(ev);
      } catch (e) { reportError(e); }
      if (ev._stopNow) return;
    }
  }
  if (phase !== 1) {
    // onclick = ... and onclick="..."
    const prop = node['_on' + ev.type];
    let handler = prop;
    if (!handler && node._id !== undefined && N('type', node._id) === ELEMENT) {
      const code = N('attr', node._id, 'on' + ev.type);
      if (code) {
        try { handler = new Function('event', code); } catch (e) { reportError(e); }
      }
    }
    if (typeof handler === 'function') {
      try { if (handler.call(node, ev) === false) ev.preventDefault(); } catch (e) { reportError(e); }
    }
  }
}
function reportError(e) {
  console.error('Uncaught ' + (e && e.stack ? e + '\n' + e.stack : e));
}
const EVENT_PROPS = ['click', 'dblclick', 'mousedown', 'mouseup', 'mouseover', 'mouseout', 'mousemove', 'mouseenter', 'mouseleave',
  'keydown', 'keyup', 'keypress', 'input', 'change', 'submit', 'reset', 'focus', 'blur', 'load', 'error', 'scroll', 'resize',
  'DOMContentLoaded', 'readystatechange', 'pointerdown', 'pointerup', 'touchstart', 'touchend', 'wheel', 'contextmenu',
  'popstate', 'hashchange', 'beforeunload', 'unload', 'pageshow', 'animationend', 'transitionend', 'message', 'toggle'];
function defineEventProps(proto) {
  for (const t of EVENT_PROPS) {
    Object.defineProperty(proto, 'on' + t.toLowerCase(), {
      get() { return this['_on' + t] || null; },
      set(v) { Object.defineProperty(this, '_on' + t, { value: v, writable: true, configurable: true }); },
      configurable: true,
    });
  }
}

// ---- nodes --------------------------------------------------------------------
class Node extends EventTarget {
  get nodeType() { return N('type', this._id); }
  get nodeName() { const t = this.nodeType; return t === TEXT ? '#text' : t === DOC ? '#document' : N('tag', this._id).toUpperCase(); }
  get parentNode() { return wrap(N('parent', this._id)); }
  get parentElement() { const p = this.parentNode; return p && p.nodeType === ELEMENT ? p : null; }
  get childNodes() { return list(N('kids', this._id)); }
  get firstChild() { return this.childNodes[0] || null; }
  get lastChild() { const c = this.childNodes; return c[c.length - 1] || null; }
  get nextSibling() { return wrap(N('sibling', this._id, 1)); }
  get previousSibling() { return wrap(N('sibling', this._id, -1)); }
  get ownerDocument() { return document; }
  get isConnected() { return N('connected', this._id); }
  get textContent() { return N('text', this._id); }
  set textContent(v) { N('settext', this._id, v === null || v === undefined ? '' : String(v)); }
  get nodeValue() { return this.nodeType === TEXT ? N('text', this._id) : null; }
  set nodeValue(v) { if (this.nodeType === TEXT) N('settext', this._id, String(v)); }
  hasChildNodes() { return this.childNodes.length > 0; }
  getRootNode() { return this.isConnected ? document : this; }
  appendChild(c) { insert(this, c, null); return c; }
  insertBefore(c, ref) { insert(this, c, ref); return c; }
  removeChild(c) { N('remove', c._id); return c; }
  replaceChild(n, old) { insert(this, n, old); N('remove', old._id); return old; }
  contains(o) { for (let n = o; n; n = n.parentNode) if (n === this) return true; return false; }
  cloneNode(deep) { return wrap(N('clone', this._id, deep ? 1 : 0)); }
  compareDocumentPosition(o) { return this.contains(o) ? 20 : o.contains(this) ? 10 : 4; }
  isSameNode(o) { return o === this; }
  isEqualNode(o) { return o && o.outerHTML === this.outerHTML; }
  normalize() {}
  remove() { N('remove', this._id); }
  before(...ns) { const p = this.parentNode; if (p) for (const n of ns) insert(p, n, this); }
  after(...ns) { const p = this.parentNode; if (!p) return; const next = this.nextSibling; for (const n of ns) insert(p, n, next); }
  replaceWith(...ns) { const p = this.parentNode; if (!p) return; for (const n of ns) insert(p, n, this); this.remove(); }
}
for (const [k, v] of Object.entries({ ELEMENT_NODE: 1, ATTRIBUTE_NODE: 2, TEXT_NODE: 3, COMMENT_NODE: 8, DOCUMENT_NODE: 9, DOCUMENT_FRAGMENT_NODE: 11,
  DOCUMENT_POSITION_DISCONNECTED: 1, DOCUMENT_POSITION_PRECEDING: 2, DOCUMENT_POSITION_FOLLOWING: 4, DOCUMENT_POSITION_CONTAINS: 8, DOCUMENT_POSITION_CONTAINED_BY: 16 })) {
  Node[k] = v; Node.prototype[k] = v;
}
function insert(parent, child, ref) {
  const pid = idOf(parent);
  const refId = ref ? ref._id : -1;
  if (child instanceof DocumentFragment) {
    for (const c of child.childNodes) N('insert', pid, c._id, refId);
  } else {
    N('insert', pid, idOf(child), refId);
  }
}
class CharacterData extends Node {
  get data() { return N('text', this._id); }
  set data(v) { N('settext', this._id, String(v)); }
  get length() { return this.data.length; }
  appendData(s) { this.data += s; }
  get nextElementSibling() { let n = this.nextSibling; while (n && n.nodeType !== ELEMENT) n = n.nextSibling; return n; }
  get previousElementSibling() { let n = this.previousSibling; while (n && n.nodeType !== ELEMENT) n = n.previousSibling; return n; }
}
class Text extends CharacterData {
  get wholeText() { return this.data; }
  splitText(o) { const t = document.createTextNode(this.data.slice(o)); this.data = this.data.slice(0, o); this.after(t); return t; }
}
class Comment extends CharacterData {}

function camelToDash(p) { return p.startsWith('--') ? p : p.replace(/[A-Z]/g, m => '-' + m.toLowerCase()).replace(/^(webkit|moz|ms)-/, '-$1-'); }
function parseDecls(text) {
  const out = new Map();
  for (const part of (text || '').split(';')) {
    const i = part.indexOf(':');
    if (i > 0) out.set(part.slice(0, i).trim().toLowerCase(), part.slice(i + 1).trim());
  }
  return out;
}
function styleProxy(el) {
  const get = () => parseDecls(N('attr', el._id, 'style'));
  const put = m => {
    const s = [...m].filter(([, v]) => v !== '').map(([k, v]) => k + ': ' + v).join('; ');
    if (s) N('setattr', el._id, 'style', s); else N('rmattr', el._id, 'style');
  };
  const api = {
    getPropertyValue: p => get().get(p.toLowerCase()) || '',
    setProperty: (p, v, prio) => { const m = get(); m.set(p.toLowerCase(), v === null || v === undefined ? '' : String(v) + (prio ? ' !important' : '')); put(m); },
    removeProperty: p => { const m = get(); const v = m.get(p) || ''; m.delete(p.toLowerCase()); put(m); return v; },
    item: i => [...get().keys()][i] || '',
  };
  return new Proxy(api, {
    get(t, p) {
      if (typeof p !== 'string') return undefined;
      if (p in api) return api[p];
      if (p === 'cssText') return N('attr', el._id, 'style') || '';
      if (p === 'length') return get().size;
      if (p === 'cssFloat') p = 'float';
      return get().get(camelToDash(p)) || '';
    },
    set(t, p, v) {
      if (typeof p !== 'string') return true;
      if (p === 'cssText') { N('setattr', el._id, 'style', String(v)); return true; }
      if (p === 'cssFloat') p = 'float';
      const m = get();
      const val = v === null || v === undefined ? '' : String(typeof v === 'number' && v !== 0 && /width|height|top|left|right|bottom|margin|padding|size/i.test(p) ? v + 'px' : v);
      m.set(camelToDash(p), val);
      put(m);
      return true;
    },
    has(t, p) { return true; },
  });
}
class DOMTokenList {
  constructor(el, attr) { this._el = el; this._attr = attr; }
  _get() { return (N('attr', this._el._id, this._attr) || '').split(/\s+/).filter(Boolean); }
  _set(a) { N('setattr', this._el._id, this._attr, a.join(' ')); }
  get length() { return this._get().length; }
  get value() { return this._get().join(' '); }
  set value(v) { N('setattr', this._el._id, this._attr, String(v)); }
  item(i) { return this._get()[i] || null; }
  contains(c) { return this._get().includes(c); }
  add(...cs) { const a = this._get(); for (const c of cs) if (!a.includes(c)) a.push(c); this._set(a); }
  remove(...cs) { this._set(this._get().filter(c => !cs.includes(c))); }
  toggle(c, force) {
    const has = this.contains(c);
    const want = force === undefined ? !has : !!force;
    if (want && !has) this.add(c); else if (!want && has) this.remove(c);
    return want;
  }
  replace(a, b) { const l = this._get(); const i = l.indexOf(a); if (i < 0) return false; l[i] = b; this._set(l); return true; }
  forEach(f, t) { this._get().forEach(f, t); }
  entries() { return this._get().entries(); }
  keys() { return this._get().keys(); }
  values() { return this._get().values(); }
  [Symbol.iterator]() { return this._get()[Symbol.iterator](); }
  toString() { return this.value; }
  supports() { return true; }
}
function rect(el) {
  const r = N('rect', el._id);
  const [x, y, w, h] = r ? r : [0, 0, 0, 0];
  return { x, y, width: w, height: h, left: x, top: y, right: x + w, bottom: y + h, toJSON() { return this; } };
}

class Element extends Node {
  get tagName() { return N('tag', this._id).toUpperCase(); }
  get localName() { return N('tag', this._id); }
  get namespaceURI() { return 'http://www.w3.org/1999/xhtml'; }
  get id() { return N('attr', this._id, 'id') || ''; }
  set id(v) { N('setattr', this._id, 'id', String(v)); }
  get className() { return N('attr', this._id, 'class') || ''; }
  set className(v) { N('setattr', this._id, 'class', String(v)); }
  get classList() { return new DOMTokenList(this, 'class'); }
  get relList() { return new DOMTokenList(this, 'rel'); }
  get attributes() {
    const a = N('attrs', this._id).map(([name, value]) => ({ name, value, nodeName: name, nodeValue: value, localName: name }));
    a.getNamedItem = n => a.find(x => x.name === n) || null;
    a.item = i => a[i] || null;
    return a;
  }
  getAttribute(n) { return N('attr', this._id, String(n).toLowerCase()); }
  getAttributeNS(ns, n) { return this.getAttribute(n); }
  setAttribute(n, v) { N('setattr', this._id, String(n).toLowerCase(), String(v)); }
  setAttributeNS(ns, n, v) { this.setAttribute(n, v); }
  removeAttribute(n) { N('rmattr', this._id, String(n).toLowerCase()); }
  removeAttributeNS(ns, n) { this.removeAttribute(n); }
  hasAttribute(n) { return N('attr', this._id, String(n).toLowerCase()) !== null; }
  hasAttributes() { return this.attributes.length > 0; }
  getAttributeNames() { return N('attrs', this._id).map(a => a[0]); }
  toggleAttribute(n, force) {
    const has = this.hasAttribute(n);
    const want = force === undefined ? !has : !!force;
    if (want && !has) this.setAttribute(n, ''); else if (!want && has) this.removeAttribute(n);
    return want;
  }
  get children() { const c = this.childNodes.filter(n => n.nodeType === ELEMENT); c.item = i => c[i] || null; c.namedItem = n => c.find(e => e.id === n) || null; return c; }
  get childElementCount() { return this.children.length; }
  get firstElementChild() { return this.children[0] || null; }
  get lastElementChild() { const c = this.children; return c[c.length - 1] || null; }
  get nextElementSibling() { let n = this.nextSibling; while (n && n.nodeType !== ELEMENT) n = n.nextSibling; return n; }
  get previousElementSibling() { let n = this.previousSibling; while (n && n.nodeType !== ELEMENT) n = n.previousSibling; return n; }
  get innerHTML() { return N('inner', this._id); }
  set innerHTML(v) { N('setinner', this._id, v === null || v === undefined ? '' : String(v)); }
  get outerHTML() { return N('outer', this._id); }
  set outerHTML(v) { const p = this.parentNode; if (!p) return; const t = document.createElement('div'); t.innerHTML = v; for (const c of t.childNodes) p.insertBefore(c, this); this.remove(); }
  get innerText() { return N('text', this._id); }
  set innerText(v) { this.textContent = v; }
  get outerText() { return this.innerText; }
  querySelector(s) { return wrap(N('qs', this._id, String(s))); }
  querySelectorAll(s) { const l = list(N('qsa', this._id, String(s))); l.item = i => l[i] || null; return l; }
  getElementsByTagName(t) { return this.querySelectorAll(t === '*' ? '*' : t); }
  getElementsByClassName(c) { return this.querySelectorAll(String(c).trim().split(/\s+/).map(x => '.' + CSS.escape(x)).join('')); }
  getElementsByName(n) { return this.querySelectorAll('[name="' + n + '"]'); }
  matches(s) { return N('matches', this._id, String(s)); }
  get webkitMatchesSelector() { return this.matches; }
  closest(s) { for (let n = this; n && n.nodeType === ELEMENT; n = n.parentNode) if (n.matches(s)) return n; return null; }
  append(...ns) { for (const n of ns) insert(this, n, null); }
  prepend(...ns) { const f = this.firstChild; for (const n of ns) insert(this, n, f); }
  replaceChildren(...ns) { N('setinner', this._id, ''); this.append(...ns); }
  insertAdjacentHTML(pos, html) {
    const t = document.createElement('div');
    t.innerHTML = html;
    this.insertAdjacentElement(pos, t, true);
  }
  insertAdjacentElement(pos, el, unwrap) {
    const nodes = unwrap ? el.childNodes : [el];
    switch (String(pos).toLowerCase()) {
      case 'beforebegin': this.before(...nodes); break;
      case 'afterbegin': this.prepend(...nodes); break;
      case 'beforeend': this.append(...nodes); break;
      case 'afterend': this.after(...nodes); break;
    }
    return el;
  }
  insertAdjacentText(pos, text) { this.insertAdjacentElement(pos, document.createTextNode(text)); }
  getBoundingClientRect() { return rect(this); }
  getClientRects() { const r = rect(this); return r.width || r.height ? [r] : []; }
  get clientWidth() { return rect(this).width; }
  get clientHeight() { return rect(this).height; }
  get clientTop() { return 0; }
  get clientLeft() { return 0; }
  get scrollWidth() { return rect(this).width; }
  get scrollHeight() { return rect(this).height; }
  get scrollTop() { return 0; }
  set scrollTop(v) {}
  get scrollLeft() { return 0; }
  set scrollLeft(v) {}
  scrollTo() {}
  scrollBy() {}
  scrollIntoView() { N('scrollto', this._id); }
  attachShadow() { const r = document.createElement('div'); this.appendChild(r); Object.defineProperty(this, 'shadowRoot', { value: r }); return r; }
  animate() { return { finished: Promise.resolve(), cancel() {}, play() {}, pause() {}, onfinish: null, addEventListener() {} }; }
  getAnimations() { return []; }
  requestFullscreen() { return Promise.reject(new Error('not supported')); }
  setPointerCapture() {}
  releasePointerCapture() {}
  hasPointerCapture() { return false; }
  get slot() { return ''; }
  get assignedSlot() { return null; }
}
defineEventProps(Element.prototype);

function boolAttr(proto, name, attr) {
  Object.defineProperty(proto, name, {
    get() { return this.hasAttribute(attr || name); },
    set(v) { this.toggleAttribute(attr || name, !!v); },
    configurable: true,
  });
}
function strAttr(proto, name, attr, dflt) {
  Object.defineProperty(proto, name, {
    get() { const v = this.getAttribute(attr || name); return v === null ? (dflt || '') : v; },
    set(v) { this.setAttribute(attr || name, v); },
    configurable: true,
  });
}
function urlAttr(proto, name) {
  Object.defineProperty(proto, name, {
    get() { const v = this.getAttribute(name); return v === null ? '' : N('resolve', v); },
    set(v) { this.setAttribute(name, v); },
    configurable: true,
  });
}

class HTMLElement extends Element {
  get style() { return styleProxy(this); }
  set style(v) { this.setAttribute('style', v); }
  get dataset() {
    const el = this;
    const key = p => 'data-' + camelToDash(p);
    return new Proxy({}, {
      get(t, p) { if (typeof p !== 'string') return undefined; const v = el.getAttribute(key(p)); return v === null ? undefined : v; },
      set(t, p, v) { el.setAttribute(key(p), v); return true; },
      has(t, p) { return el.hasAttribute(key(p)); },
      deleteProperty(t, p) { el.removeAttribute(key(p)); return true; },
      ownKeys() { return el.getAttributeNames().filter(n => n.startsWith('data-')).map(n => n.slice(5).replace(/-([a-z])/g, (m, c) => c.toUpperCase())); },
      getOwnPropertyDescriptor(t, p) { const v = el.getAttribute(key(p)); return v === null ? undefined : { value: v, enumerable: true, configurable: true }; },
    });
  }
  get offsetWidth() { return rect(this).width; }
  get offsetHeight() { return rect(this).height; }
  get offsetTop() { return rect(this).top; }
  get offsetLeft() { return rect(this).left; }
  get offsetParent() { return document.body; }
  click() {
    const ev = new MouseEvent('click', { bubbles: true, cancelable: true });
    if (this.dispatchEvent(ev)) N('activate', this._id);
  }
  focus() { document._active = this; }
  blur() { if (document._active === this) document._active = null; }
  get form() { return this.closest('form'); }
  get value() {
    const tag = this.localName;
    if (tag === 'select') { const o = this.selectedOptions[0]; return o ? o.value : ''; }
    if (tag === 'textarea') { const v = this.getAttribute('value'); return v === null ? this.textContent : v; }
    if (tag === 'option') { const v = this.getAttribute('value'); return v === null ? this.textContent.trim() : v; }
    const v = this.getAttribute('value');
    if (v === null && (this.type === 'checkbox' || this.type === 'radio')) return 'on';
    return v === null ? '' : v;
  }
  set value(v) {
    if (this.localName === 'select') { for (const o of this.options) o.selected = o.value === String(v); return; }
    this.setAttribute('value', v === null || v === undefined ? '' : v);
  }
  get checked() { return this.hasAttribute('checked'); }
  set checked(v) {
    if (v && this.type === 'radio' && this.name) {
      for (const r of document.querySelectorAll('input[type=radio][name="' + this.name + '"]')) r.removeAttribute('checked');
    }
    this.toggleAttribute('checked', !!v);
  }
  get selected() { return this.hasAttribute('selected'); }
  set selected(v) {
    if (v) { const s = this.closest('select'); if (s) for (const o of s.options) o.removeAttribute('selected'); }
    this.toggleAttribute('selected', !!v);
  }
  get type() { const t = this.getAttribute('type'); const tag = this.localName; return (t || (tag === 'button' ? 'submit' : tag === 'select' ? 'select-one' : 'text')).toLowerCase(); }
  set type(v) { this.setAttribute('type', v); }
  get options() { return this.querySelectorAll('option'); }
  get selectedOptions() { const s = this.querySelectorAll('option[selected]'); return s.length ? s : this.options.slice(0, 1); }
  get selectedIndex() { const o = this.options; const s = this.selectedOptions[0]; return s ? o.indexOf(s) : -1; }
  set selectedIndex(i) { const o = this.options; o.forEach((x, k) => x.selected = k === i); }
  get elements() { return this.querySelectorAll('input,select,textarea,button'); }
  get length() { return this.localName === 'form' ? this.elements.length : this.localName === 'select' ? this.options.length : undefined; }
  get labels() { return []; }
  get validity() { return { valid: true }; }
  checkValidity() { return true; }
  reportValidity() { return true; }
  setCustomValidity() {}
  select() {}
  setSelectionRange() {}
  get content() { return this; }
  get text() { return this.textContent; }
  set text(v) { this.textContent = v; }
  get isContentEditable() { return false; }
  get naturalWidth() { return rect(this).width; }
  get naturalHeight() { return rect(this).height; }
  get complete() { return true; }
  get sheet() { return null; }
  get pathname() { try { return new URL(this.href).pathname; } catch (e) { return ''; } }
  get hostname() { try { return new URL(this.href).hostname; } catch (e) { return ''; } }
  get host() { try { return new URL(this.href).host; } catch (e) { return ''; } }
  get protocol() { try { return new URL(this.href).protocol; } catch (e) { return ''; } }
  get search() { try { return new URL(this.href).search; } catch (e) { return ''; } }
  get hash() { try { return new URL(this.href).hash; } catch (e) { return ''; } }
  get origin() { try { return new URL(this.href).origin; } catch (e) { return ''; } }
  submit() { N('submit', this._id); }
  requestSubmit() { if (this.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true }))) this.submit(); }
  reset() {}
  showModal() { this.setAttribute('open', ''); }
  show() { this.setAttribute('open', ''); }
  close() { this.removeAttribute('open'); }
  play() { return Promise.resolve(); }
  pause() {}
  load() {}
  getContext() { return null; }
  toDataURL() { return 'data:,'; }
}
const H = HTMLElement.prototype;
for (const n of ['hidden', 'disabled', 'required', 'readOnly', 'multiple', 'autofocus', 'async', 'defer', 'noModule', 'open', 'draggable']) boolAttr(H, n, n.toLowerCase());
for (const n of ['title', 'lang', 'dir', 'name', 'placeholder', 'alt', 'rel', 'target', 'method', 'enctype', 'accept', 'autocomplete', 'role', 'htmlFor', 'charset', 'media', 'crossOrigin', 'integrity', 'loading', 'decoding', 'min', 'max', 'step', 'pattern', 'label', 'download', 'referrerPolicy', 'srcset', 'sizes', 'width', 'height', 'tabIndex', 'colSpan', 'rowSpan', 'maxLength', 'size', 'rows', 'cols', 'defaultValue', 'contentEditable']) {
  strAttr(H, n, n === 'htmlFor' ? 'for' : n === 'defaultValue' ? 'value' : n.toLowerCase());
}
for (const n of ['href', 'src', 'action', 'poster', 'cite', 'data']) urlAttr(H, n);
class HTMLInputElement extends HTMLElement {}
class HTMLFormElement extends HTMLElement {}
class HTMLSelectElement extends HTMLElement {}
class HTMLAnchorElement extends HTMLElement { toString() { return this.href; } }
class HTMLImageElement extends HTMLElement {}
class HTMLScriptElement extends HTMLElement {}
class HTMLTemplateElement extends HTMLElement {}
class DocumentFragment extends HTMLElement {
  get nodeType() { return FRAGMENT; }
  get nodeName() { return '#document-fragment'; }
}
const htmlClasses = ['HTMLDivElement', 'HTMLSpanElement', 'HTMLButtonElement', 'HTMLTextAreaElement', 'HTMLLinkElement', 'HTMLStyleElement',
  'HTMLMetaElement', 'HTMLHeadElement', 'HTMLBodyElement', 'HTMLHtmlElement', 'HTMLParagraphElement', 'HTMLHeadingElement', 'HTMLUListElement',
  'HTMLOListElement', 'HTMLLIElement', 'HTMLTableElement', 'HTMLTableRowElement', 'HTMLTableCellElement', 'HTMLIFrameElement',
  'HTMLCanvasElement', 'HTMLVideoElement', 'HTMLAudioElement', 'HTMLMediaElement', 'HTMLLabelElement', 'HTMLOptionElement',
  'HTMLDialogElement', 'HTMLPreElement', 'HTMLBRElement', 'HTMLHRElement', 'HTMLUnknownElement', 'HTMLSlotElement', 'HTMLPictureElement',
  'HTMLSourceElement', 'HTMLDetailsElement', 'HTMLFieldSetElement', 'HTMLLegendElement', 'HTMLTitleElement', 'HTMLBaseElement',
  'SVGElement', 'SVGSVGElement', 'SVGGraphicsElement'];
for (const c of htmlClasses) G[c] = class extends HTMLElement {};
Object.defineProperty(HTMLElement, Symbol.hasInstance, { value: o => o instanceof Element });
for (const c of htmlClasses) Object.defineProperty(G[c], Symbol.hasInstance, { value: o => o instanceof Element });
Object.defineProperty(DocumentFragment, Symbol.hasInstance, { value: o => !!o && o._id !== undefined && o.nodeType === FRAGMENT });
for (const c of [HTMLInputElement, HTMLFormElement, HTMLSelectElement, HTMLAnchorElement, HTMLImageElement, HTMLScriptElement, HTMLTemplateElement]) Object.defineProperty(c, Symbol.hasInstance, { value: o => !!o && Object.prototype.isPrototypeOf.call(c.prototype, o) });

// ---- document -------------------------------------------------------------------
class Document extends Node {}
const document = Object.create(Document.prototype);
Object.defineProperty(document, '_id', { value: 0 });
wrappers.set(0, document);
defineEventProps(Document.prototype);
Object.assign(Document.prototype, {
  getElementById(id) { return wrap(N('byid', String(id))); },
  querySelector(s) { return wrap(N('qs', 0, String(s))); },
  querySelectorAll(s) { const l = list(N('qsa', 0, String(s))); l.item = i => l[i] || null; return l; },
  getElementsByTagName(t) { return this.querySelectorAll(t); },
  getElementsByClassName(c) { return this.querySelectorAll(String(c).trim().split(/\s+/).map(x => '.' + CSS.escape(x)).join('')); },
  getElementsByName(n) { return this.querySelectorAll('[name="' + n + '"]'); },
  createElement(t) { return wrap(N('create', String(t).toLowerCase())); },
  createElementNS(ns, t) { return this.createElement(String(t).replace(/^.*:/, '')); },
  createTextNode(s) { return wrap(N('ctext', String(s))); },
  createComment(s) { return wrap(N('ctext', '')); },
  createDocumentFragment() { return wrap(N('create', '#fragment')); },
  createEvent(t) { return t === 'CustomEvent' ? new CustomEvent('') : new Event(''); },
  createRange() {
    return { setStart() {}, setEnd() {}, selectNodeContents() {}, collapse() {}, getBoundingClientRect: () => rect(document.body),
      createContextualFragment(html) { const f = document.createDocumentFragment(); f.innerHTML = html; return f; } };
  },
  createTreeWalker(root) {
    const all = [root, ...root.querySelectorAll('*')];
    let i = 0;
    return { currentNode: root, nextNode() { i++; this.currentNode = all[i] || null; return this.currentNode; } };
  },
  importNode(n, deep) { return n.cloneNode(deep); },
  adoptNode(n) { return n; },
  write(...s) { N('write', s.join('')); },
  writeln(...s) { N('write', s.join('') + '\n'); },
  open() {},
  close() {},
  hasFocus() { return true; },
  execCommand() { return false; },
  getSelection() { return G.getSelection(); },
  elementFromPoint(x, y) { return wrap(N('hit', x, y)); },
  elementsFromPoint(x, y) { const e = this.elementFromPoint(x, y); return e ? [e] : []; },
  exitFullscreen() { return Promise.resolve(); },
});
Object.defineProperties(Document.prototype, {
  nodeType: { get: () => DOC },
  nodeName: { get: () => '#document' },
  documentElement: { get: () => wrap(N('root', 'html')) },
  head: { get: () => wrap(N('root', 'head')) },
  body: { get: () => wrap(N('root', 'body')) },
  title: { get: () => N('title'), set: v => N('settitle', String(v)) },
  readyState: { get: () => G.__readyState },
  cookie: { get: () => N('cookie'), set: v => N('setcookie', String(v)) },
  location: { get: () => G.location, set: v => { G.location.href = v; } },
  URL: { get: () => G.location.href },
  documentURI: { get: () => G.location.href },
  baseURI: { get: () => N('resolve', '') },
  domain: { get: () => G.location.hostname },
  referrer: { get: () => '' },
  defaultView: { get: () => G },
  currentScript: { get: () => wrap(G.__currentScript) },
  activeElement: { get: () => document._active || document.body },
  visibilityState: { get: () => 'visible' },
  hidden: { get: () => false },
  characterSet: { get: () => 'UTF-8' },
  charset: { get: () => 'UTF-8' },
  contentType: { get: () => 'text/html' },
  compatMode: { get: () => 'CSS1Compat' },
  doctype: { get: () => null },
  scripts: { get: () => document.querySelectorAll('script') },
  forms: { get: () => document.querySelectorAll('form') },
  images: { get: () => document.querySelectorAll('img') },
  links: { get: () => document.querySelectorAll('a[href]') },
  styleSheets: { get: () => [] },
  fonts: { get: () => ({ ready: Promise.resolve(), add() {}, load: () => Promise.resolve([]), check: () => true, addEventListener() {} }) },
  implementation: { get: () => ({ createHTMLDocument: t => detachedDocument('', t), createDocumentType: () => null, hasFeature: () => true }) },
  fullscreenElement: { get: () => null },
  scrollingElement: { get: () => document.documentElement },
  childNodes: { get: () => list(N('kids', 0)) },
  children: { get: () => list(N('kids', 0)).filter(n => n.nodeType === ELEMENT) },
  firstElementChild: { get: () => document.documentElement },
  textContent: { get: () => null, set: () => {} },
});

// ---- window ------------------------------------------------------------------------
function makeLocation() {
  const loc = {};
  const u = () => new URL(N('location'));
  for (const k of ['protocol', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'origin']) {
    Object.defineProperty(loc, k, {
      get: () => u()[k],
      set: v => { const x = u(); x[k] = v; if (k === 'hash') N('sethash', x.hash); else N('navigate', x.href); },
    });
  }
  Object.defineProperty(loc, 'href', { get: () => N('location'), set: v => N('navigate', N('resolve', String(v))) });
  loc.assign = v => N('navigate', N('resolve', String(v)));
  loc.replace = loc.assign;
  loc.reload = () => N('navigate', N('location'));
  loc.toString = () => N('location');
  return loc;
}
const location = makeLocation();

// timers
let timerSeq = 1;
const timers = new Map();
function addTimer(fn, ms, args, repeat) {
  const id = timerSeq++;
  ms = Math.max(0, Number(ms) || 0);
  timers.set(id, { fn, at: N('now') + ms, ms: Math.max(ms, 4), args, repeat });
  return id;
}
G.setTimeout = (fn, ms, ...args) => addTimer(fn, ms, args, false);
G.setInterval = (fn, ms, ...args) => addTimer(fn, ms, args, true);
G.clearTimeout = G.clearInterval = id => { timers.delete(id); };
G.requestAnimationFrame = fn => addTimer(() => fn(performance.now()), 16, [], false);
G.cancelAnimationFrame = G.clearTimeout;
G.requestIdleCallback = fn => addTimer(() => fn({ didTimeout: false, timeRemaining: () => 10 }), 1, [], false);
G.cancelIdleCallback = G.clearTimeout;
G.setImmediate = fn => addTimer(fn, 0, [], false);
G.queueMicrotask = fn => { Promise.resolve().then(fn); };
// run due timers; returns ms until the next one, or -1
G.__everos_timers = function () {
  const now = N('now');
  let ran = 0;
  for (const [id, t] of [...timers]) {
    if (t.at > now || !timers.has(id)) continue;
    if (t.repeat) t.at = now + t.ms; else timers.delete(id);
    try {
      if (typeof t.fn === 'function') t.fn(...t.args); else (0, eval)(String(t.fn));
    } catch (e) { reportError(e); }
    if (++ran > 200) break;
  }
  let next = -1;
  for (const t of timers.values()) {
    const d = Math.max(0, t.at - N('now'));
    if (next < 0 || d < next) next = d;
  }
  return next;
};

const perf0 = N('now');
G.performance = {
  now: () => N('now') - perf0,
  timeOrigin: perf0,
  mark() {}, measure() {}, getEntriesByName: () => [], getEntriesByType: () => [], getEntries: () => [], clearMarks() {}, clearMeasures() {},
  timing: { navigationStart: perf0, loadEventEnd: perf0 },
};

// storage
function makeStorage(kind) {
  const s = {
    getItem: k => N('storage', kind, 'get', String(k)),
    setItem: (k, v) => N('storage', kind, 'set', String(k), String(v)),
    removeItem: k => N('storage', kind, 'remove', String(k)),
    clear: () => N('storage', kind, 'clear'),
    key: i => (N('storage', kind, 'keys') || [])[i] ?? null,
  };
  return new Proxy(s, {
    get(t, p) {
      if (p in s) return s[p];
      if (p === 'length') return (N('storage', kind, 'keys') || []).length;
      if (typeof p !== 'string') return undefined;
      const v = s.getItem(p);
      return v === null ? undefined : v;
    },
    set(t, p, v) { s.setItem(p, v); return true; },
    deleteProperty(t, p) { s.removeItem(p); return true; },
    ownKeys() { return N('storage', kind, 'keys') || []; },
    getOwnPropertyDescriptor(t, p) { const v = s.getItem(p); return v === null ? undefined : { value: v, enumerable: true, configurable: true }; },
  });
}

// URL and URLSearchParams
class URLSearchParams {
  constructor(init) {
    this._p = [];
    if (typeof init === 'string') {
      for (const part of init.replace(/^\?/, '').split('&')) {
        if (!part) continue;
        const i = part.indexOf('=');
        const dec = s => { try { return decodeURIComponent(s.replace(/\+/g, ' ')); } catch (e) { return s; } };
        this._p.push(i < 0 ? [dec(part), ''] : [dec(part.slice(0, i)), dec(part.slice(i + 1))]);
      }
    } else if (init && typeof init[Symbol.iterator] === 'function') {
      for (const [k, v] of init) this._p.push([String(k), String(v)]);
    } else if (init && typeof init === 'object') {
      for (const k of Object.keys(init)) this._p.push([k, String(init[k])]);
    }
  }
  get(k) { const e = this._p.find(p => p[0] === k); return e ? e[1] : null; }
  getAll(k) { return this._p.filter(p => p[0] === k).map(p => p[1]); }
  has(k) { return this._p.some(p => p[0] === k); }
  set(k, v) { const i = this._p.findIndex(p => p[0] === k); if (i < 0) this._p.push([k, String(v)]); else { this._p[i][1] = String(v); this._p = this._p.filter((p, j) => p[0] !== k || j === i); } this._u && this._u(); }
  append(k, v) { this._p.push([k, String(v)]); this._u && this._u(); }
  delete(k) { this._p = this._p.filter(p => p[0] !== k); this._u && this._u(); }
  sort() { this._p.sort((a, b) => a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0); }
  forEach(f, t) { for (const [k, v] of this._p) f.call(t, v, k, this); }
  keys() { return this._p.map(p => p[0])[Symbol.iterator](); }
  values() { return this._p.map(p => p[1])[Symbol.iterator](); }
  entries() { return this._p.map(p => [p[0], p[1]])[Symbol.iterator](); }
  [Symbol.iterator]() { return this.entries(); }
  get size() { return this._p.length; }
  toString() { return this._p.map(([k, v]) => encodeURIComponent(k) + '=' + encodeURIComponent(v).replace(/%20/g, '+')).join('&'); }
}
class URL {
  constructor(u, base) {
    const s = String(u);
    const abs = N('urljoin', base === undefined ? null : String(base), s);
    if (abs === null) throw new TypeError('Invalid URL: ' + s);
    const m = /^([a-z][a-z0-9+.-]*:)(?:\/\/([^/?#]*))?([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(abs);
    if (!m) throw new TypeError('Invalid URL: ' + s);
    this.protocol = m[1].toLowerCase();
    const auth = m[2] || '';
    const hp = auth.replace(/^.*@/, '');
    const pm = /^(\[[^\]]*\]|[^:]*)(?::(\d*))?$/.exec(hp);
    this.hostname = (pm ? pm[1] : hp).toLowerCase();
    this.port = pm && pm[2] ? pm[2] : '';
    if ((this.protocol === 'http:' && this.port === '80') || (this.protocol === 'https:' && this.port === '443')) this.port = '';
    this.pathname = m[3] || (m[2] !== undefined ? '/' : '');
    this._search = m[4] && m[4] !== '?' ? m[4] : '';
    this.hash = m[5] && m[5] !== '#' ? m[5] : '';
    this.username = ''; this.password = '';
  }
  get host() { return this.hostname + (this.port ? ':' + this.port : ''); }
  set host(v) { const [h, p] = String(v).split(':'); this.hostname = h; this.port = p || ''; }
  get origin() { return this.protocol + '//' + this.host; }
  get search() { return this._search; }
  set search(v) { v = String(v); this._search = v && v !== '?' ? (v[0] === '?' ? v : '?' + v) : ''; this._sp = null; }
  get searchParams() {
    if (!this._sp) { this._sp = new URLSearchParams(this._search); this._sp._u = () => { const q = this._sp.toString(); this._search = q ? '?' + q : ''; }; }
    return this._sp;
  }
  get href() { return this.protocol + (this.hostname || this.protocol.startsWith('http') ? '//' + this.host : '') + this.pathname + this._search + this.hash; }
  set href(v) { Object.assign(this, new URL(v)); }
  toString() { return this.href; }
  toJSON() { return this.href; }
  static createObjectURL() { return 'blob:everos'; }
  static revokeObjectURL() {}
  static canParse(u, b) { try { new URL(u, b); return true; } catch (e) { return false; } }
}

// fetch and XMLHttpRequest
class Headers {
  constructor(init) {
    this._h = new Map();
    if (init instanceof Headers) init.forEach((v, k) => this.set(k, v));
    else if (Array.isArray(init)) for (const [k, v] of init) this.append(k, v);
    else if (init) for (const k of Object.keys(init)) this.set(k, init[k]);
  }
  get(k) { const v = this._h.get(String(k).toLowerCase()); return v === undefined ? null : v; }
  set(k, v) { this._h.set(String(k).toLowerCase(), String(v)); }
  append(k, v) { const o = this.get(k); this.set(k, o === null ? v : o + ', ' + v); }
  has(k) { return this._h.has(String(k).toLowerCase()); }
  delete(k) { this._h.delete(String(k).toLowerCase()); }
  forEach(f, t) { for (const [k, v] of this._h) f.call(t, v, k, this); }
  entries() { return this._h.entries(); }
  keys() { return this._h.keys(); }
  values() { return this._h.values(); }
  [Symbol.iterator]() { return this._h.entries(); }
}
class Response {
  constructor(body, init) {
    init = init || {};
    this._body = body === undefined || body === null ? '' : String(body);
    this.status = init.status === undefined ? 200 : init.status;
    this.statusText = init.statusText || '';
    this.headers = new Headers(init.headers);
    this.url = init.url || '';
    this.redirected = false;
    this.type = 'basic';
    this.bodyUsed = false;
  }
  get ok() { return this.status >= 200 && this.status < 300; }
  text() { this.bodyUsed = true; return Promise.resolve(this._body); }
  json() { this.bodyUsed = true; return new Promise((res, rej) => { try { res(JSON.parse(this._body)); } catch (e) { rej(e); } }); }
  arrayBuffer() { return Promise.resolve(new TextEncoder().encode(this._body).buffer); }
  blob() { return Promise.resolve({ size: this._body.length, type: this.headers.get('content-type') || '', text: () => Promise.resolve(this._body) }); }
  clone() { return new Response(this._body, { status: this.status, headers: this.headers, url: this.url }); }
  static json(v, init) { return new Response(JSON.stringify(v), init); }
  static error() { return new Response('', { status: 0 }); }
}
class Request {
  constructor(input, init) {
    init = init || {};
    this.url = input instanceof Request ? input.url : N('resolve', String(input));
    this.method = (init.method || (input instanceof Request ? input.method : 'GET')).toUpperCase();
    this.headers = new Headers(init.headers || (input instanceof Request ? input.headers : undefined));
    this.body = init.body !== undefined ? init.body : (input instanceof Request ? input.body : null);
  }
}
function bodyText(b) {
  if (b === null || b === undefined) return null;
  if (typeof b === 'string') return b;
  if (b instanceof URLSearchParams) return b.toString();
  if (b instanceof FormData) return b._toQuery();
  if (b instanceof ArrayBuffer || ArrayBuffer.isView(b)) return new TextDecoder().decode(b);
  return String(b);
}
function http(method, url, body, type) {
  const r = N('http', method, url, body, type);
  return r;
}
G.fetch = function (input, init) {
  return new Promise((resolve, reject) => {
    const req = new Request(input, init);
    // answer after the current script, as a real network would
    setTimeout(() => {
      const body = bodyText(req.body);
      let type = req.headers.get('content-type');
      if (!type && req.body instanceof URLSearchParams) type = 'application/x-www-form-urlencoded';
      if (!type && req.body instanceof FormData) type = 'application/x-www-form-urlencoded';
      if (!type && body !== null) type = 'text/plain;charset=UTF-8';
      const r = http(req.method, req.url, body, type);
      if (!r || r.error) { reject(new TypeError('Failed to fetch: ' + (r && r.error || req.url))); return; }
      const res = new Response(r.body, { status: r.status, headers: { 'content-type': r.type }, url: r.url });
      res.redirected = r.url !== req.url;
      resolve(res);
    }, 0);
  });
};
class XMLHttpRequest extends EventTarget {
  constructor() { super(); this.readyState = 0; this.status = 0; this.responseText = ''; this.response = ''; this.responseType = ''; this._h = {}; this.withCredentials = false; this.timeout = 0; this.upload = new EventTarget(); }
  open(method, url, async) { this._m = String(method).toUpperCase(); this._u = N('resolve', String(url)); this._async = async !== false; this.readyState = 1; }
  setRequestHeader(k, v) { this._h[String(k).toLowerCase()] = String(v); }
  getResponseHeader(k) { return String(k).toLowerCase() === 'content-type' ? this._type || null : null; }
  getAllResponseHeaders() { return this._type ? 'content-type: ' + this._type + '\r\n' : ''; }
  overrideMimeType() {}
  abort() { this._aborted = true; }
  send(body) {
    const run = () => {
      if (this._aborted) return;
      const r = http(this._m, this._u, bodyText(body), this._h['content-type'] || (body instanceof FormData || body instanceof URLSearchParams ? 'application/x-www-form-urlencoded' : null));
      this.readyState = 4;
      if (!r || r.error) {
        this.status = 0;
        this.dispatchEvent(new ProgressEvent('error'));
        if (typeof this.onerror === 'function') this.onerror(new ProgressEvent('error'));
      } else {
        this.status = r.status;
        this.statusText = r.status === 200 ? 'OK' : '';
        this.responseURL = r.url;
        this._type = r.type;
        this.responseText = r.body;
        if (this.responseType === 'json') { try { this.response = JSON.parse(r.body); } catch (e) { this.response = null; } }
        else this.response = r.body;
      }
      for (const t of ['readystatechange', 'load', 'loadend']) {
        const ev = new ProgressEvent(t);
        this.dispatchEvent(ev);
        const h = this['on' + t];
        if (typeof h === 'function') { try { h.call(this, ev); } catch (e) { reportError(e); } }
      }
    };
    if (this._async) setTimeout(run, 0); else run();
  }
}
XMLHttpRequest.UNSENT = 0; XMLHttpRequest.OPENED = 1; XMLHttpRequest.HEADERS_RECEIVED = 2; XMLHttpRequest.LOADING = 3; XMLHttpRequest.DONE = 4;
class FormData {
  constructor(form) {
    this._p = [];
    if (form && form._id !== undefined) {
      for (const el of form.querySelectorAll('input,select,textarea')) {
        if (!el.name || el.disabled) continue;
        const t = el.type;
        if ((t === 'checkbox' || t === 'radio') && !el.checked) continue;
        if (t === 'submit' || t === 'button' || t === 'file') continue;
        this._p.push([el.name, el.value]);
      }
    }
  }
  append(k, v) { this._p.push([String(k), String(v)]); }
  set(k, v) { this.delete(k); this.append(k, v); }
  get(k) { const e = this._p.find(p => p[0] === k); return e ? e[1] : null; }
  getAll(k) { return this._p.filter(p => p[0] === k).map(p => p[1]); }
  has(k) { return this._p.some(p => p[0] === k); }
  delete(k) { this._p = this._p.filter(p => p[0] !== k); }
  entries() { return this._p[Symbol.iterator](); }
  keys() { return this._p.map(p => p[0])[Symbol.iterator](); }
  values() { return this._p.map(p => p[1])[Symbol.iterator](); }
  forEach(f, t) { for (const [k, v] of this._p) f.call(t, v, k, this); }
  [Symbol.iterator]() { return this.entries(); }
  _toQuery() { return new URLSearchParams(this._p).toString(); }
}

// text encoding
class TextEncoder {
  get encoding() { return 'utf-8'; }
  encode(s) {
    s = String(s === undefined ? '' : s);
    const out = [];
    for (const ch of s) {
      let c = ch.codePointAt(0);
      if (c < 0x80) out.push(c);
      else if (c < 0x800) out.push(0xc0 | c >> 6, 0x80 | c & 63);
      else if (c < 0x10000) out.push(0xe0 | c >> 12, 0x80 | c >> 6 & 63, 0x80 | c & 63);
      else out.push(0xf0 | c >> 18, 0x80 | c >> 12 & 63, 0x80 | c >> 6 & 63, 0x80 | c & 63);
    }
    return new Uint8Array(out);
  }
}
class TextDecoder {
  constructor(label) { this.encoding = label || 'utf-8'; }
  decode(b) {
    if (!b) return '';
    const u = b instanceof ArrayBuffer ? new Uint8Array(b) : new Uint8Array(b.buffer, b.byteOffset, b.byteLength);
    let s = '';
    for (let i = 0; i < u.length;) {
      const c = u[i];
      let cp, n;
      if (c < 0x80) { cp = c; n = 1; }
      else if (c >> 5 === 6) { cp = (c & 31) << 6 | u[i + 1] & 63; n = 2; }
      else if (c >> 4 === 14) { cp = (c & 15) << 12 | (u[i + 1] & 63) << 6 | u[i + 2] & 63; n = 3; }
      else { cp = (c & 7) << 18 | (u[i + 1] & 63) << 12 | (u[i + 2] & 63) << 6 | u[i + 3] & 63; n = 4; }
      s += String.fromCodePoint(cp || 0xfffd);
      i += n;
    }
    return s;
  }
}
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
G.btoa = s => {
  s = String(s);
  let out = '';
  for (let i = 0; i < s.length; i += 3) {
    const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2);
    if (a > 255 || b > 255 || c > 255) throw new Error('InvalidCharacterError');
    out += B64[a >> 2] + B64[(a & 3) << 4 | (b >> 4 || 0)] + (i + 1 < s.length ? B64[(b & 15) << 2 | (c >> 6 || 0)] : '=') + (i + 2 < s.length ? B64[c & 63] : '=');
  }
  return out;
};
G.atob = s => {
  s = String(s).replace(/[\s=]/g, '');
  let out = '', bits = 0, n = 0;
  for (const ch of s) {
    const v = B64.indexOf(ch);
    if (v < 0) continue;
    bits = bits << 6 | v; n += 6;
    if (n >= 8) { n -= 8; out += String.fromCharCode(bits >> n & 255); }
  }
  return out;
};

// observers do nothing, except that everything counts as visible
class MutationObserver { constructor(cb) { this._cb = cb; } observe() {} disconnect() {} takeRecords() { return []; } }
class ResizeObserver { constructor(cb) { this._cb = cb; } observe() {} unobserve() {} disconnect() {} }
class IntersectionObserver {
  constructor(cb, opts) { this._cb = cb; this.root = null; this.rootMargin = '0px'; this.thresholds = [0]; }
  observe(el) {
    setTimeout(() => {
      const r = rect(el);
      try { this._cb([{ target: el, isIntersecting: true, intersectionRatio: 1, boundingClientRect: r, intersectionRect: r, rootBounds: null, time: performance.now() }], this); } catch (e) { reportError(e); }
    }, 0);
  }
  unobserve() {}
  disconnect() {}
  takeRecords() { return []; }
}
class PerformanceObserver { constructor() {} observe() {} disconnect() {} static get supportedEntryTypes() { return []; } }
class AbortSignal extends EventTarget {
  constructor() { super(); this.aborted = false; this.reason = undefined; }
  throwIfAborted() { if (this.aborted) throw this.reason; }
  static timeout() { return new AbortSignal(); }
  static abort(r) { const s = new AbortSignal(); s.aborted = true; s.reason = r; return s; }
}
class AbortController {
  constructor() { this.signal = new AbortSignal(); }
  abort(r) { if (this.signal.aborted) return; this.signal.aborted = true; this.signal.reason = r; this.signal.dispatchEvent(new Event('abort')); }
}
class BroadcastChannel extends EventTarget { constructor(n) { super(); this.name = n; } postMessage() {} close() {} }
class MessageChannel { constructor() { this.port1 = new EventTarget(); this.port2 = new EventTarget(); this.port1.postMessage = d => setTimeout(() => { const e = new MessageEvent('message', { data: d }); if (this.port2.onmessage) this.port2.onmessage(e); this.port2.dispatchEvent(e); }, 0); this.port2.postMessage = d => setTimeout(() => { const e = new MessageEvent('message', { data: d }); if (this.port1.onmessage) this.port1.onmessage(e); this.port1.dispatchEvent(e); }, 0); this.port1.start = this.port2.start = this.port1.close = this.port2.close = () => {}; } }
class Worker extends EventTarget { constructor() { super(); } postMessage() {} terminate() {} }
class Blob { constructor(parts, opts) { this._s = (parts || []).map(String).join(''); this.size = this._s.length; this.type = opts && opts.type || ''; } text() { return Promise.resolve(this._s); } slice() { return this; } }
class File extends Blob { constructor(parts, name, opts) { super(parts, opts); this.name = name; this.lastModified = Date.now(); } }
class FileReader extends EventTarget { readAsText(b) { setTimeout(() => { this.result = b._s; this.readyState = 2; if (this.onload) this.onload({ target: this }); this.dispatchEvent(new Event('load')); }, 0); } readAsDataURL(b) { setTimeout(() => { this.result = 'data:' + b.type + ';base64,' + btoa(unescape(encodeURIComponent(b._s))); if (this.onload) this.onload({ target: this }); }, 0); } }
// A document of its own, apart from the page: what DOMParser and
// document.implementation.createHTMLDocument give. jQuery writes into one
// to test the browser, so handing back the page's document wipes the page.
function detachedDocument(html, title) {
  const root = document.createElement('html');
  root.innerHTML = String(html || '');
  let head = root.querySelector('head');
  let body = root.querySelector('body');
  if (!head) { head = document.createElement('head'); root.insertBefore(head, root.firstChild); }
  if (!body) {
    body = document.createElement('body');
    for (const c of [...root.childNodes]) if (c !== head) body.appendChild(c);
    root.appendChild(body);
  }
  let docTitle = title === undefined ? '' : String(title);
  const d = {
    nodeType: DOC, nodeName: '#document', documentElement: root, head, body,
    get title() { return docTitle; }, set title(v) { docTitle = String(v); },
    get childNodes() { return [root]; }, get children() { return [root]; },
    get firstChild() { return root; }, get firstElementChild() { return root; },
    readyState: 'complete', defaultView: null, location: null, cookie: '', characterSet: 'UTF-8', compatMode: 'CSS1Compat',
    querySelector: q => root.querySelector(q),
    querySelectorAll: q => root.querySelectorAll(q),
    getElementById: i => root.querySelector('#' + CSS.escape(String(i))),
    getElementsByTagName: t => root.querySelectorAll(t),
    getElementsByClassName: c => root.getElementsByClassName(c),
    getElementsByName: n => root.querySelectorAll('[name="' + n + '"]'),
    createElement: t => document.createElement(t),
    createElementNS: (ns, t) => document.createElementNS(ns, t),
    createTextNode: t => document.createTextNode(t),
    createComment: t => document.createComment(t),
    createDocumentFragment: () => document.createDocumentFragment(),
    createEvent: t => document.createEvent(t),
    importNode: (n, deep) => n.cloneNode(deep),
    adoptNode: n => n,
    addEventListener() {}, removeEventListener() {},
    get implementation() { return document.implementation; },
  };
  return d;
}
class DOMParser {
  parseFromString(s) { return detachedDocument(s); }
}
class XMLSerializer { serializeToString(n) { return n.outerHTML || ''; } }
class Image extends HTMLElement { constructor(w, h) { super(); const e = document.createElement('img'); if (w) e.width = w; if (h) e.height = h; return e; } }
class Option { constructor(text, value, dflt, sel) { const o = document.createElement('option'); o.textContent = text || ''; if (value !== undefined) o.value = value; if (sel) o.selected = true; return o; } }
class Audio { constructor() { return document.createElement('audio'); } }

function mediaQueryList(q) {
  const m = { media: q, matches: N('media', String(q)), onchange: null, addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {}, dispatchEvent() { return true; } };
  return m;
}
function computedStyle(el) {
  const inline = el && el.style;
  return new Proxy({}, {
    get(t, p) {
      if (p === 'getPropertyValue') return q => N('computed', el._id, String(q)) || '';
      if (typeof p !== 'string') return undefined;
      if (p === 'length') return 0;
      const v = N('computed', el._id, camelToDash(p));
      if (v) return v;
      return inline ? inline[p] : '';
    },
  });
}

const navigator = {
  userAgent: 'Mozilla/5.0 (EverOS; x86_64) EverBrowser/0.2',
  appName: 'Netscape', appVersion: '5.0 (EverOS)', appCodeName: 'Mozilla', product: 'Gecko', vendor: '',
  platform: 'EverOS x86_64', language: 'ru-RU', languages: ['ru-RU', 'ru', 'en'], onLine: true, cookieEnabled: true,
  hardwareConcurrency: 1, maxTouchPoints: 0, doNotTrack: null, webdriver: false, pdfViewerEnabled: false, deviceMemory: 1,
  sendBeacon: () => true, javaEnabled: () => false, vibrate: () => false,
  clipboard: { writeText: () => Promise.resolve(), readText: () => Promise.resolve('') },
  serviceWorker: { register: () => Promise.reject(new Error('not supported')), getRegistrations: () => Promise.resolve([]), ready: new Promise(() => {}), addEventListener() {}, controller: null },
  permissions: { query: () => Promise.resolve({ state: 'denied', addEventListener() {} }) },
  mediaDevices: { getUserMedia: () => Promise.reject(new Error('not supported')), enumerateDevices: () => Promise.resolve([]) },
  geolocation: { getCurrentPosition(ok, err) { if (err) err({ code: 1, message: 'denied' }); }, watchPosition() { return 0; }, clearWatch() {} },
  storage: { estimate: () => Promise.resolve({ quota: 0, usage: 0 }), persist: () => Promise.resolve(false) },
  userAgentData: { brands: [], mobile: false, platform: 'EverOS', getHighEntropyValues: () => Promise.resolve({}) },
  connection: { effectiveType: '4g', downlink: 10, rtt: 50, saveData: false, addEventListener() {} },
  locks: { request: (n, o, f) => Promise.resolve((f || o)()) },
};
const history = {
  length: 1, state: null, scrollRestoration: 'auto',
  pushState(s, t, u) { this.state = s; this.length++; if (u !== undefined && u !== null) N('seturl', N('resolve', String(u))); },
  replaceState(s, t, u) { this.state = s; if (u !== undefined && u !== null) N('seturl', N('resolve', String(u))); },
  back() { N('back'); }, forward() {}, go(n) { if (n < 0) N('back'); },
};
const [vw, vh] = N('viewport');
const screen = { width: 1920, height: 1080, availWidth: 1920, availHeight: 1040, colorDepth: 24, pixelDepth: 24, orientation: { type: 'landscape-primary', angle: 0, addEventListener() {} } };
const CSS = { supports: () => false, escape: s => String(s).replace(/([^\w-])/g, '\\$1').replace(/^(\d)/, '\\3$1 '), registerProperty() {} };
const selection = { rangeCount: 0, isCollapsed: true, type: 'None', toString: () => '', removeAllRanges() {}, addRange() {}, getRangeAt() { return document.createRange(); }, collapse() {}, empty() {} };
const crypto = {
  getRandomValues(a) { for (let i = 0; i < a.length; i++) a[i] = Math.floor(Math.random() * 4294967296); return a; },
  randomUUID() { const h = '0123456789abcdef'; let s = ''; for (let i = 0; i < 36; i++) s += [8, 13, 18, 23].includes(i) ? '-' : i === 14 ? '4' : h[Math.floor(Math.random() * 16)]; return s; },
  subtle: {},
};

Object.setPrototypeOf(G, EventTarget.prototype);
defineEventProps(G);
Object.assign(G, {
  document, location, navigator, history, screen, CSS, crypto, top: G, parent: G, frames: G, opener: null, closed: false, name: '',
  length: 0, frameElement: null, origin: location.origin, isSecureContext: true, crossOriginIsolated: false,
  innerWidth: vw, innerHeight: vh, outerWidth: vw, outerHeight: vh, devicePixelRatio: 1,
  scrollX: 0, scrollY: 0, pageXOffset: 0, pageYOffset: 0, screenX: 0, screenY: 0, screenLeft: 0, screenTop: 0,
  localStorage: makeStorage('local'), sessionStorage: makeStorage('session'),
  Node, Element, HTMLElement, CharacterData, Text, Comment, Document, DocumentFragment, EventTarget,
  HTMLInputElement, HTMLFormElement, HTMLSelectElement, HTMLAnchorElement, HTMLImageElement, HTMLScriptElement, HTMLTemplateElement,
  Event, CustomEvent, UIEvent, MouseEvent, PointerEvent, KeyboardEvent, FocusEvent, InputEvent, SubmitEvent, ErrorEvent, ProgressEvent, MessageEvent, PopStateEvent,
  URL, URLSearchParams, Headers, Request, Response, XMLHttpRequest, FormData, TextEncoder, TextDecoder,
  MutationObserver, ResizeObserver, IntersectionObserver, PerformanceObserver, AbortController, AbortSignal, BroadcastChannel, MessageChannel,
  Worker, Blob, File, FileReader, DOMParser, XMLSerializer, Image, Option, Audio, DOMTokenList,
  getComputedStyle: el => computedStyle(el),
  matchMedia: q => mediaQueryList(q),
  getSelection: () => selection,
  scrollTo() {}, scrollBy() {}, scroll() {}, focus() {}, blur() {}, print() {}, stop() {}, moveTo() {}, resizeTo() {},
  open(u) { if (u) N('navigate', N('resolve', String(u))); return null; },
  close() {},
  alert(m) { console.log('alert: ' + m); },
  confirm(m) { console.log('confirm: ' + m); return true; },
  prompt(m, d) { console.log('prompt: ' + m); return d === undefined ? null : d; },
  postMessage(d) { setTimeout(() => { const e = new MessageEvent('message', { data: d }); e.origin = location.origin; e.source = G; G.dispatchEvent(e); }, 0); },
  reportError,
  structuredClone: v => v === undefined ? undefined : JSON.parse(JSON.stringify(v)),
  customElements: { define() {}, get() {}, whenDefined: () => new Promise(() => {}), upgrade() {} },
  trustedTypes: { createPolicy: (n, r) => ({ createHTML: s => r && r.createHTML ? r.createHTML(s) : s, createScript: s => s, createScriptURL: s => s }) },
  visualViewport: { width: vw, height: vh, scale: 1, offsetLeft: 0, offsetTop: 0, addEventListener() {}, removeEventListener() {} },
  speechSynthesis: { speak() {}, cancel() {}, getVoices: () => [] },
  indexedDB: undefined,
});
G.__readyState = 'loading';
G.onerror = null;
G.__everos_dispatch = function (id, type, x, y) {
  const el = wrap(id);
  if (!el) return 1;
  const init = { bubbles: true, cancelable: true, clientX: x, clientY: y };
  const ev = type === 'click' || type === 'mousedown' || type === 'mouseup' || type === 'mouseover' || type === 'mouseout'
    ? new MouseEvent(type, init)
    : type === 'submit' ? new SubmitEvent(type, init)
    : type === 'input' ? new InputEvent(type, init)
    : type === 'focus' || type === 'blur' ? new FocusEvent(type, { bubbles: false })
    : new Event(type, init);
  ev.isTrusted = true;
  return el.dispatchEvent(ev) ? 1 : 0;
};
G.__everos_key = function (id, key, down) {
  const el = wrap(id) || document.body || document;
  const ev = new KeyboardEvent(down ? 'keydown' : 'keyup', { bubbles: true, cancelable: true, key, keyCode: key.length === 1 ? key.toUpperCase().charCodeAt(0) : ({ Enter: 13, Escape: 27, Backspace: 8, Tab: 9, ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40, Delete: 46 })[key] || 0 });
  ev.isTrusted = true;
  return el.dispatchEvent(ev) ? 1 : 0;
};
G.__everos_loaded = function (phase) {
  if (phase === 'interactive') {
    G.__readyState = 'interactive';
    document.dispatchEvent(new Event('readystatechange'));
    document.dispatchEvent(new Event('DOMContentLoaded', { bubbles: true }));
  } else {
    G.__readyState = 'complete';
    document.dispatchEvent(new Event('readystatechange'));
    G.dispatchEvent(new Event('load'));
    G.dispatchEvent(new Event('pageshow'));
  }
};
})();
