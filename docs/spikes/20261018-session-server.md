# Spike: herdr's session server for agterm-session

Plan: `docs/plans/20261018-zig-free-core.md`, Task 2. Run on 2026-10-18. The decisions this spike settles are written
into the plan (Technical Details > `agterm-session` > "What Task 2 settles"), and Tasks 20–32 follow them. This note
holds the evidence behind them. The probes' sources are in `docs/spikes/20261018-session-server-probes.md`; they ran
from the git-ignored `.scratch/session-spike/`.

## Verdict

**Go for Part B.** Nothing found blocks it.

- **herdr's server can be taken as planned**, with one correction. Its `AttachTerminal` renders the pane on the
  server and sends ANSI diffs; it is not the raw stream the plan attributed to it. agterm builds its raw attach mode
  on herdr's connection framework.
- **Windows handoff: conditional go.** A ConPTY session passes between two detached server processes with its
  shell alive and its state, resize and kill intact, in the VM and on two Windows runner images. It relies on the
  undocumented layout of the pseudoconsole handle, so Task 25 builds it behind a layout gate. A machine the gate
  does not know keeps today's fallback: the old server keeps serving, and the update path offers the reset.
- **macOS: go.** `posix_spawn` with `POSIX_SPAWN_SETSID` and the responsibility disclaim works as upstream's
  session host does. The server outlives its launcher, is responsible for itself, and passes a shell to a new server
  over `SCM_RIGHTS`.
- **Windows spawn from inside a kill-on-close job needs herdr's WMI path.** A plain retry after a refused breakaway
  dies with the job.

## herdr at the pin

`HERDR_REV` `143bf5fd` (master, 2026-10-16, v0.9.5 + 24 commits), cloned at `.upstream/herdr`. The server-side
modules come to about 80k lines with the TUI parts. The module-by-module verdicts are in the plan. What changed the
plan:

- **Attach.** `server/headless.rs` `attach_terminal_client` switches a connection to `TerminalAttach` and resizes the
  pane to it. `server/headless/render.rs` then renders the terminal into a virtual buffer (`render_terminal_virtual`),
  builds `FrameData` and sends `ServerMessage::Terminal(TerminalFrame)` diffs. The frames go through a render lane of
  capacity one that drops stale frames (`server/client_transport.rs` `ClientWriterQueue`), and a terminal has one
  attached owner unless a client takes over. Raw output never reaches a client. herdr's direct attach is also
  Unix-only (`platform/mod.rs` `capabilities`).
- **Encoding.** herdr frames `bincode` 2. On crates.io, `bincode` 3.0.0 (2025-12-16) is a tombstone: its `lib.rs` is
  `compile_error!` and its README says development ceased after a harassment incident. agterm uses `postcard`.
- **Runtime.** herdr's server is a tokio `select!` loop (`server/headless.rs` `run`) over channels, a deadline and a
  render notifier. Its transport, API connections, PTY actors and handoff are blocking threads. Without the TUI the
  loop is a single channel with a deadline, so agterm uses threads.
- **Windows spawn.** `platform/windows.rs` `launch_server_daemon_command` starts the server with `DETACHED_PROCESS`
  and, when the launcher sits in a kill-on-close job, through WMI `Win32_Process.Create`, so the server sits outside
  the job. It does not use `CREATE_BREAKAWAY_FROM_JOB`.
- **Windows kill.** herdr kills a pane's processes from a process snapshot (`session_processes`, then
  `signal_processes`). Its kill-on-close jobs serve only status commands. agterm's per-pane `KILL_ON_JOB_CLOSE` job
  is its own addition. The job handle has to pass in a handoff: when the old server exits, the job closes and kills
  every shell.
- **Handoff.** Unix only: a token-authenticated private socket, a JSON manifest, `SCM_RIGHTS` in batches of 64, and
  validated, restored, ready, committed and owned steps with timeouts (`server/handoff.rs`). The per-pane state
  (`handoff_runtime.rs`) carries the history as ANSI, which the new server seeds into its terminal
  (`pane.rs` `from_handoff_fd`). The new server is not the shell's parent, so the exit status after a handoff is
  unknown (`ChildExitReason::Handoff`).
