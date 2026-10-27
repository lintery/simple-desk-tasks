// Run: npm test  (node 18's --test runner chokes on Cyrillic test names)
import test from 'node:test';
import assert from 'node:assert/strict';
import { parse, trimUrl, URL_RE } from '../ui/lib/nlp.js';
import { clock, isOverdue, nextOccurrence, todayView, upcomingView, overdueLabel } from '../ui/lib/model.js';

// Friday 23 Oct 2026, 09:42
const NOW = new Date(2026, 9, 23, 9, 42);
const p = (s) => parse(s, NOW);

test('relative dates and time', () => {
  assert.deepEqual(
    (({ title, due, time }) => ({ title, due, time }))(p('Позвонить маме завтра в 15:30')),
    { title: 'Позвонить маме', due: '2026-10-24', time: '15:30' },
  );
  assert.equal(p('сдать отчёт послезавтра').due, '2026-10-25');
  assert.equal(p('купить хлеб сегодня').due, '2026-10-23');
  assert.equal(p('продлить ОСАГО через 2 недели').due, '2026-11-06');
  assert.equal(p('врач через 3 дня').due, '2026-10-26');
});

test('weekday needs a preposition; "среда" alone is a word', () => {
  assert.equal(p('встреча в пн').due, '2026-10-26');
  assert.equal(p('отчёт к пятнице').due, '2026-10-30'); // today is Friday → next week
  const r = p('настроить среду разработки');
  assert.equal(r.due, null);
  assert.equal(r.title, 'настроить среду разработки');
});

test('numeric and month-name dates', () => {
  assert.equal(p('день рождения 25.10').due, '2026-10-25');
  assert.equal(p('налоги 01.03').due, '2027-03-01'); // past this year → next year
  assert.equal(p('отпуск 5 ноября').due, '2026-11-05');
  assert.equal(p('к 15.10 сдать').due, '2027-10-15');
  assert.equal(p('к 15.10 сдать').time, null);
});

test('time alone: today if still ahead, otherwise tomorrow', () => {
  assert.deepEqual([p('созвон в 10').due, p('созвон в 10').time], ['2026-10-23', '10:00']);
  assert.deepEqual([p('созвон в 9').due, p('созвон в 9').time], ['2026-10-24', '09:00']);
  assert.equal(p('в 15.30 стендап').time, '15:30');
  assert.equal(p('купить 2 кг яблок').time, null);
});

test('recurrence', () => {
  const daily = p('витамины каждый день в 21:00');
  assert.deepEqual(daily.repeat, { kind: 'daily' });
  assert.equal(daily.due, '2026-10-23');
  assert.equal(daily.title, 'витамины');

  const mon = p('планёрка каждый пн в 10');
  assert.deepEqual(mon.repeat, { kind: 'weekly', days: [1] });
  assert.equal(mon.due, '2026-10-26');

  const two = p('спортзал по пн и чт');
  assert.deepEqual(two.repeat, { kind: 'weekly', days: [1, 4] });
  assert.equal(two.due, '2026-10-26');

  const morning = p('зарядка каждый день в 9'); // 09:00 already passed today
  assert.equal(morning.due, '2026-10-24');

  assert.deepEqual(p('оплатить аренду каждый месяц 1.11').repeat, { kind: 'monthly', day: 1 });

  // A date off the rule's days: the series starts on the first matching day from it.
  const offRule = p('планёрка каждый пн 27.10'); // the 27th is a Tuesday
  assert.equal(offRule.due, '2026-11-02');
  assert.deepEqual(offRule.repeat, { kind: 'weekly', days: [1] });
});

test('numbers that are not times or dates stay in the title', () => {
  const plain = (s) => {
    const r = p(s);
    assert.equal(r.title, s, s);
    assert.equal(r.due, null, s);
    assert.equal(r.time, null, s);
  };
  plain('увеличить шрифт в 2 раза');
  plain('распечатать в 3 экземплярах');
  plain('собрать до 10 человек');
  plain('купить 2.5 кг муки');
  plain('версия 1.2 релиз');
  plain('прочитать главу 3/4');
  plain('настроить переменные в среде разработки');
  assert.doesNotThrow(() => p('проверить через 0 дней'));
});

