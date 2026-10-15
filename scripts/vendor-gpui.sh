#!/usr/bin/env bash
# Vendor the GPUI crates agterm patches (GPUI patch 1 of docs/plans/20261014-gpui-frontend.md, Technical Details >
# GPUI patches) into vendor/<crate> with build/patches/<crate> applied, for the workspace's [patch.crates-io]:
#   gpui-pre       the `ExternalSurface` payload of `PaintSurface` on Linux and `Window::paint_external_surface`, the
#                  dmabuf import capabilities of the GPU GPUI renders with
#   gpui-pre-wgpu  the renderer's side of external surfaces (dmabufs imported through wgpu-hal Vulkan, acquired from
#                  the foreign queue family and copied on the GPU into a texture of its own; CPU pixels uploaded by
#                  damaged rectangles) and the Vulkan device opened with VK_EXT_image_drm_format_modifier and
#                  VK_EXT_queue_family_foreign, which wgpu does not enable itself
#   gpui-pre-linux the native key behind each key event GPUI dispatches on Wayland and X11 (its XKB keycode, layout
#                  character and modifiers), which GPUI's `Keystroke` drops, for the terminal's physical-key bindings
#                  and the Kitty keyboard protocol (Task 14 of the plan)
#
# Each source is the crates.io archive of GPUI_VERSION, verified against SHA256 below: the release's crates.io
# checksum (the `cksum` of its version's line in the crates.io index, https://index.crates.io/gp/ui/<crate>, also what
# a Cargo.lock without the [patch.crates-io] entries records). It is unpacked verbatim apart from its Cargo.lock (which
# pins nothing for a dependency) and, for gpui-pre, its examples/ directory (5 MB of demo programs and images no build
# of ours compiles; Cargo reads a dependency's [[example]] entries without looking for their files), then patched.
# Bump GPUI_VERSION and the sums together with gpui-kit's `gpui-pre` pin, run this, and check that the patches still
# apply.
#
# Usage: scripts/vendor-gpui.sh           (re)create vendor/gpui-pre, vendor/gpui-pre-wgpu and vendor/gpui-pre-linux
#        scripts/vendor-gpui.sh --check   rebuild into a scratch dir and fail if a vendored crate differs
#        scripts/vendor-gpui.sh --diff    regenerate build/patches/<crate>/0001-*.patch from the vendored trees
#                                         (after editing vendor/; the patch's leading description is kept)
set -euo pipefail
shopt -s nullglob

GPUI_VERSION="0.3.9"
declare -A SHA256=(
  [gpui-pre]="34f61c1cfd36fc9c436af09308c963822ae27a45a84feb21a77ba5c9e051f1c0"
  [gpui-pre-wgpu]="5b1cdb26c1503bf9662ee8fbc5ab0b44b2deeee4b5c12dc67491ba6fb5fc544b"
  [gpui-pre-linux]="e28213c3fde6f13e91ba0b74b90cbaadec9373e36a031eda98561d31059870f3"
)
CRATES=(gpui-pre gpui-pre-wgpu gpui-pre-linux)

root="$(cd "$(dirname "$0")/.." && pwd)"

die() {
  echo "$*" >&2
  exit 1
}

mode=vendor
case "${1:-}" in
  "") ;;
  --check) mode=check ;;
  --diff) mode=diff ;;
  -h | --help) sed -n '2,22s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
  *) die "unknown argument: $1 (see --help)" ;;
esac

scratch="$(mktemp -d "${TMPDIR:-/tmp}/agterm-gpui-XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

# The verified, pruned crates.io source of $1, unpatched, in $scratch/pristine/$1.
pristine() {
  local crate="$1" archive sum
  archive="$scratch/$crate-$GPUI_VERSION.crate"
  curl -sSfL -A agterm-rs-dev -o "$archive" "https://static.crates.io/crates/$crate/$crate-$GPUI_VERSION.crate"
  sum="$(sha256sum "$archive" | cut -d' ' -f1)"
  [[ "$sum" == "${SHA256[$crate]}" ]] || die "$crate $GPUI_VERSION: sha256 $sum, expected ${SHA256[$crate]}"
  mkdir -p "$scratch/unpack" "$scratch/pristine"
  tar -xzf "$archive" -C "$scratch/unpack"
  mv "$scratch/unpack/$crate-$GPUI_VERSION" "$scratch/pristine/$crate"
  rm -f "$scratch/pristine/$crate/Cargo.lock"
  if [[ "$crate" == gpui-pre ]]; then
    rm -rf "$scratch/pristine/$crate/examples"
  fi
}

# The crate $1 with its patches applied, in $scratch/built/$1.
built() {
  local crate="$1" patch
  mkdir -p "$scratch/built"
  cp -a "$scratch/pristine/$crate" "$scratch/built/$crate"
  for patch in "$root/build/patches/$crate"/*.patch; do
    patch -s -p1 -d "$scratch/built/$crate" <"$patch" || die "$(basename "$patch") does not apply to $crate $GPUI_VERSION"
  done
}

for crate in "${CRATES[@]}"; do
  pristine "$crate"
  target="$root/vendor/$crate"
  case "$mode" in
    diff)
      patch="$(ls "$root/build/patches/$crate"/0001-*.patch)"
      # $(...) drops the description's trailing blank lines; one blank line separates it from the diff
      description="$(sed -n '/^diff -ruN /q;/^--- a\//q;p' "$patch")"
      work="$scratch/diff/$crate"
      mkdir -p "$work"
      mv "$scratch/pristine/$crate" "$work/a"
      cp -a "$target" "$work/b"
      {
        printf '%s\n\n' "$description"
        (cd "$work" && diff -ruN a b | sed -E 's/^(---|\+\+\+) ([ab]\/[^\t]*)\t.*/\1 \2/') || true
      } >"$patch.new"
      mv "$patch.new" "$patch"
      echo "regenerated $patch"
      ;;
    check)
      built "$crate"
      diff -r "$scratch/built/$crate" "$target" >&2 || die "vendor/$crate differs from $crate $GPUI_VERSION + build/patches/$crate"
      echo "vendor/$crate is $crate $GPUI_VERSION + build/patches/$crate"
      ;;
    vendor)
      built "$crate"
      mkdir -p "$root/vendor"
      rm -rf "$target.new"
      mv "$scratch/built/$crate" "$target.new"
      rm -rf "$target"
      mv "$target.new" "$target"
      echo "vendored $crate $GPUI_VERSION into $target"
      ;;
  esac
done
