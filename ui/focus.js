// «Фокус»: pomodoro / 50-10 / 90-20 timer with ambient sounds. The timer runs
// in focus.rs (phases end, chime and notify even while this page is throttled);
// this page draws it and plays the sounds.

import { api, listenAll, listenMine } from './lib/api.js';
import { initBackdrop } from './lib/backdrop.js';
import { SOUNDS, createAudio } from './lib/sounds.js';
import { buildWave } from './lib/wave.js';

const $ = (id) => document.getElementById(id);
const w = $('w');
const RING = 2 * Math.PI * 74;
const MIN = 60_000;
/** Mode lengths from the settings (minutes): focus, short and long break,
 * focus sessions per long break. Until they load: the defaults. */
let settings = null;
let modes = {
  pomodoro: { focus: 25, short: 5, long: 15, every: 4 },
  focus50: { focus: 50, short: 10, long: 10, every: 0 },
  deep90: { focus: 90, short: 20, long: 20, every: 0 },
};
const lengths = (mode) => modes[mode] ?? modes.pomodoro;
const PHASE = { idle: 'Готов к фокусу', focus: 'Фокус', break: 'Перерыв', long: 'Длинный перерыв' };

const store = {
  get: (k) => {
    try {
      return localStorage.getItem(`focus.${k}`);
    } catch {
      return null;
    }
  },
  set: (k, v) => {
    try {
      localStorage.setItem(`focus.${k}`, v);
    } catch {}
  },
};

const audio = createAudio();
let timer = null;
/** A sound turned on by hand outside a running focus session; ends with the phase. */
let manual = false;
let muted = store.get('muted') === '1';
let prefsTimer = 0;

const pad = (n) => String(n).padStart(2, '0');
const mmss = (ms) => {
  const s = Math.ceil(ms / 1000);
  return `${pad(Math.floor(s / 60))}:${pad(s % 60)}`;
};
const clock = (ts) => new Date(ts).toLocaleTimeString('ru', { hour: '2-digit', minute: '2-digit' });
const minutes = (ms) => {
  const m = Math.floor(ms / MIN);
  const h = Math.floor(m / 60);
  return h ? `${h} ч ${pad(m % 60)} мин` : `${m} мин`;
};
const localDay = () => new Date().toLocaleDateString('sv');

const left = (t) => (t.running ? Math.max(0, Math.min(t.total, t.endsAt - Date.now())) : t.remaining);
/** Finished rounds; a new day starts the cycle over (focus.rs does the same on the next change). */
const round = (t) => (t.day === localDay() ? t.round : 0);
const isBreak = (t) => t.phase === 'break' || t.phase === 'long';
const focusing = (t) => t.phase === 'focus' && t.running;

/** Swaps a button's sprite icon, only when it really changes (rebuilding the
 * svg under a pressed mouse would swallow the click). */
function setIcon(button, name) {
  if (button.dataset.icon === name) return;
  button.dataset.icon = name;
  button.innerHTML = `<svg class="i"><use href="#i-${name}"/></svg>`;
}

/** Sets markup only when it changes: render() runs every 250 ms, and a node
 * replaced under a pressed mouse swallows the click. */
function setHTML(el, html) {
  if (el.dataset.html === html) return;
  el.dataset.html = html;
  el.innerHTML = html;
}

/** What comes after the current phase, e.g. «перерыв 5 мин». */
function nextLabel(t) {
  const { focus, short, long, every } = lengths(t.mode);
  if (isBreak(t)) return `фокус ${focus} мин`;
  const isLong = every > 0 && round(t) + 1 >= every;
  return isLong ? `длинный перерыв ${long} мин` : `перерыв ${short} мин`;
}

