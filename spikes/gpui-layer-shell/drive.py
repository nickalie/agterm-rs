#!/usr/bin/env python3
"""Drives the spike's scripted checks and keeps their RESULT lines in results/<run>.jsonl.

Runs (each isolated from the user's live agterm: AGTERM_*/ZMX_* stripped, a private session bus, its own app id,
nothing touches agterm's socket or state):

  quick-real        the quick panel's life cycle on the user's niri session (layer-shell, both outputs)
  quick-weston      the same on a private headless weston, which has no layer-shell: the fallback path
  quick-nested      the same on a private niri nested in a private headless weston
  activation-nested window activation on that nested niri: a decoy takes focus, then GPUI's own activate_window,
                    then second launches (GApplication and GtkApplication) with and without an xdg-activation token
  activation-real   the activation run on the user's niri session (only while the session is unlocked)
  quick-x11         the quick panel's life cycle on GPUI's X11 backend, on a private Xvfb (no window manager)
  activation-x11    the activation run there (a startup-notification id instead of a token)

usage: python3 -I drive.py RUN [RUN...]
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
BIN = HERE / "target" / "release" / "gpui-layer-shell-spike"
RESULTS = HERE / "results"
APP_ID = "io.github.melonamin.agterm.SpikeT3"
MESA_EGL = "/usr/share/glvnd/egl_vendor.d/50_mesa.json"
INTEL_ICD = "/usr/share/vulkan/icd.d/intel_icd.json"
WESTON_INI = "[core]\nidle-time=0\n\n[shell]\npanel-position=none\nlocking=false\n\n[input-method]\npath=\n"
DBUS_CONF = """<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={path}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"""
# A private niri: no startup programs, no Xwayland, no hotkey overlay, no animations (steadier timings).
NIRI_KDL = """input {
    keyboard {
        xkb {
            layout "us"
        }
    }
}
hotkey-overlay {
    skip-at-startup
}
xwayland-satellite {
    off
}
animations {
    off
}
prefer-no-csd
"""


def clean_env() -> dict[str, str]:
    """This environment without the live agterm's variables."""
    return {name: value for name, value in os.environ.items() if not name.startswith(("AGTERM_", "ZMX_"))}


def wait_socket(path: Path, process: subprocess.Popen, label: str, timeout: float = 15.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"{label} exited with {process.returncode}")
        if path.exists():
            probe = socket.socket(socket.AF_UNIX)
            try:
                probe.connect(str(path))
                return
            except OSError:
                pass
            finally:
                probe.close()
        time.sleep(0.05)
    raise RuntimeError(f"{label} never served {path}")


def wait_for(predicate, timeout: float, step: float = 0.05):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return None


