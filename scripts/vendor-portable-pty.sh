#!/usr/bin/env bash
# Vendor portable-pty (agterm-pty's PTY and child: openpty on Unix, ConPTY on Windows) into vendor/portable-pty with
# build/patches/portable-pty applied, for the workspace's [patch.crates-io]. Every patch is a Windows one; each file's
# leading description says why it exists, where it came from and when it can go:
#   0001 the ConPTY functions of the already loaded kernel32 only, never a bare conpty.dll through the DLL search path
#        (herdr's 0001 reduced: herdr also loads a hash-checked app-local conpty.dll, which agterm does not ship)
#   0002 malformed Windows environments rejected (herdr's 0003)
#   0003 directories skipped when resolving a program on PATH (herdr's 0004)
#   0004 a child spawned straight into a job object (PROC_THREAD_ATTRIBUTE_JOB_LIST), so no descendant escapes it
#   0005 a ConPTY master's handles exported, and a master rebuilt from handles another process handed over (the
#        session server's update handoff)
# herdr's 0002 (raw cmd.exe command tails) is not carried: agterm launches no cmd.exe command text.
#
# The source is the crates.io archive of PORTABLE_PTY_VERSION, verified against PORTABLE_PTY_SHA256 (the checksum
# Cargo.lock records for it), unpacked verbatim apart from its Cargo.lock (which pins nothing for a dependency), then
# patched. Bump both constants together with the workspace's portable-pty requirement, run this, and check that the
# patches still apply.
#
# Usage: scripts/vendor-portable-pty.sh           (re)create vendor/portable-pty
#        scripts/vendor-portable-pty.sh --check   rebuild into a scratch dir and fail if vendor/portable-pty differs
#        scripts/vendor-portable-pty.sh --diff    regenerate the last patch, build/patches/portable-pty/NNNN-*.patch, as
#                                                 the vendored tree against the crate with the earlier patches applied
#                                                 (after editing vendor/; the patch's leading description is kept). A
#                                                 new patch starts as a file holding only its description, numbered
#                                                 after the others; edits that belong to an earlier patch go there by
#                                                 hand.
set -euo pipefail
shopt -s nullglob

PORTABLE_PTY_VERSION="0.9.0"
PORTABLE_PTY_SHA256="b4a596a2b3d2752d94f51fac2d4a96737b8705dddd311a32b9af47211f08671e"

root="$(cd "$(dirname "$0")/.." && pwd)"
target="$root/vendor/portable-pty"
patches="$root/build/patches/portable-pty"

die() {
  echo "$*" >&2
  exit 1
}

mode=vendor
case "${1:-}" in
  "") ;;
  --check) mode=check ;;
  --diff) mode=diff ;;
  -h | --help) sed -n '2,27s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
  *) die "unknown argument: $1 (see --help)" ;;
esac

scratch="$(mktemp -d "${TMPDIR:-/tmp}/agterm-portable-pty-XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

archive="$scratch/portable-pty-$PORTABLE_PTY_VERSION.crate"
curl -sSfL -A agterm-rs-dev -o "$archive" \
  "https://static.crates.io/crates/portable-pty/portable-pty-$PORTABLE_PTY_VERSION.crate"
sum="$(sha256sum "$archive" | cut -d' ' -f1)"
[[ "$sum" == "$PORTABLE_PTY_SHA256" ]] ||
  die "portable-pty $PORTABLE_PTY_VERSION: sha256 $sum, expected $PORTABLE_PTY_SHA256"
mkdir "$scratch/unpack"
tar -xzf "$archive" -C "$scratch/unpack"
pristine="$scratch/unpack/portable-pty-$PORTABLE_PTY_VERSION"
rm -f "$pristine/Cargo.lock"

# The crate with its patches applied, in $scratch/built; with $1, only the patches before that one.
built() {
  local stop="${1:-}" patch
  cp -a "$pristine" "$scratch/built"
  for patch in "$patches"/*.patch; do
    [[ "$patch" == "$stop" ]] && break
    patch -s -p1 -d "$scratch/built" <"$patch" ||
      die "$(basename "$patch") does not apply to portable-pty $PORTABLE_PTY_VERSION"
  done
}

case "$mode" in
  diff)
    all=("$patches"/*.patch)
    ((${#all[@]})) || die "no patch in $patches"
    patch="${all[-1]}"
    # $(...) drops the description's trailing blank lines; one blank line separates it from the diff
    description="$(sed -n '/^diff -ruN /q;/^--- a\//q;p' "$patch")"
    built "$patch"
    mkdir "$scratch/diff"
    mv "$scratch/built" "$scratch/diff/a"
    cp -a "$target" "$scratch/diff/b"
    {
      printf '%s\n\n' "$description"
      (cd "$scratch/diff" && diff -ruN a b | sed -E 's/^(---|\+\+\+) ([ab]\/[^\t]*)\t.*/\1 \2/') || true
    } >"$patch.new"
    mv "$patch.new" "$patch"
    echo "regenerated $patch"
    ;;
  check)
    built
    diff -r "$scratch/built" "$target" >&2 ||
      die "vendor/portable-pty differs from portable-pty $PORTABLE_PTY_VERSION + build/patches/portable-pty"
    echo "vendor/portable-pty is portable-pty $PORTABLE_PTY_VERSION + build/patches/portable-pty"
    ;;
  vendor)
    built
    mkdir -p "$root/vendor"
    rm -rf "$target.new"
    mv "$scratch/built" "$target.new"
    rm -rf "$target"
    mv "$target.new" "$target"
    echo "vendored portable-pty $PORTABLE_PTY_VERSION into $target"
    ;;
esac
