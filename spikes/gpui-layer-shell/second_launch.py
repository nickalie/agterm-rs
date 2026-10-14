"""A second launch of the spike's app id, the way a GApplication (or GtkApplication) non-primary does it: register,
find the name owned, call `org.gtk.Actions.DescribeAll`, then `org.gtk.Application.Activate` with the platform data
it builds (where the activation token from `XDG_ACTIVATION_TOKEN` travels). Writes what it saw to OUT as JSON.

usage: python3 -I second_launch.py APP_ID OUT [--gtk]
"""

import json
import os
import sys

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio  # noqa: E402


def main() -> None:
    app_id, out = sys.argv[1], sys.argv[2]
    gtk = "--gtk" in sys.argv[3:]
    seen = {
        "kind": "GtkApplication" if gtk else "GApplication",
        "env_token": os.environ.get("XDG_ACTIVATION_TOKEN"),
        "env_startup_id": os.environ.get("DESKTOP_STARTUP_ID"),
    }
    if gtk:
        gi.require_version("Gtk", "4.0")
        from gi.repository import Gtk

        app = Gtk.Application(application_id=app_id, flags=Gio.ApplicationFlags.DEFAULT_FLAGS)
    else:
        app = Gio.Application(application_id=app_id, flags=Gio.ApplicationFlags.DEFAULT_FLAGS)
    try:
        seen["registered"] = app.register(None)
        seen["remote"] = app.get_is_remote()
        if seen["remote"]:
            app.activate()
            app.get_dbus_connection().flush_sync(None)
    except Exception as error:  # noqa: BLE001 - recorded, the drive script judges
        seen["error"] = str(error)
    with open(out, "w", encoding="utf-8") as handle:
        json.dump(seen, handle)


main()
