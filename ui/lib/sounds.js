// sounds.js: procedural ambient sounds and chimes, Web Audio API only.
// No audio files, no network, no per-sample JS at runtime: noise buffers and tiny
// one-shot samples (drops, crackles) are computed once per AudioContext, then
// everything is looped / filtered / enveloped by native nodes. Discrete events are
// scheduled slightly ahead by a timer running every 200 ms.

export const SOUNDS = [
  { id: 'rain-light', title: 'Слабый дождь', group: 'rain' },
  { id: 'rain', title: 'Дождь', group: 'rain' },
  { id: 'rain-heavy', title: 'Ливень', group: 'rain' },
  { id: 'rain-wind', title: 'Дождь с ветром', group: 'rain' },
  { id: 'rain-thunder', title: 'Гроза', group: 'rain' },
  { id: 'brown', title: 'Коричневый шум', group: 'noise' },
  { id: 'waves', title: 'Волны', group: 'nature' },
  { id: 'fire', title: 'Камин', group: 'nature' },
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
  'rain-light': 1.38, rain: 0.72, 'rain-heavy': 0.5, 'rain-wind': 0.79,
  'rain-thunder': 0.72, brown: 0.75, waves: 1, fire: 1.35,
};

const rand = (a, b) => a + Math.random() * (b - a);
const pick = (arr) => arr[(Math.random() * arr.length) | 0];
const expRand = (mean) => -Math.log(1 - Math.random()) * mean; // Poisson inter-arrival
const clamp = (x, a, b) => Math.min(b, Math.max(a, x));
// Equal-power fade curves: during a crossfade of two unrelated sounds the total
// power stays constant (a linear crossfade dips ~3 dB in the middle).
const EQ_IN = Float32Array.from({ length: 32 }, (_, i) => Math.sin((i / 31) * (Math.PI / 2)));
const EQ_OUT = EQ_IN.slice().reverse();
const chain = (...ns) => { for (let i = 0; i < ns.length - 1; i++) ns[i].connect(ns[i + 1]); return ns[ns.length - 1]; };

// ---------------------------------------------------------------- buffers

const cache = new WeakMap(); // ctx -> { white, pink, brown, 'bank-drop', ... }
const store = (ctx) => { let s = cache.get(ctx); if (!s) cache.set(ctx, (s = {})); return s; };
const noise = (ctx, kind) => (store(ctx)[kind] ??= makeNoise(ctx, kind));
const bank = (ctx, kind) => (store(ctx)['bank-' + kind] ??= makeBank(ctx, kind));

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
    let mean = 0, e = 0;
    for (let i = 0; i < n; i++) mean += y[i];
    mean /= n;
    for (let i = 0; i < n; i++) { y[i] -= mean; e += y[i] * y[i]; }
    const k = 0.2 / Math.sqrt(e / n);
    for (let i = 0; i < n; i++) y[i] *= k;
  }
  return buf;
}

// RBJ biquad applied in place (only used while building the sample banks).
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
}

const whiteArr = (n) => { const x = new Float32Array(n); for (let i = 0; i < n; i++) x[i] = Math.random() * 2 - 1; return x; };