function render() {
  if (!timer) return;
  const t = timer;
  const rest = left(t);
  const idle = t.phase === 'idle';
  const paused = !idle && !t.running;
  w.classList.toggle('is-idle', idle);
  w.classList.toggle('is-break', isBreak(t));
  w.classList.toggle('paused', paused);

  $('phase').textContent = paused ? 'На паузе' : PHASE[t.phase];
  $('time').textContent = mmss(rest);
  const done = idle ? 0 : 1 - rest / t.total;
  $('arc').setAttribute('stroke-dasharray', `${(done * RING).toFixed(1)} ${RING.toFixed(1)}`);
  $('head').style.transform = `rotate(${done * 360}deg)`;

  const { every } = lengths(t.mode);
  $('rounds').style.display = every ? '' : 'none';
  $('sub').style.display = every ? 'none' : '';
  if (every) {
    const finished = t.phase === 'long' ? every : round(t);
    setHTML($('rounds'), Array.from({ length: every }, (_, i) => {
      const cls = i < finished ? 'on' : i === finished && t.phase === 'focus' ? 'cur' : '';
      return `<i class="${cls}"></i>`;
    }).join(''));
  } else {
    $('sub').textContent = t.running ? `до ${clock(t.endsAt)}` : idle ? `до ${clock(Date.now() + t.remaining)}` : '';
  }

  const today = (t.day === localDay() ? t.today : 0) + (t.phase === 'focus' ? t.total - rest : 0);
  setHTML($('today'), `сегодня <em>${minutes(today)}</em>`);

  for (const b of $('modes').children) {
    b.classList.toggle('sel', b.dataset.mode === t.mode);
    const m = lengths(b.dataset.mode);
    b.querySelector('span').textContent = `${m.focus} / ${m.short}`;
  }
  $('modes').classList.toggle('locked', !idle);
  $('dial').title = idle ? 'Колёсико — длительность фокуса' : '';

  setIcon($('go'), t.running ? 'pause' : 'play');
  $('go').title = t.running ? 'Пауза' : idle ? 'Начать' : 'Продолжить';
  $('skip').disabled = idle;
  $('reset').disabled = idle && round(t) === 0;
  const when = idle ? 'после' : t.running ? `в ${clock(t.endsAt)}` : 'дальше';
  setHTML($('next'), `<small>${when}</small>${nextLabel(t)}`);
  renderStats();
}

// ---------- stats ----------

const WEEKDAYS = ['вс', 'пн', 'вт', 'ср', 'чт', 'пт', 'сб'];
const dayKey = (d) => d.toLocaleDateString('sv');
const daysAgo = (n) => {
  const d = new Date();
  d.setDate(d.getDate() - n);
  return d;
};
const short = (ms) => {
  const m = Math.floor(ms / MIN);
  return m >= 60 ? `${Math.floor(m / 60)} ч ${pad(m % 60)}` : `${m} мин`;
};
const daysWord = (n) => {
  const n10 = n % 10, n100 = n % 100;
  if (n10 === 1 && n100 !== 11) return 'день';
  return n10 >= 2 && n10 <= 4 && (n100 < 12 || n100 > 14) ? 'дня' : 'дней';
};

/** Focus time on a local day; today includes the session under way. */
function focusOn(t, key) {
  if (key !== localDay()) return t.history?.[key] ?? 0;
  return (t.day === key ? t.today : 0) + (t.phase === 'focus' ? t.total - left(t) : 0);
}

