//! The row glyphs' coverage, ported from the GTK build's `row_icon.rs` tests (the same design points probed at the same
//! scales), plus the controls GTK took from its icon theme.

use super::*;

/// A rendered glyph's coverage, `side` x `side`.
struct Alpha {
    side: usize,
    alpha: Vec<u8>,
}

impl Alpha {
    fn at(&self, x: usize, y: usize) -> u8 {
        self.alpha[y * self.side + x]
    }

    /// The pixels at least half opaque.
    fn opaque(&self) -> usize {
        self.alpha.iter().filter(|&&alpha| alpha >= 128).count()
    }

    /// The total coverage.
    fn ink(&self) -> u64 {
        self.alpha.iter().map(|&alpha| u64::from(alpha)).sum()
    }
}

fn render(glyph: Glyph, side: u32) -> Alpha {
    Alpha {
        side: usize::try_from(side).unwrap(),
        alpha: rasterize(glyph, side),
    }
}

fn every_glyph() -> Vec<Glyph> {
    let mut glyphs = vec![Glyph::Plus, Glyph::NewWorkspace, Glyph::AddSession];
    for on in [false, true] {
        glyphs.push(Glyph::Workspace { focus_member: on });
        glyphs.push(Glyph::Flag { filled: on });
        glyphs.push(Glyph::Grid { filled: on });
        glyphs.push(Glyph::Disclosure { expanded: on });
        for split in [None, Some(SplitAxis::LeftRight), Some(SplitAxis::TopBottom)] {
            glyphs.push(Glyph::Session { split, flagged: on });
        }
        for disconnected in [false, true] {
            glyphs.push(Glyph::Remote {
                split: on,
                disconnected,
            });
        }
    }
    glyphs
}

#[test]
fn every_glyph_renders_ink_and_differs_from_every_other() {
    // the footer's unfilled grid is the workspace row's symbol; every other pair is a distinct symbol
    let glyphs: Vec<Glyph> = every_glyph()
        .into_iter()
        .filter(|&glyph| glyph != Glyph::Grid { filled: false })
        .collect();
    let rendered: Vec<Alpha> = glyphs.iter().map(|&glyph| render(glyph, 16)).collect();
    for (glyph, alpha) in glyphs.iter().zip(&rendered) {
        assert!(alpha.opaque() > 10, "{glyph:?} renders some ink");
        assert_eq!(alpha.alpha.len(), 256, "{glyph:?} fills the 16x16 slot");
    }
    for (i, first) in rendered.iter().enumerate() {
        for (j, second) in rendered.iter().enumerate().skip(i + 1) {
            assert_ne!(
                first.alpha, second.alpha,
                "{:?} and {:?} look alike",
                glyphs[i], glyphs[j]
            );
        }
    }
}

#[test]
fn nothing_draws_outside_the_slot() {
    // the slot is pixels 4..20 of a 24-pixel square
    let slot = 4..20;
    for glyph in every_glyph() {
        let mut ink = Ink::framed(16, 4, BOX);
        trace(&mut ink, glyph);
        let framed = Alpha {
            side: 24,
            alpha: ink.coverage(),
        };
        let outside = (0..24 * 24)
            .map(|index| (index % 24, index / 24))
            .filter(|(x, y)| !slot.contains(x) || !slot.contains(y));
        for (x, y) in outside {
            assert_eq!(framed.at(x, y), 0, "{glyph:?} inks ({x}, {y}) outside the slot");
        }
    }
}

#[test]
fn a_flagged_session_fills_its_icon() {
    for split in [None, Some(SplitAxis::LeftRight), Some(SplitAxis::TopBottom)] {
        let plain = render(Glyph::Session { split, flagged: false }, 16);
        let flagged = render(Glyph::Session { split, flagged: true }, 16);
        assert!(
            flagged.opaque() > plain.opaque() * 3 / 2,
            "{split:?}: the flagged icon is filled"
        );
    }
}

#[test]
fn the_filled_terminal_knocks_its_prompt_out() {
    // 4x scale: the pixel at (26, 30) covers design point (6.625, 7.625), on the chevron's upper stroke
    let filled = render(
        Glyph::Session {
            split: None,
            flagged: true,
        },
        64,
    );
    let outline = render(
        Glyph::Session {
            split: None,
            flagged: false,
        },
        64,
    );
    assert_eq!(filled.at(26, 30), 0, "the chevron is see-through in terminal.fill");
    assert_eq!(outline.at(26, 30), 255, "the chevron is inked in terminal");
    // design point (12.5, 5.5): solid frame in the fill, empty inside the outline
    assert_eq!(filled.at(50, 22), 255);
    assert_eq!(outline.at(50, 22), 0);
}

