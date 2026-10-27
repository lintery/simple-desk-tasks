// One widget window: «Сегодня» / «Предстоящие» / «Без срока», picked by window label.

import { api, focusSelf, label, listenAll, listenMine } from './lib/api.js';
import { initBackdrop } from './lib/backdrop.js';
import { addDays, dayLabel, daysBetween, fromDateStr, isoWeekday, plural, toDateStr, WEEKDAYS_SHORT } from './lib/dates.js';
import { I, esc } from './lib/icons.js';
import {
  bucketOf, clock, isOverdue, nextOccurrence, nodateView, orderKey, overdueLabel, repeatLabel, todayView,
  upcomingView,
} from './lib/model.js';
import { parse, trimUrl, URL_RE } from './lib/nlp.js';

const KIND = ['today', 'upcoming', 'nodate'].includes(label) ? label : 'today';
const UNDO_MS = 5000;
const MONTHS_GEN = ['января', 'февраля', 'марта', 'апреля', 'мая', 'июня', 'июля', 'августа', 'сентября', 'октября', 'ноября', 'декабря'];
const MONTHS_SHORT = ['янв', 'фев', 'мар', 'апр', 'мая', 'июн', 'июл', 'авг', 'сен', 'окт', 'ноя', 'дек'];
const WEEKDAYS_FULL = ['Воскресенье', 'Понедельник', 'Вторник', 'Среда', 'Четверг', 'Пятница', 'Суббота'];
const TITLES = { today: 'Сегодня', upcoming: 'Предстоящие', nodate: 'Без срока' };

const $ = (id) => document.getElementById(id);
const listEl = $('list');
const snkEl = $('snk');
const addEl = $('add');
const addInput = $('add-input');
const parsedEl = $('parsed');

let tasks = [];
/** id → task as it was before completion; shown struck-through during the undo window. */
const ghosts = new Map();
/** id → {html, el}: rows are reused when unchanged, so hover and animations survive re-renders. */
const rowCache = new Map();
let editingId = null;
/** Set while tasks.json can't be read: shown instead of a misleading empty list. */
let storeError = null;
let renderPending = false;
/** A mouse button is held on the list: a re-render now would pull the pressed
 *  row out from under the cursor, and the click it ends with would be lost. */
let pressing = false;
let snack = null;
let lastMinute = '';

// ---------------------------------------------------------------- helpers

const shortDate = (d) => `${d.getDate()} ${MONTHS_SHORT[d.getMonth()]}`;

function relDays(n) {
  if (n === 7) return 'через неделю';
  if (n % 7 === 0 && n <= 28) return `через ${n / 7} ${plural(n / 7, 'неделю', 'недели', 'недель')}`;
  return `через ${n} ${plural(n, 'день', 'дня', 'дней')}`;
}

/** «3 нед.» / «2 мес.» for undated tasks lying around for 2+ weeks. */
function staleLabel(t, c) {
  const days = daysBetween(toDateStr(new Date(t.created)), c.today);
  if (days < 14) return '';
  return days < 60 ? `${Math.floor(days / 7)} нед.` : `${Math.floor(days / 30)} мес.`;
}

function minutesUntil(time, c) {
  const [h, m] = time.split(':').map(Number);
  const [nh, nm] = c.time.split(':').map(Number);
  return h * 60 + m - (nh * 60 + nm);
}

/** Default date for a task typed into this widget without a date. */
function defaultDue(c) {
  if (KIND === 'today') return c.today;
  if (KIND === 'upcoming') return addDays(c.today, 1);
  return null;
}

/** Parsed chips, with the date chip showing the date that will really be saved. */
function finalChips(p, due) {
  return p.chips
    .map((ch) => (ch.kind !== 'date' ? ch : due ? { kind: 'date', label: dayLabel(due, clock().today) } : null))
    .filter(Boolean);
}

function chipHtml(chip) {
  const icon = chip.kind === 'date' ? I.cal : chip.kind === 'time' ? I.clock : I.rep;
  return `<span class="chip">${icon}${esc(chip.label)}</span>`;
}

// ---------------------------------------------------------------- rows

function metaHtml(t, c, opts) {
  const parts = [];
  if (t.repeat) parts.push(I.rep);
  let cls = 'meta';
  if (t.time) {
    parts.push(esc(t.time));
    if (opts.next) cls += ' next';
  } else if (KIND === 'nodate') {
    const s = staleLabel(t, c);
    if (s) {
      cls += ' stale';
      parts.push(s);
    }
  }
  return parts.length ? `<div class="${cls}">${parts.join('')}</div>` : '';
}

