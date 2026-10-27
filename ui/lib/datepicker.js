// «Когда»: a task's date, time and recurrence in one popover — quick days, a
// month to pick any date from (with the days a recurrence lands on marked),
// times, recurrence with weekdays, and a line to type it in words.

import { addDays, dayLabel, fromDateStr, isoWeekday, toDateStr } from './dates.js';
import { attachHilite } from './hilite.js';
import { I, chipHtml, esc } from './icons.js';
import { clock, step } from './model.js';
import { parse } from './nlp.js';
import { REPEAT_KINDS, onlyWeekday, parseTime, patchOf, toggleWeekday, withDate, withRepeat, withTime } from './schedule.js';

const MONTHS = ['Январь', 'Февраль', 'Март', 'Апрель', 'Май', 'Июнь', 'Июль', 'Август', 'Сентябрь', 'Октябрь', 'Ноябрь', 'Декабрь'];
const WD = ['пн', 'вт', 'ср', 'чт', 'пт', 'сб', 'вс'];
const REPEAT_NAMES = { daily: 'День', weekdays: 'Будни', weekly: 'Неделя', monthly: 'Месяц', yearly: 'Год' };
const REPEAT_TITLES = {
  daily: 'Каждый день', weekdays: 'По будням', weekly: 'Каждую неделю — дни ниже', monthly: 'Каждый месяц, в это число', yearly: 'Каждый год, в эту дату',
};
const TIMES = ['09:00', '13:00', '18:00'];

/** Typed words, read as a date: a lone «пт» or «пятницу» clearly means a day,
 *  so it's retried as «в пт» — kept only if that found a date («3 дня» must
 *  not become 15:00). */
export function parseWhen(text, anchor) {
  const p = parse(`задача ${text}`, new Date(), anchor);
  if (!p.chips.length) {
    const retry = parse(`задача в ${text}`, new Date(), anchor);
    if (retry.hasDate) return { ...retry, offset: 'задача в '.length };
  }
  return { ...p, offset: 'задача '.length };
}

/**
 * @param task the task (its due, time, repeat)
 * @param on.apply(patch) saves a change (due / time / repeat); a promise that fails if it didn't
 * @param on.current() the task as stored now (to put back after a failed save)
 * @param on.clear() «Без срока»: the caller removes the date (and offers undo)
 * @param on.text(parsed) a date typed in words, parsed (see parseWhen)
 * @param on.close()
 * @returns the popover element (the caller places it)
 */