class Session:
    """The private helpers of one run, stopped (only these pids) when it ends."""

    def __init__(self, name: str) -> None:
        self.name = name
        self.root = Path(tempfile.mkdtemp(prefix=f"agterm-t3-{name}-"))
        self.processes: list[tuple[str, subprocess.Popen]] = []
        self.env = clean_env()
        self.env.pop("NIRI_SOCKET", None) if name != "real" else None
        self.results: list[dict] = []

    def spawn(self, label: str, argv: list[str], env: dict[str, str], log: str | None = None) -> subprocess.Popen:
        out = open(self.root / (log or f"{label}.log"), "wb")  # noqa: SIM115 - closed with the process
        process = subprocess.Popen(argv, env=env, stdout=out, stderr=subprocess.STDOUT, start_new_session=True)
        self.processes.append((label, process))
        return process

    def start_bus(self) -> None:
        path = self.root / "bus"
        (self.root / "dbus.conf").write_text(DBUS_CONF.format(path=path))
        process = self.spawn("dbus", ["dbus-daemon", "--nofork", "--nopidfile", "--nosyslog", f"--config-file={self.root / 'dbus.conf'}"], self.env)
        wait_socket(path, process, "dbus-daemon")
        self.env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={path}"

    def start_weston(self) -> Path:
        run = self.root / "weston-run"
        run.mkdir(mode=0o700)
        (self.root / "weston.ini").write_text(WESTON_INI)
        env = {name: value for name, value in self.env.items() if name not in ("WAYLAND_DISPLAY", "DISPLAY")}
        env["XDG_RUNTIME_DIR"] = str(run)
        env["__EGL_VENDOR_LIBRARY_FILENAMES"] = MESA_EGL
        argv = ["weston", "--backend=headless", "--renderer=gl", "--socket=wayland-0", f"--config={self.root / 'weston.ini'}",
                f"--log={self.root / 'weston.log'}", "--width=1920", "--height=1080", "--fake-seat"]
        process = self.spawn("weston", argv, env)
        wait_socket(run / "wayland-0", process, "weston")
        return run / "wayland-0"

    def start_nested_niri(self, parent: Path) -> None:
        run = self.root / "niri-run"
        run.mkdir(mode=0o700)
        config = self.root / "niri.kdl"
        config.write_text(NIRI_KDL)
        env = {name: value for name, value in self.env.items() if name not in ("DISPLAY", "NIRI_SOCKET")}
        env.update({"WAYLAND_DISPLAY": str(parent), "XDG_RUNTIME_DIR": str(run), "XDG_CONFIG_HOME": str(self.root / "config"),
                    "__EGL_VENDOR_LIBRARY_FILENAMES": MESA_EGL, "NIRI_DISABLE_SYSTEM_MANAGER_NOTIFY": "1"})
        process = self.spawn("niri", ["niri", "-c", str(config)], env)
        display = wait_for(lambda: next(iter(sorted(run.glob("wayland-*[0-9]"))), None), 15)
        ipc = wait_for(lambda: next(iter(run.glob("niri.*.sock")), None), 15)
        if not display or not ipc or process.poll() is not None:
            raise RuntimeError("nested niri did not come up; see niri.log")
        wait_socket(display, process, "niri")
        self.env.update({"WAYLAND_DISPLAY": str(display), "XDG_RUNTIME_DIR": str(run), "NIRI_SOCKET": str(ipc),
                         "VK_ICD_FILENAMES": INTEL_ICD, "__EGL_VENDOR_LIBRARY_FILENAMES": MESA_EGL})
        self.env.pop("DISPLAY", None)

    def use_weston(self, display: Path) -> None:
        self.env.update({"WAYLAND_DISPLAY": str(display), "XDG_RUNTIME_DIR": str(display.parent), "VK_ICD_FILENAMES": INTEL_ICD,
                         "__EGL_VENDOR_LIBRARY_FILENAMES": MESA_EGL})
        self.env.pop("DISPLAY", None)
        self.env.pop("NIRI_SOCKET", None)

    def start_xvfb(self) -> None:
        """A private Xvfb (no TCP, a free display number of its choosing) with no window manager."""
        read, write = os.pipe()
        argv = ["Xvfb", "-displayfd", str(write), "-nolisten", "tcp", "-screen", "0", "1920x1080x24"]
        out = open(self.root / "xvfb.log", "wb")  # noqa: SIM115 - closed with the process
        process = subprocess.Popen(argv, env=self.env, stdout=out, stderr=subprocess.STDOUT, pass_fds=(write,), start_new_session=True)
        self.processes.append(("xvfb", process))
        os.close(write)
        with os.fdopen(read) as pipe:
            number = pipe.readline().strip()
        if not number:
            raise RuntimeError("Xvfb did not report its display")
        self.env["DISPLAY"] = f":{number}"
        for name in ("WAYLAND_DISPLAY", "NIRI_SOCKET", "WAYLAND_SOCKET"):
            self.env.pop(name, None)
        self.x11 = True

    x11 = False

    def niri(self, *args: str) -> dict | list | None:
        output = subprocess.run(["niri", "msg", "--json", *args], env=self.env, capture_output=True, check=False, timeout=5)
        try:
            return json.loads(output.stdout)
        except json.JSONDecodeError:
            return None

    def focused_title(self) -> str | None:
        if self.x11:
            output = subprocess.run(["xdotool", "getwindowfocus", "getwindowname"], env=self.env, capture_output=True, check=False, timeout=5)
            return output.stdout.decode(errors="replace").strip() if output.returncode == 0 else None
        window = self.niri("focused-window")
        return window.get("title") if isinstance(window, dict) else None

    def lines(self, log: str) -> list[dict]:
        path = self.root / log
        if not path.exists():
            return []
        found = []
        for line in path.read_text(errors="replace").splitlines():
            for tag in ("RESULT ", "INFO "):
                if line.startswith(tag):
                    found.append(json.loads(line[len(tag):]))
        return found

    def record(self, entry: dict) -> None:
        self.results.append(entry)

    def stop(self) -> None:
        for _, process in reversed(self.processes):
            if process.poll() is None:
                process.terminate()
        for label, process in reversed(self.processes):
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            self.record({"info": "exit", "detail": {"process": label, "code": process.returncode}})

    def save(self, run: str) -> None:
        RESULTS.mkdir(exist_ok=True)
        with open(RESULTS / f"{run}.jsonl", "w", encoding="utf-8") as handle:
            for entry in self.results:
                handle.write(json.dumps(entry) + "\n")


def run_quick(session: Session) -> None:
    process = session.spawn("quick", [str(BIN), "quick"], session.env)
    try:
        process.wait(timeout=90)
    except subprocess.TimeoutExpired:
        process.kill()
        session.record({"check": "quick_finished", "pass": False, "detail": {"error": "timed out"}})
    for entry in session.lines("quick.log"):
        session.record(entry)