/** Tooltip for the task text: the details the time column only hints at
 *  (the time itself is covered by the actions pill on hover). */
function rowHint(t, c, opts) {
  const parts = [];
  if (t.repeat) parts.push(repeatLabel(t.repeat));
  if (t.time && opts.next) {
    const m = minutesUntil(t.time, c);
    parts.push(m >= 60 ? `через ${Math.floor(m / 60)} ч ${m % 60} мин` : `через ${m} мин`);
  } else if (!t.time && KIND === 'nodate' && staleLabel(t, c)) {
    parts.push(`лежит с ${shortDate(new Date(t.created))}`);
  }
  if (!parts.length) return '';
  const s = parts.join(' · ');
  return ` title="${esc(s[0].toUpperCase() + s.slice(1))}"`;
}

/** Task title with links made clickable (shown short: host/path…, full URL on hover). */
function titleHtml(title) {
  let out = '';
  let last = 0;
  for (const m of title.matchAll(URL_RE)) {
    const raw = trimUrl(m[0]);
    const href = /^https?:\/\//i.test(raw) ? raw : `https://${raw}`;
    let shown = raw.replace(/^https?:\/\//i, '').replace(/^www\./i, '');
    if (shown.length > 32) shown = `${shown.slice(0, 30)}…`;
    out += esc(title.slice(last, m.index))
      + `<a class="url" data-url="${esc(href)}" title="${esc(href)}" draggable="false">${esc(shown)}</a>`;
    last = m.index + raw.length;
  }
  return out + esc(title.slice(last));
}

function actsHtml(withTomorrow) {
  const later = KIND === 'upcoming' ? 'На день позже' : 'На завтра';
  return `<div class="acts">${withTomorrow ? `<button class="ib" data-act="later" title="${later}">${I.tomorrow}</button>` : ''}`
    + `<button class="ib" data-act="date" title="Дата…">${I.cal}</button>`
    + `<button class="ib del" data-act="del" title="Удалить">${I.del}</button></div>`;
}

function rowHtml(t, c, opts = {}) {
  const done = ghosts.has(t.id);
  if (opts.overdue) {
    const pills = (t.due < c.today ? '<button class="pill" data-act="to-today">Сегодня</button>' : '')
      + '<button class="pill" data-act="to-tomorrow">Завтра</button>';
    return `<div class="row od${done ? ' done' : ''}" data-id="${t.id}" draggable="${!done}">`
      + `<button class="cb" data-act="check" title="Выполнить">${I.tick}</button>`
      + `<div class="body"><div class="tx" data-act="edit"><span>${titleHtml(t.title)}</span></div>`
      + `<div class="odl"><span>Просрочено · ${esc(overdueLabel(t, c))}</span><span class="rs">${pills}</span></div></div>`
      + `${actsHtml(false)}</div>`;
  }
  return `<div class="row${done ? ' done' : ''}" data-id="${t.id}" draggable="${!done}">`
    + `<button class="cb" data-act="check" title="Выполнить">${I.tick}</button>`
    + `<div class="tx" data-act="edit"${rowHint(t, c, opts)}><span>${titleHtml(t.title)}</span></div>`
    + `${actsHtml(true)}${metaHtml(t, c, opts)}</div>`;
}

function rowNode(t, c, opts) {
  const html = rowHtml(t, c, opts);
  const cached = rowCache.get(t.id);
  if (cached && cached.html === html) return cached.el;
  const tpl = document.createElement('template');
  tpl.innerHTML = html;
  const el = tpl.content.firstElementChild;
  rowCache.set(t.id, { html, el });
  return el;
}

// ---------------------------------------------------------------- render

/** Tasks with completed-but-undoable ones swapped back to their old state. */
function visibleTasks() {
  const list = tasks.map((t) => (ghosts.has(t.id) ? ghosts.get(t.id) : t));
  for (const [id, snap] of ghosts) if (!tasks.some((t) => t.id === id)) list.push(snap);
  return list;
}

function render() {
  if (editingId || pressing) {
    renderPending = true;
    return;
  }
  renderPending = false;
  const c = clock();
  lastMinute = c.time;
  const all = visibleTasks();
  const nodes = [];
  const used = new Set();
  const push = (t, opts) => {
    used.add(t.id);
    return rowNode(t, c, opts);
  };

  if (KIND === 'today') {
    const v = todayView(all, c);
    const next = v.active.find((t) => t.time && t.time > c.time && !ghosts.has(t.id));
    for (const t of v.active) nodes.push(push(t, { next: t === next }));
    if (v.overdue.length) {
      nodes.push(Object.assign(document.createElement('div'), { className: 'grow' }));
      const box = document.createElement('div');
      box.className = 'ods';
      const live = v.overdue.filter((t) => !ghosts.has(t.id)).length;
      box.innerHTML = `<div class="ods-h">Не успели <span class="n">${v.overdue.length}</span>`
        + (live ? '<button class="lnk" data-act="all-tomorrow">Всё на завтра</button>' : '') + '</div>';
      for (const t of v.overdue) box.append(push(t, { overdue: true }));
      nodes.push(box);
    }
    if (!nodes.length) nodes.push(emptyNode(v.doneToday ? 'Всё сделано ✓' : 'Сегодня чисто. Можно выдохнуть.'));
    renderHeader(c, v);
  } else if (KIND === 'nodate') {
    const v = nodateView(all, c);
    for (const t of v) nodes.push(push(t, {}));
    if (!v.length) nodes.push(emptyNode('Пусто. Всё «когда-нибудь» — сюда.'));
    renderHeader(c, { count: v.filter((t) => !ghosts.has(t.id)).length });
  } else {
    const groups = upcomingView(all, c);
    for (const g of groups) {
      nodes.push(groupHeader(g.date, c));
      for (const t of g.tasks) nodes.push(push(t, {}));
    }
    if (!groups.length) nodes.push(emptyNode('Впереди тихо. Попробуйте «в пт» или «25.10».'));
    const count = groups.reduce((n, g) => n + g.tasks.filter((t) => !ghosts.has(t.id)).length, 0);
    renderHeader(c, { count });
  }

  if (storeError) {
    const banner = document.createElement('div');
    banner.className = 'banner';
    banner.textContent = 'Не получается прочитать файл задач — пробую снова. Ничего не удалено.';
    banner.title = storeError;
    nodes.splice(0, nodes.length, banner, ...nodes.filter((n) => !n.classList.contains('empty')));
  }
  for (const id of [...rowCache.keys()]) if (!used.has(id)) rowCache.delete(id);
  listEl.replaceChildren(...nodes);
}

function emptyNode(text) {
  const el = document.createElement('div');
  el.className = 'empty';
  el.textContent = text;
  return el;
}

function groupHeader(date, c) {
  const el = document.createElement('div');
  el.className = 'gh';
  el.dataset.date = date; // drop target: a task dropped under this header moves to this day
  const d = fromDateStr(date);
  const n = daysBetween(c.today, date);
  if (n === 1) {
    el.innerHTML = `<b>Завтра</b><span>${WEEKDAYS_SHORT[d.getDay()]}, ${shortDate(d)}</span><i></i>`;
  } else if (n < 7) {
    el.innerHTML = `<b>${WEEKDAYS_FULL[d.getDay()]}</b><span>${shortDate(d)}</span><i></i><em>${relDays(n)}</em>`;
  } else {
    const wd = WEEKDAYS_SHORT[d.getDay()];
    const year = d.getFullYear() !== fromDateStr(c.today).getFullYear() ? ` ${d.getFullYear()}` : '';
    el.innerHTML = `<b>${wd[0].toUpperCase()}${wd.slice(1)}, ${shortDate(d)}${year}</b><i></i><em>${relDays(n)}</em>`;
  }
  return el;
}

function renderHeader(c, v) {
  const now = fromDateStr(c.today);
  $('title').textContent = TITLES[KIND];
  if (KIND === 'today') {
    $('sub').textContent = `${WEEKDAYS_SHORT[now.getDay()]}, ${now.getDate()} ${MONTHS_GEN[now.getMonth()]}`;
    const late = v.overdue.filter((t) => !ghosts.has(t.id)).length;
    const left = v.active.filter((t) => !ghosts.has(t.id)).length;
    const done = v.doneToday;
    $('cnt').innerHTML = (late ? `<span class="odb" title="Просрочено">${late}</span>` : '')
      + `<span title="Осталось на сегодня">${left}</span>`;
    const total = done + left + late;
    const prog = $('prog');
    prog.hidden = false;
    prog.firstElementChild.style.width = total ? `${(done / total) * 100}%` : '0';
  } else {
    $('sub').textContent = KIND === 'nodate' ? 'когда-нибудь' : 'впереди';
    $('cnt').innerHTML = `<span>${v.count}</span>`;
  }
}

// ---------------------------------------------------------------- snackbar & undo

function finishSnack() {
  if (!snack) return;
  clearTimeout(snack.timer);
  const s = snack;
  snack = null;
  snkEl.hidden = true;
  s.onExpire?.();
}

function showSnack({ id, prefix, text, undo, onExpire }) {
  finishSnack();
  snkEl.classList.remove('err');
  snkEl.innerHTML = `${I.check}<b>${prefix ? `<em>${prefix}</em> ` : ''}${esc(text)}</b>`
    + '<button class="undo" data-act="undo">Отменить</button><kbd>Ctrl Z</kbd>';
  snkEl.style.setProperty('--snk-ms', `${UNDO_MS}ms`);
  snkEl.hidden = true;
  void snkEl.offsetWidth; // restart the drain animation
  snkEl.hidden = false;
  snack = { id, undo, onExpire, timer: setTimeout(finishSnack, UNDO_MS) };
}

/** Drops the current snackbar without running its expiry (the action failed). */
function cancelSnack(id) {
  if (!snack || snack.id !== id) return;
  clearTimeout(snack.timer);
  snack = null;
  snkEl.hidden = true;
}

/** Shows a failed save in the snackbar slot; nothing to undo. */
function flash(err) {
  finishSnack();
  snkEl.classList.add('err');
  snkEl.innerHTML = `<b>${esc(String(err?.message ?? err ?? 'Не получилось сохранить'))}</b>`;
  snkEl.style.setProperty('--snk-ms', '4000ms');
  snkEl.hidden = true;
  void snkEl.offsetWidth;
  snkEl.hidden = false;
  snack = { id: null, undo: () => {}, timer: setTimeout(finishSnack, 4000) };
}

const save = (promise) => promise.catch(flash);

function undoSnack() {
  if (!snack) return;
  clearTimeout(snack.timer);
  const s = snack;
  snack = null;
  snkEl.hidden = true;
  s.undo();
}

function collapseGhost(id) {
  if (!ghosts.has(id)) return;
  const el = rowCache.get(id)?.el;
  const drop = () => {
    ghosts.delete(id);
    render();
  };
  if (!el || !el.isConnected) return drop();
  el.classList.add('gone');
  el.addEventListener('animationend', drop, { once: true });
  setTimeout(drop, 400); // in case the animation never runs
}

// ---------------------------------------------------------------- actions

const find = (id) => tasks.find((t) => t.id === id);

function complete(id) {
  if (ghosts.has(id)) {
    // Unchecking a just-completed row undoes it; a row already collapsing is ignored.
    if (snack?.id === id) undoSnack();
    return;
  }
  const t = find(id);
  if (!t) return;
  const c = clock();
  const next = t.repeat ? nextOccurrence(t, c) : null;
  const patch = next ? { due: next } : { done: true };
  const back = next ? { due: t.due } : { done: false };
  ghosts.set(id, { ...t });
  render();
  api.update(id, patch).catch((e) => {
    ghosts.delete(id);
    cancelSnack(id);
    render();
    flash(e);
  });
  showSnack({
    id,
    prefix: next ? `Дальше: ${shortDate(fromDateStr(next))} ·` : '',
    text: t.title,
    undo: () => {
      ghosts.delete(id);
      render();
      save(api.update(id, back));
    },
    onExpire: () => collapseGhost(id),
  });
}

function remove(id) {
  const t = find(id);
  if (!t) return;
  save(api.remove(id).then((removed) => showSnack({
    id,
    prefix: 'Удалено:',
    text: t.title,
    undo: () => save(api.restore(removed ?? t)),
  })));
}

/** Moves a task to `due`; a time that has already passed today is dropped.
 * «Без срока» (`null`) also drops a time and a repeat: then it can be undone. */
function setDue(id, due) {
  const t = find(id);
  if (!t) return;
  const c = clock();
  const patch = { due };
  if (due === c.today && t.time && t.time <= c.time) patch.time = null;
  const loses = due === null && t.due && (t.repeat || t.time);
  save(api.update(id, patch).then(() => {
    if (loses) offerUndoNoDate(t);
  }));
}

/** The snackbar after a task lost its date and with it a repeat or a time. */
function offerUndoNoDate(t) {
  showSnack({
    id: t.id,
    prefix: t.repeat ? 'Без срока, повтор снят:' : 'Без срока, время снято:',
    text: t.title,
    undo: () => save(api.update(t.id, { due: t.due, time: t.time, repeat: t.repeat, order: t.order ?? null })),
  });
}

/**
 * Patch for a re-parsed text (`p` parsed with the task's date as anchor): a named
 * date/recurrence moves the task; a bare time («в 18») only sets the time and keeps
 * the task's own date — unless that date is already past.
 */
function parsedPatch(p, t) {
  const patch = {};
  if (p.hasDate || !t.due || t.due < clock().today) patch.due = p.due;
  if (p.time) patch.time = p.time;
  if (p.repeat) patch.repeat = p.repeat;
  return patch;
}

function later(id) {
  const t = find(id);
  if (!t) return;
  const c = clock();
  const base = KIND === 'upcoming' && t.due ? t.due : c.today;
  setDue(id, addDays(base, 1));
}

function allTomorrow() {
  const c = clock();
  const tomorrow = addDays(c.today, 1);
  for (const t of todayView(tasks, c).overdue) save(api.update(t.id, { due: tomorrow }));
}

// ---------------------------------------------------------------- inline edit

function startEdit(id) {
  // Clicking another row's text ends the current edit on mousedown; its
  // re-render waited for this click (see `pressing`) and must land first.
  if (renderPending && !editingId) render();
  const t = find(id);
  const row = rowCache.get(id)?.el;
  if (!t || !row || ghosts.has(id) || editingId) return;
  editingId = id;
  row.classList.add('editing');
  row.draggable = false; // so selecting text in the input doesn't drag the row
  const tx = row.querySelector('.tx');
  tx.removeAttribute('title');
  tx.innerHTML = '';
  const input = document.createElement('input');
  input.value = t.title;
  tx.append(input);
  const bar = document.createElement('div');
  bar.className = 'edit-bar';
  bar.innerHTML = '<span class="chips"></span><span class="hint"><kbd>↵</kbd>сохранить<kbd>Esc</kbd></span>';
  row.append(bar);
  const chips = bar.querySelector('.chips');
  let finished = false;

  const finish = (commit) => {
    if (finished) return;
    finished = true;
    const value = input.value.trim();
    editingId = null;
    rowCache.delete(id); // rebuild the row from data
    // An unchanged title is never re-parsed: leftover words like «на пятницу»
    // in a stored title must not silently move the task.
    if (commit && value && value !== t.title) {
      const p = parse(value, new Date(), t.due);
      const patch = p.chips.length ? parsedPatch(p, t) : {};
      if (p.title !== t.title) patch.title = p.title;
      if (Object.keys(patch).length) save(api.update(id, patch));
    }
    render();
  };

  input.addEventListener('input', () => {
    if (input.value.trim() === t.title) {
      chips.innerHTML = ''; // unchanged text is saved as is, nothing re-parsed
      return;
    }
    const p = parse(input.value, new Date(), t.due);
    const patch = p.chips.length ? parsedPatch(p, t) : {};
    chips.innerHTML = finalChips(p, 'due' in patch ? patch.due : t.due).map(chipHtml).join('');
  });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') { e.preventDefault(); finish(true); }
    if (e.key === 'Escape') { e.preventDefault(); finish(false); }
  });
  input.addEventListener('blur', () => finish(true));
  input.focus();
  input.setSelectionRange(input.value.length, input.value.length);
}

