//! Which client of a pane's live session owns the pty size (upstream's `ZmxLead.swift`). Only the leader's grid is
//! applied, so every other client draws output laid out for a grid it does not have. The session server keeps the
//! explicit leadership of upstream's patched zmx (its `0001-explicit-leadership.patch`): a client attached with
//! `NGTERM_LIVE_MANAGED=<nonce>` leads only by claiming at attach (`NGTERM_LIVE_MANAGED_CLAIM`), and reports its role
//! as a terminal title under a reserved prefix carrying the nonce (`ngterm_session::server::lead`).

use std::collections::BTreeMap;

use uuid::Uuid;

/// A pane's role at its daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum LeadRole {
    /// No client leads; the pty keeps its last size until one attaches.
    Unowned,
    Leader,
    Follower,
}

/// The variable the attach client reads to opt into explicit leadership.
pub const NONCE_VARIABLE: &str = "NGTERM_LIVE_MANAGED";
/// The variable that makes the attach claim the lead.
pub const CLAIM_VARIABLE: &str = "NGTERM_LIVE_MANAGED_CLAIM";

/// One attachment of a pane to its daemon: the token its attach client echoes in every role report, and whether it
/// asked for the lead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadAttachment {
    pub nonce: String,
    /// False for the automatic re-attach after the leader left: that one leads only if the session is still unowned
    /// when it arrives, so it cannot take a lead someone claimed in the meantime.
    pub claim: bool,
}

impl LeadAttachment {
    /// An attachment under a fresh nonce.
    #[must_use]
    pub fn new(claim: bool) -> Self {
        Self {
            nonce: make_nonce(),
            claim,
        }
    }

    /// The variables the attach runs with.
    #[must_use]
    pub fn environment(&self) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::from([(NONCE_VARIABLE.to_owned(), self.nonce.clone())]);
        if self.claim {
            environment.insert(CLAIM_VARIABLE.to_owned(), "1".to_owned());
        }
        environment
    }

    /// The same variables as `NAME=value` words for an `env` command line on another machine, sorted by name.
    #[must_use]
    pub fn assignments(&self) -> Vec<String> {
        self.environment()
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect()
    }
}

/// A plain token the attach client accepts: 32 lower-case hex digits.
#[must_use]
pub fn make_nonce() -> String {
    Uuid::new_v4().simple().to_string()
}

/// A role report from a pane's attach client, carried as a terminal title with a reserved prefix. A title because
/// libghostty delivers every one, where it drops a desktop notification that follows another within a second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadNotice {
    pub nonce: String,
    pub role: LeadRole,
    /// Counts the daemon's leadership changes, so a report that arrives late can be told from a newer one.
    pub generation: u32,
}

impl LeadNotice {
    /// The reserved title prefix of the attach contract's role report, which the session server's attach clients
    /// write (`ngterm_session::server::lead::role_title`).
    pub const PREFIX: &'static str = "ngterm-live-role;";

    /// The notice `title` carries; `None` for any other title, and for one that is not
    /// `<prefix><nonce>:<role>:<generation>` under [`Self::PREFIX`].
    #[must_use]
    pub fn parse(title: &str) -> Option<Self> {
        let body = title.strip_prefix(Self::PREFIX)?;
        let fields: Vec<&str> = body.split(':').collect();
        let [nonce, role, generation] = fields.as_slice() else {
            return None;
        };
        if nonce.is_empty() {
            return None;
        }
        Some(Self {
            nonce: (*nonce).to_owned(),
            role: role.parse().ok()?,
            generation: generation.parse().ok()?,
        })
    }
}

/// What one pane knows about its lead, fed by the reports of its current attachment only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadState {
    attachment: LeadAttachment,
    /// `None` until the first report: an unmanaged attach client never sends one, and such a pane behaves as it did
    /// before.
    role: Option<LeadRole>,
    /// True from a fresh attach that replaced a covered surface until its first report, so the cover stays up over a
    /// terminal that has nothing to show yet.
    reattaching: bool,
    generation: u32,
}

impl LeadState {
    #[must_use]
    pub fn new(attachment: LeadAttachment, reattaching: bool) -> Self {
        Self {
            attachment,
            role: None,
            reattaching,
            generation: 0,
        }
    }

    #[must_use]
    pub fn attachment(&self) -> &LeadAttachment {
        &self.attachment
    }

    #[must_use]
    pub fn role(&self) -> Option<LeadRole> {
        self.role
    }

    #[must_use]
    pub fn reattaching(&self) -> bool {
        self.reattaching
    }

    /// Whether the pane draws output laid out for another client's grid, or nothing yet, and must be covered.
    #[must_use]
    pub fn covered(&self) -> bool {
        self.reattaching || matches!(self.role, Some(LeadRole::Follower | LeadRole::Unowned))
    }

