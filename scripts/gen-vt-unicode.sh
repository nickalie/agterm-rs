#!/usr/bin/env bash
# Regenerate agterm-vt's checked-in Unicode tables (crates/agterm-vt/src/unicode/generated/*.rs) from the UCD that the
# pinned uucode ships (UUCODE_* in build/pins.env), with agterm-vt's `gen-unicode` binary (feature `gen`): its port of
# uucode's UCD parsing, derived fields and grapheme-break rules and of Ghostty's table generators and uucode config at
# LIBGHOSTTY_VT_REV. The uucode source archive is verified against its sha256 and unpacked once into the vendored
# prefix ${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/uucode-<key>/ (the key hashes the UUCODE_ pins only), and the UCD
# files the generator reads against UUCODE_UCD_SHA256. Run after a UUCODE_* bump, or after porting a change of
# Ghostty's src/unicode or src/build/uucode_config.zig into the generator, then review the diff and update the checksum
# tests in crates/agterm-vt/src/unicode/generated/mod.rs.
#
# Usage: scripts/gen-vt-unicode.sh                 write the tables (only the files that change)
#        scripts/gen-vt-unicode.sh --check         write nothing; exit 1 when a table is stale (CI, on every OS)
#        scripts/gen-vt-unicode.sh --print-prefix  print the uucode prefix path only
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

# The UCD files gen-unicode reads, below uucode's ucd/, in the order UUCODE_UCD_SHA256 lists them.
UCD_FILES=(UnicodeData.txt CaseFolding.txt DerivedCoreProperties.txt extracted/DerivedEastAsianWidth.txt
  auxiliary/GraphemeBreakProperty.txt auxiliary/GraphemeBreakTest.txt emoji/emoji-data.txt
  emoji/emoji-variation-sequences.txt Blocks.txt)

PREFIX="$VENDOR_ROOT/uucode-$(prefix_key UUCODE /nonexistent)"

# The sha256 of the "<sha256>  <path>" lines of the UCD files under $1/ucd, one per file in UCD_FILES order.
ucd_sha256() {
  local file
  for file in "${UCD_FILES[@]}"; do
    [[ -s "$1/ucd/$file" ]] || return 1
    printf '%s  %s\n' "$(sha256_of <"$1/ucd/$file")" "$file"
  done | sha256_of
}

# A uucode prefix is complete when its pin manifest matches the UUCODE_ pins and its UCD files their sha256.
uucode_prefix_is_complete() {
  [[ -s "$1/build.zig.zon" && -s "$1/agterm-PINS" ]] && pin_lines UUCODE | cmp -s - "$1/agterm-PINS" \
    && [[ "$(ucd_sha256 "$1")" == "$UUCODE_UCD_SHA256" ]]
}

CHECK=()
case "${1:-}" in
  --print-prefix)
    echo "$PREFIX"
    exit 0
    ;;
  --check) CHECK=(--check) ;;
  "") ;;
  *) die "usage: scripts/gen-vt-unicode.sh [--check | --print-prefix]" ;;
esac

if ! uucode_prefix_is_complete "$PREFIX"; then
  for command in curl tar; do
    command -v "$command" >/dev/null || die "$command is required to fetch uucode"
  done
  mkdir -p "$VENDOR_ROOT"
  # both scratch dirs live in the vendor root, beside the final prefix, not in a shared /tmp; the stage becomes the
  # prefix in one atomic rename
  WORK="$(mktemp -d "$VENDOR_ROOT/.uucode-fetch.XXXXXX")"
  STAGE="$(mktemp -d "$VENDOR_ROOT/.uucode-stage.XXXXXX")"
  trap 'rm -rf "$WORK" "$STAGE"' EXIT
  echo "fetching uucode $UUCODE_REV..." >&2
  curl -fsSL -A agterm-rs-dev -o "$WORK/uucode.tar.gz" "$UUCODE_REPO/archive/$UUCODE_REV.tar.gz"
  [[ "$(sha256_of <"$WORK/uucode.tar.gz")" == "$UUCODE_SHA256" ]] || die "uucode $UUCODE_REV: archive sha256 mismatch"
  tar -xzf "$WORK/uucode.tar.gz" -C "$STAGE" --strip-components=1
  [[ "$(ucd_sha256 "$STAGE")" == "$UUCODE_UCD_SHA256" ]] || die "uucode $UUCODE_REV: UCD files sha256 mismatch"
  pin_lines UUCODE >"$STAGE/agterm-PINS"
  rm -rf "$PREFIX"
  mv "$STAGE" "$PREFIX"
  echo "uucode vendored at $PREFIX" >&2
fi

UUCODE_DIR="$PREFIX"
# the generator is a native Windows program there: hand it a Windows path, not Git Bash's
if command -v cygpath >/dev/null; then
  UUCODE_DIR="$(cygpath -m "$PREFIX")"
fi
cd "$NATIVE_ROOT"
cargo run --quiet --locked --release -p agterm-vt --features gen --bin gen-unicode -- \
  --uucode "$UUCODE_DIR" --uucode-rev "$UUCODE_REV" --ucd-version "$UUCODE_UCD_VERSION" ${CHECK[@]+"${CHECK[@]}"}