function renderStats() {
  if (!timer || !w.classList.contains('show-stats')) return;
  const days = Array.from({ length: 7 }, (_, i) => daysAgo(6 - i));
  const vals = days.map((d) => focusOn(timer, dayKey(d)));
  const max = Math.max(...vals, 30 * MIN);
  const bars = days.map((d, i) => {
    const v = vals[i];
    const cls = `bar${v >= MIN ? ' has' : ''}${i === 6 ? ' today' : ''}`;
    return `<div class="${cls}"><b>${v >= MIN ? short(v) : ''}</b>` +
      `<i style="height:${Math.max(3, Math.round((v / max) * 84))}px"></i>` +
      `<span>${i === 6 ? 'сегодня' : WEEKDAYS[d.getDay()]}</span></div>`;
  });
  // Days in a row with focus, ending today (or yesterday, if today hasn't started).
  let streak = 0;
  for (let n = focusOn(timer, localDay()) >= MIN ? 0 : 1; n < 400; n++) {
    if (focusOn(timer, dayKey(daysAgo(n))) < MIN) break;
    streak++;
  }
  const best = Math.max(vals[6], ...Object.values(timer.history ?? {}));
  const week = vals.reduce((a, b) => a + b, 0);
  setHTML($('stats'), `<div class="bars">${bars.join('')}</div><div class="sums">` +
    `<div><small>За неделю</small><b>${minutes(week)}</b></div>` +
    `<div><small>Серия</small><b>${streak} ${daysWord(streak)}</b></div>` +
    `<div><small>Лучший день</small><b>${minutes(best)}</b></div></div>`);
}

$('today').addEventListener('click', () => {
  w.classList.toggle('show-stats');
  renderStats();
});

// ---------- sounds ----------

const soundTitle = (id) => SOUNDS.find((s) => s.id === id)?.title ?? '';
const groupOf = (id) => SOUNDS.find((s) => s.id === id)?.group;
const GROUPS = [['rain', 'Дождь'], ['nature', 'Природа'], ['noise', 'Шум']].filter(([g]) =>
  SOUNDS.some((s) => s.group === g));
/** The group whose tiles are shown. */
let group = null;

function renderTiles() {
  $('groups').innerHTML = GROUPS.map(([g, title]) => `<button class="grp" data-group="${g}">${title}</button>`).join('');
  $('tiles').innerHTML = SOUNDS.filter((s) => s.group === group).map(
    (s) => `<button class="tile" data-id="${s.id}"><svg class="ti"><use href="#s-${s.id}"/></svg><span>${s.title}</span>` +
      '<div class="eq"><i></i><i></i><i></i></div></button>',
  ).join('');
}

function showGroup(g) {
  if (g === group || !GROUPS.some(([id]) => id === g)) return;
  group = g;
  renderTiles();
  renderSound();
}

/** Sounds that may play together. */
const MAX_MIX = 2;
const known = (id) => SOUNDS.some((s) => s.id === id);
const mixItem = (id) => timer?.mix.find((m) => m.id === id);
const titles = (ids) => ids.map(soundTitle).join(' + ');

/** The sounds that should be audible right now. */
function wanted() {
  if (!timer?.mix.length || muted) return [];
  return focusing(timer) || manual ? timer.mix : [];
}

function renderSound() {
  const playing = audio.mix();
  for (const tile of $('tiles').children) {
    const id = tile.dataset.id;
    const item = mixItem(id);
    tile.classList.toggle('on', playing.includes(id));
    tile.classList.toggle('sel', !playing.includes(id) && !!item);
    tile.style.setProperty('--lvl', item ? item.level : 0);
    tile.title = item ? 'Колёсико — громкость этого звука' : '';
  }
  const liveGroups = new Set(playing.map(groupOf));
  for (const b of $('groups').children) {
    b.classList.toggle('sel', b.dataset.group === group);
    b.classList.toggle('live', b.dataset.group !== group && liveGroups.has(b.dataset.group));
  }
  $('wave').classList.toggle('still', !playing.length);
  const now = $('now');
  const chosen = timer?.mix.map((m) => m.id) ?? [];
  if (playing.length) now.innerHTML = `<em>${titles(playing)}</em>`;
  else if (!chosen.length || muted) now.textContent = 'выключены';
  else now.innerHTML = `<em>${titles(chosen)}</em> · ${timer.phase === 'idle' ? 'со стартом' : 'в фокусе'}`;
  const silent = muted || !chosen.length;
  $('snd').classList.toggle('muted', silent);
  $('mute').classList.toggle('off', muted);
  setIcon($('mute'), muted ? 'spk-off' : 'spk');
  $('mute').title = muted ? 'Включить звук' : 'Выключить звук';
  const volume = timer?.volume ?? 0.6;
  $('level').style.width = `${volume * 100}%`;
  $('slider').classList.toggle('mute', muted);
  $('vol').textContent = muted ? '—' : String(Math.round(volume * 100));
}

