#!/usr/bin/env bash
# Plays FILE with plain GStreamer (no WebKit) for SECONDS and prints the sink's rendered/dropped counts:
#   gst-check.sh FILE SECONDS [DECODER]   (DECODER e.g. avdec_vp9, vavp9dec, nvvp9dec; default: decodebin's pick)
set -uo pipefail
file="$1"; seconds="$2"; decoder="${3:-}"
if [[ -n "$decoder" ]]; then
  pipeline=(filesrc "location=$file" ! matroskademux ! vp9parse ! "$decoder" ! videoconvert !
            fpsdisplaysink text-overlay=false video-sink=fakesink sync=true)
else
  pipeline=(filesrc "location=$file" ! decodebin ! videoconvert ! fpsdisplaysink text-overlay=false
            video-sink=fakesink sync=true)
fi
timeout -s INT "$seconds" gst-launch-1.0 -v "${pipeline[@]}" 2>&1 | grep -oE 'rendered: [0-9]+, dropped: [0-9]+, current: [0-9.]+, average: [0-9.]+' | tail -2
