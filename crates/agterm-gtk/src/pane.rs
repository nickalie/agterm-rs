//! A session pane's terminal as the core's [`Surface`]: what the model reads back through its surface slot.

use agterm_host::{Surface, TextScope};

use crate::surface::TerminalSurface;

/// A [`TerminalSurface`] in a session's pane slot, with the pane token baked into its shell as `AGTERM_PANE_ID`.
pub struct PaneSurface {
    terminal: TerminalSurface,
    token: String,
}

impl PaneSurface {
    #[must_use]
    pub fn new(terminal: TerminalSurface, token: String) -> Self {
        Self { terminal, token }
    }
}

impl Surface for PaneSurface {
    fn pane_token(&self) -> &str {
        &self.token
    }

    fn is_realized(&self) -> bool {
        self.terminal.is_realized()
    }

    fn is_live_session(&self) -> bool {
        false
    }

    fn promote_to_primary(&mut self) {}

    // typing into and reading a terminal come with the terminal I/O commands
    fn type_text(&mut self, _text: &str) {}

    fn read_text(&self, _scope: TextScope) -> Option<String> {
        None
    }

    fn teardown(self: Box<Self>) {
        self.terminal.teardown();
    }
}
