// Small motions that only run when something happens (transform/opacity, so
// the compositor does them). «Reduce animations» in Windows turns them off:
// CSS ones through style.css, the scripted ones here.

const media = matchMedia('(prefers-reduced-motion: reduce)');
export const reduced = () => media.matches;

/** Plays a CSS animation class once more, even if it's on already. The class
 *  goes when that animation ends: the element's own, or — when the class
 *  animates something inside it — the one named `name`. Other animations
 *  ending inside it (bubbling up) don't count. */
export function replay(el, cls, name = null) {
  if (!el) return;
  el.classList.remove(cls);
  void el.offsetWidth; // a fresh start for the animation
  el.classList.add(cls);
  const done = (e) => {
    if (name ? e.animationName !== name : e.target !== el) return;
    el.removeEventListener('animationend', done);
    el.classList.remove(cls);
  };
  el.addEventListener('animationend', done);
}

/** Where each element is now (by key), before the DOM changes. */
export function positions(els, key) {
  const at = new Map();
  for (const el of els) at.set(key(el), el.getBoundingClientRect().top);
  return at;
}

/** After the DOM changed: the elements that moved slide from where they were
 *  (FLIP). Ones that are new, or moved very far, just appear. */
export function glide(before, els, key, { duration = 220, max = 600 } = {}) {
  if (reduced() || !before.size) return;
  for (const el of els) {
    const was = before.get(key(el));
    if (was === undefined) continue;
    const dy = was - el.getBoundingClientRect().top;
    if (Math.abs(dy) < 1 || Math.abs(dy) > max) continue;
    el.animate([{ transform: `translateY(${dy}px)` }, { transform: 'none' }], {
      duration, easing: 'cubic-bezier(.2,.8,.2,1)',
    });
  }
}

/** Opens a block to its height from nothing (or closes it to nothing). */
export function unfold(el, open, duration = 180) {
  if (!el || reduced()) return Promise.resolve();
  const h = el.scrollHeight;
  const frames = [{ height: '0px', opacity: 0 }, { height: `${h}px`, opacity: 1 }];
  // Closed, it stays closed until the caller removes it (no flash back to full height).
  const a = el.animate(open ? frames : frames.reverse(), { duration, easing: 'cubic-bezier(.2,.8,.2,1)', fill: open ? 'none' : 'forwards' });
  el.style.overflow = 'hidden';
  // Closed, it keeps hiding what's in it until the caller takes it away.
  return a.finished.catch(() => {}).finally(() => { if (open) el.style.overflow = ''; });
}
