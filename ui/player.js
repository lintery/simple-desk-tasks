// «Плеер»: what Spotify (or another player) is playing, with prev/play/next,
// seeking and the app's own volume. Data comes from media.rs; the window is
// shown and hidden from there too.

import { api, listenAll, listenMine } from './lib/api.js';
import { initBackdrop } from './lib/backdrop.js';
import { buildWave } from './lib/wave.js';

const $ = (id) => document.getElementById(id);
const w = $('w');
const bar = $('bar');
const slider = $('slider');
const clamp = (x, a, b) => Math.min(b, Math.max(a, x));
let media = null;
/** Play state shown right after a click, until the player itself confirms it. */
let optimistic = null;
let settle = 0;
let shownCover = null;
/** Position (ms) under the mouse while the seek bar is being dragged. */
let dragging = null;
/** Volume set here, shown until the mixer reports it back. */
const local = { value: null, muted: false, until: 0 };

/** Swaps a button's sprite icon only when it changes (see focus.js). */
function setIcon(button, name) {
  if (button.dataset.icon === name) return;
  button.dataset.icon = name;
  button.innerHTML = `<svg class="i"><use href="#i-${name}"/></svg>`;
}

const mmss = (ms) => {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
};

const playing = () => (optimistic ?? media?.playing) === true;

/** Where the track is now: the reported position moved on while playing. */
function position() {
  if (!media) return 0;
  if (dragging != null) return dragging;
  const moved = media.playing && media.updated > 0 ? Math.max(0, Date.now() - media.updated) : 0;
  return Math.min(media.position + moved, media.duration || Infinity);
}

const volume = () =>
  Date.now() < local.until ? local : { value: media?.volume ?? null, muted: !!media?.muted };

function renderTime() {
  if (!media || !media.duration) return;
  const pos = position();
  $('fill').style.width = `${(pos / media.duration) * 100}%`;
  $('pos').textContent = mmss(pos);
}

function renderVolume() {
  const v = volume();
  $('vol').style.display = v.value == null ? 'none' : '';
  if (v.value == null) return;
  $('level').style.width = `${v.value * 100}%`;
  slider.classList.toggle('mute', v.muted);
  $('mute').classList.toggle('off', v.muted);
  setIcon($('mute'), v.muted ? 'spk-off' : 'spk');
  $('mute').title = v.muted ? 'Включить звук' : 'Выключить звук';
}

function render() {
  if (!media) return;
  const on = playing();
  w.classList.toggle('paused', !on);
  w.classList.toggle('no-time', !media.duration);
  $('title').textContent = media.title;
  $('title').title = media.title;
  $('artist').textContent = media.artist;
  const icon = media.app === 'Spotify' ? 'spotify' : 'note';
  $('src').innerHTML = `<svg class="i"><use href="#i-${icon}"/></svg>`;
  $('src').append(media.app ?? '');
  $('src').style.display = media.app ? '' : 'none';
  const cover = media.cover ?? null;
  if (cover !== shownCover) {
    shownCover = cover;
    $('art').classList.toggle('has-cover', !!cover);
    $('art').style.backgroundImage = cover ? `url("${cover}")` : '';
  }
  setIcon($('pp'), on ? 'pause' : 'play');
  $('pp').title = on ? 'Пауза' : 'Играть';
  $('prev').disabled = !media.canPrev;
  $('next').disabled = !media.canNext;
  bar.classList.toggle('seekable', !!media.canSeek);
  $('dur').textContent = media.duration ? mmss(media.duration) : '';
  renderTime();
  renderVolume();
}

function show(next) {
  if (next && media && next.playing !== media.playing) optimistic = null;
  media = next;
  render();
}

async function control(action) {
  clearTimeout(settle); // an earlier click's wait mustn't undo this one
  if (action === 'toggle' && media) {
    optimistic = !playing();
    render();
  }
  try {
    await api.mediaControl(action);
  } catch {
    optimistic = null;
    render();
  }
  // Whatever the player did, the next poll (≤1 s) shows the truth.
  clearTimeout(settle);
  settle = setTimeout(() => {
    optimistic = null;
    render();
  }, 2500);
}

$('pp').addEventListener('click', () => control('toggle'));
$('prev').addEventListener('click', () => control('prev'));
$('next').addEventListener('click', () => control('next'));

