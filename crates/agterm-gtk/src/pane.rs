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

    /// A live pane is a client of its daemon once its terminal exists: the attach runs from the surface's creation,
    /// so a pane still waiting for its first size reads as not attached yet.
    fn is_live_session(&self) -> bool {
        self.terminal.is_live() && self.terminal.is_realized()
    }

    fn promote_to_primary(&mut self) {}

    fn type_text(&mut self, text: &str) -> bool {
        self.terminal.type_text(text)
    }

    fn read_text(&self, scope: TextScope) -> Option<String> {
        self.terminal.read_text(scope)
    }

    fn read_selection(&self) -> Option<String> {
        self.terminal.read_selection()
    }

    fn cursor_column(&self) -> Option<u32> {
        self.terminal.cursor_column()
    }

    fn binding_action(&mut self, action: &str) -> bool {
        self.terminal.binding_action(action)
    }

    fn foreground_pid(&self) -> Option<u32> {
        self.terminal.foreground_pid()
    }

    fn font_size(&self) -> Option<f64> {
        self.terminal.font_size()
    }

    fn cell_size(&self) -> Option<(f64, f64)> {
        self.terminal.cell_size()
    }

    fn teardown(self: Box<Self>) {
        self.terminal.teardown();
    }
}
