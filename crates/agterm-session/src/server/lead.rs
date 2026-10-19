//! Leadership: which of a session's attached clients owns its size, and what each managed client is told of its role.
//! Only the leader's grid is applied to the session, so every other client draws output laid out for a grid it does
//! not have. `agterm-core::live::lead` decides when an agterm pane claims the lead; the server sends a managed client
//! its role ([`ClientEvent::Role`](super::clients::ClientEvent)) on every change, and an attach client writes it to its
//! terminal as the attach contract's role title ([`role_title`]), which the core reads.
//!
//! Sources:
//! - the rules are upstream agterm's explicit leadership for zmx (`scripts/zmx-patches/0001-explicit-leadership.patch`
//!   in `.upstream/agterm`, carried here as `build/patches/zmx/0001`: the daemon's `handleManage`, `setLeader`,
//!   `roleOf`, `notifyRoles`, and the leadership parts of `handleInit`, `handleInput`, `handleResize` and
//!   `closeClient`), which `agterm-core`'s lead policy and the conformance suite pin, and its client's role title
//!   (`role.zig`) under the attach contract's name;
//! - [`is_user_input`] is zmx's `isUserInput` with its `parseKittyCsiU` (`src/util.zig` at `8bab1f0`, the zmx agterm
//!   ran on; MIT, NOTICE), over the states of Ghostty's VT parser it runs (`src/terminal/parse_table.zig` and
//!   `src/terminal/Parser.zig` at `LIBGHOSTTY_VT_REV`), mirrored here for the actions it looks at;
//! - herdr: `src/server/headless.rs` (`handle_terminal_attach`: a terminal's attach owner, whose stored terminal size
//!   the terminal takes when it attaches, and `direct_attach_resize_locks`, which keep every other client's size off
//!   it) at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified: the owner is the leader
//!   under the rules below, so a claiming attach takes the lead without closing the previous leader's connection
//!   (herdr's takeover closes it) and a leader leaving releases the size to no client (herdr's lock goes with its
//!   owner); kept from herdr: every client's last reported size is held, and a client taking the lead brings it, where
//!   zmx asked the new leader for its size and waited for the answer.
//!
//! # The rules
//!
//! - An attaching client leads when no client leads, or when it is managed and claims the lead; otherwise it follows.
//! - The leader's input goes to the program. A follower's is dropped, except that an unmanaged follower's typing
//!   ([`is_user_input`]: keys, not its terminal's reports) takes the lead and goes to the program. A managed client
//!   leads only by claiming: what it sends while following is its terminal answering queries, or a person typing at a
//!   pane agterm covers.
//! - Only the leader's resize applies. An unmanaged client's resize while no client leads takes the lead first; a
//!   managed client's window changing size is never a claim.
//! - The leader detaching leaves the session unowned: the session keeps its last size until a client takes the lead.
//! - The generation counts the session's leadership changes (a new leader, the leader leaving), wrapping, so a client
//!   can tell a late report from a newer one.
//! - Every managed client is told its role whenever it differs from what it was last told, the attaching one before
//!   anything else it is sent; an unmanaged client is never told.
//!
//! [`Lead`] only decides: what to resize the session to and whom to tell what come back as a [`Change`], which the
//! pane applies under its lock, in order with the output.

use crate::protocol::{GridSize, LeadRole, Managed};

use super::clients::ClientId;

/// The reserved prefix of the attach contract's role title: `OSC 2;agterm-live-role;<nonce>:<role>:<generation>`.
pub const ROLE_TITLE_PREFIX: &str = "agterm-live-role;";

/// The longest nonce a managed client may carry.
pub const NONCE_MAX: usize = 64;

