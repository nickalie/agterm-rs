#!/usr/bin/env bash
# Vendor calloop (a dependency of GPUI's Linux platform) into vendor/calloop with build/patches/calloop applied, for the
# workspace's [patch.crates-io]. The only change drops calloop's unconditional request for tracing's `log` feature
# (the patch file says why: workspace feature unification made clippy's cognitive_complexity count tracing's
# generated `log` fallback in every crate). scripts/check-tracing-features.sh keeps the feature out of the graph.
#
# The source is the crates.io archive of CALLOOP_VERSION, verified against CALLOOP_SHA256 (the checksum Cargo.lock
# records for it), unpacked verbatim apart from its .gitignore and Cargo.lock (which would hide or pin nothing here),
# then patched. Bump both constants together with GPUI's calloop requirement, run this, and check that the patch still
# applies.
#
# Usage: scripts/vendor-calloop.sh           (re)create vendor/calloop
#        scripts/vendor-calloop.sh --check   rebuild into a scratch dir and fail if vendor/calloop differs
set -euo pipefail

CALLOOP_VERSION="0.14.5"
CALLOOP_SHA256="43b32313e937c8721c9d17ee9bf60699a6402b4b7c2cb6ac1bee0ffcaa4b4e5e"

root="$(cd "$(dirname "$0")/.." && pwd)"
target="$root/vendor/calloop"
patches="$root/build/patches/calloop"

die() {
  echo "$*" >&2
  exit 1
}

check=false
case "${1:-}" in
  "") ;;
  --check) check=true ;;
  -h | --help) sed -n '2,13s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
  *) die "unknown argument: $1 (see --help)" ;;
esac

scratch="$(mktemp -d "${TMPDIR:-/tmp}/ngterm-calloop-XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

archive="$scratch/calloop-$CALLOOP_VERSION.crate"
curl -sSfL -A ngterm-dev -o "$archive" "https://static.crates.io/crates/calloop/calloop-$CALLOOP_VERSION.crate"
sum="$(sha256sum "$archive" | cut -d' ' -f1)"
[[ "$sum" == "$CALLOOP_SHA256" ]] || die "calloop $CALLOOP_VERSION: sha256 $sum, expected $CALLOOP_SHA256"
mkdir "$scratch/src"
tar -xzf "$archive" -C "$scratch/src"
built="$scratch/src/calloop-$CALLOOP_VERSION"
rm -f "$built/.gitignore" "$built/Cargo.lock"
for patch in "$patches"/*.patch; do
  patch -s -p1 -d "$built" <"$patch" || die "$(basename "$patch") does not apply to calloop $CALLOOP_VERSION"
done

if $check; then
  diff -r "$built" "$target" >&2 || die "vendor/calloop differs from calloop $CALLOOP_VERSION + build/patches/calloop"
  echo "vendor/calloop is calloop $CALLOOP_VERSION + build/patches/calloop"
  exit 0
fi

mkdir -p "$root/vendor"
rm -rf "$target.new"
mv "$built" "$target.new"
rm -rf "$target"
mv "$target.new" "$target"
echo "vendored calloop $CALLOOP_VERSION into $target"
