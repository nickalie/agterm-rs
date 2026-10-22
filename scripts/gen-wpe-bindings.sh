#!/usr/bin/env bash
# Regenerate the checked-in WPE WebKit -sys crates with gtk-rs `gir` (sys mode), one crate per GObject namespace:
#   crates/ngterm-wpe-javascriptcore-sys  WPEJavaScriptCore-2.0 (JSCValue and friends of the script bridge)
#   crates/ngterm-wpe-platform-sys        WPEPlatform-2.0 (display, view, buffers, events, keymap)
#   crates/ngterm-wpe-sys                 WPEWebKit-2.0 (web view, network session, user content, policies)
# Each crate's Gir.toml is the input checked in beside its output. Run after a WPE_ or GIR_ pin bump of
# build/pins.env, then review the diff: a pin bump is a change of its own.
#
# The inputs are all pinned and verified, so the output is reproducible: gir (built once into the vendored prefix
# ${NGTERM_VENDOR:-~/.cache/ngterm/vendor}/gir-<key>/ from its source archive and build/patches/gir/), the GLib,
# GObject and Gio .gir files of gtk-rs/gir-files, the Soup-3.0.gir of soup3-rs and the WPE .gir files of the pinned
# WPE WebKit package, whatever WPE the system has. Those are read from the vendored prefix when scripts/fetch-wpe.sh
# filled it, else extracted from the verified package alone: the .gir XML is the same on every host, so generating
# needs no WPE that loads here (any arch, any glibc). The generated files name their inputs by
# directory (`gir-files-<rev>`, `soup3-rs`, `wpewebkit-<version>`): gir runs inside the staged inputs with git out of
# reach, so no path or checkout state of this machine leaks into the output. rustfmt with the repository's
# rustfmt.toml formats the output (gir's own `cargo fmt` would not find the workspace from a scratch copy).
#
# Usage: scripts/gen-wpe-bindings.sh          regenerate the crates in place
#        scripts/gen-wpe-bindings.sh --check  regenerate into a scratch copy and fail on any difference
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

CRATES=(ngterm-wpe-javascriptcore-sys ngterm-wpe-platform-sys ngterm-wpe-sys)
# the namespaces of the pinned package the crates are generated from; gir reads the rest through their includes
WPE_GIRS=(WPEJavaScriptCore-2.0 WPEPlatform-2.0 WPEWebKit-2.0)

case "${1:-}" in
  --check) CHECK=1 ;;
  "") CHECK=0 ;;
  *) die "usage: scripts/gen-wpe-bindings.sh [--check]" ;;
esac

for command in curl tar patch sha256sum cargo rustfmt diff; do
  command -v "$command" >/dev/null || die "$command is required to generate the WPE bindings"
done

mkdir -p "$VENDOR_ROOT"
WORK="$(mktemp -d "$VENDOR_ROOT/.wpe-gir.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# Fetch URL into FILE and check it against SHA.
fetch() {
  local url="$1" file="$2" sha="$3"
  curl -fsSLo "$file" "$url" || die "cannot download $url"
  echo "$sha  $file" | sha256sum --check --quiet >&2 || die "sha256 mismatch: $url"
}