/** A change the player refused: forget the local guess and show its real state. */
function resync() {
  local.until = 0;
  api.media().then(show).catch(() => {});
}

// ---------- seeking ----------

const atPointer = (e) => {
  const r = bar.getBoundingClientRect();
  return clamp((e.clientX - r.left) / r.width, 0, 1) * media.duration;
};
const endDrag = () => {
  dragging = null;
  bar.classList.remove('dragging');
  renderTime();
};
bar.addEventListener('pointerdown', (e) => {
  if (e.button !== 0 || !media?.canSeek || !media.duration) return;
  bar.setPointerCapture(e.pointerId);
  bar.classList.add('dragging');
  dragging = atPointer(e);
  renderTime();
});
bar.addEventListener('pointermove', (e) => {
  if (dragging == null || !media || !bar.hasPointerCapture(e.pointerId)) return;
  dragging = atPointer(e);
  renderTime();
});
bar.addEventListener('pointerup', () => {
  if (dragging == null) return;
  const ms = Math.round(dragging);
  endDrag();
  if (!media) return; // the player went away mid-drag
  // Jump there right away; the player's own report follows.
  media.position = ms;
  media.updated = Date.now();
  renderTime();
  api.mediaControl('seek', ms).catch(resync);
});
// Cancelled, or capture lost (e.g. the window hid mid-drag): don't seek.
bar.addEventListener('pointercancel', endDrag);
bar.addEventListener('lostpointercapture', () => {
  if (dragging != null) endDrag();
});

// ---------- volume ----------

/** Volume and mute changes for the mixer, sent one at a time in order; a run of
 * the same change (a slider drag) collapses to its latest value. */
const mixer = [];
let mixerBusy = false;

function queueMixer(action, value) {
  const last = mixer[mixer.length - 1];
  if (last?.action === action) last.value = value;
  else mixer.push({ action, value });
  sendMixer();
}

async function sendMixer() {
  if (mixerBusy) return;
  mixerBusy = true;
  while (mixer.length) {
    const { action, value } = mixer.shift();
    try {
      await api.mediaControl(action, value);
    } catch {
      mixer.length = 0;
      resync();
    }
  }
  mixerBusy = false;
}

function setVolume(v) {
  if (volume().value == null) return;
  local.value = Math.round(clamp(v, 0, 1) * 100) / 100;
  local.muted = false;
  local.until = Date.now() + 2000;
  renderVolume();
  queueMixer('volume', local.value);
}

$('mute').addEventListener('click', () => {
  const v = volume();
  if (v.value == null) return;
  Object.assign(local, { value: v.value, muted: !v.muted, until: Date.now() + 2000 });
  renderVolume();
  queueMixer('mute', local.muted ? 1 : 0);
});
const fromSlider = (e) => {
  const r = slider.getBoundingClientRect();
  setVolume((e.clientX - r.left) / r.width);
};
slider.addEventListener('pointerdown', (e) => {
  if (e.button !== 0) return;
  slider.setPointerCapture(e.pointerId);
  fromSlider(e);
});
slider.addEventListener('pointermove', (e) => {
  if (slider.hasPointerCapture(e.pointerId)) fromSlider(e);
});
// The wheel anywhere on the card turns the volume.
w.addEventListener('wheel', (e) => {
  if (volume().value == null) return;
  e.preventDefault();
  setVolume(volume().value + (e.deltaY < 0 ? 0.05 : -0.05));
}, { passive: false });

const setMovable = (on) => {
  if (on) w.setAttribute('data-tauri-drag-region', 'deep');
  else w.removeAttribute('data-tauri-drag-region');
  document.body.classList.toggle('movable', !!on);
};
listenAll('widgets-movable', setMovable);
api.movable().then(setMovable).catch(() => {});

buildWave($('wave'), 6, 3);
listenMine('media', show);
api.media().then(show).catch(() => {});
setInterval(() => {
  if (playing() && dragging == null) renderTime();
  // A local volume guess runs out: show what the mixer reports again.
  if (local.until && Date.now() >= local.until) {
    local.until = 0;
    renderVolume();
  }
}, 500);
initBackdrop(document.querySelector('.wall'));
