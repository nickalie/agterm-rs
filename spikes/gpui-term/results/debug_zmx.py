"""Debug: launch the spike N times; when the prompt does not show, print the zmx session state and its processes."""
import os, subprocess, sys, tempfile, time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check  # noqa: E402

for i in range(int(sys.argv[1]) if len(sys.argv) > 1 else 6):
    root = Path(tempfile.mkdtemp(prefix="gpui-term-dbg-", dir="/tmp"))
    session = check.Headless(root)
    spike = check.Spike(check.DEFAULT_BIN, session, f"d-{i}")
    try:
        time.sleep(4)
        state = spike.dump()
        ok = "spike$" in state["text"]
        print(f"run {i}: prompt={'yes' if ok else 'NO'} bytes={state['bytes']}", flush=True)
        if not ok:
            env = {"PATH": os.environ["PATH"], "ZMX_DIR": os.fspath(spike.zmx_dir), "HOME": session.env["HOME"]}
            for args in (["list"], ["screen", f"d-{i}"]):
                done = subprocess.run([os.fspath(check.ZMX), *args], env=env, capture_output=True, text=True, timeout=10)
                print(" zmx", args, "->", done.returncode, done.stdout[:800], done.stderr[:300])
            marker = f"ZMX_DIR={spike.zmx_dir}".encode()
            for proc in Path("/proc").iterdir():
                if proc.name.isdigit():
                    try:
                        if marker in (proc / "environ").read_bytes().split(b"\0"):
                            stat = (proc / "stat").read_text().split()
                            print(" proc", proc.name, (proc / "cmdline").read_bytes().replace(b"\0", b" ")[:120],
                                  "state", stat[2], "wchan", (proc / "wchan").read_text())
                    except OSError:
                        pass
    finally:
        spike.stop()
        session.stop()
