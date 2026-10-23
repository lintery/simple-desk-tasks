// Draws the piece of wallpaper that is physically behind this window, blurred,
// so the widget reads as smoked glass even though Windows never makes an
// inactive desktop-layer window "acrylic".
//
// The blur is baked once into a small canvas (a heavy blur needs no detail),
// instead of a live CSS filter over a window-sized layer: that keeps the GPU
// process tens of MB lighter. Redrawn only on move/resize/wallpaper change.

import { api, listenMine } from './api.js';

const BLEED = 80; // css px drawn past the edges so the blur has no dark rim
const DOWNSCALE = 6; // css px per canvas px
const BLUR = 36; // css px

export function initBackdrop(canvas) {
  const img = new Image();
  let geo = null;
  let ver = -1;
  let frame = 0;

  function draw() {
    frame = 0;
    if (!geo || !img.naturalWidth) return;
    const { monW, monH, winX, winY, scale, style } = geo;
    const iw = img.naturalWidth, ih = img.naturalHeight;
    // Wallpaper placement on the monitor, in physical px.
    let w = iw, h = ih;
    if (style === 'fill' || style === 'span') {
      const k = Math.max(monW / iw, monH / ih); w = iw * k; h = ih * k;
    } else if (style === 'fit') {
      const k = Math.min(monW / iw, monH / ih); w = iw * k; h = ih * k;
    } else if (style === 'stretch') {
      w = monW; h = monH;
    }
    const k = 1 / DOWNSCALE;
    canvas.width = Math.ceil((innerWidth + 2 * BLEED) * k);
    canvas.height = Math.ceil((innerHeight + 2 * BLEED) * k);
    const ctx = canvas.getContext('2d');
    ctx.fillStyle = '#000';
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.filter = `blur(${BLUR * k}px) saturate(1.4)`;
    // Wallpaper's top-left relative to this canvas, in canvas px.
    const x = (((monW - w) / 2 - winX) / scale + BLEED) * k;
    const y = (((monH - h) / 2 - winY) / scale + BLEED) * k;
    const dw = (w / scale) * k, dh = (h / scale) * k;
    if (style === 'tile') {
      for (let ty = y - Math.ceil(y / dh) * dh; ty < canvas.height; ty += dh) {
        for (let tx = x - Math.ceil(x / dw) * dw; tx < canvas.width; tx += dw) ctx.drawImage(img, tx, ty, dw, dh);
      }
    } else {
      ctx.drawImage(img, x, y, dw, dh);
    }
    canvas.classList.add('ready');
  }

  const schedule = () => {
    if (!frame) frame = requestAnimationFrame(draw);
  };

  function apply(g) {
    if (!g) return;
    geo = g;
    if (g.ver !== ver) {
      ver = g.ver;
      img.onload = schedule;
      img.src = `http://wall.localhost/?v=${g.ver}`;
    } else {
      schedule();
    }
  }

  listenMine('backdrop', apply);
  api.backdrop().then(apply).catch(() => {});
}
