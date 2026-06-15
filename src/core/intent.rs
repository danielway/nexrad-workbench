//! Intents — the only thing the UI shell sends into the core.
//!
//! An *intent* is a description of what the user is trying to do
//! ("seek to this time", "open this alert", "toggle this layer"). The core
//! turns intents into state changes and [`Effect`](super::Effect)s; the shell
//! never mutates state or performs I/O on its own.
//!
//! Today the vocabulary is [`crate::state::AppCommand`]. The migration folds the
//! UI's remaining direct `&mut` mutations into this vocabulary (P5), at which
//! point `Intent` is a strict superset of the original command set. Aliasing
//! rather than renaming keeps the ~existing call sites untouched while
//! establishing the contract name.
pub use crate::state::AppCommand as Intent;
