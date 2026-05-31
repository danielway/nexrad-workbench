//! Application state management.
//!
//! This module contains all state structures used throughout the application.
//! State is organized into logical groupings that correspond to different
//! areas of functionality.

#[allow(dead_code)]
pub(crate) mod acquisition;
mod alerts;
mod app_mode;
mod errors;
mod gps;
mod layer;
mod live_mode;
mod live_radar_model;
mod mping;
mod playback;
pub(crate) mod playback_manager;
mod preferences;
pub(crate) mod radar_data;
pub mod recency;
pub(crate) mod render_cache;
mod saved_events;
mod settings;
mod stats;
pub(crate) mod theme;
mod timeline_view;
pub(crate) mod url_state;
pub(crate) mod vcp;
pub(crate) mod vcp_forecast;
pub(crate) mod vcp_position;
mod viz;
mod volume_elevation_roster;

pub use crate::geo::camera::CameraMode;
pub use acquisition::{
    AcquisitionState, DrawerTab, NetworkGroupKey, OperationId, OperationKind, OperationStatus,
    QueueState,
};
pub use alerts::AlertsState;
pub use app_mode::AppMode;
pub use errors::{AppError, ErrorContext};
pub use gps::GpsState;
pub use layer::{GeoLayerVisibility, LayerState};
pub use live_mode::{LiveExitReason, LiveModeState, LivePhase};
pub use live_radar_model::LiveRadarModel;
pub use mping::MpingState;
pub use playback::{
    LoopMode, PlaybackMode, PlaybackSpeed, PlaybackState, TimeModel, LIVE_EDGE_THRESHOLD_SECS,
};
pub use preferences::UserPreferences;
pub use radar_data::RadarTimeline;
pub use render_cache::{PrevSweepCacheKey, RenderCache};
pub use saved_events::{SavedEvent, SavedEvents};
pub use settings::{format_bytes, StorageSettings};
pub use stats::{
    DownloadPhase, DownloadProgress, IngestTimingDetail, RenderTimingDetail, SessionStats,
};
// Re-export the command type for ergonomic access.
// AppCommand is defined directly in this module above.
pub use theme::ThemeMode;
pub use timeline_view::{LiveOverlayContext, TimelineView};
pub use vcp::get_vcp_definition;
pub use vcp_forecast::{
    derive_volume_forecast, BucketKey, ChunkArrivalStat, CompletedVolumeRecord, SweepForecast,
    VolumeForecastSnapshot,
};
pub use vcp_position::{
    SweepAvailability, SweepPosition, SweepStatus, SweepTiming, VcpPositionModel,
};
pub use viz::{
    DisplayedSweep, ElevationListEntry, ElevationSelection, InterpolationMode, RadarProduct,
    RenderProcessing, StormCellInfo, SweepIdentity, ViewMode, VizState,
};
pub use volume_elevation_roster::VolumeElevationRoster;

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

    /// Download progress tracking for timeline ghost markers and pipeline display.
    pub download_progress: DownloadProgress,

    /// Command queue for cross-component signaling.
    /// UI code pushes commands; the main update loop drains and dispatches them.
    pub commands: std::collections::VecDeque<AppCommand>,

    /// Whether the next timeline load should auto-position the playback cursor.
    /// Set to true on initial startup and site changes; false for download-triggered refreshes.
    pub auto_position_on_timeline_load: bool,

    /// State for the datetime picker popup.
    pub datetime_picker: DateTimePickerState,

    /// Storage settings (quota, eviction targets).
    pub storage_settings: StorageSettings,

    /// Preferred NEXRAD site chosen during first visit. `Some` means the user
    /// has already completed the first-visit flow and this site should be used
    /// as the default on future visits.
    pub preferred_site: Option<String>,

    /// Theme mode selection (System, Dark, Light).
    pub theme_mode: ThemeMode,

    /// Resolved dark mode flag for the current frame.
    pub is_dark: bool,

    /// GPU rendering processing options (interpolation, smoothing, etc.).
    pub render_processing: RenderProcessing,

    /// Whether to display times in local timezone (false = UTC).
    pub use_local_time: bool,

    /// Developer mode: shows perf timings, FPS, network metrics, and the COI
    /// badge in the status bar, and enables the code paths that feed them.
    /// Mirrored to/from the `?dev=true` URL parameter.
    pub dev_mode: bool,

    /// Advanced UI mode: when `false` (Basic, default for new users), the
    /// left panel and several right-panel sections are hidden. When `true`
    /// (Advanced), all controls are visible regardless of operational mode.
    /// Persisted in `UserPreferences`; existing users are migrated to `true`.
    /// Override via `?ui=basic` or `?ui=advanced`.
    pub advanced_mode: bool,

    /// User-saved weather event bookmarks.
    pub saved_events: SavedEvents,

    /// Aggregate network statistics from the service worker (all intercepted traffic).
    pub network_aggregate: crate::nexrad::NetworkAggregate,

    /// Recent network requests from the service worker (ring buffer for UI log).
    /// Bounded by [`MAX_RECENT_NETWORK_REQUESTS`].
    pub recent_network_requests: std::collections::VecDeque<crate::nexrad::NetworkRequest>,

    /// Whether the browsing context is cross-origin isolated (SharedArrayBuffer available).
    pub cross_origin_isolated: bool,

    /// Recent-errors ring buffer. Reporters across the codebase push
    /// into this; UI surfaces from it instead of inventing its own
    /// per-feature error indicators.
    pub errors: ErrorContext,

    /// Persistent worker initialization error message.
    /// When set, a non-dismissable error banner is shown in the top bar.
    pub worker_init_error: Option<String>,

    /// National radar mosaic overlay — fetches the CONUS composite while
    /// the corresponding layer toggle is enabled.
    pub national_mosaic: crate::nexrad::NationalMosaic,

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

    /// Per-frame render caches: camera-motion tracking for label-tier
    /// debouncing, prev-sweep lookup memoization, and theme-gating state.
    pub render_cache: RenderCache,
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

