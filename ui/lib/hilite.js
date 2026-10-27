// What the parser understood, marked right in the text field: a layer under the
// input repeats its text (invisible) and tints the recognised words behind it.

import { esc } from './icons.js';

/**
 * @param input the text field (its background must be transparent)
 * @param spansOf text → [[start, end, kind], …] to mark
 * @returns update(): call after changing the value from code
 */
export function attachHilite(input, spansOf) {
  const wrap = document.createElement('span');
  wrap.className = 'hl';
  const layer = document.createElement('span');
  layer.className = 'hl-layer';
  layer.setAttribute('aria-hidden', 'true');
  const text = document.createElement('span');
  layer.append(text);
  input.replaceWith(wrap);
  wrap.append(layer, input);

  let html = '';
  // The field's font and padding, once it's on the page (a popover may be
  // built before it's attached, when there are no styles to read yet).
  let styled = false;
  const copyStyle = () => {
    if (styled || !input.isConnected) return;
    styled = true;
    const style = getComputedStyle(input);
    for (const prop of ['font', 'letterSpacing', 'paddingLeft', 'paddingRight']) layer.style[prop] = style[prop];
  };
  const update = () => {
    copyStyle();
    const value = input.value;
    let out = '';
    let last = 0;
    for (const [s, e, kind] of spansOf(value) ?? []) {
      if (s < last || e > value.length) continue;
      out += esc(value.slice(last, s)) + `<mark class="${esc(kind)}">${esc(value.slice(s, e))}</mark>`;
      last = e;
    }
    out += esc(value.slice(last));
    if (out !== html) {
      html = out;
      text.innerHTML = out;
    }
    scroll();
  };
  // The field scrolls sideways once the text is longer than it: the layer follows.
  const scroll = () => {
    text.style.transform = `translateX(${-input.scrollLeft}px)`;
  };
  input.addEventListener('input', update);
  input.addEventListener('focus', copyStyle);
  input.addEventListener('scroll', scroll);
  for (const type of ['keyup', 'select', 'click', 'focus']) input.addEventListener(type, scroll);
  update();
  return update;
}