/// Whether `nonce` is a token the role title can carry: 1 to [`NONCE_MAX`] ASCII letters, digits and dashes, so a
/// title a program inside the session sets cannot pass for a report, nor a nonce end the title early. An attach whose
/// nonce is not one is unmanaged, as zmx's client never opted in with one.
#[must_use]
pub fn is_nonce(nonce: &str) -> bool {
    (1..=NONCE_MAX).contains(&nonce.len()) && nonce.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// The role's name in the role title.
#[must_use]
pub fn role_name(role: LeadRole) -> &'static str {
    match role {
        LeadRole::Unowned => "unowned",
        LeadRole::Leader => "leader",
        LeadRole::Follower => "follower",
    }
}

/// The title a client attached under `nonce` writes to its terminal for a role report: the window title
/// `agterm-live-role;<nonce>:<role>:<generation>`, terminated by ST. A title and not a desktop notification, because a
/// terminal delivers every title where it rate-limits notifications, and a dropped report would leave the terminal
/// wrong about a role the server enforces.
#[must_use]
pub fn role_title(nonce: &str, role: LeadRole, generation: u32) -> String {
    format!(
        "\x1b]2;{ROLE_TITLE_PREFIX}{nonce}:{}:{generation}\x1b\\",
        role_name(role)
    )
}

/// A managed client's role changed: it is to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleNotice {
    pub client: ClientId,
    pub role: LeadRole,
    pub generation: u32,
}

/// What a leadership decision asks of the pane: a size to apply, then the notices to send.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use]
pub struct Change {
    /// The leader's size, to apply to the session (a size it already has changes nothing).
    pub resize: Option<GridSize>,
    pub notices: Vec<RoleNotice>,
}

/// What becomes of a client's input.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct Input {
    /// The input goes to the program, after the change is applied.
    pub forward: bool,
    pub change: Change,
}

/// One attached client.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    id: ClientId,
    managed: bool,
    /// Its last reported grid.
    size: GridSize,
    /// The role it was last told; `None` before its first report and for an unmanaged client.
    told: Option<LeadRole>,
}

/// One session's leadership.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lead {
    leader: Option<ClientId>,
    generation: u32,
    members: Vec<Member>,
}

impl Lead {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The leading client.
    #[must_use]
    pub fn leader(&self) -> Option<ClientId> {
        self.leader
    }

    /// The session's leadership changes so far, wrapping.
    #[must_use]
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// The role of the client `id`; `None` when it is not attached.
    #[must_use]
    pub fn role(&self, id: ClientId) -> Option<LeadRole> {
        self.member(id).map(|member| self.role_of(member.id))
    }

    /// The client `id` attaches with the grid `size`: it leads when no client does, or when it is managed and claims.
    /// `managed` with a nonce that [`is_nonce`] refuses counts as unmanaged.
    pub fn attach(&mut self, id: ClientId, size: GridSize, managed: Option<&Managed>) -> Change {
        let managed = managed.filter(|managed| is_nonce(&managed.nonce));
        self.members.retain(|member| member.id != id);
        self.members.push(Member {
            id,
            managed: managed.is_some(),
            size,
            told: None,
        });
        let mut change = Change::default();
        if self.leader.is_none() || managed.is_some_and(|managed| managed.claim) {
            self.set_leader(id, &mut change);
        }
        self.notify(&mut change);
        change
    }

    /// The client `id` sent `data` for the program.
    pub fn input(&mut self, id: ClientId, data: &[u8]) -> Input {
        let mut change = Change::default();
        if self.leader == Some(id) {
            return Input { forward: true, change };
        }
        let Some(member) = self.member(id) else {
            return Input { forward: false, change };
        };
        if member.managed || !is_user_input(data) {
            return Input { forward: false, change };
        }
        self.set_leader(id, &mut change);
        self.notify(&mut change);
        Input { forward: true, change }
    }

    /// The client `id`'s grid is now `size`: held as its size, applied while it leads.
    pub fn resize(&mut self, id: ClientId, size: GridSize) -> Change {
        let mut change = Change::default();
        let Some(member) = self.members.iter_mut().find(|member| member.id == id) else {
            return change;
        };
        member.size = size;
        let managed = member.managed;
        if self.leader.is_none() && !managed {
            self.set_leader(id, &mut change);
            self.notify(&mut change);
        } else if self.leader == Some(id) {
            change.resize = Some(size);
        }
        change
    }

