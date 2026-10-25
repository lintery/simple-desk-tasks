// sounds.js: ambient sounds and chimes, Web Audio API only.
//
// Rain, nature, fan and train are field recordings: seamless loops the app
// downloads once (sounds.rs) and hands over through createAudio's `load`.
// Without them (offline, a dev page) every sound is synthesized as below, and
// the noises always are.
//
// Synthesis: no audio files, no per-sample JS at runtime. Once per AudioContext
// (lazily, per kind) we compute noise loops, banks of short one-shot "grains"
// (drops, snaps, bubbles, chirps...) and seamless scattered textures. At runtime
// everything is looped / filtered / enveloped by native nodes, and discrete events
// are scheduled slightly ahead by a timer running every 200 ms.

export const SOUNDS = [
  { id: 'rain-light', title: 'Слабый дождь', group: 'rain' },
  { id: 'rain', title: 'Дождь', group: 'rain' },
  { id: 'rain-heavy', title: 'Ливень', group: 'rain' },
  { id: 'rain-wind', title: 'Дождь с ветром', group: 'rain' },
  { id: 'rain-thunder', title: 'Гроза', group: 'rain' },
  { id: 'rain-roof', title: 'Дождь по крыше', group: 'rain' },
  { id: 'waves', title: 'Волны', group: 'nature' },
  { id: 'fire', title: 'Камин', group: 'nature' },
  { id: 'wind', title: 'Ветер', group: 'nature' },
  { id: 'stream', title: 'Ручей', group: 'nature' },
  { id: 'night', title: 'Ночь', group: 'nature' },
  { id: 'forest', title: 'Лес', group: 'nature' },
  { id: 'white', title: 'Белый шум', group: 'noise' },
  { id: 'pink', title: 'Розовый шум', group: 'noise' },
  { id: 'brown', title: 'Коричневый шум', group: 'noise' },
  { id: 'fan', title: 'Вентилятор', group: 'noise' },
  { id: 'train', title: 'Поезд', group: 'noise' },
];

const FADE = 1.5;          // s, fade in / fade out / crossfade
const TICK_MS = 200;       // scheduler period
const LOOKAHEAD = 0.6;     // s of events scheduled ahead of currentTime
const CHIME_LEVEL = 0.56;  // chime bus gain (before master volume)
const DEFAULT_VOLUME = 0.6;
const volumeCurve = (v) => v * v; // slider position -> gain, roughly perceptual

// Per-sound output trim, measured offline so all sounds have about the same
// K-weighted loudness (see test.html?measure).
const TRIM = {
  'rain-light': 0.99, rain: 0.53, 'rain-heavy': 0.35, 'rain-wind': 0.54, 'rain-thunder': 0.38, 'rain-roof': 0.54,
  waves: 1, fire: 1.43, wind: 1.25, stream: 0.64, night: 0.65, forest: 1.32,
  white: 0.35, pink: 0.46, brown: 0.64, fan: 0.68, train: 0.72,
};

// The recorded loops and their trim to the synthesized sounds' loudness
// (K-weighted over the whole loop, measured offline; the fire's held to
// +9 dB so its crackles don't push the limiter).
const RECORDING_TRIM = {
  'rain-light': 1.84, rain: 1.91, 'rain-heavy': 1.86, 'rain-wind': 1.8, 'rain-thunder': 1.86, 'rain-roof': 1.76,
  waves: 2.09, fire: 2.8, wind: 1.88, stream: 1.93, night: 1.74, forest: 2.14, fan: 1.24, train: 1.19,
};
/** Sounds with a recording (the noises are only ever synthesized). */
export const RECORDED = new Set(Object.keys(RECORDING_TRIM));
/** After a failed download a sound is synthesized; it's tried again this much later. */
const RETRY_MS = 60_000;
/** A recording slower to arrive than this has the synthesized sound fill in meanwhile. */
const SYNTH_AFTER_MS = 4000;

const rand = (a, b) => a + Math.random() * (b - a);
const logRand = (a, b) => a * Math.pow(b / a, Math.random());
const pick = (arr) => arr[(Math.random() * arr.length) | 0];
const expRand = (mean) => -Math.log(1 - Math.random()) * mean; // Poisson inter-arrival
const clamp = (x, a, b) => Math.min(b, Math.max(a, x));
// Equal-power fade curves: during a crossfade of two unrelated sounds the total
// power stays constant (a linear crossfade dips ~3 dB in the middle).
const EQ_IN = Float32Array.from({ length: 32 }, (_, i) => Math.sin((i / 31) * (Math.PI / 2)));
const EQ_OUT = EQ_IN.slice().reverse();
const chain = (...ns) => { for (let i = 0; i < ns.length - 1; i++) ns[i].connect(ns[i + 1]); return ns[ns.length - 1]; };

// ---------------------------------------------------------------- buffers

const cache = new WeakMap(); // ctx -> { white, pink, brown, 'bank-drop', 'tex-patter', ... }
const store = (ctx) => { let s = cache.get(ctx); if (!s) cache.set(ctx, (s = {})); return s; };
const noise = (ctx, kind) => (store(ctx)[kind] ??= makeNoise(ctx, kind));
const bank = (ctx, kind) => (store(ctx)['bank-' + kind] ??= makeBank(ctx, kind));
const textureBuf = (ctx, kind) => (store(ctx)['tex-' + kind] ??= makeTexture(ctx, kind));

// 8 s of stereo noise (independent L/R), loopable without a seam, normalised to RMS 0.2.
function makeNoise(ctx, kind, seconds = 8) {
  const sr = ctx.sampleRate, n = Math.round(sr * seconds), xf = Math.round(sr * 0.5);
  const buf = ctx.createBuffer(2, n, sr), tmp = new Float32Array(n + xf);
  const leak = 1 - (2 * Math.PI * 50) / sr; // brown: leaky integrator, corner ~50 Hz (no wasted sub-bass)
  for (let ch = 0; ch < 2; ch++) {
    let b0 = 0, b1 = 0, b2 = 0, b3 = 0, b4 = 0, b5 = 0, b6 = 0, br = 0;
    const gen = () => {
      const w = Math.random() * 2 - 1;
      if (kind === 'white') return w;
      if (kind === 'brown') return (br = leak * br + w);
      // pink: Paul Kellet's refined filter (-3 dB/oct)
      b0 = 0.99886 * b0 + w * 0.0555179; b1 = 0.99332 * b1 + w * 0.0750759;
      b2 = 0.969 * b2 + w * 0.153852; b3 = 0.8665 * b3 + w * 0.3104856;
      b4 = 0.55 * b4 + w * 0.5329522; b5 = -0.7616 * b5 - w * 0.016898;
      const p = b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362;
      b6 = w * 0.115926;
      return p;
    };
    for (let i = 0; i < 4096; i++) gen(); // let filter state settle
    for (let i = 0; i < tmp.length; i++) tmp[i] = gen();
    const y = buf.getChannelData(ch);
    y.set(tmp.subarray(0, n));
    // Seamless loop: the head is an equal-power crossfade into the samples that
    // follow the tail, so y[n-1] -> y[0] continues the generator without a jump.
    for (let i = 0; i < xf; i++) {
      const a = ((i + 0.5) / xf) * (Math.PI / 2);
      y[i] = tmp[i] * Math.sin(a) + tmp[n + i] * Math.cos(a);
    }
    normRms(y);
  }
  return buf;
}

function normRms(y, target = 0.2) {
  let mean = 0, e = 0;
  for (let i = 0; i < y.length; i++) mean += y[i];
  mean /= y.length;
  for (let i = 0; i < y.length; i++) { y[i] -= mean; e += y[i] * y[i]; }
  const k = e > 0 ? target / Math.sqrt(e / y.length) : 0;
  for (let i = 0; i < y.length; i++) y[i] *= k;
  return y;
}

// RBJ biquad applied in place (only used while building grains and textures).
function biquad(x, type, f, Q, sr) {
  f = Math.min(f, 0.45 * sr); // above Nyquist the RBJ formulas go unstable (e.g. a 16 kHz headset)
  const w = (2 * Math.PI * f) / sr, c = Math.cos(w), al = Math.sin(w) / (2 * Q);
  const a0 = 1 + al, a1 = -2 * c, a2 = 1 - al;
  let b0, b1, b2;
  if (type === 'bp') { b0 = al; b1 = 0; b2 = -al; }
  else if (type === 'hp') { b0 = (1 + c) / 2; b1 = -(1 + c); b2 = b0; }
  else { b0 = (1 - c) / 2; b1 = 1 - c; b2 = b0; }
  let x1 = 0, x2 = 0, y1 = 0, y2 = 0;
  for (let i = 0; i < x.length; i++) {
    const y0 = (b0 * x[i] + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2) / a0;
    x2 = x1; x1 = x[i]; y2 = y1; y1 = y0; x[i] = y0;
  }
  return x;
}

// ---------------------------------------------------------------- grains (init time)
// Each returns a mono Float32Array (sample rate sr); banks peak-normalise them and
// fade the last 0.5 ms so no grain can end with a step.

const white = (n) => { const x = new Float32Array(n); for (let i = 0; i < n; i++) x[i] = Math.random() * 2 - 1; return x; };
// linear attack of `atk` samples, then exponential decay with time constant `tau` samples
function env(x, atk, tau, from = 0) {
  for (let i = 0; i < x.length; i++) {
    const t = i - from;
    x[i] *= t < 0 ? 0 : t < atk ? t / atk : Math.exp(-(t - atk) / tau);
  }
  return x;
}
function addInto(x, y, a = 1, at = 0) { for (let i = 0; i < y.length && at + i < x.length; i++) x[at + i] += a * y[i]; return x; }
function normPeak(x) {
  let p = 0;
  for (let i = 0; i < x.length; i++) p = Math.max(p, Math.abs(x[i]));
  if (!(p > 0 && p < Infinity)) return x.fill(0); // never let NaN reach the graph
  for (let i = 0; i < x.length; i++) x[i] /= p;
  const f = Math.min(x.length, 24);
  for (let i = 0; i < f; i++) x[x.length - 1 - i] *= i / f;
  return x;
}

