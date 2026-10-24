// «Календарь»: meetings from the Mail.ru calendar (calendar.rs), read-only —
// the month with a dot per calendar on busy days, and an agenda from today.

import { api, listenAll } from './lib/api.js';
import { initBackdrop } from './lib/backdrop.js';
import { esc } from './lib/icons.js';

const $ = (id) => document.getElementById(id);
const w = $('w');
const MIN = 60_000;
const AGENDA_DAYS = 14;
/** The next meeting is highlighted when it starts within this. */
const NEXT_WITHIN = 12 * 60 * MIN;
const MONTHS = ['Январь', 'Февраль', 'Март', 'Апрель', 'Май', 'Июнь', 'Июль', 'Август', 'Сентябрь', 'Октябрь', 'Ноябрь', 'Декабрь'];
const MONTHS_GEN = ['января', 'февраля', 'марта', 'апреля', 'мая', 'июня', 'июля', 'августа', 'сентября', 'октября', 'ноября', 'декабря'];
const MONTHS_SHORT = ['янв', 'фев', 'мар', 'апр', 'мая', 'июн', 'июл', 'авг', 'сен', 'окт', 'ноя', 'дек'];
const WD = ['вс', 'пн', 'вт', 'ср', 'чт', 'пт', 'сб'];
const WD_FULL = ['Воскресенье', 'Понедельник', 'Вторник', 'Среда', 'Четверг', 'Пятница', 'Суббота'];
const SERVICES = [['ktalk', 'Толк'], ['zoom', 'Zoom'], ['telemost', 'Телемост'], ['meet.google', 'Google Meet'], ['teams', 'Microsoft Teams'],
  ['vk.com', 'VK Звонки'], ['jazz', 'SaluteJazz']];

let state = { configured: false, meetings: [], updated: 0, error: null, syncing: false };
/** Month shown, as an offset from the current one. */
let monthOffset = 0;

const plural = (n, [one, few, many]) => {
  const n10 = n % 10, n100 = n % 100;
  if (n10 === 1 && n100 !== 11) return one;
  return n10 >= 2 && n10 <= 4 && (n100 < 12 || n100 > 14) ? few : many;
};
const startOfDay = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate());
const addDays = (d, n) => new Date(d.getFullYear(), d.getMonth(), d.getDate() + n);
const hm = (ms) => new Date(ms).toLocaleTimeString('ru', { hour: '2-digit', minute: '2-digit' });
const service = (link) => SERVICES.find(([k]) => link.includes(k))?.[1] ?? 'Созвон';
const span = (ms) => {
  const m = Math.max(1, Math.round(ms / MIN));
  if (m < 60) return `${m} мин`;
  const h = Math.floor(m / 60), r = m % 60;
  return r ? `${h} ч ${r} мин` : `${h} ч`;
};

/** Sets markup only when it changes: the agenda redraws every 15 s, and a
 * button replaced under a pressed mouse («Подключиться») swallows the click. */
function setHTML(el, html) {
  if (el.dataset.html === html) return;
  el.dataset.html = html;
  el.innerHTML = html;
}

/** Meetings on a day: all-day ones on every day they cover, timed ones on the day they start. */
function onDay(day) {
  const from = day.getTime(), to = addDays(day, 1).getTime();
  return state.meetings.filter((m) => (m.allDay ? m.start < to && m.end > from : m.start >= from && m.start < to));
}

function renderHeader() {
  const now = new Date();
  $('today').textContent = `${WD[now.getDay()]}, ${now.getDate()} ${MONTHS_GEN[now.getMonth()]}`;
  const sync = $('sync');
  sync.style.display = state.configured ? '' : 'none';
  sync.classList.toggle('err', !!state.error);
  sync.classList.toggle('busy', !!state.syncing && !state.error);
  if (state.error) {
    setHTML(sync, 'нет доступа');
    sync.title = state.error;
    return;
  }
  const ago = state.updated ? Date.now() - state.updated : null;
  const text = state.syncing ? 'обновляю' : ago == null ? '' : ago < MIN ? 'только что' : ago < 60 * MIN ? `${Math.round(ago / MIN)} мин назад` : hm(state.updated);
  setHTML(sync, `<svg class="i"><use href="#i-sync"/></svg>${text}`);
  sync.title = 'Mail.ru · обновить сейчас';
}

function renderState() {
  const box = $('state');
  if (!state.configured) {
    setHTML(box, '<div class="cta"><div class="cta-top"><span class="cta-ic"><svg class="i"><use href="#i-cal"/></svg></span>' +
      '<div><h3>Встречи из календаря Mail.ru</h3><p>Сегодня и ближайшие дни, ссылки на&nbsp;созвоны. Только чтение.</p></div></div>' +
      '<button class="btn pri" id="connect">Подключить календарь Mail.ru</button></div>');
  } else if (state.error) {
    const when = state.updated ? `Показаны данные на ${hm(state.updated)}` : 'Встречи пока не загружены';
    setHTML(box, `<div class="banner err"><svg class="i"><use href="#i-alert"/></svg><div><b>${esc(state.error)}</b>${when}</div>` +
      '<button class="lnk" id="fix">Настройки</button></div>');
  } else {
    setHTML(box, '');
  }
  $('sep').style.display = state.configured ? '' : 'none';
  $('agenda').style.display = state.configured ? '' : 'none';
}

