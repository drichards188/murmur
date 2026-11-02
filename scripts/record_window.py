#!/usr/bin/env python3
"""Record one X11 window to an H.264 mp4, outside the browser: the X server's copy of the
window is read with MIT-SHM (XShmGetImage, through ctypes and the system's libX11/libXext)
on a fixed frame clock and encoded with libx264 through PyAV. Nothing runs in the page, so
the capture costs the page's renderer nothing; it costs this process's CPU and a copy per
frame in the X server (Xwayland under WSLg).

    python scripts/record_window.py --name "Murmur" --seconds 90 --fps 60 -o logs/clip.mp4
    python scripts/record_window.py --id 0x600003 --seconds 10 -o logs/test.mp4
    python scripts/record_window.py --frames logs/video-frames -o logs/clip.mp4   # encode a screencast

For a browser on another display (Chrome on Windows driven from WSL), `console_stress.py video`
captures with CDP's screencast instead and `--frames` encodes what it wrote.

Needs PyAV (`pip install av`, its wheels carry libx264) and numpy; neither is a Murmur
dependency, so run it from any environment that has them. The window must be mapped and
unobscured (an X window's covered parts read as whatever covers them). Prints the frames
grabbed, the frames written (a grab slower than the clock repeats the previous frame) and
the grab time per frame, so a clip's real capture rate is known.

Used for the console stress clip (docs/load-test.md, "Console with the camera moving, on a real GPU").
"""

import argparse
import ctypes
import ctypes.util
import queue
import re
import subprocess
import sys
import threading
import time

import av
import numpy as np

X = ctypes.CDLL(ctypes.util.find_library("X11") or "libX11.so.6")
XEXT = ctypes.CDLL(ctypes.util.find_library("Xext") or "libXext.so.6")
LIBC = ctypes.CDLL(ctypes.util.find_library("c") or "libc.so.6", use_errno=True)

IPC_PRIVATE, IPC_CREAT, IPC_RMID = 0, 0o1000, 0
ZPixmap = 2


class XShmSegmentInfo(ctypes.Structure):
    _fields_ = [("shmseg", ctypes.c_ulong), ("shmid", ctypes.c_int), ("shmaddr", ctypes.c_void_p),
                ("readOnly", ctypes.c_int)]


class XImage(ctypes.Structure):
    _fields_ = [("width", ctypes.c_int), ("height", ctypes.c_int), ("xoffset", ctypes.c_int),
                ("format", ctypes.c_int), ("data", ctypes.c_void_p), ("byte_order", ctypes.c_int),
                ("bitmap_unit", ctypes.c_int), ("bitmap_bit_order", ctypes.c_int),
                ("bitmap_pad", ctypes.c_int), ("depth", ctypes.c_int), ("bytes_per_line", ctypes.c_int),
                ("bits_per_pixel", ctypes.c_int)]


class XWindowAttributes(ctypes.Structure):
    _fields_ = [("x", ctypes.c_int), ("y", ctypes.c_int), ("width", ctypes.c_int), ("height", ctypes.c_int),
                ("border_width", ctypes.c_int), ("depth", ctypes.c_int), ("visual", ctypes.c_void_p),
                ("root", ctypes.c_ulong), ("class", ctypes.c_int), ("bit_gravity", ctypes.c_int),
                ("win_gravity", ctypes.c_int), ("backing_store", ctypes.c_int),
                ("backing_planes", ctypes.c_ulong), ("backing_pixel", ctypes.c_ulong),
                ("save_under", ctypes.c_int), ("colormaps_installed", ctypes.c_int),
                ("map_installed", ctypes.c_int), ("map_state", ctypes.c_int),
                ("all_event_masks", ctypes.c_long), ("your_event_mask", ctypes.c_long),
                ("do_not_propagate_mask", ctypes.c_long), ("override_redirect", ctypes.c_int),
                ("colormap", ctypes.c_ulong), ("screen", ctypes.c_void_p)]


X.XOpenDisplay.restype = ctypes.c_void_p
X.XOpenDisplay.argtypes = [ctypes.c_char_p]
X.XGetWindowAttributes.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.POINTER(XWindowAttributes)]
X.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
XEXT.XShmCreateImage.restype = ctypes.POINTER(XImage)
XEXT.XShmCreateImage.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_char_p,
                                 ctypes.POINTER(XShmSegmentInfo), ctypes.c_uint, ctypes.c_uint]
