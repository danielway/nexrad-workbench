//! Application state management.
//!
//! This module contains all state structures used throughout the application.
//! State is organized into logical groupings that correspond to different
//! areas of functionality.

#[allow(dead_code)]
pub(crate) mod acquisition;
mod alerts;
mod app_mode;
mod layer;
mod live_mode;
mod live_radar_model;
mod playback;
pub(crate) mod playback_manager;
mod preferences;
pub(crate) mod radar_data;
mod saved_events;
mod settings;
mod stats;
pub(crate) mod theme;
pub(crate) mod url_state;
pub(crate) mod vcp;
pub(crate) mod vcp_forecast;
mod vcp_position;
mod viz;

pub use crate::geo::camera::CameraMode;
pub use acquisition::{
    AcquisitionState, DrawerTab, NetworkGroupKey, OperationId, OperationKind, OperationStatus,
    QueueState,
};
pub use alerts::AlertsState;
pub use app_mode::AppMode;
pub use layer::{GeoLayerVisibility, LayerState};
pub use live_mode::{LiveExitReason, LiveModeState, LivePhase};
pub use live_radar_model::LiveRadarModel;
pub use playback::{
    LoopMode, PlaybackMode, PlaybackSpeed, PlaybackState, TimeModel, MICRO_ZOOM_THRESHOLD,
};
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
pub use vcp_forecast::{ChunkArrivalStat, RateSource, SweepForecast, VolumeForecastSnapshot};
pub use vcp_position::{SweepPosition, SweepStatus, SweepTiming, VcpPositionModel};
pub use viz::{
    ElevationListEntry, ElevationSelection, InterpolationMode, RadarProduct, RenderProcessing,
    StormCellInfo, ViewMode, VizState,
};

/// Cap on the recent-network-requests ring used by the UI log.
pub const MAX_RECENT_NETWORK_REQUESTS: usize = 100;

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
    /// Retry initializing the decode worker after a failure.
    RetryWorker,
    /// Request an immediate refresh of the NWS alerts feed.
    RefreshAlerts,
    /// Open the alert detail modal for a specific alert id.
    OpenAlert(String),
    /// Close any open alert modal (detail or list).
    #[allow(dead_code)] // Provided for symmetry; modals close via their own buttons.
    CloseAlert,
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

    /// Derived top-level application mode (Idle / Archive / Live).
    /// Recomputed by [`AppState::refresh_live_model`] once per frame.
    pub app_mode: AppMode,

    /// Computed live radar model — derived once per frame from `live_mode_state`.
    /// Provides a consistent snapshot for all UI consumers within a single frame.
    pub live_radar_model: LiveRadarModel,

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

    /// Whether to display times in local timezone (false = UTC).
    pub use_local_time: bool,

    /// Whether the stats detail popup is open.
    pub stats_detail_open: bool,

    /// Whether the VCP forecast diagnostics modal is open.
    pub vcp_forecast_open: bool,

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
    /// Bounded by [`MAX_RECENT_NETWORK_REQUESTS`].
    pub recent_network_requests: std::collections::VecDeque<crate::nexrad::NetworkRequest>,

    /// Whether the browsing context is cross-origin isolated (SharedArrayBuffer available).
    pub cross_origin_isolated: bool,

    /// Whether the network request log modal is open.
    pub network_log_open: bool,

    /// Unified acquisition queue state.
    pub acquisition: AcquisitionState,

    /// Persistent worker initialization error message.
    /// When set, a non-dismissable error banner is shown in the top bar.
    pub worker_init_error: Option<String>,

    /// National radar mosaic overlay — fetches the CONUS composite while
    /// the corresponding layer toggle is enabled.
    pub national_mosaic: crate::nexrad::NationalMosaic,

    /// NWS active alerts + related modal state.
    pub alerts: AlertsState,

    /// Resolved mobile mode for the current frame. Computed by
    /// [`AppState::refresh_mobile_mode`] from viewport width and touch history.
    /// When true, panels collapse to the mobile chrome.
    pub is_mobile: bool,

    /// Sticky flag — set the first time any touch event is seen. Used by
    /// the auto-detection in [`AppState::refresh_mobile_mode`] so that a
    /// touch laptop (or phone rotated from portrait to landscape) doesn't
    /// flip back to desktop layout mid-session.
    pub touch_seen_ever: bool,

    /// User override for mobile mode. `None` = auto (default), `Some(true)` =
    /// force mobile, `Some(false)` = force desktop. Persisted via preferences.
    pub mobile_override: Option<bool>,

    /// Whether the mobile settings modal (opened via the ellipsis button in
    /// the mobile bottom bar) is currently visible.
    pub mobile_settings_open: bool,

    /// Active tab inside the mobile settings modal.
    pub mobile_settings_tab: MobileSettingsTab,

    /// Latched when the mobile bottom bar's location button is tapped. The
    /// main update loop consumes this flag and kicks off geolocation against
    /// the `SiteModalState` that lives outside `AppState`, avoiding a direct
    /// state dependency from the bottom-bar renderer.
    pub mobile_geolocate_requested: bool,
}