test('month names and parts of day', () => {
  const gift = p('купить подарок к 8 марта');
  assert.deepEqual([gift.title, gift.due, gift.time], ['купить подарок', '2027-03-08', null]);
  const report = p('сдать отчёт до 15 ноября');
  assert.deepEqual([report.title, report.due, report.time], ['сдать отчёт', '2026-11-15', null]);
  const dinner = p('ужин в 7 вечера');
  assert.deepEqual([dinner.title, dinner.time], ['ужин', '19:00']);
  assert.equal(p('встреча в 3 дня').time, '15:00');
  assert.equal(p('отчёт до пятницы').due, '2026-10-30');
  assert.equal(p('продлить подписку через месяц').due, '2026-11-23');
});

test('bare hour next to a date word, product names, versions', () => {
  const a = p('завтра в 10 созвон');
  assert.deepEqual([a.title, a.due, a.time], ['созвон', '2026-10-24', '10:00']);
  const b = p('в пт в 15 встреча');
  assert.deepEqual([b.title, b.due, b.time], ['встреча', '2026-10-30', '15:00']);
  const c = p('каждый пн в 10 планёрка');
  assert.deepEqual([c.title, c.time, c.repeat], ['планёрка', '10:00', { kind: 'weekly', days: [1] }]);
  const plain = (s) => assert.deepEqual([p(s).title, p(s).due, p(s).time], [s, null, null], s);
  plain('купить кофе 3 в 1');
  plain('установить python 3.12');
  plain('версия 1.10');
  const until = p('отпуск по пятницу');
  assert.equal(until.repeat, null);
  assert.equal(p('встреча в 3.11').due, '2026-11-03');
  assert.equal(p('встреча в 3.11').time, null);
  assert.equal(p('отпуск 29.02').due, '2028-02-29');
  assert.equal(p('созвон , потом обед').title, 'созвон, потом обед');
});

test('round-3 parser cases', () => {
  const plain = (s) => assert.deepEqual([p(s).due === null || !!p(s).due, p(s).time], [true, null], s);
  plain('сходить в 3 магазина завтра');
  plain('забронировать стол до 10 человек в пятницу');
  assert.equal(p('сходить в 3 магазина завтра').title, 'сходить в 3 магазина');
  const d = p('встреча 25.10 в 15');
  assert.deepEqual([d.title, d.due, d.time], ['встреча', '2026-10-25', '15:00']);
  assert.deepEqual([p('встреча 3.11.2026 в 18:30').title, p('встреча 3.11.2026 в 18:30').time], ['встреча', '18:30']);
  const nap = p('поспать на 1.5 часа');
  assert.deepEqual([nap.title, nap.due], ['поспать на 1.5 часа', null]);
  assert.equal(p('созвон до 18.30').time, '18:30');
  assert.equal(p('отчёт на пятницу').due, '2026-10-30');
  const pay = parse('оплата каждый месяц', new Date(2026, 10, 5, 9, 0), '2026-10-31');
  assert.deepEqual(pay.repeat, { kind: 'monthly', day: 31 });
});

test('round-4: afternoon hours, weekday qualifiers, moved recurrences', () => {
  assert.equal(p('к врачу завтра в 3').time, '15:00');
  assert.equal(p('встать завтра в 3 утра').time, '03:00');
  assert.equal(p('созвон завтра в 9').time, '09:00');
  assert.equal(p('встреча в следующую пятницу').due, '2026-10-30'); // today is Fri → next week's
  assert.equal(parse('встреча в следующую пятницу', new Date(2026, 9, 21, 9)).due, '2026-10-30'); // Wed → +9
  assert.equal(p('отчёт в эту пятницу').due, '2026-10-23');
  const bare = p('созвон пт в 10');
  assert.deepEqual([bare.title, bare.due, bare.time], ['созвон', '2026-10-30', '10:00']);
  const c = clock(new Date(2026, 10, 1, 9));
  // yearly 31 Oct moved to 1 Nov and completed → back to 31 Oct next year
  assert.equal(nextOccurrence({ due: '2026-11-01', repeat: { kind: 'yearly', month: 10, day: 31 } }, c), '2027-10-31');
  // monthly on the 28th moved to 4 Nov → 28 Nov, no skipped month
  assert.equal(nextOccurrence({ due: '2026-11-04', repeat: { kind: 'monthly', day: 28 } }, clock(new Date(2026, 10, 4))), '2026-11-28');
  assert.deepEqual(p('ДР мамы 31 октября каждый год').repeat, { kind: 'yearly', month: 10, day: 31 });
});