def second_launch(session: Session, label: str, gtk: bool, via_niri: bool) -> dict:
    out = session.root / f"{label}.json"
    argv = ["python3", "-I", str(HERE / "second_launch.py"), APP_ID, str(out), *(["--gtk"] if gtk else [])]
    if via_niri and session.x11:
        # X11 has no activation token: a launcher hands over a startup-notification id whose `_TIME` is the
        # launching event's timestamp (what GTK's platform data calls `desktop-startup-id`)
        env = dict(session.env, DESKTOP_STARTUP_ID=f"spike-launch_TIME{int(time.monotonic() * 1000) & 0xFFFFFFFF}")
        session.spawn(label, argv, env)
    elif via_niri:
        # niri spawns it with an activation token of its own making (XDG_ACTIVATION_TOKEN), as a launcher would
        keep = {name: session.env[name] for name in ("DBUS_SESSION_BUS_ADDRESS", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR") if name in session.env}
        subprocess.run(["niri", "msg", "action", "spawn", "--", "env", "-u", "AGTERM_CONTROL_SOCKET", *[f"{k}={v}" for k, v in keep.items()], *argv],
                       env=session.env, check=False, timeout=5)
    else:
        session.spawn(label, argv, session.env)
    seen = wait_for(lambda: out.exists() and out.stat().st_size > 0 and json.loads(out.read_text()), 10)
    return seen or {"error": "the second launch wrote nothing"}


def activation_results(session: Session, since: int, check: str, timeout: float = 5) -> dict | None:
    def found():
        for entry in session.lines("primary.log")[since:]:
            if entry.get("check") == check:
                return entry
        return None

    return wait_for(found, timeout)


def run_activation(session: Session) -> None:
    title = f"{APP_ID} primary"
    session.spawn("primary", [str(BIN), "primary", APP_ID], session.env)
    focused = wait_for(lambda: session.focused_title() == title, 10)
    session.record({"check": "primary_focused_on_open", "pass": bool(focused), "detail": {"focused": session.focused_title()}})

    def take_focus_away(label: str) -> bool:
        session.spawn(label, [str(BIN), "window", label], session.env)
        return bool(wait_for(lambda: session.focused_title() == label, 10))

    decoy = "spike-decoy"
    session.record({"check": "decoy_takes_focus", "pass": take_focus_away(decoy), "detail": {"focused": session.focused_title()}})

    def control(action: str) -> None:
        subprocess.run(["gdbus", "call", "--session", "--dest", APP_ID, "--object-path", "/" + APP_ID.replace(".", "/"),
                        "--method", "org.gtk.Actions.Activate", action, "[]", "{}"], env=session.env, capture_output=True, check=False, timeout=5)

    # negative control: GPUI's own activate_window (a fresh token from the last pointer serial)
    since = len(session.lines("primary.log"))
    control("raise-without-token")
    entry = activation_results(session, since, "gpui_activate_window")
    session.record({"check": "control_gpui_activate_window_refused", "pass": bool(entry) and not entry["pass"], "detail": entry})

    # (label, GtkApplication, launched by niri with a token, focus expected): a plain GApplication puts only `cwd` in
    # its platform data, never the token, so only the GtkApplication launch is expected to raise the primary
    cases = [
        ("second_gapplication_no_token", False, False, False),
        ("second_gapplication_token_not_forwarded", False, True, False),
        ("second_gtk_token", True, True, True),
    ]
    for index, (label, gtk, via_niri, expected) in enumerate(cases):
        if session.focused_title() == title:
            take_focus_away(f"spike-decoy-{index}")
        before = session.focused_title()
        since = len(session.lines("primary.log"))
        seen = second_launch(session, label, gtk, via_niri)
        entry = activation_results(session, since, "activate_with_token") or activation_results(session, since, "activate_without_token", 1)
        info = [e for e in session.lines("primary.log")[since:] if e.get("info") == "activate_received"]
        passed = bool(entry) and entry["pass"] == expected
        session.record({"check": label, "pass": passed, "detail": {"focused_before": before, "second_launch": seen, "primary": entry, "received": info}})
    control("quit")


def main() -> None:
    if not BIN.exists():
        sys.exit(f"build first: {BIN}")
    for run in sys.argv[1:]:
        session = Session("real" if run.endswith("-real") else run)
        try:
            session.start_bus()
            if run == "quick-weston":
                session.use_weston(session.start_weston())
            elif run.endswith("-x11"):
                session.start_xvfb()
            elif run.endswith("-nested"):
                session.start_nested_niri(session.start_weston())
            if run.startswith("quick"):
                run_quick(session)
            else:
                run_activation(session)
        except Exception as error:  # noqa: BLE001 - recorded as the run's failure
            session.record({"check": "run", "pass": False, "detail": {"error": repr(error)}})
        finally:
            session.stop()
            session.save(run)
            keep = session.root
            print(f"== {run} (logs in {keep})")
            for entry in session.results:
                if "check" in entry:
                    print(f"  {'PASS' if entry['pass'] else 'FAIL'} {entry['check']}")
        if os.environ.get("SPIKE_KEEP_LOGS") != "1":
            shutil.rmtree(session.root, ignore_errors=True)


main()
