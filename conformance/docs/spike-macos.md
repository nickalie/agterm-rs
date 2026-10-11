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

## reference-macos: the interim choice

- `SwiftMacosAdapter` defaults to `launch_mode="uitest"` with `enable_zmx=True`.
  That sets `AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE=1` and `AGTERM_UITEST_ENABLE_ZMX=1`.
  The sentinel gives a present window (the FB11763863 workaround), no welcome, and no quit prompt.
  The zmx flag keeps Live sessions possible.
- A bare exec is selectable: `--macos-launch-mode plain` (pytest) or `--launch-mode plain` (spike).
- Task 19 confirms the choice or flips it.

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

## Open questions for Task 19

1. Does a direct exec under the sentinel render a window and bind the socket without XCUITest?
   The socket binds from the window scene's task.
   Plain mode may need the app activated before a window renders.
2. Which `/bin/sh` channel holds on macOS?
   Ghostty on macOS wraps the shell in `/usr/bin/login`, so check `$0`, comm and parent per mechanism.
3. Does `CFFIXED_USER_HOME` move the `com.umputun.agterm` defaults and `NSHomeDirectory()` into the isolated home?
4. Does SIGTERM end the app promptly in both modes, with no quit confirmation?
5. Does the leftover check (`ps -E`) find zmx daemons by `ZMX_DIR`?
   Is `agterm-session-host` left behind?
   See `docs/backlog/harness-stop-for-macos-session-host.md`.
6. Can a binary copied out of a downloaded dmg by `ditto` start?
   It carries `com.apple.provenance` xattrs, and Gatekeeper may stop it.
