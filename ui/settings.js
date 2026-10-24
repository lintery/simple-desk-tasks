// «Настройки»: calendar account, focus durations, backup folder, autostart,
// updates. Every change is saved at once (settings.rs) and reaches the widgets.

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
const ID_MAIL = 'https://id.mail.ru/security';

let settings = null;
let saveTimer = 0;
let calendar = null;
let update = null;
/** A connect is being checked: the button stays «Проверяю…». */
let connecting = false;

const hm = (ms) => new Date(ms).toLocaleTimeString('ru', { hour: '2-digit', minute: '2-digit' });

/** Saves soon; while a save waits, the page's own values win over echoes.
 * The reply is what was saved (settings.rs keeps the calendar account its own). */
function save(delay = 250) {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = 0;
    api.setSettings(settings).then(applySettings).catch(() => api.settings().then(applySettings));
  }, delay);
}

function applySettings(s) {
  if (!s || saveTimer) return;
  settings = s;
  render();
}

// ---------- calendar ----------

function renderCalendar() {
  const connected = !!settings.calendarEmail;
  const email = $('email');
  email.readOnly = connected;
  if (connected) email.value = settings.calendarEmail;
  $('email-box').classList.toggle('ro', connected);
  const pass = $('pass');
  pass.readOnly = connected;
  if (connected) {
    pass.value = '';
    pass.placeholder = 'сохранён в Windows';
  } else {
    pass.placeholder = '';
  }
  $('pass-box').classList.toggle('ro', connected);
  show($('peek'), !connected);
  const btn = $('connect');
  if (!connecting) {
    btn.textContent = connected ? 'Отключить' : 'Подключить';
    btn.className = connected ? 'btn sub' : 'btn pri';
  }
  $('cal-list').hidden = !connected || !settings.calendars.length;
  $('cal-count').textContent = settings.calendars.length ? `найдено ${settings.calendars.length}` : '';
  // Rebuilt on every calendar event and every 30 s: only when it changed, or
  // a click landing on the rebuild is lost.
  const cals = settings.calendars.map((c, i) =>
    `<button class="ck${c.enabled ? ' on' : ''}" data-cal="${i}" style="--c:${esc(c.color || 'var(--ac)')}">` +
    `<span class="box"><svg class="i"><use href="#i-check"/></svg></span><span class="sw-dot"></span><span>${esc(c.name)}</span></button>`,
  ).join('');
  if ($('cals').dataset.html !== cals) {
    $('cals').dataset.html = cals;
    $('cals').innerHTML = cals;
  }
  const st = $('cal-st');
  st.className = 'st';
  st.textContent = '';
  st.title = '';
  if (connected && calendar) {
    if (calendar.error) {
      st.classList.add('bad');
      st.textContent = 'нет доступа';
      st.title = calendar.error;
    } else {
      st.classList.add('ok');
      const ago = calendar.updated ? Math.round((Date.now() - calendar.updated) / 60_000) : null;
      st.textContent = ago == null ? 'подключён' : `подключён · обновлено ${ago < 1 ? 'только что' : `${ago} мин назад`}`;
      st.title = '';
    }
  }
}

function calError(text) {
  $('cal-err').hidden = !text;
  $('cal-err').textContent = text || '';
}

$('connect').addEventListener('click', async () => {
  const btn = $('connect');
  calError('');
  if (settings.calendarEmail) {
    btn.disabled = true;
    try {
      await api.calendarDisconnect();
      $('email').value = '';
    } catch (e) {
      calError(String(e));
    }
    btn.disabled = false;
    return;
  }
  const email = $('email').value.trim();
  const pass = $('pass').value;
  if (!email.includes('@') || !pass.trim()) {
    calError('Нужны почта и пароль для внешних приложений');
    return;
  }
  btn.disabled = true;
  btn.textContent = 'Проверяю…';
  connecting = true;
  try {
    await api.calendarConnect(email, pass);
    $('pass').value = '';
    settings = await api.settings();
  } catch (e) {
    calError(String(e));
  }
  connecting = false;
  btn.disabled = false;
  render();
});
$('peek').addEventListener('click', () => {
  const pass = $('pass');
  pass.type = pass.type === 'password' ? 'text' : 'password';
});
$('open-id').addEventListener('click', () => api.openUrl(ID_MAIL).catch(() => {}));
$('cals').addEventListener('click', (e) => {
  const b = e.target.closest('.ck');
  if (!b) return;
  const c = settings.calendars[+b.dataset.cal];
  c.enabled = !c.enabled;
  render();
  save(0);
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
  $('modes').innerHTML = head.join('') + rows.join('');
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
$('autostart').addEventListener('click', () => {
  settings.autostart = !settings.autostart;
  render();
  save(0);
});

// ---------- updates ----------

function renderUpdate() {
  const u = update ?? { state: 'idle' };
  $('version').textContent = u.current ?? '';
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
  renderCalendar();
  renderModes();
  toggle('backup-on', settings.backupEnabled);
  $('backup-dir').textContent = settings.backupDir;
  $('backup-dir').title = settings.backupDir;
  toggle('autostart', settings.autostart);
  toggle('auto-update', settings.autoUpdate);
  renderUpdate();
}

function goTo(section) {
  if (section) document.getElementById(`sec-${section}`)?.scrollIntoView({ block: 'start' });
  if (section === 'calendar' && !settings?.calendarEmail) $('email').focus();
}

listenAll('settings-changed', applySettings);
listenAll('update-status', (u) => {
  update = u;
  renderUpdate();
});
listenAll('calendar', (c) => {
  calendar = c;
  if (settings) renderCalendar();
});
listenMine('settings-section', goTo);
Promise.all([api.settings(), api.updateStatus(), api.calendar()]).then(([s, u, c]) => {
  settings = s;
  update = u;
  calendar = c;
  render();
  api.takeSettingsSection().then(goTo).catch(() => {});
});
setInterval(() => settings && renderCalendar(), 30_000);
