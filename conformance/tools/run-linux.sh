#!/usr/bin/env bash
# Build the Rust workspace, stage it and run the protocol, cli and behaviors layers against it, then hold the
# ratchet: every id in conformance/rust-passing.txt must still pass. Every instance comes from the harness (unique NGTERM_APP_ID, fresh root under this run's own /tmp/agt-run-*
# parent, its own headless weston and session bus); no window, notification or a11y object reaches the desktop.
#
#   conformance/tools/run-linux.sh [--stage DIR] [--out DIR] [--tests DIR]
#                                       [--ratchet FILE] [--ratchet-only [--also SELECTOR]... [--also-k EXPR]...]
#                                       [-- PYTEST_ARGS...]
#
# --ratchet-only runs only the ids the ratchet list holds, plus each --also directory, file or node-id prefix (a
# path outside the layers is collected too) and each --also-k -k expression, and judges the ratchet as a full run
# does: every listed id must pass, and newly passing ids come only from what ran. The red tests a build short of
# parity has wait out their read-backs, so this is a task's routine run; a full run (the default) stays required at
# milestones (conformance/README.md > Rust runner and ratchet).
# The build (bin/ngterm, staged into target/conformance-stage, reports in conformance/reports/rust-linux) renders
# on Mesa's lavapipe only, from scripts/fetch-lavapipe.sh (exit 2 without one), its weston with --fake-seat.
# --stage runs an already staged tree (scripts/stage.sh layout) instead of building and staging into target/.
# Without the app's binary in the stage every test skips and parity reads 0%.
# Writes report.md, junit.xml, pytest.log and nodeids.jsonl into DIR.
# Exits with check_ratchet.py's status (0 holds, 1 broken, 2 no usable report), or 1 when a run left an instance
# root behind; pytest's own status does not count, since most tests are red by design until parity.
# CONFORMANCE_LINUX_DISPLAY=session uses this desktop's display and bus instead, for debugging.
# One run at a time per user: a second run waits on the lock $CONFORMANCE_LOCK (default
# $XDG_RUNTIME_DIR/agterm-conformance.lock, else /tmp/agterm-conformance-<uid>/agterm-conformance.lock), shared by
# every checkout. It does not hold back a stand-alone cargo run.
set -euo pipefail

# Set for the child the lock block below runs; cleared before anything spawns, so a nested runner queues again.
locked="${CONFORMANCE_RUN_LOCKED:-}"
unset CONFORMANCE_RUN_LOCKED
args=("$@")

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
stage=""
out=""
tests="$repo/conformance/tests"
ratchet=""
ratchet_only=""
also=()
also_k=()
extra=()

while (($#)); do
    case "$1" in
        --stage) stage="$2"; shift 2 ;;
        --out) out="$2"; shift 2 ;;
        --tests) tests="$2"; shift 2 ;;
        --ratchet) ratchet="$2"; shift 2 ;;
        --ratchet-only) ratchet_only=1; shift ;;
        --also) also+=("$2"); shift 2 ;;
        --also-k) also_k+=("$2"); shift 2 ;;
        --) shift; extra=("$@"); break ;;
        -h|--help) sed -n '2,26s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