// A raindrop hitting a surface: band-passed noise tick, ~1 ms attack, short decay,
// sometimes with a faint upward-gliding bubble "plip".
function dropGrain(sr, fLo = 1200, fHi = 5000, tLo = 0.004, tHi = 0.02, plipP = 0.25) {
  const atk = rand(0.0006, 0.002) * sr, tau = rand(tLo, tHi) * sr;
  const plip = Math.random() < plipP, d = rand(0.004, 0.01) * sr, t0 = Math.round(rand(0.001, 0.004) * sr);
  const x = white(Math.ceil(Math.max(atk + tau * 7, plip ? t0 + d * 7 : 0)));
  biquad(x, 'bp', logRand(fLo, fHi), rand(0.7, 2), sr);
  env(x, atk, tau);
  if (plip) {
    const f0 = rand(1500, 3200), amp = rand(0.06, 0.15), ramp = 0.0005 * sr;
    let ph = 0;
    for (let i = t0; i < x.length; i++) {
      const t = i - t0;
      ph += (2 * Math.PI * f0 * (1 + (0.4 * t) / x.length)) / sr;
      x[i] += amp * Math.sin(ph) * Math.exp(-t / d) * Math.min(1, t / ramp);
    }
  }
  return x;
}

// Air bubble in water (Minnaert resonance): a decaying sine whose pitch rises
// as the bubble nears the surface. Q 8-30 sets how many cycles it rings.
function bubbleGrain(sr, fLo = 300, fHi = 2000) {
  const f0 = logRand(fLo, fHi), tau = (rand(8, 30) / (Math.PI * f0)) * sr, rise = rand(0.1, 0.8);
  const n = Math.ceil(tau * 6 + 0.001 * sr), x = new Float32Array(n), ramp = 0.0004 * sr;
  let ph = 0;
  for (let i = 0; i < n; i++) {
    ph += (2 * Math.PI * f0 * (1 + (rise * i) / n)) / sr;
    x[i] = Math.sin(ph) * Math.exp(-i / tau) * Math.min(1, i / ramp);
  }
  return x;
}

// Fire snap: 1-4 micro-fractures within a few ms, random brightness.
function snapGrain(sr) {
  const n = Math.ceil(rand(0.008, 0.02) * sr), x = new Float32Array(n);
  let p = 0;
  for (let j = 0, k = 1 + ((Math.random() ** 2 * 4) | 0); j < k && p < n; j++) {
    const len = Math.max(4, (rand(0.0001, 0.0008) * sr) | 0), a = j ? rand(0.2, 0.8) : 1;
    for (let i = 0; i < len && p + i < n; i++) x[p + i] += a * (Math.random() * 2 - 1) * Math.exp(-i / (len * 0.35));
    p += (rand(0.0003, 0.004) * sr) | 0;
  }
  biquad(x, 'hp', logRand(300, 2000), 0.7, sr);
  return biquad(x, 'lp', logRand(2500, 14000), 0.7, sr);
}

// Sap sizzle: a short swarm of tiny ticks, bright, fading.
function sizzleGrain(sr) {
  const d = rand(0.08, 0.4), n = Math.ceil(d * sr), x = new Float32Array(n);
  const rate = rand(400, 1500), rise = 0.015 * sr, tau = (d * sr) / 3;
  for (let p = 0; p < n; p += Math.max(1, (expRand(1 / rate) * sr) | 0)) {
    const a = Math.min(1, p / rise) * Math.exp(-p / tau) * Math.random() ** 2, len = 2 + ((Math.random() * 10) | 0);
    for (let i = 0; i < len && p + i < n; i++) x[p + i] += a * (Math.random() * 2 - 1);
  }
  biquad(x, 'hp', 2500, 0.7, sr);
  return biquad(x, 'lp', 12000, 0.7, sr);
}

// Wood pop: loud click, a few secondary snaps and a short resonant body.
function popGrain(sr) {
  const tb = rand(0.012, 0.03) * sr, n = Math.ceil(Math.max(rand(0.06, 0.12) * sr, tb * 7)), x = new Float32Array(n);
  const snap = (at, len, a) => { for (let i = 0; i < len && at + i < n; i++) x[at + i] += a * (Math.random() * 2 - 1) * Math.exp(-i / (len * 0.3)); };
  snap(0, (rand(0.0006, 0.0012) * sr) | 0, 1);
  for (let k = 0, m = 2 + ((Math.random() * 4) | 0); k < m; k++) snap((rand(0.003, 0.04) * sr) | 0, (rand(0.0002, 0.0006) * sr) | 0, rand(0.2, 0.6));
  const body = white(n);
  biquad(body, 'bp', rand(250, 700), 3, sr);
  addInto(x, normPeak(env(body, 0.0005 * sr, tb)), 0.8);
  return biquad(x, 'hp', 120, 0.7, sr);
}

// A log settling: a dull low-mid knock followed by a little crumble of snaps.
function knockGrain(sr) {
  const tau = rand(0.025, 0.06) * sr, n = Math.ceil(Math.max(rand(0.15, 0.3) * sr, tau * 7 + 0.17 * sr)), x = new Float32Array(n), f = rand(150, 320);
  const body = white(n);
  biquad(body, 'bp', f, 3, sr); biquad(body, 'bp', f, 3, sr);
  addInto(x, normPeak(env(body, 0.0015 * sr, tau)));
  for (let k = 0, m = 3 + ((Math.random() * 6) | 0); k < m; k++) addInto(x, normPeak(snapGrain(sr)), rand(0.1, 0.3), (rand(0.01, 0.15) * sr) | 0);
  return biquad(x, 'hp', 90, 0.7, sr);
}

// Drop on wood/roof: click + three noisy resonant modes ("tok").
function woodGrain(sr) {
  const f1 = rand(260, 700), tau = rand(0.008, 0.022) * sr, n = Math.ceil(tau * 7 + 0.002 * sr), x = new Float32Array(n);
  const click = white(n);
  biquad(click, 'hp', 1500, 0.7, sr);
  addInto(x, normPeak(env(click, 0.0002 * sr, 0.0006 * sr)), 0.45);
  for (const [r, a, q] of [[1, 1, 6], [rand(1.6, 2.4), 0.5, 8], [rand(3, 4.2), 0.25, 10]]) {
    const m = white(n);
    biquad(m, 'bp', f1 * r, q, sr); biquad(m, 'bp', f1 * r, q, sr);
    addInto(x, normPeak(env(m, 0.0005 * sr, tau / Math.sqrt(r))), a);
  }
  return x;
}

// Drop on glass: a tiny click and two short high sine modes ("tik").
function glassGrain(sr) {
  const f1 = Math.min(rand(2500, 5500), 0.4 * sr), r = rand(1.35, 2.7), tau = rand(0.004, 0.014) * sr, n = Math.ceil(tau * 7), ramp = 0.0003 * sr;
  const x = new Float32Array(n), a2 = f1 * r < 0.37 * sr ? 0.5 : 0; // no partials above Nyquist (low-rate devices), even at playbackRate 1.2
  for (let i = 0; i < n; i++) {
    x[i] = Math.min(1, i / ramp) * (Math.sin((2 * Math.PI * f1 * i) / sr) * Math.exp(-i / tau)
      + a2 * Math.sin((2 * Math.PI * f1 * r * i) / sr) * Math.exp(-i / (0.6 * tau)));
  }
  const click = white(n);
  biquad(click, 'hp', 3000, 0.7, sr);
  return addInto(x, normPeak(env(click, 0.0001 * sr, 0.0003 * sr)), 0.5);
}

// Field-cricket chirp: 3-5 pulses of a ~4.5 kHz tone, each 10-16 ms.
function chirpGrain(sr, fc = 4500) {
  const np = 3 + ((Math.random() * 3) | 0), pd = rand(0.01, 0.016), gap = rand(0.007, 0.012);
  const x = new Float32Array(Math.ceil((np * (pd + gap) + 0.005) * sr));
  fc = Math.min(fc, 0.4 * sr);
  const a2 = 2 * fc < 0.45 * sr ? 0.05 : 0;
  let ph = 0;
  for (let p = 0; p < np; p++) {
    const n0 = Math.round(p * (pd + gap) * sr), n = Math.round(pd * sr), amp = (p === 0 ? 0.7 : p === np - 1 ? 0.8 : 1) * rand(0.9, 1);
    for (let i = 0; i < n; i++) {
      const u = i / n;
      ph += (2 * Math.PI * fc * (1 + 0.03 * u)) / sr;
      x[n0 + i] += amp * Math.sin(Math.PI * u) ** 2 * (Math.sin(ph) + a2 * Math.sin(2 * ph));
    }
  }
  return x;
}

