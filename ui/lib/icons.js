// Inline SVG icons (stroke-based, 16×16 grid).

const svg = (body, cls = 'i') => `<svg class="${cls}" viewBox="0 0 16 16" aria-hidden="true">${body}</svg>`;

export const I = {
  tomorrow: svg('<path d="M2.5 8h8.5M8 5l3 3-3 3M13.5 3.5v9"/>'),
  cal: svg('<rect x="2.5" y="3.5" width="11" height="10" rx="2"/><path d="M2.5 6.8h11M5.5 2v3M10.5 2v3"/>'),
  del: svg('<path d="M3 4.5h10M6.3 4.5V3h3.4v1.5M4.4 4.5l.6 8.1c.04.5.45.9.95.9h4.1c.5 0 .91-.4.95-.9l.6-8.1"/>'),
  rep: svg('<path d="M2.8 7.2A5.2 5.2 0 0 1 12 4.6M12.6 2.2v2.9H9.7M13.2 8.8A5.2 5.2 0 0 1 4 11.4M3.4 13.8v-2.9h2.9"/>'),
  clock: svg('<circle cx="8" cy="8" r="5.5"/><path d="M8 5.2V8l1.9 1.3"/>'),
  arrow: svg('<path d="M3 8h10M9.5 4.5 13 8l-3.5 3.5"/>'),
  plus: svg('<path d="M8 3.5v9M3.5 8h9"/>'),
  check: svg('<path d="M3.5 8.4 6.6 11.3 12.5 4.9"/>'),
  flag: svg('<path d="M4 14V2.8M4 3.2h7.6l-1.8 2.9 1.8 2.9H4"/>'),
  list: svg('<path d="M6.5 4.5h6.5M6.5 8h6.5M6.5 11.5h6.5"/><path d="m2.6 4.5.8.8 1.4-1.6M2.6 8l.8.8 1.4-1.6M2.6 11.5l.8.8 1.4-1.6"/>'),
  more: svg('<circle cx="3.6" cy="8" r=".9"/><circle cx="8" cy="8" r=".9"/><circle cx="12.4" cy="8" r=".9"/>'),
  x: svg('<path d="m4.5 4.5 7 7M11.5 4.5l-7 7"/>'),
  task: svg('<rect x="2.5" y="2.5" width="11" height="11" rx="3"/><path d="m5.4 8.1 1.8 1.8 3.5-3.8"/>'),
  tick: '<svg viewBox="0 0 10 10" aria-hidden="true"><path d="M2.2 5.3 4.2 7.2 7.9 3.1"/></svg>',
};

/** A chip of what the parser understood («завтра», «15:00», «каждый пн», «важное»).
 *  `order` ≥ 0: it's new and pops in, that many steps after the first new one. */
export const chipHtml = (chip, order = -1) => {
  const icon = { date: I.cal, time: I.clock, repeat: I.rep, important: I.flag }[chip.kind] ?? '';
  const fresh = order >= 0 ? ` in" style="--d:${order * 40}ms` : '';
  return `<span class="chip ${chip.kind}${fresh}">${icon}${esc(chip.label)}</span>`;
};

/** Puts chips into `el` (then `extra`); only those it didn't show a moment ago
 *  pop in, so typing doesn't make them all flicker. */
export function renderChips(el, chips, extra = '') {
  const before = new Set(el.__chips ?? []);
  const keys = chips.map((ch) => `${ch.kind}|${ch.label}`);
  let n = 0;
  el.innerHTML = chips.map((ch, i) => chipHtml(ch, before.has(keys[i]) ? -1 : n++)).join('') + extra;
  el.__chips = keys;
}

export const esc = (s) =>
  String(s).replace(/[&<>"']/g, (ch) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[ch]);
