#!/usr/bin/env python3
"""Render every view, the concept contact sheet, the STLs and the 3D viewer page in one go.

    python3 enclosure/render.py            # everything
    python3 enclosure/render.py --no-stl   # pictures only
    python3 enclosure/render.py --only dog_   # just the shells whose names start with dog_

Writes PNGs to enclosure/renders/, print-ready STLs to stl/ (one per colour for each shell), and
enclosure/viewer.html: a single self-contained page (images and models embedded) that opens straight
from disk. The shell list, titles and palettes come from main.scad. Needs openscad and ImageMagick;
on a headless machine, xvfb-run.
"""
import base64, json, os, shutil, struct, subprocess, sys
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
MAIN = os.path.join(HERE, "main.scad")
OUT = os.path.join(HERE, "renders")
TMP = os.path.join(OUT, "tmp")
STL = os.path.join(ROOT, "stl")
CORE_COLOR = "#3a3d45"

# Views are rendered turned 180 degrees about z (turn=true), so the node's front faces OpenSCAD's
# front (-y). Cameras are gimbal (translate, rotate, distance): with eye/centre cameras OpenSCAD
# 2021.01 lights the faces towards the viewer black.
VIEWS = {
    "front": ("assembly", "0,0,38,72,0,22,330", True),
    "back": ("assembly", "0,0,38,66,0,204,330", True),
    "side": ("assembly", "0,0,38,80,0,90,330", True),
    "exploded": ("exploded", "0,-20,38,64,0,40,400", True),
    "section": ("section", "0,0,0,90,0,0,150", False),  # through the snap tabs, from above
}


def run(cmd):
    if cmd[-1].endswith(".scad") and any(a.endswith(".png") for a in cmd) and not os.environ.get("DISPLAY") and shutil.which("xvfb-run"):
        cmd = ["xvfb-run", "-a"] + cmd
    return subprocess.run(cmd, capture_output=True, text=True)


def openscad(args, out, src=MAIN):
    r = run(["openscad", "-o", out] + args + [src])
    bad = [l for l in r.stderr.splitlines() if l.startswith(("WARNING", "ERROR"))]
    if r.returncode or bad or not os.path.exists(out):
        print(f"!! {os.path.basename(out)}:\n  " + "\n  ".join(bad or r.stderr.splitlines()[-5:]))
    return out


def shells():
    """[(name, title, [4 colours])] from main.scad's table."""
    r = run(["openscad", "-o", os.path.join(TMP, "palette.echo"), "-D", 'part="palette"', MAIN])
    out = []
    for line in open(os.path.join(TMP, "palette.echo")):
        if "SHELL|" in line:
            f = line.strip().strip('ECHO: "').rstrip('"').split("|")
            out.append((f[1], f[2], f[3:7]))
    return out


def fill(path, size):
    """Trim to the model and centre it with a margin, so every view fills its frame."""
    w, h = size.split(",")
    subprocess.run(["mogrify", "-fuzz", "3%", "-trim", "+repage", "-resize", f"{int(w) - 80}x{int(h) - 80}",
                    "-background", "#f8f8f8", "-gravity", "center", "-extent", f"{w}x{h}", path], check=True)
    return path


def png(shell, view, size="900,700"):
    w, h = map(int, size.split(","))
    return fill(_png(shell, view, f"{2 * w},{2 * h}"), size)   # render big, then trim and shrink


def _png(shell, view, size):
    part, cam, fit = VIEWS[view]
    out = os.path.join(OUT, f"{shell}_{view}.png")
    if view == "section":
        return section(shell, cam, size, out)
    return openscad(["-D", f'part="{part}"', "-D", f'shell="{shell}"', "-D", "turn=true", f"--camera={cam}"]
                    + (["--viewall", "--autocenter"] if fit else [])
                    + [f"--imgsize={size}", "--colorscheme=Tomorrow", "--projection=p"], out)


def section(shell, cam, size, out):
    parts = {"core": CORE_COLOR, "hw": "#a8a8b2", "shell": PALETTES[shell][0]}
    files = {}
    for p in parts:
        files[p] = os.path.join(TMP, f"{shell}_sec_{p}.stl")
        openscad(["-D", f'part="sec_{p}"', "-D", f'shell="{shell}"', "-D", "turn=true"], files[p])
    scad = os.path.join(TMP, f"{shell}_section.scad")
    with open(scad, "w") as f:
        for p, c in parts.items():
            f.write(f'color("{c}") import("{files[p]}");\n')
    return openscad([f"--camera={cam}", "--viewall", "--autocenter", f"--imgsize={size}", "--colorscheme=Tomorrow", "--projection=o"], out, scad)