// Small banks of mono one-shots, peak-normalised. Each sample is long enough for
// its exponential decay to reach ~-60 dB, so it never ends with a click.
function makeBank(ctx, kind) {
  const sr = ctx.sampleRate, out = [];
  const count = { drop: 24, crackle: 16, pop: 8 }[kind];
  for (let k = 0; k < count; k++) {
    let x;
    if (kind === 'drop') {
      // a drop hitting a surface: band-passed noise tick, ~1 ms attack, 4-20 ms decay
      const atk = rand(0.0006, 0.002) * sr, tau = rand(0.004, 0.02) * sr;
      // optional faint bubble "plip": a damped sine gliding slightly upward
      const plip = Math.random() < 0.25, d = rand(0.004, 0.01) * sr, t0 = Math.round(rand(0.001, 0.004) * sr);
      x = whiteArr(Math.ceil(Math.max(atk + tau * 7, plip ? t0 + d * 7 : 0)));
      biquad(x, 'bp', rand(1200, 5000), rand(0.7, 2), sr);
      for (let i = 0; i < x.length; i++) x[i] *= i < atk ? i / atk : Math.exp(-(i - atk) / tau);
      if (plip) {
        const f0 = rand(1500, 3200), amp = rand(0.06, 0.15), ramp = 0.0005 * sr;
        let ph = 0;
        for (let i = t0; i < x.length; i++) {
          const t = i - t0;
          ph += (2 * Math.PI * f0 * (1 + (0.4 * t) / x.length)) / sr;
          x[i] += amp * Math.sin(ph) * Math.exp(-t / d) * Math.min(1, t / ramp);
        }
      }
    } else if (kind === 'crackle') {
      // tiny dry snap: 0.5-2.5 ms of bright noise
      const atk = 0.00015 * sr, tau = rand(0.0005, 0.0025) * sr;
      x = whiteArr(Math.ceil(atk + tau * 7));
      biquad(x, 'hp', rand(1200, 3500), 0.7, sr);
      biquad(x, 'lp', 9000, 0.7, sr);
      for (let i = 0; i < x.length; i++) x[i] *= i < atk ? i / atk : Math.exp(-(i - atk) / tau);
    } else {
      // pop: a click plus a short resonant "body" (sap pocket bursting)
      const tc = 0.0012 * sr, tb = rand(0.008, 0.022) * sr;
      x = whiteArr(Math.ceil(tb * 7));
      const body = whiteArr(x.length);
      biquad(body, 'bp', rand(400, 1100), 4, sr);
      biquad(x, 'hp', 800, 0.7, sr);
      for (let i = 0; i < x.length; i++) {
        const a = Math.min(1, i / (0.0002 * sr));
        x[i] = a * (0.6 * x[i] * Math.exp(-i / tc) + 3 * body[i] * Math.exp(-i / tb));
      }
    }
    let peak = 0;
    for (let i = 0; i < x.length; i++) peak = Math.max(peak, Math.abs(x[i]));
    if (!(peak > 0 && peak < Infinity)) { x.fill(0); peak = 1; } // never let NaN reach the graph
    for (let i = 0; i < x.length; i++) x[i] /= peak;
    const b = ctx.createBuffer(1, x.length, sr);
    b.copyToChannel(x, 0);
    out.push(b);
  }
  return out;
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
    this.dead = false;
  }
  add(n) { this.nodes.push(n); return n; }
  gain(v) { const g = this.add(this.ctx.createGain()); g.gain.value = v; return g; }
  filter(type, f, Q) { return this.add(this.tmpFilter(type, f, Q)); }
  panner(p = 0) { const s = this.add(this.ctx.createStereoPanner()); s.pan.value = p; return s; }
  panBuses(dest, width, n = 6) {
    return Array.from({ length: n }, (_, i) => { const p = this.panner(-width + (2 * width * i) / (n - 1)); p.connect(dest); return p; });
  }

  // Continuous noise bed: two copies of a loop at random offsets and slightly
  // different rates. Their sum never lines up the same way twice, so the 8 s
  // loop period can't be heard; the 4-9% pitch difference is inaudible on noise.
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

  // One-shot source -> nodes... -> dest. All its nodes are released when it ends.
  shot(buf, t, nodes, dest, { rate = 1, offset = 0, dur = 0, loop = false } = {}) {
    if (this.dead) return;
    const s = this.ctx.createBufferSource();
    s.buffer = buf; s.playbackRate.value = rate; s.loop = loop;
    chain(s, ...nodes).connect(dest);
    const rec = [s, ...nodes];
    this.live.add(rec);
    s.onended = () => { this.live.delete(rec); for (const n of rec) n.disconnect(); };
    if (loop) { s.start(t, offset); s.stop(t + dur); }
    else if (dur) s.start(t, offset, dur);
    else s.start(t);
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

// Rain: pink-noise hiss (countless distant drops) + individual close drops.
function rainLayer(v, dest, c) {
  const hp = v.filter('highpass', c.hp, 0.5), lp = v.filter('lowpass', c.lp, 0.5), g = v.gain(c.bed);
  chain(v.bed('pink'), hp, lp, g, dest);
  v.drift(g.gain, c.bed * 0.82, c.bed * 1.12, 3, 9);
  v.drift(lp.frequency, c.lp * 0.8, c.lp * 1.1, 4, 11);
  if (c.low) {
    // downpour: low roar of water on roofs and ground
    const lg = v.gain(c.low);
    chain(v.bed('brown'), v.filter('highpass', 50), v.filter('lowpass', 420, 0.6), lg, dest);
    v.drift(lg.gain, c.low * 0.75, c.low * 1.15, 4, 10);
  }
  const drops = bank(v.ctx, 'drop'), buses = v.panBuses(dest, 0.9);
  v.every(rand(3, 8), () => { v.density = rand(0.7, 1.3); return rand(3, 8); });
  v.every(0.1, (t) => {
    // mostly quiet drops, an occasional close one (cubed uniform)
    const g = v.tmpGain(c.drop * (0.12 + 0.88 * Math.random() ** 3));
    v.shot(pick(drops), t, [g], pick(buses), { rate: rand(c.pitch[0], c.pitch[1]) });
    return expRand(1 / (c.rate * v.density));
  });
}

// Wind: a broad band (body) and a narrow resonant band (whistle) of noise whose
// level, centre frequency and position follow random gusts.
function windLayer(v, dest, c) {
  const bf = v.filter('bandpass', 400, 0.8), bg = v.gain(0), bp = v.panner(0);
  chain(v.bed('pink'), bf, bg, bp, dest);
  const wf = v.filter('bandpass', 900, 9), wg = v.gain(0), wp = v.panner(0);
  chain(v.bed('white'), wf, wg, wp, dest);
  v.drift(wf.Q, 6, 12, 3, 8);
  let g = 0.5;
  v.every(0, (t) => {
    // gust strength with memory, occasionally a strong one
    g = clamp(0.55 * g + 0.45 * Math.random() + (Math.random() < 0.15 ? 0.4 : 0), 0.05, 1);
    const tau = rand(0.8, 2.2);
    bg.gain.setTargetAtTime(c.body * (0.3 + 0.7 * g), t, tau);
    bf.frequency.setTargetAtTime(280 + 450 * g, t, tau * 1.3);
    wg.gain.setTargetAtTime(c.whistle * g * g, t, tau * 1.2); // whistles only when it blows hard
    wf.frequency.setTargetAtTime(rand(600, 800) + 800 * g, t, tau * 1.5);
    bp.pan.setTargetAtTime(rand(-0.5, 0.5), t, tau * 2);
    wp.pan.setTargetAtTime(rand(-0.6, 0.6), t, tau * 2);
    return rand(2.5, 7);
  });
}

// Distant thunder: optional muffled crack, then a long low rumble of low-passed
// brown noise with a slow attack, a few "rolls" and a multi-second decay.
function thunderLayer(v, dest, c) {
  v.every(c.first ?? rand(8, 20), (t) => { thunder(v, dest, t, c.level); return rand(20, 60); });
}
function thunder(v, dest, t, level) {
  const ctx = v.ctx, far = Math.random(); // 0 = nearer, 1 = very distant
  const pan = ctx.createStereoPanner(), p0 = rand(-0.7, 0.7);
  pan.connect(dest);
  let tr = t + 0.05;
  if (far < 0.75) {
    const lp = v.tmpFilter('lowpass', 3000 - 2000 * far), hp = v.tmpFilter('highpass', 250), g = v.tmpGain(0);
    const amp = level * 0.8 * (1 - 0.7 * far);
    let tt = t;
    for (let k = 0, n = 2 + ((Math.random() * 4) | 0); k < n; k++) {
      g.gain.setTargetAtTime(amp * rand(0.5, 1) * (1 - k / (n + 1)), tt, 0.008);
      g.gain.setTargetAtTime(0, tt + 0.02, rand(0.04, 0.12));
      tt += rand(0.05, 0.18);
    }
    v.shot(noise(ctx, 'white'), t, [lp, hp, g], pan, { offset: rand(0, 5), dur: tt - t + 1 });
    tr = t + rand(0.15, 0.5) + far;
  }
  const f = rand(200, 360), lp1 = v.tmpFilter('lowpass', f, 0.8), lp2 = v.tmpFilter('lowpass', f * 1.6, 0.6), g = v.tmpGain(0);
  const peak = level * (1 - 0.45 * far), atk = rand(0.6, 1.6) + 0.8 * far;
  g.gain.setValueAtTime(0, tr);
  g.gain.linearRampToValueAtTime(peak * rand(0.6, 0.9), tr + atk);
  let tk = tr + atk;
  for (let k = 0, n = 1 + ((Math.random() * 4) | 0); k < n; k++) {
    tk += rand(0.7, 1.8);
    g.gain.setTargetAtTime(peak * rand(0.4, 1), tk, rand(0.25, 0.6));
  }
  const tEnd = tk + rand(0.5, 1.2), tau = rand(1.2, 2.4), stop = tEnd + tau * 6;
  g.gain.setTargetAtTime(0, tEnd, tau);
  lp1.frequency.setTargetAtTime(f * 0.6, tEnd, tau); // the tail gets darker as it dies away
  pan.pan.setValueAtTime(p0, t);
  pan.pan.linearRampToValueAtTime(clamp(p0 + rand(-0.6, 0.6), -0.9, 0.9), stop);
  v.shot(noise(ctx, 'brown'), tr, [lp1, lp2, g, pan], dest, { loop: true, offset: rand(0, 8), dur: stop - tr });
}

// Surf: a quiet distant-ocean bed plus individual waves at irregular intervals.
function wavesLayer(v, dest) {
  const bg = v.gain(0.5);
  chain(v.bed('brown'), v.filter('highpass', 40), v.filter('lowpass', 280, 0.6), bg, dest);
  v.drift(bg.gain, 0.35, 0.6, 4, 10);
  const fg = v.gain(0.32), ff = v.filter('bandpass', 500, 0.5);
  chain(v.bed('pink'), ff, fg, dest);
  v.drift(fg.gain, 0.18, 0.42, 3, 8);
  v.drift(ff.frequency, 380, 700, 5, 12);
  v.every(rand(0.3, 1.5), (t) => { wave(v, dest, t); return Math.random() < 0.2 ? rand(5, 6.5) : rand(7, 12); });
}
function wave(v, dest, t) {
  const ctx = v.ctx, size = rand(0.55, 1), rise = rand(2.5, 4.5), crest = t + rise, wash = rand(1.3, 2.2), end = crest + wash * 5;
  const pan = ctx.createStereoPanner(), p = rand(-0.45, 0.45);
  pan.pan.setValueAtTime(p, t);
  pan.pan.linearRampToValueAtTime(clamp(p + rand(-0.35, 0.35), -0.8, 0.8), end);
  // body: an accelerating swell (exponential ramp = steady crescendo in dB) whose
  // low-pass opens as the wave rises and breaks, then closes as it washes out
  const lp = v.tmpFilter('lowpass', 250, 0.5), g = v.tmpGain(0);
  lp.frequency.setValueAtTime(250, t);
  lp.frequency.exponentialRampToValueAtTime(rand(1400, 2600) * size, crest);
  lp.frequency.setTargetAtTime(350, crest + 0.3, wash);
  g.gain.setValueAtTime(0.02 * size, t);
  g.gain.exponentialRampToValueAtTime(size, crest);
  g.gain.setTargetAtTime(0, crest + 0.15, wash);
  v.shot(noise(ctx, 'pink'), t, [lp, g, pan], dest, { loop: true, offset: rand(0, 8), dur: end - t });
  // foam: bright hiss of the broken crest running up the beach
  const hp = v.tmpFilter('highpass', rand(1500, 2500), 0.5), fg = v.tmpGain(0);
  fg.gain.setValueAtTime(0, crest - 0.5);
  fg.gain.linearRampToValueAtTime(0.35 * size, crest + 0.3);
  fg.gain.setTargetAtTime(0, crest + 0.4, wash * 0.8);
  hp.frequency.setTargetAtTime(hp.frequency.value * 0.6, crest + 0.4, wash);
  v.shot(noise(ctx, 'white'), crest - 0.5, [hp, fg], pan, { loop: true, offset: rand(0, 8), dur: end - crest + 0.5 });
  // soft low thump of the break
  const tl = v.tmpFilter('lowpass', 160), tg = v.tmpGain(0);
  tg.gain.setValueAtTime(0, crest - 0.3);
  tg.gain.linearRampToValueAtTime(0.45 * size, crest + 0.1);
  tg.gain.setTargetAtTime(0, crest + 0.15, 0.7);
  v.shot(noise(ctx, 'brown'), crest - 0.3, [tl, tg], pan, { loop: true, offset: rand(0, 8), dur: 5 });
}

// Fireplace: flickering low roar + flame band + faint hiss, clustered crackles, rare pops.
function fireLayer(v, dest) {
  const ctx = v.ctx;
  const roar = v.gain(0.5), rf = v.filter('lowpass', 320, 0.7);
  chain(v.bed('brown'), v.filter('highpass', 40), rf, roar, dest);
  v.drift(roar.gain, 0.35, 0.65, 0.4, 1.6); // flicker
  v.drift(rf.frequency, 220, 480, 1, 4);
  const flame = v.gain(0.25), ff = v.filter('bandpass', 900, 0.7);
  chain(v.bed('pink'), ff, flame, dest);
  v.drift(flame.gain, 0.12, 0.35, 0.3, 1.2);
  v.drift(ff.frequency, 600, 1400, 1, 4);
  const hiss = v.gain(0.03);
  chain(v.bed('white'), v.filter('highpass', 4000, 0.6), hiss, dest);
  v.drift(hiss.gain, 0.01, 0.05, 1, 5);
  const cr = bank(ctx, 'crackle'), pops = bank(ctx, 'pop'), buses = v.panBuses(dest, 0.7);
  v.every(0, () => { v.density = rand(0.4, 1.6); return rand(2, 6); });
  v.every(0.3, (t) => {
    // crackles come in little clusters of decreasing snaps
    const bus = pick(buses);
    let tt = t, a = rand(0.25, 0.8);
    for (let k = 0, n = 1 + ((Math.random() ** 2 * 6) | 0); k < n; k++) {
      v.shot(pick(cr), tt, [v.tmpGain(a * rand(0.4, 1))], bus, { rate: rand(0.6, 1.5) });
      tt += rand(0.004, 0.06);
      a *= 0.8;
    }
    return expRand(0.45 / v.density);
  });
  v.every(rand(2, 6), (t) => {
    v.shot(pick(pops), t, [v.tmpGain(0.6 * rand(0.5, 1))], pick(buses), { rate: rand(0.8, 1.2) });
    return rand(3, 12);
  });
}

// Brown noise: deep and smooth, top gently rolled off, very slow drift.
function brownLayer(v, dest) {
  const lp = v.filter('lowpass', 700, 0.5), g = v.gain(1);
  chain(v.bed('brown'), v.filter('highpass', 35), lp, g, dest);
  v.drift(lp.frequency, 550, 850, 6, 14);
  v.drift(g.gain, 0.9, 1.05, 5, 12);
}

const RAIN_MID = { bed: 0.8, hp: 300, lp: 7000, rate: 14, drop: 0.3, pitch: [0.75, 1.15] };
const BUILDERS = {
  'rain-light': (v, d) => rainLayer(v, d, { bed: 0.5, hp: 700, lp: 4500, rate: 6, drop: 0.35, pitch: [0.85, 1.2] }),
  rain: (v, d) => rainLayer(v, d, { bed: 0.8, hp: 350, lp: 7000, rate: 16, drop: 0.3, pitch: [0.75, 1.15] }),
  'rain-heavy': (v, d) => rainLayer(v, d, { bed: 1, hp: 200, lp: 9500, low: 0.6, rate: 30, drop: 0.3, pitch: [0.6, 1] }),
  'rain-wind': (v, d) => {
    rainLayer(v, d, { bed: 0.7, hp: 300, lp: 6500, rate: 12, drop: 0.28, pitch: [0.75, 1.15] });
    windLayer(v, d, { body: 1, whistle: 1.6 });
  },
  'rain-thunder': (v, d, o) => { rainLayer(v, d, RAIN_MID); thunderLayer(v, d, { level: 1.1, first: o.firstThunder }); },
  brown: (v, d) => brownLayer(v, d),
  waves: (v, d) => wavesLayer(v, d),
  fire: (v, d) => fireLayer(v, d),
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
    // transients (crackles, wave crests) stay a few dB under full scale at volume 1.
    const post = ctx.createGain();
    post.gain.value = 0.8;
    volume.connect(comp).connect(post).connect(dest);
    nodes.push(comp, post);
  } else volume.connect(dest);
  return { sounds, chimes, volume, nodes };
}

