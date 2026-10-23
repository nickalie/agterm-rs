---
worth: later
where: crates/ngterm-gpui/tests/html_overlay/cases.rs:325
added: 2026-10-23
---
# The HTML overlay dmabuf frame-rate test fails when the machine is loaded

`an_animation_keeps_its_frame_rate_on_dmabufs_without_falling_back` measured 12.1–21.2 frames/s against its 24
frames/s floor in four runs on 2026-10-23, under the 4-core agent slice with load average 10–65 from other sessions;
the branches involved touched no GPUI or WPE code. Unlike a timing race, this is a performance floor, so it is `later`:
the open question is whether the floor should hold on a loaded machine at all (and the test skip or retry the
measurement when the CPU is contended) or only on an idle one, which decides between scaling the floor and gating it.
Don't lower the threshold silently.
