//! Application state management.
//!
//! This module contains all state structures used throughout the application.
//! State is organized into logical groupings that correspond to different
//! areas of functionality.

mod alerts;
mod data_source;
mod layer;
mod live_mode;
mod playback;
pub mod radar_data;
mod stats;
pub mod vcp;
mod viz;

pub use alerts::{AlertSummary, AlertsState, NwsAlert};
pub use data_source::UploadState;
pub use layer::{GeoLayerVisibility, LayerState};
pub use live_mode::{LiveExitReason, LiveModeState, LivePhase};
pub use playback::{PlaybackSpeed, PlaybackState};
pub use radar_data::RadarTimeline;
pub use stats::SessionStats;
pub use vcp::get_vcp_definition;
pub use viz::{ColorPalette, ProcessingState, RadarProduct, VizState};

/// Root application state containing all sub-states.
#[derive(Default)]
pub struct AppState {
    /// State for file upload
    pub upload_state: UploadState,

    /// Playback controls state
    pub playback_state: PlaybackState,

    /// Radar timeline data (scans, sweeps, radials)
    pub radar_timeline: RadarTimeline,

    /// Visualization state (canvas, zoom/pan, product selection)
    pub viz_state: VizState,

    /// Layer visibility toggles
    pub layer_state: LayerState,

    /// Processing options
    pub processing_state: ProcessingState,

    /// Application status message displayed in top bar
    pub status_message: String,

    /// Session and performance statistics
    pub session_stats: SessionStats,

    /// NWS weather alerts
    pub alerts_state: AlertsState,

    /// Live streaming mode state
    pub live_mode_state: LiveModeState,

    /// Selected date for AWS archive download
    pub archive_date: Option<chrono::NaiveDate>,

    /// Whether an archive download is in progress
    pub download_in_progress: bool,

    /// Flag to signal that the timeline needs to be refreshed from cache.
    /// Set to true when the site changes or after a download completes.
    pub timeline_needs_refresh: bool,

    /// Flag to signal that the cache should be cleared.
    /// Set by UI, handled in main update loop.
    pub clear_cache_requested: bool,

    /// Flag to signal that scans in the selected range should be downloaded.
    /// Set by UI, handled in main update loop.
    pub download_selection_requested: bool,

    /// Whether a selection download is currently in progress.
    pub download_selection_in_progress: bool,
}

impl AppState {
    pub fn new() -> Self {
        // Use current time for initialization
        #[cfg(target_arch = "wasm32")]
        let now = js_sys::Date::now() / 1000.0;
        #[cfg(not(target_arch = "wasm32"))]
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        // Start with empty timeline - will be populated from cache
        let radar_timeline = RadarTimeline::default();

        // Set up playback state centered on "now"
        let playback_state = PlaybackState::new_at_time(now);

        Self {
            playback_state,
            radar_timeline,
            status_message: "Ready".to_string(),
            session_stats: SessionStats::new(),
            alerts_state: AlertsState::with_dummy_data(),
            // Request timeline refresh on startup to load from cache
            timeline_needs_refresh: true,
            ..Default::default()
        }
    }
}