/// Bootstrap output from [`AppState::new`].
///
/// Carries values that are loaded from persistence at construction time
/// but no longer live on `AppState` itself: the persisted speed
/// (which belongs on the Playback subsystem) and the saved mPING API
/// key (which belongs on the Diagnostics subsystem).
pub struct AppStateBootstrap {
    pub state: AppState,
    pub playback: PlaybackState,
    pub mping_api_key: Option<String>,
}

impl Default for AppStateBootstrap {
    fn default() -> Self {
        AppState::bootstrap()
    }
}

impl AppState {
    /// Construct a fresh `AppState`, loading persisted preferences from
    /// localStorage along the way. Returns an [`AppStateBootstrap`] so
    /// pieces of the persisted state that belong on subsystems
    /// (currently the playback speed and mPING API key) can be applied
    /// at the right place by the caller.
    pub fn bootstrap() -> AppStateBootstrap {
        // Use current time for initialization
        let now = js_sys::Date::now() / 1000.0;

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
            status_message: "Ready".to_string(),
            session_stats: SessionStats::new(),
            storage_settings,
            saved_events,
            theme_mode,
            is_dark,
            commands,
            auto_position_on_timeline_load: false,
            ..Default::default()
        };
        let mut playback = PlaybackState::new_at_time(now);

        // Apply persisted user preferences (speed, palette, layers, etc.).
        // Returns the mPING api key (if any) which lives on the diagnostics
        // subsystem rather than on AppState; the constructor caller is
        // responsible for applying it.
        let prefs = UserPreferences::load();
        let mping_api_key = prefs.apply_to(&mut state, &mut playback);

        AppStateBootstrap {
            state,
            playback,
            mping_api_key,
        }
    }

    /// Whether advanced controls should be shown. Helper for UI gating
    /// throughout the codebase — call this rather than reading
    /// `advanced_mode` directly so future logic (e.g. forced-advanced
    /// during a session) can live in one place.
    pub fn show_advanced(&self) -> bool {
        self.advanced_mode
    }

    /// Push a command onto the queue for the main update loop to process.
    pub fn push_command(&mut self, cmd: AppCommand) {
        self.commands.push_back(cmd);
    }

    /// Drain all pending commands from the queue.
    pub fn drain_commands(&mut self) -> Vec<AppCommand> {
        self.commands.drain(..).collect()
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

    /// Whether sweep animation is effectively enabled. Requires the user
    /// preference, micro playback mode (zoomed in), and Advanced UI mode.
    /// Macro mode and Basic UI both suppress the animation regardless of
    /// the stored preference; Basic users get a calmer display and the
    /// preference is preserved across UI-mode toggles.
    pub fn effective_sweep_animation(&self, playback: &PlaybackState) -> bool {
        self.render_processing.sweep_animation
            && playback.playback_mode() == PlaybackMode::Micro
            && self.advanced_mode
    }

    /// Elevation list for the current playback context, derived per
    /// call from the same sources the left panel uses: scan at the
    /// playback timestamp first, then the live VCP if streaming, else
    /// empty. Both panels share this so they can't disagree about
    /// what's available.
    ///
    /// `playback` / `timeline` / `live_vcp_pattern` are passed in from
    /// their respective subsystems so this method doesn't reach into
    /// them itself.
    pub fn current_elevation_list(
        &self,
        playback: &PlaybackState,
        timeline: &RadarTimeline,
        live_vcp_pattern: Option<&crate::data::keys::ExtractedVcp>,
    ) -> Vec<ElevationListEntry> {
        let ts = playback.playback_position();
        if let Some(scan) = timeline.find_scan_at_timestamp(ts) {
            return playback_manager::build_elevation_list(scan);
        }
        if let Some(vcp) = live_vcp_pattern {
            if !vcp.elevations.is_empty() {
                return playback_manager::build_elevation_list_from_vcp(vcp);
            }
        }
        Vec::new()
    }
}