/// Tabs in the mobile settings modal. Order matches the tab strip layout.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum MobileSettingsTab {
    #[default]
    Playback,
    Product,
    Layers,
    More,
}

impl MobileSettingsTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Playback => "Playback",
            Self::Product => "Product",
            Self::Layers => "Layers",
            Self::More => "More",
        }
    }

    pub fn all() -> [Self; 4] {
        [Self::Playback, Self::Product, Self::Layers, Self::More]
    }
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
            auto_position: false,
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
            commands,
            auto_position_on_timeline_load: false,
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

    /// Recompute the `live_radar_model` snapshot for this frame.
    ///
    /// Call once at the start of each UI frame so all consumers see consistent
    /// state derived from the same `now` timestamp.
    pub fn refresh_live_model(&mut self) {
        let now = js_sys::Date::now() / 1000.0;
        self.live_radar_model = self.live_mode_state.compute_model(now);
        self.app_mode = if self.live_mode_state.is_active() {
            AppMode::Live
        } else if self
            .radar_timeline
            .find_scan_at_timestamp(self.playback_state.playback_position())
            .is_some()
        {
            AppMode::Archive
        } else {
            AppMode::Idle
        };
    }

    /// Refresh the mobile-mode flag for this frame.
    ///
    /// Auto mode: `width < 600px` plus either a sticky "touch has been seen"
    /// flag or `width < 500px` (so a very narrow desktop window also switches
    /// without needing a touch event). A user override in `mobile_override`
    /// takes precedence.
    pub fn refresh_mobile_mode(&mut self, ctx: &eframe::egui::Context) {
        let width = ctx.content_rect().width();
        let touch_now = ctx.input(|i| i.any_touches() || i.multi_touch().is_some());
        if touch_now {
            self.touch_seen_ever = true;
        }
        let auto = width < 600.0 && (self.touch_seen_ever || width < 500.0);
        self.is_mobile = self.mobile_override.unwrap_or(auto);

        // Mobile v1 is 2D-only. If the user was in globe mode on desktop and
        // the layout flipped to mobile (browser resize, forced override),
        // snap back to 2D rather than leaving them in a view they have no
        // controls for.
        if self.is_mobile && self.viz_state.view_mode != ViewMode::Flat2D {
            self.viz_state.view_mode = ViewMode::Flat2D;
        }
    }

    /// Whether sweep animation is effectively enabled: requires both the user
    /// preference AND micro playback mode (zoomed in). In macro mode, sweep
    /// animation is suppressed regardless of the user preference.
    pub fn effective_sweep_animation(&self) -> bool {
        self.render_processing.sweep_animation
            && self.playback_state.playback_mode() == PlaybackMode::Micro
    }

    /// Set the status message and record the timestamp for auto-dismissal.
    #[allow(dead_code)]
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = msg.into();
        self.status_message_set_ms = js_sys::Date::now();
    }
}
