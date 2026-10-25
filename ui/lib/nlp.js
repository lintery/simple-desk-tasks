// Russian quick-add parser: «позвонить маме завтра в 15:30», «планёрка каждый пн в 10».
// Returns the cleaned title plus due/time/repeat and chips describing what was understood.
// It errs on the side of leaving words alone: «купить 2.5 кг», «в 2 раза», «в среде
// разработки», «python 3.12», «кофе 3 в 1» stay plain text.

import { addDays, dayLabel, addMonths, isoWeekday, toDateStr, toTimeStr } from './dates.js';
import { firstOccurrence, repeatLabel, step } from './model.js';

const B = '(?<![\\p{L}\\d])';
const E = '(?![\\p{L}\\d])';
const rx = (src) => new RegExp(B + src + E, 'giu');
const alt = (words) => [...words].sort((a, b) => b.length - a.length).join('|');

// Weekday spellings by grammatical case; index 0 = Monday.
const WD_SHORT = ['пн', 'вт', 'ср', 'чт', 'пт', 'сб', 'вс'];
const WD_ACC = ['понедельник', 'вторник', 'среду', 'четверг', 'пятницу', 'субботу', 'воскресенье']; // в/каждый
const WD_DAT = ['понедельнику', 'вторнику', 'среде', 'четвергу', 'пятнице', 'субботе', 'воскресенью']; // к
const WD_GEN = ['понедельника', 'вторника', 'среды', 'четверга', 'пятницы', 'субботы', 'воскресенья']; // до
const WD_PLU = ['понедельникам', 'вторникам', 'средам', 'четвергам', 'пятницам', 'субботам', 'воскресеньям']; // по
const WD_NOM = ['понедельник', 'вторник', 'среда', 'четверг', 'пятница', 'суббота', 'воскресенье']; // «пятница в 10»
const wdNumber = (word) => {
  const w = word.toLowerCase();
  for (const forms of [WD_SHORT, WD_ACC, WD_NOM, WD_DAT, WD_GEN, WD_PLU]) {
    const i = forms.indexOf(w);
    if (i >= 0) return i + 1;
  }
  return w === 'пон' ? 1 : 0;
};
// Nominative is included: «в пятница» is never a normal word, and a bare
// «среда» only counts right next to a time (see the bare-weekday rule).
const WD_IN = alt([...WD_SHORT, ...WD_ACC, ...WD_NOM]);
const WD_TO = alt([...WD_SHORT, ...WD_DAT]);
const WD_UNTIL = alt([...WD_SHORT, ...WD_GEN]);
const WD_EVERY = alt([...WD_SHORT, 'пон', ...WD_ACC]); // каждый пн / каждую среду
const WD_ON = alt([...WD_SHORT, 'пон', ...WD_PLU]); // по пн / по средам («по пятницу» = until)

const MONTHS = ['янв', 'фев', 'мар', 'апр', 'ма', 'июн', 'июл', 'авг', 'сен', 'окт', 'ноя', 'дек'];
const MONTH_ALT = 'январ[ья]|янв|феврал[ья]|фев|марта?|мар|апрел[ья]|апр|ма[йя]|июн[ья]|июн|июл[ья]|июл|августа?|авг|сентябр[ья]|сент|сен|октябр[ья]|окт|ноябр[ья]|ноя|декабр[ья]|дек';
const monthNumber = (word) => {
  const w = word.toLowerCase();
  if (w.startsWith('ма')) return w.startsWith('мар') ? 3 : 5;
  return MONTHS.findIndex((m) => m !== 'ма' && w.startsWith(m)) + 1;
};

/** Stands in for link characters while parsing (private-use, never typed). */
const LINK_FILL = '';