// Synthetic bird call: one of a few call shapes built from smooth frequency
// contours, AM (trill) or FM (warble), with a touch of 2nd harmonic.
function birdGrain(sr) {
  const type = pick(['whistle', 'whistle', 'chirps', 'trill', 'twonote', 'warble']), segs = []; // [start, dur, f(u,t), am(u,t)]
  if (type === 'whistle') {
    const f1 = rand(2200, 4200), f2 = f1 * rand(0.7, 1.4), p = rand(0.5, 2);
    segs.push([0, rand(0.15, 0.5), (u) => f1 + (f2 - f1) * u ** p, () => 1]);
  } else if (type === 'chirps') {
    const n = 3 + ((Math.random() * 5) | 0), hi = rand(4000, 6500), lo = hi * rand(0.45, 0.7), d = rand(0.03, 0.07), gap = rand(0.04, 0.11);
    for (let k = 0; k < n; k++) segs.push([k * (d + gap), d, (u) => hi * (lo / hi) ** u, () => 1 - (0.4 * k) / n]);
  } else if (type === 'trill') {
    const fc = rand(3000, 5200), r = rand(16, 34);
    segs.push([0, rand(0.5, 1.3), (u) => fc * (1 - 0.1 * u), (u, t) => 0.5 - 0.5 * Math.cos(2 * Math.PI * r * t)]);
  } else if (type === 'twonote') {
    const fa = rand(2800, 4200), fb = fa * rand(0.75, 0.9), d = rand(0.18, 0.32), gap = rand(0.05, 0.12);
    segs.push([0, d, () => fa, () => 1], [d + gap, d * rand(1, 1.4), (u) => fb * (1 - 0.03 * u), () => 1]);
  } else {
    const fc = rand(2400, 4000), r = rand(7, 14), dep = rand(200, 500);
    segs.push([0, rand(0.5, 1), (u, t) => fc + dep * Math.sin(2 * Math.PI * r * t) + 200 * Math.sin(2 * Math.PI * 1.3 * t), () => 1]);
  }
  const total = Math.max(...segs.map(([s, d]) => s + d)) + 0.02, x = new Float32Array(Math.ceil(total * sr));
  const vib = rand(20, 40), vd = rand(0, 0.015);
  for (const [s, d, f, am] of segs) {
    const n0 = Math.round(s * sr), n = Math.round(d * sr), a = Math.min(0.012 * sr, n / 4);
    let ph = 0;
    for (let i = 0; i < n; i++) {
      const u = i / n, t = i / sr, e = Math.min(1, i / a, (n - i) / a);
      const fr = Math.min(f(u, t) * (1 + vd * Math.sin(2 * Math.PI * vib * t)), 0.4 * sr);
      ph += (2 * Math.PI * fr) / sr;
      x[n0 + i] += e * e * (3 - 2 * e) * am(u, t) * (Math.sin(ph) + (2 * fr < 0.45 * sr ? 0.12 : 0) * Math.sin(2 * ph));
    }
  }
  return x;
}

// Close lightning crack: a few-ms full-band snap, then 100-300 ms of tearing
// micro-bursts that get sparser and quieter.
function crackGrain(sr) {
  const s0 = Math.max(8, (rand(0.002, 0.005) * sr) | 0), T = rand(0.1, 0.3) * sr;
  const n = Math.ceil(T * 5 + 0.03 * sr), x = new Float32Array(n);
  for (let i = 0; i < s0 * 4; i++) x[i] += (Math.random() * 2 - 1) * Math.exp(-i / s0);
  for (let p = s0; p < n;) {
    const life = Math.exp(-p / T);
    if (life < 0.01) break;
    const len = Math.max(3, (rand(0.0002, 0.002) * sr) | 0), a = life * rand(0.15, 0.7);
    for (let i = 0; i < len && p + i < n; i++) x[p + i] += a * (Math.random() * 2 - 1) * Math.sin((Math.PI * i) / len);
    p += Math.max(1, (expRand(0.0015 / Math.max(life, 0.05)) * sr) | 0); // dense at first, then sparse
  }
  biquad(x, 'hp', 200, 0.7, sr);
  return biquad(x, 'lp', 11000, 0.7, sr);
}

// Wheel over a rail joint: click + low-mid thud + short metallic ring.
function clackGrain(sr) {
  const n = Math.ceil(0.25 * sr), x = new Float32Array(n), ft = rand(140, 240), f = rand(700, 1300);
  const click = white(n);
  biquad(click, 'hp', 1000, 0.7, sr);
  addInto(x, normPeak(env(click, 0.0002 * sr, 0.0005 * sr)), 0.5);
  const thud = white(n);
  biquad(thud, 'bp', ft, 2, sr); biquad(thud, 'bp', ft, 2, sr);
  addInto(x, normPeak(env(thud, 0.001 * sr, rand(0.025, 0.045) * sr)));
  [1, 1.47, 2.09, 2.56].forEach((r, k) => {
    const tau = rand(0.015, 0.04) * sr, a = 0.2 / Math.sqrt(k + 1);
    for (let i = 0; i < n; i++) x[i] += a * Math.sin((2 * Math.PI * f * r * i) / sr) * Math.exp(-i / tau) * Math.min(1, i / (0.0003 * sr));
  });
  return x;
}

// One leaf tick (for the rustle texture).
function tickGrain(sr) {
  const x = white(Math.max(8, (rand(0.0002, 0.002) * sr) | 0));
  env(x, 2, x.length * 0.3);
  return biquad(x, 'hp', logRand(1500, 5000), 0.7, sr);
}

const BANKS = {
  drop: [24, (sr) => dropGrain(sr)],
  bubble: [32, (sr) => bubbleGrain(sr)],
  snap: [40, snapGrain], sizzle: [8, sizzleGrain], pop: [8, popGrain], knock: [4, knockGrain],
  wood: [16, woodGrain], glass: [12, glassGrain],
  chirp: [8, (sr) => chirpGrain(sr)], bird: [20, birdGrain],
  crack: [6, crackGrain], clack: [8, clackGrain],
};
function makeBank(ctx, kind) {
  const [count, gen] = BANKS[kind], sr = ctx.sampleRate;
  return Array.from({ length: count }, () => {
    const x = normPeak(gen(sr)), b = ctx.createBuffer(1, x.length, sr);
    b.copyToChannel(x, 0);
    return b;
  });
}

// ---------------------------------------------------------------- textures (init time)
// Thousands of grains scattered over a mono loop. Grains that run past the end wrap
// to the start, so the loop is seamless by construction. No grain is much louder
// than the rest, so no single event becomes recognisable when the loop repeats.

function scatter(sr, seconds, rate, grainFn, ampFn, nGrains = 48) {
  const n = Math.round(sr * seconds), x = new Float32Array(n);
  const grains = Array.from({ length: nGrains }, () => normPeak(grainFn(sr)));
  for (let k = 0, m = Math.round(rate * seconds); k < m; k++) {
    const g = pick(grains), s = (Math.random() * n) | 0, a = ampFn(s / n);
    for (let i = 0; i < g.length; i++) x[(s + i) % n] += a * g[i];
  }
  return x;
}
const TEXTURES = {
  patter: (sr) => scatter(sr, 8, 600, (s) => dropGrain(s, 2000, 7000, 0.002, 0.008, 0.1), () => rand(0.3, 1)),
  splash: (sr) => scatter(sr, 8, 90, (s) => dropGrain(s, 500, 2500, 0.01, 0.03, 0), () => rand(0.4, 1)),
  babble: (sr) => scatter(sr, 10, 140, (s) => bubbleGrain(s, 300, 2200), () => 0.2 + 0.8 * Math.random() ** 1.5),
  chorus: (sr) => scatter(sr, 8, 25, (s) => chirpGrain(s, rand(3800, 5200)), () => rand(0.15, 0.6)),
  // stationary on purpose: a level contour baked into the loop would repeat every
  // 8 s; the swells come from runtime gain (gusts / drift) instead
  rustle: (sr) => scatter(sr, 8, 2500, tickGrain, () => Math.random() ** 2, 64),
};
function makeTexture(ctx, kind) {
  const x = normRms(TEXTURES[kind](ctx.sampleRate)), b = ctx.createBuffer(1, x.length, ctx.sampleRate);
  b.copyToChannel(x, 0);
  return b;
}

// ---------------------------------------------------------------- voice

// One playing sound: owns its persistent nodes, its in-flight one-shots and its
// event streams, so dispose() can release everything.
class Voice {
  constructor(ctx, dest, t0) {
    this.ctx = ctx; this.t0 = t0;
    this.out = ctx.createGain(); // fade gain
    this.out.gain.value = 0;
    this.out.connect(dest);
    this.nodes = [this.out];     // persistent nodes
    this.live = new Set();       // in-flight one-shots: arrays of nodes
    this.streams = [];           // { next, fn } -> fn(t) schedules an event at t, returns delay to the next
    this.density = 1;            // slowly wandering event-rate multiplier
    this.shots = 0;              // one-shots started (for measuring event rates)
    this.dead = false;
  }
  add(n) { this.nodes.push(n); return n; }
  gain(v) { const g = this.add(this.ctx.createGain()); g.gain.value = v; return g; }
  filter(type, f, Q) { return this.add(this.tmpFilter(type, f, Q)); }
  panner(p = 0) { const s = this.add(this.ctx.createStereoPanner()); s.pan.value = p; return s; }
  osc(type, f) { const o = this.add(this.ctx.createOscillator()); o.type = type; o.frequency.value = f; o.start(this.t0); return o; }
  panBuses(dest, width, n = 6) {
    return Array.from({ length: n }, (_, i) => { const p = this.panner(-width + (2 * width * i) / (n - 1)); p.connect(dest); return p; });
  }

  // Continuous noise bed: two copies of a loop at random offsets and slightly
  // different rates. Their sum never lines up the same way twice, so the loop
  // period can't be heard; the 4-9% pitch difference is inaudible on noise.
  bed(kind) {
    const buf = noise(this.ctx, kind), g = this.gain(Math.SQRT1_2);
    for (const rate of [1, rand(0.91, 0.96)]) {
      const s = this.add(this.ctx.createBufferSource());
      s.buffer = buf; s.loop = true; s.playbackRate.value = rate;
      s.connect(g);
      s.start(this.t0, rand(0, buf.duration));
    }
    return g;
  }
  // Same idea for a mono texture, the two copies panned apart a little for width
  // (not hard-panned: each ear must hear a mix of both, or it hears one loop period).
  texture(kind, width = 0.35) {
    const buf = textureBuf(this.ctx, kind), g = this.gain(Math.SQRT1_2);
    for (const [rate, p] of [[1, -width], [rand(0.9, 0.95), width]]) {
      const s = this.add(this.ctx.createBufferSource());
      s.buffer = buf; s.loop = true; s.playbackRate.value = rate;
      chain(s, this.panner(p), g);
      s.start(this.t0, rand(0, buf.duration));
    }
    return g;
  }

