#!/usr/bin/env bash
# List every mention of the old upstream version in the checkout, each with what to do about it.
#
#   tag-mentions.sh OLD_TAG [NEW_TAG --apply]
#
# Classes (first matching rule wins, by path):
#   generated  catalog.yaml, ui-ids.yaml: gen_catalog.py rewrites them; never edit by hand
#   bump       the parity *target*: README.md, CLAUDE.md, conformance/README.md, spike-macos.yml's default tag
#   reference  swift_macos.py's PARITY_LEVEL, the macOS reference app's level, which is the catalog's release; its
#              mirror is the `parity_level` assert in selftests/test_impls.py::test_swift_macos_profile_follows_launch_mode.
#              Edit both by hand (the selftest file also holds unrelated version fixtures; the script tags only
#              that assert `reference`, the rest of the file `keep`)
#   pins       build/pins.env comments: rewritten with the pins (pins-delta.sh), dates included
#   resources  integrations/mod.rs: names the release resources/ was copied from; bump once sync-resources.sh ran
#   claim      what the Rust build *claims* to implement: PARITY_LEVEL in agterm-proto, the tests and docs that
#              mirror it, linux-ui-checklist.md's header. Bumped in the plan's last task, after the port, never in
#              the mechanical bump: parityLevel unlocks `since(V)` tests and states a parity not yet reached
#   cite       a citation of what upstream said at that release (fork-bugs.md): re-check it against the new
#              release before bumping, or keep it
#   keep       history and fixtures: spike-macos.md, the installed fork's level (swift_linux.py), selftests and
#              their fixtures, example values in --help texts and tests (quick.rs)
#   review     no rule knows this path: decide, then add a rule here
# With NEW_TAG --apply, rewrites the `bump` lines in place (vX.Y.Z and X.Y.Z forms) and lists the rest.
set -euo pipefail

[[ $# -eq 1 || ( $# -eq 3 && "$3" == --apply ) ]] || { echo "usage: tag-mentions.sh OLD_TAG [NEW_TAG --apply]" >&2; exit 1; }
old="${1#v}"
new="${2:-}"; new="${new#v}"
apply=$([[ $# -eq 3 ]] && echo 1 || echo 0)
root="$(git rev-parse --show-toplevel)"
cd "$root"

classify() {
  case "$1" in
    conformance/catalog.yaml|conformance/ui-ids.yaml) echo generated ;;
    README.md|CLAUDE.md|conformance/README.md|.github/workflows/spike-macos.yml) echo bump ;;
    conformance/agterm_conformance/impls/swift_macos.py) echo reference ;;
    build/pins.env) echo pins ;;
    crates/agterm-gtk/src/integrations/mod.rs) echo resources ;;
    crates/agterm-proto/src/version.rs|crates/agterm-core/src/dispatch.rs|crates/agterm-server/tests/server.rs) echo claim ;;
    conformance/agterm_conformance/impls/rust.py|conformance/docs/rust-build.md|docs/linux-ui-checklist.md) echo claim ;;
    conformance/docs/fork-bugs.md) echo cite ;;
    conformance/docs/spike-macos.md|conformance/agterm_conformance/impls/swift_linux.py) echo keep ;;
    conformance/selftests/*|conformance/tools/*.py|crates/agterm-gtk/src/quick.rs) echo keep ;;
    *) echo review ;;
  esac
}

pattern="v?${old//./\\.}([^0-9]|$)"
# tracked files only (git grep): skips target/, .venv/, .upstream/, reports and other untracked scratch;
# completed and open plans are history, never rewritten
mapfile -t hits < <(git grep -nIE "$pattern" -- . ':!docs/plans/' ':!.claude/' || true)

# the enclosing `def` of a line, for the one selftest line that mirrors swift_macos.py's level
enclosing_def() { awk -v n="$2" 'NR <= n && /^(async )?def / { name = $0 } NR == n { print name; exit }' "$1"; }

declare -A seen_bump=()
for hit in "${hits[@]}"; do
  file="${hit%%:*}"
  class="$(classify "$file")"
  if [[ "$file" == conformance/selftests/test_impls.py ]]; then
    rest="${hit#*:}"; line="${rest%%:*}"
    [[ "$(enclosing_def "$file" "$line")" == *swift_macos_profile* && "$hit" == *parity_level* ]] && class=reference
  fi
  printf '%-9s %s\n' "$class" "$(cut -c1-200 <<<"$hit")"
  [[ "$class" == bump ]] && seen_bump["$file"]=1
done

if [[ "$apply" == 1 ]]; then
  for file in "${!seen_bump[@]}"; do
    sed -i -E "s/(v?)${old//./\\.}([^0-9]|$)/\1${new}\2/g" "$file"
    echo "bumped: $file" >&2
  done
fi