    /// The client `id` detached; when it led, the session is left unowned.
    pub fn detach(&mut self, id: ClientId) -> Change {
        let mut change = Change::default();
        self.members.retain(|member| member.id != id);
        if self.leader == Some(id) {
            self.leader = None;
            self.generation = self.generation.wrapping_add(1);
        }
        self.notify(&mut change);
        change
    }

    fn member(&self, id: ClientId) -> Option<&Member> {
        self.members.iter().find(|member| member.id == id)
    }

    fn role_of(&self, id: ClientId) -> LeadRole {
        role_under(self.leader, id)
    }

    /// `id` leads from now on, with its last reported size.
    fn set_leader(&mut self, id: ClientId, change: &mut Change) {
        self.leader = Some(id);
        self.generation = self.generation.wrapping_add(1);
        change.resize = self.member(id).map(|member| member.size);
    }

    /// Tell every managed client whose role differs from the last one it was told.
    fn notify(&mut self, change: &mut Change) {
        let (leader, generation) = (self.leader, self.generation);
        for member in self.members.iter_mut().filter(|member| member.managed) {
            let role = role_under(leader, member.id);
            if member.told != Some(role) {
                member.told = Some(role);
                change.notices.push(RoleNotice {
                    client: member.id,
                    role,
                    generation,
                });
            }
        }
    }
}

/// The role of the client `id` while `leader` leads.
fn role_under(leader: Option<ClientId>, id: ClientId) -> LeadRole {
    match leader {
        None => LeadRole::Unowned,
        Some(leader) if leader == id => LeadRole::Leader,
        Some(_) => LeadRole::Follower,
    }
}

/// Whether `data` holds what a person typed, not only what a terminal reports by itself: a printable character, Return,
/// line feed, tab or backspace, a key the Kitty keyboard protocol or a `~` sequence encodes (a Kitty key release alone
/// is not), or a modified arrow key. A mouse or focus report stops the look at once, as zmx's does; a cursor position
/// or other report, a plain arrow key and SS3 keys alone are not typing.
#[must_use]
pub fn is_user_input(data: &[u8]) -> bool {
    let mut parser = Parser::default();
    let mut at = 0;
    while at < data.len() {
        if data[at] == 0x1b
            && at + 2 < data.len()
            && data[at + 1] == b'['
            && let Some(kitty) = kitty_csi_u(&data[at + 2..])
        {
            if kitty.event != KITTY_RELEASE {
                return true;
            }
            at += 2 + kitty.consumed;
            continue;
        }
        match parser.next(data[at]) {
            Act::Print | Act::Execute(0x0d | 0x0a | 0x09 | 0x08) => return true,
            Act::Csi { final_byte, params } => match final_byte {
                b'u' | b'~' => return true,
                b'A'..=b'D' if params > 1 => return true,
                // mouse and focus reports: the terminal's own, never typing
                b'M' | b'<' | b'I' | b'O' => return false,
                _ => {}
            },
            _ => {}
        }
        at += 1;
    }
    false
}

/// The Kitty keyboard protocol's event type of a key release.
const KITTY_RELEASE: u32 = 3;

/// A Kitty keyboard protocol key: `CSI key[:alternates] ; modifiers[:event] [; text] u`.
struct KittyKey {
    event: u32,
    /// The bytes after `CSI` it takes, its `u` included.
    consumed: usize,
}

