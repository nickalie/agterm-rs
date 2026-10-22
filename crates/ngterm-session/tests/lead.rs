//! Leadership on a real pane, on every OS: which attached client owns the session's size, as the program sees it,
//! and what each managed client is told of its role. The lead passing between two managed clients by claiming, a
//! follower's resize held and not applied, an unmanaged client taking the lead by typing, and the lead leaving with
//! its client, detached or dropped.

#[expect(dead_code, reason = "the helpers serve the pane and the lead tests alike")]
mod support;

use ngterm_session::protocol::{GridSize, LeadRole, Managed};
use ngterm_session::server::clients::ClientEvent;
use ngterm_session::server::pane::Pane;
use support::{Client, SIZE, TestResult, config, quit, shell, size_line};

const WIDE: GridSize = GridSize { cols: 100, rows: 40 };
const TALL: GridSize = GridSize { cols: 90, rows: 50 };

fn managed(nonce: &str, claim: bool) -> Managed {
    Managed {
        nonce: nonce.to_owned(),
        claim,
    }
}

/// A session of [`shell`] at [`SIZE`] whose creator attached as `creator`.
fn spawn(name: &str, dir: &tempfile::TempDir, creator: Option<Managed>) -> TestResult<(Pane, Client)> {
    let mut config = config(name, dir.path(), shell());
    config.managed = creator;
    let (pane, attached) = Pane::spawn(config)?;
    Ok((pane, Client::new(attached)))
}

/// The line `client` types, as its input: queued or dropped.
fn input(pane: &Pane, client: &Client, line: &str) -> TestResult<bool> {
    let enter = if cfg!(windows) { "\r" } else { "\n" };
    Ok(pane.input(client.id(), format!("{line}{enter}").as_bytes())?)
}

/// The size the program sees, as [`size_line`] prints it.
fn seen(size: GridSize) -> String {
    format!("SIZE={}x{}", size.rows, size.cols)
}

/// Whether `client` was sent a grid change since its last look.
fn resized(client: &Client) -> bool {
    client
        .events
        .iter()
        .any(|event| matches!(event, ClientEvent::Resized { .. }))
}

/// `claimer` just claimed the lead at `size` from `other`: it was told so before anything else, both were told under
/// the generation `generation`, and the program sees `size`.
fn assert_claimed(
    pane: &Pane,
    claimer: &mut Client,
    other: &mut Client,
    size: GridSize,
    generation: u32,
) -> TestResult {
    claimer.drain();
    other.drain();
    assert!(
        matches!(claimer.events.first(), Some(ClientEvent::Role { .. })),
        "the role came first: {:?}",
        claimer.events
    );
    assert_eq!(claimer.roles(), [(LeadRole::Leader, generation)]);
    assert_eq!(other.roles().last(), Some(&(LeadRole::Follower, generation)));
    assert_eq!(pane.leader(), Some(claimer.id()));
    assert_eq!(pane.size(), size);
    assert!(input(pane, claimer, &size_line())?, "the leader's input is queued");
    claimer.expect(&seen(size));
    Ok(())
}

#[test]
fn the_lead_passes_between_two_managed_clients_by_claiming() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, mut origin) = spawn("passing", &dir, Some(managed("origin", false)))?;
    origin.drain();
    assert_eq!(origin.roles(), [(LeadRole::Leader, 1)]);
    assert!(
        matches!(origin.events.first(), Some(ClientEvent::Role { .. })),
        "the role came first: {:?}",
        origin.events
    );

    // the viewer claims: it leads at its size, and both are told, under one generation
    let mut viewer = Client::join(&pane, WIDE, Some(&managed("viewer", true)))?;
    assert_claimed(&pane, &mut viewer, &mut origin, WIDE, 2)?;
    assert_eq!(origin.roles(), [(LeadRole::Leader, 1), (LeadRole::Follower, 2)]);
    assert!(!input(&pane, &origin, "echo dropped")?, "a follower's input is dropped");

    // the origin takes it back as ngterm's pane does: it attaches again, claiming, and lets its old attachment go
    let mut back = Client::join(&pane, SIZE, Some(&managed("origin", true)))?;
    assert!(pane.detach(origin.id()));
    assert_claimed(&pane, &mut back, &mut viewer, SIZE, 3)?;
    assert_eq!(viewer.roles(), [(LeadRole::Leader, 2), (LeadRole::Follower, 3)]);

    quit(&pane)?;
    back.finish();
    viewer.finish();
    Ok(())
}

