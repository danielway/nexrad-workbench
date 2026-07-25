//! Subsystems: bounded contexts that own a coherent slice of state +
//! behavior with a typed external API.
//!
//! Each subsystem replaces a scattering of fields previously split between
//! [`crate::state::AppState`] and [`crate::WorkbenchApp`]. See `ARCHITECTURE.md`
//! for the subsystem map and `docs/CORE_SHELL.md` for the functional-core /
//! thin-shell standard these subsystems serve.
//!
//! Migration is staged one subsystem at a time. The first one extracted
//! is [`acquisition::Acquisition`] (the messiest split — state lived on
//! `AppState`, channels lived on `WorkbenchApp`, the two had to be kept
//! in sync by every caller).

pub(crate) mod acquisition;
pub(crate) mod chrome;
pub(crate) mod derived;
pub(crate) mod diagnostics;
pub(crate) mod live;
pub(crate) mod network_monitor;
pub(crate) mod playback;
pub(crate) mod render;
pub(crate) mod timeline;

pub(crate) use acquisition::Acquisition;
pub(crate) use chrome::Chrome;
pub(crate) use derived::Derived;
pub(crate) use diagnostics::Diagnostics;
pub(crate) use live::Live;
pub(crate) use network_monitor::NetworkMonitor;
pub(crate) use playback::Playback;
pub(crate) use render::Render;
pub(crate) use timeline::Timeline;