/// The Kitty key `bytes` (what follows `CSI`) start with.
fn kitty_csi_u(bytes: &[u8]) -> Option<KittyKey> {
    let mut at = 0;
    decimal(bytes, &mut at)?;
    while bytes.get(at) == Some(&b':') {
        at += 1;
        let _ = decimal(bytes, &mut at);
    }
    if bytes.get(at) != Some(&b';') {
        return None;
    }
    at += 1;
    if decimal(bytes, &mut at)? < 1 {
        return None;
    }
    let mut event = 1;
    if bytes.get(at) == Some(&b':') {
        at += 1;
        event = decimal(bytes, &mut at)?;
    }
    if bytes.get(at) == Some(&b';') {
        at += 1;
        while bytes.get(at).is_some_and(|byte| byte.is_ascii_digit() || *byte == b':') {
            at += 1;
        }
    }
    if bytes.get(at) != Some(&b'u') {
        return None;
    }
    Some(KittyKey {
        event,
        consumed: at + 1,
    })
}

/// The decimal number at `*at`, wrapping as zmx's does, `*at` moved past it; `None` without a digit.
fn decimal(bytes: &[u8], at: &mut usize) -> Option<u32> {
    let start = *at;
    let mut value: u32 = 0;
    while let Some(digit) = bytes.get(*at).filter(|byte| byte.is_ascii_digit()) {
        value = value.wrapping_mul(10).wrapping_add(u32::from(digit - b'0'));
        *at += 1;
    }
    (*at > start).then_some(value)
}

/// Ghostty's parser states.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    SosPmApcString,
}

/// The transition actions [`is_user_input`] looks at; every other one is [`Act::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    None,
    Print,
    Execute(u8),
    Param(u8),
    /// A CSI's final byte, before its parameters are counted.
    Dispatch(u8),
    /// A CSI dispatched with `params` parameters.
    Csi {
        final_byte: u8,
        params: u8,
    },
}

/// Ghostty's limit on a CSI's parameters (`Parser.zig` `MAX_PARAMS`): a CSI with more dispatches nothing.
const MAX_PARAMS: u8 = 24;

/// Ghostty's VT parser, for the actions it emits and the CSI parameters it counts.
#[derive(Debug, Default)]
struct Parser {
    state: State,
    /// Parameters ended by a separator.
    params: u8,
    /// A digit since the last separator: one more parameter at the dispatch.
    digits: bool,
    /// A `:` separated two parameters, which only SGR takes.
    colon: bool,
}

impl Parser {
    fn next(&mut self, byte: u8) -> Act {
        let (state, act) = transition(self.state, byte);
        if state != self.state && matches!(state, State::Escape | State::CsiEntry | State::DcsEntry) {
            self.params = 0;
            self.digits = false;
            self.colon = false;
        }
        self.state = state;
        match act {
            Act::Param(separator @ (b';' | b':')) => {
                if self.params < MAX_PARAMS {
                    self.colon |= separator == b':';
                    self.params += 1;
                    self.digits = false;
                }
                Act::None
            }
            Act::Param(_) => {
                self.digits = true;
                Act::None
            }
            Act::Dispatch(final_byte) => {
                if self.params >= MAX_PARAMS || (final_byte != b'm' && self.colon) {
                    return Act::None;
                }
                Act::Csi {
                    final_byte,
                    params: self.params + u8::from(self.digits),
                }
            }
            act => act,
        }
    }
}

/// Ghostty's transition table (`parse_table.zig`): the state's own transitions win over the "anywhere" ones; a byte
/// neither names leaves the state as it is.
fn transition(state: State, byte: u8) -> (State, Act) {
    within(state, byte)
        .or_else(|| anywhere(byte))
        .unwrap_or((state, Act::None))
}

/// The C0 controls a state executes or ignores in place: all but CAN, SUB and ESC.
fn is_c0(byte: u8) -> bool {
    matches!(byte, 0x00..=0x17 | 0x19 | 0x1c..=0x1f)
}

fn anywhere(byte: u8) -> Option<(State, Act)> {
    Some(match byte {
        0x18 | 0x1a | 0x80..=0x8f | 0x91..=0x97 | 0x99 | 0x9a => (State::Ground, Act::Execute(byte)),
        0x9c => (State::Ground, Act::None),
        0x1b => (State::Escape, Act::None),
        0x98 | 0x9e | 0x9f => (State::SosPmApcString, Act::None),
        0x9b => (State::CsiEntry, Act::None),
        0x90 => (State::DcsEntry, Act::None),
        0x9d => (State::OscString, Act::None),
        _ => return None,
    })
}