def stl(path, part, shell="owl", colour=0):
    return openscad(["-D", f'part="{part}"', "-D", f'shell="{shell}"', "-D", f"colour={colour}"], path)


def to_binary_stl(path):
    """OpenSCAD 2021.01 writes ASCII STL; pack it as binary for embedding."""
    tris, cur = [], []
    with open(path) as f:
        for line in f:
            w = line.split()
            if w and w[0] == "vertex":
                cur.append(tuple(map(float, w[1:4])))
                if len(cur) == 3:
                    tris.append(cur); cur = []
    b = bytearray(80) + struct.pack("<I", len(tris))
    for t in tris:
        b += struct.pack("<12fH", 0, 0, 0, *t[0], *t[1], *t[2], 0)
    return bytes(b)


def montage(items, out, cols, geometry, pointsize):
    args = ["montage"]
    for label, f in items:
        args += ["-label", label, f]
    subprocess.run(args + ["-tile", f"{cols}x", "-geometry", geometry, "-pointsize", str(pointsize), "-background", "white", out], check=True)


def b64(path, mime):
    with open(path, "rb") as f:
        return f"data:{mime};base64," + base64.b64encode(f.read()).decode()


def viewer(images, models):
    page = open(os.path.join(HERE, "viewer.template.html")).read()
    imgs = [{"title": t, "src": b64(p, "image/png")} for t, p in images]
    mods = [{"group": g, "name": n, "color": c, "data": base64.b64encode(to_binary_stl(p)).decode()} for g, n, c, p in models if os.path.exists(p)]
    page = page.replace("/*IMAGES*/[]", json.dumps(imgs)).replace("/*MODELS*/[]", json.dumps(mods))
    with open(os.path.join(HERE, "viewer.html"), "w") as f:
        f.write(page)


def main():
    global PALETTES
    for d in (OUT, TMP, STL):
        os.makedirs(d, exist_ok=True)
    SH = shells()
    if "--only" in sys.argv:
        pre = sys.argv[sys.argv.index("--only") + 1]
        SH = [x for x in SH if x[0].startswith(pre)]
    PALETTES = {n: p for n, _, p in SH}
    global NCOL
    NCOL = {n: len(dict.fromkeys(p)) for n, _, p in SH}   # a palette that repeats one colour is a one-colour shell
    names = [n for n, _, _ in SH]
    titles = {n: t for n, t, _ in SH}
    with ThreadPoolExecutor(os.cpu_count() or 4) as ex:
        [j.result() for j in [ex.submit(png, s, v) for s in names for v in VIEWS]]
    for v in VIEWS:
        montage([(titles[s], os.path.join(OUT, f"{s}_{v}.png")) for s in names], os.path.join(OUT, f"sheet_{v}.png"), 3, "900x700+6+6", 30)
    montage([(f"{titles[s]}: {v}", os.path.join(OUT, f"{s}_{v}.png")) for s in names for v in VIEWS],
            os.path.join(OUT, "concepts.png"), len(VIEWS), "450x350+4+4", 18)
    models = []
    if "--no-stl" not in sys.argv:
        with ThreadPoolExecutor(os.cpu_count() or 4) as ex:
            jobs = [ex.submit(stl, os.path.join(STL, "core_placeholder.stl"), "core"),
                    ex.submit(stl, os.path.join(TMP, "hardware.stl"), "hardware")]
            for s in names:
                for c in range(1, NCOL[s] + 1):
                    jobs.append(ex.submit(stl, os.path.join(STL, f"shell_{s}_c{c}.stl"), "shell", s, c))     # print, face down
                    jobs.append(ex.submit(stl, os.path.join(TMP, f"shell_{s}_c{c}_view.stl"), "shell_view", s, c))
            [j.result() for j in jobs]
        models = [("inside", "core (placeholder)", CORE_COLOR, os.path.join(STL, "core_placeholder.stl")),
                  ("inside", "hardware", "#a8a8b2", os.path.join(TMP, "hardware.stl"))]
        models += [(titles[s], f"colour {c}", PALETTES[s][c - 1], os.path.join(TMP, f"shell_{s}_c{c}_view.stl")) for s in names for c in range(1, NCOL[s] + 1)]
    images = [("All concepts, all views", os.path.join(OUT, "concepts.png"))]
    images += [(f"All concepts: {v}", os.path.join(OUT, f"sheet_{v}.png")) for v in VIEWS]
    viewer(images, models)
    print("done:", os.path.join(HERE, "viewer.html"))


if __name__ == "__main__":
    main()