function renderMonth() {
  const today = startOfDay(new Date());
  const first = new Date(today.getFullYear(), today.getMonth() + monthOffset, 1);
  $('month').textContent = MONTHS[first.getMonth()];
  $('year').textContent = String(first.getFullYear());
  // Monday-first weeks covering the month.
  const lead = (first.getDay() + 6) % 7;
  const start = addDays(first, -lead);
  const last = new Date(first.getFullYear(), first.getMonth() + 1, 0);
  const weeks = Math.ceil((lead + last.getDate()) / 7);
  const cells = ['пн', 'вт', 'ср', 'чт', 'пт', 'сб', 'вс'].map((d) => `<div class="wd">${d}</div>`);
  for (let i = 0; i < weeks * 7; i++) {
    const day = addDays(start, i);
    const colors = [...new Set(onDay(day).map((m) => m.color || 'var(--ac)'))].slice(0, 3);
    const cls = ['d'];
    if (day.getMonth() !== first.getMonth()) cls.push('out');
    if (day < today) cls.push('past');
    if (day.getDay() === 0 || day.getDay() === 6) cls.push('we');
    if (day.getTime() === today.getTime()) cls.push('today');
    // A click scrolls the agenda to it: only days the agenda lists.
    if (colors.length && day >= today && day < addDays(today, AGENDA_DAYS)) cls.push('go');
    const dots = colors.length ? `<span class="dots">${colors.map((c) => `<i style="--c:${esc(c)}"></i>`).join('')}</span>` : '';
    cells.push(`<div class="${cls.join(' ')}" data-day="${day.getTime()}"><b>${day.getDate()}</b>${dots}</div>`);
  }
  setHTML($('grid'), cells.join(''));
}

function eventHtml(m, now, nextId) {
  const isToday = startOfDay(new Date(m.start)).getTime() === startOfDay(new Date(now)).getTime();
  const cls = ['ev'];
  const live = !m.allDay && m.start <= now && now < m.end;
  if (live) cls.push('now');
  else if (m.id === nextId) cls.push('next');
  else if (isToday && m.end <= now) cls.push('past');
  const time = m.allDay ? '<div class="ev-t all"><b>весь день</b></div>' : `<div class="ev-t"><b>${hm(m.start)}</b><span>${hm(m.end)}</span></div>`;
  const meta = m.link
    ? `<svg class="i"><use href="#i-vid"/></svg><span>${esc(service(m.link))}</span>`
    : m.location
      ? `<svg class="i"><use href="#i-pin"/></svg><span>${esc(m.location)}</span>`
      : `<svg class="i"><use href="#i-dot"/></svg><span>${esc(m.calendar)}</span>`;
  let extra = '';
  if (live) {
    const done = (now - m.start) / Math.max(1, m.end - m.start);
    extra = `<div class="ev-pr"><span>идёт <em>· ещё ${span(m.end - now)}</em></span><span class="pb"><i style="width:${(done * 100).toFixed(1)}%"></i></span></div>`;
  }
  // The live and the next meeting get a full «Подключиться»; any other one
  // with a link a small button at the end of its row (ended ones don't).
  const prominent = live || m.id === nextId;
  const join = m.link && prominent ? `<button class="join" data-link="${esc(m.link)}"><svg class="i"><use href="#i-vid"/></svg>Подключиться</button>` : '';
  const ended = m.end <= now;
  const quick = m.link && !prominent && !ended
    ? `<button class="join sm" data-link="${esc(m.link)}" title="Подключиться · ${esc(service(m.link))}"><svg class="i"><use href="#i-vid"/></svg></button>`
    : '';
  if (m.id === nextId && !live) extra += `<div class="ev-act"><span class="cd">через ${span(m.start - now)}</span>${join}</div>`;
  else if (join) extra += `<div class="ev-act">${join}</div>`;
  return `<div class="${cls.join(' ')}" style="--c:${esc(m.color || 'var(--ac)')}" title="${esc(m.title)}">${time}<div class="ev-c"></div>` +
    `<div class="ev-b"><div class="ev-ttl">${esc(m.title)}</div><div class="ev-m">${meta}</div>${extra}</div>${quick}</div>`;
}

/** A press in the agenda holds its redraws back: one landing between
 * mousedown and mouseup would replace the button and lose the click. */