// Builds a sound's graph starting at t0 (with a fade-in) and returns its Voice.
// The caller drives it with voice.tick(until, now); offline, one tick(duration) suffices.
export function buildSound(ctx, id, dest, { t0 = ctx.currentTime, fadeIn = FADE, firstThunder } = {}) {
  const build = BUILDERS[id];
  if (!build) throw new Error('Unknown sound: ' + id);
  const v = new Voice(ctx, dest, t0);
  v.fadeIn(fadeIn);
  const trim = v.gain(TRIM[id]);
  trim.connect(v.out);
  build(v, trim, { firstThunder });
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

export function createAudio() {
  let ctx = null, master = null, vol = DEFAULT_VOLUME;
  let voice = null, curId = null, timer = 0, idleTimer = 0, lastTick = 0;
  const voices = new Set(); // current + fading-out voices
  const chimes = new Set(); // scheduled chimes (+ placeholders while the context resumes)

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
      if (v.endAt != null && (now >= v.endAt || wall >= v.endWall)) { v.dispose(); voices.delete(v); }
      else v.tick(now + ahead, now);
    }
    if (!voices.size) idle();
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

  return {
    play(id) {
      if (!BUILDERS[id]) throw new Error('Unknown sound: ' + id);
      if (id === curId && voice) return;
      ensure();
      if (voice) retire(voice); // crossfade: old fades out while the new one fades in
      const now = ctx.currentTime, t0 = now + 0.05;
      voice = buildSound(ctx, id, master.sounds, { t0 });
      curId = id;
      voices.add(voice);
      voice.tick(t0 + LOOKAHEAD, now);
      if (!timer) timer = setInterval(tick, TICK_MS);
    },
    stop() {
      if (!voice) return;
      retire(voice);
      voice = null; curId = null;
    },
    current: () => curId,
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
      for (const v of voices) v.dispose();
      voices.clear();
      for (const c of [...chimes]) { c.stop(); c.cleanup(); }
      chimes.clear();
      clearInterval(timer); clearTimeout(idleTimer); timer = 0; lastTick = 0;
      voice = null; curId = null;
      if (ctx) { ctx.close().catch(() => {}); ctx = null; master = null; }
    },
  };
}