/** http(s):// and www. links (shared with the widget, which makes them clickable). */
export const URL_RE = /(?:https?:\/\/|www\.)[^\s<>"'«»]+[^\s<>"'«».,;:!?\]}]/giu;

/** Drops a closing bracket that belongs to the sentence, not the link:
 *  «(см. https://x.ru/a)» → …/a, but …/Mercury_(planet) stays whole. */
export function trimUrl(u) {
  const count = (s, ch) => s.split(ch).length - 1;
  for (let prev = ''; prev !== u;) {
    prev = u;
    if (u.endsWith(')') && count(u, ')') > count(u, '(')) u = u.slice(0, -1);
    u = u.replace(/[.,;:!?]+$/u, ''); // «(см. https://x.ru/a.)» → …/a
  }
  return u;
}

const NUM_WORDS = { один: 1, одну: 1, два: 2, две: 2, три: 3, четыре: 4, пять: 5, шесть: 6, семь: 7, десять: 10 };
const PREP = '(?:на|до|к|в|во|с|по)';

// What may follow a bare hour («в 11») or a bare «25.10»: end of phrase, punctuation
// or another date/time word. «в 2 раза», «до 10 человек» are not times.
const AFTER = /^\s*(?:$|[,.;!?)]|(?:сегодня|завтра|послезавтра|каждый|каждую|каждое|ежедневно|по|в|во|к|до|через|утром|днём|днем|вечером)(?![\p{L}\d]))/iu;
// «2.5 кг», «1.5 л» are quantities, not dates.
const UNIT_AFTER = /^\s*(?:кг|г|гр|л|мл|%|см|мм|м|км|шт|руб|р|₽|раз|раза|ч|мин)(?![\p{L}\d])/iu;
// «python 3.12», «версия 1.10», «глава 3.11» are numbers, not dates.
const NOT_DATE_BEFORE = /(?:[a-z][\w+#-]*|верси[яиюей]|пункт[а-я]*|п\.|глав[аеуыой]*|раздел[а-я]*|стр\.?|страниц[аеуыой]*|№|#|рис\.?|табл\.?)\s*$/iu;

/** Bare times/dates are accepted when they sit right next to another recognised part
 *  (optionally only of the given kind). Spans are [start, end, kind]. */
const nextToSpan = (spans, text, start, end, kind = null) =>
  spans.some(([s, e, k]) => (!kind || k === kind)
    && ((e <= start && !text.slice(e, start).trim()) || (s >= end && !text.slice(end, s).trim())));
const WD_SEP = '(?:\\s*(?:,|и)\\s*|\\s+)';
// A bare weekday right before/after a bare hour («созвон пт в 10 у шефа») makes it a time.
const WD_BEFORE = new RegExp(`(?:^|[^\\p{L}\\d])(?:${WD_IN})\\s*$`, 'iu');
const WD_AFTER = new RegExp(`^\\s*(?:${WD_IN})(?![\\p{L}\\d])`, 'iu');

const RULES = [
  // ---- repeat ----
  { kind: 'repeat', re: rx('(?:каждый\\s+день|ежедневно)'), make: () => ({ kind: 'daily' }) },
  { kind: 'repeat', re: rx('(?:по\\s+будням|каждый\\s+будний\\s+день)'), make: () => ({ kind: 'weekdays' }) },
  {
    kind: 'repeat',
    re: rx(`(?:(?:каждый|каждую|каждое)\\s+((?:${WD_EVERY})(?:${WD_SEP}(?:${WD_EVERY}))*)|по\\s+((?:${WD_ON})(?:${WD_SEP}(?:${WD_ON}))*))`),
    make: (m) => ({
      kind: 'weekly',
      days: [...new Set((m[1] ?? m[2]).split(/[\s,]+(?:и\s+)?/u).map((w) => wdNumber(w.trim())).filter(Boolean))].sort(),
    }),
  },
  { kind: 'repeat', re: rx('(?:каждую\\s+неделю|еженедельно)'), make: () => ({ kind: 'weekly' }) },
  { kind: 'repeat', re: rx('(?:каждый\\s+месяц|ежемесячно)'), make: () => ({ kind: 'monthly' }) },
  { kind: 'repeat', re: rx('(?:каждый\\s+год|ежегодно)'), make: () => ({ kind: 'yearly' }) },

  // ---- dates with a month name go first: «к 8 марта» is a date, not 08:00 ----
  {
    kind: 'date',
    re: rx(`(?:${PREP}\\s+)?(\\d{1,2})\\s+(${MONTH_ALT})\\.?(?:\\s+(\\d{4}))?`),
    make: (m, c) => ymd(c, Number(m[1]), monthNumber(m[2]), m[3]),
  },

  // ---- time ----
  {
    // «в 15.30», «до 18.30» are times; «в 3.11» / «к 10.12» (second part ≤ 12) are dates, see below.
    kind: 'time',
    re: rx('(?:в|к|до|с)\\s+(\\d{1,2})\\.(\\d{2})'),
    ok: (m) => Number(m[2]) > 12 || m[2] === '00',
    make: (m) => hm(m[1], m[2]),
  },
  {
    kind: 'time',
    // Not after a lone number («кофе 3 в 1»), but fine after a date («25.10 в 15»).
    re: rx('(?<!(?<![\\d./])\\d+\\s*)(?:в|к|до|с)\\s+(\\d{1,2})(?::(\\d{2}))?(?![./]\\d)(\\s*(?:ч|час(?:а|ов)?)(?![\\p{L}\\d]))?(?:\\s+(утра|дня|вечера|ночи))?'),
    ok: (m, text, end, spans) =>
      !!(m[2] || m[3] || m[4]) || AFTER.test(text.slice(end)) || nextToSpan(spans, text, m.index, end)
      || WD_BEFORE.test(text.slice(0, m.index)) || WD_AFTER.test(text.slice(end)),
    make: (m) => {
      let h = Number(m[1]);
      const part = m[4]?.toLowerCase();
      if ((part === 'дня' || part === 'вечера') && h < 12) h += 12;
      if (part === 'ночи' && h === 12) h = 0;
      // «к врачу в 3», «созвон в 5» mean afternoon; mornings are said «в 3 утра», «в 05», «в 03:00».
      if (!part && !m[2] && !m[1].startsWith('0') && h >= 1 && h <= 7) h += 12;
      return hm(h, m[2]);
    },
  },
  { kind: 'time', re: rx('(\\d{1,2}):(\\d{2})'), make: (m) => hm(m[1], m[2]) },
  { kind: 'time', re: rx('утром'), make: () => '09:00' },
  { kind: 'time', re: rx('(?:днём|днем)'), make: () => '13:00' },
  { kind: 'time', re: rx('вечером'), make: () => '19:00' },

  // ---- other dates ----
  { kind: 'date', re: rx(`(?:${PREP}\\s+)?послезавтра`), make: (_, c) => addDays(c.today, 2) },
  { kind: 'date', re: rx(`(?:${PREP}\\s+)?завтра`), make: (_, c) => addDays(c.today, 1) },
  { kind: 'date', re: rx(`(?:${PREP}\\s+)?сегодня`), make: (_, c) => c.today },
  {
    kind: 'date',
    re: rx('через\\s+(?:(\\d{1,3}|один|одну|два|две|три|четыре|пять|шесть|семь|десять)\\s+)?(день|дня|дней|неделю|недели|недель|месяц|месяца|месяцев)'),
    make: (m, c) => {
      const n = m[1] ? Number(m[1]) || NUM_WORDS[m[1].toLowerCase()] : 1;
      if (!n) return null;
      const unit = m[2].toLowerCase();
      if (unit.startsWith('д')) return addDays(c.today, n);
      if (unit.startsWith('н')) return addDays(c.today, 7 * n);
      return addMonths(c.today, n);
    },
  },
  {
    // «25.10», «в 3.11», «1.11.2026». Without a preposition or year it must start or
    // end the phrase (or sit next to another date/time word) and not follow «версия»…
    kind: 'date',
    re: rx(`(?:(${PREP})\\s+)?(\\d{1,2})[./](\\d{1,2})(?:[./](\\d{4}|\\d{2}))?`),
    ok: (m, text, end, spans) => {
      if (UNIT_AFTER.test(text.slice(end))) return false;
      if (m[4]) return true;
      if (m[3].length !== 2) return false; // «на 1.5 часа», «до 7.5 процентов»
      if (m[1]) return true;
      if (NOT_DATE_BEFORE.test(text.slice(0, m.index))) return false;
      return !text.slice(0, m.index).trim() || AFTER.test(text.slice(end)) || nextToSpan(spans, text, m.index, end);
    },
    make: (m, c) => ymd(c, Number(m[2]), Number(m[3]), m[4]),
  },
  {
    kind: 'date',
    re: rx(`(?:в|во|на)\\s+(?:(эт(?:от|у|о)|следующ(?:ий|ую|ее))\\s+)?(${WD_IN})`),
    make: (m, c) => nextWeekday(c, m[2], m[1]),
  },
  { kind: 'date', re: rx(`к\\s+(${WD_TO})`), make: (m, c) => nextWeekday(c, m[1]) },
  { kind: 'date', re: rx(`до\\s+(${WD_UNTIL})`), make: (m, c) => nextWeekday(c, m[1]) },
  // «созвон пт в 10»: a weekday without a preposition only right next to a time.
  {
    kind: 'date',
    re: rx(`(${WD_IN})`),
    ok: (m, text, end, spans) => nextToSpan(spans, text, m.index, end, 'time'),
    make: (m, c) => nextWeekday(c, m[1]),
  },
];

function hm(h, m = '00') {
  const hh = Number(h), mm = Number(m);
  if (hh > 23 || mm > 59) return null;
  return `${String(hh).padStart(2, '0')}:${String(mm).padStart(2, '0')}`;
}

/**
 * Next such weekday, 1–7 days ahead («в пт» said on Friday means next Friday).
 * «в эту пт» allows today; «в следующую пт» means next week's when this week's is still ahead.
 */
function nextWeekday(c, word, qualifier = '') {
  const want = wdNumber(word);
  if (!want) return null;
  const wd = isoWeekday(c.today);
  const q = qualifier.toLowerCase();
  if (q.startsWith('эт')) return addDays(c.today, (want - wd + 7) % 7);
  const ahead = ((want - wd + 7) % 7) || 7;
  return addDays(c.today, q.startsWith('след') && want > wd ? ahead + 7 : ahead);
}

/** Date from day/month(/year); without a year it's the next such date (29 Feb included). */
function ymd(c, day, month, yearStr) {
  if (!month || month > 12 || day < 1 || day > 31) return null;
  const make = (y) => {
    const d = new Date(y, month - 1, day);
    return d.getMonth() === month - 1 ? toDateStr(d) : null;
  };
  if (yearStr) return make(Number(yearStr.length === 2 ? `20${yearStr}` : yearStr));
  for (let y = c.now.getFullYear(); y <= c.now.getFullYear() + 4; y++) {
    const s = make(y);
    if (s && s >= c.today) return s;
  }
  return null;
}

/** Fills in what a recurrence still lacks from its first date (never overrides). */
function anchorRule(repeat, due) {
  if (repeat.kind === 'weekly' && !repeat.days?.length) return { kind: 'weekly', days: [isoWeekday(due)] };
  if (repeat.kind === 'monthly') return { kind: 'monthly', day: repeat.day ?? Number(due.slice(8)) };
  if (repeat.kind === 'yearly') {
    return { kind: 'yearly', month: repeat.month ?? Number(due.slice(5, 7)), day: repeat.day ?? Number(due.slice(8)) };
  }
  return repeat;
}

/**
 * @param anchor the task's current date when re-parsing an existing task: a bare
 *   recurrence («каждый год») then keeps that date instead of starting today.
 * @returns {{title, due, time, repeat, chips, hasDate}} `hasDate` is true when the
 *   text named a date or a recurrence (not just a time).
 */
export function parse(text, now = new Date(), anchor = null) {
  const c = { now, today: toDateStr(now), time: toTimeStr(now) };
  const found = {};
  const spans = [];
  // Links are never dates («…/2026/25.10»): they're blanked out (same length)
  // for matching; the title is still cut from the original text. The filler is
  // neither letter nor digit, so a link ends a phrase like punctuation does
  // («созвон в 10 https://zoom.us/…» → 10:00).
  const scan = text.replace(URL_RE, (u) => LINK_FILL.repeat(u.length));
  // Two passes: a bare hour or «25.10» may be accepted only because a date word
  // found later sits right next to it («завтра в 10 созвон»).
  for (let pass = 0; pass < 2; pass++) {
    for (const rule of RULES) {
      if (found[rule.kind] !== undefined) continue;
      for (const m of scan.matchAll(rule.re)) {
        const start = m.index, end = m.index + m[0].length;
        if (spans.some(([s, e]) => start < e && end > s)) continue;
        if (rule.ok && !rule.ok(m, scan, end, spans)) continue;
        const value = rule.make(m, c);
        if (value == null) continue;
        found[rule.kind] = value;
        spans.push([start, end, rule.kind]);
        break;
      }
    }
  }

  let title = text;
  for (const [s, e] of spans.sort((a, b) => b[0] - a[0])) title = title.slice(0, s) + ' ' + title.slice(e);
  title = title.replace(/\s+/g, ' ').replace(/\s+([,.;:!?])/g, '$1').replace(/^[\s,.;:—-]+|[\s,;:—-]+$/gu, '').trim();
  if (!title) return { title: text.trim(), due: null, time: null, repeat: null, chips: [], hasDate: false };

  let { date: due = null, time = null, repeat = null } = found;
  const explicitDate = due !== null;
  if (repeat && !due) {
    if (anchor) {
      repeat = anchorRule(repeat, anchor);
      due = firstOccurrence(repeat, anchor);
      for (let i = 0; due < c.today && i < 4000; i++) due = step(repeat, due);
    } else {
      due = firstOccurrence(repeat, c.today);
    }
  }
  if (time && !due) due = c.today;
  // «каждый пн 27.10» with the 27th a Tuesday: the series starts on the first
  // Monday from that date, not on an off-rule day.
  if (repeat && explicitDate && (repeat.kind === 'weekdays' || (repeat.kind === 'weekly' && repeat.days?.length))) {
    due = firstOccurrence(repeat, due);
  }
  if (repeat) repeat = anchorRule(repeat, due);
  if (due === c.today && time && time <= c.time && !explicitDate) {
    // The time has passed today: the next occurrence (or tomorrow).
    due = repeat ? step(repeat, due) : addDays(c.today, 1);
  }

  const chips = [];
  if (due) chips.push({ kind: 'date', label: dayLabel(due, c.today) });
  if (time) chips.push({ kind: 'time', label: time });
  if (repeat) chips.push({ kind: 'repeat', label: repeatLabel(repeat) });
  return { title, due, time, repeat, chips, hasDate: explicitDate || !!repeat };
}
