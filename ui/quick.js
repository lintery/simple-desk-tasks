// Global quick-add (Ctrl+Alt+Space): one line, Russian dates understood, Enter adds.

import { api, listenMine } from './lib/api.js';
import { I, esc } from './lib/icons.js';
import { bucketOf, clock } from './lib/model.js';
import { parse } from './lib/nlp.js';

const TITLES = { today: 'Сегодня', upcoming: 'Предстоящие', nodate: 'Без срока' };
const input = document.getElementById('q');
const chipsEl = document.getElementById('chips');
const destEl = document.getElementById('dest');
document.getElementById('tile').innerHTML = I.plus;

function draft() {
  const text = input.value.trim();
  if (!text) return null;
  const p = parse(text);
  return { ...p, dest: bucketOf({ due: p.due, done: false }, clock()) };
}

function renderDraft() {
  const d = draft();
  chipsEl.innerHTML = (d?.chips ?? []).map((chip) => {
    const icon = chip.kind === 'date' ? I.cal : chip.kind === 'time' ? I.clock : I.rep;
    return `<span class="chip">${icon}${esc(chip.label)}</span>`;
  }).join('');
  destEl.innerHTML = `${I.arrow}<b>${TITLES[d?.dest ?? 'nodate']}</b>`;
}

/** Shows why the task can't be saved in place of the destination hint. */
function showError(err) {
  destEl.innerHTML = `<span class="err">${esc(String(err?.message ?? err ?? 'Не получилось сохранить'))}</span>`;
}

function reset() {
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
listenMine('quick-open', (selected) => {
  if (selected) input.value = selected;
  renderDraft();
  input.focus();
  input.setSelectionRange(input.value.length, input.value.length);
  api.storeStatus().then((e) => e && showError(e)).catch(() => {});
});
window.addEventListener('focus', () => input.focus());
document.addEventListener('contextmenu', (e) => {
  if (!(e.target instanceof HTMLInputElement)) e.preventDefault();
});
renderDraft();
