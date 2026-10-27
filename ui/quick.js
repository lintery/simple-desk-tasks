// Global quick-add (Ctrl+Alt+Space): one line, Russian dates understood, Enter adds.

import './lib/theme.js';
import { api, listenMine } from './lib/api.js';
import { attachHilite } from './lib/hilite.js';
import { I, esc, renderChips } from './lib/icons.js';
import { replay } from './lib/motion.js';
import { bucketOf, clock } from './lib/model.js';
import { parse } from './lib/nlp.js';

const TITLES = { today: 'Сегодня', upcoming: 'Предстоящие', nodate: 'Без срока' };
const input = document.getElementById('q');
const chipsEl = document.getElementById('chips');
const destEl = document.getElementById('dest');
document.getElementById('tile').innerHTML = I.plus;
// «завтра», «в 15», «!» are tinted right in the text as they're understood.
const hilite = attachHilite(input, (text) => parse(text).spans);

function draft() {
  const text = input.value.trim();
  if (!text) return null;
  const p = parse(text);
  return { ...p, dest: bucketOf({ due: p.due, done: false }, clock()) };
}

function renderDraft() {
  hilite();
  const d = draft();
  renderChips(chipsEl, d?.chips ?? []);
  destEl.innerHTML = `${I.arrow}<b>${TITLES[d?.dest ?? 'nodate']}</b>`;
}

/** Shows why the task can't be saved in place of the destination hint. */
function showError(err) {
  destEl.innerHTML = `<span class="err">${esc(String(err?.message ?? err ?? 'Не получилось сохранить'))}</span>`;
}

function reset() {
  opened = null; // added or dropped: a late copy isn't for this field any more
  input.value = '';
  renderDraft();
  input.focus();
  api.storeStatus().then((e) => e && showError(e)).catch(() => {});
}

let adding = false;

input.addEventListener('input', renderDraft);
input.addEventListener('keydown', async (e) => {
  if (e.key === 'Escape') {
    e.preventDefault();
    reset();
    api.hideQuick();
    return;
  }
  if (e.key !== 'Enter') return;
  e.preventDefault();
  const d = draft();
  if (!d || adding) return;
  adding = true;
  try {
    await api.add(d);
    // Added: the tile turns into a tick for a moment (seen with Shift+Enter).
    const tile = document.getElementById('tile');
    tile.innerHTML = I.check;
    replay(tile, 'ok');
    setTimeout(() => { tile.innerHTML = I.plus; }, 700);
    reset();
    if (!e.shiftKey) api.hideQuick();
  } catch (err) {
    showError(err); // the text stays, so nothing typed is lost
  } finally {
    adding = false;
  }
});

// Reopening keeps a draft left by clicking away; Esc and Enter clear it.
// Text selected in another app when the hotkey was pressed replaces the draft.
/** The field as it was when the popup opened: a selection copied late by a
 * slow app replaces it only if nothing's been typed since. */
let opened = null;
listenMine('quick-open', (selected) => {
  replay(document.querySelector('.qa'), 'open'); // springs open each time it's shown
  if (selected) input.value = selected;
  opened = input.value;
  renderDraft();
  input.focus();
  input.setSelectionRange(input.value.length, input.value.length);
  api.storeStatus().then((e) => e && showError(e)).catch(() => {});
});
// Closed (or left) meanwhile: a late copy mustn't turn up as a draft.
window.addEventListener('blur', () => {
  opened = null;
});
listenMine('quick-selection', (selected) => {
  if (!selected || opened === null || input.value !== opened) return;
  input.value = opened = selected;
  renderDraft();
  input.setSelectionRange(input.value.length, input.value.length);
});
window.addEventListener('focus', () => input.focus());
document.addEventListener('contextmenu', (e) => {
  if (!(e.target instanceof HTMLInputElement)) e.preventDefault();
});
renderDraft();