// ---------------------------------------------------------------- date menu

let popEl = null;

function closePop() {
  popEl?.remove();
  popEl = null;
}

function openDateMenu(id, anchor) {
  closePop();
  const t = find(id);
  if (!t) return;
  const c = clock();
  const tomorrow = addDays(c.today, 1);
  const wd = isoWeekday(c.today);
  const nextMon = addDays(c.today, ((1 - wd + 7) % 7) || 7);
  const fmt = (d) => { const x = fromDateStr(d); return `${WEEKDAYS_SHORT[x.getDay()]}, ${shortDate(x)}`; };
  const items = [
    ['Сегодня', c.today], ['Завтра', tomorrow],
    ...(nextMon !== tomorrow ? [['Понедельник', nextMon]] : []),
    ['Через неделю', addDays(c.today, 7)],
  ];
  popEl = document.createElement('div');
  popEl.className = 'pop';
  popEl.innerHTML = items.map(([name, d]) => `<button data-due="${d}"${t.due === d ? ' class="sel"' : ''}>${name}<span>${fmt(d)}</span></button>`).join('')
    + `<button data-due=""${t.due ? '' : ' class="sel"'}>Без срока<span></span></button>`
    + (t.repeat ? '<button data-norepeat>Не повторять<span>' + esc(repeatLabel(t.repeat)) + '</span></button>' : '')
    + '<hr><input placeholder="или: 25.10 в 15, каждый пн" spellcheck="false">';
  document.body.append(popEl);

  const r = anchor.getBoundingClientRect();
  const pw = popEl.offsetWidth, ph = popEl.offsetHeight;
  popEl.style.left = `${Math.max(6, Math.min(r.right - pw, innerWidth - pw - 6))}px`;
  popEl.style.top = `${r.bottom + ph + 6 > innerHeight ? Math.max(6, r.top - ph - 4) : r.bottom + 4}px`;

  popEl.addEventListener('click', (e) => {
    const b = e.target.closest('button');
    if (!b) return;
    if (b.hasAttribute('data-norepeat')) save(api.update(id, { repeat: null }));
    else setDue(id, b.dataset.due || null);
    closePop();
  });
  const input = popEl.querySelector('input');
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') return closePop();
    if (e.key !== 'Enter') return;
    // A lone «пт» / «пятницу» here clearly means a day, so retry it as «в пт» —
    // but only keep the retry if it found a date («3 дня» must not become 15:00).
    let p = parse(`задача ${input.value}`, new Date(), t.due);
    if (!p.chips.length) {
      const retry = parse(`задача в ${input.value}`, new Date(), t.due);
      if (retry.hasDate) p = retry;
    }
    if (!p.chips.length) { input.select(); return; }
    save(api.update(id, parsedPatch(p, t)));
    closePop();
  });
}