test('round-5: weekday lists without commas, zero-padded hours', () => {
  const a = p('встреча каждый пн ср в 10');
  assert.deepEqual([a.title, a.due, a.repeat], ['встреча', '2026-10-26', { kind: 'weekly', days: [1, 3] }]);
  const b = p('тренировка по пн ср пт в 19');
  assert.deepEqual([b.title, b.due, b.time, b.repeat], ['тренировка', '2026-10-23', '19:00', { kind: 'weekly', days: [1, 3, 5] }]);
  assert.equal(p('встреча в 05').time, '05:00');
  const fri = p('созвон пятница в 10');
  assert.deepEqual([fri.title, fri.due, fri.time], ['созвон', '2026-10-30', '10:00']);
  assert.equal(p('задача в пятница').due, '2026-10-30');
  assert.equal(p('рабочая среда разработки').due, null);
  const boss = p('созвон пт в 10 у шефа');
  assert.deepEqual([boss.title, boss.due, boss.time], ['созвон у шефа', '2026-10-30', '10:00']);
  assert.equal(p('встреча завтра в 5').time, '17:00');
});

test('links are never parsed as dates or times', () => {
  const a = p('прочитать https://site.ru/news/25.10/в-10 завтра');
  assert.deepEqual([a.title, a.due, a.time], ['прочитать https://site.ru/news/25.10/в-10', '2026-10-24', null]);
  const b = p('www.example.com/2026/01.11 глянуть');
  assert.deepEqual([b.title, b.due], ['www.example.com/2026/01.11 глянуть', null]);
  const call = p('созвон в 10 https://zoom.us/j/123');
  assert.deepEqual([call.title, call.time], ['созвон https://zoom.us/j/123', '10:00']);
  const links = (s) => [...s.matchAll(URL_RE)].map((m) => trimUrl(m[0]));
  assert.deepEqual(links('прочитать https://en.wikipedia.org/wiki/Mercury_(planet) потом'), ['https://en.wikipedia.org/wiki/Mercury_(planet)']);
  assert.deepEqual(links('(см. https://x.ru/a.)'), ['https://x.ru/a']);
  assert.deepEqual(links('[https://site.ru/a]'), ['https://site.ru/a']);
});

test('recurrence with a passed time and anchored re-parse', () => {
  const late = new Date(2026, 9, 23, 14, 0);
  const m = parse('отчёт каждый месяц в 10', late);
  assert.deepEqual([m.due, m.repeat], ['2026-11-23', { kind: 'monthly', day: 23 }]);
  // the date menu parses «задача <typed text>»
  const birthday = parse('задача каждый год', NOW, '2027-03-02');
  assert.deepEqual([birthday.due, birthday.repeat], ['2027-03-02', { kind: 'yearly', month: 3, day: 2 }]);
  const old = parse('задача каждый пн', NOW, '2026-10-12');
  assert.equal(old.due, '2026-10-26');
});

test('hasDate tells a named date from a bare time', () => {
  assert.equal(p('встреча в 18').hasDate, false);
  assert.equal(p('встреча завтра в 18').hasDate, true);
  assert.equal(p('встреча каждый пн').hasDate, true);
});

test('input of only a date keeps the text as the title', () => {
  const r = p('завтра');
  assert.equal(r.title, 'завтра');
  assert.equal(r.due, null);
});

test('overdue and ordering in the Today widget', () => {
  const c = clock(NOW);
  const t = (id, due, time, created = 1) => ({ id, title: id, due, time, created, done: false });
  const tasks = [
    t('untimed', '2026-10-23', null, 1),
    t('late-yesterday', '2026-10-22', null, 2),
    t('at-10', '2026-10-23', '10:00', 3),
    t('late-9', '2026-10-23', '09:00', 4),
    t('tomorrow', '2026-10-24', null, 5),
  ];
  const v = todayView(tasks, c);
  assert.deepEqual(v.active.map((x) => x.id), ['at-10', 'untimed']);
  assert.deepEqual(v.overdue.map((x) => x.id), ['late-yesterday', 'late-9']);
  assert.ok(isOverdue(tasks[3], c));
  assert.equal(overdueLabel(tasks[1], c), 'вчера');
  assert.equal(overdueLabel(tasks[3], c), '09:00');
  assert.deepEqual(upcomingView(tasks, c).map((g) => g.label), ['Завтра']);
});

