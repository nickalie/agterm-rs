//! Restore modes and relaunch: the launch decision frozen for the process ([`mode`]), a pane's foreground program and
//! the restore capture ([`capture`]), and what a relaunched pane spawns with ([`spawn_plan`]).

pub mod capture;
pub mod mode;
pub mod spawn_plan;
