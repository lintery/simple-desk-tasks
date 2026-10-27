// A list with a scrollbar has its content that much further from the right
// edge: --sb tells the header and the other boxes, so their right edges stay
// in line with it (style.css .hd, .add; calendar.css .mo).

/** Keeps --sb on `root` equal to the width of `list`'s scrollbar. */
export function trackScrollbar(list, root = document.body) {
  let last = -1;
  const update = () => {
    const sb = list.offsetWidth - list.clientWidth;
    if (sb !== last) {
      last = sb;
      root.style.setProperty('--sb', `${sb}px`);
    }
  };
  new ResizeObserver(update).observe(list);
  // Content growing or shrinking brings the scrollbar and takes it away.
  new MutationObserver(update).observe(list, { childList: true, subtree: true });
  update();
}
