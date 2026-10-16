//! agterm's GPUI frontend: the `agterm-gpui` binary until it replaces the GTK one (plan
//! `docs/plans/20261014-gpui-frontend.md`). A library beside the binary so its integration tests and the tasks that
//! wire it up reach the modules.
//!
//! GPUI owns the main thread and the whole state ([`state::AppState`], a GPUI entity); control connections, the
//! single-instance bus and the signal thread hop onto it through [`main_loop::GpuiMainLoop`].

pub mod activation;
pub mod app;
pub mod appearance;
pub mod ask_dialog;
pub mod chrome;
pub mod deck;
pub mod external_texture;
pub mod hud;
pub mod instance;
pub mod keymap_actions;
pub mod keymap_dispatch;
pub mod main_loop;
pub mod modal;
pub mod mute;
pub mod palette;
pub mod pane;
pub mod pane_lead;
pub mod picker;
pub mod popovers;
pub mod row_icon;
pub mod search;
pub mod session_host;
pub mod session_switcher;
pub mod sidebar;
pub mod signals;
pub mod split;
pub mod state;
pub mod status_glyph;
pub mod terminal;
pub mod titlebar;
pub mod watermark;
pub mod window;
pub mod windows;