# Set GIR to the vendored gir binary, building it first unless its prefix is complete. Runs in this shell, not in a
# command substitution, so errexit holds for every step; the build and the stage live in WORK, beside the prefix.
build_gir() {
  local prefix build="$WORK/gir-build" stage="$WORK/gir-stage"
  prefix="$(gir_prefix)"
  if ! gir_prefix_is_complete "$prefix"; then
    mkdir -p "$build/src" "$stage/bin" "$stage/share/ngterm"
    echo "building gir ${GIR_REV:0:12}..." >&2
    fetch "$GIR_REPO/archive/$GIR_REV.tar.gz" "$build/source.tar.gz" "$GIR_SHA256"
    tar -xzf "$build/source.tar.gz" -C "$build/src" --strip-components=1
    for patch in "$NATIVE_ROOT"/build/patches/gir/*.patch; do
      patch -d "$build/src" -p1 --batch --forward --fuzz=0 --no-backup-if-mismatch --quiet <"$patch"
    done
    # from the repository, so rustup picks rust-toolchain.toml's toolchain (no default one needs to exist)
    (cd "$NATIVE_ROOT" && GIR_VERSION="${GIR_REV:0:12}" CARGO_TARGET_DIR="$build/target" \
      cargo build --manifest-path "$build/src/Cargo.toml" --release --locked --quiet) >&2 \
      || die "the gir build failed"
    cp "$build/target/release/gir" "$stage/bin/gir"
    pin_lines GIR >"$stage/share/ngterm/gir-PINS"
    gir_prefix_is_complete "$stage" || die "the staged gir prefix is incomplete"
    chmod 755 "$stage"
    rm -rf "$prefix"
    mv "$stage" "$prefix"
    echo "→ vendored gir into $prefix" >&2
  fi
  GIR="$prefix/bin/gir"
}

# The input directories under the staged girs, one per source. Their names are what the output records.
GIR_DIRS=("gir-files-${GIR_FILES_REV:0:12}" soup3-rs "wpewebkit-$WPE_VERSION")

# Stage every .gir input under DIR, in the GIR_DIRS layout.
stage_girs() {
  local dir="$1" files="${GIR_DIRS[0]}" soup="${GIR_DIRS[1]}" wpe="${GIR_DIRS[2]}" wpe_girs name
  mkdir -p "$dir/$files" "$dir/$soup" "$dir/$wpe"
  fetch "$GIR_FILES_REPO/archive/$GIR_FILES_REV.tar.gz" "$WORK/gir-files.tar.gz" "$GIR_FILES_SHA256"
  tar -xzf "$WORK/gir-files.tar.gz" -C "$dir/$files" --strip-components=1 --wildcards '*/GLib-2.0.gir' \
    '*/GObject-2.0.gir' '*/Gio-2.0.gir' '*/GioUnix-2.0.gir' '*/GLibUnix-2.0.gir' '*/GModule-2.0.gir'
  fetch "$GIR_SOUP_URL" "$dir/$soup/Soup-3.0.gir" "$GIR_SOUP_SHA256"
  wpe_girs="$(wpe_prefix)/share/gir-1.0"
  if ! wpe_prefix_is_complete "$(wpe_prefix)"; then
    # only the .gir files of the verified package: none of fetch-wpe.sh's checks that the engine loads on this host
    command -v zstd >/dev/null || die "zstd is required to read the WPE WebKit package"
    fetch "$WPE_ARCH_PACKAGE_URL" "$WORK/wpewebkit.pkg.tar.zst" "$WPE_ARCH_PACKAGE_SHA256"
    mkdir "$WORK/wpe-pkg"
    tar --zstd -xf "$WORK/wpewebkit.pkg.tar.zst" -C "$WORK/wpe-pkg" usr/share/gir-1.0
    wpe_girs="$WORK/wpe-pkg/usr/share/gir-1.0"
  fi
  for name in "${WPE_GIRS[@]}"; do
    cp "$wpe_girs/$name.gir" "$dir/$wpe/"
  done
  # WPEJavaScriptCore names a pkg-config package no WPE build installs: its headers and symbols ship in WPEWebKit's
  # (include/wpe-webkit-2.0/jsc, libWPEWebKit-2.0), so the crate probes that one, in Cargo.toml and the ABI tests.
  sed -i 's|<package name="wpe-javascriptcore-2.0"/>|<package name="wpe-webkit-2.0"/>|' \
    "$dir/$wpe/WPEJavaScriptCore-2.0.gir"
  grep -q '<package name="wpe-webkit-2.0"/>' "$dir/$wpe/WPEJavaScriptCore-2.0.gir" \
    || die "WPEJavaScriptCore-2.0.gir no longer names wpe-javascriptcore-2.0: review the package fix"
}

# Generate every crate below OUT (holding a copy of each crate's Gir.toml and hand-written files) from the girs in
# GIRS.
generate() {
  local gir="$1" girs="$2" out="$3" crate
  stage_girs "$girs"
  for crate in "${CRATES[@]}"; do
    echo "generating $crate..." >&2
    (
      cd "$girs"
      # GIT_DIR keeps gir's provenance probes off any repository around the scratch dirs
      GIT_DIR=/nonexistent "$gir" --disable-format -c "$out/$crate/Gir.toml" -o "$out/$crate" \
        "${GIR_DIRS[@]/#/-d}"
    ) >&2 || die "gir failed on $crate"
    # gir records each input directory as a submodule of unknown revision; the names carry the revisions
    sed -i 's| (@ ???)$||' "$out/$crate/versions.txt"
    # the rustfmt of rust-toolchain.toml (picked from the repository), as `cargo fmt --all --check` runs it
    (cd "$NATIVE_ROOT" && find "$out/$crate" -name '*.rs' -not -path '*/target/*' -print0 \
      | xargs -0 rustfmt --edition 2024 --config-path "$NATIVE_ROOT/rustfmt.toml")
  done
}

build_gir
if ((CHECK)); then
  mkdir "$WORK/out"
  for crate in "${CRATES[@]}"; do
    cp -R "$NATIVE_ROOT/crates/$crate" "$WORK/out/$crate"
  done
  generate "$GIR" "$WORK/girs" "$WORK/out"
  status=0
  for crate in "${CRATES[@]}"; do
    diff -ru --exclude=target "$NATIVE_ROOT/crates/$crate" "$WORK/out/$crate" >&2 || status=1
  done
  ((status == 0)) \
    || die "the checked-in WPE bindings differ from a regeneration at the pins: run scripts/gen-wpe-bindings.sh"
  echo "→ the WPE bindings match a regeneration at the pins" >&2
else
  generate "$GIR" "$WORK/girs" "$NATIVE_ROOT/crates"
  echo "→ regenerated ${CRATES[*]}" >&2
fi
