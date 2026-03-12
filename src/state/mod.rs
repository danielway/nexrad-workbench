//! Application state management.
//!
//! This module contains all state structures used throughout the application.
//! State is organized into logical groupings that correspond to different
//! areas of functionality.

#[allow(dead_code)]
pub(crate) mod acquisition;
mod layer;
mod live_mode;
mod playback;
mod preferences;
pub(crate) mod radar_data;
mod saved_events;
mod settings;
mod stats;
pub(crate) mod theme;
pub(crate) mod url_state;
pub(crate) mod vcp;
mod viz;

pub use crate::geo::camera::CameraMode;
pub use acquisition::{
    AcquisitionState, DrawerTab, OperationId, OperationKind, OperationStatus, QueueState,
};
pub use layer::{GeoLayerVisibility, LayerState};
pub use live_mode::{LiveExitReason, LiveModeState, LivePhase};
pub use playback::{LoopMode, PlaybackSpeed, PlaybackState, TimeModel};
pub use preferences::UserPreferences;
pub use radar_data::RadarTimeline;
pub use saved_events::{SavedEvent, SavedEvents};
pub use settings::{format_bytes, StorageSettings};
pub use stats::{
    DownloadPhase, DownloadProgress, IngestTimingDetail, RenderTimingDetail, SessionStats,
};
// Re-export the command type for ergonomic access.
// AppCommand is defined directly in this module above.
pub use theme::ThemeMode;
pub use vcp::get_vcp_definition;
pub use viz::{InterpolationMode, RadarProduct, RenderMode, RenderProcessing, ViewMode, VizState};

/// Commands dispatched by UI code and consumed by the main update loop.
///
/// Replaces scattered boolean `*_requested` flags with an explicit command queue,
/// making state transitions easier to follow and impossible to forget to clear.
#[derive(Debug, Clone, PartialEq)]
pub enum AppCommand {
    /// Refresh the timeline from the cache. Optionally auto-position the cursor.
    RefreshTimeline { auto_position: bool },
    /// Clear the record cache.
    ClearCache,
    /// Download all scans in the current selection range.
    DownloadSelection,
    /// Download the scan at the current playback position.
    DownloadAtPosition,
    /// Start live/real-time streaming.
    StartLive,
    /// Fetch the latest available archive scan for the current site.
    /// Triggered after site selection to give the user immediate data
    /// without starting real-time streaming.
    FetchLatest,
    /// Check and run eviction after a storage operation.
    CheckEviction,
    /// Wipe all data (IndexedDB + localStorage) and reload.
    WipeAll,
    /// Pause the acquisition queue.
    PauseQueue,
    /// Resume the acquisition queue.
    ResumeQueue,
    /// Retry a failed operation.
    RetryFailed(OperationId),
    /// Skip a failed operation and continue.
    SkipFailed(OperationId),
    /// Cancel a specific operation.
    CancelOperation(OperationId),
    /// Reorder an operation (delta: -1 = up, +1 = down).
    ReorderOperation(OperationId, isize),
}

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

    /// Timestamp (ms since epoch) when the status message was last set.
    /// Used for auto-dismissal.
    pub status_message_set_ms: f64,

    /// Session and performance statistics
    pub session_stats: SessionStats,

    /// Live streaming mode state
    pub live_mode_state: LiveModeState,

    /// Download progress tracking for timeline ghost markers and pipeline display.
    pub download_progress: DownloadProgress,

    /// Command queue for cross-component signaling.
    /// UI code pushes commands; the main update loop drains and dispatches them.
    pub commands: std::collections::VecDeque<AppCommand>,

    /// Whether the next timeline load should auto-position the playback cursor.
    /// Set to true on initial startup and site changes; false for download-triggered refreshes.
    pub auto_position_on_timeline_load: bool,

    /// Whether a selection download is currently in progress.
    pub download_selection_in_progress: bool,

    /// State for the datetime picker popup.
    pub datetime_picker: DateTimePickerState,

    /// Storage settings (quota, eviction targets).
    pub storage_settings: StorageSettings,

    /// Whether the site selection modal is open.
    pub site_modal_open: bool,

    /// Preferred NEXRAD site chosen during first visit. `Some` means the user
    /// has already completed the first-visit flow and this site should be used
    /// as the default on future visits.
    pub preferred_site: Option<String>,

    /// Whether the left sidebar is visible.
    pub left_sidebar_visible: bool,

    /// Whether the right sidebar is visible.
    pub right_sidebar_visible: bool,

    /// Whether the keyboard shortcut help overlay is visible.
    pub shortcuts_help_visible: bool,

    /// Whether the "wipe all data" confirmation modal is open.
    pub wipe_modal_open: bool,

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

    /// Minimum dBZ threshold for storm cell detection.
    pub storm_cell_threshold_dbz: f32,

    /// Cached storm cell detection results (centroid lat, lon, max dBZ, area km2).
    pub detected_storm_cells: Vec<StormCellInfo>,

    /// Timestamp of the currently displayed scan (seconds since epoch).
    pub displayed_scan_timestamp: Option<i64>,

    /// Elevation number of the currently displayed sweep.
    pub displayed_sweep_elevation_number: Option<u8>,

    /// Whether to display times in local timezone (false = UTC).
    pub use_local_time: bool,

    /// Whether the stats detail popup is open.
    pub stats_detail_open: bool,

    /// User-saved weather event bookmarks.
    pub saved_events: SavedEvents,

    /// Whether the event create/edit modal is open.
    pub event_modal_open: bool,

    /// Event ID being edited (None = creating new event).
    pub event_modal_editing_id: Option<u64>,

    /// Shadowed scan boundaries from the archive index.
    ///
    /// When a listing is fetched for a site/date, scan time boundaries are
    /// derived from adjacent file timestamps and stored here. The timeline
    /// renders these as subtle markers to show where scans exist before they
    /// are actually downloaded.
    pub shadow_scan_boundaries: Vec<crate::nexrad::ScanBoundary>,

    /// Aggregate network statistics from the service worker (all intercepted traffic).
    pub network_aggregate: crate::nexrad::NetworkAggregate,

    /// Recent network requests from the service worker (ring buffer for UI log).
    pub recent_network_requests: std::collections::VecDeque<crate::nexrad::NetworkRequest>,

    /// Whether the browsing context is cross-origin isolated (SharedArrayBuffer available).
    pub cross_origin_isolated: bool,

    /// Whether the network request log modal is open.
    pub network_log_open: bool,

    /// Unified acquisition queue state.
    pub acquisition: AcquisitionState,
}

