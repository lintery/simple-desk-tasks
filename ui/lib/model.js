// Which widget a task belongs to, ordering, overdue state and recurrence.

import {
  addDays, addMonths, dayLabel, daysBetween, fromDateStr, isoWeekday, plural,
  toDateStr, toTimeStr, WEEKDAYS_SHORT,
} from './dates.js';

export function clock(now = new Date()) {
  return { today: toDateStr(now), time: toTimeStr(now) };
}

export function isOverdue(t, c) {
  if (t.done || !t.due) return false;
  if (t.due < c.today) return true;
  return t.due === c.today && !!t.time && t.time < c.time;
}

/** 'today' | 'upcoming' | 'nodate' | null (done). */
export function bucketOf(t, c) {
  if (t.done) return null;
  if (!t.due) return 'nodate';
  return t.due <= c.today ? 'today' : 'upcoming';
}

const byCreated = (a, b) => a.created - b.created;
/** Important ones first; then the timed ones by time, the rest as added. */
const byTimeThenCreated = (a, b) => {
  if (!!a.important !== !!b.important) return a.important ? -1 : 1;
  if (a.time && b.time) return a.time.localeCompare(b.time) || byCreated(a, b);
  if (a.time) return -1;
  if (b.time) return 1;
  return byCreated(a, b);
};

/** Today widget: current tasks (timed first), overdue ones at the bottom. */
export function todayView(tasks, c) {
  const mine = tasks.filter((t) => bucketOf(t, c) === 'today');
  const active = mine.filter((t) => !isOverdue(t, c)).sort(byTimeThenCreated);
  const overdue = mine
    .filter((t) => isOverdue(t, c))
    .sort((a, b) => a.due.localeCompare(b.due) || byTimeThenCreated(a, b));
  const doneToday = tasks.filter(
    (t) => t.done && t.doneAt && toDateStr(new Date(t.doneAt)) === c.today,
  ).length;
  return { active, overdue, doneToday };
}

/** Position key in «Без срока»: the dragged-in order, else creation time. */
export const orderKey = (t) => t.order ?? t.created;

export function nodateView(tasks, c) {
  return tasks
    .filter((t) => bucketOf(t, c) === 'nodate')
    .sort((a, b) => (!!b.important - !!a.important) || orderKey(a) - orderKey(b));
}

/** «2/5»: how much of a task's checklist is done, or null without one. */
export function subtaskProgress(t) {
  const list = t.subtasks ?? [];
  return list.length ? { done: list.filter((s) => s.done).length, total: list.length } : null;
}

/** Upcoming widget: [{date, label, tasks}] ordered by date. */
export function upcomingView(tasks, c) {
  const groups = new Map();
  for (const t of tasks.filter((x) => bucketOf(x, c) === 'upcoming')) {
    if (!groups.has(t.due)) groups.set(t.due, []);
    groups.get(t.due).push(t);
  }
  return [...groups.keys()].sort().map((date) => ({
    date,
    label: dayLabel(date, c.today),
    tasks: groups.get(date).sort(byTimeThenCreated),
  }));
}

/** 'вчера' / '3 дня' / '14:00' — how overdue a task is. */
export function overdueLabel(t, c) {
  if (t.due === c.today) return t.time;
  const days = daysBetween(t.due, c.today);
  if (days === 1) return 'вчера';
  return `${days} ${plural(days, 'день', 'дня', 'дней')}`;
}

// ---- recurrence ----------------------------------------------------------
// repeat = {kind:'daily'} | {kind:'weekdays'} | {kind:'weekly', days:[1..7]}
//        | {kind:'monthly', day} | {kind:'yearly', month, day}

/** The occurrence after `date` (exclusive). */
export function step(rule, date) {
  switch (rule.kind) {
    case 'daily':
      return addDays(date, 1);
    case 'weekdays': {
      let d = addDays(date, 1);
      while (isoWeekday(d) > 5) d = addDays(d, 1);
      return d;
    }
    case 'weekly': {
      const days = rule.days?.length ? rule.days : [isoWeekday(date)];
      let d = addDays(date, 1);
      while (!days.includes(isoWeekday(d))) d = addDays(d, 1);
      return d;
    }
    case 'monthly': {
      // The rule's day in this month if still ahead, else next month (clamped to
      // short months), so a task moved past its date doesn't skip a month.
      const d = fromDateStr(date);
      const day = rule.day ?? d.getDate();
      const here = addMonths(toDateStr(new Date(d.getFullYear(), d.getMonth(), 1)), 0, day);
      return here > date ? here : addMonths(here, 1, day);
    }
    case 'yearly': {
      // Fixed month and day (29 Feb comes back in leap years), wherever the task was moved.
      const d = fromDateStr(date);
      const month = (rule.month ?? d.getMonth() + 1) - 1;
      const day = rule.day ?? d.getDate();
      const here = addMonths(toDateStr(new Date(d.getFullYear(), month, 1)), 0, day);
      return here > date ? here : addMonths(here, 12, day);
    }
    default:
      return null;
  }
}

/** Next due date after completing a recurring task (never in the past). */
export function nextOccurrence(t, c) {
  if (!t.repeat || !t.due) return null;
  let d = step(t.repeat, t.due);
  for (let guard = 0; d && d <= c.today && guard < 4000; guard++) d = step(t.repeat, d);
  return d;
}

/** First date matching a rule, starting from `from` inclusive. */
export function firstOccurrence(rule, from) {
  if ((rule.kind === 'monthly' || rule.kind === 'yearly') && rule.day) return step(rule, addDays(from, -1));
  const fits = (d) =>
    rule.kind === 'weekdays' ? isoWeekday(d) <= 5
      : rule.kind === 'weekly' && rule.days?.length ? rule.days.includes(isoWeekday(d))
        : true;
  let d = from;
  for (let i = 0; i < 7 && !fits(d); i++) d = addDays(d, 1);
  return d;
}

export function repeatLabel(rule) {
  if (!rule) return '';
  switch (rule.kind) {
    case 'daily': return 'каждый день';
    case 'weekdays': return 'по будням';
    case 'weekly':
      return rule.days?.length
        ? `каждый ${rule.days.map((d) => WEEKDAYS_SHORT[d % 7]).join(', ')}`
        : 'каждую неделю';
    case 'monthly': return 'каждый месяц';
    case 'yearly': return 'каждый год';
    default: return '';
  }
}