test('next occurrence never lands in the past', () => {
  const c = clock(NOW);
  assert.equal(nextOccurrence({ due: '2026-10-20', repeat: { kind: 'daily' } }, c), '2026-10-24');
  assert.equal(nextOccurrence({ due: '2026-10-23', repeat: { kind: 'weekly', days: [1, 4] } }, c), '2026-10-26');
  assert.equal(nextOccurrence({ due: '2026-10-23', repeat: { kind: 'weekdays' } }, c), '2026-10-26');
  assert.equal(nextOccurrence({ due: '2026-01-31', repeat: { kind: 'monthly', day: 31 } }, c), '2026-10-31');
});

test('a lone "!" flags the task; "!" inside words or at the end of a sentence does not', () => {
  const r = p('! отчёт завтра в 15');
  assert.deepEqual([r.title, r.important, r.due, r.time], ['отчёт', true, '2026-10-24', '15:00']);
  assert.equal(p('отчёт !!').important, true);
  assert.equal(p('отчёт !!').title, 'отчёт');
  assert.equal(p('Позвонить маме!').important, false);
  assert.equal(p('Позвонить маме!').title, 'Позвонить маме!');
  assert.equal(p('купить !важное').important, false);
  // «!» doesn't make a bare number next to it a time.
  assert.equal(p('кофе ! в 2 раза').time, null);
});

test('spans mark what was understood, in the text as typed', () => {
  const text = 'созвон завтра в 15 !';
  const kinds = p(text).spans.map(([s, e, k]) => [text.slice(s, e), k]);
  assert.deepEqual(kinds, [['завтра', 'date'], ['в 15', 'time'], ['!', 'important']]);
  assert.deepEqual(p('просто текст').spans, []);
});

test('important tasks come first within a day', () => {
  const c = clock(NOW);
  const t = (id, extra) => ({ id, title: id, due: '2026-10-23', done: false, created: id.charCodeAt(0), ...extra });
  const v = todayView([t('a', { time: '10:00' }), t('b'), t('c', { important: true }), t('d', { important: true, time: '11:00' })], c);
  assert.deepEqual(v.active.map((x) => x.id), ['d', 'c', 'a', 'b']);
});

import { parseTime, patchOf, toggleWeekday, withDate, withRepeat, withTime } from '../ui/lib/schedule.js';

test('date picker: «every Monday» in two clicks, the date follows the rule', () => {
  const c = clock(NOW); // Fri 23 Oct 2026, 09:42
  let s = { due: '2026-10-24', time: null, repeat: null };
  s = withRepeat(s, 'weekly', c); // Saturday's weekday
  assert.deepEqual(s.repeat, { kind: 'weekly', days: [6] });
  s = toggleWeekday(s, 1, c); // + Monday
  s = toggleWeekday(s, 6, c); // − Saturday
  assert.deepEqual([s.repeat.days, s.due], [[1], '2026-10-26']);
  assert.deepEqual(toggleWeekday(s, 1, c), s); // the last day stays
  // A date picked: a one-day weekly rule moves its day there; a monthly one its day.
  assert.deepEqual(withDate(s, '2026-10-29', c).repeat.days, [4]);
  const m = withRepeat({ due: '2026-10-31', time: null, repeat: null }, 'monthly', c);
  assert.deepEqual(withDate(m, '2026-11-05', c).repeat, { kind: 'monthly', day: 5 });
  // Weekdays: a Saturday picked lands on Monday.
  assert.equal(withDate(withRepeat(s, 'weekdays', c), '2026-10-31', c).due, '2026-11-02');
});

test('date picker: time and no date', () => {
  const c = clock(NOW);
  assert.deepEqual(withTime({ due: null, time: null, repeat: null }, '15:00', c), { due: '2026-10-23', time: '15:00', repeat: null });
  assert.equal(withTime({ due: null, time: null, repeat: null }, '08:00', c).due, '2026-10-24'); // gone by today
  assert.equal(withDate({ due: '2026-10-25', time: '08:00', repeat: null }, '2026-10-23', c).time, null);
  assert.deepEqual(withDate({ due: '2026-10-25', time: '15:00', repeat: { kind: 'daily' } }, null, c), { due: null, time: null, repeat: null });
  assert.equal(withRepeat({ due: null, time: null, repeat: null }, 'daily', c).due, '2026-10-23');
  assert.deepEqual(['15', '9.30', '0930', '18:45', '25', '7:61', 'abc'].map(parseTime), ['15:00', '09:30', '09:30', '18:45', null, null, null]);
  assert.deepEqual(patchOf({ due: 'a', time: null, repeat: null }, { due: 'a', time: '10:00', repeat: { kind: 'daily' } }),
    { time: '10:00', repeat: { kind: 'daily' } });
});
