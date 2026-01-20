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

    /// State for the datetime picker popup.
    pub datetime_picker: DateTimePickerState,
}

/// State for the datetime jump picker popup.
#[derive(Default)]
pub struct DateTimePickerState {
    /// Whether the picker popup is currently open.
    pub open: bool,
    /// Input values for the picker (as strings for text editing).
    pub year: String,
    pub month: String,
    pub day: String,
    pub hour: String,
    pub minute: String,
    pub second: String,
}

impl DateTimePickerState {
    /// Initialize the picker with a timestamp.
    pub fn init_from_timestamp(&mut self, ts: f64) {
        use chrono::{TimeZone, Utc};
        let dt = Utc.timestamp_opt(ts as i64, 0).unwrap();
        self.year = dt.format("%Y").to_string();
        self.month = dt.format("%m").to_string();
        self.day = dt.format("%d").to_string();
        self.hour = dt.format("%H").to_string();
        self.minute = dt.format("%M").to_string();
        self.second = dt.format("%S").to_string();
        self.open = true;
    }

    /// Try to parse the current input values into a timestamp.
    pub fn to_timestamp(&self) -> Option<f64> {
        let year: i32 = self.year.parse().ok()?;
        let month: u32 = self.month.parse().ok()?;
        let day: u32 = self.day.parse().ok()?;
        let hour: u32 = self.hour.parse().ok()?;
        let minute: u32 = self.minute.parse().ok()?;
        let second: u32 = self.second.parse().ok()?;

        use chrono::{TimeZone, Utc};
        let dt = Utc.with_ymd_and_hms(year, month, day, hour, minute, second);
        match dt {
            chrono::LocalResult::Single(dt) => Some(dt.timestamp() as f64),
            _ => None,
        }
    }

    /// Close the picker and reset state.
    pub fn close(&mut self) {
        self.open = false;
    }
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
