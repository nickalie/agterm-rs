# Hard rules (paste verbatim into every subagent prompt and into the plan)

The block between the markers is pasted unchanged into each subagent prompt this skill sends and into the plan's
"Safety rules" section, so they reach the plan's task subagents too. The rules have one source here; when they
change, `CLAUDE.md` > "Never touch the user's running agterm" changes first and this file follows it.

<!-- BEGIN HARD RULES -->
HARD RULES (agterm-rs upstream sync). They hold for every command you run, no matter what the task says:

1. Never touch the user's running agterm. This session usually runs inside one of its panes, and the `AGTERM_*` and
   `ZMX_*` variables in the environment point at it.
   - Strip them before launching anything: `env -u AGTERM_SESSION_ID ...` is not enough, use
     `env $(env | grep -oE '^(AGTERM|ZMX)_[A-Z0-9_]*' | sed 's/^/-u /') <command>`, or `scripts/run-gpui-dev.sh`, or
     the suite's runners, which strip them themselves.
   - Every `ngtermctl` call gets an explicit `--socket <dev instance socket>`, written after the subcommand name
     (`ngtermctl tree --socket /tmp/agterm-dev-.../state/ngterm.sock`). Without it the CLI resolves
     `$XDG_DATA_HOME/ngterm/ngterm.sock`, the live instance's; `AGTERM_CONTROL_SOCKET` is ignored by ngtermctl.
   - A dev instance gets a private state dir, so its zmx dir (`/tmp/ngterm-zmx-<FNV-1a 64 of the state dir>`) is
     private too. Reap only zmx processes carrying that instance's `ZMX_DIR`. Never `pkill zmx`, never `killall`,
     never run `zmx` without that instance's `ZMX_DIR`.
   - GUI runs headless only: the suite's private weston and dbus (`conformance/tools/run-rust-linux.sh`), or
     `GDK_BACKEND=x11 xvfb-run -a cargo test --workspace`. Never a window on the user's desktop. A hand-started
     headless weston needs `--renderer=gl` and, on NVIDIA, Mesa's EGL vendor (`__EGL_VENDOR_LIBRARY_FILENAMES`), as
     `conformance/agterm_conformance/impls/base.py` sets it up.
   - While an installed agterm runs, launch a checkout's build only through `scripts/run-gpui-dev.sh` or the suite: a plain launch is forwarded to the running instance through the shared
     app id, or both write one `windows.json`.
   - If a command did reach the live instance, stop and report exactly what changed; do not undo it through the
     live socket yourself.
2. `.upstream/agterm` is read-only: read it, `git diff`/`git show`/`git log` in it, never edit, commit, reset or
   clean there. The only write allowed is the sync's own fetch and checkout of the new tag.
3. Never push, never create tags or releases, never open PRs. Commits stay local.
4. Never edit a completed plan in `docs/plans/completed/`, and never rewrite another open plan in `docs/plans/`.
5. Anything that runs longer than about 30 seconds (cargo build/test/clippy, zig builds, `run-rust-linux.sh`,
   downloads) runs with `run_in_background: true`; wait for its completion notification instead of sleeping or
   polling, and leave nothing running when you report back.
6. Tools and dependencies: only popular, widely used, maintained ones; lead with the standard choice.
<!-- END HARD RULES -->