  // One-shot source -> nodes... -> dest. Its nodes (plus `extra`) are released when it ends.
  shot(buf, t, nodes, dest, { rate = 1, offset = 0, dur = 0, loop = false, extra = [] } = {}) {
    if (this.dead) return;
    const s = this.ctx.createBufferSource();
    s.buffer = buf; s.playbackRate.value = rate; s.loop = loop;
    chain(s, ...nodes).connect(dest);
    this.track(s, [s, ...nodes, ...extra]);
    if (loop) { s.start(t, offset); s.stop(t + dur); }
    else if (dur) s.start(t, offset, dur);
    else s.start(t);
  }
  // One looping noise source feeding several parallel branches (thunder bands).
  burst(buf, t, dur, branches, dest, extra = []) {
    if (this.dead) return;
    const s = this.ctx.createBufferSource(), rec = [s, ...extra];
    s.buffer = buf; s.loop = true;
    for (const br of branches) { chain(s, ...br).connect(dest); rec.push(...br); }
    this.track(s, rec);
    s.start(t, rand(0, buf.duration));
    s.stop(t + dur);
  }
  track(s, rec) {
    this.live.add(rec);
    this.shots++;
    s.onended = () => { this.live.delete(rec); for (const n of rec) n.disconnect(); };
  }
  tmpGain(v) { const g = this.ctx.createGain(); g.gain.value = v; return g; }
  tmpFilter(type, f, Q = 0.707) {
    const b = this.ctx.createBiquadFilter();
    b.type = type; b.frequency.value = f; b.Q.value = Q;
    // While a-rate params are automated, Chromium recomputes the coefficients for
    // every sample; once per 128-sample block is plenty for slow sweeps.
    try { b.frequency.automationRate = 'k-rate'; b.Q.automationRate = 'k-rate'; } catch {}
    return b;
  }

  every(first, fn) { this.streams.push({ next: this.t0 + first, fn }); }
  // Slow random movement: glide toward a new random target every few seconds.
  // Costs no nodes and never repeats (unlike an LFO).
  drift(param, lo, hi, tMin, tMax) {
    this.every(0, (t) => { const T = rand(tMin, tMax); param.setTargetAtTime(rand(lo, hi), t, T / 3); return T; });
  }
  // Schedule every stream's events up to `until`. If the timer fell behind,
  // skip the missed span instead of firing a burst of overdue events.
  tick(until, now = 0) {
    if (this.dead) return;
    for (const s of this.streams) {
      if (s.next < now) s.next = now + 0.02;
      while (s.next < until) s.next += Math.max(0.003, s.fn(s.next));
    }
  }
  fadeIn(dur) {
    if (dur > 0) this.out.gain.setValueCurveAtTime(EQ_IN, this.t0, dur);
    else this.out.gain.setValueAtTime(1, this.t0);
  }
  fadeOut(dur) {
    const g = this.out.gain, now = this.ctx.currentTime, h = g.value;
    if (g.cancelAndHoldAtTime) g.cancelAndHoldAtTime(now); // also truncates a running fade-in
    else { g.cancelScheduledValues(now); g.setValueAtTime(h, now); }
    if (dur > 0) g.setValueCurveAtTime(EQ_OUT.map((c) => c * h), now + 0.01, dur);
    else g.setValueAtTime(0, now + 0.01);
  }
  dispose() {
    if (this.dead) return;
    this.dead = true;
    this.streams.length = 0;
    for (const rec of this.live) for (const n of rec) { try { n.stop?.(); } catch {} n.disconnect(); }
    this.live.clear();
    for (const n of this.nodes) { try { n.stop?.(); } catch {} n.disconnect(); }
    this.nodes.length = 0;
  }
}

// ---------------------------------------------------------------- layers

// Rain = soft pink hiss (far away) + granular patter texture (thousands of small
// drops: it is what keeps rain from sounding like filtered static) + optional
// coarser splashes and low roar + individual close drops. Returns its bus gain
// (used for thunder ducking and for coupling to wind gusts).
function rainLayer(v, dest, c) {
  const bus = v.gain(1);
  bus.connect(dest);
  const hiss = v.gain(c.hiss), hlp = v.filter('lowpass', c.lp, 0.5);
  chain(v.bed('pink'), v.filter('highpass', c.hp, 0.5), hlp, hiss, bus);
  v.drift(hlp.frequency, c.lp * 0.8, c.lp * 1.1, 4, 11);
  const pat = v.gain(c.patter);
  chain(v.texture('patter'), v.filter('highpass', 500, 0.5), v.filter('lowpass', c.lp * 1.2, 0.5), pat, bus);
  let spl = null;
  if (c.splash) { spl = v.gain(c.splash); chain(v.texture('splash'), v.filter('bandpass', 1100, 0.6), spl, bus); }
  if (c.low) {
    // downpour: the body of water hitting roofs and ground, kept above ~100 Hz
    const lg = v.gain(c.low);
    chain(v.bed('brown'), v.filter('highpass', 110, 0.6), v.filter('lowpass', 520, 0.6), lg, bus);
    v.drift(lg.gain, c.low * 0.75, c.low * 1.15, 4, 10);
  }
  // intensity wanders: hiss, patter, splash and the drop rate move together
  v.every(0, (t) => {
    v.density = rand(0.7, 1.3);
    const T = rand(3, 9), k = Math.sqrt(v.density);
    hiss.gain.setTargetAtTime(c.hiss * Math.sqrt(k), t, T / 3);
    pat.gain.setTargetAtTime(c.patter * k, t, T / 3);
    spl?.gain.setTargetAtTime(c.splash * k, t, T / 3);
    return T;
  });
  const drops = bank(v.ctx, 'drop'), buses = v.panBuses(bus, 0.9);
  v.every(0.1, (t) => {
    // mostly quiet drops, an occasional close one (cubed uniform)
    v.shot(pick(drops), t, [v.tmpGain(c.drop * (0.12 + 0.88 * Math.random() ** 3))], pick(buses), { rate: rand(c.pitch[0], c.pitch[1]) });
    return expRand(1 / (c.rate * v.density));
  });
  return bus;
}

// Wind: a broad body band, a higher "rushing air" band, an optional narrow
// whistle (only in strong gusts) and optional leaf rustle, all driven by a gust
// process: irregular gusts that rise faster than they fall, with occasional lulls.
function windLayer(v, dest, c) {
  const bf = v.filter('bandpass', 400, 0.7), bg = v.gain(0), bp = v.panner(0);
  chain(v.bed('pink'), v.filter('highpass', 130, 0.6), bf, bg, bp, dest);
  const af = v.filter('bandpass', 1500, 0.6), ag = v.gain(0), ap = v.panner(0);
  chain(v.bed('pink'), af, ag, ap, dest);
  let wf = null, wg = null, rg = null;
  if (c.whistle) {
    wf = v.filter('bandpass', 900, 16); wg = v.gain(0);
    chain(v.bed('white'), wf, wg, v.panner(rand(-0.5, 0.5)), dest);
    v.drift(wf.Q, 12, 22, 3, 8);
  }
  if (c.rustle) { rg = v.gain(0); chain(v.texture('rustle'), v.filter('highpass', 1200, 0.6), rg, dest); }
  let raw = 0.5, lull = 0;
  v.every(0, (t) => {
    const prev = raw;
    if (lull > 0) { lull--; raw = rand(0.05, 0.2); }
    else if (Math.random() < 0.1) { lull = 1 + ((Math.random() * 2) | 0); raw = rand(0.05, 0.2); }
    else raw = clamp(0.5 * raw + 0.5 * Math.random() ** 0.8 + (Math.random() < 0.15 ? 0.35 : 0), 0.1, 1);
    // c.floor keeps a gentle background breeze (forest) from dropping out in lulls
    const g = c.floor ? c.floor + (1 - c.floor) * raw : raw, tau = raw > prev ? rand(0.5, 1.6) : rand(1.4, 3.2);
    bg.gain.setTargetAtTime(c.body * (0.2 + 0.8 * g), t, tau);
    bf.frequency.setTargetAtTime(250 + 450 * g + rand(-40, 40), t, tau * 1.3);
    ag.gain.setTargetAtTime(c.air * g ** 1.6, t, tau);
    af.frequency.setTargetAtTime(1100 + 1400 * g, t, tau * 1.3);
    if (wg) {
      const on = g > 0.62 && Math.random() < 0.7;
      wg.gain.setTargetAtTime(on ? c.whistle * (g - 0.55) * 2.2 : 0, t, tau * 1.2);
      wf.frequency.setTargetAtTime(rand(650, 1000) + 700 * g, t, tau * 1.6);
    }
    rg?.gain.setTargetAtTime(c.rustle * (0.15 + g * g), t + 0.3, tau * 1.2); // leaves answer a moment later
    bp.pan.setTargetAtTime(rand(-0.5, 0.5), t, tau * 2);
    ap.pan.setTargetAtTime(rand(-0.6, 0.6), t, tau * 2);
    c.onGust?.(g, t, tau);
    return rand(1.5, 6);
  });
}