document.addEventListener('mousedown', (e) => {
  if (popEl && !popEl.contains(e.target)) closePop();
});
window.addEventListener('blur', closePop);

// ---------------------------------------------------------------- add input

function draft() {
  const c = clock();
  const text = addInput.value.trim();
  if (!text) return null;
  const p = parse(text);
  // In «Предстоящие» a bare time («созвон в 15») means tomorrow, like no date at all.
  const due = (KIND === 'upcoming' && !p.hasDate) || !p.due ? defaultDue(c) : p.due;
  const dest = bucketOf({ due, done: false }, c);
  return { title: p.title, due, time: p.time, repeat: p.repeat, chips: finalChips(p, due), dest };
}

function renderDraft() {
  const d = draft();
  const show = d && (d.chips.length || d.dest !== KIND);
  addEl.classList.toggle('parsed-on', !!show);
  if (!show) {
    parsedEl.innerHTML = '';
    return;
  }
  parsedEl.innerHTML = d.chips.map(chipHtml).join('')
    + (d.dest !== KIND ? `<span class="dest">${I.arrow}${TITLES[d.dest]}</span>` : '');
}

let adding = false;

addInput.addEventListener('input', renderDraft);
addInput.addEventListener('focus', () => addEl.classList.add('on'));
addInput.addEventListener('blur', () => addEl.classList.remove('on'));
addInput.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    addInput.value = '';
    renderDraft();
    addInput.blur();
  }
  if (e.key !== 'Enter') return;
  e.preventDefault();
  const d = draft();
  if (!d || adding) return;
  adding = true;
  api.add(d)
    .then(() => {
      addInput.value = '';
      renderDraft();
    })
    .catch(flash)
    .finally(() => { adding = false; });
});

