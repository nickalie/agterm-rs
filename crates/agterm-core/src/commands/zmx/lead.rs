//! Explicit zmx leadership in the app (upstream's `ZmxLeadBook`, `Ghostty/PaneLead.swift` and
//! `ControlServer+PaneLead.swift`): a wrapped pane's zmx client reports its role as a terminal title, a pane that does
//! not lead is covered, and taking the lead is always a fresh attach into a new terminal, since the covered one drew
//! output laid out for another client's grid. A pane whose zmx reported a role reads its screen and cursor, and types,
//! through the daemon: the role is a report up to 250 ms behind the daemon, so it never guarantees delivery through
//! the pane's own surface.

use std::collections::HashMap;

use agterm_host::Surface;
use uuid::Uuid;

use super::PaneReplacement;
use crate::Core;
use crate::command_restore::shell_quoted_line;
use crate::zmx::daemon_name;
use crate::zmx::lead::{LeadAttachment, LeadNotice, LeadRole, LeadState};

/// Every pane's lead state, by pane identity.
#[derive(Debug, Default)]
pub(crate) struct LeadBook {
    states: HashMap<Uuid, LeadState>,
}

impl LeadBook {
    /// Starts a new attachment of `pane`, forgetting what the previous one reported; `reattaching` covers it until its
    /// first report.
    pub(crate) fn begin(&mut self, pane: Uuid, attachment: LeadAttachment, reattaching: bool) {
        self.states.insert(pane, LeadState::new(attachment, reattaching));
    }

    pub(crate) fn forget(&mut self, pane: Uuid) {
        self.states.remove(&pane);
    }

    /// Applies a report to `pane`'s current attachment; the new role when it changed.
    pub(crate) fn apply(&mut self, pane: Uuid, notice: &LeadNotice) -> Option<LeadRole> {
        let state = self.states.get_mut(&pane)?;
        state.apply(notice).then(|| state.role()).flatten()
    }

    /// The nonce of `pane`'s current attachment.
    #[must_use]
    pub(crate) fn nonce(&self, pane: Uuid) -> Option<&str> {
        self.states.get(&pane).map(|state| state.attachment().nonce.as_str())
    }

    #[must_use]
    pub(crate) fn role(&self, pane: Uuid) -> Option<LeadRole> {
        self.states.get(&pane).and_then(LeadState::role)
    }

    #[must_use]
    pub(crate) fn covered(&self, pane: Uuid) -> bool {
        self.states.get(&pane).is_some_and(LeadState::covered)
    }

    #[must_use]
    pub(crate) fn reattaching(&self, pane: Uuid) -> bool {
        self.states.get(&pane).is_some_and(LeadState::reattaching)
    }
}

/// Where a pane's reads and typing go (upstream's `PaneSource`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneSource {
    /// The pane's own surface: its zmx never reported a role, or the read is of the viewport of a pane that leads.
    Surface,
    /// The pane's daemon, by name.
    Daemon(String),
    /// Nothing may answer for the pane.
    Refused(String),
}

/// What the cover over a pane shows (upstream's `PaneLeadCover`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadCover {
    pub title: &'static str,
    /// The cover asks for a key press to take the pane over; not while a takeover is on its way.
    pub offers_takeover: bool,
}

impl Core {
    /// Where a pane carrying `token` reads and types: the daemon once its zmx reported a role, the surface for the
    /// viewport of a pane that leads (`viewport`: a plain `session.text`, the one read whose meaning is the pane's own
    /// scrolled view).
    pub(crate) fn pane_source(&self, surface: &dyn Surface, viewport: bool) -> PaneSource {
        let Ok(identity) = Uuid::parse_str(surface.pane_token()) else {
            return PaneSource::Surface;
        };
        let covered = self.zmx.leads.covered(identity);
        if !covered && self.zmx.leads.role(identity).is_none() {
            return PaneSource::Surface;
        }
        if !surface.is_live_session() || self.zmx.host().is_none() {
            // an attached pane's daemon is on another machine, out of reach of a synchronous read
            return if covered {
                PaneSource::Refused(
                    "pane is in use on the Mac it runs on; take the lead to drive it from here".to_owned(),
                )
            } else {
                PaneSource::Surface
            };
        }
        if !covered && viewport {
            PaneSource::Surface
        } else {
            PaneSource::Daemon(daemon_name(identity))
        }
    }