// Thunderstorm: first strike within 3-7 s, then every 12-35 s. Close strikes have a
// bright crack and duck the rain for a moment; distant ones are slower, darker rolls.
function thunderLayer(v, dest, c) {
  const bus = v.gain(c.level);
  bus.connect(dest);
  let n = 0;
  v.every(c.first ?? rand(3, 7), (t) => {
    // the first strike is usually a close one, so the storm announces itself
    const first = n++ === 0, close = first && c.force ? c.force === 'close' : Math.random() < (first ? 0.7 : 0.4);
    thunder(v, bus, t, close, c.duck);
    return rand(12, 35);
  });
}
function thunder(v, bus, t, close, duck) {
  const ctx = v.ctx, pan = ctx.createStereoPanner(), p0 = close ? rand(-0.4, 0.4) : rand(-0.8, 0.8);
  pan.connect(bus);
  const peak = close ? rand(0.8, 1) : rand(0.32, 0.55);
  let t1 = t + rand(0.3, 1.2); // distant: no crack, the roll just builds
  if (close) {
    v.shot(pick(bank(ctx, 'crack')), t, [v.tmpGain(0.15 * rand(0.75, 1))], pan);
    t1 = t + rand(0.01, 0.05);
    if (duck) { duck.gain.setTargetAtTime(0.6, t, 0.05); duck.gain.setTargetAtTime(1, t + rand(1.5, 2.5), 1.5); }
  } else if (Math.random() < 0.3) {
    // a far crack, heavily muffled
    v.shot(pick(bank(ctx, 'crack')), t, [v.tmpFilter('lowpass', 1200), v.tmpGain(0.12)], pan);
  }
  // Rumble: three noise bands with staggered envelopes (high band first, low band
  // last and longest), irregular "rolling" level steps every 0.12-0.45 s, and a
  // few echo-like re-swells shared by all bands.
  const bands = close
    ? [['bandpass', rand(800, 1200), 0.7, 0.9, 0.05, rand(0.6, 1.2)], // [type, f, Q, level, rise s, decay s]
      ['bandpass', rand(320, 450), 0.8, 1.3, rand(0.15, 0.35), rand(1.5, 2.8)],
      ['lowpass', rand(180, 240), 0.7, 0.5, rand(0.4, 0.8), rand(2.5, 4)]]
    : [['bandpass', rand(700, 1000), 0.7, 0.5, rand(0.6, 1.2), rand(1, 2)],
      ['bandpass', rand(280, 400), 0.8, 1.2, rand(0.8, 1.8), rand(2, 3.5)],
      ['lowpass', rand(160, 220), 0.7, 0.6, rand(1, 2), rand(3, 5)]];
  const swells = [];
  for (let k = 0, m = ((Math.random() * 3) | 0) + (close ? 1 : 0); k < m; k++) swells.push([rand(1.5, 5), rand(0.35, 0.8), rand(0.5, 1.4)]); // [delay, height, width]
  let end = 0;
  const branches = bands.map(([type, f, q, lvl, rise, decay]) => {
    const fl = v.tmpFilter(type, f, q), g = v.tmpGain(0);
    // re-swells combine by max, so overlapping ones never stack above the onset
    const shape = (x) => Math.max(x < rise ? (x / rise) ** 1.5 : Math.exp(-(x - rise) / decay),
      ...swells.map(([d, h, w]) => h * Math.exp(-(((x - d - rise) / w) ** 2))));
    const len = rise + decay * 2.5 + (swells.length ? Math.max(...swells.map((s) => s[0] + s[2])) : 0);
    let x = 0;
    g.gain.setValueAtTime(0, t1);
    while (x < len) {
      const dx = x < rise ? Math.max(0.02, rise / 3) : rand(0.12, 0.45);
      x += dx;
      g.gain.setTargetAtTime(peak * lvl * shape(x) * (x < rise ? 1 : rand(0.45, 1)), t1 + x - dx, Math.min(rand(0.06, 0.18), dx / 2));
    }
    const tail = rand(0.8, 1.5);
    g.gain.setTargetAtTime(0, t1 + x, tail);
    end = Math.max(end, x + tail * 6);
    // the low band would otherwise carry sub-bass laptops can't play, which only
    // makes the compressor pull the audible part down
    return type === 'lowpass' ? [v.tmpFilter('highpass', 60, 0.7), fl, g] : [fl, g];
  });
  pan.pan.setValueAtTime(p0, t);
  pan.pan.linearRampToValueAtTime(clamp(p0 + rand(-0.4, 0.4), -0.9, 0.9), t1 + end);
  v.burst(noise(ctx, 'pink'), t1, end, branches, pan, [pan]);
}

// Surf: a distant-surf bed plus individual waves; waves come in sets of bigger
// ones separated by calmer spells, at irregular intervals.
function wavesLayer(v, dest) {
  const surf = v.gain(0.5), sf = v.filter('bandpass', 500, 0.5);
  chain(v.bed('pink'), sf, surf, dest);
  v.drift(surf.gain, 0.35, 0.6, 3, 9);
  v.drift(sf.frequency, 350, 800, 5, 12);
  const low = v.gain(0.35);
  chain(v.bed('brown'), v.filter('highpass', 70, 0.6), v.filter('lowpass', 300, 0.6), low, dest);
  v.drift(low.gain, 0.25, 0.42, 4, 10);
  let big = true, left = 3;
  v.every(rand(0.3, 1.5), (t) => {
    if (left-- <= 0) { big = !big; left = big ? 3 + ((Math.random() * 4) | 0) : 2 + ((Math.random() * 3) | 0); }
    wave(v, dest, t, big ? rand(0.7, 1) : rand(0.35, 0.65));
    return (big ? rand(6.5, 10) : rand(8.5, 13)) * (Math.random() < 0.12 ? 0.6 : 1);
  });
}
function wave(v, dest, t, size) {
  const ctx = v.ctx, rise = rand(2.5, 4.5), crest = t + rise, wash = rand(1.3, 2.2), end = crest + wash * 5;
  const pan = ctx.createStereoPanner(), p = rand(-0.45, 0.45);
  pan.pan.setValueAtTime(p, t);
  pan.pan.linearRampToValueAtTime(clamp(p + rand(-0.35, 0.35), -0.8, 0.8), end);
  // body: an accelerating swell (exponential ramp = steady crescendo in dB) whose
  // low-pass opens as the wave rises and breaks, then closes as it washes out
  const lp = v.tmpFilter('lowpass', 250, 0.5), g = v.tmpGain(0);
  lp.frequency.setValueAtTime(250, t);
  lp.frequency.exponentialRampToValueAtTime(rand(1400, 2600) * size, crest);
  lp.frequency.setTargetAtTime(350, crest + 0.3, wash);
  g.gain.setValueAtTime(0.015 * size, t);
  g.gain.exponentialRampToValueAtTime(0.75 * size, crest);
  g.gain.setTargetAtTime(0, crest + 0.15, wash);
  v.shot(noise(ctx, 'pink'), t, [v.tmpFilter('highpass', 90, 0.6), lp, g, pan], dest, { loop: true, offset: rand(0, 8), dur: end - t });
  // foam: fizzing of countless tiny bubbles (patter texture) plus some hiss,
  // swelling at the break and running up the beach
  const fg = v.tmpGain(0);
  fg.connect(pan);
  fg.gain.setValueAtTime(0, crest - 0.5);
  fg.gain.linearRampToValueAtTime(0.5 * size, crest + 0.3);
  fg.gain.setTargetAtTime(0, crest + 0.4, wash * 0.8);
  const fh = v.tmpFilter('highpass', rand(1500, 2500), 0.5);
  fh.frequency.setTargetAtTime(fh.frequency.value * 0.6, crest + 0.4, wash);
  v.shot(textureBuf(ctx, 'patter'), crest - 0.5, [fh], fg, { loop: true, offset: rand(0, 8), dur: end - crest + 0.5, extra: [fg] });
  v.shot(noise(ctx, 'white'), crest - 0.5, [v.tmpFilter('highpass', 2500, 0.5), v.tmpGain(0.25)], fg, { loop: true, offset: rand(0, 8), dur: end - crest + 0.5 });
  // soft low-mid thump of the break
  const tg = v.tmpGain(0);
  tg.gain.setValueAtTime(0, crest - 0.3);
  tg.gain.linearRampToValueAtTime(0.45 * size, crest + 0.1);
  tg.gain.setTargetAtTime(0, crest + 0.15, 0.7);
  v.shot(noise(ctx, 'brown'), crest - 0.3, [v.tmpFilter('bandpass', 170, 0.8), tg], pan, { loop: true, offset: rand(0, 8), dur: 5 });
}

// Fireplace: mostly crackles (irregular clusters of short broadband snaps of
// varying size and brightness), small sizzles, rare pops and settling knocks,
// over a soft airy breath of the flames in the mids (no low rumble).
function fireLayer(v, dest) {
  const ctx = v.ctx;
  const roar = v.gain(0.6), rlp = v.filter('lowpass', 1400, 0.5);
  chain(v.bed('pink'), v.filter('highpass', 220, 0.6), rlp, roar, dest);
  const air = v.gain(0.1);
  chain(v.bed('white'), v.filter('bandpass', 3500, 0.6), air, dest);
  // breathing: one random intensity drives the level and brightness together
  v.every(0, (t) => {
    const k = rand(0.5, 1), T = rand(0.6, 3);
    roar.gain.setTargetAtTime(0.6 * k, t, T / 3);
    rlp.frequency.setTargetAtTime(900 + 1100 * k, t, T / 3);
    air.gain.setTargetAtTime(0.1 * k * k, t, T / 3);
    return T;
  });
  // crackle activity alternates between calm and lively spells
  v.every(0, () => { v.density = Math.random() < 0.3 ? rand(0.3, 0.6) : rand(0.8, 1.8); return rand(2, 8); });
  const snaps = bank(ctx, 'snap'), sizzles = bank(ctx, 'sizzle'), pops = bank(ctx, 'pop'), knocks = bank(ctx, 'knock');
  const buses = v.panBuses(dest, 0.6);
  v.every(0.2, (t) => {
    // a single snap or a cluster (geometric-ish size); event size is heavy-tailed
    const bus = pick(buses), n = Math.random() < 0.6 ? 1 : Math.min(14, 2 + (expRand(2.5) | 0));
    let tt = t, a = 0.7 * (0.08 + 0.9 * Math.random() ** 1.8);
    for (let k = 0; k < n; k++) {
      v.shot(pick(snaps), tt, [v.tmpGain(a * rand(0.3, 1))], bus, { rate: rand(0.7, 1.4) });
      tt += expRand(0.025);
      a *= rand(0.6, 1.05);
    }
    return expRand(0.28 / v.density);
  });
  v.every(rand(1, 3), (t) => {
    v.shot(pick(sizzles), t, [v.tmpGain(rand(0.08, 0.25))], pick(buses), { rate: rand(0.8, 1.25) });
    return 0.3 + expRand(2.5 / v.density);
  });
  v.every(rand(3, 8), (t) => { v.shot(pick(pops), t, [v.tmpGain(rand(0.4, 0.7))], pick(buses), { rate: rand(0.85, 1.15) }); return rand(5, 20); });
  v.every(rand(10, 30), (t) => { v.shot(pick(knocks), t, [v.tmpGain(rand(0.3, 0.6))], pick(buses), { rate: rand(0.85, 1.15) }); return rand(15, 50); });
}