let pressing = false;
let heldBack = false;
let holdTimer = 0;
// Only a press on a button: one on the scrollbar never reports its release.
// And a missed release (focus lost mid-press) can't hold redraws for long.
$('agenda').addEventListener('pointerdown', (e) => {
  if (!e.target.closest('.join')) return;
  pressing = true;
  clearTimeout(holdTimer);
  holdTimer = setTimeout(release, 1500);
});
window.addEventListener('blur', () => release());
function release() {
  clearTimeout(holdTimer);
  if (!pressing) return;
  pressing = false;
  // After the click this press makes.
  setTimeout(() => {
    if (heldBack && !pressing) {
      heldBack = false;
      renderAgenda();
    }
  }, 0);
}
window.addEventListener('pointerup', release);
window.addEventListener('pointercancel', release);

function renderAgenda() {
  if (!state.configured) return;
  if (pressing) {
    heldBack = true;
    return;
  }
  const now = Date.now();
  const today = startOfDay(new Date(now));
  const next = state.meetings.find((m) => !m.allDay && m.start > now && m.start - now <= NEXT_WITHIN);
  const out = [];
  for (let i = 0; i < AGENDA_DAYS; i++) {
    const day = addDays(today, i);
    const items = onDay(day).sort((a, b) => (b.allDay - a.allDay) || a.start - b.start);
    if (!items.length && i > 0) continue;
    const label = i === 0 ? 'Сегодня' : i === 1 ? 'Завтра' : WD_FULL[day.getDay()];
    const sub = i < 2 ? `${WD[day.getDay()]}, ${day.getDate()} ${MONTHS_SHORT[day.getMonth()]}` : `${day.getDate()} ${MONTHS_SHORT[day.getMonth()]}`;
    const right = i === 0 ? (items.length ? `${items.length} ${plural(items.length, ['встреча', 'встречи', 'встреч'])}` : '')
      : i >= 2 ? `через ${i} ${plural(i, ['день', 'дня', 'дней'])}` : '';
    out.push(`<div class="gh" data-day="${day.getTime()}"><b>${label}</b><span>${sub}</span><i></i>${right ? `<em>${right}</em>` : ''}</div>`);
    if (!items.length) {
      const coming = state.meetings.find((m) => m.start >= addDays(today, 1).getTime());
      const hint = coming
        ? `следующая — ${startOfDay(new Date(coming.start)).getTime() === addDays(today, 1).getTime() ? 'завтра' : `${new Date(coming.start).getDate()} ${MONTHS_SHORT[new Date(coming.start).getMonth()]}`}${coming.allDay ? '' : ` в ${hm(coming.start)}`}`
        : 'на ближайшие две недели пусто';
      out.push(`<div class="free"><div>Встреч нет<small>${hint}</small></div></div>`);
      continue;
    }
    for (const m of items) out.push(eventHtml(m, now, next?.id));
  }
  setHTML($('agenda'), out.join(''));
}

function render() {
  renderHeader();
  renderState();
  renderMonth();
  renderAgenda();
}

function apply(next) {
  if (!next) return;
  state = { ...state, ...next, meetings: next.meetings ?? [] };
  render();
}

w.addEventListener('click', (e) => {
  const target = e.target.closest('button, .d.go');
  if (!target) return;
  if (target.id === 'connect' || target.id === 'fix') api.showSettings('calendar').catch(() => {});
  else if (target.id === 'sync') api.calendarRefresh().catch(() => {});
  else if (target.id === 'prev' || target.id === 'next') {
    monthOffset += target.id === 'prev' ? -1 : 1;
    renderMonth();
  } else if (target.classList.contains('join')) api.openUrl(target.dataset.link).catch(() => {});
  else if (target.dataset.day) {
    // A busy day in the month: scroll the agenda to it, when it's there. Only
    // the agenda moves (scrollIntoView would also shift the widget's own
    // boxes, which hide their overflow but still scroll).
    const agenda = $('agenda');
    const head = agenda.querySelector(`.gh[data-day="${target.dataset.day}"]`);
    if (head) {
      // Today is the top of the list, padding included.
      const top = head === agenda.firstElementChild ? 0 : agenda.scrollTop + head.getBoundingClientRect().top - agenda.getBoundingClientRect().top;
      agenda.scrollTo({ top });
    }
  }
});

const setMovable = (on) => {
  if (on) $('hd').setAttribute('data-tauri-drag-region', 'deep');
  else $('hd').removeAttribute('data-tauri-drag-region');
  document.body.classList.toggle('movable', !!on);
};
listenAll('widgets-movable', setMovable);
api.movable().then(setMovable).catch(() => {});

listenAll('calendar', apply);
listenAll('settings-changed', () => api.calendar().then(apply).catch(() => {}));
api.calendar().then(apply).catch(() => {});
// Countdowns, «идёт», «N мин назад» and the date move on by themselves.
setInterval(() => {
  renderHeader();
  renderAgenda();
}, 15_000);
setInterval(renderMonth, 10 * MIN);
render();
initBackdrop(document.querySelector('.wall'));
