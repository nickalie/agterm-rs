#!/usr/bin/env python3
"""Scripted acceptance checks of the gpui-term spike (docs/spikes/20261014-gpui-term.md).

  python3 spikes/gpui-term/check.py [--bin PATH] [--keep DIR] [--only NAME,...] [--big-mb N]

Each check launches the spike in a private headless weston (GL renderer on Mesa's EGL, the window drawn with Mesa's
lavapipe Vulkan from LAVAPIPE_ICD), a fresh root under /tmp holding the runtime dir, HOME, ZMX_DIR and the control
socket, AGTERM_*/ZMX_* stripped from the environment. The pane runs `zmx attach` under that private ZMX_DIR. The
spike is driven through its control socket (`key`, `type`, `preedit`, `scroll`, `resize`, `dump`), screenshots come
from weston-screenshooter. Prints a JSON report; `--keep` saves screenshots and dumps.

`--gtk-baseline STAGE` runs the throughput check against the staged GTK build instead, through the conformance
adapter (same headless setup, Mesa EGL instead of lavapipe: the GTK build draws with GL).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, os.fspath(REPO / "conformance"))

from agterm_conformance.impls.base import start_compositor, stop_headless  # noqa: E402

DEFAULT_BIN = HERE / "target" / "release" / "gpui-term-spike"
VENDOR = Path(os.environ.get("AGTERM_VENDOR", Path.home() / ".cache" / "agterm-rs" / "vendor"))
LAVAPIPE_ICD = Path(os.environ.get("LAVAPIPE_ICD", VENDOR / "lavapipe-26.2.4" / "lvp_icd.json"))
ZMX = Path(os.environ.get("SPIKE_ZMX", next(iter(sorted(VENDOR.glob("zmx-*/bin/zmx"))), "zmx")))
HTOP = Path(os.environ.get("SPIKE_HTOP", VENDOR / "htop-3.5.3" / "usr" / "bin" / "htop"))
WINDOW = (1280, 800)
PROMPT = "spike$ "
# the dump trims trailing blanks, so the prompt matches without its space
PROMPT_RE = r"(?m)^spike\$"
PROMPT_LAST = r"spike\$\s*\Z"


class CheckError(Exception):
    pass


# -- PNG


def write_png(path: Path, width: int, height: int, pixel) -> None:
    rows = b"".join(b"\0" + b"".join(bytes(pixel(x, y)) for x in range(width)) for y in range(height))

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(rows))
                     + chunk(b"IEND", b""))


def read_png(data: bytes) -> tuple[int, int, int, bytes]:
    """(width, height, bytes per pixel, unfiltered rows) of an 8-bit RGB/RGBA PNG."""
    pos, idat, width = 8, b"", 0
    height = bpp = 0
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        kind, body = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            width, height, _, color = struct.unpack(">IIBB", body[:10])
            bpp = 4 if color == 6 else 3
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    raw, stride, out, prior = zlib.decompress(idat), width * bpp, bytearray(), bytearray(width * bpp)
    for y in range(height):
        kind, line = raw[y * (stride + 1)], bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b, c = prior[i], prior[i - bpp] if i >= bpp else 0
            if kind == 1:
                line[i] = (line[i] + a) & 255
            elif kind == 2:
                line[i] = (line[i] + b) & 255
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 255
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        out += line
        prior = line
    return width, height, bpp, bytes(out)


def count_color(png: tuple[int, int, int, bytes], color: tuple[int, int, int], tolerance: int = 12) -> int:
    width, height, bpp, pixels = png
    hits = 0
    for i in range(0, width * height * bpp, bpp):
        if all(abs(pixels[i + k] - color[k]) <= tolerance for k in range(3)):
            hits += 1
    return hits


# -- the headless session


class Headless:
    """A private headless weston (debug protocol on, so weston-screenshooter can read the output)."""

    def __init__(self, root: Path) -> None:
        self.root = root
        run = root / "run"
        run.mkdir(mode=0o700)
        home = root / "home"
        home.mkdir()
        self.env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": os.fspath(home),
            "XDG_RUNTIME_DIR": os.fspath(run),
            "XDG_CONFIG_HOME": os.fspath(home / ".config"),
            "XDG_DATA_HOME": os.fspath(home / ".local/share"),
            "XDG_STATE_HOME": os.fspath(home / ".local/state"),
            "XDG_CACHE_HOME": os.fspath(home / ".cache"),
            "LANG": "C.UTF-8",
            "TERM": "dumb",
        }
        width, height = WINDOW
        # --fake-seat: GPUI panics on a compositor without wl_seat ("Wayland compositor does not provide wl_seat"),
        # and weston's headless backend has none by default.
        self.compositor = start_compositor(root, self.env, extra=("--debug", "--fake-seat", f"--width={width + 200}",
                                                                   f"--height={height + 200}"))
        self.env["WAYLAND_DISPLAY"] = os.path.join(self.env["XDG_RUNTIME_DIR"], "wayland-0")
        self.shots = root / "shots"
        self.shots.mkdir()

    def screenshot(self, keep: Path | None, label: str) -> tuple[int, int, int, bytes]:
        before = set(self.shots.iterdir())
        env = {k: self.env[k] for k in ("PATH", "XDG_RUNTIME_DIR", "WAYLAND_DISPLAY")}
        done = subprocess.run(["weston-screenshooter"], cwd=self.shots, env=env, capture_output=True, text=True,
                              timeout=30)
        taken = sorted(set(self.shots.iterdir()) - before)
        if done.returncode != 0 or len(taken) != 1:
            raise CheckError(f"weston-screenshooter: {done.returncode} {done.stderr!r} {taken}")
        data = taken[0].read_bytes()
        if keep is not None:
            keep.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(taken[0], keep / f"{label}.png")
        taken[0].unlink()
        return read_png(data)

    def stop(self) -> str | None:
        return stop_headless(self.compositor, None, self.root)


class Spike:
    """The spike binary in the headless session, with its control socket."""

    def __init__(self, binary: Path, session: Headless, name: str, command: list[str] | None = None,
                 zmx: bool = True) -> None:
        self.session = session
        self.zmx_dir = session.root / "zmx"
        self.zmx_dir.mkdir(exist_ok=True, mode=0o700)
        self.control = session.root / f"{name}.sock"
        self.name = name
        env = dict(session.env)
        env.update({
            "ZMX_DIR": os.fspath(self.zmx_dir),
            "VK_DRIVER_FILES": os.fspath(LAVAPIPE_ICD),
            "VK_ICD_FILENAMES": os.fspath(LAVAPIPE_ICD),
            "SHELL": "/bin/bash",
            "RUST_LOG": os.environ.get("RUST_LOG", "info"),
            **({"SPIKE_TRACE": "1"} if os.environ.get("SPIKE_TRACE") else {}),
        })
        rc = session.root / "spike.bashrc"
        rc.write_text(f"PS1='{PROMPT}'\nunset PROMPT_COMMAND\n")
        shell = command or ["/bin/bash", "--noprofile", "--rcfile", os.fspath(rc), "-i"]
        argv = [os.fspath(binary), "--control", os.fspath(self.control), "--width", str(WINDOW[0]),
                "--height", str(WINDOW[1]), *(["--zmx", os.fspath(ZMX)] if zmx else []), "--session", name, "--", *shell]
        self.log = (session.root / f"{name}.log").open("wb")
        self.process = subprocess.Popen(argv, env=env, stdout=self.log, stderr=subprocess.STDOUT,
                                        start_new_session=True)
        deadline = time.monotonic() + 60
        while not self.control.exists():
            if self.process.poll() is not None:
                raise CheckError(f"spike exited with {self.process.returncode}:\n{self.log_tail()}")
            if time.monotonic() > deadline:
                raise CheckError(f"no control socket after 60s:\n{self.log_tail()}")
            time.sleep(0.05)

    def log_tail(self, lines: int = 30) -> str:
        text = (self.session.root / f"{self.name}.log").read_text(errors="replace")
        return "\n".join(text.splitlines()[-lines:])

    def call(self, line: str, timeout: float = 30) -> str:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(timeout)
            sock.connect(os.fspath(self.control))
            sock.sendall(line.encode() + b"\n")
            data = b""
            while not data.endswith(b"\n"):
                part = sock.recv(1 << 20)
                if not part:
                    break
                data += part
        reply = data.decode().rstrip("\n")
        if not reply.startswith("ok"):
            raise CheckError(f"{line!r}: {reply}")
        return reply[3:]

    def dump(self) -> dict[str, Any]:
        return json.loads(self.call("dump"))

    def wait(self, pattern: str, timeout: float = 20, what: str = "text") -> dict[str, Any]:
        regex = re.compile(pattern, re.M)
        deadline = time.monotonic() + timeout
        while True:
            state = self.dump()
            if regex.search(state[what]):
                return state
            if time.monotonic() > deadline:
                meta = {k: v for k, v in state.items() if k != what}
                raise CheckError(f"{pattern!r} not on screen after {timeout}s; state {meta}; screen:\n{state[what].rstrip()}")
            time.sleep(0.05)

    def type(self, text: str) -> None:
        self.call("type " + json.dumps(text))

    def cpu_seconds(self) -> float:
        return cpu_seconds(self.process.pid)

    def stop(self) -> None:
        if self.process.poll() is None:
            try:
                self.call("quit", timeout=5)
            except (OSError, CheckError):
                pass
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait()
        self.log.close()
        reap_zmx(self.zmx_dir)


def cpu_seconds(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def reap_zmx(zmx_dir: Path) -> None:
    """Kill the zmx processes carrying this ZMX_DIR, and only those."""
    marker = f"ZMX_DIR={zmx_dir}".encode()
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            if marker in (proc / "environ").read_bytes().split(b"\0") and b"zmx" in (proc / "comm").read_bytes():
                os.kill(int(proc.name), signal.SIGKILL)
        except (OSError, ProcessLookupError):
            continue


# -- checks


def check_vim(spike: Spike, keep: Path | None, workdir: Path) -> dict[str, Any]:
    target = workdir / "vim.txt"
    spike.wait(PROMPT_RE)
    spike.type(f"vim -u NONE -N {target}\n")
    spike.wait(r"^~\s*$")
    spike.type("ihello from gpui")
    spike.call("key escape")
    spike.wait(r"hello from gpui")
    spike.type("yyp")
    shot = spike.session.screenshot(keep, "vim")
    spike.type(":wq\n")
    spike.wait(PROMPT_LAST)
    written = target.read_text() if target.exists() else ""
    return {"ok": written == "hello from gpui\nhello from gpui\n", "file": written, "shot": shot[:2]}


def check_htop(spike: Spike, keep: Path | None, _workdir: Path) -> dict[str, Any]:
    spike.wait(PROMPT_RE)
    spike.type(f"{HTOP}\n")
    state = spike.wait(r"(Load average|Tasks:)", timeout=30)
    time.sleep(1.5)
    state = spike.dump()
    shot = spike.session.screenshot(keep, "htop")
    spike.call("key f10")
    after = spike.wait(PROMPT_LAST)
    return {"ok": "CPU" in state["text"] and "Mem" in state["text"], "altScreenLeft": "spike$" in after["text"],
            "shot": shot[:2]}


IMAGE_COLOR = (230, 40, 160)


def check_kitty(spike: Spike, keep: Path | None, workdir: Path) -> dict[str, Any]:
    """The APC sequence show-image.sh emits: a=T, f=100 (PNG), a c=/r= cell box, C=1, base64 in 4096-byte chunks."""
    image = workdir / "kitty.png"
    write_png(image, 96, 64, lambda x, y: (*IMAGE_COLOR, 255))
    script = workdir / "kitty.sh"
    script.write_text(r"""#!/bin/bash
