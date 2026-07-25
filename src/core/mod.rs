//! The headless functional core.
//!
//! This module is the home of the **contract vocabulary** for the
//! functional-core / thin-shell architecture (see [`docs/CORE_SHELL.md`]):
//!
//! - [`Intent`] — the only way the UI shell changes anything. Today it is an
//!   alias of [`crate::state::AppCommand`]; as direct `&mut` mutations in the UI
//!   are folded into the command vocabulary it becomes the superset the roadmap
//!   describes.
//! - [`Effect`] — a *described* side effect the core returns for the shell to
//!   execute (URL push, localStorage write, geolocation, …). Modeled on the
//!   existing [`crate::state::playback_manager::PrevSweepAction`] "effect as
//!   data" enum. Heavy, per-decision effects (GPU buffer uploads, worker
//!   dispatch) keep using their own local action enums — that *is* the
//!   PrevSweepAction idiom — while [`Effect`] carries the simple cross-cutting
//!   effects that benefit from a single shared executor.
//!
//! The decision functions that make up the core proper live next to the state
//! they read (`src/state/**` is already pure and unit-tested); this module
//! re-exports the ones routed through the contract so there is one canonical
//! "this is the core" surface. New decision logic extracted from the UI/`app`
//! orchestration during the migration lands here as `decide_*` / `reduce`
//! functions.

pub mod acquisition;
pub mod canvas;
pub mod diagnostics;
pub mod domain;
pub mod effect;
pub mod intent;
pub mod panels;
pub mod persist;
pub mod render;

pub use domain::forecast::{
    derive_volume_forecast, BucketKey, ChunkArrivalStat, CompletedVolumeRecord,
    ForecastTimingLabel, SweepForecast, SweepStatus, VolumeForecastSnapshot, WaitResolution,
};
// Consumed only by test modules today; in a bin crate that reads as unused.
#[allow(unused_imports)]
pub use domain::forecast::RateSource;
pub use domain::ops::OperationId;
pub use domain::prefs::UserPreferences;
pub use domain::radar::{RadarTimeline, Scan, ScanMetadata, Sweep};
#[allow(unused_imports)]
pub use domain::radar::{Radial, TimeRange};
pub use domain::time::FrameNow;
pub use domain::view::ViewState;
pub use domain::viz::{
    DisplayedSweep, ElevationListEntry, ElevationSelection, InterpolationMode, RadarProduct,
    RenderProcessing, StormCellInfo, SweepIdentity,
};
pub use domain::volume::VolumeElevationRoster;
pub use effect::{Effect, LocationResult};
pub use persist::{decide_persist, PersistDecision};

// `Intent` is consumed starting P5; in a bin crate the re-export reads as unused
// until the first consumer lands.
#[allow(unused_imports)]
pub use intent::Intent;