// Brook: mid-band water wash + dense far babble (bubble texture) + fine spray +
// distinct close bubbles that come in spurts.
function streamLayer(v, dest) {
  const wash = v.gain(0.5), wf = v.filter('bandpass', 1200, 0.5);
  chain(v.bed('pink'), v.filter('highpass', 180, 0.6), wf, wash, dest);
  v.drift(wf.frequency, 800, 1800, 2, 6);
  v.drift(wash.gain, 0.4, 0.6, 1.5, 5);
  const bab = v.gain(0.7);
  chain(v.texture('babble'), v.filter('highpass', 250, 0.6), bab, dest);
  v.drift(bab.gain, 0.5, 0.85, 1, 4);
  const fizz = v.gain(0.15);
  chain(v.texture('patter'), v.filter('highpass', 2500, 0.6), fizz, dest);
  const bubbles = bank(v.ctx, 'bubble'), buses = v.panBuses(dest, 0.8);
  v.every(0, () => { v.density = rand(0.5, 1.6); return rand(1, 4); });
  v.every(0.1, (t) => {
    v.shot(pick(bubbles), t, [v.tmpGain(0.5 * (0.15 + 0.85 * Math.random() ** 2.5))], pick(buses), { rate: rand(0.8, 1.25) });
    return expRand(1 / (14 * v.density));
  });
}

// Night: a few crickets (each its own pitch, tempo, position, with pauses), a
// faint distant chorus texture, very soft wind and leaves.
function nightLayer(v, dest) {
  const wind = v.gain(0.12), wf = v.filter('bandpass', 380, 0.6);
  chain(v.bed('pink'), v.filter('highpass', 150, 0.6), wf, wind, dest);
  v.drift(wind.gain, 0.06, 0.16, 3, 10);
  v.drift(wf.frequency, 300, 600, 4, 12);
  const rus = v.gain(0.04);
  chain(v.texture('rustle'), rus, dest);
  v.drift(rus.gain, 0.01, 0.06, 2, 8);
  const cho = v.gain(0.25);
  chain(v.texture('chorus'), v.filter('lowpass', 6000, 0.5), cho, dest);
  v.drift(cho.gain, 0.18, 0.3, 4, 12);
  const chirps = bank(v.ctx, 'chirp');
  for (let i = 0, n = 3 + ((Math.random() * 2) | 0); i < n; i++) {
    const pan = v.panner(rand(-0.85, 0.85)), voice = pick(chirps), rate = rand(0.9, 1.12), level = rand(0.4, 0.8);
    pan.connect(dest);
    let period = rand(0.32, 0.7), left = rand(8, 40) | 0;
    v.every(rand(0, 2), (t) => {
      if (left-- <= 0) { // pause, then resume at a slightly different tempo
        left = rand(8, 60) | 0;
        period = clamp(period * rand(0.94, 1.06), 0.3, 0.8);
        return rand(0.8, 7);
      }
      v.shot(voice, t, [v.tmpGain(level * rand(0.8, 1))], pan, { rate });
      return period * rand(0.95, 1.05);
    });
  }
}

// Forest by day: soft wind in leaves + 3-4 birds, each with a small repertoire,
// singing short phrases (a call repeated 1-4 times) separated by silences.
function forestLayer(v, dest) {
  windLayer(v, dest, { body: 0.4, air: 0.1, rustle: 0.3, floor: 0.4 });
  const calls = bank(v.ctx, 'bird'), far = v.filter('lowpass', 3500, 0.5);
  far.connect(dest);
  for (let i = 0, n = 3 + ((Math.random() * 2) | 0); i < n; i++) {
    const near = Math.random() < 0.5, pan = v.panner(rand(-0.85, 0.85));
    pan.connect(near ? dest : far);
    const rep = [pick(calls), pick(calls)], level = near ? rand(0.09, 0.18) : rand(0.04, 0.09), rate = rand(0.92, 1.08);
    let left = 1 + ((Math.random() * 4) | 0), call = pick(rep);
    v.every(rand(0.5, 8), (t) => {
      v.shot(call, t, [v.tmpGain(level * rand(0.8, 1))], pan, { rate: rate * rand(0.98, 1.02) });
      if (--left > 0) return call.duration / rate + rand(0.15, 0.9);
      left = 1 + ((Math.random() * 4) | 0);
      const d = call.duration / rate;
      call = pick(rep);
      return d + rand(4, 20);
    });
  }
}

// Plain noise colours, with a barely-there level drift.
function noiseLayer(v, dest, kind, filters) {
  const g = v.gain(1);
  chain(v.bed(kind), ...filters.map(([type, f, q]) => v.filter(type, f, q)), g, dest);
  v.drift(g.gain, 0.94, 1.03, 5, 12);
}

// Desk fan: broadband airflow with a slight blade-rate flutter, a soft air hiss
// and a motor hum (fundamental + harmonics, which carry it on small speakers)
// whose speed wobbles slightly.
function fanLayer(v, dest) {
  const air = v.gain(0.8), alp = v.filter('lowpass', 3500, 0.5);
  chain(v.bed('pink'), v.filter('highpass', 140, 0.6), alp, air, dest);
  v.drift(alp.frequency, 3200, 3800, 6, 15);
  const hiss = v.gain(0.1);
  chain(v.bed('white'), v.filter('bandpass', 2200, 0.5), hiss, dest);
  const fl = rand(12, 17), lfo = v.osc('sine', fl);
  chain(lfo, v.gain(0.04)).connect(air.gain); // +-5% amplitude flutter
  v.drift(lfo.frequency, fl * 0.97, fl * 1.03, 5, 12);
  const f0 = rand(95, 118), hum = v.gain(1);
  hum.connect(dest);
  const oscs = [[1, 0.025], [2, 0.018], [3, 0.01]].map(([h, a]) => { const o = v.osc('sine', f0 * h); chain(o, v.gain(a), hum); return o; });
  v.every(0, (t) => { const cents = rand(-10, 10), T = rand(3, 9); for (const o of oscs) o.detune.setTargetAtTime(cents, t, T / 3); return T; });
  v.drift(hum.gain, 0.8, 1.1, 2, 6);
}

// Inside a train: rolling rumble in three bands + wheel clacks at rail joints.
// Each 25 m joint is hit by the two axles of the near bogie, then (17.5 m later)
// the two axles of the far one: "ta-dam ... ta-dam". Speed drifts slowly
// (16-32 m/s) and the rumble follows it.
function trainLayer(v, dest) {
  const lowG = v.gain(0.5), midG = v.gain(0.3), hiG = v.gain(0.05), mf = v.filter('bandpass', 450, 0.6);
  chain(v.bed('brown'), v.filter('highpass', 45, 0.6), v.filter('lowpass', 280, 0.6), lowG, dest);
  chain(v.bed('pink'), v.filter('highpass', 150, 0.6), mf, midG, dest);
  chain(v.bed('pink'), v.filter('bandpass', 2000, 0.7), hiG, dest);
  v.drift(lowG.gain, 0.42, 0.58, 1, 4); // car sway
  const clacks = bank(v.ctx, 'clack'), nearB = v.filter('lowpass', 3200, 0.6), farB = v.filter('lowpass', 1800, 0.6);
  nearB.connect(dest); farB.connect(dest);
  const hit = (t, bus, a) => v.shot(pick(clacks), t + rand(0, 0.006), [v.tmpGain(a * rand(0.85, 1.15))], bus, { rate: rand(0.94, 1.06) });
  let speed = rand(20, 28), target = speed, retarget = 0;
  v.every(0.5, (t) => {
    if (--retarget <= 0) { target = rand(16, 32); retarget = rand(20, 60) | 0; } // counted in joints
    speed += (target - speed) * 0.03;
    const s = speed / 25, a = 2.6 / speed, b = 17.5 / speed;
    hit(t, nearB, 0.9 * s); hit(t + a, nearB, 0.75 * s);
    hit(t + b, farB, 0.4 * s); hit(t + b + a, farB, 0.32 * s);
    if (Math.random() < 0.01) for (let k = 0; k < 6; k++) hit(t + rand(0.1, 0.8), pick([nearB, farB]), rand(0.2, 0.5) * s); // points
    midG.gain.setTargetAtTime(0.3 * s ** 1.5, t, 2);
    mf.frequency.setTargetAtTime(350 + 150 * s, t, 2);
    hiG.gain.setTargetAtTime(0.05 * s * s, t, 2);
    return 25 / speed;
  });
}

