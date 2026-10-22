#!/usr/bin/env bash
# Find Mesa's lavapipe (software Vulkan) for the GPUI build's headless runs and print its ICD manifest, the file
# VK_DRIVER_FILES names. The GPUI build renders through Vulkan; in the conformance harness and ngterm-gpui's process
# tests it must render on lavapipe and on nothing else, never on the machine's GPU driver, so there is no fallback: no
# lavapipe is an error naming this script.
#
# Where it comes from, first match wins:
#   1. NGTERM_LAVAPIPE_ICD, an ICD manifest given by hand (it must exist);
#   2. the distribution's package, the documented source wherever one is installable (Ubuntu/Debian/Fedora
#      `mesa-vulkan-drivers`, Arch `vulkan-swrast`; CI installs Ubuntu's): `lvp_icd.<arch>.json` or `lvp_icd.json` in
#      the Vulkan loader's ICD directories (NGTERM_VULKAN_ICD_DIRS, colon-separated, replaces the list, for tests);
#   3. a rootless machine with an Arch-compatible userland: the pinned Arch `vulkan-swrast` package
#      (LAVAPIPE_ARCH_PACKAGE_URL of build/pins.env), verified against its pinned sha256 and unpacked into the vendored
#      prefix ${NGTERM_VENDOR:-~/.cache/ngterm/vendor}/lavapipe-<key>/ (no root), with an ICD manifest naming the
#      driver by absolute path. The key hashes the LAVAPIPE_ pins and the host arch only. The driver links the host's
#      libLLVM and glibc: the unpack refuses a host where any of its libraries does not resolve.
#
# Usage: scripts/fetch-lavapipe.sh                 print the ICD, unpacking the vendored prefix when that is the source
#                                                  and it is not complete yet
#        scripts/fetch-lavapipe.sh --print-icd     print the ICD without fetching anything; exit 1 when none is there
#        scripts/fetch-lavapipe.sh --print-prefix  print the vendored prefix path only
#        scripts/fetch-lavapipe.sh --verify        exit 0 when the vendored prefix is complete, 1 otherwise
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

PREFIX="$(lavapipe_prefix)"
HINT="install the distribution's lavapipe (Ubuntu/Debian/Fedora: mesa-vulkan-drivers, Arch: vulkan-swrast), run"
HINT+=" scripts/fetch-lavapipe.sh, or set NGTERM_LAVAPIPE_ICD; the GPUI build never renders on a hardware driver here"