fn within(state: State, byte: u8) -> Option<(State, Act)> {
    match state {
        State::Ground => match byte {
            _ if is_c0(byte) => Some((state, Act::Execute(byte))),
            0x20..=0x7f => Some((state, Act::Print)),
            _ => None,
        },
        State::Escape => escape(byte),
        State::EscapeIntermediate => match byte {
            _ if is_c0(byte) => Some((state, Act::Execute(byte))),
            0x20..=0x2f | 0x7f => Some((state, Act::None)),
            0x30..=0x7e => Some((State::Ground, Act::None)),
            _ => None,
        },
        State::CsiEntry | State::CsiParam | State::CsiIntermediate | State::CsiIgnore => csi(state, byte),
        State::DcsEntry | State::DcsParam | State::DcsIntermediate => dcs(state, byte),
        State::DcsPassthrough | State::DcsIgnore => match byte {
            _ if is_c0(byte) => Some((state, Act::None)),
            0x20..=0xff => Some((state, Act::None)),
            _ => None,
        },
        State::OscString => match byte {
            0x07 => Some((State::Ground, Act::None)),
            _ if is_c0(byte) => Some((state, Act::None)),
            0x20..=0xff => Some((state, Act::None)),
            _ => None,
        },
        State::SosPmApcString => match byte {
            _ if is_c0(byte) => Some((state, Act::None)),
            0x20..=0x7f => Some((state, Act::None)),
            _ => None,
        },
    }
}

fn escape(byte: u8) -> Option<(State, Act)> {
    Some(match byte {
        _ if is_c0(byte) => (State::Escape, Act::Execute(byte)),
        0x7f => (State::Escape, Act::None),
        0x20..=0x2f => (State::EscapeIntermediate, Act::None),
        0x50 => (State::DcsEntry, Act::None),
        0x58 | 0x5e | 0x5f => (State::SosPmApcString, Act::None),
        0x5b => (State::CsiEntry, Act::None),
        0x5d => (State::OscString, Act::None),
        0x30..=0x7e => (State::Ground, Act::None),
        _ => return None,
    })
}

fn csi(state: State, byte: u8) -> Option<(State, Act)> {
    if is_c0(byte) {
        return Some((state, Act::Execute(byte)));
    }
    let next = match (state, byte) {
        (_, 0x7f) | (State::CsiIgnore, 0x20..=0x3f) => state,
        (_, 0x40..=0x7e) => State::Ground,
        // a private marker (`<`, `=`, `>`, `?`) only opens the parameters
        (State::CsiEntry, 0x30..=0x39 | 0x3b..=0x3f) | (State::CsiParam, 0x30..=0x3b) => State::CsiParam,
        (_, 0x20..=0x2f) => State::CsiIntermediate,
        (_, 0x30..=0x3f) => State::CsiIgnore,
        _ => return None,
    };
    let act = match (state, next, byte) {
        // an ignored CSI ends without a dispatch
        (State::CsiIgnore, ..) => Act::None,
        (_, State::Ground, _) => Act::Dispatch(byte),
        (_, State::CsiParam, 0x30..=0x3b) => Act::Param(byte),
        _ => Act::None,
    };
    Some((next, act))
}

