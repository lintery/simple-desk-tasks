// The focus page's sound engine for showcase screenshots: same list, same
// "what's playing" answers, no audio (a live AudioContext stalls headless Edge).
// Mapped over ui/lib/sounds.js by the import map in showcase-widget.html.

export { SOUNDS } from '../../ui/lib/sounds.js?real';

export function createAudio() {
  let mix = [];
  return {
    setMix(entries) {
      mix = entries.map((e) => e.id);
    },
    play(id) {
      mix = [id];
    },
    stop() {
      mix = [];
    },
    current: () => mix[0] ?? null,
    mix: () => [...mix],
    setVolume() {},
    volume: () => 0.6,
    chime() {},
    dispose() {},
    stats: () => ({}),
  };
}
