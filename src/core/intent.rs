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

use crate::core::{GeoLayer, LoopPreset, OperationId};

/// Intents dispatched by UI code and consumed by the main update loop.
///
/// Replaces scattered boolean `*_requested` flags with an explicit command queue,
/// making state transitions easier to follow and impossible to forget to clear.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Intent {
    /// Refresh the timeline from the cache. Optionally auto-position the cursor.
    RefreshTimeline { auto_position: bool },
    /// Clear the record cache.
    ClearCache,
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
    /// Apply a loop preset (spec §8): pin-to-live, last N frames, or a duration
    /// window. Resolved into a [`crate::core::LoopWindow`] + the right playhead transition.
    ApplyLoopPreset(LoopPreset),
    /// Clear any active loop (selection bounds / pinned replay).
    ClearLoop,
    /// Check and run eviction after a storage operation.
    CheckEviction,
    /// Wipe all data (IndexedDB + localStorage) and reload.
    WipeAll,
    /// Pause the acquisition queue.
    PauseQueue,
    /// Resume the acquisition queue.
    ResumeQueue,
    /// Open or close the activity sheet.
    SetActivitySheetOpen(bool),
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
    /// Switch one map overlay on or off. Carries the new value (not a flip) so
    /// a repeated intent is idempotent.
    SetGeoLayer(GeoLayer, bool),
}