// Rain on a roof / window: rain bed + dense close taps on wood and glass + a gutter
// off to one side (running water and bubbles) + a slow, uneven drip.
function roofLayer(v, dest) {
  rainLayer(v, dest, { hiss: 0.35, hp: 500, lp: 5000, patter: 0.5, splash: 0.5, rate: 6, drop: 0.25, pitch: [0.8, 1.1] });
  const ctx = v.ctx, wood = bank(ctx, 'wood'), glass = bank(ctx, 'glass'), bubbles = bank(ctx, 'bubble'), buses = v.panBuses(dest, 0.8);
  v.every(0.2, (t) => {
    const isWood = Math.random() < 0.65;
    v.shot(pick(isWood ? wood : glass), t, [v.tmpGain((isWood ? 0.5 : 0.3) * (0.15 + 0.85 * Math.random() ** 2.5))], pick(buses), { rate: rand(0.85, 1.2) });
    return expRand(1 / (20 * v.density));
  });
  const side = Math.random() < 0.5 ? -1 : 1, gp = v.panner(side * rand(0.45, 0.75)), glp = v.filter('lowpass', 2800, 0.6);
  chain(glp, gp, dest);
  const run = v.gain(0.12), rf = v.filter('bandpass', 1300, 1.2);
  chain(v.bed('pink'), rf, run, glp);
  v.drift(run.gain, 0.06, 0.16, 1, 4);
  v.drift(rf.frequency, 900, 1700, 1, 4);
  let flow = 1;
  v.every(0, () => { flow = rand(0.4, 1.6); return rand(1, 4); });
  v.every(0.3, (t) => { v.shot(pick(bubbles), t, [v.tmpGain(0.3 * rand(0.3, 1))], glp, { rate: rand(0.9, 1.4) }); return expRand(1 / (10 * flow)); });
  const dp = v.panner(-side * rand(0.3, 0.6));
  dp.connect(dest);
  let period = rand(0.6, 1.3);
  v.every(1, (t) => {
    v.shot(pick(bubbles), t, [v.tmpGain(rand(0.3, 0.45))], dp, { rate: rand(0.95, 1.05) });
    period = clamp(period * rand(0.93, 1.07), 0.45, 1.6);
    return period * rand(0.8, 1.2);
  });
}

const RAIN = {
  light: { hiss: 0.35, hp: 600, lp: 5000, patter: 0.3, rate: 6, drop: 0.3, pitch: [0.85, 1.2] },
  mid: { hiss: 0.45, hp: 350, lp: 7000, patter: 0.6, splash: 0.15, rate: 14, drop: 0.28, pitch: [0.75, 1.15] },
  heavy: { hiss: 0.6, hp: 200, lp: 9000, patter: 0.9, splash: 0.5, low: 0.5, rate: 28, drop: 0.28, pitch: [0.6, 1] },
  storm: { hiss: 0.55, hp: 250, lp: 8000, patter: 0.8, splash: 0.35, low: 0.25, rate: 22, drop: 0.28, pitch: [0.65, 1.05] },
};
const BUILDERS = {
  'rain-light': (v, d) => rainLayer(v, d, RAIN.light),
  rain: (v, d) => rainLayer(v, d, RAIN.mid),
  'rain-heavy': (v, d) => rainLayer(v, d, RAIN.heavy),
  'rain-wind': (v, d) => {
    const bus = rainLayer(v, d, RAIN.mid);
    windLayer(v, d, { body: 0.8, air: 0.25, whistle: 4, onGust: (g, t, tau) => bus.gain.setTargetAtTime(0.85 + 0.3 * g, t, tau) });
  },
  'rain-thunder': (v, d, o) => {
    const bus = rainLayer(v, d, RAIN.storm);
    thunderLayer(v, d, { level: 7, duck: bus, first: o.firstThunder, force: o.thunder });
  },
  'rain-roof': (v, d) => roofLayer(v, d),
  waves: (v, d) => wavesLayer(v, d),
  fire: (v, d) => fireLayer(v, d),
  wind: (v, d) => windLayer(v, d, { body: 1, air: 0.35, whistle: 4, rustle: 0.5 }),
  stream: (v, d) => streamLayer(v, d),
  night: (v, d) => nightLayer(v, d),
  forest: (v, d) => forestLayer(v, d),
  white: (v, d) => noiseLayer(v, d, 'white', [['lowpass', 15000, 0.5]]),
  pink: (v, d) => noiseLayer(v, d, 'pink', [['highpass', 25, 0.6]]),
  brown: (v, d) => {
    const lp = v.filter('lowpass', 800, 0.5), g = v.gain(1);
    chain(v.bed('brown'), v.filter('highpass', 30), lp, g, d);
    v.drift(lp.frequency, 600, 1000, 6, 14);
    v.drift(g.gain, 0.9, 1.05, 5, 12);
  },
  fan: (v, d) => fanLayer(v, d),
  train: (v, d) => trainLayer(v, d),
};

// ---------------------------------------------------------------- graph builders
// These work on any BaseAudioContext (live or OfflineAudioContext), which is how
// test.html?measure renders and measures each sound.

// Master chain: sounds + chimes -> volume -> gentle compressor/limiter -> dest.
export function buildMaster(ctx, { compressor = true, dest = ctx.destination } = {}) {
  const sounds = ctx.createGain(), chimes = ctx.createGain(), volume = ctx.createGain();
  chimes.gain.value = CHIME_LEVEL;
  volume.gain.value = volumeCurve(DEFAULT_VOLUME);
  sounds.connect(volume); chimes.connect(volume);
  const nodes = [sounds, chimes, volume];
  if (compressor) {
    const comp = ctx.createDynamicsCompressor();
    comp.threshold.value = -10; comp.knee.value = 8; comp.ratio.value = 6;
    comp.attack.value = 0.004; comp.release.value = 0.3;
    // Chromium adds automatic makeup gain (~+4 dB here); trim the output back so
    // transients (crackles, wave crests, thunder) stay a few dB under full scale.
    // A fast second stage catches what the 4 ms attack lets through (crack and
    // crackle transients): hard knee, ratio 20, ~1 ms attack.
    const lim = ctx.createDynamicsCompressor();
    lim.threshold.value = -3; lim.knee.value = 0; lim.ratio.value = 20;
    lim.attack.value = 0.001; lim.release.value = 0.1;
    const post = ctx.createGain();
    post.gain.value = 0.8;
    volume.connect(comp).connect(lim).connect(post).connect(dest);
    nodes.push(comp, lim, post);
  } else volume.connect(dest);
  return { sounds, chimes, volume, nodes };
}

// Builds a sound's graph starting at t0 (with a fade-in) and returns its Voice.
// The caller drives it with voice.tick(until, now).
// Test-only options: firstThunder (s after t0), thunder ('close' | 'far' for the first strike).
export function buildSound(ctx, id, dest, { t0 = ctx.currentTime, fadeIn = FADE, firstThunder, thunder } = {}) {
  const build = BUILDERS[id];
  if (!build) throw new Error('Unknown sound: ' + id);
  const v = new Voice(ctx, dest, t0);
  v.fadeIn(fadeIn);
  const trim = v.gain(TRIM[id]);
  trim.connect(v.out);
  build(v, trim, { firstThunder, thunder });
  return v;
}

// A recorded loop as a Voice: the buffer looping from a random point (so a
// restart doesn't always open the same way), trimmed and faded in.
export function buildRecording(ctx, id, buffer, dest, { t0 = ctx.currentTime, fadeIn = FADE } = {}) {
  const v = new Voice(ctx, dest, t0);
  v.fadeIn(fadeIn);
  const s = v.add(ctx.createBufferSource());
  s.buffer = buffer;
  s.loop = true;
  chain(s, v.gain(RECORDING_TRIM[id] ?? 1), v.out);
  s.start(t0, rand(0, buffer.duration));
  return v;
}

// Bar/bell-like partials: [frequency ratio, amplitude, decay multiplier].
// The non-integer ratios give a soft metallic colour; high partials die fast.
const BELL = [[1, 1, 1], [2, 0.22, 0.6], [2.76, 0.1, 0.4], [5.4, 0.03, 0.22]];
const TICK = [[1, 1, 1], [2.76, 0.12, 0.5]];
const CHIMES = {
  'focus-end': [[659.25, 0], [523.25, 0.36]],  // E5 -> C5: calm descending third
  'break-end': [[523.25, 0], [783.99, 0.3]],   // C5 -> G5: gentle ascending fifth
  start: [[1046.5, 0]],                        // single soft blip
};

// Schedules a chime at `when`; returns { end, nodes, stop(), cleanup(), onDone }.
// cleanup() runs once (on the last oscillator's end, or when called directly).
export function buildChime(ctx, dest, kind, when = ctx.currentTime + 0.03) {
  const notes = CHIMES[kind];
  if (!notes) throw new Error('Unknown chime: ' + kind);
  const tick = kind === 'start', partials = tick ? TICK : BELL;
  const tau0 = tick ? 0.055 : 0.7, atk = tick ? 0.002 : 0.006, amp = tick ? 0.6 : 0.8;
  const nodes = [], oscs = [];
  let end = when, last = null;
  for (const [f, dt] of notes) {
    const t = when + dt, pan = ctx.createStereoPanner();
    pan.pan.value = rand(-0.15, 0.15);
    pan.connect(dest);
    nodes.push(pan);
    for (const [r, a, d] of partials) {
      const o = ctx.createOscillator(), g = ctx.createGain(), tau = tau0 * d, stop = t + atk + tau * 8;
      o.frequency.value = f * r * (1 + rand(-0.0015, 0.0015)); // tiny detune: repeats aren't identical
      g.gain.setValueAtTime(0, t);
      g.gain.linearRampToValueAtTime(amp * a, t + atk);
      g.gain.setTargetAtTime(0, t + atk, tau);
      o.connect(g).connect(pan);
      o.start(t); o.stop(stop);
      nodes.push(o, g); oscs.push(o);
      if (stop > end) { end = stop; last = o; }
    }
  }
  let done = false;
  const h = {
    end, nodes, onDone: null,
    stop() { for (const o of oscs) { try { o.stop(); } catch {} } },
    cleanup() {
      if (done) return;
      done = true;
      for (const n of nodes) n.disconnect();
      h.onDone?.();
    },
  };
  last.onended = h.cleanup;
  return h;
}