function syncAudio() {
  // Also glides the levels of sounds that keep playing.
  audio.setMix(wanted().map((m) => ({ id: m.id, level: m.level })));
  renderSound();
}

/** Remembers the mix and volume in focus.rs (levels and volume once dragging settles). */
function savePrefs(delay = 0) {
  clearTimeout(prefsTimer);
  const mix = timer.mix.map((m) => ({ ...m }));
  const { volume } = timer;
  prefsTimer = setTimeout(() => {
    prefsTimer = 0;
    api.focusPrefs(mix, volume).catch(() => {});
  }, delay);
}

/** A tile click: a playing sound leaves the mix; any other joins it (the
 * oldest goes when the mix is full) and is heard right away. */
function pickSound(id) {
  if (!timer) return;
  if (mixItem(id) && audio.mix().includes(id)) {
    timer.mix = timer.mix.filter((m) => m.id !== id);
    if (!timer.mix.length) manual = false;
  } else {
    if (!mixItem(id)) timer.mix = [...timer.mix, { id, level: 1 }].slice(-MAX_MIX);
    muted = false;
    store.set('muted', '0');
    if (!focusing(timer)) manual = true;
  }
  savePrefs();
  syncAudio();
}

/** The wheel over a sound in the mix sets its own level. */
function nudgeLevel(id, up) {
  const item = mixItem(id);
  if (!item) return false;
  item.level = Math.round(Math.min(1, Math.max(0.1, item.level + (up ? 0.1 : -0.1))) * 10) / 10;
  savePrefs(400);
  syncAudio();
  return true;
}

function setVolume(v) {
  if (!timer) return;
  timer.volume = Math.round(Math.min(1, Math.max(0, v)) * 100) / 100;
  audio.setVolume(timer.volume);
  savePrefs(400);
  renderSound();
}

$('groups').addEventListener('click', (e) => {
  const b = e.target.closest('.grp');
  if (b) showGroup(b.dataset.group);
});
$('tiles').addEventListener('click', (e) => {
  const tile = e.target.closest('.tile');
  if (tile) pickSound(tile.dataset.id);
});
$('tiles').addEventListener('wheel', (e) => {
  const tile = e.target.closest('.tile');
  if (tile && nudgeLevel(tile.dataset.id, e.deltaY < 0)) e.preventDefault();
}, { passive: false });
$('mute').addEventListener('click', () => {
  muted = !muted;
  store.set('muted', muted ? '1' : '0');
  syncAudio();
});
const slider = $('slider');
const fromPointer = (e) => {
  const r = slider.getBoundingClientRect();
  setVolume((e.clientX - r.left) / r.width);
};
slider.addEventListener('pointerdown', (e) => {
  if (e.button !== 0) return;
  slider.setPointerCapture(e.pointerId);
  fromPointer(e);
});
slider.addEventListener('pointermove', (e) => {
  if (slider.hasPointerCapture(e.pointerId)) fromPointer(e);
});
slider.addEventListener('wheel', (e) => {
  e.preventDefault();
  setVolume((timer?.volume ?? 0.6) + (e.deltaY < 0 ? 0.05 : -0.05));
}, { passive: false });

// ---------- timer ----------

