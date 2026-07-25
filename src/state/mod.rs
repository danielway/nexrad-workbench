//! Application state management.
//!
//! This module contains all state structures used throughout the application.
//! State is organized into logical groupings that correspond to different
//! areas of functionality.

use crate::core::{
    ElevationListEntry, FrameNow, OperationId, RadarTimeline, RenderProcessing, UserPreferences,
};

#[allow(dead_code)]
pub(crate) mod acquisition;
mod alerts;
mod app_mode;
pub(crate) mod calendar;
mod errors;
mod frame_clock;
mod gps;
mod layer;
mod live_mode;
mod live_radar_model;
mod mping;
mod playback;
pub(crate) mod playback_manager;
mod preferences;
pub mod recency;
pub(crate) mod render_cache;
mod saved_events;
mod settings;
mod stats;
pub(crate) mod theme;
mod timeline_view;
pub(crate) mod url_state;
mod viz;

pub use acquisition::{
    AcquisitionState, DrawerTab, NetworkGroupKey, OperationKind, OperationStatus,
};
pub use alerts::AlertsState;
pub use app_mode::{derive_app_mode, AppMode};
pub use calendar::{aggregate_day_buckets, day_tap_macro_view, DayBucket, DAY_SECS};
pub use errors::{AppError, ErrorContext};
pub use gps::GpsState;
pub use layer::LayerState;
pub use live_mode::{should_stop_for_detached_idle, LiveExitReason, LiveModeState, LivePhase};
pub use live_radar_model::LiveRadarModel;
pub use mping::MpingState;
pub use playback::{
    format_lag, FreezeAt, LoopBasis, LoopMode, LoopPreset, MacroFrameInputs, PlaybackDirection,
    PlaybackMode, PlaybackSpeed, PlaybackState, RebuildCause, TimeModel, TimeSelection,
    TimelineTier, TIMELINE_ZOOM_MAX,
};
pub use render_cache::{PrevSweepCacheKey, RenderCache};
pub use saved_events::{SavedEvent, SavedEvents};
pub use settings::{format_bytes, StorageSettings};
pub use stats::{
    DownloadPhase, DownloadProgress, IngestTimingDetail, RenderTimingDetail, SessionStats,
};
// Re-export the command type for ergonomic access.
// AppCommand is defined directly in this module above.
pub use theme::ThemeMode;
pub use timeline_view::{
    FrameCell, FrameCellState, FrameJoinInputs, ScanContainer, TimelineView,
    SCAN_JOIN_TOLERANCE_SECS,
};
pub use viz::{derive_canvas_caption, CanvasCaption, VizState};

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
    /// Re-pin the playhead to the live edge. Instant when the stream is
    /// already running (detached browsing); otherwise starts a stream.
    ReturnToLive,
    /// Apply a loop preset (spec §8): pin-to-live, last N frames, or a duration
    /// window. Resolved into a [`LoopWindow`] + the right playhead transition.
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
    /// "Show on map" for an alert: enable its overlay class and center the 2D
    /// view on its bbox. Cross-cuts diagnostics + viz, so it's handled in the
    /// shell (with viz access) via the pure `compute_alert_focus`.
    ShowAlertOnMap(String),
}

/// Root application state containing all sub-states.
#[derive(Default)]
pub struct AppState {
    /// Wall-clock "now" for this frame, captured once in
    /// `apply_frame_setup` before any consumer runs.
    pub frame_now: FrameNow,

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

    /// A timeline range selection finalized this frame (shift+click/drag),
    /// snapshotted as `(start, end)` seconds. The main update loop consumes it,
    /// applies the duration gate, and either arms the bulk-fetch pump or opens
    /// the confirm modal. `None` when no selection was just finalized.
    pub selection_just_finalized: Option<(f64, f64)>,

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

    /// Data-saver policy: when `true`, detaching the playhead stops the live
    /// stream immediately rather than letting it ingest in the background
    /// (spec §7). Default off; persisted in `UserPreferences`. Read by
    /// `Live::detach_playhead` (the single policy site).
    pub pause_stream_while_reviewing: bool,

    /// Acquisition policy: when `true` (default), playhead-driven reactive
    /// prefetch + the anchor fast-path run as the user scrubs/seeks. When
    /// `false` (data-saver), that automatic fetch is suppressed — explicit
    /// range selections and the inspector's tap-to-fetch still work (spec §10).
    /// Persisted in `UserPreferences`; read by `pump_implicit_prefetch`.
    pub autofetch_while_scrubbing: bool,

