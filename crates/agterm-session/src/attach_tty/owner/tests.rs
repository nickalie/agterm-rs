use super::*;

const GRID: GridSize = GridSize { cols: 80, rows: 24 };

fn barrier(id: u64) -> Barrier {
    Barrier { id, epoch: 1 }
}

fn clipboard_read(id: u64) -> Delegated {
    Delegated {
        id,
        epoch: 1,
        request: DelegatedRequest::ClipboardRead {
            selection: "c".to_owned(),
            list_only: false,
        },
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

/// A relay in a terminal whose probe was answered with `answers` before the fence.
fn probed(nonce: Option<&str>, answers: Vec<Answer>) -> (Relay, Effects) {
    let now = Instant::now();
    let mut relay = Relay::new(nonce.map(str::to_owned), GRID, None);
    let mut fx = Effects::default();
    relay.start(true, now, &mut fx);
    assert!(relay.probing());
    for answer in answers {
        relay.answer(answer, now, &mut fx);
    }
    relay.answer(Answer::PrimaryAttributes, now, &mut fx);
    assert!(!relay.probing());
    (relay, fx)
}

fn kitty_ok() -> Answer {
    Answer::Kitty(format!("\x1b_Gi={KITTY_PROBE_ID};OK\x1b\\").into_bytes())
}

fn metadata_sent(fx: &Effects) -> Vec<&ReplyMetadata> {
    fx.outgoing
        .iter()
        .filter_map(|outgoing| match outgoing {
            Outgoing::Metadata(metadata) => Some(metadata),
            _ => None,
        })
        .collect()
}

#[test]
fn the_probe_asks_everything_once_and_its_fence_sends_the_metadata() {
    let mut palette: Vec<Answer> = (0..=255)
        .map(|index| Answer::Palette {
            index,
            color: Some(rgb(index, index, index)),
        })
        .collect();
    palette.extend([
        Answer::Dynamic {
            which: Dynamic::Foreground,
            color: Some(rgb(1, 2, 3)),
        },
        Answer::Dynamic {
            which: Dynamic::Background,
            color: Some(rgb(4, 5, 6)),
        },
        Answer::CellSize { width: 9, height: 18 },
        Answer::ColorScheme(ColorScheme::Dark),
        kitty_ok(),
    ]);
    let (relay, fx) = probed(None, palette);
    let written = String::from_utf8_lossy(&fx.stdout);
    for query in [
        "\x1b[?2031h",
        "\x1b]10;?",
        "\x1b]11;?",
        "\x1b]12;?",
        ";255;?",
        "\x1b[16t",
        "\x1b[?996n",
    ] {
        assert_eq!(written.matches(query).count(), 1, "{query:?} in {written:?}");
    }
    assert!(written.ends_with("\x1b[c"));
    let sent = metadata_sent(&fx);
    assert_eq!(sent.len(), 1);
    let metadata = sent[0];
    assert_eq!(metadata.foreground, Some(rgb(1, 2, 3)));
    assert_eq!(metadata.background, Some(rgb(4, 5, 6)));
    assert_eq!(metadata.cursor, None);
    assert_eq!(metadata.palette.len(), 256);
    assert_eq!((metadata.cell_width_px, metadata.cell_height_px), (9, 18));
    assert_eq!(metadata.color_scheme, Some(ColorScheme::Dark));
    assert!(metadata.kitty_graphics);
    assert_eq!(&relay.metadata(), metadata);
}

#[test]
fn a_client_without_a_terminal_asks_nothing_and_answers_no_fence() {
    let now = Instant::now();
    let mut relay = Relay::new(None, GRID, None);
    let mut fx = Effects::default();
    relay.start(false, now, &mut fx);
    assert!(!relay.probing());
    assert!(fx.stdout.is_empty());
    relay.barrier(barrier(1), now, &mut fx);
    relay.delegated(clipboard_read(2), now, &mut fx);
    assert!(matches!(
        fx.outgoing.as_slice(),
        [
            Outgoing::Complete(Barrier { id: 1, .. }, data),
            Outgoing::Answer(Delegated { id: 2, .. }, DelegatedReply::Denied),
        ] if data.is_empty()
    ));
    assert!(!relay.metadata().kitty_graphics);
}

#[test]
fn a_probe_unanswered_is_given_up() {
    let now = Instant::now();
    let mut relay = Relay::new(None, GRID, None);
    let mut fx = Effects::default();
    relay.start(true, now, &mut fx);
    relay.answer(kitty_ok(), now, &mut fx);
    assert_eq!(relay.next_deadline(), Some(now + PROBE_GIVE_UP));
    relay.tick(now + PROBE_GIVE_UP, &mut fx);
    assert!(!relay.probing());
    assert!(
        !relay.metadata().kitty_graphics,
        "no fences, so no Kitty answers relayed"
    );
    assert_eq!(relay.next_deadline(), None);
}

#[test]
fn a_kitty_fence_goes_after_the_commands_st_and_completes_with_its_answers() {
    let (mut relay, _) = probed(None, vec![kitty_ok()]);
    let now = Instant::now();
    let mut fx = Effects::default();
    // the output up to the command's ESC, then the barrier
    relay.output(b"\x1b_Ga=q,i=7;AAAA\x1b", &mut fx);
    relay.barrier(barrier(5), now, &mut fx);
    assert_eq!(
        fx.stdout, b"\x1b_Ga=q,i=7;AAAA",
        "nothing written before the ST is whole"
    );
    relay.output(b"\\next", &mut fx);
    assert_eq!(fx.stdout, b"\x1b_Ga=q,i=7;AAAA\x1b\\\x1b[cnext");
    assert!(relay.awaiting());
    relay.answer(Answer::Kitty(b"\x1b_Gi=7;OK\x1b\\".to_vec()), now, &mut fx);
    relay.answer(Answer::PrimaryAttributes, now, &mut fx);
    assert!(matches!(
        fx.outgoing.as_slice(),
        [Outgoing::Complete(Barrier { id: 5, .. }, data)] if data == b"\x1b_Gi=7;OK\x1b\\"
    ));
    assert!(!relay.awaiting());
}

fn replies(fx: &Effects) -> Vec<(u64, &DelegatedReply)> {
    fx.outgoing
        .iter()
        .filter_map(|outgoing| match outgoing {
            Outgoing::Answer(request, reply) => Some((request.id, reply)),
            _ => None,
        })
        .collect()
}

fn text(text: &str) -> DelegatedReply {
    DelegatedReply::Clipboard {
        text: Some(text.to_owned()),
    }
}

#[test]
fn a_clipboard_read_waits_for_its_answer_whatever_the_fences_say() {
    let (mut relay, _) = probed(None, Vec::new());
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.delegated(clipboard_read(1), now, &mut fx);
    assert_eq!(fx.stdout, b"\x1b]52;c;?\x1b\\", "the read alone, no fence");
    assert!(relay.awaiting());
    assert_eq!(relay.next_deadline(), Some(now + DELEGATION_WAIT));
    // a terminal asking its user first answers DA1 (here another fence's) before the read
    relay.answer(Answer::PrimaryAttributes, now, &mut fx);
    assert!(fx.outgoing.is_empty());
    relay.answer(Answer::Clipboard(Some(b"hello".to_vec())), now, &mut fx);
    relay.delegated(clipboard_read(2), now, &mut fx);
    relay.answer(Answer::Clipboard(Some(Vec::new())), now, &mut fx);
    relay.delegated(clipboard_read(3), now, &mut fx);
    relay.answer(Answer::Clipboard(None), now, &mut fx);
    assert_eq!(
        replies(&fx),
        [
            (1, &text("hello")),
            (2, &DelegatedReply::Clipboard { text: None }),
            (3, &DelegatedReply::Denied),
        ]
    );
    assert!(!relay.awaiting());
    assert_eq!(relay.next_deadline(), None);
}

#[test]
fn a_clipboard_answer_nobody_asked_for_is_dropped() {
    let (mut relay, _) = probed(None, Vec::new());
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.answer(Answer::Clipboard(Some(b"stale".to_vec())), now, &mut fx);
    relay.delegated(clipboard_read(1), now, &mut fx);
    relay.tick(now + DELEGATION_WAIT, &mut fx);
    assert_eq!(replies(&fx), [(1, &DelegatedReply::Denied)]);
}

#[test]
fn a_read_whose_time_ran_out_stays_unresolved_until_its_answer_comes() {
    let (mut relay, _) = probed(None, Vec::new());
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.delegated(clipboard_read(1), now, &mut fx);
    relay.tick(now + DELEGATION_WAIT.saturating_sub(Duration::from_millis(1)), &mut fx);
    assert!(fx.outgoing.is_empty());
    relay.tick(now + DELEGATION_WAIT, &mut fx);
    assert_eq!(replies(&fx), [(1, &DelegatedReply::Denied)]);
    assert!(!relay.awaiting());
    assert_eq!(relay.next_deadline(), None);
    // asked meanwhile: denied at once, the outer terminal not asked
    let mut fx = Effects::default();
    relay.delegated(clipboard_read(2), now, &mut fx);
    assert!(fx.stdout.is_empty());
    assert_eq!(replies(&fx), [(2, &DelegatedReply::Denied)]);
    // the late answer is the first read's, the user's consent to it: discarded
    let mut fx = Effects::default();
    relay.answer(Answer::Clipboard(Some(b"first".to_vec())), now, &mut fx);
    assert!(fx.outgoing.is_empty());
    relay.delegated(clipboard_read(3), now, &mut fx);
    assert_eq!(fx.stdout, b"\x1b]52;c;?\x1b\\");
    relay.answer(Answer::Clipboard(Some(b"third".to_vec())), now, &mut fx);
    assert_eq!(replies(&fx), [(3, &text("third"))]);
}

#[test]
fn a_read_never_written_is_dropped_with_its_query() {
    let (mut relay, _) = probed(None, Vec::new());
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.output(b"\x1b]0;unfinished", &mut fx);
    relay.delegated(clipboard_read(1), now, &mut fx);
    relay.tick(now + DELEGATION_WAIT, &mut fx);
    assert_eq!(replies(&fx), [(1, &DelegatedReply::Denied)]);
    assert_eq!(relay.next_deadline(), None, "nothing waits for ground any more");
    relay.output(b"\x1b\\", &mut fx);
    assert_eq!(fx.stdout, b"\x1b]0;unfinished\x1b\\");
    // nothing unresolved: the next read is asked
    let mut fx = Effects::default();
    relay.delegated(clipboard_read(2), now, &mut fx);
    assert_eq!(fx.stdout, b"\x1b]52;c;?\x1b\\");
}

#[test]
fn a_role_title_waits_for_ground_and_the_newest_wins() {
    let mut relay = Relay::new(Some("n1".to_owned()), GRID, None);
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.start(false, now, &mut fx);
    relay.output(b"\x1b]0;unfinished", &mut fx);
    relay.role(LeadRole::Unowned, 5, now, &mut fx);
    relay.role(LeadRole::Follower, 6, now, &mut fx);
    relay.output(b"\x1b\\", &mut fx);
    let written = String::from_utf8_lossy(&fx.stdout);
    assert!(!written.contains("unowned"));
    assert!(written.ends_with("\x1b]0;unfinished\x1b\\\x1b]2;ngterm-live-role;n1:follower:6\x1b\\"));
}

#[test]
fn an_unfinished_sequence_is_cancelled_at_the_deadline() {
    let mut relay = Relay::new(Some("n1".to_owned()), GRID, None);
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.start(false, now, &mut fx);
    relay.output(b"\x1bPnever finished", &mut fx);
    relay.role(LeadRole::Leader, 7, now, &mut fx);
    assert_eq!(relay.next_deadline(), Some(now + INJECT_WAIT));
    relay.tick(now + INJECT_WAIT / 2, &mut fx);
    assert!(!String::from_utf8_lossy(&fx.stdout).contains("live-role"));
    relay.tick(now + INJECT_WAIT, &mut fx);
    assert_eq!(
        fx.stdout,
        b"\x1bPnever finished\x18\x1b]2;ngterm-live-role;n1:leader:7\x1b\\"
    );
    assert_eq!(relay.next_deadline(), None);
}

#[test]
fn an_unmanaged_client_writes_no_role() {
    let mut relay = Relay::new(None, GRID, None);
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.start(false, now, &mut fx);
    relay.role(LeadRole::Leader, 1, now, &mut fx);
    assert!(fx.stdout.is_empty());
}

#[test]
fn an_appearance_change_asks_the_colours_again() {
    let (mut relay, _) = probed(
        None,
        vec![Answer::Dynamic {
            which: Dynamic::Background,
            color: Some(rgb(0, 0, 0)),
        }],
    );
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.answer(Answer::ColorScheme(ColorScheme::Light), now, &mut fx);
    assert!(String::from_utf8_lossy(&fx.stdout).contains("\x1b]11;?"));
    assert!(fx.outgoing.is_empty(), "sent once the refresh's fence comes");
    relay.answer(
        Answer::Dynamic {
            which: Dynamic::Background,
            color: Some(rgb(0xff, 0xff, 0xff)),
        },
        now,
        &mut fx,
    );
    relay.answer(Answer::PrimaryAttributes, now, &mut fx);
    let sent = metadata_sent(&fx);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].background, Some(rgb(0xff, 0xff, 0xff)));
    assert_eq!(sent[0].color_scheme, Some(ColorScheme::Light));
}