    /// The refusal of a command acting on a covered pane's own surface (`session.paste`, `.selectall`, `.copy`, an
    /// opening search): that surface holds another grid's output and the daemon drops what it sends.
    pub(crate) fn covered_refusal(&self, surface: &dyn Surface) -> Option<&'static str> {
        let identity = Uuid::parse_str(surface.pane_token()).ok()?;
        self.zmx
            .leads
            .covered(identity)
            .then_some("pane is covered while another Mac leads it; take the lead first (session lead)")
    }

    /// A role report a pane's terminal title carried (upstream's `PaneLead.report`): applied to the pane's current
    /// attachment, and a pane its daemon left unowned attaches again without the claim, so it leads only if nobody
    /// claimed the session meanwhile. Whether the role changed.
    pub(crate) fn lead_reported(&mut self, session: Uuid, token: &str, notice: &LeadNotice) -> bool {
        let Ok(identity) = Uuid::parse_str(token) else {
            return false;
        };
        let Some(role) = self.zmx.leads.apply(identity, notice) else {
            return false;
        };
        if role == LeadRole::Unowned {
            self.reattach_pane(session, token, false);
        }
        true
    }

    /// The cover over the pane carrying `token`, `None` while it is uncovered.
    #[must_use]
    pub fn pane_lead_cover(&self, token: &str) -> Option<LeadCover> {
        let identity = Uuid::parse_str(token).ok()?;
        let leads = &self.zmx.leads;
        if !leads.covered(identity) {
            return None;
        }
        let reattaching = leads.reattaching(identity);
        let title = if reattaching {
            "Taking over…"
        } else if leads.role(identity) == Some(LeadRole::Unowned) {
            "Reconnecting…"
        } else if self.pane_is_remote(identity) {
            "In use on the machine it runs on"
        } else {
            "In use from another machine"
        };
        Some(LeadCover {
            title,
            offers_takeover: !reattaching,
        })
    }

    /// The lead the tree reports for the pane carrying `token`: `leader`, `follower` or `unowned`, absent until its
    /// zmx reported one.
    #[must_use]
    pub(crate) fn pane_lead(&self, token: &str) -> Option<&'static str> {
        let identity = Uuid::parse_str(token).ok()?;
        self.zmx.leads.role(identity).map(<&'static str>::from)
    }

    /// A key press on a covered pane (upstream's `PaneLead.consumes`): the pane is attached again with the claim,
    /// unless a takeover is already on its way. Whether one started; settle afterwards.
    pub fn take_pane_lead(&mut self, session: Uuid, token: &str) -> bool {
        let Ok(identity) = Uuid::parse_str(token) else {
            return false;
        };
        // a remote pane waiting to be attached again retries now instead (upstream's `PaneLead.consumes`)
        if self.remote.viewer.reconnects.waiting(identity) {
            self.retry_reconnect(identity, std::time::Instant::now());
            return true;
        }
        if !self.zmx.leads.covered(identity) || self.zmx.leads.reattaching(identity) {
            return false;
        }
        self.reattach_pane(session, token, true)
    }

    /// Replaces the live pane carrying `token` with a fresh attach to its daemon (upstream's `reattachPane` and
    /// `PaneReattach.launch`), covered until its first report. The attach never creates: its trailing `/bin/sh -c`
    /// runs only when the daemon is gone, and fails, so a vanished session ends the pane rather than handing back a
    /// fresh shell under the old identity. False when the pane is not a live one this instance can wrap.
    pub(crate) fn reattach_pane(&mut self, session: Uuid, token: &str, claim: bool) -> bool {
        let Ok(identity) = Uuid::parse_str(token) else {
            return false;
        };
        let library = self.library();
        let live = library
            .window_holding_session(session)
            .and_then(|window| library.store(window))
            .and_then(|store| store.session(session).or_else(|| store.pending_close_session(session)))
            .and_then(|found| found.pane_role_for_token(token).map(|role| found.pane_slot(role)))
            .and_then(|slot| slot.get())
            .is_some_and(Surface::is_live_session);
        if !live {
            // a remote replica is attached again over ssh, covered until its first report
            return self.reattach_remote_pane(session, identity, LeadAttachment::new(claim), true);
        }
        let attachment = LeadAttachment::new(claim);
        let Some(configuration) = self.wrap_configuration(identity, Some(attachment.clone())) else {
            return false;
        };
        let mut arguments = configuration.attach_arguments();
        arguments.extend(
            ["/bin/sh", "-c", "printf '%s\\n' 'agterm: session is gone'; exit 1"]
                .into_iter()
                .map(str::to_owned),
        );
        self.zmx.leads.begin(identity, attachment, true);
        self.zmx.leaders.borrow_mut().note_lifecycle_change();
        self.request_pane_replacement(PaneReplacement {
            session,
            token: token.to_owned(),
            command: crate::zmx::support::settled(&shell_quoted_line(&arguments)),
            env: configuration.environment.into_iter().collect(),
            cwd: None,
            remote: false,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(nonce: &str, role: LeadRole, generation: u32) -> LeadNotice {
        LeadNotice {
            nonce: nonce.to_owned(),
            role,
            generation,
        }
    }

    #[test]
    fn the_book_follows_the_current_attachment_only() {
        let pane = Uuid::new_v4();
        let mut book = LeadBook::default();
        let first = LeadAttachment::new(false);
        book.begin(pane, first.clone(), false);
        assert!(!book.covered(pane));
        assert_eq!(
            book.apply(pane, &notice(&first.nonce, LeadRole::Follower, 1)),
            Some(LeadRole::Follower)
        );
        assert!(book.covered(pane));
        // a report from an attachment the pane replaced is dropped
        let second = LeadAttachment::new(true);
        book.begin(pane, second.clone(), true);
        assert!(book.covered(pane) && book.reattaching(pane));
        assert_eq!(book.apply(pane, &notice(&first.nonce, LeadRole::Leader, 2)), None);
        assert_eq!(
            book.apply(pane, &notice(&second.nonce, LeadRole::Leader, 1)),
            Some(LeadRole::Leader)
        );
        assert!(!book.covered(pane) && !book.reattaching(pane));
        assert_eq!(book.role(pane), Some(LeadRole::Leader));
        book.forget(pane);
        assert_eq!(book.role(pane), None);
        assert_eq!(book.apply(pane, &notice(&second.nonce, LeadRole::Follower, 3)), None);
    }
}
