---
worth: later
where: conformance/agterm_conformance/impls/base.py:330
added: 2026-10-11
---
# may the harness SIGTERM the macOS agterm-session-host its own instance started

Came up in plan `docs/plans/20261011-rust-conformance-suite.md`, Task 3 review round 2. A macOS Live-sessions
scenario makes the reference spawn a detached `agterm-session-host` that outlives the app by design
(`agtermCore/Sources/SessionHostRuntime/Host.swift:61` setsid; windows.md "Live session host"). No harness
stop exists against an unmodified reference: the binary's CLI has only `host`/`client` modes
(`agterm-session-host/main.swift:7`), the `.stop` request needs a peer inside the same `.app`
(`SessionHost.swift:225`, `HostIdentity.swift:6`), and the app never sends it. So `stop()` reaps the zmx
daemons, SIGTERMs the app pid, and the leftover check still fails every macOS Live scenario on the host.

Options: (a) authorize the harness to SIGTERM that host; (b) leave it running and let the leftover check fail;
(c) drop `live-sessions` from the macOS adapter.
Claude's view: (a) is safe, because `ZMX_DIR` hashes a state dir inside a fresh mkdtemp root. Codex's view:
the run rules ("SIGTERM that pid only", "never signal any other agterm/zmx process") forbid it until the user
explicitly authorizes it. Any authorization should require an exact `ZMX_DIR` match, the executable inside
the adapter's own `--bin` bundle, and re-checking the process identity just before the signal (PID reuse).
Both agree on the interim.

Unknown that settles it: whether the user extends the signal rule to this one process. Decide before
Task 19's macOS Live runs.

Meanwhile: no host signal. The `LeftoverProcesses` message names the host and points here
(`SESSION_HOST_NOTE` in `impls/base.py`). `live-sessions` stays on the macOS adapter. To undo, delete the note.
Transcript: /tmp/plan-exec-revmux-rust-conformance-suite/fixer-session-host-stop/transcript.md
