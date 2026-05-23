//! Frame-loop subsystems extracted from `main.rs`.
//!
//! Each submodule contributes additional `impl WorkbenchApp` methods that
//! the main `update()` orchestration loop dispatches to. Splitting these
//! out keeps `main.rs` focused on the high-level frame sequence and lets
//! each subsystem be read in isolation.

pub(crate) mod command_dispatch;
pub(crate) mod frame_setup;
