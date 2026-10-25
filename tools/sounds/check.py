"""Seam check: the level just before the loop's end vs just after its start,
and the sample jump across the seam vs typical neighbouring-sample steps."""
import array, json, math, pathlib, subprocess, sys

out = pathlib.Path(sys.argv[1])
RATE = 48000
for sid, m in json.loads((out / "manifest.json").read_text(encoding="utf-8")).items():
    raw = subprocess.run(["ffmpeg", "-v", "error", "-i", str(out / m["file"]), "-ac", "1", "-ar", str(RATE), "-f", "s16le", "-"],
                         check=True, capture_output=True).stdout
    pcm = array.array("h"); pcm.frombytes(raw[: len(raw) // 2 * 2])
    q = RATE // 4
    rms = lambda xs: 10 * math.log10(sum(v * v for v in xs) / len(xs) + 1e-9)
    head, tail = rms(pcm[:q]), rms(pcm[-q:])
    steps = sorted(abs(pcm[i + 1] - pcm[i]) for i in range(0, len(pcm) - 1, 97))
    typical = steps[int(len(steps) * 0.99)]
    jump = abs(pcm[0] - pcm[-1])
    lufs = subprocess.run(["ffmpeg", "-v", "info", "-i", str(out / m["file"]), "-af", "ebur128", "-f", "null", "-"],
                          capture_output=True, text=True).stderr
    i_line = [l for l in lufs.splitlines() if l.strip().startswith("I:")][-1].split()[1]
    print(f"{sid:13} level end {tail:6.1f} / start {head:6.1f} dB, seam jump {jump:5d} vs p99 step {typical:5d}, {i_line} LUFS")
