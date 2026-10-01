// Small DOM helpers shared by the views.

/** h('tag', {attr: value, onclick: fn}, ...children) */
export function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs ?? {})) {
    if (v == null || v === false) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else if (k in el && typeof v !== 'string') el[k] = v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  el.append(...children.flat(Infinity).filter(c => c != null && c !== false).map(c => c instanceof Node ? c : String(c)));
  return el;
}

export const $ = (sel, root = document) => root.querySelector(sel);

export function toast(text, ms = 2600) {
  const t = $('#toast');
  t.textContent = text;
  t.showPopover?.();
  clearTimeout(t._timer);
  t._timer = setTimeout(() => t.hidePopover?.(), ms);
}

/** Non-blocking confirmation with a <dialog>. Resolves true/false. */
export function ask(text, ok = 'OK') {
  const d = h('dialog', {},
    h('form', { method: 'dialog' },
      h('p', {}, text),
      h('menu', {}, h('button', { value: 'no', formnovalidate: true }, 'Cancel'), h('button', { value: 'yes' }, ok))));
  document.body.append(d);
  d.showModal();
  return new Promise(res => d.addEventListener('close', () => { res(d.returnValue === 'yes'); d.remove(); }, { once: true }));
}

export function fmtDur(s) {
  s = Math.floor(s || 0);
  const hh = Math.floor(s / 3600), mm = Math.floor(s % 3600 / 60), ss = s % 60;
  return hh ? `${hh}h ${String(mm).padStart(2, '0')}m` : mm ? `${mm}m ${String(ss).padStart(2, '0')}s` : `${ss}s`;
}

export function fmtNum(n, digits = 1) {
  if (n == null || Number.isNaN(n)) return '–';
  const a = Math.abs(n);
  if (a >= 1e6) return (n / 1e6).toFixed(digits) + 'M';
  if (a >= 1e4) return (n / 1e3).toFixed(digits) + 'k';
  return Number.isInteger(n) ? String(n) : n.toFixed(digits);
}

export const fmt = (v, d = 2) => v == null || !Number.isFinite(v) ? '–' : v.toFixed(d);

export function hexId(id, ext) {
  return id.toString(16).toUpperCase().padStart(ext ? 8 : 3, '0');
}

export function hexData(bytes) {
  return Array.from(bytes, b => b.toString(16).toUpperCase().padStart(2, '0')).join(' ');
}

/** "DE AD be ef" -> Uint8Array, or null if malformed. */
export function parseHex(s) {
  const t = s.replace(/[\s:,-]/g, '');
  if (t.length % 2 || /[^0-9a-f]/i.test(t)) return null;
  return Uint8Array.from(t.match(/../g) ?? [], x => parseInt(x, 16));
}

export function download(name, blob) {
  const a = h('a', { href: URL.createObjectURL(blob), download: name });
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 5000);
}

export function timeLabel(s, wall) {
  if (!wall) return `${s.toFixed(3)} s`;
  return new Date(s * 1000).toLocaleString(undefined, { dateStyle: 'short', timeStyle: 'medium' });
}

/** Replace the children of `el`. */
export function fill(el, ...children) {
  el.replaceChildren(...children.flat(Infinity).filter(c => c != null && c !== false));
  return el;
}

export function debounce(fn, ms) {
  let t;
  return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); };
}
