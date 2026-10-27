// «Настройки»: calendar accounts, focus durations, backup folder, general,
// updates — one section at a time. Every change is saved at once
// (settings.rs) and reaches the widgets.

import './lib/theme.js';
import { api, listenAll, listenMine } from './lib/api.js';
import { esc } from './lib/icons.js';

const $ = (id) => document.getElementById(id);
// settings.css makes [hidden] win over any display rule.
const show = (el, on) => {
  el.hidden = !on;
};
const MODES = [['pomodoro', 'Помодоро', true], ['focus50', 'Фокус', false], ['deep90', 'Глубокий', false]];
/** Step, min and max of each duration. */
const STEPS = { focus: [5, 5, 240], short: [1, 1, 60], long: [5, 5, 90], every: [1, 1, 12] };
const SECTIONS = ['calendar', 'focus', 'backup', 'general', 'updates'];
/** A calendar's reminder: minutes before its meetings (settings.rs REMIND_CHOICES). */
const REMIND = [[0, 'без напоминания'], [1, 'за 1 мин'], [5, 'за 5 мин'], [10, 'за 10 мин'], [15, 'за 15 мин'], [30, 'за 30 мин']];

/** Calendars one can connect, and how to get what each needs. */
const PROVIDERS = [
  {
    id: 'google', name: 'Google', sub: 'по закрытой ссылке iCal', color: '#4285f4', letter: 'G', kind: 'link',
    steps: [
      'Откройте <a data-url="https://calendar.google.com/calendar/r/settings">настройки Google Календаря</a>',
      'Слева выберите свой календарь, затем <b>Интеграция календаря</b>',
      'Скопируйте <b>Закрытый адрес в формате iCal</b> и вставьте ниже',
    ],
    note: 'Google обновляет такую ссылку раз в несколько часов',
  },
  {
    id: 'mailru', name: 'Mail.ru', sub: 'почта и пароль приложения', color: '#005ff9', letter: '@', kind: 'caldav',
    login: ['Почта', 'name@mail.ru'],
    steps: [
      'Откройте <a data-url="https://id.mail.ru/security">id.mail.ru → Безопасность</a>',
      'В <b>Паролях для внешних приложений</b> создайте пароль с доступом к календарю',
      'Вставьте почту и этот пароль ниже',
    ],
  },
  {
    id: 'yandex', name: 'Яндекс', sub: 'логин и пароль приложения', color: '#fc3f1d', letter: 'Я', kind: 'caldav',
    login: ['Логин или почта', 'name@yandex.ru'],
    steps: [
      'Откройте <a data-url="https://id.yandex.ru/security/app-passwords">id.yandex.ru → Пароли приложений</a>',
      'Создайте пароль для <b>Календаря (CalDAV)</b>',
      'Вставьте логин и этот пароль ниже',
    ],
  },
  {
    id: 'icloud', name: 'iCloud', sub: 'Apple ID и пароль приложения', color: '#6e7787', letter: '', kind: 'caldav',
    login: ['Apple ID', 'name@icloud.com'],
    steps: [
      'Откройте <a data-url="https://account.apple.com/account/manage">account.apple.com</a>, раздел <b>Вход и безопасность</b>',
      'В <b>Паролях приложений</b> создайте пароль',
      'Вставьте Apple ID и этот пароль ниже',
    ],
  },
  {
    id: 'outlook', name: 'Outlook', sub: 'по опубликованной ссылке', color: '#0f6cbd', letter: 'O', kind: 'link',
    steps: [
      'Откройте <a data-url="https://outlook.live.com/calendar/0/options/calendar/SharedCalendars">Outlook → Параметры → Общие календари</a>',
      'В <b>Опубликовать календарь</b> выберите календарь и «Может просматривать все сведения»',
      'Скопируйте ссылку <b>ICS</b> и вставьте ниже',
    ],
  },
  {
    id: 'caldav', name: 'Свой CalDAV', sub: 'Nextcloud, Synology и другие', color: '#3aa58a', letter: '⌂', kind: 'caldav', server: true,
    login: ['Логин', ''],
    steps: [
      'Адрес CalDAV есть в настройках вашего сервера, например <b>https://cloud.example.com/remote.php/dav</b>',
      'Вместо основного пароля лучше создать пароль приложения',
    ],
  },
  {
    id: 'ics', name: 'Ссылка .ics', sub: 'любой календарь по ссылке', color: '#9c6ade', letter: '⇣', kind: 'link',
    steps: ['Вставьте ссылку на календарь: <b>https://…/calendar.ics</b> или <b>webcal://…</b>'],
  },
];
const providerOf = (id) => PROVIDERS.find((p) => p.id === id) ?? PROVIDERS[PROVIDERS.length - 1];

