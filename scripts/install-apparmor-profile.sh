#!/usr/bin/env bash
# Install (or remove) the AppArmor profile that lets WebKit's bubblewrap sandbox run for agterm's HTML overlays on
# Ubuntu 23.10+ and other hosts that restrict unprivileged user namespaces through AppArmor
# (kernel.apparmor_restrict_unprivileged_userns=1). The profile file (packaging/linux/apparmor/agterm) is Ubuntu's own
# shape for its WebKitGTK apps: unconfined, plus `userns`, attached to a binary by its absolute path. It holds two
# profiles while both frontends coexist: `agterm` for the GTK build's binary (WebKitGTK) and `agterm-gpui` for the GPUI
# build's (WPE WebKit starts the same sandbox); after the switch the GPUI build is `agterm`, which the first one covers.
# The sandbox stays on; without the profile agterm refuses every HTML page there.
#
#   scripts/install-apparmor-profile.sh [--gpui GPUI_BINARY] [BINARY]          install /etc/apparmor.d/agterm, load it
#   scripts/install-apparmor-profile.sh --uninstall                             unload and remove it
#   scripts/install-apparmor-profile.sh --print [--gpui GPUI_BINARY] [BINARY]  print the profiles, change nothing
#
# BINARY is the GTK build's agterm executable that is launched and GPUI_BINARY the GPUI build's, symlinks resolved
# (AppArmor attaches by the real path); defaults: the conformance stage's bin/agterm and bin/agterm-gpui under
# ${CARGO_TARGET_DIR:-target}/conformance-stage. Neither need exist yet. Where AppArmor is off or does not restrict user
# namespaces, nothing is installed. Root is gained through sudo; running it again with the same binaries changes
# nothing.
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

TEMPLATE="$NATIVE_ROOT/packaging/linux/apparmor/agterm"
PROFILE_NAME=agterm
PROFILE_FILE="/etc/apparmor.d/$PROFILE_NAME"
# the profiles the file defines, one per frontend's binary
PROFILES=(agterm agterm-gpui)
APPARMOR_ENABLED=/sys/module/apparmor/parameters/enabled
USERNS_RESTRICTION=/proc/sys/kernel/apparmor_restrict_unprivileged_userns
LOADED=/sys/kernel/security/apparmor/profiles

usage() {
  die "usage: scripts/install-apparmor-profile.sh [--print] [--gpui GPUI_BINARY] [BINARY] | --uninstall (see --help)"
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

# The profiles for the GTK build's binary $1 and the GPUI build's $2 (absolute, symlinks resolved).
render() {
  local binary="$1" gpui="$2" template
  check_path "$binary"
  check_path "$gpui"
  template="$(<"$TEMPLATE")"
  template="${template//@AGTERM_BIN@/"$binary"}"
  printf '%s\n' "${template//@AGTERM_GPUI_BIN@/"$gpui"}"
}

install_profile() {
  local binary="$1" gpui="$2" profile
  profile="$(render "$binary" "$gpui")"
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
    echo "$PROFILE_FILE is installed and loaded for $binary and $gpui"
    return
  fi
  printf '%s\n' "$profile" | as_root tee "$PROFILE_FILE" >/dev/null
  as_root apparmor_parser -r "$PROFILE_FILE"
  loaded || die "apparmor_parser loaded $PROFILE_FILE, but not every one of ${PROFILES[*]} is listed in $LOADED"
  echo "installed and loaded $PROFILE_FILE for $binary and $gpui"
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
gpui=""
while (($#)); do
  case "$1" in
    --uninstall) mode=uninstall; shift ;;
    --print) mode=print; shift ;;
    --gpui)
      (($# >= 2)) && [[ -z "$gpui" && -n "$2" ]] || usage
      gpui="$2"
      shift 2
      ;;
    -h | --help) sed -n '2,18s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
    -*) usage ;;
    *) [[ -z "$binary" ]] || usage; binary="$1"; shift ;;
  esac
done

if [[ "$mode" == uninstall ]]; then
  [[ -z "$binary" && -z "$gpui" ]] || usage
  uninstall_profile
  exit 0
fi
target="${CARGO_TARGET_DIR:-$NATIVE_ROOT/target}"
[[ "$target" == /* ]] || target="$NATIVE_ROOT/$target"
binary="$(realpath -m -- "${binary:-$target/conformance-stage/bin/agterm}")"
gpui="$(realpath -m -- "${gpui:-$target/conformance-stage/bin/agterm-gpui}")"
if [[ "$mode" == print ]]; then
  render "$binary" "$gpui"
else
  install_profile "$binary" "$gpui"
fi
