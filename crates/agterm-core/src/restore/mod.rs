//! Restore modes and relaunch: the launch decision frozen for the process ([`mode`]), a pane's foreground program and
//! the restore capture ([`capture`]), what a relaunched pane spawns with and in which order ([`spawn_plan`]), and the
//! pacing of those spawns ([`pacer`]).

pub mod capture;
pub mod mode;
pub mod pacer;
pub mod spawn_plan;
