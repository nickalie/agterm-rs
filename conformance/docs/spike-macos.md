# Spikes on the reference

## Spike A: launching the reference

Spike A asks one question of the reference.
Can the harness launch an isolated instance, talk to its socket, and get a deterministic `/bin/sh` in its pane?

Run it with `conformance/tools/spike_a.py`:

```sh
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

### reference-macos (confirmed 2026-10-12)

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
  With one bundle path shared, launches next to a long-running instance sat in their run loop with no window and
  no socket. The likely cause, not verified: the sentinel's FB11763863 workaround reopens `Bundle.main.bundleURL`
  through Launch Services, which delivered the reopen to the other instance.
  `SwiftMacosAdapter` clones the bundle into `<root>/app/` (APFS `cp -c`, no data copied) and execs the clone;
  no launch has failed since.
- The macOS Local Network prompt for the test process ("Allow Python to find devices on local networks") is a
  system alert that, while it stays up, keeps every later launch from presenting a window.
  It came up while tests served pages with Python's `HTTPServer`, whose bind resolves the host with `getfqdn`.
  Pages are now served by `agterm_conformance.loopback`, which skips that lookup, and the prompt has not
  come back.
- `CFFIXED_USER_HOME` moves `NSHomeDirectory()`: the reference writes `Library/Caches` and
  `Library/Application Support` under the isolated home.
  It does not move the defaults, which cfprefsd keeps per account: every instance reads and writes the account's
  own `com.umputun.agterm` domain, so a run on a Mac with a live agterm would share its settings.
  The workflow prints what the run left there.
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
  own bundle copy's `agterm-session-host` with `host <ZMX_DIR>` for that instance's state dir.
- Reaping waits for a claimed daemon zmx does not list yet (`observation: absent` refuses a kill) and for a closed
  session's daemon to leave its 3s undo window, and the leftover check gives a killed daemon up to 5s to exit:
  zmx confirms a kill before the process is gone.

## Spike B: the ax driver on reference-macos (confirmed 2026-10-12)

Spike B asks whether pyobjc's AX API can drive the reference the way the upstream XCUITests do.
`conformance/tools/spike_b.py` runs it, the `spike-b` job of `spike-macos.yml` runs it on `macos-26`.
It launches the reference through `SwiftMacosAdapter` (sentinel mode) and drives it through `AxDriver`.

- `add-session` is found by `AXIdentifier`.
  It is a SwiftUI `Menu` with `AXDescription` "Add session" and one action, `AXPress`.
  Its role follows the OS, not the reference: `AXPopUpButton` on macOS 15, `AXMenuButton` on macOS 26
  (spike-macos run 38248492987), so `test_add_session_menu_opens_a_session` accepts either.
- `AXPress` returns success at once and opens the menu.
  Its "New Session" `AXMenuItem` is found by title outside the menu bar, since the menu bar keeps every closed
  menu's items in the tree; `AXPress` on it adds a session, and the socket confirms it.
- Synthesized keystrokes (`CGEventCreateKeyboardEvent`) reach the new session's pane on every route tried:
  posted to the pid with the app in the background, the same after `AXFrontmost`, and through the HID tap.
  The driver posts to the pid.
- No grant was needed. The image's TCC databases already allow Accessibility for `/bin/bash` and
  `/opt/hca/hosted-compute-agent`, and PostEvent for `/bin/bash` in the user database.
  The run's process chain is `Python` ← `/bin/bash` ← `Runner.Worker` ← `Runner.Listener` ←
  `hosted-compute-agent`, and TCC answers for the responsible process rather than Python:
  `AXIsProcessTrusted` and `CGPreflightPostEventAccess` were both true with no row for Python.
  The workflow prints both checks and the relevant TCC rows.
  If an image drops the grant, `AxDriver` refuses to start, so a ui run fails rather than skipping.
- pyobjc AX is viable, so the thin XCUITest bridge the plan held in reserve is not needed.
  The bindings are the `ax` extra: `pip install -e "conformance[ax]"`.

## The ui layer on reference-macos (confirmed 2026-10-12)

`conformance/tests/ui` runs as its own step of `reference-macos.yml`, with `--reruns 1`.
What the reference showed while the tests settled:

- `palette-subtitle` and `palette-badge` never reach the accessibility tree.
  The palette row's own `palette-item-{item}` identifier overrides those of its texts,
  so the subtitle reads as a second `palette-item-{item}` element. Both ids are exempted in `catalog.yaml`.
- The first-run welcome is an `NSAlert` run modally before the window: the socket file exists but no request is
  answered until it is dismissed. `launch(first_run=True)` therefore returns unserved, and the test calls
  `adapter.serving(instance)` after pressing Later. Install is never pressed.
- `workspace-add-session` is in the tree only while the pointer is over its workspace row.
  `hover` moves the real pointer through the HID tap with the app frontmost: tracking areas ignore events posted
  to a pid.
- Keys posted to the pid did not reach the search field, and setting its `AXValue` shows the text without
  searching. Focusing it through `AXFocused` (app frontmost) and typing does.
- Some Settings elements fail `AXDescription` reads with `kAXErrorFailure` every time. The driver reads a failing
  title, description, value or enabled flag as no value, and still treats a failing `AXChildren` or
  `AXIdentifier` read as a read that did not happen.
- The Settings window opens with `cmd+,`; its tabs are toolbar buttons found by title (`Driver.tab`).
- `add-session`'s name read "Add session" in spike B and "Add Slide" in a ui run (reference-macos 38248492989).
  Upstream sets only `accessibilityLabel("Add session")`, which is `AXDescription`; the driver reads `AXTitle`
  first, and the SwiftUI `Menu` whose label is just the `plus.rectangle` glyph presumably fills `AXTitle` with
  the symbol's own description some time after the window appears.
  Unverified beyond those two reads, so the test leaves the name unchecked; the menu's effect is its check.

## The upstream release (v0.36.0)

- It has one asset, `agterm-0.36.0.dmg`, with sha256 `aa09d955…f36bf`, which GitHub records as the asset digest.
  `fetch_reference.py` verifies that digest.
- The bundle's binaries are thin arm64: `agterm`, `agtermctl`, `agterm-session-host` and `zmx`.
  This was checked on the extracted dmg, and upstream `project.yml` sets `ARCHS: arm64`.
- **The x86_64 macOS VM cannot run it.** Rosetta translates x86_64 to arm64, never the reverse.
  `fetch_reference.py` exits 2 on such a host.
  Task 19 needs one of these instead:
  - an Apple-silicon host, such as the GitHub `macos-26` runner the workflows use;
  - an x86_64 build from source on the VM (Xcode 26 and zig, a heavy setup).
- The dmg needs `hdiutil`, so extraction works only on macOS.
  On Linux the tool stops after the download and the digest check, and caches nothing.