fn dcs(state: State, byte: u8) -> Option<(State, Act)> {
    if is_c0(byte) {
        return Some((state, Act::None));
    }
    let next = match (state, byte) {
        (_, 0x7f) => state,
        (_, 0x40..=0x7e) => State::DcsPassthrough,
        (_, 0x20..=0x2f) => State::DcsIntermediate,
        (State::DcsEntry, 0x30..=0x39 | 0x3b..=0x3f) | (State::DcsParam, 0x30..=0x39 | 0x3b) => State::DcsParam,
        (_, 0x30..=0x3f) => State::DcsIgnore,
        _ => return None,
    };
    Some((next, Act::None))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: GridSize = GridSize { cols: 80, rows: 24 };
    const WIDE: GridSize = GridSize { cols: 100, rows: 30 };
    const TALL: GridSize = GridSize { cols: 89, rows: 60 };

    fn managed(claim: bool) -> Managed {
        Managed {
            nonce: "n1".to_owned(),
            claim,
        }
    }

    fn notice(client: ClientId, role: LeadRole, generation: u32) -> RoleNotice {
        RoleNotice {
            client,
            role,
            generation,
        }
    }

    #[test]
    fn a_managed_client_leads_by_claiming_never_by_typing_or_resizing() {
        let mut lead = Lead::new();
        // the first managed attach takes the vacant lead without asking for it
        let origin = lead.attach(1, WIDE, Some(&managed(false)));
        assert_eq!(origin.resize, Some(WIDE));
        assert_eq!(origin.notices, [notice(1, LeadRole::Leader, 1)]);

        // a second one without the claim follows, and neither typing nor resizing promotes it
        let viewer = lead.attach(2, SMALL, Some(&managed(false)));
        assert_eq!(viewer.resize, None);
        assert_eq!(viewer.notices, [notice(2, LeadRole::Follower, 1)]);
        let typed = lead.input(2, b"typed by a person\r");
        assert!(!typed.forward);
        assert_eq!(typed.change, Change::default());
        assert_eq!(lead.resize(2, TALL), Change::default());
        assert_eq!(lead.leader(), Some(1));

        // the leader going away leaves the session unowned; a managed follower's resize is still not a claim
        let left = lead.detach(1);
        assert_eq!(left.resize, None);
        assert_eq!(left.notices, [notice(2, LeadRole::Unowned, 2)]);
        assert_eq!(lead.resize(2, SMALL), Change::default());
        assert_eq!(lead.leader(), None);
        assert_eq!(lead.role(2), Some(LeadRole::Unowned));
        assert_eq!(lead.role(1), None);
    }

    #[test]
    fn a_claiming_attach_takes_the_lead_from_a_live_leader_and_both_are_told() {
        let mut lead = Lead::new();
        let _ = lead.attach(1, WIDE, Some(&managed(false)));
        let claim = lead.attach(2, TALL, Some(&managed(true)));
        assert_eq!(claim.resize, Some(TALL));
        assert_eq!(
            claim.notices,
            [notice(1, LeadRole::Follower, 2), notice(2, LeadRole::Leader, 2)]
        );
        assert_eq!(lead.leader(), Some(2));
        assert_eq!(lead.generation(), 2);
        // the lead passes back the same way
        let _ = lead.detach(2);
        let back = lead.attach(3, SMALL, Some(&managed(true)));
        assert_eq!(back.resize, Some(SMALL));
        assert_eq!(
            back.notices,
            [notice(1, LeadRole::Follower, 4), notice(3, LeadRole::Leader, 4)]
        );
    }

    #[test]
    fn only_the_leaders_resize_applies() {
        let mut lead = Lead::new();
        let _ = lead.attach(1, SMALL, None);
        let _ = lead.attach(2, SMALL, None);
        assert_eq!(lead.resize(1, WIDE).resize, Some(WIDE));
        assert_eq!(lead.resize(2, TALL), Change::default());
        assert!(lead.input(1, b"\x1b[I").forward, "the leader's every byte goes through");
    }

    #[test]
    fn an_unmanaged_client_leads_by_typing_with_its_last_size_and_is_never_told() {
        let mut lead = Lead::new();
        let _ = lead.attach(1, WIDE, Some(&managed(false)));
        let stock = lead.attach(2, SMALL, None);
        assert_eq!(stock, Change::default());
        assert_eq!(lead.role(2), Some(LeadRole::Follower));
        // its window changes while it follows: held, not applied
        assert_eq!(lead.resize(2, TALL), Change::default());
        // a terminal report is not typing
        let report = lead.input(2, b"\x1b[I\x1b[12;5R");
        assert!(!report.forward);
        let typed = lead.input(2, b"x");
        assert!(typed.forward);
        assert_eq!(typed.change.resize, Some(TALL));
        assert_eq!(typed.change.notices, [notice(1, LeadRole::Follower, 2)]);
        assert_eq!(lead.leader(), Some(2));
    }

    #[test]
    fn an_unmanaged_resize_claims_an_unowned_session() {
        let mut lead = Lead::new();
        let _ = lead.attach(1, WIDE, Some(&managed(false)));
        let _ = lead.attach(2, SMALL, None);
        let left = lead.detach(1);
        assert!(left.notices.is_empty());
        let resized = lead.resize(2, TALL);
        assert_eq!(resized.resize, Some(TALL));
        assert_eq!(lead.leader(), Some(2));
        assert_eq!(lead.generation(), 3);
    }

    #[test]
    fn an_attach_leads_an_unowned_session_whatever_it_is() {
        let mut lead = Lead::new();
        let _ = lead.attach(1, WIDE, Some(&managed(false)));
        let _ = lead.detach(1);
        let next = lead.attach(2, SMALL, Some(&managed(false)));
        assert_eq!(next.resize, Some(SMALL));
        assert_eq!(next.notices, [notice(2, LeadRole::Leader, 3)]);
    }

    #[test]
    fn a_follower_leaving_changes_nothing() {
        let mut lead = Lead::new();
        let _ = lead.attach(1, WIDE, Some(&managed(false)));
        let _ = lead.attach(2, SMALL, Some(&managed(false)));
        assert_eq!(lead.detach(2), Change::default());
        assert_eq!(lead.generation(), 1);
        assert_eq!(lead.detach(7), Change::default(), "an unknown client");
        assert!(!lead.input(7, b"x").forward);
        assert_eq!(lead.resize(7, TALL), Change::default());
    }

    #[test]
    fn a_nonce_the_title_cannot_carry_is_unmanaged() {
        let mut lead = Lead::new();
        let _ = lead.attach(1, WIDE, None);
        let forged = Managed {
            nonce: "a;b".to_owned(),
            claim: true,
        };
        let change = lead.attach(2, SMALL, Some(&forged));
        assert_eq!(change, Change::default(), "neither claims nor is told");
        assert!(
            lead.input(2, b"x").forward,
            "and leads by typing, as an unmanaged client"
        );
    }

    #[test]
    fn the_generation_wraps() {
        let mut lead = Lead {
            generation: u32::MAX,
            ..Lead::new()
        };
        let change = lead.attach(1, WIDE, Some(&managed(false)));
        assert_eq!(change.notices, [notice(1, LeadRole::Leader, 0)]);
    }

    #[test]
    fn nonces_are_plain_tokens() {
        assert!(is_nonce("a1B-2"));
        assert!(is_nonce(&"x".repeat(NONCE_MAX)));
        for refused in ["", "a;b", "a b", "a:b", "é", "a\x1b\\"] {
            assert!(!is_nonce(refused), "{refused:?}");
        }
        assert!(!is_nonce(&"x".repeat(NONCE_MAX + 1)));
    }

    #[test]
    fn the_role_title_is_the_attach_contracts() {
        assert_eq!(
            role_title("n1", LeadRole::Leader, 7),
            "\x1b]2;agterm-live-role;n1:leader:7\x1b\\"
        );
        assert_eq!(
            role_title("n-2", LeadRole::Unowned, u32::MAX),
            "\x1b]2;agterm-live-role;n-2:unowned:4294967295\x1b\\"
        );
        // its payload is what agterm-core's lead policy reads, under its prefix
        for role in [LeadRole::Unowned, LeadRole::Leader, LeadRole::Follower] {
            let title = role_title("n1", role, 3);
            let payload = title
                .strip_prefix("\x1b]2;")
                .and_then(|title| title.strip_suffix("\x1b\\"))
                .and_then(|title| title.strip_prefix(ROLE_TITLE_PREFIX))
                .expect("the title's frame");
            let notice = agterm_core::live::lead::LeadNotice::parse(&format!(
                "{}{payload}",
                agterm_core::live::lead::LeadNotice::PREFIX
            ))
            .expect("a notice the core parses");
            assert_eq!((notice.nonce.as_str(), notice.generation), ("n1", 3));
            assert_eq!(<&str>::from(notice.role), role_name(role));
        }
    }

    // zmx's `isUserInput` tests (src/util.zig), and the cases its rules imply

    #[test]
    fn typing_is_user_input() {
        for typed in [
            "hello",
            "Hello World!",
            "12345",
            "!@#$%^&*()",
            " ",
            "\n",
            "test\n",
            "\r",
            "\t",
            "col1\tcol2",
            "\x08",
            "\x7f",
            "\n\r\t",
            "hello\nworld",
        ] {
            assert!(is_user_input(typed.as_bytes()), "{typed:?}");
        }
    }

    #[test]
    fn keys_are_user_input() {
        for key in [
            "\x1b[3~",
            "\x1b[5~",
            "\x1b[15;2~",
            "\x1b[3~\x1b[6~",
            "abc\x1b[3~def",
            // SS3 dispatches, then its final key prints
            "\x1bOA",
            "\x1b[1;1A",
            "\x1b[1;1;1A",
            "\x1b[1;5A",
            "\x1b[1;5D",
            "\x1b[1;3B",
            "\x1b[200~",
            "\x1b[200~hello\x1b[201~",
            // a Return inside an unfinished sequence still executes
            "\x1b[\r",
        ] {
            assert!(is_user_input(key.as_bytes()), "{key:?}");
        }
    }

    #[test]
    fn kitty_keys_are_user_input_but_a_release_alone_is_not() {
        for key in [
            "\x1b[11;2u",
            "\x1b[102;1:1u",
            "\x1b[57444;1:1u",
            "\x1b[97:65;2:2u",
            "\x1b[102;1:1u\x1b[67;65;31M",
            "\x1b[102;1:3ux",
        ] {
            assert!(is_user_input(key.as_bytes()), "{key:?}");
        }
        assert!(!is_user_input(b"\x1b[102;1:3u"));
        assert!(!is_user_input(b"\x1b[102;1:3u\x1b[102;1:3u"));
    }

    #[test]
    fn terminal_reports_are_not_user_input() {
        for report in [
            "",
            "\x1b[2;1H",
            "\x1b[0m",
            "\x1b[6n",
            "\x1b[12;40R",
            "\x1b[?62;22c",
            // a plain arrow key carries no modifier: zmx leaves it out
            "\x1b[A",
            "\x1b[M@ 0 0",
            "\x1b[M@ 1 1",
            "\x1b[<0;1;1M",
            "\x1b[<64;1;1M",
            "\x1b[I",
            "\x1b[O",
            // a mouse report ends the look, what follows it unread
            "\x1b[<0;1;1Mtyped",
            // control strings' bodies are not printed
            "\x1b]11;rgb:0000/0000/0000\x07",
            "\x1b]11;rgb:0000/0000/0000\x1b\\",
            "\x1bP1$r0m\x1b\\",
            "\x1b_Gi=1;OK\x1b\\",
            // a colon outside SGR dispatches nothing
            "\x1b[1:5A",
            // UTF-8 is not printed by the bare parser zmx runs
            "\u{e9}",
        ] {
            assert!(!is_user_input(report.as_bytes()), "{report:?}");
        }
    }

    #[test]
    fn a_csi_past_ghosttys_parameter_limit_dispatches_nothing() {
        let arrow = |params: usize| format!("\x1b[{}A", vec!["1"; params].join(";"));
        assert!(is_user_input(arrow(usize::from(MAX_PARAMS)).as_bytes()));
        assert!(!is_user_input(arrow(usize::from(MAX_PARAMS) + 1).as_bytes()));
    }
}