// ---------------------------------------------------------------- engine

// Options: load(id) -> Promise<ArrayBuffer> of a sound's recorded loop (Ogg
// Opus); without it everything is synthesized. onChange() is called when a
// recording starts playing (or gives way to synthesis), so the page can redraw.
export function createAudio({ load = null, onChange = () => {} } = {}) {
  let ctx = null, master = null, vol = DEFAULT_VOLUME;
  let timer = 0, idleTimer = 0, lastTick = 0;
  const live = new Map();   // id -> voice of each sound in the mix (not fading out)
  const voices = new Set(); // live + fading-out voices
  const chimes = new Set(); // scheduled chimes (+ placeholders while the context resumes)
  const decoded = new Map(); // id -> Promise<AudioBuffer> of its recording, for this context
  const failed = new Map();  // id -> when its recording last failed (performance.now())

  function ensure() {
    if (!ctx) {
      ctx = new AudioContext({ latencyHint: 'playback' }); // bigger buffers, less CPU
      master = buildMaster(ctx);
      master.volume.gain.value = volumeCurve(vol);
    }
    clearTimeout(idleTimer);
    if (ctx.state === 'suspended') ctx.resume().catch(() => {});
  }
  function tick() {
    const now = ctx.currentTime, wall = performance.now();
    // If timers get throttled (e.g. a hidden window), look further ahead so
    // events don't leave gaps between ticks.
    const gap = lastTick ? (wall - lastTick) / 1000 : 0;
    const ahead = clamp(gap * 1.5 + 0.2, LOOKAHEAD, 3);
    lastTick = wall;
    for (const v of voices) {
      // retired voices are freed once their fade is over (audio clock), or after a
      // wall-clock safety margin in case the context never got to run
      if (v.endAt != null && (now >= v.endAt || wall >= v.endWall)) {
        v.dispose();
        if (!v.handedOver) v.level.disconnect(); // a replaced voice's level gain lives on in its successor
        voices.delete(v);
        forget();
      } else v.tick(now + ahead, now);
    }
    if (!voices.size) idle();
  }
  // A decoded loop is ~35-70 MB: keep only those of sounds in the mix (one
  // picked again later comes back from the disk copy in a moment).
  function forget() {
    for (const [id, p] of decoded) if (p.settled && !live.has(id)) decoded.delete(id);
  }
  // Nothing playing: stop the scheduler and suspend the context so it costs no CPU.
  function idle() {
    if (voices.size || chimes.size) return;
    clearInterval(timer); timer = 0; lastTick = 0;
    clearTimeout(idleTimer);
    idleTimer = setTimeout(() => { if (ctx && !voices.size && !chimes.size) ctx.suspend().catch(() => {}); }, 1000);
  }
  function retire(v) {
    v.fadeOut(FADE);
    v.endAt = ctx.currentTime + FADE + 0.05;
    v.endWall = performance.now() + (FADE + 1) * 1000;
  }
  // A sound's own level in the mix (0..1, same curve as the master volume).
  const levelGain = (level) => volumeCurve(clamp(Number(level) || 0, 0, 1));

  // Plays exactly these sounds, each at its own level: new ones fade in, dropped
  // ones fade out (a crossfade when one replaces another), kept ones glide.
  function setMix(entries) {
    const want = new Map();
    for (const e of entries) {
      if (!BUILDERS[e.id]) throw new Error('Unknown sound: ' + e.id);
      want.set(e.id, e.level ?? 1);
    }
    if (!ctx && !want.size) return;
    ensure();
    const now = ctx.currentTime;
    for (const [id, v] of live) {
      if (!want.has(id)) { retire(v); live.delete(id); }
    }
    for (const [id, level] of want) {
      const v = live.get(id);
      if (v) { v.level.gain.setTargetAtTime(levelGain(level), now, 0.08); continue; }
      const g = ctx.createGain();
      g.gain.value = levelGain(level);
      g.connect(master.sounds);
      const t0 = now + 0.05, voice = startVoice(id, g, t0);
      voice.level = g;
      live.set(id, voice);
      voices.add(voice);
      voice.tick(t0 + LOOKAHEAD, now);
    }
    if (voices.size && !timer) timer = setInterval(tick, TICK_MS);
  }

  // The recording when there is one. Until it's loaded a silent stand-in
  // holds the sound's place in the mix; if that takes long, the synthesized
  // sound fills in, and the recording crossfades over it when it comes. A
  // recording that can't be had leaves the synthesized sound.
  function startVoice(id, dest, t0) {
    const recorded = load && RECORDED.has(id) && !(performance.now() - (failed.get(id) ?? -Infinity) < RETRY_MS);
    if (!recorded) return buildSound(ctx, id, dest, { t0 });
    const ready = decoded.get(id)?.settled;
    if (ready) return buildRecording(ctx, id, ready, dest, { t0 });
    const turn = {}; // this pick of the sound: its voices carry it
    const standIn = new Voice(ctx, dest, t0);
    standIn.loading = true;
    standIn.turn = turn;
    const c = ctx;
    const synth = (g, t) => Object.assign(buildSound(ctx, id, g, { t0: t }), { turn });
    const filler = setTimeout(() => {
      if (live.get(id) === standIn) replace(id, turn, synth, true);
    }, SYNTH_AFTER_MS);
    recording(id).then(
      (buffer) => { clearTimeout(filler); replace(id, turn, (g, t) => buildRecording(ctx, id, buffer, g, { t0: t })); },
      () => {
        clearTimeout(filler);
        if (ctx === c) failed.set(id, performance.now());
        const now = live.get(id);
        if (now === standIn) replace(id, turn, synth);
        else if (now?.turn === turn && now.loading) { now.loading = false; onChange(); } // the filler stays
      },
    );
    return standIn;
  }
  // The decoded recording, fetched and decoded once per context.
  function recording(id) {
    let p = decoded.get(id);
    if (!p) {
      const c = ctx;
      p = Promise.resolve()
        .then(() => load(id))
        .then((bytes) => c.decodeAudioData(bytes))
        .then((buffer) => {
          p.settled = buffer;
          // Dropped from the mix while it loaded: don't hold it (see forget()).
          if (!live.has(id) && decoded.get(id) === p) decoded.delete(id);
          return buffer;
        });
      p.catch(() => { if (decoded.get(id) === p) decoded.delete(id); });
      decoded.set(id, p);
    }
    return p;
  }
  // Swaps the sound's voice of this pick (`turn`) for a new one, crossfading,
  // and reusing its level gain. Nothing happens if the sound was dropped (or
  // the engine disposed) meanwhile.
  function replace(id, turn, build, loading = false) {
    const old = live.get(id);
    if (!ctx || old?.turn !== turn) return;
    const t0 = ctx.currentTime + 0.05, voice = build(old.level, t0);
    voice.level = old.level;
    voice.loading = loading;
    old.handedOver = true;
    retire(old);
    live.set(id, voice);
    voices.add(voice);
    voice.tick(t0 + LOOKAHEAD, ctx.currentTime);
    if (!timer) timer = setInterval(tick, TICK_MS);
    onChange();
  }

  return {
    setMix,
    play(id) { setMix([{ id, level: 1 }]); },
    stop() { setMix([]); },
    /** The first sound of the mix, or null. */
    current: () => live.keys().next().value ?? null,
    /** Ids of the sounds playing now, in the order they were added. */
    mix: () => [...live.keys()],
    /** Ids in the mix still waiting for their recording. */
    loading: () => [...live].filter(([, v]) => v.loading).map(([id]) => id),
    setVolume(v) {
      vol = clamp(Number(v) || 0, 0, 1);
      if (ctx) master.volume.gain.setTargetAtTime(volumeCurve(vol), ctx.currentTime, 0.05);
    },
    volume: () => vol,
    // Chimes have their own fixed bus level but still follow the master volume.
    chime(kind) {
      if (!CHIMES[kind]) throw new Error('Unknown chime: ' + kind);
      const fresh = !ctx;
      ensure();
      const c0 = ctx, running = !fresh && ctx.state === 'running';
      const pending = { nodes: [], stop() {}, cleanup() {} }; // keeps idle() from suspending meanwhile
      chimes.add(pending);
      // if the context can't start (autoplay blocked), drop the chime rather than
      // queueing it to fire late on the next user gesture
      const giveUp = setTimeout(() => { if (chimes.delete(pending)) idle(); }, 2000);
      const go = () => {
        clearTimeout(giveUp);
        if (!chimes.delete(pending) || ctx !== c0) return; // disposed or given up meanwhile
        // right after a (re)start the output device may swallow the first ~100 ms
        const c = buildChime(ctx, master.chimes, kind, ctx.currentTime + (running ? 0.03 : 0.15));
        chimes.add(c);
        // wall-clock fallback in case the context never runs and onended never fires
        const fallback = setTimeout(c.cleanup, (c.end - ctx.currentTime + 1) * 1000);
        c.onDone = () => { clearTimeout(fallback); chimes.delete(c); idle(); };
      };
      if (running) go();
      else ctx.resume().then(go, go);
    },
    // Debug counters (used by the leak test in test.html).
    stats: () => ({
      voices: voices.size, chimes: chimes.size, timer: !!timer, state: ctx?.state ?? 'none',
      oneShots: [...voices].reduce((s, v) => s + v.live.size, 0),
    }),
    dispose() {
      for (const v of voices) { v.dispose(); v.level.disconnect(); }
      voices.clear();
      live.clear();
      decoded.clear();
      for (const c of [...chimes]) { c.stop(); c.cleanup(); }
      chimes.clear();
      clearInterval(timer); clearTimeout(idleTimer); timer = 0; lastTick = 0;
      if (ctx) { ctx.close().catch(() => {}); ctx = null; master = null; }
    },
  };
}