// ---------------------------------------------------------------- drag & drop
// Rows are dragged with HTML drag & drop, which also works between the widget
// windows: the task id travels as text. Dropping into «Сегодня» moves the task
// to today, into «Предстоящие» to the day it's dropped under (tomorrow above the
// first day), into «Без срока» removes the date at the dropped position.

const DRAG_TYPE = 'application/x-desktask';
let dropLine = null;
/** Id of the task being dragged in any widget (broadcast via Rust): drag data
 *  doesn't reliably reach another WebView2 window, this does. */
let activeDrag = null;
let dragSeq = 0;

const hasType = (e, type) => [...(e.dataTransfer?.types ?? [])].includes(type);

function clearDropMarks() {
  dropLine?.remove();
  dropLine = null;
  document.body.classList.remove('drop-on');
  listEl.querySelector('.gh.drop-target')?.classList.remove('drop-target');
}

/** Rows other than the dragged one, in display order. */
const otherRows = () => [...listEl.querySelectorAll('.row')].filter((r) => !r.classList.contains('dragging'));

/** Index among `otherRows()` the dragged task would land at. */
function dropIndex(rows, clientY) {
  const i = rows.findIndex((r) => {
    const b = r.getBoundingClientRect();
    return clientY < b.top + b.height / 2;
  });
  return i < 0 ? rows.length : i;
}

