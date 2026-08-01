//! The headless functional core.
//!
//! This module is the home of the **contract vocabulary** for the
//! functional-core / thin-shell architecture (see [`docs/CORE_SHELL.md`]):
//!
//! - [`Intent`] — the only way the UI shell changes anything. Today it is an
//!   alias of [`crate::core::Intent`]; as direct `&mut` mutations in the UI
//!   are folded into the command vocabulary it becomes the superset the roadmap
//!   describes.
//! - [`Effect`] — a *described* side effect the core returns for the shell to
//!   execute (URL push, localStorage write, geolocation, …). Modeled on the
//!   existing [`crate::core::playback_manager::PrevSweepAction`] "effect as
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

pub(crate) mod acquisition;
pub(crate) mod canvas;
pub(crate) mod diagnostics;
pub(crate) mod domain;
pub(crate) mod effect;
pub(crate) mod geo_layers;
pub(crate) mod geocode;
pub(crate) mod intent;
pub(crate) mod live_mode;
pub(crate) mod live_radar_model;
pub(crate) mod panels;
pub(crate) mod persist;
pub(crate) mod playback_manager;
pub(crate) mod projection;
pub(crate) mod render;
pub(crate) mod render_loop;
pub(crate) mod streaming_filter;
pub(crate) mod streaming_plan;
pub(crate) mod timeline_geometry;
pub(crate) mod timeline_interaction;
pub(crate) mod timeline_view;
pub(crate) mod timing;
pub(crate) mod transport;
pub(crate) mod worker_decoded;
pub(crate) mod worker_ingest;

pub(crate) use acquisition::{selection_span_allowed, MAX_SELECTION_SPAN_SECS};
#[allow(unused_imports)]
pub(crate) use domain::errors::TimestampedError;
pub(crate) use domain::errors::{AppError, ErrorContext, WorkerErrorKind};
pub(crate) use domain::feeds::{AlertsState, GpsState, MpingState};
pub(crate) use domain::forecast::{
    derive_volume_forecast, BucketKey, ChunkArrivalStat, CompletedVolumeRecord,
    ForecastTimingLabel, SweepForecast, SweepStatus, VolumeForecastSnapshot, WaitResolution,
};
// Consumed only by test modules today; in a bin crate that reads as unused.
#[allow(unused_imports)]
pub(crate) use domain::forecast::RateSource;
pub(crate) use domain::ops::OperationId;
pub(crate) use domain::playback::{
    format_lag, macro_frame_bounds, BucketGranularity, FreezeAt, LoopBasis, LoopMode, LoopPreset,
    MacroFrameInputs, MacroPlaybackState, PlaybackDirection, PlaybackMode, PlaybackSpeed,
    PlaybackState, PlayheadMode, RebuildCause, TimeModel, TimeSelection, TimelineTier,
    TIMELINE_ZOOM_MAX,
};
// Consumed only by test modules today; in a bin crate that reads as unused.
#[allow(unused_imports)]
pub(crate) use domain::playback::LoopWindow;
pub(crate) use domain::prefs::UserPreferences;
pub(crate) use domain::radar::{RadarTimeline, Scan, ScanBoundary, ScanMetadata, Sweep};
#[allow(unused_imports)]
pub(crate) use domain::radar::{Radial, TimeRange};
pub(crate) use domain::telemetry::{NetworkAggregate, NetworkRequest};
pub(crate) use domain::time::FrameNow;
pub(crate) use domain::view::ViewState;
pub(crate) use domain::viz::{
    build_elevation_list_from_vcp, DisplayedSweep, ElevationListEntry, ElevationSelection,
    InterpolationMode, RadarProduct, RenderProcessing, StormCellInfo, SweepIdentity,
};
pub(crate) use domain::volume::VolumeElevationRoster;
pub(crate) use domain::worker::{
    CacheLoadResult, ChunkIngestContext, ChunkIngestResult, DecodeResult, IngestContext,
    IngestResult, RenderContext, VolumeData, VolumeSweepMeta,
};
pub(crate) use effect::{Effect, LocationResult};
pub(crate) use geo_layers::GeoLayer;
pub(crate) use live_mode::{
    should_stop_for_detached_idle, LiveExitReason, LiveModeState, LivePhase, StreamActivity,
};
pub(crate) use live_radar_model::LiveRadarModel;
pub(crate) use persist::{decide_persist, persist_due, PersistDecision};
pub(crate) use streaming_filter::StreamingFilter;
pub(crate) use streaming_plan::{ChunkProjectedTimes, ChunkProjectionInfo, StreamingPlan};
pub(crate) use timeline_geometry::{
    block_extent, cell_gap_px, layout_block, should_draw_sweep_texture, MIN_BLOCK_W,
};
pub(crate) use timeline_interaction::{
    clamp_view_start, resolve_gesture, StripGesture, StripInput, NEXRAD_ARCHIVE_START_SECS,
};
pub(crate) use timeline_view::{
    FrameCell, FrameCellState, FrameJoinInputs, ScanContainer, TimelineView,
    SCAN_JOIN_TOLERANCE_SECS,
};

// `Intent` is consumed starting P5; in a bin crate the re-export reads as unused
// until the first consumer lands.
#[allow(unused_imports)]
pub(crate) use intent::Intent;
