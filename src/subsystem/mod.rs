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

pub mod acquisition;
pub mod chrome;
pub mod derived;
pub mod diagnostics;
pub mod live;
pub mod playback;
pub mod render;
pub mod timeline;

pub use acquisition::Acquisition;
pub use chrome::Chrome;
pub use derived::Derived;
pub use diagnostics::Diagnostics;
pub use live::Live;
pub use playback::Playback;
pub use render::Render;
pub use timeline::Timeline;