/** «Предстоящие»: the day whose header is above the pointer (tomorrow above the first). */
function dropDate(clientY) {
  let date = null;
  for (const gh of listEl.querySelectorAll('.gh')) {
    if (gh.getBoundingClientRect().top <= clientY) date = gh;
  }
  return date;
}

function markDrop(clientY) {
  document.body.classList.add('drop-on');
  if (KIND === 'upcoming') {
    const gh = dropDate(clientY);
    const cur = listEl.querySelector('.gh.drop-target');
    if (cur !== gh) {
      cur?.classList.remove('drop-target');
      gh?.classList.add('drop-target');
    }
  } else if (KIND === 'nodate') {
    const rows = otherRows();
    const i = dropIndex(rows, clientY);
    const ref = rows[i] ?? rows[rows.length - 1];
    let y = 4;
    if (ref) y = i < rows.length ? ref.offsetTop - 1 : ref.offsetTop + ref.offsetHeight + 1;
    if (!dropLine?.isConnected) { // a re-render mid-drag drops it
      dropLine = document.createElement('div');
      dropLine.className = 'drop-line';
      listEl.append(dropLine);
    }
    dropLine.style.top = `${y}px`;
  }
}

/** Order value for a drop at `clientY` in «Без срока»; renumbers if values got too close. */
function orderAt(id, clientY) {
  const rows = otherRows();
  const list = rows.map((r) => find(r.dataset.id)).filter(Boolean);
  const i = dropIndex(rows, clientY);
  const prev = list[i - 1], next = list[i];
  if (!prev && !next) return Date.now();
  if (!prev) return orderKey(next) - 1000;
  if (!next) return orderKey(prev) + 1000;
  const a = orderKey(prev), b = orderKey(next);
  if (b - a > 1e-3) return (a + b) / 2;
  // Too many drops into the same gap: renumber everything in the new order.
  list.splice(i, 0, { id });
  list.forEach((t, k) => { if (t.id !== id) save(api.update(t.id, { order: (k + 1) * 1000 })); });
  return (i + 1) * 1000;
}