let settings = null;
let saveTimer = 0;
let calendar = null;
let update = null;
/** The form being filled: { provider, account? (reconnecting) }, or null. */
let adding = null;
/** Account whose «Отключить» was pressed once (a second press confirms). */
let confirming = null;
let confirmTimer = 0;

const hm = (ms) => new Date(ms).toLocaleTimeString('ru', { hour: '2-digit', minute: '2-digit' });
const ago = (ms) => {
  const min = Math.round((Date.now() - ms) / 60_000);
  return min < 1 ? 'только что' : min < 60 ? `${min} мин назад` : `в ${hm(ms)}`;
};

/** Sets markup only when it changes: a node replaced under a pressed mouse
 * (the 30 s redraw, calendar events) swallows the click. */
function setHTML(el, html) {
  if (el.dataset.html === html) return;
  el.dataset.html = html;
  el.innerHTML = html;
}

/** A short note at the top (what didn't work), gone after a while. */
let toastTimer = 0;
function toast(text) {
  $('toast').textContent = text;
  show($('toast'), true);
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => show($('toast'), false), 6000);
}

/** Saves soon; while a save waits, the page's own values win over echoes.
 * The reply is what was saved (settings.rs keeps the calendar accounts its own).
 * A failed save says why and shows what's really stored. */
function save(delay = 250) {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = 0;
    api.setSettings(settings).then(applySettings).catch((e) => {
      toast(String(e));
      api.settings().then(applySettings).catch(() => {});
    });
  }, delay);
}

function applySettings(s) {
  if (!s || saveTimer) return;
  settings = s;
  render();
}

// ---------- sections ----------

function goTo(section) {
  const sec = SECTIONS.includes(section) ? section : 'calendar';
  for (const s of SECTIONS) show($(`sec-${s}`), s === sec);
  for (const b of $('nav').querySelectorAll('button')) b.classList.toggle('sel', b.dataset.sec === sec);
  try { localStorage.setItem('settings.section', sec); } catch {}
  $('nav').dataset.sec = sec;
  // Asked to open at the calendar with nothing connected: straight to the choice.
  if (section === 'calendar' && settings && !settings.calendarAccounts.length && !adding) openPicker();
}

$('nav').addEventListener('click', (e) => {
  const b = e.target.closest('button[data-sec]');
  if (b) goTo(b.dataset.sec);
});

// ---------- calendars ----------

const accountState = (id) => calendar?.accounts?.find((a) => a.id === id);

function logo(p) {
  return `<span class="logo" style="--lc:${p.color}">${p.letter ? esc(p.letter) : '<svg class="i" style="width:15px;height:15px"><use href="#i-cal"/></svg>'}</span>`;
}

