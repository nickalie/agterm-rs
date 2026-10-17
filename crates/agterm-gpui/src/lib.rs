//! agterm's GPUI frontend: the `agterm-gpui` binary until it replaces the GTK one (plan
//! `docs/plans/20261014-gpui-frontend.md`). A library beside the binary so its integration tests and the tasks that
//! wire it up reach the modules.
//!
//! GPUI owns the main thread and the whole state ([`state::AppState`], a GPUI entity); control connections, the
//! single-instance bus and the signal thread hop onto it through [`main_loop::GpuiMainLoop`].

pub mod activation;
pub mod app;
pub mod app_menu;
pub mod appearance;
pub mod ask_dialog;
pub mod chrome;
pub mod dashboard;
pub mod deck;
pub mod external_texture;
pub mod folder_chooser;
pub mod html_overlay;
pub mod hud;
pub mod instance;
pub mod integrations;
pub mod keymap_actions;
pub mod keymap_dispatch;
pub mod main_loop;
pub mod modal;
pub mod mute;
pub mod notifications;
pub mod overlay;
pub mod palette;
pub mod pane;
pub mod pane_lead;
pub mod picker;
pub mod popovers;
pub mod quick;
pub mod remote;
pub mod remotes_watcher;
pub mod row_icon;
pub mod search;
pub mod session_host;
pub mod session_switcher;
pub mod settings;
pub mod sidebar;
pub mod signals;
pub mod split;
pub mod state;
pub mod status_glyph;
pub mod status_sound;
pub mod terminal;
pub mod titlebar;
#[cfg(test)]
mod ui_ids;
pub mod watermark;
pub mod welcome;
pub mod window;
pub mod window_frames;
pub mod windows;
pub mod zmx_runtime;
