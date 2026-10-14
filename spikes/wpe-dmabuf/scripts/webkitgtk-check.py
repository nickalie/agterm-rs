#!/usr/bin/env python3
"""Opens URL in a bare WebKitGTK 6 WebView for SECONDS and prints the page's <video> state every second (isolates
agterm's overlay setup from WebKitGTK itself): webkitgtk-check.py URL SECONDS [gesture]"""
import json
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("WebKit", "6.0")
from gi.repository import GLib, Gtk, WebKit  # noqa: E402

PROBE = """(() => {
  const v = document.querySelector('video');
  if (!v) return JSON.stringify({video: false});
  const q = v.getVideoPlaybackQuality ? v.getVideoPlaybackQuality() : {};
  const p = document.getElementById('movie_player');
  if (p && p.setPlaybackQualityRange) p.setPlaybackQualityRange('hd1080', 'hd1080');
  if (v.paused) { v.muted = true; v.play(); }
  return JSON.stringify({t: v.currentTime, w: v.videoWidth, h: v.videoHeight, paused: v.paused,
    total: q.totalVideoFrames, dropped: q.droppedVideoFrames, error: v.error ? v.error.message : null,
    quality: p && p.getPlaybackQuality ? p.getPlaybackQuality() : null,
    codecs: p && p.getStatsForNerds ? p.getStatsForNerds().codecs : null,
    resolution: p && p.getStatsForNerds ? p.getStatsForNerds().resolution : null});
})()"""

url, seconds = sys.argv[1], int(sys.argv[2])
gesture = len(sys.argv) > 3 and sys.argv[3] == "gesture"
app = Gtk.Application(application_id="io.github.agtermrs.webkitgtkcheck", flags=0)


def activate(app):
    win = Gtk.ApplicationWindow(application=app, default_width=1920, default_height=1080)
    settings = WebKit.Settings()
    settings.set_media_playback_requires_user_gesture(gesture)
    view = WebKit.WebView(settings=settings, network_session=WebKit.NetworkSession.new_ephemeral())
    win.set_child(view)
    view.load_uri(url)
    win.present()

    def done(view, result):
        try:
            print(json.dumps({"sample": view.evaluate_javascript_finish(result).to_string()}), flush=True)
        except GLib.Error as error:
            print(json.dumps({"error": error.message}), flush=True)

    def probe():
        view.evaluate_javascript(PROBE, -1, None, None, None, done)
        return True

    GLib.timeout_add_seconds(1, probe)
    GLib.timeout_add_seconds(seconds, app.quit)


app.connect("activate", activate)
app.run([])
