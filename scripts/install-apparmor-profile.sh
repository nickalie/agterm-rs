#!/usr/bin/env bash
# Install (or remove) the AppArmor profile that lets WebKit's bubblewrap sandbox run for ngterm's HTML overlays on
# Ubuntu 23.10+ and other hosts that restrict unprivileged user namespaces through AppArmor
# (kernel.apparmor_restrict_unprivileged_userns=1). The profile file (packaging/linux/apparmor/ngterm) is Ubuntu's own
# shape for its WebKit apps: unconfined, plus `userns`, attached to a binary by its absolute path. WPE WebKit starts
# its web process in that sandbox; it stays on, and without the profile ngterm refuses every HTML page there.
#
#   scripts/install-apparmor-profile.sh [BINARY]          install /etc/apparmor.d/ngterm and load it
#   scripts/install-apparmor-profile.sh --uninstall       unload and remove it
#   scripts/install-apparmor-profile.sh --print [BINARY]  print the profile, change nothing
#
# BINARY is the ngterm executable that is launched, symlinks resolved (AppArmor attaches by the real path); default:
# the conformance stage's, bin/ngterm under ${CARGO_TARGET_DIR:-target}/conformance-stage
# (conformance/tools/run-linux.sh). It need not exist yet. Where AppArmor is off or does not restrict user
# namespaces, nothing is installed. Root is gained through sudo; running it again with the same binary changes
# nothing.
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

TEMPLATE="$NATIVE_ROOT/packaging/linux/apparmor/ngterm"
PROFILE_NAME=ngterm
PROFILE_FILE="/etc/apparmor.d/$PROFILE_NAME"
# the profiles the file defines
PROFILES=(ngterm)
APPARMOR_ENABLED=/sys/module/apparmor/parameters/enabled
USERNS_RESTRICTION=/proc/sys/kernel/apparmor_restrict_unprivileged_userns
LOADED=/sys/kernel/security/apparmor/profiles

usage() {
  die "usage: scripts/install-apparmor-profile.sh [--print] [BINARY] | --uninstall (see --help)"
}

as_root() {
  if ((EUID == 0)); then "$@"; else sudo "$@"; fi
}

apparmor_enabled() {
  [[ "$(cat "$APPARMOR_ENABLED" 2>/dev/null || true)" == Y ]]
}

userns_restricted() {
  [[ "$(cat "$USERNS_RESTRICTION" 2>/dev/null || echo 0)" != 0 ]]
}

# How many of the file's profiles are loaded.
loaded_count() {
  local listed name count=0
  listed="$(as_root cat "$LOADED")"
  for name in "${PROFILES[@]}"; do
    if grep -qxF "$name (unconfined)" <<<"$listed"; then
      count=$((count + 1))
    fi
  done
  echo "$count"
}

loaded() {
  (($(loaded_count) == ${#PROFILES[@]}))
}

# Quoted in the profile: a space is fine, AppArmor's pattern characters and a quote are not.
check_path() {
  [[ "$1" != *[\"\\*?\[\]\{\}^]* ]] || die "the path has a character AppArmor reads as a pattern: $1"
}

# The profile for the binary $1 (absolute, symlinks resolved).
render() {
  local binary="$1" template
  check_path "$binary"
  template="$(<"$TEMPLATE")"
  printf '%s\n' "${template//@NGTERM_BIN@/"$binary"}"
}

install_profile() {
  local binary="$1" profile
  profile="$(render "$binary")"
  if ! apparmor_enabled; then
    echo "AppArmor is not enabled on this host: WebKit's sandbox needs no profile, nothing installed"
    return
  fi
  if ! userns_restricted; then
    echo "AppArmor does not restrict unprivileged user namespaces here: nothing installed"
    return
  fi
  command -v apparmor_parser >/dev/null || die "no apparmor_parser on PATH (the apparmor package)"
  if [[ -f "$PROFILE_FILE" && "$(cat "$PROFILE_FILE")" == "$profile" ]] && loaded; then
    echo "$PROFILE_FILE is installed and loaded for $binary"
    return
  fi
  printf '%s\n' "$profile" | as_root tee "$PROFILE_FILE" >/dev/null
  as_root apparmor_parser -r "$PROFILE_FILE"
  loaded || die "apparmor_parser loaded $PROFILE_FILE, but not every one of ${PROFILES[*]} is listed in $LOADED"
  echo "installed and loaded $PROFILE_FILE for $binary"
}

uninstall_profile() {
  if [[ ! -e "$PROFILE_FILE" ]]; then
    echo "no $PROFILE_FILE: nothing to remove"
    return
  fi
  if apparmor_enabled && (($(loaded_count) > 0)); then
    as_root apparmor_parser -R "$PROFILE_FILE"
  fi
  as_root rm -f "$PROFILE_FILE"
  echo "removed $PROFILE_FILE"
}

mode=install
binary=""
while (($#)); do
  case "$1" in
    --uninstall) mode=uninstall; shift ;;
    --print) mode=print; shift ;;
    -h | --help) sed -n '2,16s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
    -*) usage ;;
    *) [[ -z "$binary" ]] || usage; binary="$1"; shift ;;
  esac
done

if [[ "$mode" == uninstall ]]; then
  [[ -z "$binary" ]] || usage
  uninstall_profile
  exit 0
fi
target="${CARGO_TARGET_DIR:-$NATIVE_ROOT/target}"
[[ "$target" == /* ]] || target="$NATIVE_ROOT/$target"
binary="$(realpath -m -- "${binary:-$target/conformance-stage/bin/ngterm}")"
if [[ "$mode" == print ]]; then
  render "$binary"
else
  install_profile "$binary"
fi