#[test]
fn a_followers_resize_is_held_not_applied() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, mut leader) = spawn("follower-resize", &dir, None)?;
    let follower = Client::join(&pane, SIZE, Some(&managed("follower", false)))?;
    pane.resize_client(follower.id(), WIDE);
    assert_eq!(pane.size(), SIZE);
    assert!(input(&pane, &leader, &size_line())?);
    leader.expect(&seen(SIZE));
    assert!(!resized(&leader), "{:?}", leader.events);

    // the leader's resize applies, for the program and every client
    pane.resize_client(leader.id(), TALL);
    assert_eq!(pane.size(), TALL);
    assert!(input(&pane, &leader, &size_line())?);
    leader.expect(&seen(TALL));
    assert!(resized(&leader));
    quit(&pane)?;
    leader.finish();

    // the session's end leaves no client attached or leading
    assert_eq!(pane.clients(), 0);
    assert_eq!(pane.leader(), None);
    assert_eq!(pane.role(leader.id()), None);
    assert_eq!(pane.role(follower.id()), None);
    Ok(())
}

#[test]
fn an_unmanaged_client_takes_the_lead_by_typing_with_its_size() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, mut origin) = spawn("unmanaged", &dir, Some(managed("origin", false)))?;
    let mut stock = Client::join(&pane, WIDE, None)?;
    assert_eq!(pane.role(stock.id()), Some(LeadRole::Follower));
    // its window changes while it follows: held for when it leads
    pane.resize_client(stock.id(), TALL);
    assert_eq!(pane.size(), SIZE);
    // its terminal's reports are not typing
    assert!(!pane.input(stock.id(), b"\x1b[I\x1b[5;1R")?);
    assert_eq!(pane.leader(), Some(origin.id()));

    assert!(
        input(&pane, &stock, &size_line())?,
        "typing takes the lead and is queued"
    );
    assert_eq!(pane.leader(), Some(stock.id()));
    assert_eq!(pane.size(), TALL);
    stock.expect(&seen(TALL));
    origin.drain();
    assert_eq!(origin.roles(), [(LeadRole::Leader, 1), (LeadRole::Follower, 2)]);
    assert!(stock.roles().is_empty(), "an unmanaged client is never told");
    quit(&pane)?;
    stock.finish();
    Ok(())
}

#[test]
fn the_lead_leaves_with_its_client() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, origin) = spawn("leaving", &dir, Some(managed("origin", false)))?;
    let mut viewer = Client::join(&pane, WIDE, Some(&managed("viewer", false)))?;
    let stock = Client::join(&pane, TALL, None)?;

    // the leader detaches: the session is unowned and keeps its size, and a managed resize is still no claim
    assert!(pane.detach(origin.id()));
    viewer.drain();
    assert_eq!(viewer.roles(), [(LeadRole::Follower, 1), (LeadRole::Unowned, 2)]);
    assert_eq!(pane.leader(), None);
    pane.resize_client(viewer.id(), WIDE);
    assert_eq!(pane.size(), SIZE);

    // an unmanaged client's resize claims the unowned session
    pane.resize_client(stock.id(), TALL);
    assert_eq!(pane.leader(), Some(stock.id()));
    assert_eq!(pane.size(), TALL);
    viewer.drain();
    assert_eq!(viewer.roles().last(), Some(&(LeadRole::Follower, 3)));

    // a client gone without detaching (its subscription dropped) leaves the lead too, at once: the viewer is told
    // before anything else happens at the pane
    drop(stock);
    viewer.drain();
    assert_eq!(viewer.roles().last(), Some(&(LeadRole::Unowned, 4)));
    assert_eq!(pane.leader(), None);
    assert_eq!(pane.size(), TALL, "the session keeps the last leader's size");
    assert!(
        !input(&pane, &viewer, &size_line())?,
        "a managed client's input is dropped while no client leads"
    );
    quit(&pane)?;
    viewer.finish();
    Ok(())
}
