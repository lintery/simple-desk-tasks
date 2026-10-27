// Light or dark: as chosen in the settings, or as Windows' app mode («system»).
// <html data-theme> switches the tokens in style.css. The last one is kept in
// localStorage, so each page's <head> puts it on before anything draws.

import { api, label, listenAll } from './api.js';

const root = document.documentElement;
let pref = 'system';
let asked = 0;

function set(mode) {
  if (root.dataset.theme !== mode) root.dataset.theme = mode;
  try { localStorage.setItem('theme', mode); } catch {}
  // The settings window's own title bar follows too (widgets have none).
  if (label === 'settings') window.__TAURI__.window?.getCurrentWindow().setTheme?.(mode)?.catch(() => {});
}

async function apply() {
  if (pref !== 'system') return set(pref);
  const n = ++asked;
  const light = await api.systemLight().catch(() => matchMedia('(prefers-color-scheme: light)').matches);
  if (n === asked && pref === 'system') set(light ? 'light' : 'dark');
}

const heard = (s) => {
  pref = s?.theme ?? 'system';
  apply();
};
listenAll('settings-changed', heard);
api.settings().then(heard).catch(() => {});
// Windows' mode can change any time: checked now and then, and when a window wakes.
setInterval(() => pref === 'system' && apply(), 20_000);
window.addEventListener('focus', () => pref === 'system' && apply());
matchMedia('(prefers-color-scheme: light)').addEventListener('change', () => pref === 'system' && apply());