clear
printf '\n\n'
printf '\033[4C'
data=$(base64 < "$1" | tr -d '\n')
i=0; n=${#data}; first=1
while [ "$i" -lt "$n" ]; do
  chunk=${data:i:4096}; i=$((i + 4096))
  if [ "$i" -ge "$n" ]; then m=0; else m=1; fi
  if [ "$first" = 1 ]; then printf '\033_Ga=T,f=100,c=20,r=8,C=1,m=%d;%s\033\\' "$m" "$chunk"; first=0
  else printf '\033_Gm=%d;%s\033\\' "$m" "$chunk"; fi
done
printf '\033[12;1Himage-shown\n'
""")
    spike.wait(PROMPT_RE)
    spike.type(f"bash {script} {image}\n")
    state = spike.wait(r"image-shown")
    time.sleep(0.5)
    shot = spike.session.screenshot(keep, "kitty")
    hits = count_color(shot, IMAGE_COLOR)
    cell = state["cell"]
    expected = 20 * cell[0] * 8 * cell[1]
    return {"ok": abs(hits - expected) <= expected * 0.1, "pixels": hits, "expected": expected,
            "images": state.get("images")}


def check_ime(spike: Spike, keep: Path | None, _workdir: Path) -> dict[str, Any]:
    spike.wait(PROMPT_RE)
    spike.type("echo ")
    spike.call("preedit " + json.dumps("にほんご"))
    time.sleep(0.3)
    marked = spike.dump()
    shot = spike.session.screenshot(keep, "ime-preedit")
    spike.call("commit " + json.dumps("日本語"))
    spike.type("\n")
    state = spike.wait(r"^日本語\s*$")
    return {"ok": marked["preedit"] == "にほんご" and "にほんご" not in state["text"], "preeditShown": marked["preedit"],
            "shot": shot[:2]}


def check_scroll(spike: Spike, keep: Path | None, _workdir: Path) -> dict[str, Any]:
    spike.wait(PROMPT_RE)
    spike.type("seq 1 400\n")
    bottom = spike.wait(r"^400\s*$")
    spike.call("scroll 10")
    time.sleep(0.2)
    scrolled = spike.dump()
    spike.session.screenshot(keep, "scroll")
    spike.call("scroll -10")
    time.sleep(0.2)
    back = spike.dump()
    first = lambda state: state["text"].splitlines()[0].strip()  # noqa: E731
    moved = int(first(bottom)) - int(first(scrolled)) if first(scrolled).isdigit() and first(bottom).isdigit() else None
    return {"ok": moved is not None and moved > 0 and first(back) == first(bottom), "linesMoved": moved,
            "topBefore": first(bottom), "topScrolled": first(scrolled)}


def check_resize(spike: Spike, keep: Path | None, _workdir: Path) -> dict[str, Any]:
    spike.wait(PROMPT_RE)
    before = spike.dump()
    spike.call("resize 900 500")
    deadline = time.monotonic() + 10
    while (state := spike.dump())["cols"] == before["cols"] and time.monotonic() < deadline:
        time.sleep(0.1)
    spike.type("stty size\n")
    reported = spike.wait(r"^(\d+) (\d+)\s*$")
    rows, cols = map(int, re.search(r"^(\d+) (\d+)\s*$", reported["text"], re.M).groups())
    spike.session.screenshot(keep, "resize")
    return {"ok": (rows, cols) == (state["rows"], state["cols"]) and state["cols"] < before["cols"],
            "before": [before["rows"], before["cols"]], "vt": [state["rows"], state["cols"]], "pty": [rows, cols]}


def make_big(path: Path, megabytes: int) -> None:
    if path.exists() and path.stat().st_size >= megabytes << 20:
        return
    line_no, written = 0, 0
    with path.open("w") as out:
        while written < megabytes << 20:
            line_no += 1
            if line_no % 10 == 0:
                line = f"\033[3{line_no % 8}m{line_no:08d}\033[0m colored line with some text to wrap around the grid\n"
            else:
                line = f"{line_no:08d} the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnop\n"
            out.write(line)
            written += len(line)


TPUT = r"^TPUT (\d+\.\d+)\s*$"


def check_throughput(spike: Spike, keep: Path | None, workdir: Path, big: Path) -> dict[str, Any]:
    spike.wait(PROMPT_RE)
    spike.type("clear\n")
    cpu0, t0 = spike.cpu_seconds(), time.monotonic()
    spike.type(f"TIMEFORMAT='TPUT %R'; time cat {big}\n")
    found = spike.wait(TPUT, timeout=600)
    wall, cpu = time.monotonic() - t0, spike.cpu_seconds() - cpu0
    spike.session.screenshot(keep, "throughput")
    shell_time = float(re.search(TPUT, found["text"], re.M).group(1))
    return {"ok": True, "catSeconds": shell_time, "wallSeconds": round(wall, 2), "cpuSeconds": round(cpu, 2),
            "frames": found.get("frames"), "framesBuilt": found.get("framesBuilt"),
            "bytesIntoVt": found.get("bytes"), "vtSeconds": found.get("vtSeconds"),
            "frameBuildSeconds": found.get("frameBuildSeconds"),
            "megabytes": big.stat().st_size / (1 << 20)}


CHECKS = {"vim": check_vim, "htop": check_htop, "kitty": check_kitty, "ime": check_ime, "scroll": check_scroll,
          "resize": check_resize, "throughput": None, "throughput-plain": None}


def run_spike(args: argparse.Namespace) -> dict[str, Any]:
    report: dict[str, Any] = {"bin": os.fspath(args.bin), "checks": {}}
    names = args.only.split(",") if args.only else list(CHECKS)
    workdir = Path(tempfile.mkdtemp(prefix="gpui-term-work-"))
    big = Path(tempfile.gettempdir()) / f"gpui-term-big-{args.big_mb}m.txt"
    for name in names:
        root = Path(tempfile.mkdtemp(prefix="gpui-term-spike-", dir="/tmp"))
        session = Headless(root)
        spike = None
        try:
            spike = Spike(args.bin, session, f"s-{name}", zmx=name != "throughput-plain")
            if name.startswith("throughput"):
                make_big(big, args.big_mb)
                result = check_throughput(spike, args.keep, workdir, big)
            else:
                result = CHECKS[name](spike, args.keep, workdir)
        except Exception as error:  # noqa: BLE001 - the report names whatever failed
            result = {"ok": False, "error": f"{type(error).__name__}: {error}"}
            if spike is not None:
                result["log"] = spike.log_tail()
        finally:
            if spike is not None:
                spike.stop()
                if args.keep is not None:
                    args.keep.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(root / f"s-{name}.log", args.keep / f"{name}.log")
            if problem := session.stop():
                result["stopProblem"] = problem
            shutil.rmtree(root, ignore_errors=True)
        report["checks"][name] = result
        print(f"{name}: {'ok' if result.get('ok') else 'FAIL'}", file=sys.stderr)
    shutil.rmtree(workdir, ignore_errors=True)
    report["ok"] = all(check.get("ok") for check in report["checks"].values())
    return report


def run_gtk_baseline(args: argparse.Namespace) -> dict[str, Any]:
    """`time cat` of the same file in the staged GTK build, through the conformance adapter."""
    from agterm_conformance.impls.rust import RustAdapter
    from agterm_conformance.match import active_session_id, assert_ok

    def wait_text(instance, session, pattern, timeout):
        """session.text until `pattern` matches; 'session not realized' while the pane comes up is retried."""
        regex, deadline = re.compile(pattern, re.M), time.monotonic() + timeout
        while True:
            response = instance.request("session.text", session)
            text = (response.result or {}).get("text", "") if response.ok else ""
            if found := regex.search(text):
                return found
            if time.monotonic() > deadline:
                raise CheckError(f"{pattern!r} not in session.text after {timeout}s: {response.raw}")
            time.sleep(0.05)

    big = Path(tempfile.gettempdir()) / f"gpui-term-big-{args.big_mb}m.txt"
    make_big(big, args.big_mb)
    adapter = RustAdapter(args.gtk_baseline / "bin" / "agterm")
    instance = adapter.launch()
    try:
        session = active_session_id(assert_ok(instance.request("tree")))
        wait_text(instance, session, r"\S", 30)
        assert_ok(instance.request("session.type", session, text="clear\n"))
        time.sleep(1)
        cpu0, t0 = cpu_seconds(instance.pid), time.monotonic()
        assert_ok(instance.request("session.type", session, text=f"TIMEFORMAT='TPUT %R'; time cat {big}\n"))
        found = wait_text(instance, session, TPUT, 600)
        wall, cpu = time.monotonic() - t0, cpu_seconds(instance.pid) - cpu0
        return {"ok": True, "catSeconds": float(found.group(1)), "wallSeconds": round(wall, 2),
                "cpuSeconds": round(cpu, 2), "megabytes": big.stat().st_size / (1 << 20)}
    finally:
        adapter.stop(instance)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--bin", type=Path, default=DEFAULT_BIN)
    parser.add_argument("--keep", type=Path)
    parser.add_argument("--only")
    parser.add_argument("--big-mb", type=int, default=50)
    parser.add_argument("--gtk-baseline", type=Path, metavar="STAGE")
    args = parser.parse_args(argv)
    report = run_gtk_baseline(args) if args.gtk_baseline else run_spike(args)
    print(json.dumps(report, indent=2, default=repr))
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