#[test]
fn a_filled_split_keeps_a_transparent_gap_on_its_divider() {
    // 4x scale: design point (8.125, 4.625) sits on the left/right divider, (4.625, 8.125) on the top/bottom one
    let cases = [(SplitAxis::LeftRight, (32, 18)), (SplitAxis::TopBottom, (18, 32))];
    for (axis, (x, y)) in cases {
        let filled = render(
            Glyph::Session {
                split: Some(axis),
                flagged: true,
            },
            64,
        );
        let outline = render(
            Glyph::Session {
                split: Some(axis),
                flagged: false,
            },
            64,
        );
        assert_eq!(filled.at(x, y), 0, "{axis:?}: the halves stand apart");
        assert_eq!(outline.at(x, y), 255, "{axis:?}: the outline draws the divider");
        assert!(
            filled.at(y, x) == 255 && outline.at(y, x) == 0,
            "{axis:?}: no divider across"
        );
    }
}

#[test]
fn a_focus_member_workspace_is_heavier_but_not_filled() {
    let plain = render(Glyph::Workspace { focus_member: false }, 64);
    let member = render(Glyph::Workspace { focus_member: true }, 64);
    assert!(member.ink() > plain.ink() * 3 / 2, "the focus member's grid is heavier");
    // the center of the top-left square (design (4.125, 4.125)) stays hollow, unlike the filled footer grid
    assert_eq!(member.at(16, 16), 0);
    assert_eq!(render(Glyph::Grid { filled: true }, 64).at(16, 16), 255);
}

#[test]
fn the_footer_toggles_fill_when_on() {
    for (off, on) in [
        (Glyph::Flag { filled: false }, Glyph::Flag { filled: true }),
        (Glyph::Grid { filled: false }, Glyph::Grid { filled: true }),
    ] {
        assert!(
            render(on, 16).opaque() > render(off, 16).opaque() * 6 / 5,
            "{on:?} is filled"
        );
    }
    // 4x scale: design point (8.125, 6.125) is inside the banner, solid only in flag.fill
    assert_eq!(render(Glyph::Flag { filled: true }, 64).at(32, 24), 255);
    assert_eq!(render(Glyph::Flag { filled: false }, 64).at(32, 24), 0);
    assert_eq!(
        render(Glyph::Grid { filled: false }, 16).alpha,
        render(Glyph::Workspace { focus_member: false }, 16).alpha,
        "the footer's grid is the workspace row's"
    );
}

#[test]
fn a_remote_split_is_bolder_and_a_disconnected_cloud_is_slashed() {
    for disconnected in [false, true] {
        let single = render(
            Glyph::Remote {
                split: false,
                disconnected,
            },
            64,
        );
        let split = render(
            Glyph::Remote {
                split: true,
                disconnected,
            },
            64,
        );
        assert!(split.ink() > single.ink() * 5 / 4, "a split cloud is bolder");
    }
    // design point (3.125, 2.625): on the slash, clear of the cloud
    let connected = render(
        Glyph::Remote {
            split: false,
            disconnected: false,
        },
        64,
    );
    let slashed = render(
        Glyph::Remote {
            split: false,
            disconnected: true,
        },
        64,
    );
    assert_eq!(connected.at(12, 10), 0);
    assert_eq!(slashed.at(12, 10), 255);
    // the cloud's inside stays empty either way (design (8.125, 9.625))
    assert_eq!(connected.at(32, 38), 0);
}

#[test]
fn the_disclosure_points_right_while_folded_and_down_while_open() {
    // 4x scale: design (6.25, 4.5) starts the folded chevron's upper arm, (4.5, 6.25) the open one's left arm
    let folded = render(Glyph::Disclosure { expanded: false }, 64);
    let open = render(Glyph::Disclosure { expanded: true }, 64);
    assert_eq!(folded.at(25, 18), 255);
    assert_eq!(open.at(25, 18), 0);
    assert_eq!(open.at(18, 25), 255);
    assert_eq!(folded.at(18, 25), 0);
}

#[test]
fn the_new_workspace_badge_knocks_its_plus_out() {
    // 4x scale: design (12.0, 4.0) is the badge's center, under its knocked-out plus; (14.75, 6.0) solid badge
    let alpha = render(Glyph::NewWorkspace, 64);
    assert_eq!(alpha.at(48, 16), 0, "the plus is see-through");
    assert_eq!(alpha.at(59, 24), 255, "the badge is solid around it");
    // the plus.rectangle's center is inked
    assert_eq!(render(Glyph::AddSession, 64).at(32, 32), 255);
}

#[test]
fn the_slot_scales_with_the_display() {
    assert_eq!(device_side(1.0), 16);
    assert_eq!(device_side(1.5), 24);
    assert_eq!(device_side(2.0), 32);
    assert_eq!(rasterize(Glyph::Plus, 24).len(), 24 * 24);
}