export function datePicker(task, on) {
  let s = { due: task.due ?? null, time: task.time ?? null, repeat: task.repeat ?? null };
  let c = clock();
  /** First day of the month shown. */
  let month = `${(s.due ?? c.today).slice(0, 7)}-01`;
  /** The weekly rule's day came from the date, not from a click: the first
   *  weekday clicked replaces it («Неделя», «пн» → every Monday). */
  let autoDay = false;
  /** The month is folded away until «Дата…» asks for it (the menu stays short). */
  let calOpen = false;

  const el = document.createElement('div');
  el.className = 'pop dp';
  el.tabIndex = -1; // can hold the keyboard itself (see render)
  // Each part under its own label: Дата, Время, Повтор (and its days), Словами.
  el.innerHTML = `<div class="dp-sec first"><span class="dp-lb">${I.cal}Дата</span><div class="dp-quick"></div></div>`
    + '<div class="dp-cal" hidden><div class="dp-mh"><b></b>'
    + '<button class="ib" data-nav="-1" title="Предыдущий месяц"><svg class="i" viewBox="0 0 16 16"><path d="M9.8 3.6 5.4 8l4.4 4.4"/></svg></button>'
    + '<button class="ib" data-nav="1" title="Следующий месяц"><svg class="i" viewBox="0 0 16 16"><path d="M6.2 3.6 10.6 8l-4.4 4.4"/></svg></button></div>'
    + '<div class="dp-g"></div></div>'
    + `<div class="dp-sec"><span class="dp-lb">${I.clock}Время</span><div class="dp-chips dp-time"></div></div>`
    + `<div class="dp-sec"><span class="dp-lb">${I.rep}Повтор</span><div class="dp-chips dp-rep"></div>`
    + '<div class="dp-wds-box"><span class="dp-lb dp-days">По каким дням</span><div class="dp-wds"></div></div></div>'
    + '<div class="dp-sec"><span class="dp-lb">Или словами</span>'
    + '<div class="dp-txt"><input class="dp-in" placeholder="25.10 в 15, каждый пн" spellcheck="false"><div class="dp-prev"></div></div></div>';
  const $ = (sel) => el.querySelector(sel);
  const text = $('.dp-in');

  const chip = (attr, value, label, on, title = '') =>
    `<button class="dp-c${on ? ' sel' : ''}" ${attr}="${esc(value)}"${title ? ` title="${esc(title)}"` : ''}>${label}</button>`;

  function render() {
    c = clock();

    // Quick days.
    const tomorrow = addDays(c.today, 1);
    const wd = isoWeekday(c.today);
    const monday = addDays(c.today, ((1 - wd + 7) % 7) || 7);
    const quick = [['Сегодня', c.today], ['Завтра', tomorrow]];
    if (monday !== tomorrow) quick.push(['В пн', monday]);
    quick.push(['Через неделю', addDays(c.today, 7)]);
    // «Дата…» opens the month; a date picked there shows on it.
    const other = s.due && !quick.some(([, d]) => d === s.due);
    $('.dp-quick').innerHTML = quick.map(([label, d]) => chip('data-date', d, label, s.due === d, dayLabel(d, c.today))).join('')
      + chip('data-date', '', 'Без срока', !s.due)
      + `<button class="dp-c dp-more${other ? ' sel' : ''}${calOpen ? ' open' : ''}" data-cal title="Выбрать дату в календаре">`
      + `${I.cal}${other ? esc(dayLabel(s.due, c.today)) : 'Дата…'}</button>`;
    $('.dp-cal').hidden = !calOpen;

    // The month: picked day, today, the days a recurrence lands on.
    const first = fromDateStr(month);
    $('.dp-mh b').textContent = `${MONTHS[first.getMonth()]} ${first.getFullYear()}`;
    const lead = (first.getDay() + 6) % 7;
    const start = addDays(month, -lead);
    const end = toDateStr(new Date(first.getFullYear(), first.getMonth() + 1, 0));
    const weeks = Math.ceil((lead + Number(end.slice(8))) / 7);
    const last = addDays(start, weeks * 7 - 1);
    const lands = new Set();
    if (s.due && s.repeat) {
      // Up to the first day shown (however far ahead it's browsed), then the ones shown.
      let d = s.due;
      for (let guard = 0; d && d < start && guard < 20000; guard++) d = step(s.repeat, d);
      for (let guard = 0; d && d <= last && guard < 50; guard++, d = step(s.repeat, d)) lands.add(d);
    }
    let cells = WD.map((w) => `<i>${w}</i>`).join('');
    for (let i = 0; i < weeks * 7; i++) {
      const d = addDays(start, i);
      const cls = ['dp-d'];
      if (d.slice(0, 7) !== month.slice(0, 7)) cls.push('out');
      if (d < c.today) cls.push('past');
      if (d === c.today) cls.push('today');
      if (d === s.due) cls.push('sel');
      else if (lands.has(d)) cls.push('occ');
      cells += `<button class="${cls.join(' ')}" data-date="${d}" title="${esc(dayLabel(d, c.today))}">${Number(d.slice(8))}</button>`;
    }
    $('.dp-g').innerHTML = cells;

    // Time: none, a few usual ones, or any typed.
    const own = s.time && !TIMES.includes(s.time) ? s.time : '';
    $('.dp-time').innerHTML = chip('data-time', '', 'Нет', !s.time)
      + TIMES.map((t) => chip('data-time', t, t, s.time === t)).join('')
      + `<input class="dp-own${own ? ' sel' : ''}" value="${own}" placeholder="чч:мм" maxlength="5" title="Своё время: 15, 9.30, 18:45 — и Enter">`;

    // Recurrence, and for a weekly one its days.
    const kind = s.repeat?.kind ?? '';
    $('.dp-rep').innerHTML = chip('data-rep', '', 'Нет', !kind)
      + REPEAT_KINDS.map((k) => chip('data-rep', k, REPEAT_NAMES[k], kind === k, REPEAT_TITLES[k])).join('');
    const days = kind === 'weekly' ? s.repeat.days ?? [] : [];
    $('.dp-wds-box').hidden = kind !== 'weekly';
    $('.dp-wds').innerHTML = WD.map((w, i) => chip('data-wd', i + 1, w, days.includes(i + 1),
      autoDay ? 'Повторять по этому дню' : days.includes(i + 1) && days.length === 1 ? 'Хотя бы один день остаётся' : '')).join('');
    // The button that had the keyboard was just rebuilt: keep it in the
    // popover, so Esc still closes it.
    if (el.isConnected && !el.contains(document.activeElement)) el.focus({ preventScroll: true });
  }

  /** Takes the new state, saves what changed, shows the month of the date.
   *  A save that fails puts back what the task really has. */
  function set(next) {
    const patch = patchOf(s, next);
    if (!Object.keys(patch).length) return;
    s = next;
    if (s.due && s.due.slice(0, 7) !== month.slice(0, 7)) month = `${s.due.slice(0, 7)}-01`;
    Promise.resolve(on.apply(patch)).catch(() => {
      const t = on.current();
      if (!t || !el.isConnected) return;
      s = { due: t.due ?? null, time: t.time ?? null, repeat: t.repeat ?? null };
      render();
    });
    render();
  }

  el.addEventListener('click', (e) => {
    const b = e.target.closest('button');
    if (!b) return;
    if ('cal' in b.dataset) {
      calOpen = !calOpen;
      render();
    } else if (b.dataset.nav) {
      const f = fromDateStr(month);
      month = toDateStr(new Date(f.getFullYear(), f.getMonth() + Number(b.dataset.nav), 1));
      render();
    } else if ('date' in b.dataset) {
      if (!b.dataset.date) return on.clear();
      if (b.classList.contains('dp-d')) calOpen = false; // picked in the month: fold it back
      set(withDate(s, b.dataset.date, c));
      render(); // folded even when the day was already the date
    } else if ('time' in b.dataset) {
      set(withTime(s, b.dataset.time || null, c));
    } else if ('rep' in b.dataset) {
      if (b.dataset.rep === 'weekly' && s.repeat?.kind !== 'weekly') autoDay = true;
      set(withRepeat(s, b.dataset.rep || null, c));
    } else if (b.dataset.wd) {
      const day = Number(b.dataset.wd);
      set(autoDay ? onlyWeekday(s, day, c) : toggleWeekday(s, day, c));
      autoDay = false;
    }
  });

  el.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      on.close();
      return;
    }
    if (e.key !== 'Enter') return;
    const own = e.target.closest('.dp-own');
    if (own) {
      e.preventDefault();
      const t = parseTime(own.value);
      if (t) set(withTime(s, t, c));
      else own.select();
    } else if (e.target === text) {
      e.preventDefault();
      const p = parseWhen(text.value, s.due);
      if (!p.chips.length) return text.select();
      on.text(p);
    }
  });

  // Typed words: what they'd set, before Enter.
  const hilite = attachHilite(text, (value) => {
    // The spans of «задача [в ]<text>», shifted back onto the text itself
    // (a «в» put in front belongs to the first span).
    const p = parseWhen(value, s.due);
    return p.spans.map(([a, b, k]) => [Math.max(0, a - p.offset), b - p.offset, k]).filter(([, b]) => b > 0);
  });
  text.addEventListener('input', () => {
    hilite();
    const p = text.value.trim() ? parseWhen(text.value, s.due) : null;
    $('.dp-prev').innerHTML = p?.chips.length ? p.chips.map(chipHtml).join('') + '<kbd>↵</kbd>' : '';
  });

  render();
  return el;
}