    /// Applies `notice` and says whether the role changed. A report for another attachment's nonce is forged or left
    /// over from a surface this pane replaced; an older generation lost a race.
    pub fn apply(&mut self, notice: &LeadNotice) -> bool {
        if notice.nonce != self.attachment.nonce {
            return false;
        }
        if self.role.is_some() && notice.generation < self.generation {
            return false;
        }
        self.generation = notice.generation;
        // a re-attach starts with no role, so its first report always changes it
        if self.role == Some(notice.role) {
            return false;
        }
        self.reattaching = false;
        self.role = Some(notice.role);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(nonce: &str, claim: bool) -> LeadAttachment {
        LeadAttachment {
            nonce: nonce.to_owned(),
            claim,
        }
    }

    fn notice(title: &str) -> LeadNotice {
        LeadNotice::parse(title).unwrap()
    }

    #[test]
    fn the_attachment_environment_carries_the_claim_only_when_asked() {
        let claiming = attachment("n1", true);
        assert_eq!(
            claiming.environment(),
            BTreeMap::from([
                ("NGTERM_LIVE_MANAGED".to_owned(), "n1".to_owned()),
                ("NGTERM_LIVE_MANAGED_CLAIM".to_owned(), "1".to_owned())
            ])
        );
        assert_eq!(
            claiming.assignments(),
            ["NGTERM_LIVE_MANAGED=n1", "NGTERM_LIVE_MANAGED_CLAIM=1"]
        );
        assert_eq!(
            attachment("n2", false).environment(),
            BTreeMap::from([("NGTERM_LIVE_MANAGED".to_owned(), "n2".to_owned())])
        );
    }

    #[test]
    fn a_generated_nonce_is_a_plain_token() {
        let nonce = make_nonce();
        assert_eq!(nonce.len(), 32);
        assert!(
            nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
        );
        assert_ne!(LeadAttachment::new(true).nonce, LeadAttachment::new(true).nonce);
    }

    #[test]
    fn a_notice_parses_the_reserved_title() {
        for (title, role, generation) in [
            ("ngterm-live-role;n1:leader:7", LeadRole::Leader, 7),
            ("ngterm-live-role;n1:follower:0", LeadRole::Follower, 0),
            ("ngterm-live-role;n1:unowned:4294967295", LeadRole::Unowned, u32::MAX),
        ] {
            assert_eq!(
                LeadNotice::parse(title),
                Some(LeadNotice {
                    nonce: "n1".to_owned(),
                    role,
                    generation
                })
            );
        }
    }

    #[test]
    fn a_notice_leaves_every_other_title_alone() {
        for title in [
            "vim README.md",
            "ngterm-live-role",
            "my ngterm-live-role;n1:leader:7",
            "ngterm-live-role;n1:leader",
            "ngterm-live-role;n1:owner:7",
            "ngterm-live-role;n1:leader:-1",
            "ngterm-live-role;n1:leader:4294967296",
            "ngterm-live-role;:leader:7",
            "ngterm-live-role;n1:leader:7:extra",
            // upstream's patched zmx's prefix: no live session reports under it any more
            "zmx-role;n1:leader:7",
            // this build's prefix before it took the name ngterm
            "agterm-live-role;n1:leader:7",
        ] {
            assert_eq!(LeadNotice::parse(title), None, "{title}");
        }
    }

    #[test]
    fn the_state_stays_unknown_until_the_first_report() {
        let mut state = LeadState::new(attachment("n1", true), false);
        assert_eq!(state.role(), None);
        assert!(!state.covered());
        assert!(state.apply(&notice("ngterm-live-role;n1:follower:3")));
        assert!(state.covered());
        assert!(state.apply(&notice("ngterm-live-role;n1:leader:4")));
        assert!(!state.covered());
    }

    #[test]
    fn the_state_drops_another_attachments_report_and_an_older_generation() {
        let mut state = LeadState::new(attachment("n1", true), false);
        assert!(state.apply(&notice("ngterm-live-role;n1:leader:5")));
        assert!(!state.apply(&notice("ngterm-live-role;forged:follower:9")));
        assert!(!state.apply(&notice("ngterm-live-role;n1:follower:4")));
        assert!(!state.apply(&notice("ngterm-live-role;n1:leader:6")));
        assert_eq!(state.role(), Some(LeadRole::Leader));
    }

    #[test]
    fn reattaching_stays_covered_until_its_first_report_even_when_the_role_repeats() {
        let mut state = LeadState::new(attachment("n2", true), true);
        assert!(state.covered());
        assert!(state.apply(&notice("ngterm-live-role;n2:leader:8")));
        assert!(!state.reattaching());
        assert!(!state.covered());
    }
}
