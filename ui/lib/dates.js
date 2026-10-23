// Local-date helpers. Dates are 'YYYY-MM-DD' strings, times 'HH:MM'.

export const WEEKDAYS_SHORT = ['вс', 'пн', 'вт', 'ср', 'чт', 'пт', 'сб'];
export const WEEKDAYS_ACC = ['воскресенье', 'понедельник', 'вторник', 'среду', 'четверг', 'пятницу', 'субботу'];
const MONTHS_SHORT = ['янв', 'фев', 'мар', 'апр', 'мая', 'июн', 'июл', 'авг', 'сен', 'окт', 'ноя', 'дек'];

const pad = (n) => String(n).padStart(2, '0');

export function toDateStr(d) {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export function fromDateStr(s) {
  const [y, m, d] = s.split('-').map(Number);
  return new Date(y, m - 1, d);
}

export function toTimeStr(d) {
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function addDays(dateStr, n) {
  const d = fromDateStr(dateStr);
  d.setDate(d.getDate() + n);
  return toDateStr(d);
}

export function addMonths(dateStr, n, dayOfMonth) {
  const d = fromDateStr(dateStr);
  const target = new Date(d.getFullYear(), d.getMonth() + n, 1);
  const last = new Date(target.getFullYear(), target.getMonth() + 1, 0).getDate();
  target.setDate(Math.min(dayOfMonth ?? d.getDate(), last));
  return toDateStr(target);
}

export function daysBetween(a, b) {
  return Math.round((fromDateStr(b) - fromDateStr(a)) / 86400000);
}

/** ISO weekday: Monday = 1 … Sunday = 7. */
export function isoWeekday(dateStr) {
  return fromDateStr(dateStr).getDay() || 7;
}

export function plural(n, one, few, many) {
  const m10 = n % 10, m100 = n % 100;
  if (m10 === 1 && m100 !== 11) return one;
  if (m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14)) return few;
  return many;
}

/** 'Сегодня' / 'Завтра' / 'Пт, 31 окт' (year added when it differs). */
export function dayLabel(dateStr, today) {
  const diff = daysBetween(today, dateStr);
  if (diff === 0) return 'Сегодня';
  if (diff === 1) return 'Завтра';
  if (diff === -1) return 'Вчера';
  const d = fromDateStr(dateStr);
  const wd = WEEKDAYS_SHORT[d.getDay()];
  const base = `${wd[0].toUpperCase()}${wd.slice(1)}, ${d.getDate()} ${MONTHS_SHORT[d.getMonth()]}`;
  return d.getFullYear() === fromDateStr(today).getFullYear() ? base : `${base} ${d.getFullYear()}`;
}
