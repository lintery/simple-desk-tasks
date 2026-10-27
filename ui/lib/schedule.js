// A task's when — date, time, recurrence — changed one choice at a time (the
// date picker). Each step returns the whole new {due, time, repeat}; the
// rules stay consistent with the date (a weekly rule's days, a monthly one's day).

import { addDays, isoWeekday } from './dates.js';
import { firstOccurrence } from './model.js';

/** The kinds of recurrence the picker offers, in its order. */
export const REPEAT_KINDS = ['daily', 'weekdays', 'weekly', 'monthly', 'yearly'];

/** A rule of `kind` that fits the date `due`. */
function ruleFor(kind, due) {
  switch (kind) {
    case 'daily': return { kind: 'daily' };
    case 'weekdays': return { kind: 'weekdays' };
    case 'weekly': return { kind: 'weekly', days: [isoWeekday(due)] };
    case 'monthly': return { kind: 'monthly', day: Number(due.slice(8)) };
    case 'yearly': return { kind: 'yearly', month: Number(due.slice(5, 7)), day: Number(due.slice(8)) };
    default: return null;
  }
}

/** A date picked (null: no date — then no time and no repeat either). The
 *  rule follows: its weekday or day of month becomes the new date's, or —
 *  with several weekdays — the date moves to the first of them. Today with
 *  a time already gone by loses the time (it would be overdue at once). */
export function withDate(s, date, c) {
  if (!date) return { due: null, time: null, repeat: null };
  const r = s.repeat;
  let next;
  if (!r || r.kind === 'daily') next = { ...s, due: date };
  else if (r.kind === 'weekdays' || (r.kind === 'weekly' && (r.days?.length ?? 0) > 1)) next = { ...s, due: firstOccurrence(r, date) };
  else next = { ...s, due: date, repeat: ruleFor(r.kind, date) };
  if (next.due === c.today && next.time && next.time <= c.time) next.time = null;
  return next;
}

/** A time picked (null: none). Without a date it's today, or tomorrow when
 *  that time has already gone by. */
export function withTime(s, time, c) {
  if (!time) return { ...s, time: null };
  const due = s.due ?? (time > c.time ? c.today : addDays(c.today, 1));
  return { ...s, due, time };
}

/** A recurrence picked (null: none). It starts from the task's date (or
 *  today), on the first day that fits it. */
export function withRepeat(s, kind, c) {
  if (!kind) return { ...s, repeat: null };
  const base = s.due ?? c.today;
  if (s.repeat?.kind === kind) return s;
  const repeat = ruleFor(kind, base);
  return { ...s, due: firstOccurrence(repeat, base), repeat };
}

/** A weekday (1 = Monday … 7) switched on or off in a weekly rule; the last
 *  one can't go. The date moves to the first of the days from today (or
 *  from the date, when that's later). */
export function toggleWeekday(s, day, c) {
  const days = new Set(s.repeat?.kind === 'weekly' ? s.repeat.days ?? [] : []);
  if (days.has(day)) days.delete(day);
  else days.add(day);
  if (!days.size) return s;
  const repeat = { kind: 'weekly', days: [...days].sort((a, b) => a - b) };
  const from = s.due && s.due > c.today ? s.due : c.today;
  return { ...s, due: firstOccurrence(repeat, from), repeat };
}

/** Weekly on just this weekday (the first day picked after «каждую неделю»
 *  replaces the one taken from the date). */
export function onlyWeekday(s, day, c) {
  const repeat = { kind: 'weekly', days: [day] };
  // That weekday in the week up to the date (not the week after it), never before today.
  const weekBefore = s.due ? addDays(s.due, -6) : c.today;
  return { ...s, due: firstOccurrence(repeat, weekBefore > c.today ? weekBefore : c.today), repeat };
}

/** «15», «9.30», «0930», «18:05» → «HH:MM»; anything else null. */
export function parseTime(text) {
  const m = String(text).trim().match(/^(\d{1,2})(?:[:.\s]?(\d{2}))?$/);
  if (!m) return null;
  const h = Number(m[1]), min = Number(m[2] ?? 0);
  if (h > 23 || min > 59) return null;
  return `${String(h).padStart(2, '0')}:${String(min).padStart(2, '0')}`;
}

/** What changed from `a` to `b`, as an update patch (due, time, repeat). */
export function patchOf(a, b) {
  const patch = {};
  for (const key of ['due', 'time', 'repeat']) {
    if (JSON.stringify(a[key] ?? null) !== JSON.stringify(b[key] ?? null)) patch[key] = b[key] ?? null;
  }
  return patch;
}
