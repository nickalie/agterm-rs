//! agterm's GPUI frontend: the `agterm-gpui` binary until it replaces the GTK one (plan
//! `docs/plans/20261014-gpui-frontend.md`). A library beside the binary so its integration tests and the tasks that
//! wire it up reach the modules.
//!
//! GPUI owns the main thread and the whole state ([`state::AppState`], a GPUI entity); control connections, the
//! single-instance bus and the signal thread hop onto it through [`main_loop::GpuiMainLoop`].

pub mod activation;
pub mod app;
pub mod external_texture;
pub mod instance;
pub mod main_loop;
pub mod signals;
pub mod state;
pub mod terminal;
