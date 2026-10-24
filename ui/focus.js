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
/** Focus, break, long break (minutes) and focus sessions per long break. */
const MODES = { pomodoro: [25, 5, 15, 4], focus50: [50, 10, 10, 0], deep90: [90, 20, 20, 0] };
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
  button.innerHTML = `<svg class="i"><use href="#${name}"/></svg>`;
}

/** What comes after the current phase, e.g. «перерыв 5 мин». */
function nextLabel(t) {
  const [focus, short, long, every] = MODES[t.mode] ?? MODES.pomodoro;
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

  const [, , , every] = MODES[t.mode] ?? MODES.pomodoro;
  $('rounds').style.display = every ? '' : 'none';
  $('sub').style.display = every ? 'none' : '';
  if (every) {
    const finished = t.phase === 'long' ? every : round(t);
    $('rounds').innerHTML = Array.from({ length: every }, (_, i) => {
      const cls = i < finished ? 'on' : i === finished && t.phase === 'focus' ? 'cur' : '';
      return `<i class="${cls}"></i>`;
    }).join('');
  } else {
    $('sub').textContent = t.running ? `до ${clock(t.endsAt)}` : idle ? `до ${clock(Date.now() + t.remaining)}` : '';
  }

  const today = (t.day === localDay() ? t.today : 0) + (t.phase === 'focus' ? t.total - rest : 0);
  $('today').innerHTML = `сегодня <em>${minutes(today)}</em>`;

  for (const b of $('modes').children) b.classList.toggle('sel', b.dataset.mode === t.mode);
  $('modes').classList.toggle('locked', !idle);

  setIcon($('go'), t.running ? 'pause' : 'play');
  $('go').title = t.running ? 'Пауза' : idle ? 'Начать' : 'Продолжить';
  $('skip').disabled = idle;
  $('reset').disabled = idle && round(t) === 0;
  const when = idle ? 'после' : t.running ? `в ${clock(t.endsAt)}` : 'дальше';
  $('next').innerHTML = `<small>${when}</small>${nextLabel(t)}`;
}

// ---------- sounds ----------

const soundTitle = (id) => SOUNDS.find((s) => s.id === id)?.title ?? '';

/** The sound that should be audible right now, if any. */
function wanted() {
  if (!timer?.sound || muted) return null;
  return focusing(timer) || manual ? timer.sound : null;
}

function renderSound() {
  const playing = audio.current();
  for (const tile of $('tiles').children) {
    const id = tile.dataset.id;
    tile.classList.toggle('on', id === playing);
    tile.classList.toggle('sel', id !== playing && id === timer?.sound);
  }
  $('wave').classList.toggle('still', !playing);
  const now = $('now');
  if (playing) now.innerHTML = `<em>${soundTitle(playing)}</em>`;
  else if (!timer?.sound || muted) now.textContent = 'выключены';
  else now.innerHTML = `<em>${soundTitle(timer.sound)}</em> · ${timer.phase === 'idle' ? 'со стартом' : 'в фокусе'}`;
  const silent = muted || !timer?.sound;
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
  const want = wanted();
  if (want !== audio.current()) {
    if (want) audio.play(want);
    else audio.stop();
  }
  renderSound();
}

/** Remembers the sound and volume in focus.rs (the volume once dragging settles). */
function savePrefs(delay = 0) {
  clearTimeout(prefsTimer);
  const { sound, volume } = timer;
  prefsTimer = setTimeout(() => {
    prefsTimer = 0;
    api.focusPrefs(sound ?? null, volume).catch(() => {});
  }, delay);
}

function pickSound(id) {
  if (!timer) return;
  if (audio.current() === id) {
    // Clicking what's playing turns the sound off.
    manual = false;
    timer.sound = null;
  } else {
    muted = false;
    store.set('muted', '0');
    manual = !focusing(timer);
    timer.sound = id;
  }
  savePrefs();
  syncAudio();
}

function setVolume(v) {
  if (!timer) return;
  timer.volume = Math.round(Math.min(1, Math.max(0, v)) * 100) / 100;
  audio.setVolume(timer.volume);
  savePrefs(400);
  renderSound();
}

$('tiles').innerHTML = SOUNDS.map(
  (s) => `<button class="tile" data-id="${s.id}"><svg class="ti"><use href="#s-${s.id}"/></svg><span>${s.title}</span>` +
    '<div class="eq"><i></i><i></i><i></i></div></button>',
).join('');
$('tiles').addEventListener('click', (e) => {
  const tile = e.target.closest('.tile');
  if (tile) pickSound(tile.dataset.id);
});
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
  if (next.sound && !SOUNDS.some((s) => s.id === next.sound)) next.sound = null;
  // A new phase, a pause or a start ends any hand-started sound.
  if (timer && (timer.phase !== next.phase || timer.running !== next.running)) manual = false;
  // The page's own unsaved sound/volume choice wins over an older echo.
  if (timer && prefsTimer) {
    next.sound = timer.sound;
    next.volume = timer.volume;
  }
  timer = next;
  audio.setVolume(timer.volume);
  render();
  syncAudio();
}

async function act(action) {
  try {
    apply(await api.focusAction(action));
  } catch {}
}

$('go').addEventListener('click', () => {
  if (!timer) return;
  if (!timer.running) audio.chime('start');
  act(timer.running ? 'pause' : 'start');
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