    /// One-shot boot intent: when set, the app should open tethered to live as
    /// soon as a site is established. Set at boot (no deep-link time) when the
    /// first-visit site modal is open; consumed by the site modal's
    /// `apply_site_selection` to queue `StartLive` once the user picks a site
    /// (spec §7 / alignment §5 — open tethered on first visit too).
    pub start_live_on_site_select: bool,

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

    /// Resolved desktop width tier for the current frame. Computed alongside
    /// [`AppState::is_mobile`] in [`AppState::refresh_mobile_mode`]. Drives
    /// progressive collapse of low-priority chrome into overflow menus so the
    /// top/bottom bars don't overlap when the window is narrow.
    pub width_tier: WidthTier,

    /// Per-frame render caches: camera-motion tracking for label-tier
    /// debouncing, prev-sweep lookup memoization, and theme-gating state.
    pub render_cache: RenderCache,
}

/// Desktop horizontal-space tier, ordered narrowest → widest. Drives how much
/// chrome the top/bottom bars keep inline vs. fold into an overflow `⋯` menu.
/// Mobile has its own dedicated chrome and ignores this; these tiers apply only
/// to the desktop layout. Breakpoints are sized against the tightest case (the
/// Advanced top bar) so the surviving inline content provably fits each tier's
/// smallest viewport, which is what structurally prevents overlap.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum WidthTier {
    /// `< 720px` (down to the ~500px floor before mobile takes over):
    /// aggressive overflow — title dropped, status hidden, the Basic/Advanced
    /// pill and all view pills demoted, timestamp compacted to time-only.
    Cramped,
    /// `720..1080px`: moderate overflow — help/version/UTC/loop demoted, and
    /// the wide four-pill Advanced view selector folded into the menu.
    Compact,
    /// `>= 1080px`: full desktop layout, nothing collapsed. The ceiling leaves
    /// headroom for a wide NWS alerts chip (many active warnings/watches).
    #[default]
    Full,
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

/// Idle threshold (seconds) of no interaction before mobile chrome hides while
/// playing (spec §13 phone: "chrome auto-hides during playback").
pub const MOBILE_CHROME_IDLE_HIDE_SECS: f64 = 3.0;

/// Per-frame bookkeeping for the mobile chrome auto-hide (spec §13 phone:
/// "Canvas full-bleed; chrome auto-hides during playback, tap to reveal").
///
/// Lives on the [`Chrome`](crate::subsystem::Chrome) subsystem (its visibility
/// domain) but is defined here, beside [`MobileSettingsTab`], so the `subsystem`
/// layer doesn't have to reach into `ui`. The hide *policy* is the pure
/// [`crate::ui::mobile::auto_hide::should_hide_chrome`]; this struct only holds
/// the timer + a one-frame reveal latch it feeds.
#[derive(Clone, Copy, Debug)]
pub struct MobileChromeAutoHide {
    /// egui `input.time` of the last interaction (chrome tap/drag or reveal
    /// tap). Far-past sentinel by default so the first idle period while
    /// playing still hides on schedule rather than instantly on launch.
    pub last_interaction_secs: f64,
    /// Whether the chrome is hidden as resolved for the current frame. Computed
    /// once per frame (before layout) and read by the mobile layers' `visible()`
    /// and the canvas, so all three agree. The canvas also uses last frame's
    /// value to recognise a reveal tap (a press while hidden, when only the
    /// canvas is on screen).
    pub hidden: bool,
    /// Set on the frame the user reveals hidden chrome by tapping the canvas —
    /// consumed by the canvas so the same tap doesn't also pan/zoom.
    pub revealed_this_frame: bool,
}

impl Default for MobileChromeAutoHide {
    fn default() -> Self {
        Self {
            last_interaction_secs: f64::NEG_INFINITY,
            hidden: false,
            revealed_this_frame: false,
        }
    }
}

impl MobileChromeAutoHide {
    /// Record an interaction at `now`, resetting the idle timer so chrome stays
    /// visible for another [`MOBILE_CHROME_IDLE_HIDE_SECS`] window.
    pub fn touch(&mut self, now_secs: f64) {
        self.last_interaction_secs = now_secs;
    }

    /// Seconds remaining until chrome would auto-hide given `now` and whether
    /// playback is advancing, or `None` if it won't hide (paused, or already
    /// past the threshold). Used to schedule one repaint at the hide moment
    /// rather than spinning every frame.
    pub fn secs_until_hide(&self, now_secs: f64, is_playing: bool) -> Option<f64> {
        if !is_playing {
            return None;
        }
        let remaining = MOBILE_CHROME_IDLE_HIDE_SECS - (now_secs - self.last_interaction_secs);
        (remaining > 0.0).then_some(remaining)
    }
}

