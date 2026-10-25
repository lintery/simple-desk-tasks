"""Seamless loops for the focus widget from the downloaded recordings.

    python -I loops.py <src dir with <id>.ogg> <out dir>

For each recording: pick a window (the steadiest one; for "events" sounds,
like the thunderstorm, the most eventful), cut a loop of up to LOOP_MAX s
whose end crossfades (equal power) into its start, bring it to a common
loudness and encode it as Opus. Writes <out>/<id>.opus and manifest.json.
"""
import array, json, math, pathlib, subprocess, sys

LOOP_MAX = 180.0  # s
FADE = 4.0  # s of crossfade at the seam
EDGE = 1.0  # s skipped at both ends of the recording (handling noise)
TARGET = {"I": -24.0, "TP": -2.0, "LRA": 11.0}
ENV_RATE = 4000

here = pathlib.Path(__file__).parent
src, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
sources = json.loads((here / "sources.json").read_text(encoding="utf-8"))


def run(args, **kw):
    return subprocess.run(args, check=True, capture_output=True, **kw)


def probe(path):
    info = json.loads(run(["ffprobe", "-v", "error", "-show_entries", "stream=channels:format=duration", "-of", "json", str(path)]).stdout)
    return float(info["format"]["duration"]), int(info["streams"][0]["channels"])


def loudness_per_second(path):
    """RMS level (dB) of each second, from a mono low-rate decode."""
    raw = run(["ffmpeg", "-v", "error", "-i", str(path), "-ac", "1", "-ar", str(ENV_RATE), "-f", "s16le", "-"]).stdout
    pcm = array.array("h")
    pcm.frombytes(raw[: len(raw) // 2 * 2])
    levels = []
    for i in range(0, len(pcm) - ENV_RATE + 1, ENV_RATE):
        chunk = pcm[i : i + ENV_RATE]
        ms = sum(v * v for v in chunk) / ENV_RATE
        levels.append(10 * math.log10(ms + 1e-9))
    return levels


def pick_window(levels, length, mode):
    """Start (s) of the best `length`-second window."""
    n = int(math.ceil(length))
    first, last = int(EDGE), len(levels) - int(EDGE) - n
    if last <= first:
        return float(first)
    best, best_score = first, None
    for s in range(first, last + 1):
        w = levels[s : s + n]
        mean = sum(w) / n
        sd = math.sqrt(sum((x - mean) ** 2 for x in w) / n)
        # steady: little change in level; events: lots going on (thunder)
        score = -sd if mode == "steady" else mean + sd
        # the seam joins the window's end to its start: alike is better
        score -= 0.5 * abs(sum(w[:3]) / 3 - sum(w[-3:]) / 3)
        if best_score is None or score > best_score:
            best, best_score = s, score
    return float(best)


# Optional ids after the two dirs: rebuild just those, keep the rest of the manifest.
only = set(sys.argv[3:])
manifest_path = out / "manifest.json"
manifest = json.loads(manifest_path.read_text(encoding="utf-8")) if only and manifest_path.exists() else {}
for sid, meta in sources.items():
    if only and sid not in only:
        continue
    path = src / f"{sid}.ogg"
    dur, channels = probe(path)
    loop = min(LOOP_MAX, dur - 2 * EDGE - FADE)
    fade = min(FADE, loop / 6)
    start = pick_window(loudness_per_second(path), loop + fade, meta.get("pick", "steady"))
    L, X = loop, fade
    graph = (
        f"[0:a]asplit=3[a][b][c];"
        f"[a]atrim=start={X}:end={L},asetpts=N/SR/TB[body];"
        f"[b]atrim=start={L}:end={L + X},asetpts=N/SR/TB,afade=t=out:d={X}:curve=qsin[tail];"
        f"[c]atrim=start=0:end={X},asetpts=N/SR/TB,afade=t=in:d={X}:curve=qsin[head];"
        f"[tail][head]amix=inputs=2:normalize=0[mix];"
        f"[body][mix]concat=n=2:v=0:a=1[loop]"
    )
    wav = out / f"{sid}.loop.wav"
    run(["ffmpeg", "-y", "-v", "error", "-ss", str(start), "-t", str(L + X), "-i", str(path),
         "-filter_complex", graph, "-map", "[loop]", "-ar", "48000", "-c:a", "pcm_s16le", str(wav)])
    # Loudness: measure, then one fixed gain to the target (loudnorm's own
    # second pass may switch to a running AGC, whose gain would differ at the
    # loop's two ends) and a fast limiter for the few peaks it pushes past TP.
    norm = f"loudnorm=I={TARGET['I']}:TP={TARGET['TP']}:LRA={TARGET['LRA']}"
    report = run(["ffmpeg", "-v", "info", "-i", str(wav), "-af", norm + ":print_format=json", "-f", "null", "-"], text=True).stderr
    m = json.loads(report[report.rindex("{") : report.rindex("}") + 1])
    gain = TARGET["I"] - float(m["input_i"])
    ceiling = 10 ** (TARGET["TP"] / 20)
    # latency: without it the limiter's lookahead delays the output by `attack`
    # and the trim below would cut the loop's end off its start.
    apply = f"volume={gain:.2f}dB,alimiter=limit={ceiling:.4f}:attack=1:release=50:level=disabled:latency=1"
    opus = out / f"{sid}.opus"
    bitrate = "64k" if channels > 1 else "40k"
    # Exactly L seconds: resampling may leave a few ms of near-silence at the
    # end, which a loop would play as a dip.
    exact = f"{apply},aresample=48000,atrim=end_sample={round(L * 48000)}"
    run(["ffmpeg", "-y", "-v", "error", "-i", str(wav), "-af", exact, "-ar", "48000",
         "-c:a", "libopus", "-b:a", bitrate, "-vbr", "on", "-application", "audio", str(opus)])
    wav.unlink()
    manifest[sid] = {
        "file": opus.name, "bytes": opus.stat().st_size, "seconds": round(L, 2), "channels": channels,
        "from": round(start, 1), "source": meta["page"], "title": meta["title"], "author": meta["author"], "license": "CC0-1.0",
    }
    print(f"{sid}: {L:.0f}s from {start:.0f}s of {dur:.0f}s, {channels}ch, {opus.stat().st_size / 1e6:.2f} MB, was {m['input_i']} LUFS, {gain:+.1f} dB")

manifest = {sid: manifest[sid] for sid in sources if sid in manifest}  # in the widget's order
manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
