# Shared helpers of the native build scripts (build-ghostty.sh, build-layer-shell.sh, pkg-config.sh, stage.sh, later
# build-zmx.sh). Sourced, not run.
#
# Each native dependency lives in its own vendored prefix, ${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/<name>-<key>/,
# where <key> hashes that dependency's pins and patches only. Cargo never runs zig: agterm-ghostty-sys asks
# `build-ghostty.sh --print-prefix` where the prefix is and fails when it is missing. A relative AGTERM_VENDOR is
# taken against the repository root, so every caller (and cargo, which runs from a crate directory) sees one path.

NATIVE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PINS="$NATIVE_ROOT/build/pins.env"
VENDOR_ROOT="${AGTERM_VENDOR:-$HOME/.cache/agterm-rs/vendor}"
[[ "$VENDOR_ROOT" == /* ]] || VENDOR_ROOT="$NATIVE_ROOT/$VENDOR_ROOT"
# shellcheck source=../../build/pins.env
source "$PINS"

die() {
  echo "$*" >&2
  exit 1
}

sha256_of() {
  if command -v sha256sum >/dev/null; then
    sha256sum | cut -d' ' -f1
  else
    shasum -a 256 | cut -d' ' -f1
  fi
}

# The pin lines of one dependency (every line starting with PREFIX_), as recorded in its prefix.
pin_lines() {
  grep -E "^$1_" "$PINS"
}

# A key over the pin lines of one dependency, the patches of one directory (top level only) and the host arch.
prefix_key() {
  local pins_prefix="$1" patches="$2" patch
  {
    echo "arch $(uname -m)"
    pin_lines "$pins_prefix"
    for patch in "$patches"/*.patch; do
      [[ -f "$patch" ]] || continue
      echo "patch $(basename "$patch")"
      cat "$patch"
    done
  } | sha256_of | cut -c1-16
}

ghostty_prefix() {
  echo "$VENDOR_ROOT/ghostty-$(prefix_key GHOSTTY "$NATIVE_ROOT/build/patches/ghostty")"
}

# gtk4-layer-shell carries no patches: its key hashes the LAYER_SHELL_ pins only.
layer_shell_prefix() {
  echo "$VENDOR_ROOT/gtk4-layer-shell-$(prefix_key LAYER_SHELL /nonexistent)"
}

# A gtk4-layer-shell prefix is complete when its library, header and pkg-config file are all there.
layer_shell_prefix_is_complete() {
  local prefix="$1"
  [[ -s "$prefix/lib/libgtk4-layer-shell.so" && -s "$prefix/include/gtk4-layer-shell/gtk4-layer-shell.h" \
    && -s "$prefix/lib/pkgconfig/gtk4-layer-shell-0.pc" ]]
}

# The Zig triple arch and the matching pinned sha256 variable suffix for this host.
host_zig_arch() {
  case "$(uname -m)" in
    x86_64 | amd64) echo x86_64 ;;
    aarch64 | arm64) echo aarch64 ;;
    *) die "no pinned Zig for $(uname -m)" ;;
  esac
}

# Print a zig of exactly VERSION: $OVERRIDE_VAR, a matching zig on PATH, mise's, or the pinned ziglang.org
# tarball (sha256 SHA) unpacked once into the vendor root.
#
# Callers run this as ZIG="$(find_zig ...)", where bash clears errexit, so every step checks its own failure: a
# failed fetch or unpack must never leave a partial tree in the final directory, which later runs would reuse.
find_zig() {
  local version="$1" sha="$2" override="$3" candidate arch dir tarball
  for candidate in "$override" "$(command -v zig 2>/dev/null || true)" \
    "$(mise where "zig@$version" 2>/dev/null || true)/bin/zig"; do
    if [[ -n "$candidate" && -x "$candidate" && "$("$candidate" version 2>/dev/null)" == "$version" ]]; then
      echo "$candidate"
      return
    fi
  done
  [[ -z "$override" ]] || die "$override is not zig $version"
  arch="$(host_zig_arch)" || exit
  dir="$VENDOR_ROOT/zig-$arch-linux-$version"
  if [[ ! -x "$dir/zig" ]]; then
    command -v curl >/dev/null || die "curl is required to fetch zig $version"
    command -v xz >/dev/null || die "xz is required to unpack zig $version"
    tarball="$(mktemp "$VENDOR_ROOT/.zig-$version.XXXXXX")" || die "cannot create a download file in $VENDOR_ROOT"
    echo "fetching zig $version..." >&2
    curl -fsSLo "$tarball" "https://ziglang.org/download/$version/zig-$arch-linux-$version.tar.xz" \
      || { rm -f "$tarball"; die "cannot download zig $version"; }
    echo "$sha  $tarball" | sha256sum --check --quiet >&2 || { rm -f "$tarball"; die "zig $version sha256 mismatch"; }
    { rm -rf "$dir.partial" && mkdir -p "$dir.partial"; } || { rm -f "$tarball"; die "cannot create $dir.partial"; }
    tar -xJf "$tarball" -C "$dir.partial" --strip-components=1 \
      || { rm -rf "$dir.partial" "$tarball"; die "cannot unpack zig $version"; }
    rm -f "$tarball"
    { rm -rf "$dir" && mv "$dir.partial" "$dir"; } || { rm -rf "$dir.partial"; die "cannot move zig into $dir"; }
  fi
  [[ "$("$dir/zig" version)" == "$version" ]] || die "$dir/zig is not zig $version"
  echo "$dir/zig"
}

# Verify the Ghostty resources below SHARE (port of ../agterm-linux scripts/verify-linux-resources.sh).
verify_ghostty_resources() {
  local share="$1" ghostty themes terminfo theme_count theme file
  command -v infocmp >/dev/null || { echo "infocmp is required to verify Ghostty resources" >&2; return 1; }
  ghostty="$share/ghostty"
  themes="$ghostty/themes"
  terminfo="$share/terminfo"
  for file in "$ghostty/shell-integration/bash/ghostty.bash" "$ghostty/shell-integration/zsh/ghostty-integration" \
    "$themes/.agterm-resource-manifest" "$terminfo/x/xterm-ghostty"; do
    [[ -s "$file" ]] || { echo "missing or empty Ghostty resource: $file" >&2; return 1; }
  done
  pin_lines GHOSTTY | cmp -s - "$themes/.agterm-resource-manifest" || {
    echo "Ghostty resource provenance does not match the GHOSTTY_ pins in $PINS" >&2
    return 1
  }
  theme_count="$(find "$themes" -maxdepth 1 -type f ! -name '.agterm-resource-manifest' | wc -l)"
  if ((theme_count != GHOSTTY_THEME_COUNT)); then
    echo "Ghostty theme catalog has $theme_count files; expected $GHOSTTY_THEME_COUNT" >&2
    return 1
  fi
  for theme in "${GHOSTTY_KNOWN_THEMES[@]}"; do
    [[ -s "$themes/$theme" ]] || { echo "missing or empty Ghostty theme: $themes/$theme" >&2; return 1; }
  done
  infocmp -A "$terminfo" xterm-ghostty >/dev/null || {
    echo "xterm-ghostty is not readable from $terminfo" >&2
    return 1
  }
  echo "→ verified Ghostty resources from $GHOSTTY_REV ($theme_count themes + xterm-ghostty)"
}

# A prefix is complete when the library, both headers and the resources are all there and verify.
ghostty_prefix_is_complete() {
  local prefix="$1"
  [[ -s "$prefix/lib/libghostty.so" && -s "$prefix/include/ghostty.h" && -s "$prefix/include/ghostty/vt.h" ]] \
    && verify_ghostty_resources "$prefix/share" >/dev/null 2>&1
}