function dropTask(id, clientY) {
  const t = find(id);
  if (!t || t.done) return;
  const c = clock();
  if (KIND === 'today') {
    if (t.due !== c.today) setDue(id, c.today); // already today (even if overdue): leave as is
  } else if (KIND === 'upcoming') {
    const date = dropDate(clientY)?.dataset.date ?? addDays(c.today, 1);
    if (t.due !== date) setDue(id, date);
  } else {
    const patch = { order: orderAt(id, clientY) };
    if (t.due) patch.due = null;
    const loses = t.due && (t.repeat || t.time); // no date ⇒ no time and no repeat
    save(api.update(id, patch).then(() => {
      if (!loses) return;
      // A drop doesn't focus the target window; without this, Ctrl+Z would go
      // to the widget the task came from.
      focusSelf();
      offerUndoNoDate(t);
    }));
  }
}

listEl.addEventListener('dragstart', (e) => {
  const row = e.target.closest?.('.row');
  if (!row || editingId || ghosts.has(row.dataset.id)) {
    e.preventDefault();
    return;
  }
  closePop();
  const id = row.dataset.id;
  // Our id travels in a private type; plain text is the title, so dropping a
  // task into another app (notes, chat) just pastes its text.
  e.dataTransfer.setData(DRAG_TYPE, id);
  e.dataTransfer.setData('text/plain', find(id)?.title ?? '');
  e.dataTransfer.effectAllowed = 'copyMove';
  activeDrag = id;
  const token = `${label}:${id}:${++dragSeq}:${Date.now()}`;
  api.taskDrag(id, token).catch(() => {});
  let ended = false;
  // Next frame, so the drag image still shows the row at full opacity
  // (unless the drag already ended — then the row would stay dimmed).
  requestAnimationFrame(() => { if (!ended) row.classList.add('dragging'); });
  // On the row itself: a re-render mid-drag detaches it, and a detached
  // element's dragend never bubbles to the list.
  row.addEventListener('dragend', (end) => {
    ended = true;
    row.classList.remove('dragging');
    listEl.querySelector('.row.dragging')?.classList.remove('dragging');
    clearDropMarks();
    // Nobody accepted the drop (it may not reach another WebView2 window at all):
    // let the widget really under the mouse take it.
    if (end.dataTransfer?.dropEffect === 'none') api.dropAtCursor(id).catch(() => {});
    // A moment later: the target window handles its drop before this fires,
    // but its IPC may still be in flight. Only clears if this is still the drag.
    setTimeout(() => api.taskDrag(null, token).catch(() => {}), 400);
  }, { once: true });
});

listenAll('task-drag', (id) => {
  activeDrag = id;
  if (!id) clearDropMarks();
});

listenMine('task-dropped', ({ id, y }) => {
  clearDropMarks();
  dropTask(id, y);
});

/** One of our task drags: it carries the private type, or — when that didn't
 *  cross windows — a widget announced a drag and this one carries nothing a
 *  browser link/page/file would (uri-list, html, Files). */
const isOurDrag = (e) => hasType(e, DRAG_TYPE)
  || (!!activeDrag && !hasType(e, 'Files') && !hasType(e, 'text/uri-list') && !hasType(e, 'text/html'));