if [[ -z "$ratchet_only" ]] && ((${#also[@]} + ${#also_k[@]})); then
    echo "--also and --also-k need --ratchet-only" >&2
    exit 2
fi

# The app binary in the stage, and the defaults.
app=ngterm
out="${out:-$repo/conformance/reports/rust-linux}"
ratchet="${ratchet:-$repo/conformance/rust-passing.txt}"

[[ -f "$ratchet" ]] || { echo "no ratchet list at $ratchet" >&2; exit 2; }
python="${PYTHON:-$repo/.venv/bin/python}"
[[ -x "$python" ]] || python="$(command -v python3)"
# This checkout's harness, ahead of whichever checkout the venv's editable install points at.
export PYTHONPATH="$repo/conformance${PYTHONPATH:+:$PYTHONPATH}"
display="${CONFORMANCE_LINUX_DISPLAY:-headless}"
if [[ "$display" == headless ]]; then
    for tool in weston dbus-daemon; do
        if ! command -v "$tool" >/dev/null; then
            echo "no $tool on PATH: install it, or set CONFORMANCE_LINUX_DISPLAY=session" >&2
            exit 2
        fi
    done
fi

layers=()
for layer in protocol cli behaviors; do
    if [[ -d "$tests/$layer" ]]; then layers+=("$tests/$layer"); fi
done
((${#layers[@]})) || { echo "no protocol, cli or behaviors layer under $tests" >&2; exit 2; }

# --ratchet-only: the plugin keeps the listed ids and the extras. An --also path outside the layers (tests/ui, say) is
# collected as well; selectors are spelled like the list's ids, relative to the suite's root (the tests dir's parent).
selection=()
if [[ -n "$ratchet_only" ]]; then
    selection=(--ratchet-only "$ratchet")
    for selector in "${also[@]}"; do
        selection+=(--also "$selector")
        path="${selector#./}"
        path="${path#conformance/}"
        path="$(dirname "$tests")/${path%%::*}"
        path="${path%/}"
        inside=""
        for layer in "${layers[@]}"; do
            if [[ "$path" == "$layer" || "$path" == "$layer"/* ]]; then inside=1; fi
        done
        [[ -e "$path" ]] || { echo "--also $selector: no $path" >&2; exit 2; }
        if [[ -z "$inside" ]]; then layers+=("$path"); fi
    done
    for expression in "${also_k[@]}"; do selection+=(--also-k "$expression"); done
fi

[[ -z "$stage" || -d "$stage" ]] || { echo "no staged tree at $stage" >&2; exit 2; }

# The build renders through Vulkan: on lavapipe and nothing else (the harness points VK_DRIVER_FILES at it), never on
# this machine's GPU driver, so a missing lavapipe stops the run here. Not NGTERM_- or AGTERM_-prefixed: the plugin
# scrubs those.
if ! lavapipe="$("$repo/scripts/fetch-lavapipe.sh")"; then
    echo "no lavapipe for the GPUI build: see scripts/fetch-lavapipe.sh" >&2
    exit 2
fi
export CONFORMANCE_LAVAPIPE_ICD="$lavapipe"
# GPUI panics without a wl_seat, which headless weston offers only with --fake-seat (weston 15.0 and later): a system
# weston without it gives way to the one scripts/build-weston.sh vendors.
if [[ "$display" == headless ]] && ! env -u WAYLAND_DISPLAY -u DISPLAY weston --help 2>/dev/null \
    | grep -q -- --fake-seat; then
    if ! "$repo/scripts/build-weston.sh" --verify; then
        echo "$(command -v weston) has no --fake-seat, which the GPUI build needs (weston 15.0 or later):" \
            "run scripts/build-weston.sh" >&2
        exit 2
    fi
    PATH="$("$repo/scripts/build-weston.sh" --print-prefix)/bin:$PATH"
    export PATH
fi

# Concurrent suites starve each other's software-rendered pane mounts past their deadlines: queue instead. This shell
# holds the lock and runs the suite in a child without its descriptor, so the instances' session servers, which outlive
# a run by design, do not keep holding it. One descriptor for the try and the wait: no window in which a queued run says
# nothing.
if [[ -z "$locked" ]]; then
    lock="${CONFORMANCE_LOCK:-}"
    if [[ -z "$lock" ]]; then
        lock_dir="${XDG_RUNTIME_DIR:-/tmp/agterm-conformance-$(id -u)}"
        if [[ -z "${XDG_RUNTIME_DIR:-}" ]]; then
            mkdir -p -m 0700 "$lock_dir"
            [[ -O "$lock_dir" ]] || { echo "$lock_dir belongs to another user" >&2; exit 2; }
        fi
        lock="$lock_dir/agterm-conformance.lock"
    fi
    if ! exec {lock_fd}>>"$lock"; then
        echo "cannot open the lock $lock: set CONFORMANCE_LOCK to a writable path" >&2
        exit 2
    fi
    if ! flock -n "$lock_fd"; then
        echo "waiting for another conformance run (lock $lock)"
        flock "$lock_fd" || { echo "cannot take the lock $lock" >&2; exit 2; }
    fi
    status=0
    CONFORMANCE_RUN_LOCKED=1 bash "$here/$(basename "${BASH_SOURCE[0]}")" "${args[@]}" {lock_fd}>&- || status=$?
    exit "$status"
fi

if [[ -z "$stage" ]]; then
    stage="${CARGO_TARGET_DIR:-$repo/target}/conformance-stage"
    # the workspace without what links WPE WebKit (WORKSPACE_WITHOUT_WPE: ngtermctl is in it), then the app with its
    # HTML overlays, which need a WPE WebKit (README > WPE WebKit)
    # shellcheck source=../../scripts/lib/native.sh
    (
        cd "$repo" && source scripts/lib/native.sh && cargo build --release "${WORKSPACE_WITHOUT_WPE[@]}" || exit
        cargo build --release -p ngterm-gpui
    )
    rm -rf "$stage"
    "$repo/scripts/stage.sh" "$stage"
fi
[[ -d "$stage" ]] || { echo "no staged tree at $stage" >&2; exit 2; }
bin_args=()
if [[ -x "$stage/bin/$app" ]]; then
    bin_args=(--bin "$stage/bin/$app")
fi

# A shell inside a live ngterm or agterm session exports its socket and session; the plugin scrubs these too.
while IFS= read -r name; do unset "$name"; done < <(compgen -e | grep -E '^(NGTERM_|AGTERM_|ZMX_)' || true)

mkdir -p "$out"
rm -f "$out/report.md" "$out/junit.xml" "$out/pytest.log" "$out/nodeids.jsonl"
# A per-run parent, so a concurrent run's roots are neither blamed on this one nor listed for removal.
parent="$(mktemp -d /tmp/agt-run-XXXXXX)"
trap 'rmdir "$parent" 2>/dev/null || true' EXIT
export CONFORMANCE_TMP="$parent"

set +e
started=$SECONDS
# -rfE: the hundreds of skips of a build short of parity would bury the failures; the summary line counts them.
"$python" -m pytest "${layers[@]}" --impl rust "${bin_args[@]}" -rfE \
    --junitxml "$out/junit.xml" --nodeid-report "$out/nodeids.jsonl" "${selection[@]}" "${extra[@]}" 2>&1 \
    | tee "$out/pytest.log"
pytest_status=${PIPESTATUS[0]}
elapsed=$((SECONDS - started))
ratchet_log="$("$python" "$here/check_ratchet.py" --report "$out/nodeids.jsonl" --list "$ratchet" 2>&1)"
status=$?
set -e
echo "$ratchet_log"

left="$(find "$parent" -mindepth 1 -maxdepth 1 | sort)"
if [[ -n "$left" && $status -eq 0 ]]; then
    status=1
fi

build_commit="$(git -C "$repo" rev-parse --short HEAD)"
if [[ -n "$(git -C "$repo" status --porcelain)" ]]; then build_commit+=" (dirty)"; fi

{
    echo "## rust-linux conformance"
    echo
    echo "- build: $build_commit, \`$stage\`"
    if [[ -n "$ratchet_only" ]]; then
        echo "- mode: ratchet-only, the ids of \`$ratchet\`$( ((${#also[@]})) && printf ' + --also %s' "${also[@]}")$( \
            ((${#also_k[@]})) && printf ' + --also-k %s' "${also_k[@]}")"
    else
        echo "- mode: full"
    fi
    echo "- suite time: ${elapsed}s"
    if ((${#bin_args[@]})); then echo "- ngterm: \`$stage/bin/$app\`"; else echo "- ngterm: not staged"; fi
    echo "- Vulkan: lavapipe only, \`$lavapipe\`"
    echo "- host: $(uname -sr), $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "- layers: ${layers[*]#"$tests/"}"
    echo "- display: $display"
    if ((${#extra[@]})); then echo "- pytest args: ${extra[*]}"; fi
    echo "- pytest status: $pytest_status"
    echo "- exit status: $status"
    echo
    echo '```'
    grep -E '^rust: ' "$out/pytest.log" || echo "no conformance summary"
    if grep -q 'short test summary info' "$out/pytest.log"; then
        sed -n '/short test summary info/,$p' "$out/pytest.log" | sed '1d'
    else
        tail -n 1 "$out/pytest.log"
    fi
    echo '```'
    echo
    echo "### ratchet"
    echo
    echo '```'
    echo "$ratchet_log"
    echo '```'
    if [[ -n "$left" ]]; then
        echo
        echo "Left behind (kept for diagnosis, remove by hand):"
        printf -- '- `%s`\n' $left
    fi
} > "$out/report.md"

echo "report: $out/report.md"
exit "$status"
