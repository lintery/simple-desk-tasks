// «Плеер»: what Spotify (or another player) is playing, with prev/play/next.
// Data comes from media.rs; the window is shown and hidden from there too.

import { api, listenAll, listenMine } from './lib/api.js';
import { initBackdrop } from './lib/backdrop.js';
import { buildWave } from './lib/wave.js';

const $ = (id) => document.getElementById(id);
const w = $('w');
let media = null;
/** Play state shown right after a click, until the player itself confirms it. */
let optimistic = null;
let settle = 0;
let shownCover = null;

/** Swaps a button's sprite icon only when it changes (see focus.js). */
function setIcon(button, name) {
  if (button.dataset.icon === name) return;
  button.dataset.icon = name;
  button.innerHTML = `<svg class="i"><use href="#${name}"/></svg>`;
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
  const moved = media.playing && media.updated > 0 ? Math.max(0, Date.now() - media.updated) : 0;
  return Math.min(media.position + moved, media.duration || Infinity);
}

function renderTime() {
  if (!media || !media.duration) return;
  const pos = position();
  $('fill').style.width = `${(pos / media.duration) * 100}%`;
  $('pos').textContent = mmss(pos);
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
  $('src').innerHTML = `<svg class="i"><use href="#${icon}"/></svg>`;
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
  $('dur').textContent = media.duration ? mmss(media.duration) : '';
  renderTime();
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
  if (playing()) renderTime();
}, 500);
initBackdrop(document.querySelector('.wall'));