- **macOS.** herdr starts its server through a one-shot launchd job (`platform/macos/launchd_spawn.rs`) because a
  setsid server started from Ghostty died on Cmd-Q (herdr#4920). It also moves the server into the user's Mach
  bootstrap context (`platform/macos/bootstrap.rs`). Upstream agterm disclaims responsibility instead
  (`Responsibility.swift`).
- **`vendor/portable-pty`.** Four Windows patches on 0.9.0, documented in `vendor/portable-pty.patches.md`:
  - 0001: a hash-checked app-local `conpty.dll` in place of the bare DLL search;
  - 0002: raw `cmd.exe` command tails;
  - 0003: malformed environment blocks rejected;
  - 0004: directories skipped in the `PATH` search.
- **Gaps for agterm: none.** `persist` saves layouts and, optionally, screen history to disk for fresh shells after
  a restart, which upstream's restore and replay cover without screen history. `remote` runs herdr on an ssh host
  behind a bridge, where `agterm-core::remote::ssh` (upstream's `attachCommand`) already attaches.

## Windows: ConPTY handoff

Probe `win-handoff`. `probe run <dir>` drives three processes:

1. A launcher puts itself in a kill-on-close job that allows breakaway, as a terminal or an IDE may. It starts server
   A with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`, then exits and closes the job.
2. A creates a pseudoconsole and runs `cmd.exe` in it, inside a `KILL_ON_JOB_CLOSE` job, and sets a variable in that
   shell. It starts server B suspended and detached, and `DuplicateHandle`s into B:
   - the ConPTY input write end and output read end;
   - the three handles inside the HPCON (`*(hpc as *const [HANDLE; 3])`: signal, reference, conhost process);
   - the job and the shell process.

   Then A exits without `ClosePseudoConsole`, as a replaced server would.
3. B waits for A to be gone and runs the checks.

| check | VM 10.0.26100.9711 | `windows-latest` 10.0.26100.33438 | `windows-2022` 10.0.20348.5622 |
|---|---|---|---|
| the launcher's job closed, and A lived (breakaway allowed) | pass | pass | pass |
| the shell survives the old server | pass | pass | pass |
| conhost survives the old server | pass | pass | pass |
| B's typing reaches the shell, which kept A's variable | pass | pass | pass |
| `ResizePseudoConsole` on an HPCON rebuilt from the three handles reaches the child (`[Console]::WindowWidth` 100) | pass | pass | pass |
| closing the duplicated job ends the shell | pass | pass | pass |
| closing the duplicated console handles ends conhost | pass | pass | pass |

With `PROBE_NO_BREAKAWAY=1` (VM), the launcher's job has no `JOB_OBJECT_LIMIT_BREAKAWAY_OK`. `CreateProcess` with
breakaway fails with `ERROR_ACCESS_DENIED` (5), the probe retries without it, and A dies when the launcher closes the
job, so no result is ever written. This is why the plan goes to WMI next, never to a plain retry.

What the probe does not show, and Task 25 must (Codex's review, below):

- the layout is undocumented: a gate is needed before any dereference, and the rebuilt struct must never reach
  `ClosePseudoConsole`, whose destruction is heap-specific;
- the output is not handed over losslessly: A exits with its drain thread running;
- there is no prepare/ready/commit protocol and no rollback;
- repeated handoffs, descendants and continuous output are untested.

The escape hatch, if no reliable gate can be made: a stable per-namespace ConPTY owner process.

## macOS: spawn, disclaim and handoff

Probe `mac-handoff`. `probe run <dir>` drives three processes:

1. A launcher `posix_spawn`s the server with `POSIX_SPAWN_SETSID` (0x0400 in `<spawn.h>`; the `libc` crate exports
   the constant for Linux only) and `responsibility_spawnattrs_setdisclaim` (through `dlsym`, as upstream does), then
   exits.
2. The server runs `/bin/sh` on a PTY, sets a variable in it, starts the import server the same way, passes the
   master and the shell's pid over `SCM_RIGHTS` and exits.
3. The import server runs the checks.

| check | VM macOS 26.7 (x86_64) | `macos-26` | `macos-15` |
|---|---|---|---|
| the server leads its own session after the launcher exits | pass | pass | pass |
| the server is responsible for itself (`responsibility_get_pid_responsible_for_pid`) | pass | pass | pass |
| the import server leads its session and is responsible for itself | pass | pass | pass |
| the shell survives the old server | pass | pass | pass |
| the PTY passes to the new server, and the shell keeps its state | pass | pass | pass |
| a resize from the new server reaches the child (`stty size` 30 100) | pass | pass | pass |
| a hangup from the new server ends the shell, reaped by launchd | pass (5/5 after the fix below) | pass | pass |

**A finding on the way.** The first runs failed the last check in two of four: the shell stayed in state `E` (`ps`:
`?Es (bash)`), and even `SIGKILL` did not end it. Two things combined:

- the server's PTY master was not close-on-exec, so the import server inherited a second copy that nobody read;
- the import server had stopped reading, so the shell's last output (its prompt) sat undrained.

The exiting shell blocked in the tty drain until both were fixed. Made close-on-exec, with the reader draining
until the shell ends, as a real server's PTY reader does, the check passed in five runs of five. The plan now asks
for close-on-exec masters and `POSIX_SPAWN_CLOEXEC_DEFAULT` on the server spawn.

**herdr#4920** (did a terminal app's quit take the setsid server with it?). Not reproduced:

- `probe app` makes a minimal LaunchServices app (`open -W -n`) whose executable starts a plain child and a
  setsid + disclaim child, then exits after 2 s. Both children lived on, in the VM and on both runners.
- **Normal quit** (`term-quit-normal.sh`, over ssh in the VM): Terminal.app runs a script that starts a setsid child
  and a setsid + disclaim child, then the tab's shell exits. `osascript -e 'quit app "Terminal"'` sends Apple Events
  `quit`, as Cmd-Q does, and Terminal quit normally. Both children lived on.
- **SIGTERM** (`term-quit.sh`, through vmrun, where `osascript` hung without an Automation grant): Terminal.app
  keeps a plain child and a setsid + disclaim child, and Terminal.app is ended by `SIGTERM`. The plain child died, by
  the tab's hangup; the setsid + disclaim child lived on.

herdr's report concerns Ghostty, which this VM does not have. The launchd job stays the recorded fallback, and the
plan's Post-Completion macOS check covers agterm's own quit.

## Linux

The `mac-handoff` probe also runs on Linux, without the disclaim: all checks pass. A subreaper (`systemd --user`)
reaps the orphaned shell more slowly than launchd, so the hangup check waits up to 20 s.

## Baselines

Dev box: CachyOS, Linux 7.2.9, every run inside `agterm-agents.slice` (10 CPUs, 10 GB). zmx at its pin `8bab1f0`
with agterm's patches (`scripts/build-zmx.sh` prefix `zmx-ebaf291b501c4455`).

- **`agterm-term`'s `bench.rs`** (`read_path_plain_and_zmx`: a 50 MiB `cat` under the paint and key stand-ins, 141×41).
  The zmx sockets live under `TERM_BENCH_DIR/zmx`, so that directory must be short: under a scratch path in the
  worktree the socket path exceeds `SUN_LEN`, and the attach never prints its mark (the runs used `/tmp`).

  | run | plain `cat_s` | plain wall | zmx `cat_s` | zmx wall | zmx `vt_s` |
  |---|---|---|---|---|---|
  | 1 | 0.462 | 0.501 | 0.801 | 0.836 | 0.165 |
  | 2 | 0.486 | 0.521 | 0.899 | 0.929 | 0.191 |
  | 3 | 0.430 | 0.466 | 0.906 | 0.930 | 0.191 |

- **The GPUI build** (release, staged, headless weston with lavapipe through `conformance/tools/run-rust-linux.sh`,
  a 50 MiB `cat` typed into the pane, timed by the shell). `wall` runs from `session.type` until the harness reads the
  mark:

  | run | plain `cat_s` | plain wall | live zmx `cat_s` | live zmx wall |
  |---|---|---|---|---|
  | 1 | 1.503 | 1.554 | 2.466 | 3.152 |
  | 2 | 1.670 | 1.741 | 2.630 | 2.704 |
  | 3 | 1.627 | 1.677 | 2.253 | 2.287 |

  The line runs under `sh -c`, because the live pane starts the account's login shell (fish on this box). The whole
  app runs here, rendering on lavapipe on the same CPUs, so the numbers sit well above `bench.rs`'s (the cause is not
  profiled); they are the end-to-end baseline Task 30 compares against.

- **Idle zmx daemons**, each holding an idle `/bin/sh` after `seq 1 20000` (`rss.sh`, `VmRSS` and `smaps_rollup`
  `Pss`):

  | daemons | RSS total | PSS total | RSS each | PSS each |
  |---|---|---|---|---|
  | 1 | 14.6 MiB | 13.9 MiB | 14.6 MiB | 13.9 MiB |
  | 4 | 54.3 MiB | 45.6 MiB | 13.6 MiB | 11.4 MiB |
  | 16 | 222.9 MiB | 182.4 MiB | 13.9 MiB | 11.4 MiB |

The targets: `agterm-session`'s 50 MiB `cat` through a live pane is no slower than the zmx rows, and the server's
idle RSS for N such panes is no larger than the N-daemon totals.

## Design discussion with Codex

Transcript: `~/.local/state/plan-exec-revmux/zig-free-core/discussions/task-2-session-server-design/transcript.md`
(three turns). Starting positions and what moved:

- **Attach.** Both sides agreed it is not herdr's mode. Codex brought the offset- and epoch-tagged stream, and the
  correction that a snapshot carries `agterm-vt`'s validated continuation with an explicit "unavailable" outcome,
  never a truncated raw tail.
- **Slow clients.** Codex proposed disconnecting them. I argued for an in-band resync, because a disconnect costs
  the leader its lead; Codex accepted, adding the rules and a recovery budget.
- **Terminal replies.** I proposed zmx's rule (attached clients answer; the server only while none is). Codex showed
  that it leaves several responders and lets a stalled client block a reply. I accepted server-owned state and
  capability replies, with frontend facts as client metadata, replies suppressed in live panes, and a query filter
  in the attach CLI.
- **Runtime.** Codex started with tokio and moved to threads once the reduced loop was shown to be one channel with
  a deadline. It pointed out herdr's Windows input forwarder as the one async piece to replace.
- **Encoding, endpoint, Windows spawn and handoff guards.** Agreed. Codex added the stable version envelope and
  the guard list.

Codex signed off on the synthesis that the plan records.