#[cfg(test)]
mod mobile_auto_hide_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn secs_until_hide_none_when_paused() {
        let h = MobileChromeAutoHide {
            last_interaction_secs: 10.0,
            ..Default::default()
        };
        assert!(h.secs_until_hide(11.0, false).is_none());
    }

    #[wasm_bindgen_test]
    fn secs_until_hide_counts_down_while_playing() {
        let h = MobileChromeAutoHide {
            last_interaction_secs: 10.0,
            ..Default::default()
        };
        let remaining = h.secs_until_hide(11.0, true).expect("should be pending");
        assert!((remaining - (MOBILE_CHROME_IDLE_HIDE_SECS - 1.0)).abs() < 1e-9);
        // Past the threshold → no longer pending.
        assert!(h
            .secs_until_hide(10.0 + MOBILE_CHROME_IDLE_HIDE_SECS, true)
            .is_none());
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
            frame_now: FrameNow(now),
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

        // Desktop width tier — used by the top/bottom bars to fold low-priority
        // controls into overflow menus before they collide. Only meaningful when
        // not mobile (mobile uses its own chrome), but computing it
        // unconditionally is harmless and keeps the field always current.
        self.width_tier = if width >= 1080.0 {
            WidthTier::Full
        } else if width >= 720.0 {
            WidthTier::Compact
        } else {
            WidthTier::Cramped
        };

        // Mobile v1 is 2D-only. If the user was in globe mode on desktop and
        // the layout flipped to mobile (browser resize, forced override),
        // snap back to 2D rather than leaving them in a view they have no
        // controls for.
        if self.is_mobile && !self.viz_state.is_2d() {
            self.viz_state
                .camera
                .switch_to_flat_2d(crate::geo::Flat2DState::default());
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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ---- MobileSettingsTab ----

    #[wasm_bindgen_test]
    fn mobile_settings_tab_labels() {
        assert_eq!(MobileSettingsTab::Playback.label(), "Playback");
        assert_eq!(MobileSettingsTab::Product.label(), "Product");
        assert_eq!(MobileSettingsTab::Layers.label(), "Layers");
        assert_eq!(MobileSettingsTab::More.label(), "More");
    }

    #[wasm_bindgen_test]
    fn mobile_settings_tab_all_order() {
        let all = MobileSettingsTab::all();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0], MobileSettingsTab::Playback);
        assert_eq!(all[1], MobileSettingsTab::Product);
        assert_eq!(all[2], MobileSettingsTab::Layers);
        assert_eq!(all[3], MobileSettingsTab::More);
    }

    #[wasm_bindgen_test]
    fn mobile_settings_tab_default_is_playback() {
        assert_eq!(MobileSettingsTab::default(), MobileSettingsTab::Playback);
    }

    // ---- WidthTier ----

    #[wasm_bindgen_test]
    fn width_tier_default_is_full() {
        assert_eq!(WidthTier::default(), WidthTier::Full);
    }

    #[wasm_bindgen_test]
    fn width_tier_ordering_narrowest_to_widest() {
        assert!(WidthTier::Cramped < WidthTier::Compact);
        assert!(WidthTier::Compact < WidthTier::Full);
        assert!(WidthTier::Cramped < WidthTier::Full);
    }

    // ---- MobileChromeAutoHide ----

    #[wasm_bindgen_test]
    fn mobile_chrome_default_sentinel() {
        let h = MobileChromeAutoHide::default();
        assert_eq!(h.last_interaction_secs, f64::NEG_INFINITY);
        assert!(!h.hidden);
        assert!(!h.revealed_this_frame);
    }

    #[wasm_bindgen_test]
    fn mobile_chrome_touch_resets_timer() {
        let mut h = MobileChromeAutoHide::default();
        h.touch(42.5);
        assert!((h.last_interaction_secs - 42.5).abs() < 1e-9);
        h.touch(100.0);
        assert!((h.last_interaction_secs - 100.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn mobile_chrome_secs_until_hide_none_when_paused() {
        let h = MobileChromeAutoHide {
            last_interaction_secs: 10.0,
            ..Default::default()
        };
        assert!(h.secs_until_hide(11.0, false).is_none());
    }

    #[wasm_bindgen_test]
    fn mobile_chrome_secs_until_hide_counts_down() {
        let h = MobileChromeAutoHide {
            last_interaction_secs: 10.0,
            ..Default::default()
        };
        // 1 second of idle elapsed → remaining = threshold - 1.
        let remaining = h.secs_until_hide(11.0, true).expect("pending");
        assert!((remaining - (MOBILE_CHROME_IDLE_HIDE_SECS - 1.0)).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn mobile_chrome_secs_until_hide_none_past_threshold() {
        let h = MobileChromeAutoHide {
            last_interaction_secs: 10.0,
            ..Default::default()
        };
        // Exactly at the threshold remaining == 0.0, which is not > 0.0.
        assert!(h
            .secs_until_hide(10.0 + MOBILE_CHROME_IDLE_HIDE_SECS, true)
            .is_none());
        // Well past the threshold.
        assert!(h
            .secs_until_hide(10.0 + MOBILE_CHROME_IDLE_HIDE_SECS + 5.0, true)
            .is_none());
    }

    // ---- DateTimePickerState ----

    #[wasm_bindgen_test]
    fn datetime_picker_default_is_closed_and_empty() {
        let p = DateTimePickerState::default();
        assert!(!p.open);
        assert!(p.year.is_empty());
        assert!(p.month.is_empty());
        assert!(p.day.is_empty());
        assert!(p.hour.is_empty());
        assert!(p.minute.is_empty());
        assert!(p.second.is_empty());
    }

    #[wasm_bindgen_test]
    fn datetime_picker_close_clears_open() {
        let mut p = DateTimePickerState {
            open: true,
            ..Default::default()
        };
        p.close();
        assert!(!p.open);
    }

    #[wasm_bindgen_test]
    fn datetime_picker_to_timestamp_utc_epoch() {
        let p = DateTimePickerState {
            year: "1970".to_string(),
            month: "01".to_string(),
            day: "01".to_string(),
            hour: "00".to_string(),
            minute: "00".to_string(),
            second: "00".to_string(),
            ..Default::default()
        };
        let ts = p.to_timestamp(false).expect("valid utc datetime");
        assert!((ts - 0.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn datetime_picker_to_timestamp_utc_known_value() {
        // 2021-01-01 00:00:00 UTC == 1609459200 seconds since epoch.
        let p = DateTimePickerState {
            year: "2021".to_string(),
            month: "1".to_string(),
            day: "1".to_string(),
            hour: "0".to_string(),
            minute: "0".to_string(),
            second: "0".to_string(),
            ..Default::default()
        };
        let ts = p.to_timestamp(false).expect("valid utc datetime");
        assert!((ts - 1_609_459_200.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn datetime_picker_to_timestamp_utc_rejects_unparseable() {
        // Default (all-empty) inputs cannot parse → None.
        let p = DateTimePickerState::default();
        assert!(p.to_timestamp(false).is_none());

        // Garbage month also fails to parse.
        let bad = DateTimePickerState {
            year: "2021".to_string(),
            month: "abc".to_string(),
            day: "1".to_string(),
            hour: "0".to_string(),
            minute: "0".to_string(),
            second: "0".to_string(),
            ..Default::default()
        };
        assert!(bad.to_timestamp(false).is_none());
    }

    #[wasm_bindgen_test]
    fn datetime_picker_to_timestamp_utc_rejects_invalid_date() {
        // Month 13 is out of range → chrono returns non-Single → None.
        let p = DateTimePickerState {
            year: "2021".to_string(),
            month: "13".to_string(),
            day: "1".to_string(),
            hour: "0".to_string(),
            minute: "0".to_string(),
            second: "0".to_string(),
            ..Default::default()
        };
        assert!(p.to_timestamp(false).is_none());
    }

    // ---- AppState command queue / gating ----

    #[wasm_bindgen_test]
    fn app_state_default_show_advanced_false() {
        let state = AppState::default();
        assert!(!state.show_advanced());
        assert!(state.commands.is_empty());
    }

    #[wasm_bindgen_test]
    fn app_state_show_advanced_tracks_flag() {
        let mut state = AppState::default();
        state.advanced_mode = true;
        assert!(state.show_advanced());
        state.advanced_mode = false;
        assert!(!state.show_advanced());
    }

    #[wasm_bindgen_test]
    fn app_state_push_and_drain_commands_fifo() {
        let mut state = AppState::default();
        state.push_command(AppCommand::ClearCache);
        state.push_command(AppCommand::StartLive);
        state.push_command(AppCommand::ClearLoop);
        assert_eq!(state.commands.len(), 3);

        let drained = state.drain_commands();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0], AppCommand::ClearCache);
        assert_eq!(drained[1], AppCommand::StartLive);
        assert_eq!(drained[2], AppCommand::ClearLoop);
        // Draining empties the queue.
        assert!(state.commands.is_empty());
        assert!(state.drain_commands().is_empty());
    }
}
