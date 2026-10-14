---
worth: yes
where: crates/agterm-gtk/src/app.rs:256
added: 2026-10-14
---
# SIGTERM quit can stall past 10 s after the control server stops

Under load (3–5 parallel cargo builds + conformance suites), test_session.py's shared app logged `SIGTERM: quitting`
and was SIGKILLed 10 s later (StopError at module teardown, assertion green; seen once). rust-build.md:39 requires
quitting within 10 s. Kept root /tmp/agt-run-2rNc3b/agt-ma2y5efc: windows.json written 9–16 ms after the signal and the
socket unlinked, so save_on_quit and server.stop() finished; the stall is in the Shell drop, surface::teardown_all()
(libghostty surface free: renderer/IO thread joins, child reap), app.quit()/window destruction, or main.rs's
post-run app/ghostty shutdown. Suspect, unproven: termio's child kill/wait. Next occurrence: the shutdown phase logs
and the harness's thread snapshot + core (coredumpctl) name the step; reproduce with the full test_session.py module
under CPU load. Do not raise STOP_TIMEOUT or skip the save.