#[test]
fn the_cell_size_follows_the_window_and_is_sent_when_it_changes() {
    let (mut relay, _) = probed(None, vec![Answer::CellSize { width: 8, height: 16 }]);
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.resize(GridSize { cols: 100, rows: 50 }, Some((1000, 1000)), now, &mut fx);
    relay.resize(GridSize { cols: 100, rows: 50 }, Some((1000, 1000)), now, &mut fx);
    let sent = metadata_sent(&fx);
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0].cell_width_px, sent[0].cell_height_px), (10, 20));
    // without the window's pixels the cell is asked for again
    let mut fx = Effects::default();
    relay.resize(GridSize { cols: 50, rows: 25 }, None, now, &mut fx);
    assert_eq!(fx.stdout, b"\x1b[16t");
    relay.answer(Answer::CellSize { width: 20, height: 40 }, now, &mut fx);
    assert_eq!(
        metadata_sent(&fx)
            .last()
            .map(|metadata| (metadata.cell_width_px, metadata.cell_height_px)),
        Some((20, 40))
    );
}

#[test]
fn fences_and_reads_of_an_ended_connection_complete_nothing() {
    let (mut relay, _) = probed(None, vec![kitty_ok()]);
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.barrier(barrier(1), now, &mut fx);
    relay.delegated(clipboard_read(2), now, &mut fx);
    relay.reattached();
    relay.answer(Answer::Kitty(b"\x1b_Gi=1;OK\x1b\\".to_vec()), now, &mut fx);
    relay.answer(Answer::PrimaryAttributes, now, &mut fx);
    assert!(!relay.awaiting());
    // the ended connection's read is still asked of the outer terminal: a read meanwhile is denied, and its answer
    // is discarded
    relay.delegated(clipboard_read(3), now, &mut fx);
    relay.answer(Answer::Clipboard(Some(b"old".to_vec())), now, &mut fx);
    relay.delegated(clipboard_read(4), now, &mut fx);
    relay.answer(Answer::Clipboard(Some(b"new".to_vec())), now, &mut fx);
    assert_eq!(replies(&fx), [(3, &DelegatedReply::Denied), (4, &text("new"))]);
}