function apply(next) {
  // Events and command replies travel separately; an older state may come last.
  if (!next || (timer && next.rev < timer.rev)) return;
  next.mix = (next.mix ?? []).filter((m) => known(m.id));
  // A new phase, a pause or a start ends any hand-started sound.
  if (timer && (timer.phase !== next.phase || timer.running !== next.running)) manual = false;
  // The page's own unsaved mix/volume choice wins over an older echo.
  if (timer && prefsTimer) {
    next.mix = timer.mix;
    next.volume = timer.volume;
  }
  // So does a length being set with the wheel (until its save comes back).
  if (timer && wheelBusy() && next.phase === 'idle' && timer.phase === 'idle' && next.mode === timer.mode) {
    next.total = timer.total;
    next.remaining = timer.remaining;
  }
  const first = !timer;
  timer = next;
  // Open on the group of the chosen sound.
  if (first && groupOf(timer.mix[0]?.id)) showGroup(groupOf(timer.mix[0].id));
  audio.setVolume(timer.volume);
  render();
  syncAudio();
}

async function act(action) {
  try {
    apply(await api.focusAction(action));
  } catch {}
}

$('go').addEventListener('click', async () => {
  if (!timer) return;
  if (!timer.running) audio.chime('start');
  const action = timer.running ? 'pause' : 'start';
  // A length just set with the wheel must reach focus.rs before the start.
  if (action === 'start' && wheelBusy()) await (wheelSave ? saveWheel() : wheelSaved);
  act(action);
});
$('skip').addEventListener('click', () => act('skip'));
$('reset').addEventListener('click', () => act('reset'));
$('modes').addEventListener('click', async (e) => {
  const b = e.target.closest('.mode');
  if (!b || !timer || timer.phase !== 'idle' || b.dataset.mode === timer.mode) return;
  try {
    apply(await api.focusMode(b.dataset.mode));
  } catch {}
});

const setMovable = (on) => {
  if (on) w.setAttribute('data-tauri-drag-region', 'deep');
  else w.removeAttribute('data-tauri-drag-region');
  document.body.classList.toggle('movable', !!on);
};
listenAll('widgets-movable', setMovable);
api.movable().then(setMovable).catch(() => {});

// ---------- durations ----------

// The wheel over the dial sets the focus length while waiting to start
// (saved to the settings once the wheel rests). Until the last save comes
// back, other settings news is older than what the page shows.
let wheelSave = 0;
let wheelInFlight = 0;
const wheelBusy = () => !!wheelSave || wheelInFlight > 0;

function applySettings(s, ownSave = false) {
  if (!s || (!ownSave && wheelBusy())) return;
  settings = s;
  modes = s.modes;
  render();
}

/** The last wheel save on its way (settled either way). */
let wheelSaved = Promise.resolve();
function saveWheel() {
  clearTimeout(wheelSave);
  wheelSave = 0;
  wheelInFlight++;
  wheelSaved = api.setSettings(settings)
    .then((s) => {
      wheelInFlight--;
      if (!wheelBusy()) applySettings(s, true);
    })
    .catch(() => {
      wheelInFlight--;
      if (!wheelBusy()) api.settings().then(applySettings).catch(() => {});
    });
  return wheelSaved;
}

$('dial').addEventListener('wheel', (e) => {
  if (!timer || timer.phase !== 'idle' || !settings) return;
  e.preventDefault();
  const step = 5;
  const m = { ...lengths(timer.mode) };
  m.focus = Math.min(240, Math.max(step, Math.round(m.focus / step) * step + (e.deltaY < 0 ? step : -step)));
  modes = { ...modes, [timer.mode]: m };
  settings = { ...settings, modes };
  timer.total = timer.remaining = m.focus * MIN;
  render();
  clearTimeout(wheelSave);
  wheelSave = setTimeout(saveWheel, 400);
}, { passive: false });
listenAll('settings-changed', applySettings);
api.settings().then(applySettings).catch(() => {});

showGroup(GROUPS[0][0]);
listenMine('focus', apply);
listenMine('focus-chime', (kind) => audio.chime(kind));
api.focus().then(apply).catch(() => {});
buildWave($('wave'), 20, 1);
// The countdown moves every second; when idle only «до HH:MM» and the day change.
setInterval(() => {
  if (timer?.running) render();
}, 250);
setInterval(render, 30_000);
initBackdrop(document.querySelector('.wall'));