function renderAccounts() {
  // A reminder list being picked from would close under a rebuild: later, when it's left.
  if (document.activeElement?.matches?.('#accounts select')) {
    accountsLater = true;
    return;
  }
  accountsLater = false;
  const accounts = settings.calendarAccounts;
  const html = accounts.map((a, i) => {
    const p = providerOf(a.provider);
    const st = accountState(a.id);
    const status = st?.error
      ? `<span class="bad">${esc(st.error)}</span>`
      : calendar?.syncing && !st?.updated
        ? '<span>обновляю…</span>'
        : st?.updated
          ? `<span class="ok">обновлено ${ago(st.updated)}</span>`
          : '<span>подключаю…</span>';
    const confirm = confirming === a.id;
    // Each calendar: on/off (when there's a choice) and when its meetings are reminded of.
    const cals = `<div class="hr"></div><div class="cals">${a.calendars.map((c, j) => {
      const name = `<span class="sw-dot"></span><span>${esc(c.name)}</span>`;
      const ck = a.calendars.length > 1
        ? `<button class="ck${c.enabled ? ' on' : ''}" data-acc="${i}" data-cal="${j}" style="--c:${esc(c.color || 'var(--ac)')}">` +
          `<span class="box"><svg class="i"><use href="#i-check"/></svg></span>${name}</button>`
        : `<span class="ck on one" style="--c:${esc(c.color || 'var(--ac)')}">${name}</span>`;
      const remind = REMIND.map(([m, label]) => `<option value="${m}"${(c.remind ?? 5) === m ? ' selected' : ''}>${label}</option>`).join('');
      return `<div class="cal">${ck}<label class="rem${c.enabled ? '' : ' off'}" title="Напоминание о встречах">` +
        `<svg class="i"><use href="#i-bell"/></svg><select data-acc="${i}" data-cal="${j}"${c.enabled ? '' : ' disabled'}>${remind}</select></label></div>`;
    }).join('')}</div>`;
    const title = a.label && a.label !== p.name ? `${p.name} · ${esc(a.label)}` : p.name;
    return `<div class="card acc"><div class="acc-h">${logo(p)}<div class="acc-t"><b title="${esc(a.label)}">${title}</b>${status}</div>` +
      '<div class="acc-a">' +
      `<button class="btn ghost" data-redo="${i}">${p.kind === 'link' ? 'Сменить ссылку' : 'Сменить пароль'}</button>` +
      `<button class="btn ${confirm ? 'sub danger' : 'ghost'}" data-drop="${i}">${confirm ? 'Точно отключить?' : 'Отключить'}</button>` +
      `</div></div>${cals}</div>`;
  }).join('');
  setHTML($('accounts'), accounts.length || adding ? html
    : '<div class="card empty"><b>Календари не подключены</b>Подключите Google, Mail.ru, Яндекс, iCloud, Outlook или любой календарь по ссылке.</div>');
  show($('add-cal'), !adding);
  const trouble = accounts.some((a) => accountState(a.id)?.error);
  $('nav-cal').className = trouble ? 'bad' : '';
  $('nav-cal').textContent = trouble || !accounts.length ? '' : String(accounts.length);
}

function openPicker() {
  adding = { picking: true };
  setHTML($('providers'), PROVIDERS.map((p) =>
    `<button class="prov" data-prov="${p.id}">${logo(p)}<span><b>${p.name}</b><span>${p.sub}</span></span></button>`).join(''));
  show($('picker'), true);
  show($('form'), false);
  renderAccounts();
}

function openForm(providerId, account = null) {
  const p = providerOf(providerId);
  adding = { provider: p.id, account: account?.id ?? null };
  $('form-title').textContent = account ? `${p.name}: ${p.kind === 'link' ? 'новая ссылка' : 'новый пароль'}` : `Подключить ${p.name}`;
  $('form-steps').innerHTML = p.steps.map((s) => `<li>${s}</li>`).join('');
  const field = (id, label, icon, type, value, ph) =>
    `<div class="fld"><label for="${id}">${label}</label><div class="inp"><svg class="i"><use href="#${icon}"/></svg>` +
    `<input id="${id}" type="${type}" value="${esc(value)}" placeholder="${esc(ph)}" autocomplete="off" spellcheck="false">` +
    (type === 'password' ? `<button class="ib" data-peek="${id}" title="Показать"><svg class="i"><use href="#i-eye"/></svg></button>` : '') +
    '</div></div>';
  const fields = [];
  if (p.kind === 'link') {
    fields.push(field('f-secret', 'Ссылка на календарь', 'i-link', 'password', '', 'https://… или webcal://…'));
  } else {
    if (p.server) fields.push(field('f-server', 'Адрес сервера CalDAV', 'i-server', 'url', account?.server ?? '', 'https://'));
    fields.push(field('f-login', p.login[0], 'i-mail', 'email', account?.label ?? '', p.login[1]));
    fields.push(field('f-secret', 'Пароль приложения', 'i-key', 'password', '', ''));
  }
  $('form-fields').innerHTML = fields.join('');
  $('form-note').textContent = p.note ?? 'Пароль и ссылка хранятся в Диспетчере учётных данных Windows';
  formError('');
  // Another form's connect still running: this one waits for it.
  $('form-go').textContent = connecting ? 'Проверяю…' : account ? 'Сохранить' : 'Подключить';
  $('form-go').disabled = !!connecting;
  show($('picker'), false);
  show($('form'), true);
  renderAccounts();
  ($('f-server')?.value === '' ? $('f-server') : !$('f-login') || $('f-login').value ? $('f-secret') : $('f-login'))?.focus();
}

function closeAdding() {
  adding = null;
  show($('picker'), false);
  show($('form'), false);
  renderAccounts();
}

function formError(text) {
  show($('form-err'), !!text);
  $('form-err').textContent = text || '';
}

/** The form whose connect is under way: one at a time, and its answer only
 * touches the form if that's still the one on screen. */
let connecting = null;

