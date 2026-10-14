"""Probe: does `zmx attach` print the shell's prompt in a plain PTY, with and without a resize after start?"""
import fcntl, os, pty, select, struct, sys, termios, tempfile, time, signal

zmx = sys.argv[1]
resize_after = float(sys.argv[2]) if len(sys.argv) > 2 else -1
root = tempfile.mkdtemp(prefix="zmx-probe-")
rc = os.path.join(root, "rc")
open(rc, "w").write("PS1='spike$ '\n")
env = {"PATH": os.environ["PATH"], "HOME": root, "ZMX_DIR": os.path.join(root, "zmx"), "SHELL": "/bin/bash",
       "TERM": "xterm-256color", "LANG": "C.UTF-8"}
os.mkdir(env["ZMX_DIR"], 0o700)
pid, fd = pty.fork()
if pid == 0:
    os.execve(zmx, [zmx, "attach", "p", "/bin/bash", "--noprofile", "--rcfile", rc, "-i"], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
out, start, resized = b"", time.time(), False
while time.time() - start < 5:
    if resize_after >= 0 and not resized and time.time() - start > resize_after:
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 41, 141, 0, 0))
        resized = True
    r, _, _ = select.select([fd], [], [], 0.1)
    if r:
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        out += data
        print(f"{time.time() - start:.2f} {data!r}")
os.kill(pid, signal.SIGKILL)
os.system(f"ZMX_DIR={env['ZMX_DIR']} {zmx} kill p --force >/dev/null 2>&1")
