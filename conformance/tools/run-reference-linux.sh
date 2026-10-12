#!/usr/bin/env bash
# Run the protocol, cli and behaviors layers against the installed Swift/GTK fork and write a report to attach to a PR.
# Every instance comes from the harness (unique AGTERM_APP_ID, fresh root under this run's own /tmp/agt-run-*
# parent, its own headless weston); nothing here talks to the user's running agterm or reaches the desktop.
#
#   conformance/tools/run-reference-linux.sh [--bin PATH] [--out DIR] [--tests DIR] [-- PYTEST_ARGS...]
#
# Writes report.md, junit.xml and pytest.log into DIR, replacing only those.
# Exits with pytest's status, or 1 when a run left an instance root behind.
# CONFORMANCE_LINUX_DISPLAY=session draws the instances on this desktop instead, for debugging.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
bin="$HOME/.local/share/agterm-linux/bin/agterm-linux"
out="$repo/conformance/reports/reference-linux"
tests="$repo/conformance/tests"
extra=()

while (($#)); do
    case "$1" in
        --bin) bin="$2"; shift 2 ;;
        --out) out="$2"; shift 2 ;;
        --tests) tests="$2"; shift 2 ;;
        --) shift; extra=("$@"); break ;;
        -h|--help) sed -n '2,10s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[[ -x "$bin" ]] || { echo "no executable fork at $bin (pass --bin)" >&2; exit 2; }
python="${PYTHON:-$repo/.venv/bin/python}"
[[ -x "$python" ]] || python="$(command -v python3)"
# This checkout's harness, ahead of whichever checkout the venv's editable install points at.
export PYTHONPATH="$repo/conformance${PYTHONPATH:+:$PYTHONPATH}"
display="${CONFORMANCE_LINUX_DISPLAY:-headless}"
if [[ "$display" == headless ]] && ! command -v weston >/dev/null; then
    echo "no weston on PATH: install it, or set CONFORMANCE_LINUX_DISPLAY=session" >&2
    exit 2
fi

# A shell inside a live agterm session exports its socket and session; the plugin scrubs these too.
while IFS= read -r name; do unset "$name"; done < <(compgen -e | grep -E '^(AGTERM_|ZMX_)' || true)

layers=()
for layer in protocol cli behaviors; do
    if [[ -d "$tests/$layer" ]]; then layers+=("$tests/$layer"); fi
done
((${#layers[@]})) || { echo "no protocol, cli or behaviors layer under $tests" >&2; exit 2; }

mkdir -p "$out"
rm -f "$out/report.md" "$out/junit.xml" "$out/pytest.log"
# A per-run parent, so a concurrent run's roots are neither blamed on this one nor listed for removal.
parent="$(mktemp -d /tmp/agt-run-XXXXXX)"
trap 'rmdir "$parent" 2>/dev/null || true' EXIT
export CONFORMANCE_TMP="$parent"

set +e
"$python" -m pytest "${layers[@]}" --impl swift-linux --bin "$bin" -ra \
    --junitxml "$out/junit.xml" "${extra[@]}" 2>&1 | tee "$out/pytest.log"
status=${PIPESTATUS[0]}
set -e

left="$(find "$parent" -mindepth 1 -maxdepth 1 | sort)"
if [[ -n "$left" && $status -eq 0 ]]; then
    status=1
fi

share="$(dirname "$(readlink -f "$bin")")/../share/agterm"
fork_version="$(cat "$share/VERSION" 2>/dev/null || echo unknown)"
fork_commit="$(cat "$share/COMMIT" 2>/dev/null || echo unknown)"
suite_commit="$(git -C "$repo" rev-parse --short HEAD)"
if [[ -n "$(git -C "$repo" status --porcelain -- conformance)" ]]; then suite_commit+=" (dirty)"; fi

{
    echo "## reference-linux conformance"
    echo
    echo "- fork: $fork_version ($fork_commit), \`$bin\`"
    echo "- suite: $suite_commit"
    echo "- host: $(uname -sr), $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "- layers: ${layers[*]#"$tests/"}"
    echo "- display: $display"
    if ((${#extra[@]})); then echo "- pytest args: ${extra[*]}"; fi
    echo "- exit status: $status"
    echo
    echo '```'
    grep -E '^swift-linux: ' "$out/pytest.log" || echo "no conformance summary"
    if grep -q 'short test summary info' "$out/pytest.log"; then
        sed -n '/short test summary info/,$p' "$out/pytest.log" | sed '1d'
    else
        tail -n 1 "$out/pytest.log"
    fi
    echo '```'
    if [[ -n "$left" ]]; then
        echo
        echo "Left behind (kept for diagnosis, remove by hand):"
        printf -- '- `%s`\n' $left
    fi
} > "$out/report.md"

echo "report: $out/report.md"
exit "$status"