XEXT.XShmAttach.argtypes = [ctypes.c_void_p, ctypes.POINTER(XShmSegmentInfo)]
XEXT.XShmDetach.argtypes = [ctypes.c_void_p, ctypes.POINTER(XShmSegmentInfo)]
XEXT.XShmGetImage.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.POINTER(XImage), ctypes.c_int, ctypes.c_int,
                              ctypes.c_ulong]
LIBC.shmget.argtypes = [ctypes.c_int, ctypes.c_size_t, ctypes.c_int]
LIBC.shmat.restype = ctypes.c_void_p
LIBC.shmat.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_int]
LIBC.shmdt.argtypes = [ctypes.c_void_p]
LIBC.shmctl.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]


def find_window(name: str) -> int:
    """The first top-level window whose title contains `name` (xwininfo's tree)."""
    tree = subprocess.run(["xwininfo", "-root", "-tree"], capture_output=True, text=True, check=True).stdout
    for line in tree.splitlines():
        m = re.match(r'\s*(0x[0-9a-f]+) "(.*?)":', line)
        if m and name in m.group(2):
            return int(m.group(1), 16)
    sys.exit(f"no window titled like {name!r}")


class Grabber:
    def __init__(self, wid: int):
        self.d = X.XOpenDisplay(None)
        if not self.d:
            sys.exit("cannot open the X display (DISPLAY)")
        self.wid = wid
        attrs = XWindowAttributes()
        X.XGetWindowAttributes(self.d, wid, ctypes.byref(attrs))
        self.w, self.h = attrs.width, attrs.height
        self.shm = XShmSegmentInfo()
        self.img = XEXT.XShmCreateImage(self.d, attrs.visual, attrs.depth, ZPixmap, None, ctypes.byref(self.shm),
                                        self.w, self.h)
        if not self.img:
            sys.exit("XShmCreateImage failed (no MIT-SHM?)")
        size = self.img.contents.bytes_per_line * self.h
        self.shm.shmid = LIBC.shmget(IPC_PRIVATE, size, IPC_CREAT | 0o600)
        self.shm.shmaddr = LIBC.shmat(self.shm.shmid, None, 0)
        self.shm.readOnly = 0
        self.img.contents.data = self.shm.shmaddr
        XEXT.XShmAttach(self.d, ctypes.byref(self.shm))
        X.XSync(self.d, 0)
        LIBC.shmctl(self.shm.shmid, IPC_RMID, None)  # freed once both sides detach
        self.bpl = self.img.contents.bytes_per_line
        self.buf = (ctypes.c_ubyte * size).from_address(self.shm.shmaddr)

    def grab(self) -> np.ndarray:
        if not XEXT.XShmGetImage(self.d, self.wid, self.img, 0, 0, 0xFFFFFFFF):
            raise RuntimeError("XShmGetImage failed (window resized or unmapped?)")
        a = np.frombuffer(self.buf, dtype=np.uint8).reshape(self.h, self.bpl // 4, 4)[:, : self.w, :]
        return a.copy()  # BGRA

    def close(self):
        XEXT.XShmDetach(self.d, ctypes.byref(self.shm))
        LIBC.shmdt(self.shm.shmaddr)


def encode_frames(d: str, out_path: str, fps: int, crf: int, preset: str, size=(1920, 1080)):
    """Encode `console_stress.py video` frames (JPEGs with the browser's timestamps) to a
    constant frame rate: each output frame is the latest captured one at its time, scaled to fit
    `size` and centred on black."""
    import json
    from pathlib import Path
    meta = json.loads((Path(d) / "frames.json").read_text())
    shots = meta["frames"]
    out = av.open(out_path, "w")
    st = out.add_stream("libx264", rate=fps, options={"crf": str(crf), "preset": preset, "tune": "film"})
    st.width, st.height, st.pix_fmt = size[0], size[1], "yuv420p"
    t0, t1 = shots[0][0], shots[-1][0]
    n_out = int((t1 - t0) * fps) + 1
    j, cur, used = 0, None, set()
    canvas = np.zeros((size[1], size[0], 3), dtype=np.uint8)
    for k in range(n_out):
        t = t0 + k / fps
        changed = False
        while j < len(shots) and shots[j][0] <= t:
            j += 1
            changed = True
        if changed or cur is None:
            name = shots[max(0, j - 1)][1]
            used.add(name)
            with av.open(str(Path(d) / name)) as c:
                img = next(c.decode(video=0))
            s = min(size[0] / img.width, size[1] / img.height)
            w, h = int(img.width * s) // 2 * 2, int(img.height * s) // 2 * 2
            arr = img.reformat(width=w, height=h, format="rgb24").to_ndarray()
            canvas[:] = 0
            y0, x0 = (size[1] - h) // 2, (size[0] - w) // 2
            canvas[y0:y0 + h, x0:x0 + w] = arr
            cur = av.VideoFrame.from_ndarray(canvas, format="rgb24")
        for p in st.encode(cur):
            out.mux(p)
    for p in st.encode():
        out.mux(p)
    out.close()
    print(f"{len(shots)} captured frames ({len(shots) / max(t1 - t0, 1e-9):.1f}/s over {t1 - t0:.1f} s), "
          f"{len(used)} shown, {n_out} written at {fps} fps, {size[0]}x{size[1]} -> {out_path}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--id", help="X window id (xwininfo -root -tree)")
    g.add_argument("--name", help="a substring of the window's title")
    g.add_argument("--frames", help="encode a directory of `console_stress.py video` frames instead of recording")
    ap.add_argument("--seconds", type=float, default=10)
    ap.add_argument("--fps", type=int, default=60)
    ap.add_argument("--crf", type=int, default=18)
    ap.add_argument("--preset", default="veryfast")
    ap.add_argument("--png", help="write the first frame as a PNG here and exit")
    ap.add_argument("-o", "--out", default="logs/window.mp4")
    a = ap.parse_args()
    if a.frames:
        encode_frames(a.frames, a.out, a.fps, a.crf, a.preset)
        return
    wid = int(a.id, 16) if a.id else find_window(a.name)
    gr = Grabber(wid)
    w, h = gr.w - gr.w % 2, gr.h - gr.h % 2
    if a.png:
        c = av.open(a.png, "w")
        s = c.add_stream("png", rate=1)
        s.width, s.height, s.pix_fmt = w, h, "rgb24"
        f = av.VideoFrame.from_ndarray(gr.grab()[:h, :w], format="bgra").reformat(format="rgb24")
        for p in s.encode(f):
            c.mux(p)
        for p in s.encode():
            c.mux(p)
        c.close()
        print(f"window 0x{wid:x} {gr.w}x{gr.h}: wrote {a.png}")
        return
    out = av.open(a.out, "w")
    st = out.add_stream("libx264", rate=a.fps, options={"crf": str(a.crf), "preset": a.preset, "tune": "film"})
    st.width, st.height, st.pix_fmt = w, h, "yuv420p"
    st.thread_count = 8
    q: queue.Queue = queue.Queue(maxsize=a.fps * 2)
    dropped = [0]

    def encoder():
        while True:
            item = q.get()
            if item is None:
                break
            frame = av.VideoFrame.from_ndarray(item[:h, :w], format="bgra")
            for p in st.encode(frame):
                out.mux(p)
        for p in st.encode():
            out.mux(p)

    th = threading.Thread(target=encoder)
    th.start()
    period = 1.0 / a.fps
    n_total = round(a.seconds * a.fps)
    grabs, grab_ms, last = 0, [], None
    t0 = time.perf_counter()
    for i in range(n_total):
        due = t0 + i * period
        now = time.perf_counter()
        if now < due:
            time.sleep(due - now)
        if time.perf_counter() - due < period:  # on time: grab a fresh frame, else repeat the last
            g0 = time.perf_counter()
            last = gr.grab()
            grab_ms.append((time.perf_counter() - g0) * 1000)
            grabs += 1
        try:
            q.put_nowait(last)
        except queue.Full:
            dropped[0] += 1
            q.put(last)
    q.put(None)
    th.join()
    out.close()
    gr.close()
    el = time.perf_counter() - t0
    gm = sorted(grab_ms)
    print(f"window 0x{wid:x} {w}x{h}, {n_total} frames at {a.fps} fps ({el:.1f} s wall): {grabs} fresh grabs "
          f"({grabs / el:.1f}/s), {n_total - grabs} repeats, encoder waits {dropped[0]}; grab {gm[len(gm) // 2]:.1f} ms "
          f"median, {gm[int(len(gm) * 0.95)]:.1f} ms p95 -> {a.out}")


if __name__ == "__main__":
    main()
