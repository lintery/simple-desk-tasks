// Bars for a `.wave` indicator: fixed pseudo-random heights and speeds, taller
// in the middle, so the row "breathes" without any script running afterwards.

export function buildWave(el, n, seed = 0) {
  el.textContent = '';
  for (let j = 0; j < n; j++) {
    const s = Math.sin((j + 1) * 12.9898 + seed * 7.1) * 43758.5453;
    const r = s - Math.floor(s);
    const env = Math.sin((Math.PI * (j + 0.5)) / n);
    const bar = document.createElement('i');
    bar.style.height = `${(4 + env * 10 * (0.55 + r * 0.45)).toFixed(1)}px`;
    bar.style.animationDuration = `${(1.2 + r * 1.4).toFixed(2)}s`;
    bar.style.animationDelay = `${(-r * 2).toFixed(2)}s`;
    el.appendChild(bar);
  }
}
