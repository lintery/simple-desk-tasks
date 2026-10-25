"""One-shots for «Самолёт»: cabin chimes and crew announcements.

    python -I plane.py <src dir with plane.json's <name>.ogg> <out dir>

The cabin loop itself is made by loops.py (sources.json, "plane"). Writes
<out>/plane-chime-<n>.opus, <out>/plane-pa-<n>.opus and plane.json (what
each is, how long). Chimes are trimmed and peak-levelled; announcements are
cut out of the recordings, the one taken straight from a seat's headphone
jack is made to sound like the cabin speakers, and all are levelled to the
same loudness (one gain plus a limiter, as in loops.py).
"""
import json, pathlib, re, subprocess, sys

here = pathlib.Path(__file__).parent
src, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
sources = json.loads((here / "plane.json").read_text(encoding="utf-8"))

CHIMES = ["seatbelt", "tritone", "double"]
SPEECH_LUFS, CEILING = -24.0, 10 ** (-2 / 20)
# An announcement from the headphone-jack recording: this long, in seconds
# (shorter bits are chimes or scraps; longer ones would drag, and are left
# whole rather than cut mid-sentence: skipped).
CLIP_MIN, CLIP_MAX = 8.0, 70.0
# The line hiss sits around -35 dB; the gaps between announcements are ~1.2-1.5 s.
GAP = "silencedetect=noise=-25dB:d=1.1"
# Cabin speakers: narrow band, a little squashed, a short hard reflection.
SPEAKER = "highpass=f=320,lowpass=f=3800,acompressor=threshold=-20dB:ratio=3:attack=5:release=120,aecho=0.85:0.6:23|41:0.22|0.12"


def run(args, **kw):
    return subprocess.run(args, check=True, capture_output=True, **kw)


def duration(path):
    return float(run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)], text=True).stdout)


def loudness(path, chain):
    report = run(["ffmpeg", "-v", "info", "-i", str(path), "-af", f"{chain},loudnorm=print_format=json", "-f", "null", "-"], text=True).stderr
    return json.loads(report[report.rindex("{") : report.rindex("}") + 1])


def encode(path, chain, dest):
    run(["ffmpeg", "-y", "-v", "error", "-i", str(path), "-af", chain, "-ac", "1", "-ar", "48000",
         "-c:a", "libopus", "-b:a", "48k", "-application", "audio", str(dest)])
    return round(duration(dest), 2)


def speech(path, cut, dest, extra=""):
    """An announcement: cut, shaped, levelled to SPEECH_LUFS."""
    chain = ",".join(x for x in [cut, extra] if x)
    m = loudness(path, chain)
    gain = SPEECH_LUFS - float(m["input_i"])
    tail = f"volume={gain:.2f}dB,alimiter=limit={CEILING:.4f}:attack=2:release=80:level=disabled:latency=1"
    return encode(path, f"{chain},{tail},afade=t=in:d=0.3,areverse,afade=t=in:d=0.6,areverse", dest)


manifest = {"chimes": [], "announcements": []}

for n, name in enumerate(CHIMES, 1):
    path = src / f"{name}.ogg"
    # Trim the silence around it, peak at -6 dBFS (the page sets the level).
    trim = "silenceremove=start_periods=1:start_threshold=-50dB,areverse,silenceremove=start_periods=1:start_threshold=-60dB,areverse"
    peak = float(re.search(r"max_volume: (-?[\d.]+) dB", run(["ffmpeg", "-i", str(path), "-af", f"{trim},volumedetect", "-f", "null", "-"], text=True).stderr).group(1))
    dest = out / f"plane-chime-{n}.opus"
    secs = encode(path, f"{trim},volume={-6 - peak:.2f}dB,areverse,afade=t=in:d=0.05,areverse", dest)
    manifest["chimes"].append({"file": dest.stem, "from": name, "seconds": secs, "source": sources[name]["page"]})
    print(f"{dest.stem}: {name}, {secs}s")

# The headphone-jack recording: announcements with silence between them.
delta = src / "delta.ogg"
gaps = run(["ffmpeg", "-i", str(delta), "-af", GAP, "-f", "null", "-"], text=True).stderr
starts = [float(x) for x in re.findall(r"silence_start: (-?[\d.]+)", gaps)]
ends = [float(x) for x in re.findall(r"silence_end: ([\d.]+)", gaps)]
total = duration(delta)
spans, at = [], 0.0
for s, e in zip(starts + [total], ends + [total]):
    if CLIP_MIN <= s - at <= CLIP_MAX:
        spans.append((at, s))
    at = e
# The file names (plane-pa-1..4 from this recording, then the other two) are
# what the page and CREDITS.md expect: a different split must be looked at.
assert len(spans) == 4, f"expected 4 announcements in delta.ogg, found {len(spans)}: {spans}"

clips =[(delta, f"atrim={max(0, a - 0.2):.2f}:{b + 0.3:.2f},asetpts=N/SR/TB", SPEAKER, "delta") for a, b in spans]
# The two recorded in the cabin are speaker sound already.
clips.append((src / "southwest.ogg", "", "", "southwest"))
clips.append((src / "speaker.ogg", "", "", "speaker"))
for n, (path, cut, extra, name) in enumerate(clips, 1):
    dest = out / f"plane-pa-{n}.opus"
    secs = speech(path, cut or "anull", dest, extra)
    manifest["announcements"].append({"file": dest.stem, "from": name, "seconds": secs, "source": sources[name]["page"]})
    print(f"{dest.stem}: {name}, {secs}s")

(out / "plane.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