async function submitForm() {
  if (!adding?.provider || connecting) return;
  const form = adding;
  const value = (id) => $(id)?.value.trim() ?? '';
  const request = {
    provider: form.provider,
    login: value('f-login'),
    secret: value('f-secret'),
    server: value('f-server'),
    account: form.account,
  };
  const go = $('form-go');
  connecting = form;
  go.disabled = true;
  go.textContent = 'Проверяю…';
  formError('');
  try {
    await api.calendarConnect(request);
    settings = await api.settings().catch(() => settings);
    if (adding === form) closeAdding();
    render();
  } catch (e) {
    if (adding === form) formError(String(e));
  } finally {
    connecting = null;
    if (adding?.provider) {
      go.disabled = false;
      go.textContent = adding.account ? 'Сохранить' : 'Подключить';
    }
  }
}

$('add-cal').addEventListener('click', openPicker);
$('picker-back').addEventListener('click', closeAdding);
$('form-back').addEventListener('click', () => (adding?.account ? closeAdding() : openPicker()));
$('providers').addEventListener('click', (e) => {
  const b = e.target.closest('[data-prov]');
  if (b) openForm(b.dataset.prov);
});
$('form-go').addEventListener('click', submitForm);
$('form').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && e.target instanceof HTMLInputElement) submitForm();
  if (e.key === 'Escape') closeAdding();
});
$('form').addEventListener('click', (e) => {
  const link = e.target.closest('a[data-url]');
  if (link) api.openUrl(link.dataset.url).catch((err) => toast(String(err)));
  const peek = e.target.closest('[data-peek]');
  if (peek) {
    const input = $(peek.dataset.peek);
    input.type = input.type === 'password' ? 'text' : 'password';
  }
});
/** A rebuild of the accounts held back while a reminder list was open. */
let accountsLater = false;
$('accounts').addEventListener('change', (e) => {
  const sel = e.target.closest('select[data-cal]');
  if (!sel) return;
  settings.calendarAccounts[+sel.dataset.acc].calendars[+sel.dataset.cal].remind = +sel.value;
  save(0);
});
$('accounts').addEventListener('focusout', (e) => {
  if (!e.target.matches('select')) return;
  setTimeout(() => accountsLater && settings && renderAccounts());
});
$('accounts').addEventListener('click', async (e) => {
  const ck = e.target.closest('button.ck');
  if (ck) {
    const c = settings.calendarAccounts[+ck.dataset.acc].calendars[+ck.dataset.cal];
    c.enabled = !c.enabled;
    renderAccounts();
    save(0);
    return;
  }
  const redo = e.target.closest('[data-redo]');
  if (redo) {
    const a = settings.calendarAccounts[+redo.dataset.redo];
    openForm(a.provider, a);
    return;
  }
  const drop = e.target.closest('[data-drop]');
  if (drop) {
    const a = settings.calendarAccounts[+drop.dataset.drop];
    if (confirming !== a.id) {
      confirming = a.id;
      clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => {
        confirming = null;
        renderAccounts();
      }, 4000);
      renderAccounts();
      return;
    }
    confirming = null;
    try {
      await api.calendarDisconnect(a.id);
      settings = await api.settings();
      render();
    } catch (err) {
      toast(String(err));
    }
  }
});

// ---------- focus ----------

function renderModes() {
  const head = ['', 'Фокус', 'Перерыв', 'Длинный', 'Раундов'].map((t) => `<div class="th">${t}</div>`);
  const stepper = (mode, field) =>
    `<div><span class="stp"><button data-mode="${mode}" data-field="${field}" data-d="-1" title="Меньше"><svg class="i"><use href="#i-minus"/></svg></button>` +
    `<span>${settings.modes[mode][field]}</span>` +
    `<button data-mode="${mode}" data-field="${field}" data-d="1" title="Больше"><svg class="i"><use href="#i-plus"/></svg></button></span></div>`;
  const rows = MODES.map(([mode, title, long]) =>
    `<div class="md"><b>${title}</b></div>${stepper(mode, 'focus')}${stepper(mode, 'short')}` +
    (long ? `${stepper(mode, 'long')}${stepper(mode, 'every')}` : '<div class="na">—</div><div class="na">—</div>'),
  );
  setHTML($('modes'), head.join('') + rows.join(''));
}

$('modes').addEventListener('click', (e) => {
  const b = e.target.closest('button[data-field]');
  if (!b) return;
  const [step, min, max] = STEPS[b.dataset.field];
  const m = settings.modes[b.dataset.mode];
  m[b.dataset.field] = Math.min(max, Math.max(min, m[b.dataset.field] + step * +b.dataset.d));
  renderModes();
  save();
});