/// Lightweight storm cell info for rendering on the canvas.
#[derive(Clone, Debug)]
#[allow(dead_code)]
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
    /// Initialize the picker with a timestamp, respecting the timezone setting.
    pub fn init_from_timestamp(&mut self, ts: f64, use_local: bool) {
        if use_local {
            let d = js_sys::Date::new_0();
            d.set_time(ts * 1000.0);
            self.year = format!("{:04}", d.get_full_year());
            self.month = format!("{:02}", d.get_month() + 1); // JS months are 0-based
            self.day = format!("{:02}", d.get_date());
            self.hour = format!("{:02}", d.get_hours());
            self.minute = format!("{:02}", d.get_minutes());
            self.second = format!("{:02}", d.get_seconds());
        } else {
            use chrono::{TimeZone, Utc};
            let dt = Utc.timestamp_opt(ts as i64, 0).unwrap();
            self.year = dt.format("%Y").to_string();
            self.month = dt.format("%m").to_string();
            self.day = dt.format("%d").to_string();
            self.hour = dt.format("%H").to_string();
            self.minute = dt.format("%M").to_string();
            self.second = dt.format("%S").to_string();
        }
        self.open = true;
    }

    /// Try to parse the current input values into a UTC timestamp (seconds).
    pub fn to_timestamp(&self, use_local: bool) -> Option<f64> {
        let year: i32 = self.year.parse().ok()?;
        let month: u32 = self.month.parse().ok()?;
        let day: u32 = self.day.parse().ok()?;
        let hour: u32 = self.hour.parse().ok()?;
        let minute: u32 = self.minute.parse().ok()?;
        let second: u32 = self.second.parse().ok()?;

        if use_local {
            // Construct a JS Date from local components and read back UTC millis
            let d = js_sys::Date::new_0();
            d.set_full_year(year as u32);
            d.set_month(month.checked_sub(1)?); // JS months are 0-based
            d.set_date(day);
            d.set_hours(hour);
            d.set_minutes(minute);
            d.set_seconds(second);
            d.set_milliseconds(0);
            let ts = d.get_time(); // UTC milliseconds
            if ts.is_nan() {
                return None;
            }
            Some(ts / 1000.0)
        } else {
            use chrono::{TimeZone, Utc};
            let dt = Utc.with_ymd_and_hms(year, month, day, hour, minute, second);
            match dt {
                chrono::LocalResult::Single(dt) => Some(dt.timestamp() as f64),
                _ => None,
            }
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

        // Load saved events from localStorage
        let saved_events = SavedEvents::load();

        // Load theme preference
        let theme_mode = theme::load_theme_mode();
        let is_dark = theme_mode.is_dark();

        let mut commands = std::collections::VecDeque::new();
        // Request timeline refresh on startup to load from cache
        commands.push_back(AppCommand::RefreshTimeline {
            auto_position: true,
        });

        let mut state = Self {
            playback_state,
            radar_timeline,
            status_message: "Ready".to_string(),
            session_stats: SessionStats::new(),
            storage_settings,
            saved_events,
            left_sidebar_visible: true,
            right_sidebar_visible: true,
            theme_mode,
            is_dark,
            storm_cell_threshold_dbz: 35.0,
            commands,
            auto_position_on_timeline_load: true,
            ..Default::default()
        };

        // Apply persisted user preferences (speed, palette, layers, etc.)
        let prefs = UserPreferences::load();
        prefs.apply_to(&mut state);

        state
    }

    /// Push a command onto the queue for the main update loop to process.
    pub fn push_command(&mut self, cmd: AppCommand) {
        self.commands.push_back(cmd);
    }

    /// Drain all pending commands from the queue.
    pub fn drain_commands(&mut self) -> Vec<AppCommand> {
        self.commands.drain(..).collect()
    }

    /// Set the status message and record the timestamp for auto-dismissal.
    #[allow(dead_code)]
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = msg.into();
        self.status_message_set_ms = js_sys::Date::now();
    }
}
