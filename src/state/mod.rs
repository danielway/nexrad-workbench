//! Application state management.
//!
//! This module contains all state structures used throughout the application.
//! State is organized into logical groupings that correspond to different
//! areas of functionality.

use crate::data::keys::ScanKey;

mod layer;
mod live_mode;
#[allow(dead_code)]
mod playback;
mod preferences;
pub mod radar_data;
#[allow(dead_code)]
mod settings;
#[allow(dead_code)]
mod stats;
pub mod theme;
pub mod url_state;
pub mod vcp;
#[allow(dead_code)]
mod viz;

pub use layer::{GeoLayerVisibility, LayerState};
pub use live_mode::{LiveExitReason, LiveModeState, LivePhase};
pub use playback::{LoopMode, PlaybackSpeed, PlaybackState};
pub use preferences::UserPreferences;
pub use radar_data::RadarTimeline;
pub use settings::{format_bytes, StorageSettings};
pub use stats::SessionStats;
pub use theme::ThemeMode;
pub use vcp::get_vcp_definition;
pub use viz::{InterpolationMode, RadarProduct, RenderMode, RenderProcessing, VizState};

/// Root application state containing all sub-states.
#[derive(Default)]
pub struct AppState {
    /// Playback controls state
    pub playback_state: PlaybackState,

    /// Radar timeline data (scans, sweeps, radials)
    pub radar_timeline: RadarTimeline,

    /// Visualization state (canvas, zoom/pan, product selection)
    pub viz_state: VizState,

    /// Layer visibility toggles
    pub layer_state: LayerState,

    /// Application status message displayed in top bar
    pub status_message: String,

    /// Session and performance statistics
    pub session_stats: SessionStats,

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

    /// Flag to signal that live mode should be started.
    /// Set by UI, handled in main update loop.
    pub start_live_requested: bool,

    /// Pending partial volume decode request (timestamp_ms, scan_key).
    /// Set when a PartialVolumeReady event is received, processed in update loop.
    pub pending_partial_decode: Option<(i64, ScanKey)>,

    /// Storage settings (quota, eviction targets).
    pub storage_settings: StorageSettings,

    /// Flag to signal that eviction should be checked after storage.
    /// Set after downloads complete, handled in main update loop.
    pub check_eviction_requested: bool,

    /// Whether the site selection modal is open.
    pub site_modal_open: bool,

    /// Whether the left sidebar is visible.
    pub left_sidebar_visible: bool,

    /// Whether the right sidebar is visible.
    pub right_sidebar_visible: bool,

    /// Whether the keyboard shortcut help overlay is visible.
    pub shortcuts_help_visible: bool,

    /// Whether the "wipe all data" confirmation modal is open.
    pub wipe_modal_open: bool,

    /// Flag to signal that all data should be wiped (IDB + localStorage + reload).
    /// Set by the wipe modal, handled in main update loop.
    pub wipe_all_requested: bool,

    /// Theme mode selection (System, Dark, Light).
    pub theme_mode: ThemeMode,

    /// Resolved dark mode flag for the current frame.
    pub is_dark: bool,

    /// GPU rendering processing options (interpolation, smoothing, etc.).
    pub render_processing: RenderProcessing,

    /// Whether the inspector tool is active (hover shows lat/lon and data value).
    pub inspector_enabled: bool,

    /// Whether the distance measurement tool is active.
    pub distance_tool_active: bool,

    /// Distance measurement start point (lat, lon).
    pub distance_start: Option<(f64, f64)>,

    /// Distance measurement end point (lat, lon).
    pub distance_end: Option<(f64, f64)>,

    /// Whether storm cell detection overlay is visible.
    pub storm_cells_visible: bool,

    /// Cached storm cell detection results (centroid lat, lon, max dBZ, area km2).
    pub detected_storm_cells: Vec<StormCellInfo>,
}

/// Lightweight storm cell info for rendering on the canvas.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Fields are part of detection results data model
pub struct StormCellInfo {
    /// Centroid latitude.
    pub lat: f64,
    /// Centroid longitude.
    pub lon: f64,
    /// Maximum reflectivity (dBZ).
    pub max_dbz: f32,
    /// Cell area in km^2.
    pub area_km2: f32,
    /// Bounding box (min_lat, min_lon, max_lat, max_lon).
    pub bounds: (f64, f64, f64, f64),
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
        let now = js_sys::Date::now() / 1000.0;

        // Start with empty timeline - will be populated from cache
        let radar_timeline = RadarTimeline::default();

        // Set up playback state centered on "now"
        let playback_state = PlaybackState::new_at_time(now);

        // Load storage settings from localStorage
        let storage_settings = StorageSettings::load();

        // Load theme preference
        let theme_mode = theme::load_theme_mode();
        let is_dark = theme_mode.is_dark();

        let mut state = Self {
            playback_state,
            radar_timeline,
            status_message: "Ready".to_string(),
            session_stats: SessionStats::new(),
            storage_settings,
            left_sidebar_visible: true,
            right_sidebar_visible: true,
            theme_mode,
            is_dark,
            // Request timeline refresh on startup to load from cache
            timeline_needs_refresh: true,
            ..Default::default()
        };

        // Apply persisted user preferences (speed, palette, layers, etc.)
        let prefs = UserPreferences::load();
        prefs.apply_to(&mut state);

        state
    }
}
