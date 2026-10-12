# Spike A: launching the references

Spike A asks one question per reference.
Can the harness launch an isolated instance, talk to its socket, and get a deterministic `/bin/sh` in its pane?

Run it with `conformance/tools/spike_a.py`:

```sh
.venv/bin/python conformance/tools/spike_a.py --impl swift-linux --bin ~/.local/share/agterm-linux/bin/agterm-linux
bundle=$(python conformance/tools/fetch_reference.py --tag v0.36.0 | sed -n 's/^bundle=//p')
python conformance/tools/spike_a.py --impl swift-macos --bin "$bundle"
```

`--mechanism` chooses which channel tells the instance about `/bin/sh`:

- `both` is what the adapters ship;
- `config` keeps only `command = /bin/sh` in `<state>/config/ghostty.conf`, with `SHELL` set to a decoy;
- `env` keeps only `SHELL=/bin/sh`;
- `none` keeps neither.

The report holds the pane shell's `$0`, its `ps` comm, its parent's comm, and the shell version.
`.github/workflows/spike-macos.yml` runs the macOS matrix.

## reference-linux (confirmed 2026-10-11)

The installed fork was 0.36.0, commit `8a5f58d4`.
It ran through `SwiftLinuxAdapter` with a unique `AGTERM_APP_ID` and an isolated `/tmp/agt-*` root.

| mechanism | `$0` | comm | parent | result |
|---|---|---|---|---|
| both | `/bin/sh` | `sh` | `agterm-linux.bin` | sh |
| config (SHELL=/bin/bash) | `/bin/sh` | `sh` | `agterm-linux.bin` | sh |
| env | `/bin/sh` | `sh` | `agterm-linux.bin` | sh |
| none (SHELL=/bin/bash) | `/bin/bash` | `bash` | `agterm-linux.bin` | bash |

- Either channel alone is enough.
  Without either, the pane runs `$SHELL`, not the password-database shell (fish on this host).
  The adapters keep both, so a reference that ignores one channel still gets `/bin/sh`.
- The shell is the app's direct child: there is no login wrapper.
  `/bin/sh` here is bash 5.3 in POSIX mode.
- `session.type` to the active session was accepted as soon as `version` answered.
  The probe line came back through `session.text` with no further wait.
- In a fresh state dir the restore mode is `none`, so no zmx daemon starts and `stop` found no leftovers.
- Live sessions are gated on the account's login shell.
  `ZmxSupport.configuration` refuses any login shell but zsh (`unsupportedLoginShell`).
  This host's login shell is fish, so a Live-mode request on reference-linux falls back to fresh shells here.
  The adapter's static `live-sessions` capability does not hold on this host; Task 17 has to account for it.
  A zmx-wrapped pane also runs the login zsh rather than `/bin/sh`.
  Both points come from static reading and are unverified at runtime.

## reference-macos (confirmed 2026-10-12)

Upstream v0.36.0 (commit `6cef448a`) on the GitHub `macos-15` arm64 runner, not the x86_64 VM (see below).

| mode | mechanism | `$0` | comm | parent | result |
|---|---|---|---|---|---|
| uitest, plain | both | `-/bin/sh` | `-/bin/sh` | `/usr/bin/login` | sh |
| uitest, plain | config (SHELL=/bin/zsh) | `-/bin/sh` | `-/bin/sh` | `/usr/bin/login` | sh |
| uitest, plain | env | `-/bin/bash` | `-/bin/bash` | `/usr/bin/login` | the account's shell |

- Only `command = /bin/sh` in the scoped ghostty.conf holds on macOS.
  `SHELL` alone gives the password-database shell, because the pane runs
  `login -flp <user> /bin/bash --noprofile --norc -c "exec -l <command>"`.
  `/bin/sh` there is bash 3.2.57 in POSIX mode.
- The sentinel (`uitest`) stays the default; `plain` also renders a window and binds the socket for a lone instance.
  The full suite ran only under the sentinel, and only once each instance ran **its own copy of the bundle**.
  The sentinel's FB11763863 workaround reopens `Bundle.main.bundleURL` through Launch Services, and with one
  bundle path shared by several running instances that reopen reached another instance: every launch next to
  a long-running one then sat in its run loop with no window and no socket.
  `SwiftMacosAdapter` clones the bundle into `<root>/app/` (APFS `cp -c`, no data copied) and execs the clone.
- The macOS Local Network prompt for the test process ("Allow Python to find devices on local networks") is a
  system alert that, while it stays up, keeps every later launch from presenting a window.
  Python's `HTTPServer` raised it by resolving its host with `getfqdn`; pages are now served by
  `agterm_conformance.loopback`, which skips that lookup.
- `CFFIXED_USER_HOME` holds: the reference writes `Library/Caches` and `Library/Application Support` under the
  isolated home, and the runner's own `~/Library/Preferences` gains no agterm domain (checked by the workflow).
- SIGTERM ends the app at once in both modes, with no quit prompt and no final save:
  a selection saved on upstream's 0.3s debounce is lost if the quit comes first.
- A downloaded and `ditto`-copied bundle starts, and so does its `cp -c` clone.
- The account's login shell is bash on the runner; the workflow switches it to zsh with `dscl` before the run,
  so `live-sessions` holds and `--must-pass live-sessions` fails the job on any Live skip.
- Live sessions: each pane runs
  `login … agterm-session-host client <daemon> -- zmx attach <daemon>`, the first one starts a detached
  `agterm-session-host host <ZMX_DIR>`, and the zmx daemons run detached as well.
  `ps -E` shows the environment of the user's own processes, so the leftover check finds daemons by `ZMX_DIR`
  and the host by that argv.
  The host outlives the app and takes a stop request only from a peer inside its bundle, so
  `SwiftMacosAdapter.stop_helpers` SIGTERMs it, by pid, only after re-checking that the pid runs the instance's
  own bundle copy's `agterm-session-host` with `host <ZMX_DIR>` for that instance's state dir
  (`docs/backlog/harness-stop-for-macos-session-host.md`, resolved).
- Reaping waits for a claimed daemon zmx does not list yet (`observation: absent` refuses a kill) and for a closed
  session's daemon to leave its 3s undo window.

## The upstream release (v0.36.0)

- It has one asset, `agterm-0.36.0.dmg`, with sha256 `aa09d955…f36bf`, which GitHub records as the asset digest.
  `fetch_reference.py` verifies that digest.
- The bundle's binaries are thin arm64: `agterm`, `agtermctl`, `agterm-session-host` and `zmx`.
  This was checked on the extracted dmg, and upstream `project.yml` sets `ARCHS: arm64`.
- **The x86_64 macOS VM cannot run it.** Rosetta translates x86_64 to arm64, never the reverse.
  `fetch_reference.py` exits 2 on such a host.
  Task 19 needs one of these instead:
  - an Apple-silicon host, such as the GitHub `macos-15` runner the workflows use;
  - an x86_64 build from source on the VM (Xcode 26 and zig, a heavy setup).
- The dmg needs `hdiutil`, so extraction works only on macOS.
  On Linux the tool stops after the download and the digest check, and caches nothing.