const intoInput = (e) => e.target instanceof HTMLInputElement;

document.addEventListener('dragover', (e) => {
  const task = isOurDrag(e);
  if (intoInput(e) && !task && !hasType(e, 'Files')) return; // dropping text into a field: default
  e.preventDefault(); // anything else must never make the widget navigate away
  if (!task) {
    e.dataTransfer.dropEffect = 'none';
    return;
  }
  e.dataTransfer.dropEffect = 'move';
  markDrop(e.clientY);
});

document.addEventListener('dragleave', (e) => {
  if (!e.relatedTarget) clearDropMarks(); // left the window
});

document.addEventListener('drop', (e) => {
  const id = isOurDrag(e) ? e.dataTransfer.getData(DRAG_TYPE) || activeDrag : null;
  if (intoInput(e) && !id && !hasType(e, 'Files')) return; // text into a field: default
  e.preventDefault();
  clearDropMarks();
  if (id) dropTask(id, e.clientY);
});

// ---------------------------------------------------------------- events

listEl.addEventListener('click', (e) => {
  const link = e.target.closest('a[data-url]');
  if (link) {
    e.preventDefault();
    save(api.openUrl(link.dataset.url));
    return;
  }
  const el = e.target.closest('[data-act]');
  if (!el) return;
  const row = el.closest('.row');
  const id = row?.dataset.id;
  // A just-completed row only accepts un-checking during its undo window.
  if (id && ghosts.has(id) && el.dataset.act !== 'check') return;
  switch (el.dataset.act) {
    case 'check': return complete(id);
    case 'edit': return startEdit(id);
    case 'later': return later(id);
    case 'date': return openDateMenu(id, el);
    case 'del': return remove(id);
    case 'to-today': return setDue(id, clock().today);
    case 'to-tomorrow': return setDue(id, addDays(clock().today, 1));
    case 'all-tomorrow': return allTomorrow();
    default:
  }
});

listEl.addEventListener('pointerdown', () => { pressing = true; });
const release = () => {
  if (!pressing) return;
  pressing = false;
  // In a later task, after the click this release produces.
  if (renderPending) setTimeout(() => { if (renderPending) render(); });
};
// pointercancel and dragstart: a row drag started; blur: Alt+Tab or focus
// stolen mid-press. A move with no button held catches any release we missed.
for (const type of ['pointerup', 'pointercancel', 'blur']) window.addEventListener(type, release);
listEl.addEventListener('dragstart', release);
window.addEventListener('pointermove', (e) => { if (pressing && !e.buttons) release(); });

snkEl.addEventListener('click', (e) => {
  if (e.target.closest('[data-act="undo"]')) undoSnack();
});

document.addEventListener('keydown', (e) => {
  const typing = e.target instanceof HTMLInputElement;
  if (e.ctrlKey && e.code === 'KeyZ' && !typing && snack) {
    e.preventDefault();
    undoSnack();
  } else if (e.code === 'KeyN' && !typing && !e.ctrlKey && !e.altKey) {
    e.preventDefault();
    addInput.focus();
  }
});

// Disable the browser context menu: this is a widget, not a page.
document.addEventListener('contextmenu', (e) => {
  if (!(e.target instanceof HTMLInputElement)) e.preventDefault();
});

// What the events already said wins over the first reply, which may be older.
let heardTasks = false, heardStatus = false;
listenAll('tasks-changed', (list) => {
  heardTasks = true;
  tasks = list;
  render();
});

// Widgets are pinned; the header becomes a drag handle only while the tray
// option «Разрешить перемещение» is on.
const setMovable = (on) => {
  // "deep": children (counters with tooltips) also act as the drag handle.
  if (on) $('hd').setAttribute('data-tauri-drag-region', 'deep');
  else $('hd').removeAttribute('data-tauri-drag-region');
  document.body.classList.toggle('movable', !!on);
};
listenAll('widgets-movable', setMovable);
api.movable().then(setMovable).catch(() => {});

listenAll('store-status', (error) => {
  heardStatus = true;
  storeError = error;
  render();
});

setInterval(() => {
  if (clock().time !== lastMinute) render();
}, 15000);

addInput.placeholder = KIND === 'upcoming' ? 'Новая задача · «в пт в 15»' : 'Новая задача';
initBackdrop(document.querySelector('.wall'));
Promise.all([api.tasks(), api.storeStatus()]).then(([list, error]) => {
  if (!heardTasks) tasks = list;
  if (!heardStatus) storeError = error;
  render();
});
