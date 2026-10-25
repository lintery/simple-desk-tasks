"""Downloads the Freesound previews listed in sources.json (the loops) and
plane.json (the plane's chimes and announcements) into <out>/<name>.ogg."""
import json, pathlib, sys, urllib.request

here = pathlib.Path(__file__).parent
out = pathlib.Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)
sources = json.loads((here / "sources.json").read_text(encoding="utf-8"))
plane = json.loads((here / "plane.json").read_text(encoding="utf-8"))
sources.update({k: v for k, v in plane.items() if k != "cabin"})  # the cabin is sources.json's "plane"
for sid, s in sources.items():
    dest = out / f"{sid}.ogg"
    if dest.exists() and dest.stat().st_size > 0:
        print(f"{sid}: have {dest.stat().st_size} bytes")
        continue
    req = urllib.request.Request(s["url"], headers={"User-Agent": "SimpleDeskTasks-sound-prep"})
    part = dest.with_suffix(".part")  # an interrupted download isn't taken for a finished one
    with urllib.request.urlopen(req, timeout=120) as r, open(part, "wb") as f:
        f.write(r.read())
    part.replace(dest)
    print(f"{sid}: {dest.stat().st_size} bytes")