#[test]
fn a_reset_on_its_way_turns_the_scheme_reports_on_again() {
    let (mut relay, _) = probed(None, Vec::new());
    let mut fx = Effects::default();
    relay.output(b"x\x1bcy", &mut fx);
    assert_eq!(fx.stdout, b"x\x1bc\x1b[?2031hy");
    let mut fx = Effects::default();
    relay.output(crate::server::snapshot::RESYNC_RESET, &mut fx);
    assert!(fx.stdout.starts_with(b"\x18\x1bc\x1b[?2031h"));
    // a client without a terminal subscribed to nothing
    let mut relay = Relay::new(None, GRID, None);
    let mut fx = Effects::default();
    relay.start(false, Instant::now(), &mut fx);
    relay.output(b"\x1bc", &mut fx);
    assert_eq!(fx.stdout, b"\x1bc");
}

#[test]
fn the_window_pixels_give_the_cell_from_the_start() {
    let mut relay = Relay::new(None, GridSize { cols: 100, rows: 50 }, Some((1000, 1000)));
    let now = Instant::now();
    let mut fx = Effects::default();
    relay.start(true, now, &mut fx);
    let metadata = relay.attach_metadata();
    assert_eq!((metadata.cell_width_px, metadata.cell_height_px), (10, 20));
    relay.answer(Answer::CellSize { width: 7, height: 7 }, now, &mut fx);
    relay.answer(Answer::PrimaryAttributes, now, &mut fx);
    assert_eq!(
        (relay.metadata().cell_width_px, relay.metadata().cell_height_px),
        (10, 20),
        "the window's pixels come first"
    );
}
