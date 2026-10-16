//! Intents — the only thing the UI shell sends into the core.
//!
//! An *intent* is a description of what the user is trying to do
//! ("seek to this time", "open this alert", "toggle this layer"). The core
//! turns intents into state changes and [`Effect`](super::Effect)s; the shell
//! never mutates state or performs I/O on its own.
//!
//! The migration folds the UI's remaining direct `&mut` mutations into this
//! vocabulary (P5), at which point `Intent` is a strict superset of the
//! original command set. (Formerly `crate::core::Intent`; the definition moved
//! here so the contract's vocabulary lives in the core.)

use crate::core::transport::{LoopEdge, SeekGesture, StepDir, StepUnit};
use crate::core::{GeoLayer, LoopMode, LoopPreset, OperationId, PlaybackMode, PlaybackSpeed};

/// Intents dispatched by UI code and consumed by the main update loop.
///
/// Replaces scattered boolean `*_requested` flags with an explicit command queue,
/// making state transitions easier to follow and impossible to forget to clear.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Intent {
    /// Refresh the timeline from the cache. Optionally auto-position the cursor.
    RefreshTimeline { auto_position: bool },
    /// Ask to clear the record cache: opens the destructive confirmation
    /// ([`crate::core::dialogs::Confirm::ClearCache`]).
    ClearCache,
    /// Clear the record cache now (the confirmation was accepted).
    ClearCacheNow,
    /// Start live/real-time streaming. The plumbing-level "open a stream"
    /// intent used by boot/deep-link paths; the user-facing GO LIVE button is
    /// [`Intent::GoLive`].
    StartLive,
    /// The user pressed GO LIVE (now-cap, transport LIVE button, mobile LIVE
    /// button): drop any selection, start streaming from now, and pace at
    /// realtime speed.
    GoLive,
    /// The user pressed a stop-the-stream control while tethered (the timeline
    /// now-cap, the mobile LIVE button): stop streaming and drop to ARCHIVE.
    /// The payload says where that leaves the playhead; decided by
    /// [`crate::core::transport::reduce_stop_live`].
    StopLive(crate::core::transport::LiveStopPlacement),
    /// Re-pin the playhead to the live edge. Instant when the stream is
    /// already running (detached browsing); otherwise starts a stream.
    ReturnToLive,
    /// Scroll the timeline view so the live edge ("now") is centered. Emitted
    /// by the off-screen now-chip before it attaches to live.
    CenterTimelineOnNow,
    /// Play/pause per the current mode (archive playback, freeze while
    /// tethered, or re-pin from a lookback replay). Decided by
    /// [`crate::core::transport::reduce_toggle_play_pause`].
    TogglePlayPause,
    /// Loop the last N frames or a duration ending now (spec §8): a range
    /// attached to the live edge, starting a stream if none runs.
    ApplyLoopPreset(LoopPreset),
    /// Clear any active loop (selection bounds / pinned replay).
    ClearLoop,
    /// Stop looping and stay at the playhead as a fixed point. Decided by
    /// [`crate::core::transport::reduce_stop_looping`].
    StopLooping,
    /// Select a range: attached to the live edge when `to_live` and a stream
    /// runs, otherwise fixed (see [`crate::core::transport::select_range`]).
    SelectRange { start: f64, end: f64, to_live: bool },
    /// Move the playhead to `ts` (the timeline, the playhead clock). Keeps a
    /// range it lands inside; a scrub pauses. Decided by
    /// [`crate::core::transport::reduce_seek`].
    Seek { ts: f64, gesture: SeekGesture },
    /// Step one frame or volume (the step buttons, ← →, Shift+← →, the
    /// phone's jog buttons), wrapping only inside a range. Decided by
    /// [`crate::core::transport::reduce_step`].
    Step { dir: StepDir, unit: StepUnit },
    /// Put the range's in (I) or out (O) point at the playhead. Decided by
    /// [`crate::core::transport::reduce_loop_point`].
    SetLoopPoint(LoopEdge),
    /// Choose how a range loops (the chip menu's loop style).
    SetLoopStyle(LoopMode),
    /// Choose a playback speed (the transport's speed menu). A no-op while
    /// pinned to now in Rays.
    SetSpeed(PlaybackSpeed),
    /// One speed rung faster or slower ([ / ]).
    StepSpeed { faster: bool },
    /// Open the event dialog for the selected range (the chip menu's "Save
    /// as event"). A no-op without a range.
    SaveSelectionAsEvent,
    /// Choose how playback advances: Frames (whole sweeps at an fps) or Rays
    /// (continuous time at a realtime multiple). Independent of zoom.
    SetPlayMode(PlaybackMode),
    /// Check and run eviction after a storage operation.
    CheckEviction,
    /// Ask to reset settings: opens the destructive confirmation
    /// ([`crate::core::dialogs::Confirm::ResetSettings`]). The Storage
    /// section still raises `Chrome::reset_settings_modal_open` (folded into
    /// the same confirmation) until it moves to this intent.
    #[cfg_attr(not(test), allow(dead_code))]
    ResetSettings,
    /// Reset local settings and preferences while preserving saved events,
    /// then reload (the confirmation was accepted).
    ResetSettingsNow,
    /// Pause the acquisition queue.
    PauseQueue,
    /// Resume the acquisition queue.
    ResumeQueue,
    /// Open or close the activity sheet.
    SetActivitySheetOpen(bool),
    /// Expand or collapse the activity sheet's Details disclosure.
    SetActivityDetailsOpen(bool),
    /// Retry every failed download at once. A single intent rather than a UI
    /// loop over `RetryFailed`, so "which failures are retryable" stays a core
    /// decision instead of leaking into the shell.
    RetryAllFailed,
    /// Data-saving policy: auto-fetch scans while scrubbing.
    SetAutofetchWhileScrubbing(bool),
    /// Data-saving policy: stop the live stream when the playhead detaches.
    SetPauseStreamWhileReviewing(bool),
    /// Retry a failed operation.
    RetryFailed(OperationId),
    /// Explicitly fetch one archive scan (scan inspector's tap-to-fetch).
    /// `elevation_filter = Some(n)` scopes the decode/store to one tilt
    /// ("fetch this sweep"); `None` fetches the whole volume ("fetch whole
    /// scan"). `scan_start` is the scan's start time in Unix seconds.
    FetchScan {
        scan_start: i64,
        elevation_filter: Option<u8>,
    },
    /// Skip a failed operation and continue.
    SkipFailed(OperationId),
    /// Cancel a specific operation.
    CancelOperation(OperationId),
    /// Reorder an operation (delta: -1 = up, +1 = down).
    ReorderOperation(OperationId, isize),
    /// Retry initializing the decode worker after a failure.
    RetryWorker,
    /// An intent for the diagnostics overlays (NWS alerts / mPING / GPS). The
    /// alerts/mPING/GPS UI emits these instead of mutating overlay state; the
    /// main loop applies them through the pure
    /// [`crate::core::diagnostics::reduce`].
    Diagnostics(crate::core::diagnostics::DiagnosticsIntent),
    /// Open an external link (the version pill's changelog URL).
    OpenExternalUrl(&'static str),
    /// The site modal's "Use My Location" button (and the mobile action
    /// bar's location button): open the modal pending and geolocate.
    LocateMeForSite,
    /// The site modal's zip form was submitted with this raw field text.
    /// Validation is the core's ([`crate::core::geocode::decide_zip_submission`]);
    /// the shell either starts the lookup or shows the message.
    SubmitZip(String),
    /// "Show on map" for an alert: enable its overlay class and center the 2D
    /// view on its bbox. Cross-cuts diagnostics + viz, so it's handled in the
    /// shell (with viz access) via the pure `compute_alert_focus`.
    ShowAlertOnMap(String),
    /// Make `site_id` the active radar: retarget viz, recenter the camera,
    /// remember it as the preferred site, close the site modal, and refresh
    /// the timeline + alerts. Emitted by every site-picking surface (the
    /// modal's list, the zip/geolocation result, and a canvas click on a site
    /// marker).
    SelectSite { site_id: String, lat: f64, lon: f64 },
    /// A distance-tool click on the canvas, in geographic coordinates. Which
    /// endpoint it lands on is decided by
    /// [`crate::core::canvas::decide_distance_click`].
    PlaceDistancePoint { lat: f64, lon: f64 },
    /// A button of the map's corner toolbar (data probe, distance). Decided
    /// by [`crate::core::panels::toggle_map_tool`].
    ToggleMapTool(crate::core::panels::MapTool),
    /// Switch one map overlay on or off. Carries the new value (not a flip) so
    /// a repeated intent is idempotent.
    SetGeoLayer(GeoLayer, bool),
    /// A top-bar picker, toggle or the toast under it (one-instrument §9).
    TopBar(crate::core::top_bar::TopBarIntent),
    /// A dialog intent: Escape, close, confirm, the event dialog's writes
    /// (one-instrument §12, [`crate::core::dialogs`]).
    Dialog(crate::core::dialogs::DialogIntent),
    /// "Loop from here → now" on the scan inspector: a range from this scan
    /// start to now, the playhead at the scan. Closes the inspector only when
    /// the range was made.
    LoopFromScan(f64),
}
