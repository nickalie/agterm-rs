"""The first-run welcome dialog. Install is found, never pressed: it runs the installers."""

import pytest
from agterm_conformance.match import session_ids


@pytest.mark.covers("ui:welcome-install", "ui:welcome-later", "ui:welcome-skill-checkbox", "ui:welcome-hooks-checkbox")
def test_welcome_offers_both_installs_and_later_dismisses_it(adapter, launch, ui_for):
    # The dialog is modal: the socket answers only once it is dismissed.
    ctl = launch(first_run=True)
    ui = ui_for(ctl)
    later = ui.find("welcome-later", role="button", name="Later")
    ui.find("welcome-install", role="button", name="Install", enabled=True)
    ui.find("welcome-skill-checkbox", role="checkbox", value=1)
    ui.find("welcome-hooks-checkbox", role="checkbox", value=1)
    ui.click(later)
    ui.absent("welcome-later")
    adapter.serving(ctl.instance)
    (sid,) = session_ids(ctl.tree())
    ctl.wait_mounted(sid, "left")