// ---------- backup, general ----------

function toggle(id, on) {
  $(id).classList.toggle('on', !!on);
  $(id).setAttribute('aria-pressed', on ? 'true' : 'false');
}

$('backup-on').addEventListener('click', () => {
  settings.backupEnabled = !settings.backupEnabled;
  render();
  save(0);
});
$('pick').addEventListener('click', async () => {
  const dir = await api.pickFolder(settings.backupDir).catch(() => null);
  if (!dir) return;
  settings.backupDir = dir;
  render();
  save(0);
});
$('theme').addEventListener('click', (e) => {
  const b = e.target.closest('button[data-theme]');
  if (!b || settings.theme === b.dataset.theme) return;
  settings.theme = b.dataset.theme;
  render();
  save(0);
});
$('autostart').addEventListener('click', () => {
  settings.autostart = !settings.autostart;
  render();
  save(0);
});

// ---------- updates ----------

function renderUpdate() {
  const u = update ?? { state: 'idle' };
  $('version').textContent = u.current ?? '';
  $('side-ver').textContent = u.current ? `версия ${u.current}` : '';
  const tag = $('upd-tag');
  show(tag, u.state === 'uptodate' || u.state === 'ready');
  tag.className = u.state === 'ready' ? 'tag new' : 'tag';
  tag.innerHTML = u.state === 'ready' ? `доступна ${esc(u.latest ?? '')}` : '<svg class="i"><use href="#i-check"/></svg>последняя';
  const note = $('upd-note');
  note.className = u.state === 'error' ? 'note bad' : 'note';
  note.textContent = {
    checking: 'проверяю…',
    uptodate: u.checkedAt ? `проверено в ${hm(u.checkedAt)}` : '',
    ready: 'скачано и проверено, поставится, когда вы не заняты в приложении',
    error: u.message ?? 'не удалось проверить',
    unsupported: u.message ?? 'обновляется только установленная копия',
  }[u.state] ?? '';
  $('check').disabled = u.state === 'checking' || u.state === 'unsupported';
  $('check').classList.toggle('busy', u.state === 'checking');
  show($('install'), u.state === 'ready');
  $('nav-upd').className = u.state === 'ready' ? 'new' : '';
  $('nav-upd').textContent = u.state === 'ready' ? 'новая' : '';
}

$('auto-update').addEventListener('click', () => {
  settings.autoUpdate = !settings.autoUpdate;
  render();
  save(0);
});
$('check').addEventListener('click', async () => {
  update = { ...(update ?? {}), state: 'checking' };
  renderUpdate();
  update = await api.updateCheck().catch((e) => ({ ...(update ?? {}), state: 'error', message: String(e) }));
  renderUpdate();
});
$('install').addEventListener('click', () => {
  $('install').disabled = true;
  api.updateInstall().catch((e) => {
    update = { ...update, state: 'error', message: String(e) };
    $('install').disabled = false;
    renderUpdate();
  });
});

// ---------- all ----------

function render() {
  if (!settings) return;
  renderAccounts();
  renderModes();
  toggle('backup-on', settings.backupEnabled);
  $('backup-dir').textContent = settings.backupDir;
  $('backup-dir').title = settings.backupDir;
  toggle('autostart', settings.autostart);
  for (const b of $('theme').querySelectorAll('button')) {
    const on = b.dataset.theme === (settings.theme ?? 'system');
    b.classList.toggle('sel', on);
    b.setAttribute('aria-checked', on ? 'true' : 'false');
  }
  toggle('auto-update', settings.autoUpdate);
  renderUpdate();
}

listenAll('settings-changed', applySettings);
listenAll('update-status', (u) => {
  update = u;
  renderUpdate();
});
listenAll('calendar', (c) => {
  calendar = c;
  if (settings) renderAccounts();
});
listenMine('settings-section', goTo);
api.hotkey().then((k) => {
  $('hotkey').textContent = k || 'не назначена';
}).catch(() => {});
let first = 'calendar';
try { first = localStorage.getItem('settings.section') || first; } catch {}
goTo(first);
Promise.all([api.settings(), api.updateStatus(), api.calendar()]).then(([s, u, c]) => {
  settings = s;
  update = u;
  calendar = c;
  render();
  api.takeSettingsSection().then((section) => section && goTo(section)).catch(() => {});
});
setInterval(() => settings && renderAccounts(), 30_000);