# Whether the driver an ICD manifest names is there, by the Vulkan loader's rules: an absolute library_path as is, one
# with a slash relative to the manifest's directory, a bare name on the library search path. A manifest without its
# driver would leave the GPUI build no Vulkan device, and wgpu would quietly render through GL instead.
icd_library_exists() {
  local manifest="$1" library dir dirs
  library="$(sed -n 's/.*"library_path": *"\([^"]*\)".*/\1/p' "$manifest" | head -n1)"
  case "$library" in
    "") return 1 ;;
    /*) [[ -f "$library" ]] ;;
    */*) [[ -f "$(dirname "$manifest")/$library" ]] ;;
    *)
      local search="${LD_LIBRARY_PATH:-}"
      for dir in /usr/lib/*-linux-gnu*; do search+=":$dir"; done
      search+=":/usr/local/lib:/usr/lib64:/usr/lib:/lib64:/lib"
      IFS=: read -r -a dirs <<<"$search"
      for dir in "${dirs[@]}"; do
        [[ -n "$dir" && -f "$dir/$library" ]] && return 0
      done
      { ldconfig -p || /sbin/ldconfig -p || /usr/sbin/ldconfig -p; } 2>/dev/null | grep -qE "^\s*$library .*=> /"
      ;;
  esac
}

# The distribution's lavapipe ICD manifest, if one is installed with its driver.
system_icd() {
  local dirs dir candidate
  local default=/etc/vulkan/icd.d:/usr/local/share/vulkan/icd.d:/usr/share/vulkan/icd.d
  IFS=: read -r -a dirs <<<"${NGTERM_VULKAN_ICD_DIRS-$default}"
  for dir in "${dirs[@]}"; do
    [[ -n "$dir" ]] || continue
    for candidate in "$dir/lvp_icd.$(uname -m).json" "$dir/lvp_icd.json"; do
      [[ -s "$candidate" ]] || continue
      if icd_library_exists "$candidate"; then
        echo "$candidate"
        return 0
      fi
      echo "skipping $candidate: the driver it names is not installed" >&2
    done
  done
  return 1
}

# The vendored prefix's ICD manifest, when the prefix is complete and its driver loads on this host.
vendored_icd() {
  lavapipe_prefix_is_complete "$PREFIX" || return 1
  local missing
  missing="$(unresolved_libraries "$PREFIX/lib/libvulkan_lvp.so")"
  [[ -z "$missing" ]] || die "the vendored lavapipe at $PREFIX needs libraries this host lacks: $missing; $HINT"
  echo "$PREFIX/lvp_icd.json"
}

# The ICD from the override or the system, else the vendored one; status 1 when there is none yet.
find_icd() {
  if [[ -n "${NGTERM_LAVAPIPE_ICD:-}" ]]; then
    [[ -s "$NGTERM_LAVAPIPE_ICD" ]] || die "NGTERM_LAVAPIPE_ICD names no ICD manifest: $NGTERM_LAVAPIPE_ICD"
    icd_library_exists "$NGTERM_LAVAPIPE_ICD" \
      || die "NGTERM_LAVAPIPE_ICD=$NGTERM_LAVAPIPE_ICD names a driver that is not there"
    echo "$NGTERM_LAVAPIPE_ICD"
    return 0
  fi
  system_icd || vendored_icd
}

case "${1:-}" in
  --print-prefix)
    echo "$PREFIX"
    exit 0
    ;;
  --verify)
    lavapipe_prefix_is_complete "$PREFIX"
    exit
    ;;
  --print-icd)
    find_icd || die "no lavapipe ICD found: $HINT"
    exit 0
    ;;
  "") ;;
  *) die "usage: scripts/fetch-lavapipe.sh [--print-icd | --print-prefix | --verify]" ;;
esac

if find_icd; then
  exit 0
fi

[[ "$(uname -m)" == x86_64 ]] || die "the pinned lavapipe package is x86_64 only: $HINT"
for command in curl tar zstd sha256sum getconf ldd; do
  command -v "$command" >/dev/null || die "$command is required to unpack lavapipe"
done
# The package is built against Arch's glibc; an older one cannot load it, so refuse before downloading.
glibc="$(getconf GNU_LIBC_VERSION | awk '{print $2}')"
[[ "$(printf '%s\n%s\n' "$LAVAPIPE_ARCH_GLIBC" "$glibc" | sort -V | head -n1)" == "$LAVAPIPE_ARCH_GLIBC" ]] \
  || die "the pinned lavapipe package needs glibc $LAVAPIPE_ARCH_GLIBC, this host has $glibc: $HINT"

mkdir -p "$VENDOR_ROOT"
# both scratch dirs live in the vendor root, beside the final prefix, not in a shared /tmp
WORK="$(mktemp -d "$VENDOR_ROOT/.lavapipe-fetch.XXXXXX")"
# the stage becomes the prefix in one atomic rename
STAGE="$(mktemp -d "$VENDOR_ROOT/.lavapipe-stage.XXXXXX")"
trap 'rm -rf "$WORK" "$STAGE"' EXIT

echo "fetching lavapipe $LAVAPIPE_VERSION..." >&2
curl -fsSLo "$WORK/vulkan-swrast.pkg.tar.zst" "$LAVAPIPE_ARCH_PACKAGE_URL"
echo "$LAVAPIPE_ARCH_PACKAGE_SHA256  $WORK/vulkan-swrast.pkg.tar.zst" | sha256sum --check --quiet >&2 \
  || die "lavapipe package sha256 mismatch ($LAVAPIPE_ARCH_PACKAGE_URL)"
mkdir "$WORK/pkg"
tar --zstd -xf "$WORK/vulkan-swrast.pkg.tar.zst" -C "$WORK/pkg" usr
mkdir -p "$STAGE/lib" "$STAGE/share/ngterm" "$STAGE/share/licenses"
mv "$WORK/pkg/usr/lib/libvulkan_lvp.so" "$STAGE/lib/libvulkan_lvp.so"
mv "$WORK/pkg/usr/share/licenses/vulkan-swrast" "$STAGE/share/licenses/vulkan-swrast"
# The package's manifest names the driver by file name, for the system's library path; this one names the prefix's,
# by the path the stage is renamed to below.
sed "s|\"library_path\": *\"[^\"]*\"|\"library_path\": \"$PREFIX/lib/libvulkan_lvp.so\"|" \
  "$WORK/pkg/usr/share/vulkan/icd.d/lvp_icd.json" >"$STAGE/lvp_icd.json"
grep -q "\"library_path\": \"$PREFIX/lib/libvulkan_lvp.so\"" "$STAGE/lvp_icd.json" \
  || die "the package's lvp_icd.json has no library_path to relocate"
missing="$(unresolved_libraries "$STAGE/lib/libvulkan_lvp.so")"
[[ -z "$missing" ]] || die "lavapipe $LAVAPIPE_VERSION needs libraries this host lacks: $missing (Arch: the" \
  "vulkan-swrast package's dependencies, llvm-libs among them); $HINT"
pin_lines LAVAPIPE >"$STAGE/share/ngterm/lavapipe-PINS"

lavapipe_prefix_is_complete "$STAGE" || die "the staged lavapipe prefix is incomplete"
chmod 755 "$STAGE"
rm -rf "$PREFIX"
mv "$STAGE" "$PREFIX"
echo "→ vendored lavapipe $LAVAPIPE_VERSION into $PREFIX" >&2
echo "$PREFIX/lvp_icd.json"
